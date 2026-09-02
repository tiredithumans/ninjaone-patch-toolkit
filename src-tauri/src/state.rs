use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::warn;

use crate::actions::{JobReport, MAX_JOBS};
use crate::api::{NinjaApiClient, ProgressFn};
use crate::auth::AuthState;
use crate::model::PatchRow;
use crate::model::{Device, Location, Organization, Patch, Role};
use crate::rows::{GroupBy, PatchGroup, QueryResult, RowSort, build_groups, sort_order};
use crate::settings::Settings;

/// How long a confirmation token issued by `plan_action` stays usable. Short
/// enough that a dialog left open over lunch fails closed rather than dispatching
/// against a fleet whose state has moved on.
const CONFIRM_TTL: Duration = Duration::from_secs(300);

/// A plan the operator has been shown and may confirm.
///
/// The hash binds the token to the exact action + device set + parameters that
/// were planned, so an altered request fails the check even with a valid token.
pub struct PendingConfirm {
    pub token: String,
    pub request_hash: String,
    pub issued_at: Instant,
    /// The tenant the plan was approved against.
    ///
    /// `request_hash` destructures `ActionRequest` exhaustively — a new field there
    /// is a compile error — but `ActionRequest` carries no instance or client id, so
    /// no amount of hashing could bind the approval to a tenant. `run_action` then
    /// re-reads `instance_base_url` from settings *after* consuming the token, which
    /// means a plan approved against instance A could dispatch against instance B
    /// inside the 5-minute window. Every other cache in this file is tenant-stamped
    /// for exactly this reason; the one slot that authorizes writes to real devices
    /// was not. It has to live on the slot rather than in the hash for that reason.
    tenant: TenantKey,
}

/// How long cached org/location/role lookups stay fresh before a query refetches
/// them. They change rarely, so this spares repeat queries and every auto-refresh
/// tick from three extra round trips.
const LOOKUP_TTL: Duration = Duration::from_secs(300);

/// How long the whole-fleet device inventory stays fresh. Devices change rarely
/// (membership shifts over days, not minutes), so even a patching-operation
/// auto-refresh reuses the cached inventory instead of re-pulling thousands of
/// detailed devices each tick — only the live patch state is refetched.
const DEVICE_TTL: Duration = Duration::from_secs(15 * 60);

/// How long whole-fleet current patches stay fresh for a *non-forced* run (a
/// re-filter / Run query). A bound, not the freshness control: an auto-refresh tick
/// or the manual refresh forces a refetch regardless (see `fleet_current_patches`),
/// so this only caps staleness when the user is rapidly re-filtering without asking
/// for fresh data.
const CURRENT_PATCHES_TTL: Duration = Duration::from_secs(120);

/// Floor on how often a *forced* refetch may actually hit the API.
///
/// `force` exists so an auto-refresh tick or the manual ↻ can pull fresh patch
/// state mid-patching, which means it bypasses [`CURRENT_PATCHES_TTL`] — but
/// unbounded it makes the whole cache decorative on the one path that runs
/// unattended for hours. Re-paging two whole-fleet reporting feeds costs dozens
/// of sequential round trips, so a force arriving within this window is served
/// from cache instead. Enforced backend-side on purpose: the frontend cadence is
/// a hint, and a stale or buggy one must not be able to hammer the API.
const FORCE_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Identifies the tenant a cache entry belongs to. Every whole-fleet/result cache
/// stamps its entries with this and re-checks it at *read* time, so switching the
/// instance or client id invalidates them structurally — a caller that forgets to
/// `clear_*` after a tenant switch can't serve or export the prior tenant's data
/// (the read misses instead).
#[derive(Clone, PartialEq, Eq)]
struct TenantKey {
    instance_base_url: String,
    client_id: Option<String>,
}

/// Returned when the result-cache lock was poisoned by a panic while held, so a
/// caller can report it instead of silently serving an empty read.
#[derive(Debug)]
pub struct CachePoisoned;

/// Why a [`QueryToken`] redemption did or didn't reach the result cache.
///
/// The three failure arms are deliberately distinct rather than one `false`,
/// because the caller must treat them differently. Supersession is invisible to
/// the operator and the frontend already drops the response itself (it applies
/// only its own newest `query_seq`), so the summary may still be returned. Tenant
/// drift has no such frontend guard — `query_seq` counts runs the frontend
/// *starts*, and switching instance never bumps it — so returning the summary
/// would paint the previous tenant's rows over the new tenant's empty cache, with
/// paging and export reading the miss. Poisoning is the same shape: the rows would
/// be on screen while every path that re-reads them fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    /// Cached, and authoritative for this tenant.
    Stored,
    /// A newer query started while this one was in flight.
    Superseded,
    /// The operator switched instance/client id while this query was in flight.
    TenantChanged,
    /// The session was cleared (sign-out, sign-in or re-authorization) while this
    /// query was in flight. The tenant is unchanged, so `TenantChanged` cannot see
    /// it — but the rows belong to the operator who just left and must not be stored.
    SessionCleared,
    /// The cache lock was poisoned by a panic while held.
    Poisoned,
}

/// RAII claim on the single job-poller slot, issued by
/// [`AppState::try_claim_job_poller`].
///
/// Dropping it releases the slot, so **every** exit from the poller task frees the
/// claim — a panic in `advance_job`/`audit::record`/`emit_progress`, a runtime
/// shutdown dropping the task, or an early `return`. The flag used to be a bare
/// `AtomicBool` cleared only inside `release_job_poller_if_idle`, reached from the
/// single "no pending jobs" arm of the loop; any other exit leaked the claim
/// permanently, after which every later `spawn_job_poller` returned immediately and
/// **no dispatched job was polled again for the life of the process** — jobs simply
/// sat at Queued while the operator watched.
///
/// [`AppState::release_job_poller_if_idle`] takes the claim by value so the release
/// can happen under the jobs lock; it hands the claim back when work is still
/// pending.
pub struct JobPollerClaim {
    flag: Arc<AtomicBool>,
}

