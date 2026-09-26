use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::warn;

use crate::actions::JobReport;
use crate::api::{NinjaApiClient, ProgressFn};
use crate::auth::AuthState;
#[cfg(test)]
use crate::model::PatchRow;
use crate::model::{Device, Location, Organization, Patch, Role};
#[cfg(test)]
use crate::rows::sort_order;
use crate::rows::{GroupBy, PatchGroup, QueryResult, RowSort};
use crate::settings::Settings;

mod cache;
mod jobs;

use cache::TenantCache;
use jobs::PendingConfirm;

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

/// What [`AppState::sort_memo`] / [`AppState::group_memo`] hand back: a handle on
/// the cached result, and the memo for the requested view when one is already built.
pub struct Memo<T> {
    pub result: Arc<QueryResult>,
    pub memo: Option<Arc<T>>,
}

/// The three reference lists, cached as one value.
///
/// One `TenantCache` entry rather than three, because they are fetched together and
/// are only ever meaningful together — a row labelled with this tenant's orgs and
/// the previous tenant's locations would be worse than no labels at all.
struct LookupSet {
    orgs: Vec<Organization>,
    locations: Vec<Location>,
    roles: Vec<Role>,
}

/// The two current-patch families live in separate `TenantCache` slots rather than
/// one, because they are wildly asymmetric — a whole-fleet third-party feed runs to
/// six figures and is usually the largest single fetch in a query — and `PatchType`
/// lets the operator ask for only one. Cached as a pair, an OS-only query still paged
/// the entire software feed and then discarded it, which also made that feed the
/// critical path: an OS-only query took about as long as an ALL query. Split, each
/// family is fetched only when the requested `PatchType` includes it, and a later
/// widening to ALL reuses whatever is already warm.
///
/// The TTL a family is served under depends on the caller: a re-filter accepts
/// [`CURRENT_PATCHES_TTL`], a forced refresh drops to [`FORCE_MIN_INTERVAL`] so a
/// fast auto-refresh cadence cannot become a refetch loop. That is why
/// [`TenantCache::get_or_fetch`] takes the TTL per call rather than holding one.
fn current_patches_ttl(force: bool) -> Duration {
    if force {
        FORCE_MIN_INTERVAL
    } else {
        CURRENT_PATCHES_TTL
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
    /// Serializes the *writers* of `settings` across their disk write. A save is a
    /// read-modify-write whose write is file (and keyring) I/O, so it cannot run
    /// under `settings` — that would block every reader, on whatever thread, for the
    /// duration. Held across `.await` (it is a `tokio` mutex, for exactly that),
    /// so two saves cannot interleave and lose one's change. Readers never take it.
    pub settings_write: tokio::sync::Mutex<()>,
    /// Last query result, stamped with the tenant it belongs to and cached so export
    /// and row paging read it without the frontend round-tripping all rows over IPC.
    /// Private on purpose: all access goes through `store_last_result` /
    /// `with_current_result`, which enforce the tenant check — a tenant switch reads
    /// as a miss, so a forgotten clear can't serve the previous tenant's rows.
    last_result: Mutex<Option<CachedResult>>,
    /// Near-static lookups (orgs/locations/roles) cached with a short TTL.
    lookups_cache: TenantCache<LookupSet>,
    /// Whole-fleet device inventory cached with a long TTL ([`DEVICE_TTL`]).
    fleet_devices_cache: TenantCache<Vec<Device>>,
    /// Whole-fleet current OS patches, cached so a re-filter recomputes without a
    /// refetch; refreshed on force or past [`CURRENT_PATCHES_TTL`].
    fleet_current_os: TenantCache<Vec<Patch>>,
    /// Whole-fleet current third-party patches. Held apart from the OS family so a
    /// query that doesn't ask for it never pays to fetch it.
    fleet_current_sw: TenantCache<Vec<Patch>>,
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
    /// The lookups/devices/current-patch slots each own their own epoch and
    /// single-flight gate inside [`TenantCache`]; only the result cache, whose
    /// protocol is generation-gated rather than TTL-gated, still keeps one here.
    /// The same guard for the result cache, which was the one tenant-scoped slot
    /// without it. `clear_last_result` used to clear the slot bare, so a whole-fleet
    /// query still in flight at sign-out redeemed a token whose generation and tenant
    /// were both still current and stored the signed-out operator's rows straight
    /// back — after which export, the HTML report and all three paging commands
    /// served them to whoever signed in next. `TenantKey` cannot cover this either:
    /// a second operator on the same instance is the same tenant.
    result_epoch: AtomicU64,
    /// At most one poller at a time, so a burst of batches doesn't spawn N tasks
    /// all hammering `/activities`. Held behind an [`Arc`] so [`JobPollerClaim`] can
    /// own a handle and clear it on drop.
    job_poller_running: Arc<AtomicBool>,
    /// Single-slot confirmation gate — one dialog is open at a time.
    pending_confirm: Mutex<Option<PendingConfirm>>,
}

impl AppState {
    pub fn new() -> Result<Self> {
        let settings = Settings::load_or_recover();

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
            settings_write: tokio::sync::Mutex::const_new(()),
            last_result: Mutex::new(None),
            lookups_cache: TenantCache::default(),
            fleet_devices_cache: TenantCache::default(),
            fleet_current_os: TenantCache::default(),
            fleet_current_sw: TenantCache::default(),
            jobs: Mutex::new(None),
            job_seq: AtomicU64::new(1),
            query_generation: AtomicU64::new(0),
            result_epoch: AtomicU64::new(0),
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

    /// Replaces the in-memory settings. Only a writer holding `settings_write`, and
    /// only after the new value is safely on disk, calls this.
    pub fn replace_settings(&self, next: Settings) {
        *self
            .settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
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
        let set = self.lookup_set().await?;
        // The three lists are cached as one entry but handed out separately, because
        // `list_orgs`/`list_locations`/`list_roles` each want only their own. Cloning
        // out of the shared `Arc<LookupSet>` costs one Vec copy per call; these are
        // the small near-static lookups, not the whole-fleet feeds, and the
        // alternative is leaking `LookupSet` into five call sites.
        Ok((
            Arc::new(set.orgs.clone()),
            Arc::new(set.locations.clone()),
            Arc::new(set.roles.clone()),
        ))
    }

    /// Organization id → name, read straight out of the cached lookups.
    ///
    /// For the action planner, which needs nothing else: going through
    /// [`Self::lookups`] cloned all three lists on every plan and every confirm just
    /// to read the org names.
    pub async fn org_names(&self) -> Result<HashMap<i64, String>> {
        let set = self.lookup_set().await?;
        Ok(set.orgs.iter().map(|o| (o.id, o.name.clone())).collect())
    }

    async fn lookup_set(&self) -> Result<Arc<LookupSet>> {
        let key = self.tenant_key();
        let (set, _) = self
            .lookups_cache
            .get_or_fetch(&key, LOOKUP_TTL, "lookups", || async {
                let (orgs, locations, roles) = tokio::try_join!(
                    self.api.organizations(),
                    async {
                        // Locations only supply optional row labels, so a failure
                        // here is non-fatal — fall back to none, but warn so a
                        // tenant-wide locations outage isn't silently rendered as
                        // blank location names.
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
                Ok(LookupSet {
                    orgs,
                    locations,
                    roles,
                })
            })
            .await?;
        Ok(set)
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
        let (devices, _) = self
            .fleet_devices_cache
            .get_or_fetch(&key, DEVICE_TTL, "device inventory", || {
                self.api.devices(None, on_progress)
            })
            .await?;
        Ok(devices)
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
                        .fleet_current_os
                        .get_or_fetch(
                            &key,
                            current_patches_ttl(force),
                            "current OS patches",
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
                        .fleet_current_sw
                        .get_or_fetch(
                            &key,
                            current_patches_ttl(force),
                            "current software patches",
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

    /// Drops cached lookups so a different tenant (after sign-out or an instance
    /// change) doesn't see stale org/location/role names. Also drops the whole-fleet
    /// device/patch caches, which are likewise tenant-scoped.
    pub fn clear_lookups_cache(&self) {
        // Bump-then-clear, so both interleavings of a racing write lose. This slot
        // used to be cleared bare — an in-flight lookups fetch wrote its rows
        // straight back and restarted LOOKUP_TTL on them, and the tenant stamp
        // cannot catch it because the case it happens on (a same-tenant sign-out)
        // has the same stamp. That ordering now lives in `TenantCache::invalidate`
        // rather than being restated per slot.
        self.lookups_cache.invalidate();
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

    /// Phase one of a memoized read: a handle on the current tenant's result plus
    /// the sort order memoized for `sort`, if there is one. The lock is held for two
    /// `Arc` bumps.
    ///
    /// The memo used to be *built* here, under the lock: the first sorted page of a
    /// six-figure fleet ran the whole `O(n log n)` sweep holding the mutex every
    /// paging command and the export take, on an async worker. Now a miss hands back
    /// the handle; the caller builds the order off the runtime
    /// (`spawn_blocking`) and offers it back through [`Self::store_sort_memo`].
    pub fn sort_memo(&self, sort: RowSort) -> Result<Option<Memo<Vec<u32>>>, CachePoisoned> {
        self.memo(|c| match &c.sorted {
            Some((s, o)) if *s == sort => Some(Arc::clone(o)),
            _ => None,
        })
    }

    /// Phase two: keeps `order` as the memo for `sort` — **only** if `result` is
    /// still the cached result. A query that finished (or a sign-out that landed)
    /// while the order was being built has replaced or cleared the slot, and an
    /// index permutation over the old rows would page the new ones in a meaningless
    /// order. Checked by identity (`Arc::ptr_eq`), which is exact: the slot never
    /// re-wraps a result it already holds.
    pub fn store_sort_memo(&self, result: &Arc<QueryResult>, sort: RowSort, order: Arc<Vec<u32>>) {
        self.store_memo(result, |c| c.sorted = Some((sort, order)));
    }

    /// [`Self::sort_memo`] for the grouping: the handle plus the groups memoized for
    /// `group_by`, if any. Same reason and same two-phase shape.
    pub fn group_memo(
        &self,
        group_by: GroupBy,
    ) -> Result<Option<Memo<Vec<PatchGroup>>>, CachePoisoned> {
        self.memo(|c| match &c.groups {
            Some((g, v)) if *g == group_by => Some(Arc::clone(v)),
            _ => None,
        })
    }

    /// [`Self::store_sort_memo`] for the grouping, with the same identity check.
    pub fn store_group_memo(
        &self,
        result: &Arc<QueryResult>,
        group_by: GroupBy,
        groups: Arc<Vec<PatchGroup>>,
    ) {
        self.store_memo(result, |c| c.groups = Some((group_by, groups)));
    }

    fn memo<T>(
        &self,
        read: impl FnOnce(&CachedResult) -> Option<Arc<T>>,
    ) -> Result<Option<Memo<T>>, CachePoisoned> {
        let key = self.tenant_key();
        let guard = self.last_result.lock().map_err(|_| CachePoisoned)?;
        Ok(match guard.as_ref() {
            Some(c) if c.tenant == key => Some(Memo {
                result: Arc::clone(&c.result),
                memo: read(c),
            }),
            _ => None,
        })
    }

    fn store_memo(&self, result: &Arc<QueryResult>, write: impl FnOnce(&mut CachedResult)) {
        let key = self.tenant_key();
        if let Ok(mut guard) = self.last_result.lock()
            && let Some(cached) = guard.as_mut()
            && cached.tenant == key
            && Arc::ptr_eq(&cached.result, result)
        {
            write(cached);
        }
    }

    /// Both phases of [`Self::sort_memo`] composed synchronously, building the order
    /// on the calling thread with the lock released. The paging command does the
    /// same with the build on `spawn_blocking`; this is the form the memo tests use.
    #[cfg(test)]
    pub fn with_sorted_result<T>(
        &self,
        sort: Option<RowSort>,
        f: impl FnOnce(&[PatchRow], Option<&[u32]>) -> T,
    ) -> Result<Option<T>, CachePoisoned> {
        let Some(sort) = sort else {
            return Ok(self
                .current_result_handle()?
                .map(|result| f(&result.rows, None)));
        };
        let Some(Memo { result, memo }) = self.sort_memo(sort)? else {
            return Ok(None);
        };
        let order = memo.unwrap_or_else(|| {
            let order = Arc::new(sort_order(&result.rows, sort));
            self.store_sort_memo(&result, sort, Arc::clone(&order));
            order
        });
        Ok(Some(f(&result.rows, Some(&order))))
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
        // Each family owns its own epoch; `invalidate` bumps before clearing so a
        // fetch about to store sees the bump and drops its write.
        self.fleet_current_os.invalidate();
        self.fleet_current_sw.invalidate();
    }

    /// Drops **only** the device inventory. A reboot flips `os.needsReboot`, and
    /// [`DEVICE_TTL`] is 15 minutes — long enough to render the reboot invisible.
    pub fn invalidate_fleet_devices(&self) {
        self.fleet_devices_cache.invalidate();
    }

    /// The device and current-patch cache epochs, for tests that assert an
    /// invalidation did (or did not) happen without racing a fetch. Both families
    /// share one number because `invalidate_current_patches` always bumps them
    /// together; the OS slot's is the representative.
    #[cfg(test)]
    pub(crate) fn cache_epochs(&self) -> (u64, u64) {
        (
            self.fleet_devices_cache.epoch(),
            self.fleet_current_os.epoch(),
        )
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
            settings_write: tokio::sync::Mutex::const_new(()),
            last_result: Mutex::new(None),
            lookups_cache: TenantCache::default(),
            fleet_devices_cache: TenantCache::default(),
            fleet_current_os: TenantCache::default(),
            fleet_current_sw: TenantCache::default(),
            jobs: Mutex::new(None),
            job_seq: AtomicU64::new(1),
            query_generation: AtomicU64::new(0),
            result_epoch: AtomicU64::new(0),
            job_poller_running: Arc::new(AtomicBool::new(false)),
            pending_confirm: Mutex::new(None),
        }
    }
}

#[cfg(test)]
mod tests;
