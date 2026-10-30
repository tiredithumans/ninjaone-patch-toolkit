//! The results panel: the tab bar, the stale/error banners, the applied-filter
//! chips and the shared scope banner. Each tab's view lives in its own submodule.

use leptos::web_sys::HtmlElement;
use wasm_bindgen::JsCast;

use super::*;

mod backlog;
mod changes;
mod compliance;
mod cve;
mod device;
mod failures;
mod live_jobs;
mod patches;
mod reboot;
mod trend;

use changes::ChangesPanel;
use compliance::ComplianceTab;
use cve::CveSearch;
pub(crate) use device::DeviceDetailModal;
use device::DeviceLink;
use failures::FailuresTable;
use live_jobs::{DeviceRunningJobs, LiveJobsPanel, RunningJobChip};
use patches::{PatchesTable, reboot_likely_chip};
use reboot::RebootTable;
use trend::TrendTab;

/// Says what to do first, before there is anything to show: connect in Settings,
/// or sign in. Decided by `util::onboarding`, which waits for the saved settings.
#[component]
fn OnboardingCard() -> impl IntoView {
    let state = expect_context::<AppState>();
    let step = move || {
        util::onboarding(
            state.session.web_mode.get(),
            state.settings.loaded.get(),
            state.settings.has_client_id.get(),
            state.is_authed(),
        )
    };
    move || {
        step().map(|step| match step {
            util::Onboarding::Connect => view! {
                <div class="onboarding" role="note">
                    <h2>"Connect to NinjaOne"</h2>
                    <p>
                        "Open Settings, pick your region and enter the Client ID of your NinjaOne "
                        "API app, then sign in. The README's “NinjaOne setup” section lists what "
                        "the app needs on the NinjaOne side."
                    </p>
                    <button class="btn btn-primary" on:click=move |_| state.ui.show_settings.set(true)>
                        "Open Settings"
                    </button>
                </div>
            }
            .into_any(),
            util::Onboarding::SignIn => view! {
                <div class="onboarding" role="note">
                    <h2>"Sign in to load your fleet"</h2>
                    <p>
                        "The connection is set up. Sign in to list patches — a browser window "
                        "opens if NinjaOne needs you to log in."
                    </p>
                    <SignInButton/>
                </div>
            }
            .into_any(),
        })
    }
}

#[component]
pub(crate) fn Results() -> impl IntoView {
    let state = expect_context::<AppState>();
    let tab = state.ui.active_tab;

    let summary = move || {
        // Read the active tab so the line re-renders (and re-describes) on switch.
        let tab = tab.get();
        state
            .query
            .result
            .with(|r| r.as_ref().map(|r| summary_line(tab, &summary_counts(r))))
    };

    view! {
        <section class="panel results">
            <OnboardingCard/>
            <div class="tabs">
                // Only tabs are owned by the tablist: the group captions are visual
                // grouping (hidden from the accessibility tree, the tabs carry their
                // own names) and the summary line sits outside it. Arrow keys move
                // between tabs (roving tabindex), per the WAI-ARIA tabs pattern.
                <div
                    class="tab-list"
                    role="tablist"
                    aria-label="Results views"
                    on:keydown=move |ev| {
                        let Some(next) = util::tab_after_key(tab.get_untracked(), &ev.key()) else {
                            return;
                        };
                        ev.prevent_default();
                        tab.set(next);
                        focus_tab(next);
                    }
                >
                    <div class="tab-group" role="presentation">
                        <span class="tab-group-label" aria-hidden="true">"Filtered results"</span>
                        <TabButton this=Tab::Patches label="Patches"/>
                        <TabButton this=Tab::Failures label="Failures"/>
                    </div>
                    <span class="tab-divider" aria-hidden="true"></span>
                    <div class="tab-group" role="presentation">
                        <span class="tab-group-label" aria-hidden="true">"Fleet health"</span>
                        <TabButton this=Tab::Compliance label="Compliance"/>
                        <TabButton this=Tab::Reboot label="Needs Reboot"/>
                        <TabButton this=Tab::Trend label="Trend"/>
                    </div>
                    <span class="tab-divider" aria-hidden="true"></span>
                    <div class="tab-group" role="presentation">
                        <span class="tab-group-label" aria-hidden="true">"Activity"</span>
                        <TabButton this=Tab::Jobs label="Jobs"/>
                    </div>
                </div>
                // What the tab reflects and which filters reach it, on hover and
                // to a screen reader (`util::tab_scope`).
                <span class="result-summary" title=move || util::tab_scope(tab.get()).unwrap_or_default()>
                    {summary}
                    {move || {
                        util::tab_scope(tab.get())
                            .map(|scope| {
                                view! {
                                    <span class="result-summary__info" aria-hidden="true">" ⓘ"</span>
                                    <span class="sr-only">{format!(" {scope}")}</span>
                                }
                            })
                    }}
                </span>
            </div>
            // A dispatched action changed the fleet after these results were
            // computed, so the backlog on screen is already out of date.
            <Show when=move || state.actions.results_stale.get()>
                <div class="stale-banner" role="status">
                    <span>"An action was dispatched — these results predate it."</span>
                    <button class="link-btn" on:click=move |_| state.refresh_now()>
                        "Refresh"
                    </button>
                </div>
            </Show>
            // Persistent record of the last failed query — the announcing toast
            // auto-dismisses; this stays until the next success or a dismiss.
            <Show when=move || state.query.query_error.with(|e| e.is_some())>
                <div class="error-banner" role="alert">
                    <span>
                        {move || {
                            let e = state.query.query_error.get().unwrap_or_default();
                            if state.query.result.with(|r| r.is_some()) {
                                format!(
                                    "Last query failed — the results below are from the previous run: {e}",
                                )
                            } else {
                                format!("Last query failed: {e}")
                            }
                        }}
                    </span>
                    <button
                        class="x"
                        aria-label="Dismiss error"
                        on:click=move |_| state.query.query_error.set(None)
                    >
                        "×"
                    </button>
                </div>
            </Show>
            <div
                id=TAB_PANEL_ID
                role="tabpanel"
                tabindex="0"
                aria-labelledby=move || tab_dom_id(tab.get())
            >
                {move || match tab.get() {
                    Tab::Patches => view! { <PatchesTable/> }.into_any(),
                    Tab::Compliance => view! { <ComplianceTab/> }.into_any(),
                    Tab::Reboot => view! { <RebootTable/> }.into_any(),
                    Tab::Failures => view! { <FailuresTable/> }.into_any(),
                    Tab::Trend => view! { <TrendTab/> }.into_any(),
                    Tab::Jobs => view! {
                        <LiveJobsPanel/>
                        <JobsTable/>
                    }
                        .into_any(),
                }}
            </div>
        </section>
    }
}