impl Drop for JobPollerClaim {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Opaque claim on a query run, issued by [`AppState::begin_query`] and redeemed by
/// [`AppState::store_last_result_if_current`]. Carries the generation that orders
/// overlapping queries and the tenant the run started under, so neither can be
/// re-derived (or forged) at write time.
pub struct QueryToken {
    generation: u64,
    tenant: TenantKey,
    /// The result-cache epoch at the moment the query started. A sign-out bumps it,
    /// so a query that began before the clear cannot store after it.
    result_epoch: u64,
}

/// The cached query result plus a memo of the grouping most recently asked for.
///
/// `group_page` rebuilt the entire grouping — a HashMap accumulation plus a sort
/// over every row — on *every* paging request, so clicking through a grouped view
/// re-grouped the whole fleet per click, under the same lock the export takes. The
/// memo lives inside the slot rather than beside it deliberately: it is derived from
/// exactly these rows, so replacing or clearing the result drops it in the same
/// operation and no second staleness protocol exists to get wrong.
struct CachedResult {
    tenant: TenantKey,
    /// Behind an `Arc` so a reader can take a *handle* under the lock and do its work
    /// after releasing it. Export and the HTML report used to `clone()` the whole
    /// result — every row, every `Arc<str>` field refcounted — while holding this
    /// mutex, which is the same mutex all three paging commands take. On a six-figure
    /// fleet that blocked the table for the length of a full deep copy, and then the
    /// copy was thrown away.
    result: Arc<QueryResult>,
    groups: Option<(GroupBy, Arc<Vec<PatchGroup>>)>,
    /// The row order most recently asked for, memoized for exactly the same reason
    /// `groups` is: `page_rows` re-sorted the entire cached row set on every paging
    /// request, so clicking through a sorted view of a large fleet paid a full
    /// `O(n log n)` string-comparison sweep per click — under this lock, which the
    /// export takes too. Kept inside the slot so replacing or clearing the result
    /// drops it in the same operation.
    sorted: Option<(RowSort, Arc<Vec<u32>>)>,
}

struct LookupCache {
    at: Instant,
    tenant: TenantKey,
    // Held behind `Arc` so a cache hit (and every auto-refresh tick) hands out a
    // cheap refcount bump instead of deep-cloning three Vecs.
    orgs: Arc<Vec<Organization>>,
    locations: Arc<Vec<Location>>,
    roles: Arc<Vec<Role>>,
}

struct DeviceCache {
    at: Instant,
    tenant: TenantKey,
    devices: Arc<Vec<Device>>,
}

/// One whole-fleet current-patch family (OS *or* third-party), cached independently
/// of the other.
///
/// They are separate slots rather than one pair because the two families are wildly
/// asymmetric — a whole-fleet third-party feed runs to six figures and is usually the
/// largest single fetch in a query — and `PatchType` lets the operator ask for only
/// one. Cached as a pair, an OS-only query still paged the entire software feed and
/// then discarded it, which also made that feed the critical path: an OS-only query
/// took about as long as an ALL query. Split, each family is fetched only when the
/// requested `PatchType` includes it, and a later widening to ALL reuses whatever is
/// already warm.
struct FamilyCache {
    at: Instant,
    tenant: TenantKey,
    fetched_at: DateTime<Utc>,
    patches: Arc<Vec<Patch>>,
}

impl FamilyCache {
    /// Whether this entry may be served for `tenant`. `force` still honors
    /// [`FORCE_MIN_INTERVAL`] so a fast cadence can't turn into a refetch loop.
    fn is_fresh(&self, tenant: &TenantKey, force: bool) -> bool {
        self.tenant == *tenant
            && self.at.elapsed()
                < if force {
                    FORCE_MIN_INTERVAL
                } else {
                    CURRENT_PATCHES_TTL
                }
    }
}

/// Whole-fleet current patches handed to a query: both families behind `Arc` (a
/// cache hit is a refcount bump) plus the wall-clock fetch time for the UI's
/// "patch data as of …" label.
#[derive(Clone)]
pub struct CurrentPatches {
    pub os: Arc<Vec<Patch>>,
    pub sw: Arc<Vec<Patch>>,
    pub fetched_at: DateTime<Utc>,
}

/// Process-wide application state injected into every Tauri command.
pub struct AppState {
    pub auth: AuthState,
    pub api: NinjaApiClient,
    /// Locked only for brief read/clone/replace — never held across `.await`.
    pub settings: Mutex<Settings>,
    /// Last query result, stamped with the tenant it belongs to and cached so export
    /// and row paging read it without the frontend round-tripping all rows over IPC.
    /// Private on purpose: all access goes through `store_last_result` /
    /// `with_current_result`, which enforce the tenant check — a tenant switch reads
    /// as a miss, so a forgotten clear can't serve the previous tenant's rows.
    last_result: Mutex<Option<CachedResult>>,
    /// Near-static lookups (orgs/locations/roles) cached with a short TTL.
    lookups_cache: Mutex<Option<LookupCache>>,
    /// Whole-fleet device inventory cached with a long TTL ([`DEVICE_TTL`]).
    fleet_devices_cache: Mutex<Option<DeviceCache>>,
    /// Whole-fleet current OS patches, cached so a re-filter recomputes without a
    /// refetch; refreshed on force or past [`CURRENT_PATCHES_TTL`].
    fleet_current_os: Mutex<Option<FamilyCache>>,
    /// Whole-fleet current third-party patches. Held apart from the OS family so a
    /// query that doesn't ask for it never pays to fetch it — see [`FamilyCache`].
    fleet_current_sw: Mutex<Option<FamilyCache>>,
    /// Dispatched action jobs, stamped with the tenant they belong to. Mutable and
    /// long-lived — it outlives the IPC call that created it — so it carries the
    /// same tenant check as `last_result`: a tenant switch reads as a miss, and a
    /// forgotten clear can't surface another tenant's dispatch history.
    jobs: Mutex<Option<(TenantKey, Vec<JobReport>)>>,
    /// Monotonic source of `JobReport.id` / `batch_id`.
    job_seq: AtomicU64,
    /// Monotonic query generation, bumped by [`AppState::begin_query`]. Queries
    /// overlap routinely — an auto-refresh tick fires while a manual Run is still
    /// paging the fleet — and whichever *finished* last used to win the cache
    /// regardless of which started last. Since the frontend renders the summary of
    /// the run it started last, the two could disagree: the visible table came from
    /// one query while paging, export and the HTML report read another.
    ///
    /// Owned here rather than taken from the frontend's `query_id`, which is a
    /// display hint for dropping stale progress events — a stale or malicious
    /// frontend must not be able to decide which result is authoritative.
    query_generation: AtomicU64,
    /// Bumped by [`AppState::invalidate_fleet_devices`], and re-read under the slot
    /// lock before a fetch stores. Without it, a whole-fleet fetch that started
    /// before an invalidation could complete after it and write pre-action data back
    /// into the slot the invalidation had just cleared — restarting [`DEVICE_TTL`] on
    /// exactly the rows the caller wanted gone. `TenantKey` cannot cover this: it is
    /// the same tenant.
    /// Bumped by [`AppState::clear_lookups_cache`] before it clears the slot, and
    /// re-read under the slot lock at the store in [`AppState::lookups`], so a fetch
    /// already in flight cannot write its pre-invalidation rows back. Mirrors
    /// `devices_epoch`/`current_epoch`, which the lookups cache was missing.
    lookups_epoch: AtomicU64,
    devices_epoch: AtomicU64,
    /// The same guard for both current-patch families. Bumped by
    /// [`AppState::invalidate_current_patches`], which a mutating action calls — and
    /// whose whole purpose is defeated if an in-flight fetch stores over it.
    current_epoch: AtomicU64,
    /// The same guard for the result cache, which was the one tenant-scoped slot
    /// without it. `clear_last_result` used to clear the slot bare, so a whole-fleet
    /// query still in flight at sign-out redeemed a token whose generation and tenant
    /// were both still current and stored the signed-out operator's rows straight
    /// back — after which export, the HTML report and all three paging commands
    /// served them to whoever signed in next. `TenantKey` cannot cover this either:
    /// a second operator on the same instance is the same tenant.
    result_epoch: AtomicU64,
    /// Single-flight gates for the two whole-fleet fetches, mirroring
    /// `AuthState::refresh_lock`. Queries overlap routinely, and on a cold cache both
    /// would otherwise page the entire inventory / third-party feed independently —
    /// the largest fetch in the app, run twice. A waiter re-checks the cache after
    /// acquiring and normally finds the winner's rows already there. Held across
    /// `.await`, so these are `tokio` mutexes; the `std` ones above are not.
    devices_fetch_lock: tokio::sync::Mutex<()>,
    os_fetch_lock: tokio::sync::Mutex<()>,
    sw_fetch_lock: tokio::sync::Mutex<()>,
    /// At most one poller at a time, so a burst of batches doesn't spawn N tasks
    /// all hammering `/activities`. Held behind an [`Arc`] so [`JobPollerClaim`] can
    /// own a handle and clear it on drop.
    job_poller_running: Arc<AtomicBool>,
    /// Single-slot confirmation gate — one dialog is open at a time.
    pending_confirm: Mutex<Option<PendingConfirm>>,
}

impl AppState {
    pub fn new() -> Result<Self> {
        let settings = Settings::load().unwrap_or_default();

        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "ninjaone-patch-toolkit/",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(Duration::from_secs(45))
            .build()
            .context("build http client")?;

        let auth = AuthState::new(
            http.clone(),
            settings.instance_base_url.clone(),
            settings.callback_port,
            settings.client_id.clone(),
            settings.actions.enabled,
        );
        let api = NinjaApiClient::new(http, auth.clone());

        Ok(Self {
            auth,
            api,
            settings: Mutex::new(settings),
            last_result: Mutex::new(None),
            lookups_cache: Mutex::new(None),
            fleet_devices_cache: Mutex::new(None),
            fleet_current_os: Mutex::new(None),
            fleet_current_sw: Mutex::new(None),
            jobs: Mutex::new(None),
            job_seq: AtomicU64::new(1),
            query_generation: AtomicU64::new(0),
            lookups_epoch: AtomicU64::new(0),
            devices_epoch: AtomicU64::new(0),
            current_epoch: AtomicU64::new(0),
            result_epoch: AtomicU64::new(0),
            devices_fetch_lock: tokio::sync::Mutex::new(()),
            os_fetch_lock: tokio::sync::Mutex::new(()),
            sw_fetch_lock: tokio::sync::Mutex::new(()),
            job_poller_running: Arc::new(AtomicBool::new(false)),
            pending_confirm: Mutex::new(None),
        })
    }

