use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::demo;
use crate::types::*;

mod actions;
mod charts;
mod controls;
mod filters;
mod header;
mod settings;
mod state;
mod tables;
mod toaster;
mod update;
mod util;

use actions::{ActionBar, ConfirmActionModal, JobsTable};
use charts::{ComplianceByOsBars, ComplianceCharts};
use controls::RunControls;
use filters::Filters;
use header::Header;
use settings::SettingsPanel;
use state::*;
use tables::Results;
use toaster::Toaster;
use update::UpdateSplash;
use util::{
    MdBlock, MdSpan, SummaryCounts, action_blocked_reason, aged_badge, aria_sort, date_to_epoch,
    epoch_to_date, filter_chips, format_duration, group_thousands, is_fleet_tab, job_mode_label,
    next_sort, non_empty, parse_changelog, parse_opt, patch_key, sev_class, sort_glyph,
    sort_patch_rows, status_class, summary_line, tab_class,
};

const PATCHES_PAGE_SIZE: usize = 100;

/// How many member rows an expanded group loads. A by-patch group can span the
/// whole fleet (one Chrome update covers every device), so the expand is capped —
/// and because the group checkbox only ever ticks *loaded* members, the cap also
/// stops a single click selecting thousands of rows the operator never saw. Well
/// above the 25-device blast-radius cap the backend enforces on dispatch.
const GROUP_MEMBER_LIMIT: usize = 500;

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

/// Severity facet options as (raw value sent to the backend, display label), in
/// `Severity::rank()` order. Covers NinjaOne's full vocabulary, not just the MSRC
/// subset: `SECURITY` and `RECOMMENDED` are its own classifications, and `UNKNOWN`
/// is selectable because an unrated patch is otherwise unreachable — ticking any
/// severity would silently exclude every patch NinjaOne didn't grade.
const SEVERITY_OPTIONS: [(&str, &str); 8] = [
    ("CRITICAL", "Critical"),
    ("IMPORTANT", "Important"),
    ("SECURITY", "Security"),
    ("MODERATE", "Moderate"),
    ("RECOMMENDED", "Recommended"),
    ("LOW", "Low"),
    ("OPTIONAL", "Optional"),
    ("UNKNOWN", "Unknown"),
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
                let can_act = authed && a.actions_enabled && a.write_enabled;
                state.session.auth.set(Some(a));
                if authed {
                    state.load_lookups();
                }
                if can_act {
                    state.load_scripts();
                    state.refresh_jobs();
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
        // The action surface still renders in the demo — hiding it would make the
        // hosted page a dishonest advertisement — but every control is disabled and
        // says why. `web_mode` alone drives that now, via `blocked_reason`.
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

    // Live job status from the backend poller. Rows arrive already advanced, so
    // this merges them in by job id rather than refetching the whole list.
    api::on_action_progress(move |ev| {
        state
            .actions
            .dispatch_progress
            .set(match ev.stage.as_str() {
                "dispatching" => Some((ev.dispatched, ev.total)),
                _ => None,
            });
        if ev.jobs.is_empty() {
            return;
        }
        state.actions.jobs.update(|jobs| {
            for incoming in ev.jobs {
                match jobs.iter_mut().find(|j| j.id == incoming.id) {
                    Some(slot) => *slot = incoming,
                    None => jobs.push(incoming),
                }
            }
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
            <ConfirmActionModal/>
        </main>
    }
}
