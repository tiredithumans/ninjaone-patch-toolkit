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
    /// Dispatched action jobs, stamped with the tenant they belong to. Mutable and
    /// long-lived — it outlives the IPC call that created it — so it carries the
    /// same tenant check as `last_result`: a tenant switch reads as a miss, and a
    /// forgotten clear can't surface another tenant's dispatch history.
    jobs: Mutex<Option<(TenantKey, Vec<JobReport>)>>,
    /// Monotonic source of `JobReport.id` / `batch_id`.
    job_seq: AtomicU64,
    /// At most one poller at a time, so a burst of batches doesn't spawn N tasks
    /// all hammering `/activities`.
    job_poller_running: AtomicBool,
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
            fleet_current_cache: Mutex::new(None),
            jobs: Mutex::new(None),
            job_seq: AtomicU64::new(1),
            job_poller_running: AtomicBool::new(false),
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

    /// Drops **only** the whole-fleet current-patch cache.
    ///
    /// Called after a mutating action: the device's pending list is about to
    /// change, and [`CURRENT_PATCHES_TTL`] would otherwise keep serving pre-action
    /// data for up to two minutes. `clear_lookups_cache` is the wrong tool here —
    /// it also drops the 15-minute device inventory and the org/location/role
    /// lookups, neither of which a patch action can affect.
    pub fn invalidate_current_patches(&self) {
        if let Ok(mut guard) = self.fleet_current_cache.lock() {
            *guard = None;
        }
    }

    /// Drops **only** the device inventory. A reboot flips `os.needsReboot`, and
    /// [`DEVICE_TTL`] is 15 minutes — long enough to render the reboot invisible.
    pub fn invalidate_fleet_devices(&self) {
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
        let jobs = match guard.as_mut() {
            Some((t, jobs)) if *t == key => jobs,
            _ => {
                *guard = Some((key, Vec::new()));
                &mut guard.as_mut().expect("just inserted").1
            }
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

    /// Claims the single poller slot. `false` means one is already running and the
    /// caller should just let it pick up the new batch.
    pub fn try_claim_job_poller(&self) -> bool {
        self.job_poller_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn release_job_poller(&self) {
        self.job_poller_running.store(false, Ordering::Release);
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
        assert!(state.try_claim_job_poller());
        assert!(
            !state.try_claim_job_poller(),
            "a second batch must join the running poller, not spawn another"
        );
        state.release_job_poller();
        assert!(state.try_claim_job_poller());
    }
}
