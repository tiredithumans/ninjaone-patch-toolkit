//! All reactive state shared via context: the `AppState` wrapper, its eight
//! `Copy` sub-structs (grouped by concern), and the frontend-only value types
//! they carry (`Tab`, `AppliedFilters`, `Toast`, `Progress`).

use leptos::prelude::*;
use leptos::task::spawn_local;

use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Tab {
    Patches,
    Compliance,
    Reboot,
    Failures,
}

/// A snapshot of the filters that produced the currently displayed result, captured
/// at Run time (ids resolved to display names, raw values to labels) so the chip row
/// always describes the on-screen data — even after the user edits a control but has
/// not re-run. Frontend-only; never crosses IPC, so it is not mirrored in `types.rs`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AppliedFilters {
    pub organization: Option<String>,
    pub location: Option<String>,
    pub role: Option<String>,
    pub os_types: Vec<String>,
    pub os_name: Option<String>,
    pub patch_type: String,
    pub statuses: Vec<String>,
    pub severities: Vec<String>,
    pub search: Option<String>,
    pub release_window: String,
    pub release_after: String,
    pub release_before: String,
    pub install_days: Option<i64>,
}

#[derive(Clone)]
pub struct Toast {
    pub msg: String,
    pub error: bool,
}

impl Toast {
    pub(super) fn ok(m: impl Into<String>) -> Self {
        Self {
            msg: m.into(),
            error: false,
        }
    }
    pub(super) fn err(m: impl Into<String>) -> Self {
        Self {
            msg: m.into(),
            error: true,
        }
    }
}

/// Live record counts streamed from the backend while a query runs.
#[derive(Clone, Copy, Default)]
pub(super) struct Progress {
    pub(super) devices: usize,
    pub(super) os_patches: usize,
    pub(super) sw_patches: usize,
    pub(super) os_installs: usize,
    pub(super) sw_installs: usize,
    pub(super) joining: bool,
}

impl Progress {
    pub(super) fn records(self) -> usize {
        self.devices + self.os_patches + self.sw_patches + self.os_installs + self.sw_installs
    }
}

/// Auth + frontend-context state: who we're signed in as and which environment
/// (desktop, browser demo) the frontend is running in.
#[derive(Clone, Copy)]
pub(crate) struct SessionState {
    pub(super) auth: RwSignal<Option<AuthStatus>>,
    pub(super) signing_in: RwSignal<bool>,
    /// Sample data is loaded (drives the "sample data" banner). Set by `enter_demo`.
    pub(super) demo: RwSignal<bool>,
    /// Running in a plain browser with no Tauri backend — the GitHub Pages demo.
    /// Disables the backend-only actions (sign-in, live query, export).
    pub(super) web_mode: RwSignal<bool>,
}

impl SessionState {
    pub(super) fn new() -> Self {
        Self {
            auth: RwSignal::new(None),
            signing_in: RwSignal::new(false),
            demo: RwSignal::new(false),
            web_mode: RwSignal::new(false),
        }
    }

    pub(super) fn is_authed(self) -> bool {
        self.auth.get().map(|a| a.authenticated).unwrap_or(false)
    }

    pub(super) fn refresh_auth(self) {
        spawn_local(async move {
            if let Ok(a) = api::auth_status().await {
                self.auth.set(Some(a));
            }
        });
    }
}

/// The org/location/role/OS-type reference lists that fill the scope dropdowns.
#[derive(Clone, Copy)]
pub(crate) struct LookupState {
    pub(super) orgs: RwSignal<Vec<Organization>>,
    pub(super) locations: RwSignal<Vec<Location>>,
    pub(super) roles: RwSignal<Vec<Role>>,
    pub(super) node_classes: RwSignal<Vec<NodeClass>>,
    /// Count of in-flight org/role/class lookup requests; > 0 means "loading".
    pub(super) lookups_pending: RwSignal<u32>,
}

impl LookupState {
    pub(super) fn new() -> Self {
        Self {
            orgs: RwSignal::new(Vec::new()),
            locations: RwSignal::new(Vec::new()),
            roles: RwSignal::new(Vec::new()),
            node_classes: RwSignal::new(Vec::new()),
            lookups_pending: RwSignal::new(0),
        }
    }

