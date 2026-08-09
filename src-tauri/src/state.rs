use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::warn;

use crate::actions::{JobReport, MAX_JOBS};
use crate::api::{NinjaApiClient, ProgressFn};
use crate::auth::AuthState;
use crate::model::{Device, Location, Organization, Patch, Role};
use crate::rows::QueryResult;
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
    last_result: Mutex<Option<(TenantKey, QueryResult)>>,
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
    devices_epoch: AtomicU64,
    /// The same guard for both current-patch families. Bumped by
    /// [`AppState::invalidate_current_patches`], which a mutating action calls — and
    /// whose whole purpose is defeated if an in-flight fetch stores over it.
    current_epoch: AtomicU64,
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
            devices_epoch: AtomicU64::new(0),
            current_epoch: AtomicU64::new(0),
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
            *guard = Some(LookupCache {
                at: Instant::now(),
                tenant: key,
                orgs: orgs.clone(),
                locations: locations.clone(),
                roles: roles.clone(),
            });
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
        let (os_hit, sw_hit) = tokio::try_join!(
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
        )?;

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
        }
    }

    /// Stores a query result for paging and export, **unless** a newer query has
    /// started or the tenant changed while this one was in flight. Returns whether
    /// the write happened.
    ///
    /// Two races close here, and they need opposite treatment:
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
                *slot = Some((token.tenant, result));
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
            Some((t, r)) if *t == key => Some(f(r)),
            _ => None,
        })
    }

    /// Drops the cached query result after sign-out or an instance change. The tenant
    /// stamp already makes a stale read impossible (a switch reads as a miss); this
    /// reclaims the memory promptly and wipes rows on an explicit sign-out of the same
    /// tenant, which the stamp alone would not.
    pub fn clear_last_result(&self) {
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
        if let Ok(mut guard) = self.pending_confirm.lock() {
            *guard = Some(PendingConfirm {
                token,
                request_hash,
                issued_at: Instant::now(),
            });
        }
    }

    /// Consumes a confirmation token, returning whether it authorizes this exact
    /// request. Single-use: the slot is cleared on any match attempt, so a
    /// double-click can't dispatch twice.
    pub fn consume_confirm_token(&self, token: &str, request_hash: &str) -> bool {
        let Ok(mut guard) = self.pending_confirm.lock() else {
            return false;
        };
        let Some(pending) = guard.take() else {
            return false;
        };
        pending.token == token
            && pending.request_hash == request_hash
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
            devices_epoch: AtomicU64::new(0),
            current_epoch: AtomicU64::new(0),
            devices_fetch_lock: tokio::sync::Mutex::new(()),
            os_fetch_lock: tokio::sync::Mutex::new(()),
            sw_fetch_lock: tokio::sync::Mutex::new(()),
            job_poller_running: Arc::new(AtomicBool::new(false)),
            pending_confirm: Mutex::new(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ActionKind, JobState};
    use crate::rows::QueryResult;

    fn sample_result() -> QueryResult {
        QueryResult {
            rows: Vec::new(),
            devices: Vec::new(),
            compliance: Vec::new(),
            compliance_by_os: Vec::new(),
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            devices_total: 0,
            generated_at: "2026-01-01 00:00:00 UTC".into(),
            data_fetched_at: "2026-01-01 00:00:00 UTC".into(),
        }
    }

    /// A mock exposing both current-patch feeds, each counting its own hits.
    async fn patch_feed_server() -> wiremock::MockServer {
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        for p in [
            "/api/v2/queries/os-patches",
            "/api/v2/queries/software-patches",
        ] {
            Mock::given(method("GET"))
                .and(path(p))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "results": [{ "id": 1, "deviceId": 1, "kbNumber": "KB1" }],
                    "cursor": ""
                })))
                .mount(&server)
                .await;
        }
        server
    }

    /// How many requests the mock saw for `path`.
    async fn hits(server: &wiremock::MockServer, path: &str) -> usize {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == path)
            .count()
    }

    #[tokio::test]
    async fn an_os_only_query_never_fetches_the_third_party_feed() {
        let server = patch_feed_server().await;
        let state = AppState::seeded(server.uri());

        let current = state
            .fleet_current_patches(false, true, false, None, None)
            .await
            .expect("os-only fetch");

        assert_eq!(current.os.len(), 1, "the requested family is fetched");
        assert!(current.sw.is_empty(), "the unrequested family stays empty");
        // The whole point: a whole-fleet third-party feed runs to six figures and is
        // usually the largest fetch in a query, so an OS-only query must not page it
        // just to discard it.
        assert_eq!(
            hits(&server, "/api/v2/queries/software-patches").await,
            0,
            "software-patches must not be requested at all"
        );
    }

    #[tokio::test]
    async fn widening_to_both_families_reuses_the_already_warm_one() {
        let server = patch_feed_server().await;
        let state = AppState::seeded(server.uri());

        state
            .fleet_current_patches(false, true, false, None, None)
            .await
            .expect("os-only fetch");
        let both = state
            .fleet_current_patches(false, true, true, None, None)
            .await
            .expect("widened fetch");

        assert_eq!(both.os.len(), 1);
        assert_eq!(both.sw.len(), 1);
        // Splitting the cache per family must not cost a refetch of the family that
        // was already warm.
        assert_eq!(
            hits(&server, "/api/v2/queries/os-patches").await,
            1,
            "the warm OS family must be served from cache"
        );
        assert_eq!(hits(&server, "/api/v2/queries/software-patches").await, 1);
    }

    #[tokio::test]
    async fn a_forced_refetch_inside_the_floor_is_served_from_cache() {
        let server = patch_feed_server().await;
        let state = AppState::seeded(server.uri());

        for _ in 0..3 {
            state
                .fleet_current_patches(true, true, false, None, None)
                .await
                .expect("forced fetch");
        }

        // `force` bypasses CURRENT_PATCHES_TTL so a patching operator can pull fresh
        // state — but unbounded it makes the cache decorative on the auto-refresh
        // path, which runs unattended for hours. FORCE_MIN_INTERVAL is the floor.
        assert_eq!(
            hits(&server, "/api/v2/queries/os-patches").await,
            1,
            "forced refetches inside FORCE_MIN_INTERVAL must collapse onto the cache"
        );
    }

    /// A whole-fleet feed takes long enough that a mutating action routinely lands
    /// mid-fetch. `invalidate_current_patches` exists precisely so the next query
    /// re-reads post-action state — but the fetch stored unconditionally after its
    /// await, so it wrote the pre-action rows straight back and CURRENT_PATCHES_TTL
    /// restarted on them. The tenant stamp cannot catch this: it is the same tenant.
    #[tokio::test]
    async fn an_invalidation_during_a_fetch_is_not_undone_by_that_fetch() {
        use std::time::Duration as StdDuration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patches"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({
                        "results": [{ "id": 1, "deviceId": 1, "kbNumber": "KB1" }],
                        "cursor": ""
                    }))
                    // Long enough for the action below to land while this is in flight.
                    .set_delay(StdDuration::from_millis(150)),
            )
            .mount(&server)
            .await;
        let state = Arc::new(AppState::seeded(server.uri()));

        let fetching = {
            let state = state.clone();
            tokio::spawn(async move {
                state
                    .fleet_current_patches(false, true, false, None, None)
                    .await
                    .expect("in-flight fetch")
            })
        };
        // Stand in for `run_action` completing an apply while the fetch is paging.
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        state.invalidate_current_patches();
        fetching.await.expect("join");

        // The in-flight fetch may still return its rows to its own caller — it has
        // them — but it must not leave them in the cache for the *next* query.
        state
            .fleet_current_patches(false, true, false, None, None)
            .await
            .expect("post-action fetch");
        assert_eq!(
            hits(&server, "/api/v2/queries/os-patches").await,
            2,
            "the query after the action must re-fetch, not be served pre-action rows"
        );
    }

    /// Queries overlap by design (an auto-refresh tick fires while a manual Run is
    /// still paging), so on a cold cache both used to page the entire inventory
    /// independently — the largest fetch in the app, run twice.
    #[tokio::test]
    async fn concurrent_cold_fetches_collapse_onto_one_request() {
        use std::time::Duration as StdDuration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/devices-detailed"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{ "id": 1, "systemName": "web-01" }]))
                    .set_delay(StdDuration::from_millis(100)),
            )
            .mount(&server)
            .await;
        let state = Arc::new(AppState::seeded(server.uri()));

        let (a, b) = tokio::join!(
            {
                let state = state.clone();
                async move { state.fleet_devices(None).await.expect("first") }
            },
            {
                let state = state.clone();
                async move { state.fleet_devices(None).await.expect("second") }
            }
        );

        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(
            hits(&server, "/api/v2/devices-detailed").await,
            1,
            "the second caller must wait on the first fetch, not start its own"
        );
    }

    #[test]
    fn last_result_cache_starts_empty_and_clears() {
        let state = AppState::new().expect("build state");
        // A fresh state has no cached result, so export errors with "Run a query
        // before exporting" rather than writing a stale workbook.
        assert!(state.with_current_result(|_| ()).unwrap().is_none());

        state.store_last_result_if_current(state.begin_query(), sample_result());
        assert!(state.with_current_result(|_| ()).unwrap().is_some());

        // Sign-out / instance change drops the cache so a later export can't leak a
        // previous tenant's rows.
        state.clear_last_result();
        assert!(state.with_current_result(|_| ()).unwrap().is_none());
    }

    /// An auto-refresh tick and a manual Run overlap routinely and do not finish in
    /// start order. The run that started last owns the cache, because that is the one
    /// whose summary the frontend renders — otherwise the visible table and the rows
    /// behind paging/export come from different queries.
    #[test]
    fn a_superseded_query_does_not_clobber_a_newer_one() {
        let state = AppState::new().expect("build state");

        let first = state.begin_query();
        let second = state.begin_query();

        // The newer run finishes first.
        assert_eq!(
            state.store_last_result_if_current(second, sample_result()),
            StoreOutcome::Stored
        );
        // The older run finishes second and must NOT overwrite it.
        assert_eq!(
            state.store_last_result_if_current(first, sample_result()),
            StoreOutcome::Superseded,
            "a query superseded before it finished must drop its cache write"
        );
        assert!(state.with_current_result(|_| ()).unwrap().is_some());
    }

    /// Starting a run retires the previous generation even if that run never
    /// completes, so a late arrival from an abandoned query is dropped rather than
    /// resurrecting rows the operator has moved on from.
    #[test]
    fn a_query_retired_by_a_later_start_is_dropped_even_if_the_later_one_never_finishes() {
        let state = AppState::new().expect("build state");

        let abandoned = state.begin_query();
        let _newer = state.begin_query(); // started, never stored

        assert_eq!(
            state.store_last_result_if_current(abandoned, sample_result()),
            StoreOutcome::Superseded
        );
        assert!(state.with_current_result(|_| ()).unwrap().is_none());
    }

    #[test]
    fn a_lone_query_stores_normally() {
        let state = AppState::new().expect("build state");
        let only = state.begin_query();
        assert_eq!(
            state.store_last_result_if_current(only, sample_result()),
            StoreOutcome::Stored
        );
        assert!(state.with_current_result(|_| ()).unwrap().is_some());
    }

    /// A whole-fleet fetch runs for minutes. If the operator switches instance while
    /// one is in flight, the result belongs to the tenant it was *fetched* under —
    /// stamping it with whatever tenant is current at write time would file another
    /// tenant's rows under the new one, which is the one way the tenant check can be
    /// wrong rather than merely miss.
    #[test]
    fn a_query_that_spans_a_tenant_switch_is_dropped_not_restamped() {
        let state = AppState::new().expect("build state");
        let token = state.begin_query();

        // The operator switches instance while the query is still running.
        state.settings.lock().unwrap().instance_base_url = "https://other.example.com".into();

        assert_eq!(
            state.store_last_result_if_current(token, sample_result()),
            StoreOutcome::TenantChanged,
            "a result fetched under the previous tenant must not be stored, and the              caller must be able to tell that apart from supersession"
        );
        assert!(
            state.with_current_result(|_| ()).unwrap().is_none(),
            "and it must not be readable under the new tenant either"
        );
    }

    #[test]
    fn last_result_invisible_after_instance_switch() {
        let state = AppState::new().expect("build state");
        state.store_last_result_if_current(state.begin_query(), sample_result());
        assert!(state.with_current_result(|_| ()).unwrap().is_some());

        // Switch the instance WITHOUT calling clear_* — the read must still miss, so a
        // forgotten invalidation can't serve the previous tenant's rows.
        state.settings.lock().unwrap().instance_base_url = "https://other.example.com".into();
        assert!(
            state.with_current_result(|_| ()).unwrap().is_none(),
            "a tenant switch must invalidate the cached result at read time"
        );
    }

    #[test]
    fn last_result_invisible_after_client_id_switch() {
        // Pre-refactor, only an instance-URL change invalidated the result, so
        // switching to a different client id (app registration) left the prior rows
        // exportable. Tenant-keyed reads close that gap.
        let state = AppState::new().expect("build state");
        state.store_last_result_if_current(state.begin_query(), sample_result());
        assert!(state.with_current_result(|_| ()).unwrap().is_some());

        state.settings.lock().unwrap().client_id = Some("different-client".into());
        assert!(state.with_current_result(|_| ()).unwrap().is_none());
    }

    fn sample_job(id: u64, state: JobState) -> JobReport {
        JobReport {
            id,
            batch_id: 1,
            device_id: 7,
            device_name: "srv-1".into(),
            organization: "Contoso".into(),
            kind: ActionKind::OsPatchApply,
            detail: "Apply OS patches".into(),
            dry_run: false,
            state,
            dispatched_at: "2026-01-01 00:00:00 UTC".into(),
            dispatched_ts: 0,
            finished_at: None,
            duration_seconds: None,
            activity_id: None,
            series_uid: None,
            exit_code: None,
        }
    }

    #[test]
    fn jobs_are_invisible_after_an_instance_switch() {
        let state = AppState::new().expect("build state");
        state.append_jobs(vec![sample_job(1, JobState::Running)]);
        assert_eq!(state.jobs_snapshot().len(), 1);

        // Same guarantee as `last_result`: switching tenant WITHOUT calling clear_*
        // must read as a miss, so a forgotten invalidation can't surface another
        // tenant's dispatch history.
        state.settings.lock().unwrap().instance_base_url = "https://other.example.com".into();
        assert!(state.jobs_snapshot().is_empty());
        assert!(state.pending_jobs().is_empty());
    }

    #[test]
    fn job_updates_key_on_job_id_not_device_id() {
        let state = AppState::new().expect("build state");
        // Two rows for the SAME device, as happens when batches overlap.
        state.append_jobs(vec![
            sample_job(1, JobState::Running),
            sample_job(2, JobState::Running),
        ]);

        state.apply_job_updates(vec![sample_job(2, JobState::Completed)]);
        let jobs = state.jobs_snapshot();
        assert_eq!(jobs[0].state, JobState::Running, "row 1 must be untouched");
        assert_eq!(jobs[1].state, JobState::Completed);
        assert_eq!(state.pending_jobs().len(), 1);
    }

    #[test]
    fn job_history_evicts_terminal_rows_before_in_flight_ones() {
        let state = AppState::new().expect("build state");
        // One in-flight row, then enough terminal rows to overflow the cap.
        state.append_jobs(vec![sample_job(0, JobState::Running)]);
        let filler: Vec<JobReport> = (1..=MAX_JOBS as u64)
            .map(|i| sample_job(i, JobState::Completed))
            .collect();
        state.append_jobs(filler);

        let jobs = state.jobs_snapshot();
        assert_eq!(jobs.len(), MAX_JOBS);
        assert!(
            jobs.iter().any(|j| j.id == 0),
            "an in-flight job must never be evicted out from under the poller"
        );
    }

    #[test]
    fn confirm_token_is_single_use_and_bound_to_the_request() {
        let state = AppState::new().expect("build state");
        state.store_pending_confirm("tok".into(), "hash-a".into());

        // A token that doesn't match the request it was issued for is refused.
        assert!(!state.consume_confirm_token("tok", "hash-b"));
        // ...and that attempt already consumed the slot, so the correct pair now
        // fails too. Failing closed is the right direction for a dispatch gate.
        assert!(!state.consume_confirm_token("tok", "hash-a"));

        state.store_pending_confirm("tok2".into(), "hash-a".into());
        assert!(state.consume_confirm_token("tok2", "hash-a"));
        // Single use: a double-click can't dispatch twice.
        assert!(!state.consume_confirm_token("tok2", "hash-a"));
    }

    #[test]
    fn the_job_poller_slot_admits_only_one_claimant() {
        let state = AppState::new().expect("build state");
        let claim = state.try_claim_job_poller().expect("first claim");
        assert!(
            state.try_claim_job_poller().is_none(),
            "a second batch must join the running poller, not spawn another"
        );
        // Idle (no jobs recorded), so the claim is released and re-claimable.
        assert!(state.release_job_poller_if_idle(claim).is_none());
        assert!(state.try_claim_job_poller().is_some());
    }

    /// Dropping the claim on *any* path releases the slot. The flag used to be
    /// cleared only inside `release_job_poller_if_idle`, so a panic or a cancelled
    /// task leaked it permanently and every later poller returned immediately —
    /// silently ending all job polling for the life of the process.
    #[test]
    fn dropping_the_claim_releases_the_slot() {
        let state = AppState::new().expect("build state");
        {
            let _claim = state.try_claim_job_poller().expect("first claim");
            assert!(state.try_claim_job_poller().is_none(), "held while alive");
        }
        assert!(
            state.try_claim_job_poller().is_some(),
            "an unwound poller must not strand the slot"
        );
    }

    /// The claim outlives a panic, which is the case a plain `store(false)` at one
    /// call site cannot cover.
    #[test]
    fn a_panicking_poller_still_releases_the_slot() {
        let state = AppState::new().expect("build state");
        let claim = state.try_claim_job_poller().expect("first claim");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _held = claim;
            panic!("advance_job blew up");
        }));
        assert!(result.is_err(), "the panic must actually have happened");
        assert!(
            state.try_claim_job_poller().is_some(),
            "unwinding drops the claim, so the next dispatch can poll"
        );
    }

    /// The lost-wakeup race: the poller finds its pending set empty and moves to
    /// release, but a batch is dispatched in that gap. Its `try_claim` fails because
    /// the flag is still set, so if the poller released unconditionally those jobs
    /// would never be polled by anyone.
    #[test]
    fn the_poller_keeps_its_claim_when_work_arrives_during_release() {
        let state = AppState::new().expect("build state");
        let claim = state.try_claim_job_poller().expect("first claim");

        // A batch lands: its jobs are recorded before it tries to claim.
        state.append_jobs(vec![sample_job(1, JobState::Running)]);
        assert!(
            state.try_claim_job_poller().is_none(),
            "the running poller still holds the claim"
        );

        let claim = state
            .release_job_poller_if_idle(claim)
            .expect("an unresolved job must keep the poller alive rather than orphan it");
        assert!(
            state.try_claim_job_poller().is_none(),
            "and the claim must still be held"
        );

        // Once the job settles, the poller may retire.
        state.apply_job_updates(vec![sample_job(1, JobState::Completed)]);
        assert!(state.release_job_poller_if_idle(claim).is_none());
        assert!(state.try_claim_job_poller().is_some());
    }
}