    /// Snapshot of settings for use across `.await` points without holding the lock.
    pub fn settings_snapshot(&self) -> Settings {
        self.settings.lock().map(|g| g.clone()).unwrap_or_else(|p| {
            // A poisoned lock still holds the real settings — recover them (and warn)
            // rather than silently defaulting, which would point queries at the
            // wrong instance/tenant.
            warn!("settings mutex poisoned; recovering the last-known settings");
            p.into_inner().clone()
        })
    }

    /// The tenant (instance + client id) that owns freshly cached data. Cheap — a
    /// brief settings lock cloning two fields, never held across `.await`. Compared
    /// at every cache read so switching tenant invalidates the caches structurally.
    fn tenant_key(&self) -> TenantKey {
        match self.settings.lock() {
            Ok(g) => TenantKey {
                instance_base_url: g.instance_base_url.clone(),
                client_id: g.client_id.clone(),
            },
            // A poisoned lock still holds the real settings — recover the identity
            // rather than defaulting, which would mis-scope every cache.
            Err(p) => {
                let g = p.into_inner();
                TenantKey {
                    instance_base_url: g.instance_base_url.clone(),
                    client_id: g.client_id.clone(),
                }
            }
        }
    }

    /// Orgs/locations/roles used to label patch rows, served from a short-TTL
    /// cache. Fetches the three concurrently on a miss. The lock is never held
    /// across the `.await`.
    pub async fn lookups(
        &self,
    ) -> Result<(Arc<Vec<Organization>>, Arc<Vec<Location>>, Arc<Vec<Role>>)> {
        let key = self.tenant_key();
        if let Ok(guard) = self.lookups_cache.lock()
            && let Some(c) = guard.as_ref()
            && c.tenant == key
            && c.at.elapsed() < LOOKUP_TTL
        {
            return Ok((c.orgs.clone(), c.locations.clone(), c.roles.clone()));
        }
        // Sampled before the fetch and re-checked under the slot lock at the store.
        let epoch = self.lookups_epoch.load(Ordering::SeqCst);
        let (orgs, locations, roles) = tokio::try_join!(
            self.api.organizations(),
            async {
                // Locations only supply optional row labels, so a failure here is
                // non-fatal — fall back to none, but warn so a tenant-wide locations
                // outage isn't silently rendered as blank location names.
                Ok::<_, anyhow::Error>(match self.api.all_locations().await {
                    Ok(locs) => locs,
                    Err(e) => {
                        warn!(error = %e, "locations fetch failed; rows will omit location names");
                        Vec::new()
                    }
                })
            },
            self.api.roles(),
        )?;
        let (orgs, locations, roles) = (Arc::new(orgs), Arc::new(locations), Arc::new(roles));
        if let Ok(mut guard) = self.lookups_cache.lock() {
            // Epoch re-check under the slot lock, exactly as `fleet_devices` and
            // `current_family` do. Storing unconditionally let a fetch that was
            // already in flight when `clear_lookups_cache` ran write its
            // pre-invalidation rows straight back and restart LOOKUP_TTL on them —
            // the tenant stamp cannot catch that, since a same-tenant sign-out is
            // the case it happens on.
            if self.lookups_epoch.load(Ordering::SeqCst) == epoch {
                *guard = Some(LookupCache {
                    at: Instant::now(),
                    tenant: key,
                    orgs: orgs.clone(),
                    locations: locations.clone(),
                    roles: roles.clone(),
                });
            } else {
                tracing::debug!("lookups invalidated mid-fetch; not caching");
            }
        }
        Ok((orgs, locations, roles))
    }