    pub(super) fn loading_lookups(self) -> bool {
        self.lookups_pending.get() > 0
    }

    pub(super) fn lookup_done(self) {
        self.lookups_pending.update(|n| *n = n.saturating_sub(1));
    }
}

/// The live filter controls (device scope + patch facets) as the user edits them.
#[derive(Clone, Copy)]
pub(crate) struct FilterState {
    pub(super) org_id: RwSignal<Option<i64>>,
    pub(super) loc_id: RwSignal<Option<i64>>,
    pub(super) role_id: RwSignal<Option<i64>>,
    pub(super) selected_classes: RwSignal<Vec<String>>,
    pub(super) selected_severities: RwSignal<Vec<String>>,
    pub(super) os_name: RwSignal<String>,
    pub(super) search: RwSignal<String>,
    /// Release-date filter: "" (any), "1"/"7"/"30"/"90" (last N days), or "custom".
    pub(super) release_window: RwSignal<String>,
    pub(super) release_after_date: RwSignal<String>,
    pub(super) release_before_date: RwSignal<String>,
    pub(super) patch_type: RwSignal<String>,
    pub(super) statuses: RwSignal<Vec<String>>,
    pub(super) install_days: RwSignal<i64>,
}

impl FilterState {
    pub(super) fn new() -> Self {
        Self {
            org_id: RwSignal::new(None),
            loc_id: RwSignal::new(None),
            role_id: RwSignal::new(None),
            selected_classes: RwSignal::new(Vec::new()),
            selected_severities: RwSignal::new(Vec::new()),
            os_name: RwSignal::new(String::new()),
            search: RwSignal::new(String::new()),
            release_window: RwSignal::new(String::new()),
            release_after_date: RwSignal::new(String::new()),
            release_before_date: RwSignal::new(String::new()),
            patch_type: RwSignal::new("ALL".to_string()),
            statuses: RwSignal::new(vec!["PENDING".to_string()]),
            install_days: RwSignal::new(30),
        }
    }

    pub(super) fn toggle_in(self, sig: RwSignal<Vec<String>>, value: String) {
        sig.update(|v| {
            if let Some(pos) = v.iter().position(|x| x == &value) {
                v.remove(pos);
            } else {
                v.push(value);
            }
        });
    }

    pub(super) fn current_filter(self) -> FilterParams {
        let window = self.release_window.get_untracked();
        let (release_within_days, release_after, release_before) = match window.as_str() {
            "1" | "7" | "30" | "90" => (window.parse::<i64>().ok(), None, None),
            "custom" => (
                None,
                date_to_epoch(&self.release_after_date.get_untracked()),
                // Include the whole "before" day (end of day in UTC).
                date_to_epoch(&self.release_before_date.get_untracked()).map(|e| e + 86_399),
            ),
            _ => (None, None, None),
        };
        FilterParams {
            organization_id: self.org_id.get_untracked(),
            location_id: self.loc_id.get_untracked(),
            role_id: self.role_id.get_untracked(),
            node_classes: self.selected_classes.get_untracked(),
            os_name_contains: non_empty(self.os_name.get_untracked()),
            search: non_empty(self.search.get_untracked()),
            severities: self.selected_severities.get_untracked(),
            release_within_days,
            release_after,
            release_before,
        }
    }
}

/// The displayed query result and the Patches-table view over it (paging, sort,
/// the persistent error record).
#[derive(Clone, Copy)]
pub(crate) struct QueryState {
    pub(super) result: RwSignal<Option<QueryResult>>,
    /// Filters that produced `result`, snapshotted on the last successful run. Drives
    /// the read-only applied-filter chip row (kept in sync with the displayed result,
    /// not the live controls).
    pub(super) applied_filters: RwSignal<Option<AppliedFilters>>,
    /// Zero-based page index for the paginated Patches table.
    pub(super) patches_page: RwSignal<usize>,
    /// The detail rows for the currently displayed page, fetched from the backend
    /// cache via `get_patch_rows` (the full row set is never shipped over IPC).
    pub(super) page_rows: RwSignal<Vec<PatchRow>>,
    /// The last failed query/paging error, kept as a persistent banner in the
    /// results area after the announcing toast auto-dismisses. Cleared by the next
    /// successful run/page fetch or an explicit dismiss.
    pub(super) query_error: RwSignal<Option<String>>,
    /// Active sort for the Patches detail table; pages re-fetch with it. `None` is
    /// the backend's canonical order. Reset by each manual run.
    pub(super) patches_sort: RwSignal<Option<RowSort>>,
}

