//! All reactive state shared via context: the `AppState` wrapper, its nine
//! `Copy` sub-structs (grouped by concern), and the frontend-only value types
//! they carry (`Tab`, `AppliedFilters`, `Toast`, `Progress`, `DeviceSelection`).

use std::collections::{BTreeMap, BTreeSet};

use leptos::prelude::*;
use leptos::task::spawn_local;

use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Tab {
    Patches,
    Compliance,
    Reboot,
    Failures,
    Jobs,
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
    pub detected_window: String,
    pub detected_after: String,
    pub detected_before: String,
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
    pub(super) detected_window: RwSignal<String>,
    pub(super) detected_after_date: RwSignal<String>,
    pub(super) detected_before_date: RwSignal<String>,
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
            detected_window: RwSignal::new(String::new()),
            detected_after_date: RwSignal::new(String::new()),
            detected_before_date: RwSignal::new(String::new()),
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
        let window = self.detected_window.get_untracked();
        let (detected_within_days, detected_after, detected_before) = match window.as_str() {
            "1" | "7" | "30" | "90" => (window.parse::<i64>().ok(), None, None),
            "custom" => (
                None,
                date_to_epoch(&self.detected_after_date.get_untracked()),
                // Include the whole "before" day (end of day in UTC).
                date_to_epoch(&self.detected_before_date.get_untracked()).map(|e| e + 86_399),
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
            detected_within_days,
            detected_after,
            detected_before,
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
    /// Patches view mode: `None` is the flat row table, `Some(_)` groups by device
    /// or by patch. Grouping is computed backend-side over the cached rows, so
    /// switching modes is a fetch, not a client-side regroup of the visible page.
    pub(super) group_by: RwSignal<Option<GroupBy>>,
    /// Group headers for the current page of the grouped view, and the total so
    /// the pager knows how far it runs.
    pub(super) groups: RwSignal<Vec<PatchGroup>>,
    pub(super) groups_total: RwSignal<usize>,
    /// Keys of the groups the operator has opened.
    pub(super) expanded: RwSignal<BTreeSet<String>>,
    /// Member rows per opened group. A key present in `expanded` but absent here
    /// is still loading — which is what the view renders a spinner from.
    pub(super) members: RwSignal<BTreeMap<String, Vec<PatchRow>>>,
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
            group_by: RwSignal::new(None),
            groups: RwSignal::new(Vec::new()),
            groups_total: RwSignal::new(0),
            expanded: RwSignal::new(BTreeSet::new()),
            members: RwSignal::new(BTreeMap::new()),
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
    /// Whole write-path block, held as one value so a field the panel doesn't
    /// expose round-trips unchanged instead of resetting to its default on save.
    pub(super) f_actions: RwSignal<ActionSettings>,
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
            f_actions: RwSignal::new(ActionSettings::default()),
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

/// One device with the specific patch rows the operator ticked on it.
///
/// Tracked **per patch**, not merely per device, because the two dispatch paths
/// differ in what they can honor. **Apply** has no per-KB endpoint — it installs
/// everything approved on the device regardless of what's ticked — but a library
/// script declaring `kbAllowList` *can* be told which KBs to install. The earlier
/// device-keyed model swept every KB on the device into that list the moment one
/// row was checked, so the one path capable of per-patch targeting could never
/// actually be given a subset.
///
/// Third-party patches carry no KB (NinjaOne's software feed has no `kbNumber`),
/// so they map to `None` and cannot be targeted individually on either path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceSelection {
    pub name: String,
    pub organization: String,
    pub offline: bool,
    /// Ticked patch rows on this device: patch identity → its KB, if it has one.
    pub patches: BTreeMap<String, Option<String>>,
}

/// Selection and dispatch state for the actions surface. Everything stays empty in
/// web/demo mode — there is no backend to dispatch to.
#[derive(Clone, Copy)]
pub(crate) struct ActionState {
    /// device id → what was checked. Survives page changes; cleared by every
    /// successful query, because the underlying rows changed.
    pub(super) selected: RwSignal<BTreeMap<i64, DeviceSelection>>,
    pub(super) scripts: RwSignal<Vec<ScriptSummary>>,
    pub(super) scripts_loading: RwSignal<bool>,
    pub(super) script_id: RwSignal<Option<i64>>,
    pub(super) script_params: RwSignal<String>,
    pub(super) use_kb_targeting: RwSignal<bool>,
    /// `rebootBehavior` handed to a dispatched script. Distinct from
    /// `reboot_mode`, which addresses the reboot endpoint directly.
    pub(super) script_reboot: RwSignal<RebootChoice>,
    pub(super) run_as: RwSignal<String>,
    pub(super) reboot_mode: RwSignal<String>,
    pub(super) reason: RwSignal<String>,
    pub(super) include_offline: RwSignal<bool>,
    pub(super) override_window: RwSignal<bool>,
    /// Defaults **true**: the operator opts *out* of preview, never into it.
    pub(super) dry_run: RwSignal<bool>,
    /// The action awaiting confirmation, plus the plan describing it.
    pub(super) pending: RwSignal<Option<PendingAction>>,
    /// Type-to-confirm text for the forced-reboot tier.
    pub(super) confirm_input: RwSignal<String>,
    pub(super) dispatching: RwSignal<bool>,
    /// `(sent, total)` while a batch is going out, so a 25-device dispatch shows
    /// movement instead of a frozen "Dispatching…".
    pub(super) dispatch_progress: RwSignal<Option<(usize, usize)>>,
    pub(super) jobs: RwSignal<Vec<JobReport>>,
    /// A mutating action landed since the displayed result was computed.
    pub(super) results_stale: RwSignal<bool>,
}

/// A planned action held open in the confirmation modal.
#[derive(Clone, Debug)]
pub(crate) struct PendingAction {
    pub request: ActionRequest,
    pub plan: ActionPlan,
}

impl ActionState {
    pub(super) fn new() -> Self {
        Self {
            selected: RwSignal::new(BTreeMap::new()),
            scripts: RwSignal::new(Vec::new()),
            scripts_loading: RwSignal::new(false),
            script_id: RwSignal::new(None),
            script_params: RwSignal::new(String::new()),
            use_kb_targeting: RwSignal::new(true),
            script_reboot: RwSignal::new(RebootChoice::Never),
            run_as: RwSignal::new(String::new()),
            reboot_mode: RwSignal::new("NORMAL".to_string()),
            reason: RwSignal::new(String::new()),
            include_offline: RwSignal::new(false),
            override_window: RwSignal::new(false),
            dry_run: RwSignal::new(true),
            pending: RwSignal::new(None),
            confirm_input: RwSignal::new(String::new()),
            dispatching: RwSignal::new(false),
            dispatch_progress: RwSignal::new(None),
            jobs: RwSignal::new(Vec::new()),
            results_stale: RwSignal::new(false),
        }
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
    pub(super) actions: ActionState,
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
            actions: ActionState::new(),
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
            detected_window: self.filters.detected_window.get_untracked(),
            detected_after: self.filters.detected_after_date.get_untracked(),
            detected_before: self.filters.detected_before_date.get_untracked(),
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
                    // The underlying rows just changed, so a selection made against
                    // the previous result no longer describes what is on screen.
                    self.clear_selection();
                    self.actions.results_stale.set(false);
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

    /// Switches the Patches view between flat rows and a grouped view.
    ///
    /// Resets paging and every expand, because group keys and page offsets mean
    /// different things in each mode — carrying them over would open arbitrary
    /// groups. Demo mode groups its in-memory sample instead of round-tripping.
    pub(super) fn set_group_by(self, group_by: Option<GroupBy>) {
        if self.query.group_by.get_untracked() == group_by {
            return;
        }
        self.query.group_by.set(group_by);
        self.query.patches_page.set(0);
        self.query.expanded.update(|e| e.clear());
        self.query.members.update(|m| m.clear());
        match group_by {
            None => self.fetch_page(0),
            Some(_) => self.fetch_groups(0),
        }
    }

    /// Loads one page of group headers for the active grouping.
    pub(super) fn fetch_groups(self, page: usize) {
        let Some(group_by) = self.query.group_by.get_untracked() else {
            return;
        };
        if self.session.demo.get_untracked() {
            let all = demo::group_rows(&self.demo_rows(), group_by);
            self.query.groups_total.set(all.len());
            self.query.groups.set(
                all.into_iter()
                    .skip(page * PATCHES_PAGE_SIZE)
                    .take(PATCHES_PAGE_SIZE)
                    .collect(),
            );
            return;
        }
        spawn_local(async move {
            match api::get_patch_groups(group_by, page * PATCHES_PAGE_SIZE, PATCHES_PAGE_SIZE).await
            {
                Ok(page) => {
                    self.query.groups.set(page.groups);
                    self.query.groups_total.set(page.total);
                    self.query.query_error.set(None);
                }
                Err(e) => {
                    self.query.query_error.set(Some(e.clone()));
                    self.notify(Toast::err(e));
                }
            }
        });
    }

    /// Opens or closes a group, fetching its members the first time it opens.
    /// Members are cached per key, so re-opening is free and a collapse doesn't
    /// discard what was already loaded.
    pub(super) fn toggle_group(self, key: String) {
        let open = self.query.expanded.with_untracked(|e| e.contains(&key));
        if open {
            self.query.expanded.update(|e| {
                e.remove(&key);
            });
            return;
        }
        self.query.expanded.update(|e| {
            e.insert(key.clone());
        });
        if self.query.members.with_untracked(|m| m.contains_key(&key)) {
            return;
        }
        let Some(group_by) = self.query.group_by.get_untracked() else {
            return;
        };
        if self.session.demo.get_untracked() {
            let rows = demo::group_members(&self.demo_rows(), group_by, &key);
            self.query.members.update(|m| {
                m.insert(key, rows);
            });
            return;
        }
        spawn_local(async move {
            match api::get_patch_group_members(group_by, key.clone(), 0, GROUP_MEMBER_LIMIT).await {
                Ok(rows) => self.query.members.update(|m| {
                    m.insert(key, rows);
                }),
                Err(e) => {
                    // Leave the group open but empty and say why, rather than
                    // silently collapsing it back under the operator.
                    self.query.members.update(|m| {
                        m.insert(key, Vec::new());
                    });
                    self.notify(Toast::err(e));
                }
            }
        });
    }

    /// The sample rows behind demo-mode grouping — the displayed result's rows.
    fn demo_rows(self) -> Vec<PatchRow> {
        self.query
            .result
            .with_untracked(|r| r.as_ref().map(|r| r.rows.clone()).unwrap_or_default())
    }

    /// Ticks or clears every member of a group, loading them first if the group has
    /// never been expanded — otherwise the checkbox on a collapsed group would
    /// silently do nothing.
    ///
    /// Members are capped at `GROUP_MEMBER_LIMIT`, so one click can never select
    /// more rows than the expanded group would show.
    pub(super) fn toggle_group_selection(self, key: &str, checked: bool) {
        if let Some(rows) = self.query.members.with_untracked(|m| m.get(key).cloned()) {
            for row in &rows {
                self.toggle_row_selection(row, checked);
            }
            return;
        }
        let Some(group_by) = self.query.group_by.get_untracked() else {
            return;
        };
        let key = key.to_string();
        if self.session.demo.get_untracked() {
            let rows = demo::group_members(&self.demo_rows(), group_by, &key);
            for row in &rows {
                self.toggle_row_selection(row, checked);
            }
            self.query.members.update(|m| {
                m.insert(key, rows);
            });
            return;
        }
        spawn_local(async move {
            match api::get_patch_group_members(group_by, key.clone(), 0, GROUP_MEMBER_LIMIT).await {
                Ok(rows) => {
                    for row in &rows {
                        self.toggle_row_selection(row, checked);
                    }
                    self.query.members.update(|m| {
                        m.insert(key, rows);
                    });
                }
                Err(e) => self.notify(Toast::err(e)),
            }
        });
    }

    /// `(all, some)` ticked state for a group's loaded members, for its checkbox.
    pub(super) fn group_selection_state(self, key: &str) -> (bool, bool) {
        let rows = self
            .query
            .members
            .with(|m| m.get(key).cloned().unwrap_or_default());
        if rows.is_empty() {
            return (false, false);
        }
        let n = rows.iter().filter(|r| self.is_row_selected(r)).count();
        (n == rows.len(), n > 0 && n < rows.len())
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
        self.settings.f_actions.set(v.actions);
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
        match (f.detected_within_days, f.detected_after, f.detected_before) {
            (Some(d), _, _) => {
                self.filters.detected_window.set(d.to_string());
                self.filters.detected_after_date.set(String::new());
                self.filters.detected_before_date.set(String::new());
            }
            (None, after, before) if after.is_some() || before.is_some() => {
                self.filters.detected_window.set("custom".to_string());
                self.filters.detected_after_date.set(epoch_to_date(after));
                self.filters.detected_before_date.set(epoch_to_date(before));
            }
            _ => {
                self.filters.detected_window.set(String::new());
                self.filters.detected_after_date.set(String::new());
                self.filters.detected_before_date.set(String::new());
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

    // --- Device actions ------------------------------------------------------

    /// Whether this exact patch row is ticked.
    pub(super) fn is_row_selected(self, row: &PatchRow) -> bool {
        let key = patch_key(row);
        self.actions.selected.with(|sel| {
            sel.get(&row.device_id)
                .is_some_and(|d| d.patches.contains_key(&key))
        })
    }

    /// Ticks or unticks exactly the patch row clicked — nothing else.
    ///
    /// The device is implied by its ticked rows: it enters the selection with the
    /// first one and leaves when the last is cleared, so a device with nothing
    /// ticked is never dispatched against. What `Apply` then does on that device
    /// is still all-or-nothing (there is no per-KB apply endpoint); the per-row
    /// detail is what lets a `kbAllowList` script receive the actual subset.
    pub(super) fn toggle_row_selection(self, row: &PatchRow, checked: bool) {
        let key = patch_key(row);
        self.actions.selected.update(|sel| {
            if checked {
                sel.entry(row.device_id)
                    .or_insert_with(|| DeviceSelection {
                        name: row.device_name.clone(),
                        organization: row.organization.clone(),
                        offline: row.offline,
                        patches: BTreeMap::new(),
                    })
                    .patches
                    .insert(key, row.kb.clone().filter(|k| !k.is_empty()));
            } else if let Some(entry) = sel.get_mut(&row.device_id) {
                entry.patches.remove(&key);
                if entry.patches.is_empty() {
                    sel.remove(&row.device_id);
                }
            }
        });
    }

    /// Whether every row on the current page is selected. Used for the header
    /// checkbox's checked/indeterminate state.
    pub(super) fn page_selection_state(self) -> (bool, bool) {
        let rows = self.query.page_rows.get();
        if rows.is_empty() {
            return (false, false);
        }
        let sel = self.actions.selected.get();
        // Counts ticked *rows*, not devices: with per-row selection a device can
        // be partly ticked, and the header box must read indeterminate for that.
        let selected = rows
            .iter()
            .filter(|r| {
                sel.get(&r.device_id)
                    .is_some_and(|d| d.patches.contains_key(&patch_key(r)))
            })
            .count();
        (
            selected == rows.len(),
            selected > 0 && selected < rows.len(),
        )
    }

    /// Ticks or clears every patch row on the current page. Idempotent per row, so
    /// re-running it never double-counts.
    pub(super) fn toggle_page_selection(self, checked: bool) {
        let rows = self.query.page_rows.get_untracked();
        for row in &rows {
            self.toggle_row_selection(row, checked);
        }
    }

    pub(super) fn clear_selection(self) {
        self.actions.selected.update(|s| s.clear());
    }

    /// `(devices, patch rows, offline devices)` for the action bar's running total.
    /// Cross-page selection is invisible unless it is surfaced somewhere.
    pub(super) fn selection_counts(self) -> (usize, usize, usize) {
        self.actions.selected.with(|sel| {
            (
                sel.len(),
                sel.values().map(|d| d.patches.len()).sum(),
                sel.values().filter(|d| d.offline).count(),
            )
        })
    }

    /// KBs checked across the selection, for a script that accepts an allow list.
    fn selected_kbs(self) -> Vec<String> {
        self.actions.selected.with_untracked(|sel| {
            sel.values()
                // Only ticked rows contribute, and only those that have a KB —
                // third-party patches have none, so they can't be targeted here.
                .flat_map(|d| d.patches.values().flatten().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        })
    }

    pub(super) fn load_scripts(self) {
        if !self.can_act() {
            return;
        }
        self.actions.scripts_loading.set(true);
        spawn_local(async move {
            match api::list_scripts().await {
                Ok(list) => self.actions.scripts.set(list),
                Err(e) => self.notify(Toast::err(format!("Couldn't load scripts: {e}"))),
            }
            self.actions.scripts_loading.set(false);
        });
    }

    /// Why actions are unavailable, if they are — tracked, so a view reading this
    /// re-renders when the operator signs in or flips the Settings toggle.
    pub(super) fn blocked_reason(self) -> Option<String> {
        self.session.auth.with(|auth| {
            action_blocked_reason(
                self.session.web_mode.get(),
                self.session.demo.get(),
                auth.as_ref(),
            )
        })
    }

    /// Same verdict without subscribing — for event handlers, which run outside a
    /// reactive scope and would otherwise register a spurious dependency.
    pub(super) fn blocked_reason_untracked(self) -> Option<String> {
        self.session.auth.with_untracked(|auth| {
            action_blocked_reason(
                self.session.web_mode.get_untracked(),
                self.session.demo.get_untracked(),
                auth.as_ref(),
            )
        })
    }

    /// Whether the action affordances should be live. The backend re-checks all of
    /// this — this only decides what the UI offers.
    pub(super) fn can_act(self) -> bool {
        self.blocked_reason_untracked().is_none()
    }

    /// Builds the request for `kind` from the current selection and form state.
    pub(super) fn build_request(self, kind: ActionKind) -> ActionRequest {
        let device_ids: Vec<i64> = self
            .actions
            .selected
            .with_untracked(|s| s.keys().copied().collect());
        let mut req = ActionRequest::new(kind, device_ids);
        req.include_offline = self.actions.include_offline.get_untracked();
        req.override_window = self.actions.override_window.get_untracked();
        // Only a script has a real preview; for everything else the backend rejects
        // a dry run outright rather than pretending.
        req.dry_run = kind == ActionKind::Script && self.actions.dry_run.get_untracked();

        if kind == ActionKind::Reboot {
            req.reboot_mode = Some(if self.actions.reboot_mode.get_untracked() == "FORCED" {
                RebootMode::Forced
            } else {
                RebootMode::Normal
            });
            req.reason = Some(self.actions.reason.get_untracked());
        }
        if kind == ActionKind::Script {
            req.reboot = self.actions.script_reboot.get_untracked();
            let id = self.actions.script_id.get_untracked();
            req.script_id = id;
            req.script_name = self
                .actions
                .scripts
                .with_untracked(|s| s.iter().find(|s| Some(s.id) == id).map(|s| s.name.clone()));
            let run_as = self.actions.run_as.get_untracked();
            req.run_as = (!run_as.trim().is_empty()).then_some(run_as);
            let params = self.actions.script_params.get_untracked();
            req.parameters = (!params.trim().is_empty()).then_some(params);
            if self.actions.use_kb_targeting.get_untracked() {
                req.targets = self.selected_kbs();
            }
        }
        req
    }

    /// Asks the backend what `kind` would do and opens the confirmation modal.
    pub(super) fn open_plan(self, kind: ActionKind) {
        if !self.can_act() {
            if let Some(reason) = self.blocked_reason_untracked() {
                self.notify(Toast::err(reason));
            }
            return;
        }
        let request = self.build_request(kind);
        if request.device_ids.is_empty() {
            self.notify(Toast::err("Select at least one device first"));
            return;
        }
        self.actions.confirm_input.set(String::new());
        self.actions.dispatching.set(true);
        spawn_local(async move {
            match api::plan_action(request.clone()).await {
                Ok(plan) => self
                    .actions
                    .pending
                    .set(Some(PendingAction { request, plan })),
                Err(e) => self.notify(Toast::err(e)),
            }
            self.actions.dispatching.set(false);
        });
    }

    pub(super) fn cancel_plan(self) {
        self.actions.pending.set(None);
        self.actions.confirm_input.set(String::new());
    }

    /// Dispatches the plan currently held in the modal.
    pub(super) fn confirm_plan(self) {
        let Some(pending) = self.actions.pending.get_untracked() else {
            return;
        };
        let mut request = pending.request;
        request.confirm_token = pending.plan.confirm_token.clone();
        let mutating = request.kind.is_mutating();

        self.actions.dispatching.set(true);
        spawn_local(async move {
            match api::run_action(request).await {
                Ok(batch) => {
                    self.actions.pending.set(None);
                    self.actions.confirm_input.set(String::new());
                    // Seed from the response rather than re-fetching; the backend
                    // poller advances these rows over `action:progress`.
                    self.actions
                        .jobs
                        .update(|jobs| jobs.extend(batch.jobs.clone()));
                    self.ui.active_tab.set(Tab::Jobs);
                    if mutating && batch.dispatched > 0 {
                        // The on-screen result predates the change we just made.
                        self.actions.results_stale.set(true);
                    }
                    let msg = if batch.skipped > 0 {
                        format!(
                            "Dispatched to {} device(s); {} skipped",
                            batch.dispatched, batch.skipped
                        )
                    } else {
                        format!("Dispatched to {} device(s)", batch.dispatched)
                    };
                    self.notify(Toast::ok(msg));
                }
                Err(e) => self.notify(Toast::err(e)),
            }
            self.actions.dispatching.set(false);
            self.actions.dispatch_progress.set(None);
        });
    }

    pub(super) fn refresh_jobs(self) {
        if self.session.web_mode.get_untracked() || self.session.demo.get_untracked() {
            return;
        }
        spawn_local(async move {
            if let Ok(jobs) = api::list_jobs().await {
                self.actions.jobs.set(jobs);
            }
        });
    }

    pub(super) fn clear_job_history(self) {
        spawn_local(async move {
            match api::clear_jobs().await {
                Ok(jobs) => self.actions.jobs.set(jobs),
                Err(e) => self.notify(Toast::err(e)),
            }
        });
    }
}
