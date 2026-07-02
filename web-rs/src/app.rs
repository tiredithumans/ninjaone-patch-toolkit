use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::demo;
use crate::types::*;

mod charts;
mod filters;
mod settings;
mod tables;
mod util;

use charts::{ComplianceByOsBars, ComplianceCharts};
use filters::Filters;
use settings::SettingsPanel;
use tables::Results;
use util::{
    MdBlock, MdSpan, SummaryCounts, aged_badge, aria_sort, date_to_epoch, epoch_to_date,
    filter_chips, group_thousands, is_fleet_tab, next_sort, non_empty, parse_changelog, parse_opt,
    sev_class, sort_glyph, sort_patch_rows, status_class, summary_line, tab_class,
};

const PATCHES_PAGE_SIZE: usize = 100;

const REGIONS: [(&str, &str); 5] = [
    ("https://app.ninjarmm.com", "North America (app)"),
    ("https://us2.ninjarmm.com", "North America (us2)"),
    ("https://eu.ninjarmm.com", "Europe (eu)"),
    ("https://oc.ninjarmm.com", "Oceania (oc)"),
    ("https://ca.ninjarmm.com", "Canada (ca)"),
];

const STATUS_OPTIONS: [&str; 5] = ["PENDING", "APPROVED", "REJECTED", "INSTALLED", "FAILED"];

/// Releases page linked from the web demo's "Get the app" call to action.
const RELEASES_URL: &str = "https://github.com/tiredithumans/ninjaone-patch-toolkit/releases";

/// Severity facet options as (raw value sent to the backend, display label).
const SEVERITY_OPTIONS: [(&str, &str); 5] = [
    ("CRITICAL", "Critical"),
    ("IMPORTANT", "Important"),
    ("MODERATE", "Moderate"),
    ("LOW", "Low"),
    ("OPTIONAL", "Optional"),
];

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
    fn ok(m: impl Into<String>) -> Self {
        Self {
            msg: m.into(),
            error: false,
        }
    }
    fn err(m: impl Into<String>) -> Self {
        Self {
            msg: m.into(),
            error: true,
        }
    }
}

/// Live record counts streamed from the backend while a query runs.
#[derive(Clone, Copy, Default)]
struct Progress {
    devices: usize,
    os_patches: usize,
    sw_patches: usize,
    os_installs: usize,
    sw_installs: usize,
    joining: bool,
}

impl Progress {
    fn records(self) -> usize {
        self.devices + self.os_patches + self.sw_patches + self.os_installs + self.sw_installs
    }
}

/// Auth + frontend-context state: who we're signed in as and which environment
/// (desktop, browser demo) the frontend is running in.
#[derive(Clone, Copy)]
pub(crate) struct SessionState {
    auth: RwSignal<Option<AuthStatus>>,
    signing_in: RwSignal<bool>,
    /// Sample data is loaded (drives the "sample data" banner). Set by `enter_demo`.
    demo: RwSignal<bool>,
    /// Running in a plain browser with no Tauri backend — the GitHub Pages demo.
    /// Disables the backend-only actions (sign-in, live query, export).
    web_mode: RwSignal<bool>,
}

impl SessionState {
    fn new() -> Self {
        Self {
            auth: RwSignal::new(None),
            signing_in: RwSignal::new(false),
            demo: RwSignal::new(false),
            web_mode: RwSignal::new(false),
        }
    }

    fn is_authed(self) -> bool {
        self.auth.get().map(|a| a.authenticated).unwrap_or(false)
    }

    fn refresh_auth(self) {
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
    orgs: RwSignal<Vec<Organization>>,
    locations: RwSignal<Vec<Location>>,
    roles: RwSignal<Vec<Role>>,
    node_classes: RwSignal<Vec<NodeClass>>,
    /// Count of in-flight org/role/class lookup requests; > 0 means "loading".
    lookups_pending: RwSignal<u32>,
}

impl LookupState {
    fn new() -> Self {
        Self {
            orgs: RwSignal::new(Vec::new()),
            locations: RwSignal::new(Vec::new()),
            roles: RwSignal::new(Vec::new()),
            node_classes: RwSignal::new(Vec::new()),
            lookups_pending: RwSignal::new(0),
        }
    }