    /// Whole-fleet device inventory (no `df`), served from a long-TTL cache so
    /// identity facets can be applied client-side without re-pulling the fleet on
    /// every scope change. Fetches on a miss / past [`DEVICE_TTL`]. The lock is never
    /// held across the `.await`.
    pub async fn fleet_devices(
        &self,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Arc<Vec<Device>>> {
        let key = self.tenant_key();
        if let Some(hit) = self.cached_devices(&key) {
            return Ok(hit);
        }
        // Single-flight: the loser of the race waits here rather than paging the
        // whole inventory a second time, then finds the winner's rows on the
        // re-check below.
        let _fetching = self.devices_fetch_lock.lock().await;
        if let Some(hit) = self.cached_devices(&key) {
            return Ok(hit);
        }
        // Sampled *before* the fetch and re-checked under the slot lock at the store,
        // so an `invalidate_fleet_devices` landing during the await wins.
        let epoch = self.devices_epoch.load(Ordering::SeqCst);
        let devices = Arc::new(self.api.devices(None, on_progress).await?);
        if let Ok(mut guard) = self.fleet_devices_cache.lock() {
            if self.devices_epoch.load(Ordering::SeqCst) == epoch {
                *guard = Some(DeviceCache {
                    at: Instant::now(),
                    tenant: key,
                    devices: devices.clone(),
                });
            } else {
                tracing::debug!("device inventory invalidated mid-fetch; not caching");
            }
        }
        Ok(devices)
    }

    /// A live device-cache entry for `tenant`, or `None` on a miss / past
    /// [`DEVICE_TTL`]. Split out so the pre-lock probe and the post-lock re-check in
    /// [`Self::fleet_devices`] cannot drift apart.
    fn cached_devices(&self, tenant: &TenantKey) -> Option<Arc<Vec<Device>>> {
        let guard = self.fleet_devices_cache.lock().ok()?;
        let c = guard.as_ref()?;
        (c.tenant == *tenant && c.at.elapsed() < DEVICE_TTL).then(|| c.devices.clone())
    }

    /// Whole-fleet current patches (no `df`) for the requested families, cached so a
    /// re-filter recomputes without a refetch. `force` (an auto-refresh tick or the
    /// manual refresh) trades [`CURRENT_PATCHES_TTL`] for [`FORCE_MIN_INTERVAL`] to
    /// pull fresh patch state mid-patching without letting a fast cadence become a
    /// refetch loop.
    ///
    /// `include_os` / `include_sw` come from the query's `PatchType`. A family that
    /// wasn't asked for is neither fetched nor returned — `run_query` would discard
    /// it anyway, and the third-party feed is large enough that fetching it
    /// unconditionally dominated the query. Any locks are released before the
    /// `.await`s.
    pub async fn fleet_current_patches(
        &self,
        force: bool,
        include_os: bool,
        include_sw: bool,
        on_os: Option<&ProgressFn<'_>>,
        on_sw: Option<&ProgressFn<'_>>,
    ) -> Result<CurrentPatches> {
        let key = self.tenant_key();

        // Each family resolves through its own single-flight gate, so an OS-only
        // query never waits on an in-flight third-party fetch it doesn't need.
        //
        // `join!`, not `try_join!`: the two families are independent and each caches
        // its own result, so cancelling the sibling on the first error threw away a
        // whole-fleet feed that had already been paged — the third-party one runs to
        // six figures and is usually the longest fetch in the query. Both now finish
        // and cache; the `?` below still fails the call on either error.
        let (os_hit, sw_hit) = tokio::join!(
            async {
                match include_os {
                    true => self
                        .current_family(
                            &self.fleet_current_os,
                            &self.os_fetch_lock,
                            &key,
                            force,
                            || self.api.fleet_os_patches(None, None, on_os),
                        )
                        .await
                        .map(Some),
                    false => Ok(None),
                }
            },
            async {
                match include_sw {
                    true => self
                        .current_family(
                            &self.fleet_current_sw,
                            &self.sw_fetch_lock,
                            &key,
                            force,
                            || self.api.fleet_software_patches(None, None, on_sw),
                        )
                        .await
                        .map(Some),
                    false => Ok(None),
                }
            },
        );
        let (os_hit, sw_hit) = (os_hit?, sw_hit?);

        let (os, os_at) = match os_hit {
            Some((rows, at)) => (rows, Some(at)),
            None => (Arc::new(Vec::new()), None),
        };
        let (sw, sw_at) = match sw_hit {
            Some((rows, at)) => (rows, Some(at)),
            None => (Arc::new(Vec::new()), None),
        };

        Ok(CurrentPatches {
            os,
            sw,
            // The UI's "patch data as of …" must not over-promise: with two families
            // fetched at different times the data as a whole is only as fresh as the
            // older one.
            fetched_at: [os_at, sw_at]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or_else(Utc::now),
        })
    }

    /// Serves one current-patch family from its slot, fetching it at most once
    /// across concurrent callers.
    ///
    /// Three guards compose here, in this order: the cheap pre-lock probe, the
    /// single-flight gate (so the loser waits instead of re-paging a six-figure
    /// feed), and the re-check under that gate — which is what turns the wait into a
    /// cache hit. The store is then epoch-gated for the same reason
    /// [`Self::fleet_devices`] is: a mutating action's `invalidate_current_patches`
    /// landing during the await must not be undone by this write, or
    /// [`CURRENT_PATCHES_TTL`] restarts on pre-action rows.
    async fn current_family<F, Fut>(
        &self,
        slot: &Mutex<Option<FamilyCache>>,
        fetch_lock: &tokio::sync::Mutex<()>,
        key: &TenantKey,
        force: bool,
        fetch: F,
    ) -> Result<(Arc<Vec<Patch>>, DateTime<Utc>)>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<Patch>>>,
    {
        if let Some(hit) = Self::fresh_family(slot, key, force) {
            return Ok(hit);
        }
        let _fetching = fetch_lock.lock().await;
        if let Some(hit) = Self::fresh_family(slot, key, force) {
            return Ok(hit);
        }
        let epoch = self.current_epoch.load(Ordering::SeqCst);
        let patches = Arc::new(fetch().await?);
        let now = Utc::now();
        if let Ok(mut guard) = slot.lock() {
            if self.current_epoch.load(Ordering::SeqCst) == epoch {
                *guard = Some(FamilyCache {
                    at: Instant::now(),
                    tenant: key.clone(),
                    fetched_at: now,
                    patches: patches.clone(),
                });
            } else {
                tracing::debug!("current patches invalidated mid-fetch; not caching");
            }
        }
        Ok((patches, now))
    }

    /// A live entry in one family slot, or `None` on a miss / stale. Shared by the
    /// probe and the post-gate re-check so the two cannot disagree.
    fn fresh_family(
        slot: &Mutex<Option<FamilyCache>>,
        key: &TenantKey,
        force: bool,
    ) -> Option<(Arc<Vec<Patch>>, DateTime<Utc>)> {
        let guard = slot.lock().ok()?;
        let c = guard.as_ref()?;
        c.is_fresh(key, force)
            .then(|| (c.patches.clone(), c.fetched_at))
    }

    /// Drops cached lookups so a different tenant (after sign-out or an instance
    /// change) doesn't see stale org/location/role names. Also drops the whole-fleet
    /// device/patch caches, which are likewise tenant-scoped.
    pub fn clear_lookups_cache(&self) {
        // Bump *before* clearing, so both interleavings lose the racing write: a
        // fetch that stores before the clear is wiped by it, and one that stores
        // after sees the new epoch and declines. This slot used to be cleared bare,
        // which is precisely what the comment below says is wrong for the other two
        // — an in-flight lookups fetch wrote its rows straight back and restarted
        // LOOKUP_TTL on them. The tenant stamp cannot catch it, because the case it
        // happens on (a same-tenant sign-out) has the same stamp.
        self.lookups_epoch.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = self.lookups_cache.lock() {
            *guard = None;
        }
        // Through the epoch-bumping invalidators, not by clearing the slots here — a
        // tenant switch is exactly when a long whole-fleet fetch is likely still in
        // flight, and a bare clear would let it store its rows straight back.
        self.invalidate_fleet_devices();
        self.invalidate_current_patches();
    }

    /// Claims the next query generation and records the tenant the query is starting
    /// under. Call once at the *start* of a query and hand the token back to
    /// [`store_last_result_if_current`].
    ///
    /// [`store_last_result_if_current`]: Self::store_last_result_if_current
    pub fn begin_query(&self) -> QueryToken {
        QueryToken {
            generation: self.query_generation.fetch_add(1, Ordering::SeqCst) + 1,
            tenant: self.tenant_key(),
            result_epoch: self.result_epoch.load(Ordering::SeqCst),
        }
    }

    /// Stores a query result for paging and export, **unless** a newer query has
    /// started or the tenant changed while this one was in flight. Returns whether
    /// the write happened.
    ///
    /// Three races close here, and they need different treatment:
    ///
    /// *Supersession.* Ordering by completion rather than by start let an
    /// auto-refresh tick clobber a manual Run: the two overlap routinely, a warm
    /// cache can let either finish first, and the frontend renders the summary of the
    /// run *it* started last. Dropping the superseded write keeps the cache — read by
    /// export, the HTML report and row paging — consistent with the summary on
    /// screen.
    ///
    /// *Tenant drift.* The stamp is taken from the token, i.e. the tenant the query
    /// started under, not from the tenant that happens to be current now. A
    /// whole-fleet fetch runs for minutes; stamping at write time meant a result
    /// fetched under the old tenant could be labelled with the new one if the
    /// operator switched instance mid-query — the one way the tenant defense could be
    /// *wrong* rather than merely miss. A drifted result is dropped rather than
    /// stored under either key.
    ///
    /// *Session clearing.* A sign-out, sign-in or re-authorization bumps
    /// `result_epoch` before it clears the slot. The tenant stamp is blind to this —
    /// a second operator on the same instance produces an identical `TenantKey` — and
    /// so is the generation, since clearing the session starts no new query. Without
    /// the epoch, an in-flight whole-fleet query simply stored the departing
    /// operator's rows back over the clear.
    ///
    /// A poisoned cache is warned (not panicked) so the failure is observable but the
    /// app survives.
    pub fn store_last_result_if_current(
        &self,
        token: QueryToken,
        result: QueryResult,
    ) -> StoreOutcome {
        if token.tenant != self.tenant_key() {
            return StoreOutcome::TenantChanged;
        }
        match self.last_result.lock() {
            Ok(mut slot) => {
                // Checked under the lock so a query that started between the caller's
                // last look and here cannot still lose to us.
                if self.query_generation.load(Ordering::SeqCst) != token.generation {
                    return StoreOutcome::Superseded;
                }
                // Read under the slot lock, exactly like the three fleet caches: the
                // invalidator bumps *before* it clears, so a store landing either side
                // of the clear sees the new epoch and declines. Without this the result
                // slot was the one place a sign-out could be silently undone.
                if self.result_epoch.load(Ordering::SeqCst) != token.result_epoch {
                    return StoreOutcome::SessionCleared;
                }
                *slot = Some(CachedResult {
                    tenant: token.tenant,
                    result: Arc::new(result),
                    // Both memos are built on first request, not here: most queries
                    // are never grouped or re-sorted, and doing either over the whole
                    // fleet eagerly would pay for a view the operator may not open.
                    groups: None,
                    sorted: None,
                });
                StoreOutcome::Stored
            }
            // Once poisoned the slot stays poisoned: `with_current_result` returns
            // `Err(CachePoisoned)`, so export, the HTML report and row paging all
            // fail outright rather than serving the prior query. Warn so that shows
            // up in the log as the cause.
            Err(_) => {
                warn!("result cache poisoned; export, report and paging will now fail");
                StoreOutcome::Poisoned
            }
        }
    }

    /// Runs `f` against the cached result **iff** it belongs to the current tenant,
    /// under the lock (keep `f` cheap — no `.await`). `Ok(None)` = nothing cached for
    /// this tenant (never queried, or a tenant switch invalidated it); `Err` = a
    /// poisoned lock. The sole read path, so the tenant check can't be bypassed.
    pub fn with_current_result<T>(
        &self,
        f: impl FnOnce(&QueryResult) -> T,
    ) -> Result<Option<T>, CachePoisoned> {
        let key = self.tenant_key();
        let guard = self.last_result.lock().map_err(|_| CachePoisoned)?;
        Ok(match guard.as_ref() {
            Some(c) if c.tenant == key => Some(f(&c.result)),
            _ => None,
        })
    }

    /// Takes a *handle* on the cached result for the current tenant, releasing the
    /// lock immediately.
    ///
    /// For readers that need the whole result for a long time — the Excel export and
    /// the HTML report, both of which then hand it to `spawn_blocking` — this is the
    /// right shape: an `Arc` bump under the lock instead of an O(rows) deep copy that
    /// blocks every paging command for its duration. Prefer
    /// [`Self::with_current_result`] when a cheap projection will do.
    pub fn current_result_handle(&self) -> Result<Option<Arc<QueryResult>>, CachePoisoned> {
        let key = self.tenant_key();
        let guard = self.last_result.lock().map_err(|_| CachePoisoned)?;
        Ok(match guard.as_ref() {
            Some(c) if c.tenant == key => Some(Arc::clone(&c.result)),
            _ => None,
        })
    }

    /// Runs `f` against the cached rows and the index permutation for `sort`,
    /// building that permutation at most once per sort.
    ///
    /// `None` passes `None` through rather than materializing an identity
    /// permutation — the unsorted view is already in cache order, and a fleet that
    /// never asked to be re-sorted should not pay four bytes a row to say so.
    ///
    /// Mirrors [`Self::with_grouped_result`], including reading as a miss on a tenant
    /// switch.
    pub fn with_sorted_result<T>(
        &self,
        sort: Option<RowSort>,
        f: impl FnOnce(&[PatchRow], Option<&[u32]>) -> T,
    ) -> Result<Option<T>, CachePoisoned> {
        let key = self.tenant_key();
        let mut guard = self.last_result.lock().map_err(|_| CachePoisoned)?;
        let Some(cached) = guard.as_mut() else {
            return Ok(None);
        };
        if cached.tenant != key {
            return Ok(None);
        }
        let Some(sort) = sort else {
            return Ok(Some(f(&cached.result.rows, None)));
        };
        // Built and bound in one step. This used to repopulate the memo and then
        // re-read it with `.expect("just populated")` — a panic inside a held guard,
        // which poisons the slot and takes export, the report and all three paging
        // commands down with it. That exact pattern was deliberately removed from
        // `append_jobs`; there is no reason to keep it here.
        let order = match &cached.sorted {
            Some((s, o)) if *s == sort => Arc::clone(o),
            _ => {
                let o = Arc::new(sort_order(&cached.result.rows, sort));
                cached.sorted = Some((sort, Arc::clone(&o)));
                o
            }
        };
        Ok(Some(f(&cached.result.rows, Some(&order))))
    }

    /// Runs `f` against one page of group headers, building the grouping only when
    /// the memo does not already hold this `group_by`.
    ///
    /// Same tenant check and same lock as [`Self::with_current_result`]; the memo is
    /// stored inside the result, so replacing or clearing the result discards it
    /// automatically. Switching between *By device* and *By patch* rebuilds once and
    /// then pages freely, which is the access pattern the Patches tab actually has.
    pub fn with_grouped_result<T>(
        &self,
        group_by: GroupBy,
        f: impl FnOnce(&[PatchGroup]) -> T,
    ) -> Result<Option<T>, CachePoisoned> {
        let key = self.tenant_key();
        let mut guard = self.last_result.lock().map_err(|_| CachePoisoned)?;
        let Some(cached) = guard.as_mut() else {
            return Ok(None);
        };
        if cached.tenant != key {
            return Ok(None);
        }
        // Same non-panicking shape as `with_sorted_result`.
        let groups = match &cached.groups {
            Some((g, v)) if *g == group_by => Arc::clone(v),
            _ => {
                let v = Arc::new(build_groups(&cached.result.rows, group_by));
                cached.groups = Some((group_by, Arc::clone(&v)));
                v
            }
        };
        Ok(Some(f(&groups)))
    }

    /// Drops the cached query result after sign-out or an instance change. The tenant
    /// stamp already makes a stale read impossible (a switch reads as a miss); this
    /// reclaims the memory promptly and wipes rows on an explicit sign-out of the same
    /// tenant, which the stamp alone would not.
    pub fn clear_last_result(&self) {
        // Bumped *before* the slot is cleared, for the same reason
        // `invalidate_current_patches` does it: a whole-fleet query runs for minutes,
        // so one is routinely in flight at sign-out. The store re-reads this under the
        // slot lock, so whichever order the two interleave, the write loses.
        self.result_epoch.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut slot) = self.last_result.lock() {
            *slot = None;
        }
    }

