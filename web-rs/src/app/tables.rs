use leptos::prelude::*;

use super::*;

#[component]
pub(crate) fn Results() -> impl IntoView {
    let state = expect_context::<AppState>();
    let tab = state.active_tab;

    let summary = move || {
        // Read the active tab so the line re-renders (and re-describes) on switch.
        let tab = tab.get();
        state.result.with(|r| {
            r.as_ref().map(|r| {
                let c = SummaryCounts {
                    rows_total: r.rows_total,
                    devices_total: r.devices_total,
                    failures: r.failures.len(),
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
                <div class="tab-group">
                    <span class="tab-group-label">"Filtered results"</span>
                    <button
                        class=move || tab_class(tab.get(), Tab::Patches)
                        on:click=move |_| tab.set(Tab::Patches)
                    >
                        "Patches"
                    </button>
                    <button
                        class=move || tab_class(tab.get(), Tab::Failures)
                        on:click=move |_| tab.set(Tab::Failures)
                    >
                        "Failures"
                    </button>
                </div>
                <span class="tab-divider" aria-hidden="true"></span>
                <div class="tab-group">
                    <span class="tab-group-label">"Fleet health"</span>
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
                </div>
                <span class="result-summary">{summary}</span>
            </div>
            <AppliedFilterChips/>
            {move || match tab.get() {
                Tab::Patches => view! { <PatchesTable/> }.into_any(),
                Tab::Compliance => view! { <ComplianceTab/> }.into_any(),
                Tab::Reboot => view! { <RebootTable/> }.into_any(),
                Tab::Failures => view! { <FailuresTable/> }.into_any(),
            }}
        </section>
    }
}

/// Read-only chips describing the filters that produced the current result (snapshot
/// taken at Run time). Patch-tier chips grey out + strike through on Fleet-health tabs,
/// where those filters are ignored — making the silent scope change explicit.
#[component]
fn AppliedFilterChips() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <Show when=move || state.applied_filters.with(|a| a.is_some())>
            <div
                class="applied-filters"
                role="group"
                aria-label="Filters applied to the current results"
            >
                {move || {
                    let chips = state
                        .applied_filters
                        .with(|a| a.as_ref().map(filter_chips).unwrap_or_default());
                    let fleet = is_fleet_tab(state.active_tab.get());
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

#[component]
fn PatchesTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    // The total row count comes from the summary; the visible page lives in
    // `page_rows`, fetched from the backend cache rather than held in full here.
    let total = move || {
        state
            .result
            .with(|r| r.as_ref().map_or(0, |r| r.rows_total))
    };
    let page_count = move || total().div_ceil(PATCHES_PAGE_SIZE).max(1);
    // Clamp the stored page so a shorter result (e.g. after an auto-refresh) can't
    // leave us past the last page.
    let page = move || state.patches_page.get().min(page_count() - 1);
    let rows = move || state.page_rows.get();
    let pager_summary = move || {
        let t = total();
        let start = page() * PATCHES_PAGE_SIZE;
        let end = (start + PATCHES_PAGE_SIZE).min(t);
        format!(
            "Rows {}\u{2013}{} of {} \u{00b7} Page {} of {}",
            start + 1,
            end,
            group_thousands(t),
            page() + 1,
            page_count(),
        )
    };
    // Page navigation updates the index and fetches that page's rows on demand.
    let go_to = move |target: usize| {
        state.patches_page.set(target);
        state.fetch_page(target);
    };
    let go_prev = move |_| go_to(page().saturating_sub(1));
    let go_next = move |_| {
        let last = page_count().saturating_sub(1);
        go_to((page().min(last) + 1).min(last));
    };

    view! {
        <Show
            when=move || state.result.with(|r| r.is_some())
            fallback=|| view! { <p class="empty">"Run a query to list patches."</p> }
        >
            <ScopeBanner
                kind="filtered"
                tier="Filtered results"
                reflects="every patch matching your device scope and all patch filters."
                filters="Device scope + Type, Status, Severity, Search, Released and Installed-within are all applied."
            />
            <Show
                when=move || { total() > 0 }
                fallback=|| {
                    view! {
                        <p class="empty">
                            "No patches matched your filters. Try widening the organization, severity, or status selection."
                        </p>
                    }
                }
            >
                <Show when=move || { page_count() > 1 }>
                    <div class="pager">
                        <button
                            class="btn"
                            prop:disabled=move || page() == 0
                            on:click=go_prev
                        >
                            "‹ Prev"
                        </button>
                        <span class="pager-info">{pager_summary}</span>
                        <button
                            class="btn"
                            prop:disabled=move || { page() + 1 >= page_count() }
                            on:click=go_next
                        >
                            "Next ›"
                        </button>
                    </div>
                </Show>
                <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Organization"</th>
                            <th scope="col">"Location"</th>
                            <th scope="col">"Role"</th>
                            <th scope="col">"Device"</th>
                            <th scope="col">"OS"</th>
                            <th scope="col">"Type"</th>
                            <th scope="col">"KB"</th>
                            <th scope="col">"Patch"</th>
                            <th scope="col">"Severity"</th>
                            <th scope="col">"Status"</th>
                            <th scope="col">"Release"</th>
                            <th scope="col">"Installed"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            rows()
                                .into_iter()
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
        </Show>
    }
}

#[component]
fn ComplianceTab() -> impl IntoView {
    let state = expect_context::<AppState>();
    let has_result = move || state.result.with(|r| r.is_some());
    let org_rows: Signal<Vec<ComplianceRow>> = Signal::derive(move || {
        state.result.with(|r| {
            r.as_ref()
                .map(|r| r.compliance.iter().map(ComplianceRow::from).collect())
                .unwrap_or_default()
        })
    });
    let os_rows: Signal<Vec<ComplianceRow>> = Signal::derive(move || {
        state.result.with(|r| {
            r.as_ref()
                .map(|r| r.compliance_by_os.iter().map(ComplianceRow::from).collect())
                .unwrap_or_default()
        })
    });
    view! {
        <Show
            when=has_result
            fallback=|| view! { <p class="empty">"Run a query to see compliance."</p> }
        >
            <ScopeBanner
                kind="fleet"
                tier="Fleet health"
                reflects="the whole pending backlog for the selected device scope."
                filters="Device scope only (Org / Location / Role / OS Type / OS name). Status, Severity, Search, Released and Installed-within are ignored here."
            />
            <ComplianceCharts/>
            <ComplianceRollupTable first_col="Organization" rows=org_rows/>
            <section class="compliance-os">
                <h3 class="chart-title">"Compliance by OS"</h3>
                <div class="chart-card">
                    <ComplianceByOsBars/>
                </div>
                <ComplianceRollupTable first_col="OS" rows=os_rows/>
            </section>
        </Show>
    }
}

/// One row of a compliance rollup table, independent of the grouping key. The two
/// bucket types stay distinct hand-maintained IPC mirrors (`types.rs`); they
/// converge here only for rendering.
#[derive(Clone)]
struct ComplianceRow {
    label: String,
    devices_total: usize,
    devices_compliant: usize,
    compliance_pct: f64,
    pending_critical: usize,
    aged_critical: usize,
}

impl From<&ComplianceBucket> for ComplianceRow {
    fn from(b: &ComplianceBucket) -> Self {
        Self {
            label: b.organization.clone(),
            devices_total: b.devices_total,
            devices_compliant: b.devices_compliant,
            compliance_pct: b.compliance_pct,
            pending_critical: b.pending_critical,
            aged_critical: b.aged_critical,
        }
    }
}

impl From<&OsCompliance> for ComplianceRow {
    fn from(b: &OsCompliance) -> Self {
        Self {
            label: b.os.clone(),
            devices_total: b.devices_total,
            devices_compliant: b.devices_compliant,
            compliance_pct: b.compliance_pct,
            pending_critical: b.pending_critical,
            aged_critical: b.aged_critical,
        }
    }
}

/// Shared table for the two compliance rollups (per-organization and per-OS):
/// identical columns, differing only in the grouping column's header and values.
#[component]
fn ComplianceRollupTable(
    first_col: &'static str,
    #[prop(into)] rows: Signal<Vec<ComplianceRow>>,
) -> impl IntoView {
    view! {
        <Show
            when=move || rows.with(|r| !r.is_empty())
            fallback=|| view! { <p class="empty">"No compliance data yet."</p> }
        >
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">{first_col}</th>
                            <th scope="col">"Devices"</th>
                            <th scope="col">"Compliant"</th>
                            <th scope="col">"Compliance"</th>
                            <th scope="col">"Pending Critical/Important Patches"</th>
                            <th scope="col">"Aged (past SLA)"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            rows.get()
                                .into_iter()
                                .map(|b| {
                                    let pct = format!("{:.0}%", b.compliance_pct);
                                    let (aged_class, aged_label, aged_title) = aged_badge(
                                        b.aged_critical,
                                    );
                                    view! {
                                        <tr>
                                            <td>{b.label}</td>
                                            <td>{b.devices_total}</td>
                                            <td>{b.devices_compliant}</td>
                                            <td>{pct}</td>
                                            <td>{b.pending_critical}</td>
                                            <td>
                                                <span class=aged_class title=aged_title>
                                                    {aged_label}
                                                </span>
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
fn FailuresTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Backend ships the failure rollup whole (one entry per failing patch) in the
    // summary, already sorted by affected-device count — render it as-is.
    let failures = move || {
        state
            .result
            .with(|r| r.as_ref().map(|r| r.failures.clone()).unwrap_or_default())
    };
    let has_failures = move || {
        state
            .result
            .with(|r| r.as_ref().is_some_and(|r| !r.failures.is_empty()))
    };

    view! {
        <Show
            when=has_failures
            fallback=|| {
                view! {
                    <p class="empty">
                        "No patch failures. Select the FAILED status and Run query to analyze failures."
                    </p>
                }
            }
        >
            <ScopeBanner
                kind="filtered"
                tier="Filtered results"
                reflects="failed installs matching your device scope and all patch filters."
                filters="Restricted to Status = FAILED — select FAILED and Run query to populate this tab."
            />
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Severity"</th>
                            <th scope="col">"KB"</th>
                            <th scope="col">"Patch"</th>
                            <th scope="col">"Affected devices"</th>
                            <th scope="col">"Latest failure"</th>
                            <th scope="col">"Devices"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            failures()
                                .into_iter()
                                .map(|f| {
                                    let sev = sev_class(&f.severity);
                                    view! {
                                        <tr>
                                            <td>
                                                <span class=sev>{f.severity}</span>
                                            </td>
                                            <td>{f.kb.unwrap_or_default()}</td>
                                            <td class="patch-name">{f.name}</td>
                                            <td>{f.affected_devices}</td>
                                            <td>{f.latest_failure.unwrap_or_default()}</td>
                                            <td class="device-list">{f.device_names.join(", ")}</td>
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
    // The backend already trimmed the device list to the needs-reboot subset, so
    // clone that directly; the emptiness check clones nothing.
    let devices = move || {
        state.result.with(|r| {
            r.as_ref()
                .map(|r| r.reboot_devices.clone())
                .unwrap_or_default()
        })
    };
    let has_devices = move || {
        state
            .result
            .with(|r| r.as_ref().is_some_and(|r| !r.reboot_devices.is_empty()))
    };

    view! {
        <Show
            when=has_devices
            fallback=|| view! { <p class="empty">"No devices flagged for reboot."</p> }
        >
            <ScopeBanner
                kind="fleet"
                tier="Fleet health"
                reflects="devices in the selected device scope flagged for reboot."
                filters="Device scope only. Status, Severity, Search, Released and Installed-within are ignored here."
            />
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Organization"</th>
                            <th scope="col">"Location"</th>
                            <th scope="col">"Role"</th>
                            <th scope="col">"Device"</th>
                            <th scope="col">"OS"</th>
                            <th scope="col">"Pending patches"</th>
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
