use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::demo;
use crate::types::*;

mod filters;
mod settings;
mod tables;
mod util;

use filters::Filters;
use settings::SettingsPanel;
use tables::Results;
use util::{
    date_to_epoch, epoch_to_date, group_thousands, non_empty, parse_opt, sev_class, status_class,
    tab_class,
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

/// All reactive state, shared via context. `RwSignal` is `Copy`, so this whole
/// struct is `Copy` and cheap to hand to every component.
#[derive(Clone, Copy)]
pub struct AppState {
    toast: RwSignal<Option<Toast>>,
    auth: RwSignal<Option<AuthStatus>>,
    show_settings: RwSignal<bool>,
    busy: RwSignal<bool>,
    signing_in: RwSignal<bool>,
    active_tab: RwSignal<Tab>,

    orgs: RwSignal<Vec<Organization>>,
    locations: RwSignal<Vec<Location>>,
    roles: RwSignal<Vec<Role>>,
    node_classes: RwSignal<Vec<NodeClass>>,
    /// Count of in-flight org/role/class lookup requests; > 0 means "loading".
    lookups_pending: RwSignal<u32>,

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
    refresh_secs: RwSignal<u32>,

    result: RwSignal<Option<QueryResult>>,
    /// Zero-based page index for the paginated Patches table.
    patches_page: RwSignal<usize>,
    /// The detail rows for the currently displayed page, fetched from the backend
    /// cache via `get_patch_rows` (the full row set is never shipped over IPC).
    page_rows: RwSignal<Vec<PatchRow>>,
    /// Collapses the Filters panel body to give the results more room. Expanded
    /// (false) by default.
    filters_collapsed: RwSignal<bool>,
    presets: RwSignal<Vec<Preset>>,
    preset_name: RwSignal<String>,

    f_instance: RwSignal<String>,
    f_client_id: RwSignal<String>,
    f_client_secret: RwSignal<String>,
    f_port: RwSignal<u16>,
    f_install_days: RwSignal<i64>,
    f_sla: RwSignal<i64>,
    has_secret: RwSignal<bool>,
    f_auto_update: RwSignal<bool>,

    update: RwSignal<Option<UpdateInfo>>,
    update_busy: RwSignal<bool>,

    refreshing: RwSignal<bool>,
    toast_gen: RwSignal<u64>,

    /// Wall-clock timing for the running-query progress bar / elapsed display.
    /// `elapsed_tick` is bumped by a timer to re-evaluate the elapsed label.
    query_started_ms: RwSignal<f64>,
    elapsed_tick: RwSignal<u32>,
    last_duration_ms: RwSignal<Option<f64>>,
    /// Live record counts from backend `query:progress` events, plus a sequence
    /// number stamped on each run so stale events from a superseded run are dropped.
    progress: RwSignal<Progress>,
    query_seq: RwSignal<u64>,

    /// Sample data is loaded (drives the "sample data" banner). Set by `load_demo`.
    demo: RwSignal<bool>,
    /// Running in a plain browser with no Tauri backend — the GitHub Pages demo.
    /// Disables the backend-only actions (sign-in, live query, export).
    web_mode: RwSignal<bool>,
}

impl AppState {
    fn new() -> Self {
        Self {
            toast: RwSignal::new(None),
            auth: RwSignal::new(None),
            show_settings: RwSignal::new(false),
            busy: RwSignal::new(false),
            signing_in: RwSignal::new(false),
            active_tab: RwSignal::new(Tab::Patches),
            orgs: RwSignal::new(Vec::new()),
            locations: RwSignal::new(Vec::new()),
            roles: RwSignal::new(Vec::new()),
            node_classes: RwSignal::new(Vec::new()),
            lookups_pending: RwSignal::new(0),
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
            refresh_secs: RwSignal::new(0),
            result: RwSignal::new(None),
            patches_page: RwSignal::new(0),
            page_rows: RwSignal::new(Vec::new()),
            filters_collapsed: RwSignal::new(false),
            presets: RwSignal::new(Vec::new()),
            preset_name: RwSignal::new(String::new()),
            f_instance: RwSignal::new("https://us2.ninjarmm.com".to_string()),
            f_client_id: RwSignal::new(String::new()),
            f_client_secret: RwSignal::new(String::new()),
            f_port: RwSignal::new(11434),
            f_install_days: RwSignal::new(30),
            f_sla: RwSignal::new(30),
            has_secret: RwSignal::new(false),
            f_auto_update: RwSignal::new(true),
            update: RwSignal::new(None),
            update_busy: RwSignal::new(false),
            refreshing: RwSignal::new(false),
            toast_gen: RwSignal::new(0),
            query_started_ms: RwSignal::new(0.0),
            elapsed_tick: RwSignal::new(0),
            last_duration_ms: RwSignal::new(None),
            progress: RwSignal::new(Progress::default()),
            query_seq: RwSignal::new(0),
            demo: RwSignal::new(false),
            web_mode: RwSignal::new(false),
        }
    }

    fn is_authed(self) -> bool {
        self.auth.get().map(|a| a.authenticated).unwrap_or(false)
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

    fn refresh_auth(self) {
        spawn_local(async move {
            if let Ok(a) = api::auth_status().await {
                self.auth.set(Some(a));
            }
        });
    }

    fn loading_lookups(self) -> bool {
        self.lookups_pending.get() > 0
    }

    fn load_lookups(self) {
        self.lookups_pending.set(2);
        spawn_local(async move {
            match api::list_orgs().await {
                Ok(o) => self.orgs.set(o),
                Err(e) => self.notify(Toast::err(format!("Couldn't load organizations: {e}"))),
            }
            self.lookup_done();
        });
        spawn_local(async move {
            match api::list_roles().await {
                Ok(r) => self.roles.set(r),
                Err(e) => self.notify(Toast::err(format!("Couldn't load roles: {e}"))),
            }
            self.lookup_done();
        });
    }

    /// Loads the static OS-type list. It needs no auth or API call, so it runs at
    /// startup rather than waiting for sign-in like the org/role/location lookups.
    fn load_node_classes(self) {
        spawn_local(async move {
            match api::list_node_classes().await {
                Ok(n) => self.node_classes.set(n),
                Err(e) => self.notify(Toast::err(format!("Couldn't load OS types: {e}"))),
            }
        });
    }

    fn lookup_done(self) {
        self.lookups_pending.update(|n| *n = n.saturating_sub(1));
    }

    fn select_org(self, org: Option<i64>) {
        self.org_id.set(org);
        self.loc_id.set(None);
        self.locations.set(Vec::new());
        if let Some(id) = org {
            // Demo mode resolves locations from the sample, not the backend.
            if self.demo.get_untracked() {
                self.locations.set(demo::sample_locations(id));
                return;
            }
            spawn_local(async move {
                match api::list_locations(id).await {
                    Ok(locs) => self.locations.set(locs),
                    Err(e) => self.notify(Toast::err(format!("Couldn't load locations: {e}"))),
                }
            });
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

    fn run_query(self) {
        self.run_query_inner(false);
    }

    /// Auto-refresh variant: flags a subtle `refreshing` state instead of the main
    /// `busy` one (so the Run-query button doesn't flicker each tick) and stays
    /// quiet about precondition failures.
    fn run_query_auto(self) {
        self.run_query_inner(true);
    }

    fn run_query_inner(self, silent: bool) {
        if self.busy.get_untracked() || self.refreshing.get_untracked() {
            return;
        }
        // In demo mode there is no backend to query — filter the sample locally.
        if self.demo.get_untracked() {
            self.run_demo_query(silent);
            return;
        }
        if !self.is_authed() {
            if !silent {
                self.notify(Toast::err("Sign in first"));
            }
            return;
        }
        let statuses = self.statuses.get_untracked();
        if statuses.is_empty() {
            if !silent {
                self.notify(Toast::err("Select at least one status"));
            }
            return;
        }
        let args = PatchQueryArgs {
            filter: self.current_filter(),
            patch_type: self.patch_type.get_untracked(),
            statuses,
            install_after_days: Some(self.install_days.get_untracked()),
        };
        // Stamp this run so progress events from a superseded run are ignored, and
        // clear the previous run's counts.
        let seq = self.query_seq.get_untracked().wrapping_add(1);
        self.query_seq.set(seq);
        self.progress.set(Progress::default());
        let flag = if silent { self.refreshing } else { self.busy };
        let started = js_sys::Date::now();
        self.query_started_ms.set(started);
        flag.set(true);
        spawn_local(async move {
            match api::query_patches(args, seq).await {
                Ok(r) => {
                    // Jump back to page 1 on a manual run; an auto-refresh keeps the
                    // current page, clamped in case the new result is shorter.
                    let page_count = r.rows_total.div_ceil(PATCHES_PAGE_SIZE).max(1);
                    let page = if silent {
                        self.patches_page.get_untracked().min(page_count - 1)
                    } else {
                        0
                    };
                    self.patches_page.set(page);
                    // Page 0 ships inline with the summary, so seed it directly; any
                    // other page (only reachable via a silent refresh) is fetched.
                    if page == 0 {
                        self.page_rows.set(r.rows.clone());
                    } else {
                        self.fetch_page(page);
                    }
                    self.result.set(Some(r));
                }
                Err(e) => self.notify(Toast::err(e)),
            }
            // Record the round-trip so the next run can show "Last run took Ns"
            // and drive the estimated progress bar.
            self.last_duration_ms
                .set(Some(js_sys::Date::now() - started));
            flag.set(false);
        });
    }

    /// Loads the detail rows for `page` from the backend's cached result into
    /// `page_rows`. Paging fetches just the visible window rather than holding the
    /// whole row set in the frontend.
    fn fetch_page(self, page: usize) {
        spawn_local(async move {
            match api::get_patch_rows(page * PATCHES_PAGE_SIZE, PATCHES_PAGE_SIZE).await {
                Ok(rows) => self.page_rows.set(rows),
                Err(e) => self.notify(Toast::err(e)),
            }
        });
    }

    /// Enters demo mode (browser/Pages) without populating results: seeds the facet
    /// dropdowns from the sample and flags `demo` so **Run query** filters the sample
    /// locally. The results stay empty ("Run a query to list patches") until the user
    /// runs a query — exactly like the real app, which lists nothing until queried.
    fn enter_demo(self) {
        self.orgs.set(demo::sample_orgs());
        self.roles.set(demo::sample_roles());
        self.node_classes.set(demo::sample_node_classes());
        self.demo.set(true);
    }

    /// Demo-mode counterpart to `run_query`: filters the in-memory sample with the
    /// current facets (no backend, no auth) and recomputes the row count.
    fn run_demo_query(self, silent: bool) {
        let statuses = self.statuses.get_untracked();
        if statuses.is_empty() {
            if !silent {
                self.notify(Toast::err("Select at least one status"));
            }
            return;
        }
        let r = demo::filtered_result(
            &self.current_filter(),
            &self.patch_type.get_untracked(),
            &statuses,
            Some(self.install_days.get_untracked()),
        );
        self.patches_page.set(0);
        self.page_rows.set(r.rows.clone());
        self.result.set(Some(r));
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

    fn apply_settings_view(self, v: SettingsView) {
        self.f_instance.set(v.instance_base_url);
        self.f_client_id.set(v.client_id.unwrap_or_default());
        self.f_port.set(v.callback_port);
        self.f_install_days.set(v.install_window_days);
        self.f_sla.set(v.sla_days);
        self.has_secret.set(v.has_client_secret);
        self.f_auto_update.set(v.auto_check_updates);
        self.install_days.set(v.install_window_days);
        self.presets.set(v.presets);
    }

    fn apply_preset(self, p: Preset) {
        let f = p.filter;
        // Restore the patch-query selectors only when the preset captured them, so a
        // legacy preset leaves the current Type/Status/install-window untouched.
        if let Some(pt) = p.patch_type {
            self.patch_type.set(pt);
        }
        if let Some(st) = p.statuses {
            self.statuses.set(st);
        }
        if let Some(d) = p.install_days {
            self.install_days.set(d);
        }
        self.role_id.set(f.role_id);
        self.selected_classes.set(f.node_classes);
        self.selected_severities.set(f.severities);
        self.os_name.set(f.os_name_contains.unwrap_or_default());
        self.search.set(f.search.unwrap_or_default());
        // Restore the release-date filter UI from the stored bounds.
        match (f.release_within_days, f.release_after, f.release_before) {
            (Some(d), _, _) => {
                self.release_window.set(d.to_string());
                self.release_after_date.set(String::new());
                self.release_before_date.set(String::new());
            }
            (None, after, before) if after.is_some() || before.is_some() => {
                self.release_window.set("custom".to_string());
                self.release_after_date.set(epoch_to_date(after));
                self.release_before_date.set(epoch_to_date(before));
            }
            _ => {
                self.release_window.set(String::new());
                self.release_after_date.set(String::new());
                self.release_before_date.set(String::new());
            }
        }
        // Load the org's locations, then restore the saved location.
        self.org_id.set(f.organization_id);
        self.loc_id.set(None);
        self.locations.set(Vec::new());
        if let Some(org) = f.organization_id {
            let want_loc = f.location_id;
            spawn_local(async move {
                match api::list_locations(org).await {
                    Ok(locs) => {
                        self.locations.set(locs);
                        self.loc_id.set(want_loc);
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
                state.auth.set(Some(a));
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
                    state.update.set(Some(info));
                }
            }
        });
    } else {
        // Browser/Pages demo: there is no backend, so every IPC call would fail.
        // Enter demo mode (facets seeded from the sample) but leave the results
        // empty until the user presses Run query, just like the real app.
        state.web_mode.set(true);
        state.enter_demo();
    }

    // Stream live record counts from the backend into `progress`, ignoring events
    // from a run the user has already superseded.
    api::on_query_progress(move |ev| {
        if ev.query_id != state.query_seq.get_untracked() {
            return;
        }
        state.progress.update(|p| match ev.stage.as_str() {
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
        if state.busy.get_untracked() || state.refreshing.get_untracked() {
            state.elapsed_tick.update(|t| *t = t.wrapping_add(1));
        }
    })
    .forget();

    // Auto-refresh: rebuild the interval whenever the cadence or auth changes.
    let interval = StoredValue::new_local(None::<gloo_timers::callback::Interval>);
    Effect::new(move |_| {
        let secs = state.refresh_secs.get();
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
            <Show when=move || state.demo.get()>
                <p class="demo-banner" role="note">
                    "Demo mode — press Run query to list sample patches (not a live fleet)."
                </p>
            </Show>
            <Show when=move || state.show_settings.get()>
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

/// Modal shown when an update is available. Renders the new version + the
/// release notes (changelog) and offers to install + relaunch.
#[component]
fn UpdateSplash() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Escape dismisses the update splash (same as "Later"), unless an install is
    // already running.
    window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape"
            && state.update.get_untracked().is_some()
            && !state.update_busy.get_untracked()
        {
            state.update.set(None);
        }
    });
    view! {
        {move || {
            let Some(info) = state.update.get() else {
                return ().into_any();
            };
            let notes = info.notes.unwrap_or_default();
            let changelog = if notes.trim().is_empty() {
                None
            } else {
                Some(
                    view! {
                        <div class="changelog">
                            <h3>"What's new"</h3>
                            <div class="changelog-body">{notes}</div>
                        </div>
                    },
                )
            };
            let install = move |_| {
                state.update_busy.set(true);
                spawn_local(async move {
                    // On success the backend installs and relaunches the app, so
                    // this never returns Ok; an Err means the install failed.
                    if let Err(e) = api::install_update().await {
                        state.update_busy.set(false);
                        state.notify(Toast::err(format!("Update failed: {e}")));
                    }
                });
            };
            let dismiss = move |_| state.update.set(None);
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
                                prop:disabled=move || state.update_busy.get()
                                on:click=install
                            >
                                {move || {
                                    if state.update_busy.get() {
                                        "Updating…"
                                    } else {
                                        "Update & restart"
                                    }
                                }}
                            </button>
                            <button
                                class="btn btn-ghost"
                                prop:disabled=move || state.update_busy.get()
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
                <Show when=move || state.web_mode.get()>
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
                <Show when=move || !state.web_mode.get()>
                <span class=move || if authed() { "pill pill-on" } else { "pill pill-off" }>
                    {move || if authed() { "Connected" } else { "Not signed in" }}
                </span>
                <button class="btn" on:click=move |_| state.show_settings.update(|s| *s = !*s)>
                    "Settings"
                </button>
                <Show
                    when=move || authed()
                    fallback=move || {
                        view! {
                            <button
                                class="btn btn-primary"
                                prop:disabled=move || state.signing_in.get()
                                on:click=move |_| {
                                    if state.signing_in.get_untracked() {
                                        return;
                                    }
                                    state.signing_in.set(true);
                                    state
                                        .notify(Toast::ok("Complete the sign-in in your browser…"));
                                    spawn_local(async move {
                                        match api::sign_in().await {
                                            Ok(()) => {
                                                state.refresh_auth();
                                                state.load_lookups();
                                                state.notify(Toast::ok("Signed in"));
                                            }
                                            Err(e) => state.notify(Toast::err(e)),
                                        }
                                        state.signing_in.set(false);
                                    });
                                }
                            >
                                {move || {
                                    if state.signing_in.get() { "Signing in…" } else { "Sign in" }
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
                                        state.refresh_auth();
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
                    prop:disabled=move || state.busy.get()
                    on:click=move |_| state.run_query()
                >
                    {move || if state.busy.get() { "Running…" } else { "Run query" }}
                </button>
                <button
                    class="btn"
                    prop:disabled=move || {
                        state.result.get().is_none() || state.web_mode.get() || state.demo.get()
                    }
                    title=move || {
                        if state.web_mode.get() || state.demo.get() {
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
                <Show when=move || state.refreshing.get()>
                    <span class="chips-label">"↻ refreshing…"</span>
                </Show>
                <label class="inline">
                    "Auto-refresh"
                    <select on:change=move |ev| {
                        state.refresh_secs.set(event_target_value(&ev).parse().unwrap_or(0))
                    }>
                        {[("0", "Off"), ("30", "30s"), ("60", "1m"), ("300", "5m"), ("900", "15m")]
                            .into_iter()
                            .map(|(val, label)| {
                                let sel = move || state.refresh_secs.get().to_string() == val;
                                view! {
                                    <option value=val selected=sel>
                                        {label}
                                    </option>
                                }
                            })
                            .collect_view()}
                    </select>
                </label>
                <PresetRow/>
            </div>
            <Show when=move || state.busy.get()>
                <div class="query-progress">
                    <div class="progress">
                        {move || match state.progress_estimate() {
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
                            let p = state.progress.get();
                            let secs = state.elapsed_secs();
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
                !state.busy.get() && state.last_duration_ms.get().is_some()
            }>
                <p class="query-hint">
                    {move || {
                        format!(
                            "Last run took {:.0}s",
                            state.last_duration_ms.get().unwrap_or(0.0) / 1000.0,
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
                state
                    .toast
                    .get()
                    .map(|t| {
                        let cls = if t.error { "toast toast-err" } else { "toast toast-ok" };
                        view! {
                            <div class=cls>
                                <span>{t.msg}</span>
                                <button
                                    class="x"
                                    aria-label="Dismiss notification"
                                    on:click=move |_| state.toast.set(None)
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
        let name = state.preset_name.get_untracked();
        if name.trim().is_empty() {
            state.notify(Toast::err("Name the preset first"));
            return;
        }
        let preset = Preset {
            name: name.trim().to_string(),
            filter: state.current_filter(),
            patch_type: Some(state.patch_type.get_untracked()),
            statuses: Some(state.statuses.get_untracked()),
            install_days: Some(state.install_days.get_untracked()),
        };
        spawn_local(async move {
            match api::save_preset(preset).await {
                Ok(p) => {
                    state.presets.set(p);
                    state.preset_name.set(String::new());
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
                state
                    .presets
                    .get()
                    .into_iter()
                    .map(|p| {
                        let name = p.name.clone();
                        let label_name = p.name.clone();
                        let p2 = p.clone();
                        let del_name = p.name.clone();
                        view! {
                            <span class="chip chip-preset">
                                <button
                                    class="link"
                                    on:click=move |_| state.apply_preset(p2.clone())
                                >
                                    {name}
                                </button>
                                <button
                                    class="x"
                                    aria-label=format!("Delete preset {label_name}")
                                    on:click=move |_| {
                                        let n = del_name.clone();
                                        spawn_local(async move {
                                            if let Ok(p) = api::delete_preset(n).await {
                                                state.presets.set(p);
                                            }
                                        });
                                    }
                                >
                                    "×"
                                </button>
                            </span>
                        }
                    })
                    .collect_view()
            }}
            <input
                class="preset-name"
                placeholder="Preset name"
                prop:value=move || state.preset_name.get()
                on:input=move |ev| state.preset_name.set(event_target_value(&ev))
            />
            <button class="btn btn-ghost" on:click=save_preset>
                "Save preset"
            </button>
        </div>
    }
}