    /// Drops **only** the whole-fleet current-patch caches — both families, since an
    /// apply can move either.
    ///
    /// Called after a mutating action: the device's pending list is about to
    /// change, and [`CURRENT_PATCHES_TTL`] would otherwise keep serving pre-action
    /// data for up to two minutes. `clear_lookups_cache` is the wrong tool here —
    /// it also drops the 15-minute device inventory and the org/location/role
    /// lookups, neither of which a patch action can affect.
    pub fn invalidate_current_patches(&self) {
        // Bumped *before* the slots are cleared. A fetch that is about to store
        // re-reads the epoch under the slot lock, so whichever order the two
        // interleave it sees the bump and drops its write — clearing alone would
        // leave the window this method exists to close.
        self.current_epoch.fetch_add(1, Ordering::SeqCst);
        for slot in [&self.fleet_current_os, &self.fleet_current_sw] {
            if let Ok(mut guard) = slot.lock() {
                *guard = None;
            }
        }
    }

    /// Drops **only** the device inventory. A reboot flips `os.needsReboot`, and
    /// [`DEVICE_TTL`] is 15 minutes — long enough to render the reboot invisible.
    pub fn invalidate_fleet_devices(&self) {
        // Same ordering as `invalidate_current_patches`: bump, then clear.
        self.devices_epoch.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = self.fleet_devices_cache.lock() {
            *guard = None;
        }
    }