impl QueryState {
    pub(super) fn new() -> Self {
        Self {
            result: RwSignal::new(None),
            applied_filters: RwSignal::new(None),
            patches_page: RwSignal::new(0),
            page_rows: RwSignal::new(Vec::new()),
            query_error: RwSignal::new(None),
            patches_sort: RwSignal::new(None),
        }
    }
}

/// The in-flight-query machinery: busy flags, progress events, timing, and the
/// auto-refresh cadence.
#[derive(Clone, Copy)]
pub(crate) struct RunState {
    pub(super) busy: RwSignal<bool>,
    pub(super) refreshing: RwSignal<bool>,
    /// Wall-clock timing for the running-query progress bar / elapsed display.
    /// `elapsed_tick` is bumped by a timer to re-evaluate the elapsed label.
    pub(super) query_started_ms: RwSignal<f64>,
    pub(super) elapsed_tick: RwSignal<u32>,
    pub(super) last_duration_ms: RwSignal<Option<f64>>,
    /// Live record counts from backend `query:progress` events, plus a sequence
    /// number stamped on each run so stale events from a superseded run are dropped.
    pub(super) progress: RwSignal<Progress>,
    pub(super) query_seq: RwSignal<u64>,
    pub(super) refresh_secs: RwSignal<u32>,
}

impl RunState {
    pub(super) fn new() -> Self {
        Self {
            busy: RwSignal::new(false),
            refreshing: RwSignal::new(false),
            query_started_ms: RwSignal::new(0.0),
            elapsed_tick: RwSignal::new(0),
            last_duration_ms: RwSignal::new(None),
            progress: RwSignal::new(Progress::default()),
            query_seq: RwSignal::new(0),
            refresh_secs: RwSignal::new(0),
        }
    }

    /// Seconds since the running query started (re-evaluated on each timer tick).
    pub(super) fn elapsed_secs(self) -> f64 {
        let _ = self.elapsed_tick.get();
        let started = self.query_started_ms.get_untracked();
        if started <= 0.0 {
            0.0
        } else {
            ((js_sys::Date::now() - started) / 1000.0).max(0.0)
        }
    }

    /// Estimated completion fraction (0.0–0.95) from the previous run's duration,
    /// or `None` when there's no prior timing yet (→ indeterminate bar). Capped
    /// below 1.0 so an over-running query doesn't claim to be finished.
    pub(super) fn progress_estimate(self) -> Option<f64> {
        let _ = self.elapsed_tick.get();
        let last = self.last_duration_ms.get()?;
        if last <= 0.0 {
            return None;
        }
        let elapsed = js_sys::Date::now() - self.query_started_ms.get_untracked();
        Some((elapsed / last).clamp(0.0, 0.95))
    }
}

/// The Settings form fields (`f_*`), plus the persisted presets.
#[derive(Clone, Copy)]
pub(crate) struct SettingsState {
    pub(super) f_instance: RwSignal<String>,
    pub(super) f_client_id: RwSignal<String>,
    pub(super) f_client_secret: RwSignal<String>,
    pub(super) f_port: RwSignal<u16>,
    pub(super) f_install_days: RwSignal<i64>,
    pub(super) f_sla: RwSignal<i64>,
    pub(super) has_secret: RwSignal<bool>,
    pub(super) f_auto_update: RwSignal<bool>,
    pub(super) presets: RwSignal<Vec<Preset>>,
    pub(super) preset_name: RwSignal<String>,
}

impl SettingsState {
    pub(super) fn new() -> Self {
        Self {
            f_instance: RwSignal::new("https://us2.ninjarmm.com".to_string()),
            f_client_id: RwSignal::new(String::new()),
            f_client_secret: RwSignal::new(String::new()),
            f_port: RwSignal::new(11434),
            f_install_days: RwSignal::new(30),
            f_sla: RwSignal::new(30),
            has_secret: RwSignal::new(false),
            f_auto_update: RwSignal::new(true),
            presets: RwSignal::new(Vec::new()),
            preset_name: RwSignal::new(String::new()),
        }
    }
}