    fn loading_lookups(self) -> bool {
        self.lookups_pending.get() > 0
    }

    fn lookup_done(self) {
        self.lookups_pending.update(|n| *n = n.saturating_sub(1));
    }
}

/// The live filter controls (device scope + patch facets) as the user edits them.
#[derive(Clone, Copy)]
pub(crate) struct FilterState {
    org_id: RwSignal<Option<i64>>,
    loc_id: RwSignal<Option<i64>>,
    role_id: RwSignal<Option<i64>>,
    selected_classes: RwSignal<Vec<String>>,
    selected_severities: RwSignal<Vec<String>>,
    os_name: RwSignal<String>,
    search: RwSignal<String>,
    /// Release-date filter: "" (any), "1"/"7"/"30"/"90" (last N days), or "custom".
    release_window: RwSignal<String>,
    release_after_date: RwSignal<String>,
    release_before_date: RwSignal<String>,
    patch_type: RwSignal<String>,
    statuses: RwSignal<Vec<String>>,
    install_days: RwSignal<i64>,
}

impl FilterState {
    fn new() -> Self {
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

    fn toggle_in(self, sig: RwSignal<Vec<String>>, value: String) {
        sig.update(|v| {
            if let Some(pos) = v.iter().position(|x| x == &value) {
                v.remove(pos);
            } else {
                v.push(value);
            }
        });
    }

    fn current_filter(self) -> FilterParams {
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
    result: RwSignal<Option<QueryResult>>,
    /// Filters that produced `result`, snapshotted on the last successful run. Drives
    /// the read-only applied-filter chip row (kept in sync with the displayed result,
    /// not the live controls).
    applied_filters: RwSignal<Option<AppliedFilters>>,
    /// Zero-based page index for the paginated Patches table.
    patches_page: RwSignal<usize>,
    /// The detail rows for the currently displayed page, fetched from the backend
    /// cache via `get_patch_rows` (the full row set is never shipped over IPC).
    page_rows: RwSignal<Vec<PatchRow>>,
    /// The last failed query/paging error, kept as a persistent banner in the
    /// results area after the announcing toast auto-dismisses. Cleared by the next
    /// successful run/page fetch or an explicit dismiss.
    query_error: RwSignal<Option<String>>,
    /// Active sort for the Patches detail table; pages re-fetch with it. `None` is
    /// the backend's canonical order. Reset by each manual run.
    patches_sort: RwSignal<Option<RowSort>>,
}

impl QueryState {
    fn new() -> Self {
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
    busy: RwSignal<bool>,
    refreshing: RwSignal<bool>,
    /// Wall-clock timing for the running-query progress bar / elapsed display.
    /// `elapsed_tick` is bumped by a timer to re-evaluate the elapsed label.
    query_started_ms: RwSignal<f64>,
    elapsed_tick: RwSignal<u32>,
    last_duration_ms: RwSignal<Option<f64>>,
    /// Live record counts from backend `query:progress` events, plus a sequence
    /// number stamped on each run so stale events from a superseded run are dropped.
    progress: RwSignal<Progress>,
    query_seq: RwSignal<u64>,
    refresh_secs: RwSignal<u32>,
}

impl RunState {
    fn new() -> Self {
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
    fn elapsed_secs(self) -> f64 {
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
    fn progress_estimate(self) -> Option<f64> {
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
    f_instance: RwSignal<String>,
    f_client_id: RwSignal<String>,
    f_client_secret: RwSignal<String>,
    f_port: RwSignal<u16>,
    f_install_days: RwSignal<i64>,
    f_sla: RwSignal<i64>,
    has_secret: RwSignal<bool>,
    f_auto_update: RwSignal<bool>,
    presets: RwSignal<Vec<Preset>>,
    preset_name: RwSignal<String>,
}

impl SettingsState {
    fn new() -> Self {
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
    update: RwSignal<Option<UpdateInfo>>,
    update_busy: RwSignal<bool>,
}

impl UpdateState {
    fn new() -> Self {
        Self {
            update: RwSignal::new(None),
            update_busy: RwSignal::new(false),
        }
    }
}

/// App-chrome state: the toast, panel visibility, and the active results tab.
#[derive(Clone, Copy)]
pub(crate) struct UiState {
    toast: RwSignal<Option<Toast>>,
    toast_gen: RwSignal<u64>,
    show_settings: RwSignal<bool>,
    /// Collapses the Filters panel body to give the results more room. Expanded
    /// (false) by default.
    filters_collapsed: RwSignal<bool>,
    active_tab: RwSignal<Tab>,
}

impl UiState {
    fn new() -> Self {
        Self {
            toast: RwSignal::new(None),
            toast_gen: RwSignal::new(0),
            show_settings: RwSignal::new(false),
            filters_collapsed: RwSignal::new(false),
            active_tab: RwSignal::new(Tab::Patches),
        }
    }

    fn notify(self, t: Toast) {
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
    session: SessionState,
    lookups: LookupState,
    filters: FilterState,
    query: QueryState,
    run: RunState,
    settings: SettingsState,
    updates: UpdateState,
    ui: UiState,
}

impl AppState {
    fn new() -> Self {
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
    fn is_authed(self) -> bool {
        self.session.is_authed()
    }

    fn notify(self, t: Toast) {
        self.ui.notify(t)
    }

    fn current_filter(self) -> FilterParams {
        self.filters.current_filter()
    }

    fn load_lookups(self) {
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
    fn load_node_classes(self) {
        spawn_local(async move {
            match api::list_node_classes().await {
                Ok(n) => self.lookups.node_classes.set(n),
                Err(e) => self.notify(Toast::err(format!("Couldn't load OS types: {e}"))),
            }
        });
    }

    fn select_org(self, org: Option<i64>) {
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
    fn snapshot_filters(self) -> AppliedFilters {
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
    fn run_query(self) {
        self.run_query_inner(false, false);
    }

    /// Auto-refresh variant: flags a subtle `refreshing` state instead of the main
    /// `busy` one (so the Run-query button doesn't flicker each tick) and stays
    /// quiet about precondition failures. Forces a refetch of the live patch data —
    /// the point of the cadence is fresh patch state during a patching operation.
    fn run_query_auto(self) {
        self.run_query_inner(true, true);
    }

    /// Manual ↻ **Refresh**: user-initiated refetch of the live patch data for the
    /// current filter (shows the main busy/progress, unlike the silent auto tick).
    fn refresh_now(self) {
        self.run_query_inner(false, true);
    }

    fn run_query_inner(self, silent: bool, force: bool) {
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
    fn fetch_page(self, page: usize) {
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
    fn cycle_sort(self, key: RowSortKey) {
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
    fn enter_demo(self) {
        self.lookups.orgs.set(demo::sample_orgs());
        self.lookups.roles.set(demo::sample_roles());
        self.lookups.node_classes.set(demo::sample_node_classes());
        self.session.demo.set(true);
    }

    /// Demo-mode counterpart to `run_query`: filters the in-memory sample with the
    /// current facets (no backend, no auth) and recomputes the row count.
    fn run_demo_query(self, silent: bool) {
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

    fn apply_settings_view(self, v: SettingsView) {
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

    fn apply_preset(self, p: Preset) {
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

#[component]
pub fn App() -> impl IntoView {
    let state = AppState::new();
    provide_context(state);

    if api::is_tauri() {
        // Initial load. The OS-type facet is static, so load it immediately rather
        // than gating it behind sign-in with the org/role/location lookups.
        state.load_node_classes();
        spawn_local(async move {
            if let Ok(a) = api::auth_status().await {
                let authed = a.authenticated;
                state.session.auth.set(Some(a));
                if authed {
                    state.load_lookups();
                }
            }
        });
        spawn_local(async move {
            if let Ok(s) = api::get_settings().await {
                let auto = s.auto_check_updates;
                state.apply_settings_view(s);
                if auto && let Ok(Some(info)) = api::check_for_update().await {
                    state.updates.update.set(Some(info));
                }
            }
        });
    } else {
        // Browser/Pages demo: there is no backend, so every IPC call would fail.
        // Enter demo mode (facets seeded from the sample) but leave the results
        // empty until the user presses Run query, just like the real app.
        state.session.web_mode.set(true);
        state.enter_demo();
    }

    // Stream live record counts from the backend into `progress`, ignoring events
    // from a run the user has already superseded.
    api::on_query_progress(move |ev| {
        if ev.query_id != state.run.query_seq.get_untracked() {
            return;
        }
        state.run.progress.update(|p| match ev.stage.as_str() {
            "devices" => p.devices = ev.loaded,
            "osPatches" => p.os_patches = ev.loaded,
            "swPatches" => p.sw_patches = ev.loaded,
            "osInstalls" => p.os_installs = ev.loaded,
            "swInstalls" => p.sw_installs = ev.loaded,
            "joining" => p.joining = true,
            _ => {}
        });
    });

    // Tick the elapsed-time display roughly twice a second while a query runs.
    gloo_timers::callback::Interval::new(500, move || {
        if state.run.busy.get_untracked() || state.run.refreshing.get_untracked() {
            state.run.elapsed_tick.update(|t| *t = t.wrapping_add(1));
        }
    })
    .forget();

    // Auto-refresh: rebuild the interval whenever the cadence or auth changes.
    let interval = StoredValue::new_local(None::<gloo_timers::callback::Interval>);
    Effect::new(move |_| {
        let secs = state.run.refresh_secs.get();
        let authed = state.is_authed();
        interval.set_value(None);
        if secs > 0 && authed {
            let iv =
                gloo_timers::callback::Interval::new(secs * 1000, move || state.run_query_auto());
            interval.set_value(Some(iv));
        }
    });

    view! {
        <main>
            <Header/>
            <Show when=move || state.session.demo.get()>
                <p class="demo-banner" role="note">
                    "Demo mode — press Run query to list sample patches (not a live fleet)."
                </p>
            </Show>
            <Show when=move || state.ui.show_settings.get()>
                <SettingsPanel/>
            </Show>
            <Filters/>
            <RunControls/>
            <Results/>
            <Toaster/>
            <UpdateSplash/>
        </main>
    }
}

/// Renders one changelog line's inline runs — plain text and `**bold**` — into views.
fn render_spans(spans: Vec<MdSpan>) -> impl IntoView {
    spans
        .into_iter()
        .map(|span| match span {
            MdSpan::Text(t) => view! { {t} }.into_any(),
            MdSpan::Strong(t) => view! { <strong>{t}</strong> }.into_any(),
        })
        .collect_view()
}

/// Modal shown when an update is available. Renders the new version + the
/// release notes (changelog) and offers to install + relaunch.
#[component]
fn UpdateSplash() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Escape dismisses the update splash (same as "Later"), unless an install is
    // already running.
    window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape"
            && state.updates.update.get_untracked().is_some()
            && !state.updates.update_busy.get_untracked()
        {
            state.updates.update.set(None);
        }
    });
    view! {
        {move || {
            let Some(info) = state.updates.update.get() else {
                return ().into_any();
            };
            let notes = info.notes.unwrap_or_default();
            let changelog = if notes.trim().is_empty() {
                None
            } else {
                // The notes are the CHANGELOG.md section (markdown); render the
                // headings / bullet lists / bold runs instead of dumping the source.
                let body = parse_changelog(&notes)
                    .into_iter()
                    .map(|block| match block {
                        MdBlock::Heading(h) => view! { <h4>{h}</h4> }.into_any(),
                        MdBlock::List(items) => view! {
                            <ul>
                                {items
                                    .into_iter()
                                    .map(|spans| view! { <li>{render_spans(spans)}</li> })
                                    .collect_view()}
                            </ul>
                        }
                            .into_any(),
                        MdBlock::Paragraph(spans) => {
                            view! { <p>{render_spans(spans)}</p> }.into_any()
                        }
                    })
                    .collect_view();
                Some(
                    view! {
                        <div class="changelog">
                            <h3>"What's new"</h3>
                            <div class="changelog-body">{body}</div>
                        </div>
                    },
                )
            };
            let install = move |_| {
                state.updates.update_busy.set(true);
                spawn_local(async move {
                    // On success the backend installs and relaunches the app, so
                    // this never returns Ok; an Err means the install failed.
                    if let Err(e) = api::install_update().await {
                        state.updates.update_busy.set(false);
                        state.notify(Toast::err(format!("Update failed: {e}")));
                    }
                });
            };
            let dismiss = move |_| state.updates.update.set(None);
            view! {
                <div class="modal-overlay">
                    <div
                        class="modal"
                        role="dialog"
                        aria-modal="true"
                        aria-labelledby="update-title"
                    >
                        <h2 id="update-title">
                            {format!("Update available — v{}", info.version)}
                        </h2>
                        <p class="modal-sub">
                            {format!(
                                "You're on v{}. Install the new version now?",
                                info.current_version,
                            )}
                        </p>
                        {changelog}
                        <div class="row modal-actions">
                            <button
                                class="btn btn-primary"
                                prop:disabled=move || state.updates.update_busy.get()
                                on:click=install
                            >
                                {move || {
                                    if state.updates.update_busy.get() {
                                        "Updating…"
                                    } else {
                                        "Update & restart"
                                    }
                                }}
                            </button>
                            <button
                                class="btn btn-ghost"
                                prop:disabled=move || state.updates.update_busy.get()
                                on:click=dismiss
                            >
                                "Later"
                            </button>
                        </div>
                    </div>
                </div>
            }
                .into_any()
        }}
    }
}

#[component]
fn Header() -> impl IntoView {
    let state = expect_context::<AppState>();
    let authed = move || state.is_authed();
    let instance = move || {
        state
            .session
            .auth
            .get()
            .map(|a| a.instance_base_url)
            .unwrap_or_default()
    };

    view! {
        <header class="topbar">
            <div class="brand">
                <span class="logo">"◆"</span>
                <div>
                    <h1>"NinjaOne Patch Toolkit"</h1>
                    <p class="subtitle">{instance}</p>
                </div>
            </div>
            <div class="actions">
                <Show when=move || state.session.web_mode.get()>
                    <span class="pill pill-demo">"Demo"</span>
                    <a
                        class="btn btn-primary"
                        href=RELEASES_URL
                        target="_blank"
                        rel="noreferrer"
                    >
                        "Get the app ↗"
                    </a>
                </Show>
                <Show when=move || !state.session.web_mode.get()>
                <span class=move || if authed() { "pill pill-on" } else { "pill pill-off" }>
                    {move || if authed() { "Connected" } else { "Not signed in" }}
                </span>
                <button class="btn" on:click=move |_| state.ui.show_settings.update(|s| *s = !*s)>
                    "Settings"
                </button>
                <Show
                    when=move || authed()
                    fallback=move || {
                        view! {
                            <button
                                class="btn btn-primary"
                                prop:disabled=move || state.session.signing_in.get()
                                on:click=move |_| {
                                    if state.session.signing_in.get_untracked() {
                                        return;
                                    }
                                    state.session.signing_in.set(true);
                                    state
                                        .notify(Toast::ok("Complete the sign-in in your browser…"));
                                    spawn_local(async move {
                                        match api::sign_in().await {
                                            Ok(()) => {
                                                state.session.refresh_auth();
                                                state.load_lookups();
                                                state.notify(Toast::ok("Signed in"));
                                            }
                                            Err(e) => state.notify(Toast::err(e)),
                                        }
                                        state.session.signing_in.set(false);
                                    });
                                }
                            >
                                {move || {
                                    if state.session.signing_in.get() { "Signing in…" } else { "Sign in" }
                                }}
                            </button>
                        }
                    }
                >
                    <button
                        class="btn"
                        on:click=move |_| {
                            spawn_local(async move {
                                match api::sign_out().await {
                                    Ok(()) => {
                                        state.session.refresh_auth();
                                        state.notify(Toast::ok("Signed out"));
                                    }
                                    Err(e) => state.notify(Toast::err(e)),
                                }
                            });
                        }
                    >
                        "Sign out"
                    </button>
                </Show>
                </Show>
            </div>
        </header>
    }
}

#[component]
fn RunControls() -> impl IntoView {
    let state = expect_context::<AppState>();

    view! {
        <section class="panel">
            <div class="controls">
                <button
                    class="btn btn-primary"
                    prop:disabled=move || state.run.busy.get()
                    on:click=move |_| state.run_query()
                >
                    {move || if state.run.busy.get() { "Running…" } else { "Run query" }}
                </button>
                <button
                    class="btn"
                    prop:disabled=move || {
                        state.query.result.get().is_none() || state.session.web_mode.get() || state.session.demo.get()
                    }
                    title=move || {
                        if state.session.web_mode.get() || state.session.demo.get() {
                            "Excel export needs a live query in the desktop app"
                        } else {
                            ""
                        }
                    }
                    on:click=move |_| {
                        spawn_local(async move {
                            match api::export_patches().await {
                                Ok(Some(p)) => state.notify(Toast::ok(format!("Exported to {p}"))),
                                Ok(None) => {}
                                Err(e) => state.notify(Toast::err(e)),
                            }
                        });
                    }
                >
                    "Export to Excel"
                </button>
                <button
                    class="btn"
                    prop:disabled=move || {
                        state.query.result.get().is_none() || state.session.web_mode.get() || state.session.demo.get()
                    }
                    title=move || {
                        if state.session.web_mode.get() || state.session.demo.get() {
                            "The HTML report needs a live query in the desktop app"
                        } else {
                            ""
                        }
                    }
                    on:click=move |_| {
                        spawn_local(async move {
                            match api::export_report().await {
                                Ok(Some(p)) => {
                                    state.notify(Toast::ok(format!("Report saved to {p}")))
                                }
                                Ok(None) => {}
                                Err(e) => state.notify(Toast::err(e)),
                            }
                        });
                    }
                >
                    "Export report"
                </button>
                <Show when=move || state.run.refreshing.get()>
                    <span class="chips-label">"↻ refreshing…"</span>
                </Show>
                <label class="inline">
                    "Auto-refresh"
                    <select on:change=move |ev| {
                        state.run.refresh_secs.set(event_target_value(&ev).parse().unwrap_or(0))
                    }>
                        {[("0", "Off"), ("30", "30s"), ("60", "1m"), ("300", "5m"), ("900", "15m")]
                            .into_iter()
                            .map(|(val, label)| {
                                let sel = move || state.run.refresh_secs.get().to_string() == val;
                                view! {
                                    <option value=val selected=sel>
                                        {label}
                                    </option>
                                }
                            })
                            .collect_view()}
                    </select>
                </label>
                <button
                    class="btn"
                    prop:disabled=move || {
                        state.run.busy.get() || state.run.refreshing.get() || state.session.web_mode.get()
                            || state.session.demo.get() || state.query.result.get().is_none()
                    }
                    title="Refetch live patch data from NinjaOne for the current filter"
                    on:click=move |_| state.refresh_now()
                >
                    "↻ Refresh"
                </button>
                <Show when=move || state.query.result.get().is_some()>
                    <span class="chips-label">
                        {move || {
                            state.query.result
                                .get()
                                .map(|r| format!("patch data as of {}", r.data_fetched_at))
                                .unwrap_or_default()
                        }}
                    </span>
                </Show>
                <PresetRow/>
            </div>
            <Show when=move || state.run.busy.get()>
                <div class="query-progress">
                    <div class="progress">
                        {move || match state.run.progress_estimate() {
                            Some(p) => {
                                view! {
                                    <div
                                        class="progress-bar"
                                        style=format!("width:{:.1}%", p * 100.0)
                                    ></div>
                                }
                                    .into_any()
                            }
                            None => {
                                view! { <div class="progress-bar progress-indeterminate"></div> }
                                    .into_any()
                            }
                        }}
                    </div>
                    <span class="progress-label">
                        {move || {
                            let p = state.run.progress.get();
                            let secs = state.run.elapsed_secs();
                            if p.joining {
                                format!("Running… {secs:.0}s · computing rollups…")
                            } else {
                                let n = p.records();
                                if n > 0 {
                                    format!(
                                        "Running… {secs:.0}s · loaded {} records",
                                        group_thousands(n),
                                    )
                                } else {
                                    format!("Running… {secs:.0}s")
                                }
                            }
                        }}
                    </span>
                </div>
            </Show>
            <Show when=move || {
                !state.run.busy.get() && state.run.last_duration_ms.get().is_some()
            }>
                <p class="query-hint">
                    {move || {
                        format!(
                            "Last run took {:.0}s",
                            state.run.last_duration_ms.get().unwrap_or(0.0) / 1000.0,
                        )
                    }}
                </p>
            </Show>
        </section>
    }
}

#[component]
fn Toaster() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        // Always-present live region: a screen reader announces the toast as it
        // appears. An aria-live region created at the same moment as its content
        // is not reliably announced, so the wrapper stays mounted.
        <div class="toaster" role="status" aria-live="assertive" aria-atomic="true">
            {move || {
                state.ui.toast
                    .get()
                    .map(|t| {
                        let cls = if t.error { "toast toast-err" } else { "toast toast-ok" };
                        view! {
                            <div class=cls>
                                <span>{t.msg}</span>
                                <button
                                    class="x"
                                    aria-label="Dismiss notification"
                                    on:click=move |_| state.ui.toast.set(None)
                                >
                                    "×"
                                </button>
                            </div>
                        }
                    })
            }}
        </div>
    }
}
#[component]
fn PresetRow() -> impl IntoView {
    let state = expect_context::<AppState>();

    let save_preset = move |_| {
        let name = state.settings.preset_name.get_untracked();
        if name.trim().is_empty() {
            state.notify(Toast::err("Name the preset first"));
            return;
        }
        let preset = Preset {
            name: name.trim().to_string(),
            filter: state.current_filter(),
            patch_type: Some(state.filters.patch_type.get_untracked()),
            statuses: Some(state.filters.statuses.get_untracked()),
            install_days: Some(state.filters.install_days.get_untracked()),
        };
        spawn_local(async move {
            match api::save_preset(preset).await {
                Ok(p) => {
                    state.settings.presets.set(p);
                    state.settings.preset_name.set(String::new());
                    state.notify(Toast::ok("Preset saved"));
                }
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    };

    view! {
        <div class="row presets">
            <span class="chips-label">"Presets:"</span>
            {move || {
                state.settings.presets
                    .get()
                    .into_iter()
                    .map(|p| {
                        let name = p.name.clone();
                        let label_name = p.name.clone();
                        let p2 = p.clone();
                        let del_name = p.name.clone();
                        // Two-click confirm: first click arms, second deletes;
                        // mouseleave/blur disarm. Component-local so the signal is
                        // disposed with this chip when the preset list re-renders.
                        let armed = RwSignal::new(false);
                        view! {
                            <span class="chip chip-preset">
                                <button
                                    class="link"
                                    on:click=move |_| state.apply_preset(p2.clone())
                                >
                                    {name}
                                </button>
                                <button
                                    class=move || if armed.get() { "x x-armed" } else { "x" }
                                    aria-label=move || {
                                        if armed.get() {
                                            format!("Confirm delete preset {label_name}")
                                        } else {
                                            format!("Delete preset {label_name}")
                                        }
                                    }
                                    on:click=move |_| {
                                        if !armed.get_untracked() {
                                            armed.set(true);
                                            return;
                                        }
                                        let n = del_name.clone();
                                        spawn_local(async move {
                                            if let Ok(p) = api::delete_preset(n).await {
                                                state.settings.presets.set(p);
                                            }
                                        });
                                    }
                                    on:mouseleave=move |_| armed.set(false)
                                    on:blur=move |_| armed.set(false)
                                >
                                    {move || if armed.get() { "Delete?" } else { "×" }}
                                </button>
                            </span>
                        }
                    })
                    .collect_view()
            }}
            <input
                class="preset-name"
                placeholder="Preset name"
                prop:value=move || state.settings.preset_name.get()
                on:input=move |ev| state.settings.preset_name.set(event_target_value(&ev))
            />
            <button class="btn btn-ghost" on:click=save_preset>
                "Save preset"
            </button>
        </div>
    }
}