    /// The two cache epochs, for tests that assert an invalidation did (or did
    /// not) happen without racing a fetch.
    #[cfg(test)]
    pub(crate) fn cache_epochs(&self) -> (u64, u64) {
        (
            self.devices_epoch.load(Ordering::SeqCst),
            self.current_epoch.load(Ordering::SeqCst),
        )
    }

    /// Reserves `count` consecutive job ids, returning `(batch_id, first_job_id)`.
    pub fn next_job_ids(&self, count: usize) -> (u64, u64) {
        let batch = self.job_seq.fetch_add(1, Ordering::Relaxed);
        let base = self.job_seq.fetch_add(count as u64, Ordering::Relaxed);
        (batch, base)
    }

    /// Appends newly dispatched rows for the current tenant, trimming history to
    /// [`MAX_JOBS`] by dropping the oldest **terminal** rows first — an in-flight
    /// job must never be evicted out from under the poller.
    pub fn append_jobs(&self, new_jobs: Vec<JobReport>) {
        let key = self.tenant_key();
        let Ok(mut guard) = self.jobs.lock() else {
            warn!("job store poisoned; dispatched jobs will not appear in the Jobs tab");
            return;
        };
        // `insert` hands back the `&mut` directly, so the re-lookup that needed an
        // `expect` is gone. That expect was the only one in production code, and it
        // sat inside a held guard — a panic there would have poisoned the job store
        // for the rest of the process, which the arm above warns is unrecoverable.
        let jobs = match guard.as_mut() {
            Some((t, jobs)) if *t == key => jobs,
            _ => &mut guard.insert((key, Vec::new())).1,
        };
        jobs.extend(new_jobs);

        if jobs.len() > MAX_JOBS {
            let excess = jobs.len() - MAX_JOBS;
            let mut dropped = 0;
            jobs.retain(|j| {
                if dropped < excess && j.state.is_terminal() {
                    dropped += 1;
                    return false;
                }
                true
            });
        }
    }