/// Auto-update state: the available-update info (drives `UpdateSplash`) and the
/// install-in-flight flag.
#[derive(Clone, Copy)]
pub(crate) struct UpdateState {
    pub(super) update: RwSignal<Option<UpdateInfo>>,
    pub(super) update_busy: RwSignal<bool>,
}

impl UpdateState {
    pub(super) fn new() -> Self {
        Self {
            update: RwSignal::new(None),
            update_busy: RwSignal::new(false),
        }
    }
}

/// App-chrome state: the toast, panel visibility, and the active results tab.
#[derive(Clone, Copy)]
pub(crate) struct UiState {
    pub(super) toast: RwSignal<Option<Toast>>,
    pub(super) toast_gen: RwSignal<u64>,
    pub(super) show_settings: RwSignal<bool>,
    /// Collapses the Filters panel body to give the results more room. Expanded
    /// (false) by default.
    pub(super) filters_collapsed: RwSignal<bool>,
    pub(super) active_tab: RwSignal<Tab>,
}

impl UiState {
    pub(super) fn new() -> Self {
        Self {
            toast: RwSignal::new(None),
            toast_gen: RwSignal::new(0),
            show_settings: RwSignal::new(false),
            filters_collapsed: RwSignal::new(false),
            active_tab: RwSignal::new(Tab::Patches),
        }
    }

    pub(super) fn notify(self, t: Toast) {
        // Auto-dismiss after a few seconds (errors linger a little longer); a
        // newer toast supersedes this one via the generation guard.
        let ms = if t.error { 7000 } else { 4000 };
        let generation = self.toast_gen.get_untracked().wrapping_add(1);
        self.toast_gen.set(generation);
        self.toast.set(Some(t));
        gloo_timers::callback::Timeout::new(ms, move || {
            if self.toast_gen.get_untracked() == generation {
                self.toast.set(None);
            }
        })
        .forget();
    }
}

/// All reactive state, shared via context as one `Copy` value (`RwSignal` handles
/// are `Copy`, so the wrapper and every group above are too). Fields are grouped
/// by concern; methods that orchestrate across groups stay on this wrapper.
#[derive(Clone, Copy)]
pub struct AppState {
    pub(super) session: SessionState,
    pub(super) lookups: LookupState,
    pub(super) filters: FilterState,
    pub(super) query: QueryState,
    pub(super) run: RunState,
    pub(super) settings: SettingsState,
    pub(super) updates: UpdateState,
    pub(super) ui: UiState,
}

impl AppState {
    pub(super) fn new() -> Self {
        Self {
            session: SessionState::new(),
            lookups: LookupState::new(),
            filters: FilterState::new(),
            query: QueryState::new(),
            run: RunState::new(),
            settings: SettingsState::new(),
            updates: UpdateState::new(),
            ui: UiState::new(),
        }
    }

    // Thin delegators for the hottest cross-module calls, so their many existing
    // call sites read the same after the sub-struct split.
    pub(super) fn is_authed(self) -> bool {
        self.session.is_authed()
    }

    pub(super) fn notify(self, t: Toast) {
        self.ui.notify(t)
    }

    pub(super) fn current_filter(self) -> FilterParams {
        self.filters.current_filter()
    }

    pub(super) fn load_lookups(self) {
        self.lookups.lookups_pending.set(2);
        spawn_local(async move {
            match api::list_orgs().await {
                Ok(o) => self.lookups.orgs.set(o),
                Err(e) => self.notify(Toast::err(format!("Couldn't load organizations: {e}"))),
            }
            self.lookups.lookup_done();
        });
        spawn_local(async move {
            match api::list_roles().await {
                Ok(r) => self.lookups.roles.set(r),
                Err(e) => self.notify(Toast::err(format!("Couldn't load roles: {e}"))),
            }
            self.lookups.lookup_done();
        });
    }

