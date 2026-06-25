use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::types::*;

const ROW_DISPLAY_CAP: usize = 1000;

const REGIONS: [(&str, &str); 5] = [
    ("https://app.ninjarmm.com", "North America (app)"),
    ("https://us2.ninjarmm.com", "North America (us2)"),
    ("https://eu.ninjarmm.com", "Europe (eu)"),
    ("https://oc.ninjarmm.com", "Oceania (oc)"),
    ("https://ca.ninjarmm.com", "Canada (ca)"),
];

const STATUS_OPTIONS: [&str; 5] = ["PENDING", "APPROVED", "REJECTED", "INSTALLED", "FAILED"];

#[derive(Clone, Copy, PartialEq)]
enum Tab {
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

    org_id: RwSignal<Option<i64>>,
    loc_id: RwSignal<Option<i64>>,
    role_id: RwSignal<Option<i64>>,
    selected_classes: RwSignal<Vec<String>>,
    os_name: RwSignal<String>,
    search: RwSignal<String>,

    patch_type: RwSignal<String>,
    statuses: RwSignal<Vec<String>>,
    install_days: RwSignal<i64>,
    refresh_secs: RwSignal<u32>,

    result: RwSignal<Option<QueryResult>>,
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
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
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
            org_id: RwSignal::new(None),
            loc_id: RwSignal::new(None),
            role_id: RwSignal::new(None),
            selected_classes: RwSignal::new(Vec::new()),
            os_name: RwSignal::new(String::new()),
            search: RwSignal::new(String::new()),
            patch_type: RwSignal::new("ALL".to_string()),
            statuses: RwSignal::new(vec!["PENDING".to_string()]),
            install_days: RwSignal::new(30),
            refresh_secs: RwSignal::new(0),
            result: RwSignal::new(None),
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

    fn load_lookups(self) {
        spawn_local(async move {
            match api::list_orgs().await {
                Ok(o) => self.orgs.set(o),
                Err(e) => self.notify(Toast::err(e)),
            }
        });
        spawn_local(async move {
            if let Ok(r) = api::list_roles().await {
                self.roles.set(r);
            }
        });
        spawn_local(async move {
            if let Ok(n) = api::list_node_classes().await {
                self.node_classes.set(n);
            }
        });
    }

