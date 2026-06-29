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

struct LookupCache {
    at: Instant,
    // Held behind `Arc` so a cache hit (and every auto-refresh tick) hands out a
    // cheap refcount bump instead of deep-cloning three Vecs.
    orgs: Arc<Vec<Organization>>,
    locations: Arc<Vec<Location>>,
    roles: Arc<Vec<Role>>,
}

struct DeviceCache {
    at: Instant,
    devices: Arc<Vec<Device>>,
}

struct CurrentPatchesCache {
    at: Instant,
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
    /// Last query result, cached so the export command can write it without the
    /// frontend round-tripping all rows back over IPC.
    pub last_result: Mutex<Option<QueryResult>>,
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

    /// Orgs/locations/roles used to label patch rows, served from a short-TTL
    /// cache. Fetches the three concurrently on a miss. The lock is never held
    /// across the `.await`.
    pub async fn lookups(
        &self,
    ) -> Result<(Arc<Vec<Organization>>, Arc<Vec<Location>>, Arc<Vec<Role>>)> {
        if let Ok(guard) = self.lookups_cache.lock()
            && let Some(c) = guard.as_ref()
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
        if let Ok(guard) = self.fleet_devices_cache.lock()
            && let Some(c) = guard.as_ref()
            && c.at.elapsed() < DEVICE_TTL
        {
            return Ok(c.devices.clone());
        }
        let devices = Arc::new(self.api.devices(None, on_progress).await?);
        if let Ok(mut guard) = self.fleet_devices_cache.lock() {
            *guard = Some(DeviceCache {
                at: Instant::now(),
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
        if !force
            && let Ok(guard) = self.fleet_current_cache.lock()
            && let Some(c) = guard.as_ref()
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

    /// Drops the cached query result so a later export can't write a previous
    /// tenant's rows after sign-out or an instance change.
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

    #[test]
    fn last_result_cache_starts_empty_and_clears() {
        let state = AppState::new().expect("build state");
        // A fresh state has no cached result, so export_patches_xlsx errors with
        // "Run a query before exporting" rather than writing a stale workbook.
        assert!(state.last_result.lock().unwrap().is_none());

        *state.last_result.lock().unwrap() = Some(QueryResult {
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
        });
        assert!(state.last_result.lock().unwrap().is_some());

        // Sign-out / instance change drops the cache so a later export can't leak a
        // previous tenant's rows.
        state.clear_last_result();
        assert!(state.last_result.lock().unwrap().is_none());
    }
}
