use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::warn;

use crate::api::{NinjaApiClient, ProgressFn};
use crate::auth::AuthState;
use crate::model::{Device, Location, Organization, Patch, Role};
use crate::rows::QueryResult;
use crate::settings::Settings;

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

struct CurrentPatchesCache {
    at: Instant,
    tenant: TenantKey,
    fetched_at: DateTime<Utc>,
    os: Arc<Vec<Patch>>,
    sw: Arc<Vec<Patch>>,
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
    /// Whole-fleet current patches (OS + 3rd-party) cached so a re-filter recomputes
    /// without a refetch; refreshed on force or past [`CURRENT_PATCHES_TTL`].
    fleet_current_cache: Mutex<Option<CurrentPatchesCache>>,
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
        );
        let api = NinjaApiClient::new(http, auth.clone());

        Ok(Self {
            auth,
            api,
            settings: Mutex::new(settings),
            last_result: Mutex::new(None),
            lookups_cache: Mutex::new(None),
            fleet_devices_cache: Mutex::new(None),
            fleet_current_cache: Mutex::new(None),
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
        if let Ok(guard) = self.fleet_devices_cache.lock()
            && let Some(c) = guard.as_ref()
            && c.tenant == key
            && c.at.elapsed() < DEVICE_TTL
        {
            return Ok(c.devices.clone());
        }
        let devices = Arc::new(self.api.devices(None, on_progress).await?);
        if let Ok(mut guard) = self.fleet_devices_cache.lock() {
            *guard = Some(DeviceCache {
                at: Instant::now(),
                tenant: key,
                devices: devices.clone(),
            });
        }
        Ok(devices)
    }

    /// Whole-fleet current patches (OS + 3rd-party, no `df`), cached so a re-filter
    /// recomputes without a refetch. `force` (an auto-refresh tick or the manual
    /// refresh) bypasses the TTL to pull fresh patch state mid-patching; otherwise
    /// the cache serves until it passes [`CURRENT_PATCHES_TTL`]. Both families are
    /// fetched concurrently. The lock is never held across the `.await`.
    pub async fn fleet_current_patches(
        &self,
        force: bool,
        on_os: Option<&ProgressFn<'_>>,
        on_sw: Option<&ProgressFn<'_>>,
    ) -> Result<CurrentPatches> {
        let key = self.tenant_key();
        if !force
            && let Ok(guard) = self.fleet_current_cache.lock()
            && let Some(c) = guard.as_ref()
            && c.tenant == key
            && c.at.elapsed() < CURRENT_PATCHES_TTL
        {
            return Ok(CurrentPatches {
                os: c.os.clone(),
                sw: c.sw.clone(),
                fetched_at: c.fetched_at,
            });
        }
        let (os, sw) = tokio::try_join!(
            self.api.fleet_os_patches(None, None, on_os),
            self.api.fleet_software_patches(None, None, on_sw),
        )?;
        let fetched_at = Utc::now();
        let (os, sw) = (Arc::new(os), Arc::new(sw));
        if let Ok(mut guard) = self.fleet_current_cache.lock() {
            *guard = Some(CurrentPatchesCache {
                at: Instant::now(),
                tenant: key,
                fetched_at,
                os: os.clone(),
                sw: sw.clone(),
            });
        }
        Ok(CurrentPatches { os, sw, fetched_at })
    }

    /// Drops cached lookups so a different tenant (after sign-out or an instance
    /// change) doesn't see stale org/location/role names. Also drops the whole-fleet
    /// device/patch caches, which are likewise tenant-scoped.
    pub fn clear_lookups_cache(&self) {
        if let Ok(mut guard) = self.lookups_cache.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.fleet_devices_cache.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.fleet_current_cache.lock() {
            *guard = None;
        }
    }

    /// Stores a query result stamped with the current tenant so paging and export can
    /// read it. A poisoned cache is warned (not panicked) so the staleness is
    /// observable but the app survives.
    pub fn store_last_result(&self, result: QueryResult) {
        let key = self.tenant_key();
        match self.last_result.lock() {
            Ok(mut slot) => *slot = Some((key, result)),
            // A poisoned cache means export/paging would read the previous run — warn
            // rather than silently dropping the write so the staleness is observable.
            Err(_) => warn!("result cache poisoned; export and paging will use the prior query"),
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
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn last_result_cache_starts_empty_and_clears() {
        let state = AppState::new().expect("build state");
        // A fresh state has no cached result, so export errors with "Run a query
        // before exporting" rather than writing a stale workbook.
        assert!(state.with_current_result(|_| ()).unwrap().is_none());

        state.store_last_result(sample_result());
        assert!(state.with_current_result(|_| ()).unwrap().is_some());

        // Sign-out / instance change drops the cache so a later export can't leak a
        // previous tenant's rows.
        state.clear_last_result();
        assert!(state.with_current_result(|_| ()).unwrap().is_none());
    }

    #[test]
    fn last_result_invisible_after_instance_switch() {
        let state = AppState::new().expect("build state");
        state.store_last_result(sample_result());
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
        state.store_last_result(sample_result());
        assert!(state.with_current_result(|_| ()).unwrap().is_some());

        state.settings.lock().unwrap().client_id = Some("different-client".into());
        assert!(state.with_current_result(|_| ()).unwrap().is_none());
    }
}