    /// Loads the static OS-type list. It needs no auth or API call, so it runs at
    /// startup rather than waiting for sign-in like the org/role/location lookups.
    pub(super) fn load_node_classes(self) {
        spawn_local(async move {
            match api::list_node_classes().await {
                Ok(n) => self.lookups.node_classes.set(n),
                Err(e) => self.notify(Toast::err(format!("Couldn't load OS types: {e}"))),
            }
        });
    }

    pub(super) fn select_org(self, org: Option<i64>) {
        self.filters.org_id.set(org);
        self.filters.loc_id.set(None);
        self.lookups.locations.set(Vec::new());
        if let Some(id) = org {
            // Demo mode resolves locations from the sample, not the backend.
            if self.session.demo.get_untracked() {
                self.lookups.locations.set(demo::sample_locations(id));
                return;
            }
            spawn_local(async move {
                match api::list_locations(id).await {
                    Ok(locs) => self.lookups.locations.set(locs),
                    Err(e) => self.notify(Toast::err(format!("Couldn't load locations: {e}"))),
                }
            });
        }
    }

    /// Snapshots the active filters for the applied-filter chips, resolving org/loc/role
    /// ids to display names and severity raw values to labels. All reads are untracked
    /// (this runs imperatively at Run time, not inside a reactive scope).
    pub(super) fn snapshot_filters(self) -> AppliedFilters {
        let statuses = self.filters.statuses.get_untracked();
        let install_days = statuses
            .iter()
            .any(|s| s == "INSTALLED")
            .then(|| self.filters.install_days.get_untracked());

        let organization = self.filters.org_id.get_untracked().and_then(|id| {
            self.lookups
                .orgs
                .get_untracked()
                .into_iter()
                .find(|o| o.id == id)
                .map(|o| o.name)
        });
        let location = self.filters.loc_id.get_untracked().and_then(|id| {
            self.lookups
                .locations
                .get_untracked()
                .into_iter()
                .find(|l| l.id == id)
                .map(|l| l.name)
        });
        let role = self.filters.role_id.get_untracked().and_then(|id| {
            self.lookups
                .roles
                .get_untracked()
                .into_iter()
                .find(|r| r.id == id)
                .map(|r| r.name)
        });
        let selected = self.filters.selected_classes.get_untracked();
        let os_types = self
            .lookups
            .node_classes
            .get_untracked()
            .into_iter()
            .filter(|nc| selected.contains(&nc.value))
            .map(|nc| nc.label)
            .collect();
        let sev_raw = self.filters.selected_severities.get_untracked();
        let severities = SEVERITY_OPTIONS
            .iter()
            .filter(|(v, _)| sev_raw.iter().any(|s| s == v))
            .map(|(_, label)| label.to_string())
            .collect();

        AppliedFilters {
            organization,
            location,
            role,
            os_types,
            os_name: non_empty(self.filters.os_name.get_untracked()),
            patch_type: self.filters.patch_type.get_untracked(),
            statuses,
            severities,
            search: non_empty(self.filters.search.get_untracked()),
            release_window: self.filters.release_window.get_untracked(),
            release_after: self.filters.release_after_date.get_untracked(),
            release_before: self.filters.release_before_date.get_untracked(),
            install_days,
        }
    }

    /// Manual **Run query** / filter change: re-scopes the cached whole-fleet data
    /// client-side (no refetch unless the cache is cold or past its staleness bound).
    pub(super) fn run_query(self) {
        self.run_query_inner(false, false);
    }

    /// Auto-refresh variant: flags a subtle `refreshing` state instead of the main
    /// `busy` one (so the Run-query button doesn't flicker each tick) and stays
    /// quiet about precondition failures. Forces a refetch of the live patch data —
    /// the point of the cadence is fresh patch state during a patching operation.
    pub(super) fn run_query_auto(self) {
        self.run_query_inner(true, true);
    }

    /// Manual ↻ **Refresh**: user-initiated refetch of the live patch data for the
    /// current filter (shows the main busy/progress, unlike the silent auto tick).
    pub(super) fn refresh_now(self) {
        self.run_query_inner(false, true);
    }

