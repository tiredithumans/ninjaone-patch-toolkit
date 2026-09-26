//! The results panel: the tab bar, the stale/error banners, the applied-filter
//! chips and the shared scope banner. Each tab's view lives in its own submodule.

use leptos::web_sys::HtmlElement;
use wasm_bindgen::JsCast;

use super::*;

mod compliance;
mod failures;
mod patches;
mod reboot;
mod trend;

use compliance::ComplianceTab;
use failures::FailuresTable;
use patches::PatchesTable;
use reboot::RebootTable;
use trend::TrendTab;

#[component]
pub(crate) fn Results() -> impl IntoView {
    let state = expect_context::<AppState>();
    let tab = state.ui.active_tab;

    let summary = move || {
        // Read the active tab so the line re-renders (and re-describes) on switch.
        let tab = tab.get();
        state.query.result.with(|r| {
            r.as_ref().map(|r| {
                let c = SummaryCounts {
                    rows_total: r.rows_total,
                    devices_total: r.devices_total,
                    failures: r.failures.len(),
                    failing_devices: util::failing_device_count(&r.failures),
                    orgs: r.compliance.len(),
                    reboot: r.reboot_devices.len(),
                };
                summary_line(tab, &c, &r.generated_at)
            })
        })
    };

    view! {
        <section class="panel results">
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
                <span class="result-summary">{summary}</span>
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
            <AppliedFilterChips/>
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
                    Tab::Jobs => view! { <JobsTable/> }.into_any(),
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
#[component]
fn TabButton(this: Tab, label: &'static str) -> impl IntoView {
    let state = expect_context::<AppState>();
    let tab = state.ui.active_tab;
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
        </button>
    }
}

/// Read-only chips describing the filters that produced the current result (snapshot
/// taken at Run time). Patch-tier chips grey out + strike through on Fleet-health tabs,
/// where those filters are ignored — making the silent scope change explicit.
#[component]
fn AppliedFilterChips() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        // Hidden on Jobs: dispatch history isn't produced by the query, so chips
        // describing that query would claim a scope the tab doesn't have.
        <Show when=move || {
            state.query.applied_filters.with(|a| a.is_some())
                && !matches!(state.ui.active_tab.get(), Tab::Jobs | Tab::Trend)
        }>
            <div
                class="applied-filters"
                role="group"
                aria-label="Filters applied to the current results"
            >
                {move || {
                    let chips = state.query.applied_filters
                        .with(|a| a.as_ref().map(filter_chips).unwrap_or_default());
                    let fleet = is_fleet_tab(state.ui.active_tab.get());
                    if chips.is_empty() {
                        return view! {
                            <span class="applied-chip applied-chip-none">
                                "No filters — whole fleet"
                            </span>
                        }
                            .into_any();
                    }
                    chips
                        .into_iter()
                        .map(|c| {
                            let dim = c.patch && fleet;
                            let cls = if dim {
                                "applied-chip applied-chip-dim"
                            } else {
                                "applied-chip"
                            };
                            let title = if dim { "Ignored on this tab" } else { "" };
                            view! { <span class=cls title=title>{c.label}</span> }
                        })
                        .collect_view()
                        .into_any()
                }}
            </div>
        </Show>
    }
}

/// The contract banner shown at the top of every results tab: which tier it belongs
/// to, what it reflects, and exactly which filters apply or are ignored. `kind` picks
/// the accent ("filtered" = patch-filtered tier, "fleet" = device-scope-only tier).
#[component]
fn ScopeBanner(
    kind: &'static str,
    tier: &'static str,
    reflects: &'static str,
    filters: &'static str,
) -> impl IntoView {
    view! {
        <div class=format!("scope-banner scope-banner-{kind}")>
            <span class="scope-banner-tier">{tier}</span>
            <p class="scope-banner-text">
                <strong>"Showing "</strong>
                {reflects}
            </p>
            <p class="scope-banner-filters">{filters}</p>
        </div>
    }
}
