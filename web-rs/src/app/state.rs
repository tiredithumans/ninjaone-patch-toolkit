//! All reactive state shared via context: the `AppState` wrapper, its nine
//! `Copy` sub-structs (grouped by concern), and the frontend-only value types
//! they carry (`Tab`, `AppliedFilters`, `Toast`, `Progress`, `DeviceSelection`).

use std::collections::{BTreeMap, BTreeSet};

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
    /// Names of the selected organizations (empty = every organization). Plural for
    /// the same reason the facet is: one chip has to describe a set.
    pub organizations: Vec<String>,
    pub locations: Vec<String>,
    pub roles: Vec<String>,
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
    /// Selected organizations; empty = every organization. All three identity
    /// facets are multi-select, so a scope like "these four sites" is one query
    /// rather than four.
    pub(super) org_ids: RwSignal<Vec<i64>>,
    pub(super) loc_ids: RwSignal<Vec<i64>>,
    pub(super) role_ids: RwSignal<Vec<i64>>,
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
            org_ids: RwSignal::new(Vec::new()),
            loc_ids: RwSignal::new(Vec::new()),
            role_ids: RwSignal::new(Vec::new()),
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

    /// [`toggle_in`](Self::toggle_in) for an id facet. Kept sorted so the chip row,
    /// the `df` clause and preset equality all see one canonical order regardless of
    /// the order the operator ticked things.
    pub(super) fn toggle_id(self, sig: RwSignal<Vec<i64>>, id: i64) {
        sig.update(|v| {
            match v.iter().position(|x| *x == id) {
                Some(pos) => {
                    v.remove(pos);
                }
                None => v.push(id),
            }
            v.sort_unstable();
        });
    }

    /// Reads the panel's signals and hands them to the pure [`filter_params`]
    /// mapping. Everything below the signal reads is testable there; this method
    /// stays a lift so it has nothing left to get wrong.
    pub(super) fn current_filter(self) -> FilterParams {
        filter_params(FilterInputs {
            organization_ids: self.org_ids.get_untracked(),
            location_ids: self.loc_ids.get_untracked(),
            role_ids: self.role_ids.get_untracked(),
            node_classes: self.selected_classes.get_untracked(),
            severities: self.selected_severities.get_untracked(),
            os_name: self.os_name.get_untracked(),
            search: self.search.get_untracked(),
            detected_window: self.detected_window.get_untracked(),
            detected_after: self.detected_after_date.get_untracked(),
            detected_before: self.detected_before_date.get_untracked(),
        })
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
            // Starts from the operator's remembered choice. `None` (never chosen)
            // opens expanded so the controls are discoverable on a first launch;
            // `run_query` collapses it once the first result lands, because at that
            // moment the table is what they came for and the panel is ~487px of the
            // window standing in front of it.
            filters_collapsed: RwSignal::new(
                api::ui_pref(api::PREF_FILTERS_COLLAPSED).unwrap_or(false),
            ),
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
    /// Ticked patch rows on this device, keyed by `util::patch_key`.
    pub patches: BTreeMap<String, SelectedPatch>,
}