    pub(super) fn run_query_inner(self, silent: bool, force: bool) {
        if self.run.busy.get_untracked() || self.run.refreshing.get_untracked() {
            return;
        }
        // In demo mode there is no backend to query — filter the sample locally.
        if self.session.demo.get_untracked() {
            self.run_demo_query(silent);
            return;
        }
        if !self.is_authed() {
            if !silent {
                self.notify(Toast::err("Sign in first"));
            }
            return;
        }
        let statuses = self.filters.statuses.get_untracked();
        if statuses.is_empty() {
            if !silent {
                self.notify(Toast::err("Select at least one status"));
            }
            return;
        }
        let args = PatchQueryArgs {
            filter: self.current_filter(),
            patch_type: self.filters.patch_type.get_untracked(),
            statuses,
            install_after_days: Some(self.filters.install_days.get_untracked()),
        };
        // Snapshot the filters driving this run; applied only if the query succeeds, so
        // a failed run leaves the chips matching the still-displayed prior result.
        let snapshot = self.snapshot_filters();
        // Stamp this run so progress events from a superseded run are ignored, and
        // clear the previous run's counts.
        let seq = self.run.query_seq.get_untracked().wrapping_add(1);
        self.run.query_seq.set(seq);
        self.run.progress.set(Progress::default());
        let flag = if silent {
            self.run.refreshing
        } else {
            self.run.busy
        };
        let started = js_sys::Date::now();
        self.run.query_started_ms.set(started);
        flag.set(true);
        spawn_local(async move {
            match api::query_patches(args, seq, force).await {
                Ok(r) => {
                    // Jump back to page 1 on a manual run; an auto-refresh keeps the
                    // current page, clamped in case the new result is shorter.
                    let page_count = r.rows_total.div_ceil(PATCHES_PAGE_SIZE).max(1);
                    let page = if silent {
                        self.query.patches_page.get_untracked().min(page_count - 1)
                    } else {
                        // A manual run returns to page 1 in the canonical order.
                        self.query.patches_sort.set(None);
                        0
                    };
                    self.query.patches_page.set(page);
                    // Page 0 ships inline with the summary (canonical order), so seed
                    // it directly; a later page — or a silent refresh with an active
                    // sort — is fetched instead.
                    if page == 0 && self.query.patches_sort.get_untracked().is_none() {
                        self.query.page_rows.set(r.rows.clone());
                    } else {
                        self.fetch_page(page);
                    }
                    self.query.result.set(Some(r));
                    self.query.applied_filters.set(Some(snapshot));
                    self.query.query_error.set(None);
                }
                // The toast announces the failure (aria-live); the banner keeps it
                // visible after the toast auto-dismisses.
                Err(e) => {
                    self.query.query_error.set(Some(e.clone()));
                    self.notify(Toast::err(e));
                }
            }
            // Record the round-trip so the next run can show "Last run took Ns"
            // and drive the estimated progress bar.
            self.run
                .last_duration_ms
                .set(Some(js_sys::Date::now() - started));
            flag.set(false);
        });
    }

    /// Loads the detail rows for `page` from the backend's cached result into
    /// `page_rows`. Paging fetches just the visible window rather than holding the
    /// whole row set in the frontend.
    pub(super) fn fetch_page(self, page: usize) {
        let sort = self.query.patches_sort.get_untracked();
        spawn_local(async move {
            match api::get_patch_rows(page * PATCHES_PAGE_SIZE, PATCHES_PAGE_SIZE, sort).await {
                Ok(rows) => {
                    self.query.page_rows.set(rows);
                    self.query.query_error.set(None);
                }
                Err(e) => {
                    self.query.query_error.set(Some(e.clone()));
                    self.notify(Toast::err(e));
                }
            }
        });
    }

    /// Cycles a Patches-table column through none → ascending → descending and
    /// re-fetches page 1 in the new order. Demo mode sorts its in-memory rows
    /// instead — the sample ships whole, so there is no backend to re-page from.
    pub(super) fn cycle_sort(self, key: RowSortKey) {
        let next = next_sort(self.query.patches_sort.get_untracked(), key);
        self.query.patches_sort.set(next);
        self.query.patches_page.set(0);
        if self.session.demo.get_untracked() {
            match next {
                Some(s) => self.query.page_rows.update(|rows| sort_patch_rows(rows, s)),
                // Unsorted = the sample's canonical order, kept on `result`.
                None => self.query.page_rows.set(
                    self.query
                        .result
                        .with_untracked(|r| r.as_ref().map(|r| r.rows.clone()).unwrap_or_default()),
                ),
            }
            return;
        }
        self.fetch_page(0);
    }

