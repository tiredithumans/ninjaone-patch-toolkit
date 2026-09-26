//! All reactive state shared via context: the `AppState` wrapper, its nine
//! `Copy` sub-structs (grouped by concern), and the frontend-only value types
//! they carry (`Tab`, `AppliedFilters`, `Toast`, `Progress`, `DeviceSelection`).
//!
//! The `impl AppState` methods that orchestrate across those groups live in the
//! submodules, one file per concern. Like this file they have no test module:
//! anything worth asserting belongs in `util`.

use std::collections::{BTreeMap, BTreeSet};

use leptos::task::spawn_local;

use super::*;

mod actions;
mod lookups;
mod presets;
mod query;
mod selection;
mod view;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Tab {
    Patches,
    Compliance,
    Reboot,
    Failures,
    Trend,
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
pub(crate) struct Progress {
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
    /// A *desktop* build found no `window.__TAURI__`. Distinct from `web_mode`,
    /// which is the same observation in a build where it is expected and fine.
    /// Replaces the whole UI with an error rather than falling back to sample data.
    pub(super) backend_missing: RwSignal<bool>,
}

impl SessionState {
    pub(super) fn new() -> Self {
        Self {
            auth: RwSignal::new(None),
            signing_in: RwSignal::new(false),
            demo: RwSignal::new(false),
            web_mode: RwSignal::new(false),
            backend_missing: RwSignal::new(false),
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
    /// Stamp of the newest page/group-header request. A response carrying an older
    /// stamp is dropped: requests overlap (Next clicked twice, a sort change while
    /// a page is loading, a refresh landing mid-page) and resolve in any order, so
    /// without it a slow response overwrote a newer one on screen.
    pub(super) view_seq: RwSignal<u64>,
    /// Bumped whenever `members` is thrown away (new result, new grouping). A
    /// member fetch started before that is from a result no longer on screen, so
    /// it must neither fill the cache nor tick anything into the selection.
    pub(super) members_gen: RwSignal<u64>,
}

impl QueryState {
    /// Stamps a new page/group-header request; see `view_seq`.
    pub(super) fn next_view_seq(self) -> u64 {
        let seq = util::next_query_seq(self.view_seq.get_untracked());
        self.view_seq.set(seq);
        seq
    }

    /// Collapses every group and forgets the loaded members, invalidating any
    /// member fetch still in flight.
    pub(super) fn reset_members(self) {
        self.expanded.update(|e| e.clear());
        self.members.update(|m| m.clear());
        self.members_gen.update(|g| *g = util::next_query_seq(*g));
    }

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
            view_seq: RwSignal::new(0),
            members_gen: RwSignal::new(0),
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
    /// A manual run requested while another was in flight, waiting for it to
    /// settle. Holds the queued run's `force` flag; see `util::queue_run`.
    pub(super) queued: RwSignal<Option<bool>>,
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
            queued: RwSignal::new(None),
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
        // Auto-dismiss after a few seconds; a newer toast supersedes this one via
        // the generation guard. An error stays three times as long: it usually
        // carries a backend message worth reading in full, and 7s was not enough
        // to read one — let alone act on it — before it vanished.
        let ms = if t.error { 12_000 } else { 4000 };
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
    /// Why the last `run_action` from the open dialog failed. Shown inside the
    /// dialog, and it disables Run: the confirm token is single-use and already
    /// spent, so the only way forward is a fresh plan (or Cancel).
    pub(super) dispatch_error: RwSignal<Option<String>>,
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
            dispatch_error: RwSignal::new(None),
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
}
