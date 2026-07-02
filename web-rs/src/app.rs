use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::demo;
use crate::types::*;

mod charts;
mod filters;
mod settings;
mod state;
mod tables;
mod util;

use charts::{ComplianceByOsBars, ComplianceCharts};
use filters::Filters;
use settings::SettingsPanel;
use state::*;
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