/// What a ticked row contributes to a remediation script's target list.
///
/// The two families are targeted differently — OS patches by KB number, third-party
/// software by product title — and a device can have rows of both ticked at once, so
/// the row has to remember which it is. Keying only by KB (the earlier shape) made
/// software rows indistinguishable from OS rows that happen to lack one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SelectedPatch {
    /// `None` for third-party patches — NinjaOne's software feed has no `kbNumber`.
    pub kb: Option<String>,
    /// The product/patch title, which is how a software patch is targeted.
    pub name: String,
    pub is_os: bool,
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
        self.lookups.lookups_pending.set(3);
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
        // Locations load up front too, rather than only once an organization is
        // picked. With the facets multi-select, "every organization" is a real and
        // common scope, and under it the location picker would otherwise sit
        // permanently empty and disabled — the operator could not narrow to a site
        // without first selecting its org.
        spawn_local(async move {
            match api::list_locations(Vec::new()).await {
                Ok(locs) => self.lookups.locations.set(locs),
                Err(e) => self.notify(Toast::err(format!("Couldn't load locations: {e}"))),
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

    /// Toggles one organization in the scope and reloads the locations available
    /// under the new selection.
    ///
    /// Locations that no longer belong to any selected organization are dropped from
    /// the selection rather than left behind: an invisible location id would go on
    /// narrowing every query with nothing on screen to explain the empty result.
    pub(super) fn toggle_org(self, org_id: i64) {
        self.filters.toggle_id(self.filters.org_ids, org_id);
        self.reload_locations();
    }

    /// Reloads the location list for the current organization selection, then prunes
    /// any selected location that is no longer offered.
    pub(super) fn reload_locations(self) {
        let orgs = self.filters.org_ids.get_untracked();
        // Demo mode resolves locations from the sample, not the backend.
        if self.session.demo.get_untracked() {
            self.lookups.locations.set(demo::sample_locations(&orgs));
            self.prune_selected_locations();
            return;
        }
        spawn_local(async move {
            match api::list_locations(orgs).await {
                Ok(locs) => {
                    self.lookups.locations.set(locs);
                    self.prune_selected_locations();
                }
                Err(e) => self.notify(Toast::err(format!("Couldn't load locations: {e}"))),
            }
        });
    }

    /// Drops selected location ids that the current list no longer offers.
    fn prune_selected_locations(self) {
        let available: Vec<i64> = self
            .lookups
            .locations
            .get_untracked()
            .iter()
            .map(|l| l.id)
            .collect();
        self.filters
            .loc_ids
            .update(|sel| sel.retain(|id| available.contains(id)));
    }

    /// Snapshots the active filters for the applied-filter chips, resolving org/loc/role
    /// ids to display names and severity raw values to labels. All reads are untracked
    /// (this runs imperatively at Run time, not inside a reactive scope).
    pub(super) fn snapshot_filters(self) -> AppliedFilters {
        let statuses = self.filters.statuses.get_untracked();
        // The lookback bounds *both* install-history statuses, not just INSTALLED:
        // `QueryPlan` sets `installed_after` whenever any `is_install_history()`
        // status is requested, and the query always sends `install_after_days`. Tying
        // the chip to INSTALLED alone meant a FAILED-only run — the failure dashboard —
        // was silently truncated to the window with nothing on screen saying so, so an
        // operator reading "12 failures" had no way to know it meant "12 in 30 days".
        let install_days = statuses
            .iter()
            .any(|s| s == "INSTALLED" || s == "FAILED")
            .then(|| self.filters.install_days.get_untracked());

        let organizations = util::names_for(
            &self.filters.org_ids.get_untracked(),
            self.lookups.orgs.get_untracked().into_iter(),
        );
        let locations = util::names_for(
            &self.filters.loc_ids.get_untracked(),
            self.lookups.locations.get_untracked().into_iter(),
        );
        let roles = util::names_for(
            &self.filters.role_ids.get_untracked(),
            self.lookups.roles.get_untracked().into_iter(),
        );
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
            organizations,
            locations,
            roles,
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
        let statuses = self.filters.statuses.get_untracked();
        // The guard chain lives in `util::run_decision` so its ordering is testable;
        // this method only carries out the decision.
        match util::run_decision(
            self.run.busy.get_untracked(),
            self.run.refreshing.get_untracked(),
            self.session.demo.get_untracked(),
            self.is_authed(),
            statuses.is_empty(),
        ) {
            util::RunDecision::AlreadyRunning => return,
            util::RunDecision::Demo => {
                self.run_demo_query(silent);
                return;
            }
            util::RunDecision::NotSignedIn => {
                if !silent {
                    self.notify(Toast::err("Sign in first"));
                }
                return;
            }
            util::RunDecision::NoStatusSelected => {
                if !silent {
                    self.notify(Toast::err("Select at least one status"));
                }
                return;
            }
            util::RunDecision::Run => {}
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
        let seq = util::next_query_seq(self.run.query_seq.get_untracked());
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
            let outcome = api::query_patches(args, seq, force).await;
            // Apply only if this is still the newest run. Queries overlap routinely
            // (an auto-refresh tick fires while a manual Run is still paging the
            // fleet) and they do not resolve in start order, so without this a
            // superseded response could overwrite a newer one on screen — while the
            // backend, which now drops the superseded *cache* write, kept the newer
            // rows. The table would then disagree with paging and export.
            //
            // Not an early return: this run still owns the busy/refreshing flag it
            // set, and a manual Run superseded by a refresh tick would otherwise
            // leave `busy` stuck on forever.
            let superseded = util::is_superseded(self.run.query_seq.get_untracked(), seq);
            if superseded {
                flag.set(false);
                return;
            }
            match outcome {
                Ok(r) => {
                    // Jump back to page 1 on a manual run; an auto-refresh keeps the
                    // current page, clamped in case the new result is shorter. The
                    // bound comes from whichever collection this view pages — see
                    // `util::paged_total`, which is also what sizes the pager itself,
                    // so the two cannot disagree about how many pages exist.
                    let grouped = self.query.group_by.get_untracked().is_some();
                    let total = util::paged_total(
                        grouped,
                        r.rows_total,
                        self.query.groups_total.get_untracked(),
                    );
                    let page = if silent {
                        util::clamp_page(
                            self.query.patches_page.get_untracked(),
                            util::page_count(total, PATCHES_PAGE_SIZE),
                        )
                    } else {
                        // A manual run returns to page 1 in the canonical order.
                        self.query.patches_sort.set(None);
                        0
                    };
                    self.query.patches_page.set(page);
                    // Fetch only the collection the active view renders, mirroring
                    // `set_group_by`. A grouped view is built from group headers and
                    // per-group member pages, none of which ride along with the
                    // summary — so without this the previous query's headers and
                    // cached members stayed on screen against the new result's counts,
                    // and re-ticking a checkbox could select a device/patch pair that
                    // isn't in the current result at all. The flat rows behind a
                    // grouped view are never drawn, so fetching them alongside was a
                    // wasted round trip on every auto-refresh tick; switching back to
                    // flat re-fetches page 0 through `set_group_by`.
                    if grouped {
                        self.query.expanded.update(|e| e.clear());
                        self.query.members.update(|m| m.clear());
                        self.fetch_groups(page);
                    } else if page == 0 && self.query.patches_sort.get_untracked().is_none() {
                        // Page 0 ships inline with the summary (canonical order), so
                        // seed it directly; a later page — or a silent refresh with an
                        // active sort — is fetched instead.
                        self.query.page_rows.set(r.rows.clone());
                    } else {
                        self.fetch_page(page);
                    }
                    // Reclaim the fold. The filter panel is ~487px tall and always
                    // opened expanded, so on the app's own default window not one
                    // patch row was visible on first paint — the operator scrolled
                    // past the controls to reach the thing they ran the query for.
                    // Only when they have expressed no preference: an explicit
                    // toggle is remembered and always wins.
                    if !silent && api::ui_pref(api::PREF_FILTERS_COLLAPSED).is_none() {
                        self.ui.filters_collapsed.set(true);
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
                Ok(response) => {
                    self.query.groups_total.set(response.total);
                    self.query.query_error.set(None);
                    // The caller could only bound `page` by the *previous* result's
                    // group total; this response carries the real one. If the new
                    // result is shorter the request was past the end and came back
                    // empty, so land on the last real page instead of showing an
                    // empty table. Re-entrant exactly once: the retry is issued
                    // against the total we just stored, so its own clamp is a no-op.
                    let clamped =
                        util::clamp_page(page, util::page_count(response.total, PATCHES_PAGE_SIZE));
                    if clamped != page {
                        self.query.patches_page.set(clamped);
                        self.fetch_groups(clamped);
                        return;
                    }
                    self.query.groups.set(response.groups);
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
        // Every location up front, matching the signed-in path: with no organization
        // selected the demo's location picker would otherwise be empty and disabled.
        self.lookups.locations.set(demo::sample_locations(&[]));
        self.lookups.node_classes.set(demo::sample_node_classes());
        self.session.demo.set(true);
    }

    /// Demo-mode counterpart to `run_query`: filters the in-memory sample with the
    /// current facets (no backend, no auth) and recomputes the row count.
    /// Narrows the filters to one organization and shows the matching patch rows.
    ///
    /// The rollup tabs used to be terminal: reading "Contoso · 63% · 41 pending
    /// Critical/Important" gave the operator no way to reach those 41 rows except to
    /// scroll back to Filters, re-pick the org by hand, tick Severity, press Run and
    /// switch tabs. That is what made five tabs read as five disconnected reports
    /// rather than one console — and it was self-inflicted, because the whole fleet is
    /// already cached backend-side, so an org/severity narrowing is a client-side
    /// re-filter with zero HTTP calls.
    ///
    /// Matching on the display name is deliberate: the compliance rollup is keyed by
    /// org *name* (`ComplianceBucket.organization`) because that is what labels a row,
    /// and the backend's synthetic `(unknown)` bucket has no id at all. An unmatched
    /// name leaves the org scope alone rather than silently clearing it.
    pub(super) fn drill_to_org(self, organization: String) {
        let id = self
            .lookups
            .orgs
            .with_untracked(|orgs| orgs.iter().find(|o| o.name == organization).map(|o| o.id));
        match id {
            // Replaces the org scope rather than adding to it: a drill-down means
            // "show me this org's rows", not "add this org to whatever was picked".
            Some(id) => {
                self.filters.org_ids.set(vec![id]);
                self.reload_locations();
            }
            None => self.notify(Toast::err(format!(
                "No organization named \"{organization}\" in the current scope — showing every org."
            ))),
        }
        self.drill_run();
    }

    /// Narrows to a single severity band and shows the matching patch rows. Replaces
    /// the selection rather than adding to it, so clicking a chart segment shows that
    /// segment and not an accumulation of everything clicked before it.
    pub(super) fn drill_to_severity(self, severity: String) {
        self.filters.selected_severities.set(vec![severity]);
        self.drill_run();
    }

    /// Narrows to one patch by KB (or by name when the patch carries no KB — third
    /// party patches have no `kbNumber`) and shows the affected rows. `search_allowed`
    /// matches the needle against both fields, so one control covers both cases.
    pub(super) fn drill_to_patch(self, needle: String) {
        self.filters.search.set(needle);
        self.drill_run();
    }

    /// Shared tail of every drill-down: re-run and land the operator on the rows.
    ///
    /// Flat view on purpose — a drill-down is a request to see *the rows behind this
    /// number*, and a grouped view would re-collapse them behind headers. Filters are
    /// left expanded/collapsed as the operator had them; the chip row already reports
    /// what the drill-down applied.
    fn drill_run(self) {
        self.query.patches_page.set(0);
        self.set_group_by(None);
        self.ui.active_tab.set(Tab::Patches);
        self.run_query();
    }

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
        // Same reason as the live path: a grouped view's headers and members don't
        // ride along with the result, so they'd otherwise describe the last query.
        // `fetch_groups` re-derives them from `demo_rows()`, which reads the result
        // just set above.
        if self.query.group_by.get_untracked().is_some() {
            self.query.expanded.update(|e| e.clear());
            self.query.members.update(|m| m.clear());
            self.fetch_groups(0);
        }
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
        // The backend dropped its cached result when the tenant changed, so whatever
        // is on screen can no longer be paged, sorted, grouped or exported — every
        // one of those re-reads that cache and would now find the miss. Clearing is
        // the honest end of the same rule `query_patches` enforces for a query that
        // spans the switch.
        if v.tenant_changed {
            self.clear_session();
            // The scope ids belong to the previous tenant's lookups. Left in place
            // they narrowed the next query to organizations that do not exist here,
            // and the chips could only name them as "(not found)".
            self.filters.org_ids.set(Vec::new());
            self.filters.loc_ids.set(Vec::new());
            self.filters.role_ids.set(Vec::new());
        }
    }

    /// Everything the backend drops on sign-out, sign-in, re-authorization and a
    /// tenant switch (`commands::auth::clear_session_state`): the cached result
    /// *and* the job store *and* any pending confirmation. The frontend used to
    /// refresh only the auth badge, so the table, the selection and the Jobs list
    /// stayed rendered over a cache that was already gone — the next page came back
    /// blank under "Rows 101–200 of N", and Export said "Run a query before
    /// exporting" beside a visible table.
    pub(super) fn clear_session(self) {
        self.clear_results();
        self.actions.jobs.set(Vec::new());
        self.actions.pending.set(None);
        self.actions.confirm_input.set(String::new());
    }

    /// Drops everything derived from the last query: the summary, the current page,
    /// grouping state, the applied-filter chips and any device selection made
    /// against those rows.
    pub(super) fn clear_results(self) {
        self.query.result.set(None);
        self.query.page_rows.set(Vec::new());
        self.query.patches_page.set(0);
        self.query.patches_sort.set(None);
        self.query.groups.set(Vec::new());
        self.query.expanded.update(|e| e.clear());
        self.query.members.update(|m| m.clear());
        self.query.applied_filters.set(None);
        self.query.query_error.set(None);
        self.clear_selection();
        self.actions.results_stale.set(false);
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
        self.filters.role_ids.set(f.role_ids);
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
        // Load the locations for the restored org scope, then restore the saved
        // location selection — the list has to exist before the ids can be pruned
        // against it.
        self.filters.org_ids.set(f.organization_ids);
        let want_locs = f.location_ids;
        self.filters.loc_ids.set(Vec::new());
        self.lookups.locations.set(Vec::new());
        let orgs = self.filters.org_ids.get_untracked();
        if self.session.demo.get_untracked() {
            self.lookups.locations.set(demo::sample_locations(&orgs));
            self.filters.loc_ids.set(want_locs);
            self.prune_selected_locations();
            return;
        }
        spawn_local(async move {
            match api::list_locations(orgs).await {
                Ok(locs) => {
                    self.lookups.locations.set(locs);
                    self.filters.loc_ids.set(want_locs);
                    self.prune_selected_locations();
                }
                Err(e) => self.notify(Toast::err(format!("Couldn't load locations: {e}"))),
            }
        });
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
        self.actions
            .selected
            .update(|sel| util::apply_row_selection(sel, row, checked));
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

    /// Per-device targets for a remediation kind, and the devices that have any.
    pub(super) fn remediation_targets(self, kind: ActionKind) -> BTreeMap<i64, Vec<String>> {
        self.actions
            .selected
            .with(|sel| util::remediation_targets(sel, kind))
    }

    /// Per-device KBs a hand-picked `kbAllowList` script would receive. Same shape as
    /// the OS remediation targets — the checkbox says "the selected KBs", and there
    /// is only one honest reading of that.
    pub(super) fn script_kb_targets(self) -> BTreeMap<i64, Vec<String>> {
        self.actions
            .selected
            .with(|sel| util::targets_by_device(sel, true))
    }

    /// Whether a library script id is configured for this remediation kind's patch
    /// family. Advisory — the backend re-reads the same setting and blocks without
    /// one; this is what lets the button explain itself instead of failing on click.
    pub(super) fn remediation_script_configured(self, kind: ActionKind) -> bool {
        self.settings.f_actions.with(|a| {
            if kind.is_os_family() {
                a.os_patch_script_id.is_some()
            } else {
                a.software_patch_script_id.is_some()
            }
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

    /// Whether the action bar should be rendered at all.
    ///
    /// Distinct from [`can_act`](Self::can_act), which is whether the buttons work.
    /// An install that never switched patch actions on in Settings is a read-only
    /// reporting tool, and ~95px of permanently disabled dispatch controls sat above
    /// the table on every query — in a layout where the table was already below the
    /// fold. Demo and browser mode keep it: the hosted page showing an action
    /// surface it cannot use is an honest advertisement, and hiding it there would
    /// make the demo misrepresent the product.
    pub(super) fn action_surface_visible(self) -> bool {
        self.session.web_mode.get()
            || self.session.demo.get()
            || self
                .session
                .auth
                .with(|a| a.as_ref().is_some_and(|a| a.actions_enabled))
    }

    /// Whether the action affordances should be live. The backend re-checks all of
    /// this — this only decides what the UI offers.
    pub(super) fn can_act(self) -> bool {
        self.blocked_reason_untracked().is_none()
    }

    /// Builds the request for `kind` from the current selection and form state.
    /// Reads the run options out of the signals and hands them to the pure
    /// `util::build_action_request`.
    ///
    /// The branching this used to do inline decides which devices get dispatched to
    /// and what each is told to install — including the rule that a remediation kind
    /// skips devices with nothing ticked of its family, which exists because handing
    /// one an empty allow list produces a job that reports success having installed
    /// nothing. That belongs somewhere a test can reach it; this file has no test
    /// module, and the crate's only gates are a compile check and clippy.
    pub(super) fn build_request(self, kind: ActionKind) -> ActionRequest {
        let opts = util::RunOptions {
            use_kb_targeting: self.actions.use_kb_targeting.get_untracked(),
            include_offline: self.actions.include_offline.get_untracked(),
            override_window: self.actions.override_window.get_untracked(),
            dry_run: self.actions.dry_run.get_untracked(),
            script_reboot: self.actions.script_reboot.get_untracked(),
            run_as: self.actions.run_as.get_untracked(),
            reboot_mode_forced: self.actions.reboot_mode.get_untracked() == "FORCED",
            reason: self.actions.reason.get_untracked(),
            script_id: self.actions.script_id.get_untracked(),
            script_name: {
                let id = self.actions.script_id.get_untracked();
                self.actions
                    .scripts
                    .with_untracked(|s| s.iter().find(|s| Some(s.id) == id).map(|s| s.name.clone()))
            },
            script_params: self.actions.script_params.get_untracked(),
        };
        self.actions
            .selected
            .with_untracked(|sel| util::build_action_request(kind, sel, &opts))
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
        // A dry run changes nothing on the device, so the results on screen are not
        // stale after it. `dry_run` defaults on, so without this every default
        // "Preview on N devices" raised the amber banner and its Refresh link forced
        // a whole-fleet refetch for nothing.
        let mutating = request.kind.is_mutating() && !request.dry_run;

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