    /// Enters demo mode (browser/Pages) without populating results: seeds the facet
    /// dropdowns from the sample and flags `demo` so **Run query** filters the sample
    /// locally. The results stay empty ("Run a query to list patches") until the user
    /// runs a query — exactly like the real app, which lists nothing until queried.
    pub(super) fn enter_demo(self) {
        self.lookups.orgs.set(demo::sample_orgs());
        self.lookups.roles.set(demo::sample_roles());
        self.lookups.node_classes.set(demo::sample_node_classes());
        self.session.demo.set(true);
    }

    /// Demo-mode counterpart to `run_query`: filters the in-memory sample with the
    /// current facets (no backend, no auth) and recomputes the row count.
    pub(super) fn run_demo_query(self, silent: bool) {
        let statuses = self.filters.statuses.get_untracked();
        if statuses.is_empty() {
            if !silent {
                self.notify(Toast::err("Select at least one status"));
            }
            return;
        }
        let r = demo::filtered_result(
            &self.current_filter(),
            &self.filters.patch_type.get_untracked(),
            &statuses,
            Some(self.filters.install_days.get_untracked()),
        );
        self.query.patches_page.set(0);
        self.query.page_rows.set(r.rows.clone());
        self.query.result.set(Some(r));
        self.query
            .applied_filters
            .set(Some(self.snapshot_filters()));
        self.query.query_error.set(None);
    }

    pub(super) fn apply_settings_view(self, v: SettingsView) {
        self.settings.f_instance.set(v.instance_base_url);
        self.settings
            .f_client_id
            .set(v.client_id.unwrap_or_default());
        self.settings.f_port.set(v.callback_port);
        self.settings.f_install_days.set(v.install_window_days);
        self.settings.f_sla.set(v.sla_days);
        self.settings.has_secret.set(v.has_client_secret);
        self.settings.f_auto_update.set(v.auto_check_updates);
        self.filters.install_days.set(v.install_window_days);
        self.settings.presets.set(v.presets);
    }

    pub(super) fn apply_preset(self, p: Preset) {
        let f = p.filter;
        // Restore the patch-query selectors only when the preset captured them, so a
        // legacy preset leaves the current Type/Status/install-window untouched.
        if let Some(pt) = p.patch_type {
            self.filters.patch_type.set(pt);
        }
        if let Some(st) = p.statuses {
            self.filters.statuses.set(st);
        }
        if let Some(d) = p.install_days {
            self.filters.install_days.set(d);
        }
        self.filters.role_id.set(f.role_id);
        self.filters.selected_classes.set(f.node_classes);
        self.filters.selected_severities.set(f.severities);
        self.filters
            .os_name
            .set(f.os_name_contains.unwrap_or_default());
        self.filters.search.set(f.search.unwrap_or_default());
        // Restore the release-date filter UI from the stored bounds.
        match (f.release_within_days, f.release_after, f.release_before) {
            (Some(d), _, _) => {
                self.filters.release_window.set(d.to_string());
                self.filters.release_after_date.set(String::new());
                self.filters.release_before_date.set(String::new());
            }
            (None, after, before) if after.is_some() || before.is_some() => {
                self.filters.release_window.set("custom".to_string());
                self.filters.release_after_date.set(epoch_to_date(after));
                self.filters.release_before_date.set(epoch_to_date(before));
            }
            _ => {
                self.filters.release_window.set(String::new());
                self.filters.release_after_date.set(String::new());
                self.filters.release_before_date.set(String::new());
            }
        }
        // Load the org's locations, then restore the saved location.
        self.filters.org_id.set(f.organization_id);
        self.filters.loc_id.set(None);
        self.lookups.locations.set(Vec::new());
        if let Some(org) = f.organization_id {
            let want_loc = f.location_id;
            spawn_local(async move {
                match api::list_locations(org).await {
                    Ok(locs) => {
                        self.lookups.locations.set(locs);
                        self.filters.loc_id.set(want_loc);
                    }
                    Err(e) => self.notify(Toast::err(format!("Couldn't load locations: {e}"))),
                }
            });
        }
    }
}