    /// Applies polled updates, matching on `JobReport.id`. Rows the caller no
    /// longer knows about are left untouched.
    pub fn apply_job_updates(&self, updates: Vec<JobReport>) {
        let key = self.tenant_key();
        let Ok(mut guard) = self.jobs.lock() else {
            return;
        };
        let Some((t, jobs)) = guard.as_mut() else {
            return;
        };
        if *t != key {
            return;
        }
        for update in updates {
            if let Some(slot) = jobs.iter_mut().find(|j| j.id == update.id) {
                *slot = update;
            }
        }
    }

    /// Clone-out of the jobs still awaiting a terminal state. Returns owned rows so
    /// the lock is released before the poller's `.await`s.
    pub fn pending_jobs(&self) -> Vec<JobReport> {
        self.jobs_snapshot()
            .into_iter()
            .filter(|j| !j.state.is_terminal())
            .collect()
    }

    /// All jobs for the current tenant, newest last. Empty after a tenant switch.
    pub fn jobs_snapshot(&self) -> Vec<JobReport> {
        let key = self.tenant_key();
        self.jobs
            .lock()
            .ok()
            .and_then(|g| match g.as_ref() {
                Some((t, jobs)) if *t == key => Some(jobs.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Drops dispatch history on sign-out or an instance change.
    pub fn clear_jobs(&self) {
        if let Ok(mut guard) = self.jobs.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.pending_confirm.lock() {
            *guard = None;
        }
    }

    /// Claims the single poller slot. `None` means one is already running and the
    /// caller should just let it pick up the new batch.
    ///
    /// The claim is an RAII guard: dropping it releases the slot. That is what makes
    /// the flag safe to hold across a long-running task — see [`JobPollerClaim`].
    pub fn try_claim_job_poller(&self) -> Option<JobPollerClaim> {
        self.job_poller_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then(|| JobPollerClaim {
                flag: Arc::clone(&self.job_poller_running),
            })
    }

    /// Releases the poller claim **only if** no unresolved job remains. Returns
    /// `None` when the claim was released and the caller should stop polling, and
    /// `Some(claim)` — the claim handed back — when work appeared and it must keep
    /// going.
    ///
    /// Closes a lost-wakeup race. The poller used to break out of its loop on an
    /// empty pending set and release the claim afterwards; a batch dispatched in
    /// that gap recorded its jobs, then found `try_claim_job_poller` still taken
    /// because the flag had not been cleared yet — so nothing polled it, and those
    /// jobs sat unresolved until some later dispatch happened to start a new poller.
    ///
    /// The check and the release happen under the jobs lock, and dispatch records
    /// its jobs before calling `try_claim_job_poller`. A concurrent dispatch is
    /// therefore either visible here (we keep polling) or strictly after the release
    /// (its own claim succeeds). There is no order in which it is neither.
    pub fn release_job_poller_if_idle(&self, claim: JobPollerClaim) -> Option<JobPollerClaim> {
        let Ok(guard) = self.jobs.lock() else {
            // A poisoned job store cannot be polled meaningfully; release so a
            // later dispatch can at least try.
            drop(claim);
            return None;
        };
        let key = self.tenant_key();
        let has_pending = guard.as_ref().is_some_and(|(tenant, jobs)| {
            *tenant == key && jobs.iter().any(|j| !j.state.is_terminal())
        });
        if has_pending {
            return Some(claim);
        }
        // Dropped before `guard`, so the release still happens under the jobs lock —
        // that ordering is the whole point of the check above.
        drop(claim);
        drop(guard);
        None
    }

    /// Records the plan the operator is being asked to confirm, replacing any
    /// earlier one — only one dialog is open at a time.
    pub fn store_pending_confirm(&self, token: String, request_hash: String) {
        let tenant = self.tenant_key();
        if let Ok(mut guard) = self.pending_confirm.lock() {
            *guard = Some(PendingConfirm {
                token,
                request_hash,
                issued_at: Instant::now(),
                tenant,
            });
        }
    }

    /// Consumes a confirmation token, returning whether it authorizes this exact
    /// request. Single-use: the slot is cleared on any match attempt, so a
    /// double-click can't dispatch twice.
    ///
    /// Both secrets are compared in constant time. The realistic threat here is low —
    /// the slot holds one token at a time, it is single-use, it expires in five
    /// minutes, and the only party that can present one is the frontend that was
    /// handed it — but `==` on a `String` returns on the first differing byte, and
    /// this is the gate standing between a stale or modified frontend and a fleet-wide
    /// reboot. A comparison that leaks nothing costs nothing here.
    pub fn consume_confirm_token(&self, token: &str, request_hash: &str) -> bool {
        let Ok(mut guard) = self.pending_confirm.lock() else {
            return false;
        };
        let Some(pending) = guard.take() else {
            return false;
        };
        // Not short-circuiting: both comparisons run regardless of the first result.
        let token_ok = constant_time_eq(pending.token.as_bytes(), token.as_bytes());
        let hash_ok = constant_time_eq(pending.request_hash.as_bytes(), request_hash.as_bytes());
        // The tenant is compared here rather than folded into `request_hash` because
        // `ActionRequest` has no field to hash it from — see `PendingConfirm::tenant`.
        // An approval is for one instance; the operator can change instance in
        // Settings while the dialog is open.
        token_ok & hash_ok
            && pending.tenant == self.tenant_key()
            && pending.issued_at.elapsed() < CONFIRM_TTL
    }
}

#[cfg(test)]
impl AppState {
    /// An already-authenticated state whose API client points at `base_url`, for
    /// tests that exercise the whole-fleet caches against a mock NinjaOne server.
    /// Mirrors [`AuthState::seeded`]; the settings carry the same base url so the
    /// tenant stamp matches what the caches record.
    fn seeded(base_url: String) -> Self {
        let http = reqwest::Client::new();
        let auth = AuthState::seeded(http.clone(), base_url.clone(), "test-token");
        let api = NinjaApiClient::new(http, auth.clone());
        let settings = Settings {
            instance_base_url: base_url,
            ..Settings::default()
        };
        Self {
            auth,
            api,
            settings: Mutex::new(settings),
            last_result: Mutex::new(None),
            lookups_cache: Mutex::new(None),
            fleet_devices_cache: Mutex::new(None),
            fleet_current_os: Mutex::new(None),
            fleet_current_sw: Mutex::new(None),
            jobs: Mutex::new(None),
            job_seq: AtomicU64::new(1),
            query_generation: AtomicU64::new(0),
            lookups_epoch: AtomicU64::new(0),
            devices_epoch: AtomicU64::new(0),
            current_epoch: AtomicU64::new(0),
            result_epoch: AtomicU64::new(0),
            devices_fetch_lock: tokio::sync::Mutex::new(()),
            os_fetch_lock: tokio::sync::Mutex::new(()),
            sw_fetch_lock: tokio::sync::Mutex::new(()),
            job_poller_running: Arc::new(AtomicBool::new(false)),
            pending_confirm: Mutex::new(None),
        }
    }
}

/// Byte-equality that does not return early on the first difference.
///
/// The length check is deliberately *not* constant time — the length of a token is
/// not the secret, and both sides here are fixed-width by construction.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests;