    fn select_org(self, org: Option<i64>) {
        self.org_id.set(org);
        self.loc_id.set(None);
        self.locations.set(Vec::new());
        if let Some(id) = org {
            spawn_local(async move {
                if let Ok(locs) = api::list_locations(id).await {
                    self.locations.set(locs);
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
        FilterParams {
            organization_id: self.org_id.get_untracked(),
            location_id: self.loc_id.get_untracked(),
            role_id: self.role_id.get_untracked(),
            node_classes: self.selected_classes.get_untracked(),
            os_name_contains: non_empty(self.os_name.get_untracked()),
            search: non_empty(self.search.get_untracked()),
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
        let flag = if silent { self.refreshing } else { self.busy };
        flag.set(true);
        spawn_local(async move {
            match api::query_patches(args).await {
                Ok(r) => self.result.set(Some(r)),
                Err(e) => self.notify(Toast::err(e)),
            }
            flag.set(false);
        });
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
        self.role_id.set(f.role_id);
        self.selected_classes.set(f.node_classes);
        self.os_name.set(f.os_name_contains.unwrap_or_default());
        self.search.set(f.search.unwrap_or_default());
        // Load the org's locations, then restore the saved location.
        self.org_id.set(f.organization_id);
        self.loc_id.set(None);
        self.locations.set(Vec::new());
        if let Some(org) = f.organization_id {
            let want_loc = f.location_id;
            spawn_local(async move {
                if let Ok(locs) = api::list_locations(org).await {
                    self.locations.set(locs);
                    self.loc_id.set(want_loc);
                }
            });
        }
    }
}

#[component]
pub fn App() -> impl IntoView {
    let state = AppState::new();
    provide_context(state);

    // Initial load.
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
            <Show when=move || state.show_settings.get()>
                <SettingsPanel/>
            </Show>
            <FilterBar/>
            <QueryControls/>
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
                    <div class="modal">
                        <h2>{format!("Update available — v{}", info.version)}</h2>
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
                                let _ = api::sign_out().await;
                                state.refresh_auth();
                                state.notify(Toast::ok("Signed out"));
                            });
                        }
                    >
                        "Sign out"
                    </button>
                </Show>
            </div>
        </header>
    }
}

#[component]
fn SettingsPanel() -> impl IntoView {
    let state = expect_context::<AppState>();

    let save = move |_| {
        let args = SaveSettingsArgs {
            instance_base_url: state.f_instance.get_untracked(),
            client_id: non_empty(state.f_client_id.get_untracked()),
            callback_port: state.f_port.get_untracked(),
            install_window_days: state.f_install_days.get_untracked(),
            sla_days: state.f_sla.get_untracked(),
            client_secret: non_empty(state.f_client_secret.get_untracked()),
            clear_secret: false,
            auto_check_updates: state.f_auto_update.get_untracked(),
        };
        spawn_local(async move {
            match api::save_settings(args).await {
                Ok(v) => {
                    state.apply_settings_view(v);
                    state.f_client_secret.set(String::new());
                    state.refresh_auth();
                    state.notify(Toast::ok("Settings saved"));
                }
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    };

    let clear_secret = move |_| {
        spawn_local(async move {
            let args = SaveSettingsArgs {
                instance_base_url: state.f_instance.get_untracked(),
                client_id: non_empty(state.f_client_id.get_untracked()),
                callback_port: state.f_port.get_untracked(),
                install_window_days: state.f_install_days.get_untracked(),
                sla_days: state.f_sla.get_untracked(),
                client_secret: None,
                clear_secret: true,
                auto_check_updates: state.f_auto_update.get_untracked(),
            };
            match api::save_settings(args).await {
                Ok(v) => {
                    state.apply_settings_view(v);
                    state.notify(Toast::ok("Cleared stored secret"));
                }
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    };

    let check_now = move |_| {
        if state.update_busy.get_untracked() {
            return;
        }
        state.update_busy.set(true);
        spawn_local(async move {
            match api::check_for_update().await {
                Ok(Some(info)) => state.update.set(Some(info)),
                Ok(None) => state.notify(Toast::ok("You're on the latest version")),
                Err(e) => state.notify(Toast::err(e)),
            }
            state.update_busy.set(false);
        });
    };

    view! {
        <section class="panel settings">
            <h2>"Connection"</h2>
            <div class="grid">
                <label>
                    "Region / Instance"
                    <select on:change=move |ev| state.f_instance.set(event_target_value(&ev))>
                        {REGIONS
                            .iter()
                            .map(|(url, label)| {
                                let url = url.to_string();
                                let sel = {
                                    let url = url.clone();
                                    move || state.f_instance.get() == url
                                };
                                view! {
                                    <option value=url.clone() selected=sel>
                                        {label.to_string()}
                                    </option>
                                }
                            })
                            .collect_view()}
                        <option value="">"Custom…"</option>
                    </select>
                </label>
                <label>
                    "Instance URL"
                    <input
                        prop:value=move || state.f_instance.get()
                        on:input=move |ev| state.f_instance.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    "Client ID"
                    <input
                        prop:value=move || state.f_client_id.get()
                        on:input=move |ev| state.f_client_id.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    {move || {
                        if state.has_secret.get() {
                            "Client secret (leave blank to keep)"
                        } else {
                            "Client secret (Native apps have none)"
                        }
                    }}
                    <input
                        type="password"
                        prop:value=move || state.f_client_secret.get()
                        on:input=move |ev| state.f_client_secret.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    "Callback port"
                    <input
                        type="number"
                        min="1024"
                        max="65535"
                        prop:value=move || state.f_port.get().to_string()
                        on:change=move |ev| {
                            let v = event_target_value(&ev)
                                .parse::<u16>()
                                .unwrap_or_else(|_| state.f_port.get_untracked());
                            state.f_port.set(v.clamp(1024, 65535));
                        }
                    />
                </label>
                <label>
                    "Install history window (days)"
                    <input
                        type="number"
                        min="1"
                        max="3650"
                        prop:value=move || state.f_install_days.get().to_string()
                        on:change=move |ev| {
                            let v = event_target_value(&ev)
                                .parse::<i64>()
                                .unwrap_or_else(|_| state.f_install_days.get_untracked());
                            state.f_install_days.set(v.clamp(1, 3650));
                        }
                    />
                </label>
                <label>
                    "SLA window for aged criticals (days)"
                    <input
                        type="number"
                        min="1"
                        max="3650"
                        prop:value=move || state.f_sla.get().to_string()
                        on:change=move |ev| {
                            let v = event_target_value(&ev)
                                .parse::<i64>()
                                .unwrap_or_else(|_| state.f_sla.get_untracked());
                            state.f_sla.set(v.clamp(1, 3650));
                        }
                    />
                </label>
                <label class="inline">
                    <input
                        type="checkbox"
                        prop:checked=move || state.f_auto_update.get()
                        on:change=move |ev| state.f_auto_update.set(event_target_checked(&ev))
                    />
                    "Automatically check for updates on launch"
                </label>
            </div>
            <div class="row">
                <button class="btn btn-primary" on:click=save>
                    "Save settings"
                </button>
                <Show when=move || state.has_secret.get()>
                    <button class="btn btn-ghost" on:click=clear_secret>
                        "Clear stored secret"
                    </button>
                </Show>
                <button
                    class="btn btn-ghost"
                    prop:disabled=move || state.update_busy.get()
                    on:click=check_now
                >
                    {move || {
                        if state.update_busy.get() { "Checking…" } else { "Check for updates" }
                    }}
                </button>
            </div>
            <p class="app-version">
                {concat!("NinjaOne Patch Toolkit v", env!("CARGO_PKG_VERSION"))}
            </p>
        </section>
    }
}

#[component]
fn FilterBar() -> impl IntoView {
    let state = expect_context::<AppState>();

    view! {
        <section class="panel">
            <h2>"Filters"</h2>
            <div class="grid">
                <label>
                    "Organization"
                    <select
                        prop:value=move || {
                            state.org_id.get().map(|id| id.to_string()).unwrap_or_default()
                        }
                        on:change=move |ev| {
                            state.select_org(parse_opt(&event_target_value(&ev)));
                        }
                    >
                        <option value="">"All organizations"</option>
                        {move || {
                            state
                                .orgs
                                .get()
                                .into_iter()
                                .map(|o| {
                                    view! { <option value=o.id.to_string()>{o.name}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Location"
                    <select
                        prop:disabled=move || state.locations.get().is_empty()
                        prop:value=move || {
                            state.loc_id.get().map(|id| id.to_string()).unwrap_or_default()
                        }
                        on:change=move |ev| state.loc_id.set(parse_opt(&event_target_value(&ev)))
                    >
                        <option value="">"All locations"</option>
                        {move || {
                            state
                                .locations
                                .get()
                                .into_iter()
                                .map(|l| {
                                    view! { <option value=l.id.to_string()>{l.name}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Device Role"
                    <select
                        prop:value=move || {
                            state.role_id.get().map(|id| id.to_string()).unwrap_or_default()
                        }
                        on:change=move |ev| {
                            state.role_id.set(parse_opt(&event_target_value(&ev)))
                        }
                    >
                        <option value="">"All roles"</option>
                        {move || {
                            state
                                .roles
                                .get()
                                .into_iter()
                                .map(|r| {
                                    view! { <option value=r.id.to_string()>{r.name}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "OS name contains"
                    <input
                        placeholder="e.g. Server 2022"
                        prop:value=move || state.os_name.get()
                        on:input=move |ev| state.os_name.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    "Search (KB or name)"
                    <input
                        placeholder="e.g. KB5040434"
                        prop:value=move || state.search.get()
                        on:input=move |ev| state.search.set(event_target_value(&ev))
                    />
                </label>
            </div>
            <div class="chips">
                <span class="chips-label">"OS Type:"</span>
                {move || {
                    state
                        .node_classes
                        .get()
                        .into_iter()
                        .map(|nc| {
                            let value = nc.value.clone();
                            let checked = move || state.selected_classes.get().contains(&value);
                            let toggle_value = nc.value.clone();
                            view! {
                                <label class="chip">
                                    <input
                                        type="checkbox"
                                        prop:checked=checked
                                        on:change=move |_| {
                                            state.toggle_in(state.selected_classes, toggle_value.clone())
                                        }
                                    />
                                    {nc.label}
                                </label>
                            }
                        })
                        .collect_view()
                }}
            </div>
            <PresetRow/>
        </section>
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

#[component]
fn QueryControls() -> impl IntoView {
    let state = expect_context::<AppState>();
    let installed_selected = move || state.statuses.get().iter().any(|s| s == "INSTALLED");

    view! {
        <section class="panel">
            <div class="controls">
                <div class="control-group">
                    <span class="chips-label">"Type:"</span>
                    {["ALL", "OS", "SOFTWARE"]
                        .iter()
                        .map(|t| {
                            let t = t.to_string();
                            let val = t.clone();
                            let active = move || state.patch_type.get() == val;
                            let set = t.clone();
                            view! {
                                <button
                                    class=move || if active() { "seg seg-on" } else { "seg" }
                                    on:click=move |_| state.patch_type.set(set.clone())
                                >
                                    {t}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
                <div class="control-group">
                    <span class="chips-label">"Status:"</span>
                    {STATUS_OPTIONS
                        .iter()
                        .map(|s| {
                            let s = s.to_string();
                            let value = s.clone();
                            let checked = move || state.statuses.get().contains(&value);
                            let toggle = s.clone();
                            view! {
                                <label class="chip">
                                    <input
                                        type="checkbox"
                                        prop:checked=checked
                                        on:change=move |_| {
                                            state.toggle_in(state.statuses, toggle.clone())
                                        }
                                    />
                                    {s}
                                </label>
                            }
                        })
                        .collect_view()}
                </div>
                <Show when=installed_selected>
                    <label class="inline">
                        "Installed within (days)"
                        <input
                            type="number"
                            class="narrow"
                            min="1"
                            max="3650"
                            prop:value=move || state.install_days.get().to_string()
                            on:change=move |ev| {
                                let v = event_target_value(&ev)
                                    .parse::<i64>()
                                    .unwrap_or_else(|_| state.install_days.get_untracked());
                                state.install_days.set(v.clamp(1, 3650));
                            }
                        />
                    </label>
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
                <Show when=move || state.refreshing.get()>
                    <span class="chips-label">"↻ refreshing…"</span>
                </Show>
                <button
                    class="btn btn-primary"
                    prop:disabled=move || state.busy.get()
                    on:click=move |_| state.run_query()
                >
                    {move || if state.busy.get() { "Running…" } else { "Run query" }}
                </button>
                <button
                    class="btn"
                    prop:disabled=move || state.result.get().is_none()
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
            </div>
        </section>
    }
}

#[component]
fn Results() -> impl IntoView {
    let state = expect_context::<AppState>();
    let tab = state.active_tab;

    let summary = move || {
        state.result.get().map(|r| {
            format!(
                "{} patch rows · {} devices · generated {}",
                r.rows.len(),
                r.devices_total,
                r.generated_at
            )
        })
    };

    view! {
        <section class="panel results">
            <div class="tabs">
                <button
                    class=move || tab_class(tab.get(), Tab::Patches)
                    on:click=move |_| tab.set(Tab::Patches)
                >
                    "Patches"
                </button>
                <button
                    class=move || tab_class(tab.get(), Tab::Compliance)
                    on:click=move |_| tab.set(Tab::Compliance)
                >
                    "Compliance"
                </button>
                <button
                    class=move || tab_class(tab.get(), Tab::Reboot)
                    on:click=move |_| tab.set(Tab::Reboot)
                >
                    "Needs Reboot"
                </button>
                <span class="result-summary">{summary}</span>
            </div>
            {move || match tab.get() {
                Tab::Patches => view! { <PatchesTable/> }.into_any(),
                Tab::Compliance => view! { <ComplianceTable/> }.into_any(),
                Tab::Reboot => view! { <RebootTable/> }.into_any(),
            }}
        </section>
    }
}

#[component]
fn PatchesTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    let rows = move || state.result.get().map(|r| r.rows).unwrap_or_default();
    let total = move || rows().len();

    view! {
        <Show
            when=move || state.result.get().is_some()
            fallback=|| view! { <p class="empty">"Run a query to list patches."</p> }
        >
            <Show when=move || { total() > ROW_DISPLAY_CAP }>
                <p class="note">
                    {move || {
                        format!(
                            "Showing first {} of {} rows. Export to Excel for the full set.",
                            ROW_DISPLAY_CAP,
                            total(),
                        )
                    }}
                </p>
            </Show>
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th>"Organization"</th>
                            <th>"Location"</th>
                            <th>"Role"</th>
                            <th>"Device"</th>
                            <th>"OS"</th>
                            <th>"Type"</th>
                            <th>"KB"</th>
                            <th>"Patch"</th>
                            <th>"Severity"</th>
                            <th>"Status"</th>
                            <th>"Release"</th>
                            <th>"Installed"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            rows()
                                .into_iter()
                                .take(ROW_DISPLAY_CAP)
                                .map(|r| {
                                    let sev = sev_class(&r.severity);
                                    let stat = status_class(&r.status);
                                    view! {
                                        <tr>
                                            <td>{r.organization}</td>
                                            <td>{r.location.unwrap_or_default()}</td>
                                            <td>{r.device_role.unwrap_or_default()}</td>
                                            <td>{r.device_name}</td>
                                            <td>{r.os_name.unwrap_or_default()}</td>
                                            <td>{r.patch_type}</td>
                                            <td>{r.kb.unwrap_or_default()}</td>
                                            <td class="patch-name">{r.name}</td>
                                            <td>
                                                <span class=sev>{r.severity}</span>
                                            </td>
                                            <td>
                                                <span class=stat>{r.status}</span>
                                            </td>
                                            <td>{r.release_date.unwrap_or_default()}</td>
                                            <td>{r.installed_date.unwrap_or_default()}</td>
                                        </tr>
                                    }
                                })
                                .collect_view()
                        }}
                    </tbody>
                </table>
            </div>
        </Show>
    }
}

#[component]
fn ComplianceTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    let buckets = move || state.result.get().map(|r| r.compliance).unwrap_or_default();

    view! {
        <Show
            when=move || !buckets().is_empty()
            fallback=|| view! { <p class="empty">"No compliance data yet."</p> }
        >
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th>"Organization"</th>
                            <th>"Devices"</th>
                            <th>"Compliant"</th>
                            <th>"Compliance"</th>
                            <th>"Pending Crit/Imp"</th>
                            <th>"Aged (past SLA)"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            buckets()
                                .into_iter()
                                .map(|b| {
                                    let pct = format!("{:.0}%", b.compliance_pct);
                                    let aged_class = if b.aged_critical > 0 {
                                        "sev-critical"
                                    } else {
                                        ""
                                    };
                                    view! {
                                        <tr>
                                            <td>{b.organization}</td>
                                            <td>{b.devices_total}</td>
                                            <td>{b.devices_compliant}</td>
                                            <td>{pct}</td>
                                            <td>{b.pending_critical}</td>
                                            <td>
                                                <span class=aged_class>{b.aged_critical}</span>
                                            </td>
                                        </tr>
                                    }
                                })
                                .collect_view()
                        }}
                    </tbody>
                </table>
            </div>
        </Show>
    }
}

#[component]
fn RebootTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    let devices = move || {
        state
            .result
            .get()
            .map(|r| {
                r.devices
                    .into_iter()
                    .filter(|d| d.needs_reboot)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    view! {
        <Show
            when=move || !devices().is_empty()
            fallback=|| view! { <p class="empty">"No devices flagged for reboot."</p> }
        >
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th>"Organization"</th>
                            <th>"Location"</th>
                            <th>"Role"</th>
                            <th>"Device"</th>
                            <th>"OS"</th>
                            <th>"Pending patches"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            devices()
                                .into_iter()
                                .map(|d| {
                                    view! {
                                        <tr>
                                            <td>{d.organization}</td>
                                            <td>{d.location.unwrap_or_default()}</td>
                                            <td>{d.device_role.unwrap_or_default()}</td>
                                            <td>{d.device_name}</td>
                                            <td>{d.os_name.unwrap_or_default()}</td>
                                            <td>{d.pending_count}</td>
                                        </tr>
                                    }
                                })
                                .collect_view()
                        }}
                    </tbody>
                </table>
            </div>
        </Show>
    }
}

#[component]
fn Toaster() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <Show when=move || state.toast.get().is_some()>
            {move || {
                state
                    .toast
                    .get()
                    .map(|t| {
                        let cls = if t.error { "toast toast-err" } else { "toast toast-ok" };
                        view! {
                            <div class=cls>
                                <span>{t.msg}</span>
                                <button class="x" on:click=move |_| state.toast.set(None)>
                                    "×"
                                </button>
                            </div>
                        }
                    })
            }}
        </Show>
    }
}

fn parse_opt(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}

fn tab_class(active: Tab, this: Tab) -> &'static str {
    if active == this { "tab tab-on" } else { "tab" }
}

fn sev_class(sev: &str) -> &'static str {
    match sev {
        "Critical" => "sev sev-critical",
        "Important" => "sev sev-important",
        "Moderate" => "sev sev-moderate",
        "Low" => "sev sev-low",
        _ => "sev sev-none",
    }
}

fn status_class(status: &str) -> &'static str {
    match status {
        "INSTALLED" => "stat stat-installed",
        "APPROVED" => "stat stat-approved",
        "PENDING" => "stat stat-pending",
        "REJECTED" => "stat stat-rejected",
        "FAILED" => "stat stat-failed",
        _ => "stat",
    }
}
