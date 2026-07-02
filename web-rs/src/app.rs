use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::demo;
use crate::types::*;

mod charts;
mod controls;
mod filters;
mod header;
mod settings;
mod state;
mod tables;
mod toaster;
mod util;

use charts::{ComplianceByOsBars, ComplianceCharts};
use controls::RunControls;
use filters::Filters;
use header::Header;
use settings::SettingsPanel;
use state::*;
use tables::Results;
use toaster::Toaster;
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