/// The one tabpanel every tab controls (its content is swapped on selection).
const TAB_PANEL_ID: &str = "results-tabpanel";

/// Moves keyboard focus to a tab button after an arrow-key selection. JS-backed,
/// so it stays here; which tab to move to is `util::tab_after_key`.
fn focus_tab(tab: Tab) {
    if let Some(el) = document()
        .get_element_by_id(tab_dom_id(tab))
        .and_then(|e| e.dyn_into::<HtmlElement>().ok())
    {
        let _ = el.focus();
    }
}

/// Stable DOM ids linking each tab button to the tabpanel's `aria-labelledby`.
fn tab_dom_id(tab: Tab) -> &'static str {
    match tab {
        Tab::Patches => "tab-patches",
        Tab::Compliance => "tab-compliance",
        Tab::Reboot => "tab-reboot",
        Tab::Failures => "tab-failures",
        Tab::Trend => "tab-trend",
        Tab::Jobs => "tab-jobs",
    }
}

/// One results tab button with proper tab semantics. `aria-selected` is set as a
/// string — Leptos drops boolean-ish ARIA attributes when they're false. Only the
/// selected tab is in the Tab order (roving tabindex); the arrow keys reach the rest.
/// The counts the tab bar reports on, from the result on screen.
fn summary_counts(r: &QueryResult) -> SummaryCounts {
    SummaryCounts {
        rows_total: r.rows_total,
        devices_total: r.devices_total,
        failures: r.failures.len(),
        failing_devices: r.failing_devices,
        orgs: r.compliance.len(),
        devices_counted: r.compliance_total.devices_total,
        reboot: r.reboot_devices_total,
    }
}

#[component]
fn TabButton(this: Tab, label: &'static str) -> impl IntoView {
    let state = expect_context::<AppState>();
    let tab = state.ui.active_tab;
    // What is waiting on the tab, so the bar says where the work is without a
    // visit: rows, failures, devices to reboot, jobs still running.
    let badge = move || {
        let in_flight = state.actions.jobs.with(|j| util::jobs_in_flight(j));
        state
            .query
            .result
            .with(|r| util::tab_badge(this, r.as_ref().map(summary_counts).as_ref(), in_flight))
    };
    view! {
        <button
            id=tab_dom_id(this)
            type="button"
            role="tab"
            class=move || tab_class(tab.get(), this)
            aria-selected=move || (tab.get() == this).to_string()
            aria-controls=TAB_PANEL_ID
            tabindex=move || if tab.get() == this { "0" } else { "-1" }
            on:click=move |_| tab.set(this)
        >
            {label}
            {move || badge().map(|n| view! { <span class="tab-badge">{n}</span> })}
        </button>
    }
}

/// Prev / "Rows 1–100 of N" / Next above a paged table, rendered only when there
/// is more than one page. The arithmetic is `util`'s; the caller owns the page
/// index (already clamped) and decides what moving to a page fetches.
#[component]
fn Pager(
    page: Signal<usize>,
    page_count: Signal<usize>,
    summary: Signal<String>,
    on_page: Callback<usize>,
) -> impl IntoView {
    view! {
        <Show when=move || { page_count.get() > 1 }>
            <div class="pager">
                <button
                    class="btn"
                    prop:disabled=move || page.get() == 0
                    on:click=move |_| on_page.run(util::prev_page(page.get()))
                >
                    "‹ Prev"
                </button>
                <span class="pager-info">{move || summary.get()}</span>
                <button
                    class="btn"
                    prop:disabled=move || { page.get() + 1 >= page_count.get() }
                    on:click=move |_| on_page.run(util::next_page(page.get(), page_count.get()))
                >
                    "Next ›"
                </button>
            </div>
        </Show>
    }
}

/// A table's header row from its [`util::HelpedColumn`]s: every header that measures
/// something carries its definition as a tooltip, with a dotted underline saying so.
#[component]
fn HelpHeaderRow(columns: &'static [util::HelpedColumn]) -> impl IntoView {
    view! {
        <tr>
            {columns
                .iter()
                .map(|&(label, help)| {
                    if help.is_empty() {
                        view! { <th scope="col">{label}</th> }.into_any()
                    } else {
                        view! {
                            <th scope="col">
                                <span class="col-help" title=help>
                                    {label}
                                </span>
                            </th>
                        }
                            .into_any()
                    }
                })
                .collect_view()}
        </tr>
    }
}
