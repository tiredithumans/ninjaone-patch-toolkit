//! The Compliance tab: scope note, charts, and the per-organization and per-OS
//! rollup tables (the organization one drills down to its rows).

use super::*;

/// The scope sentence under the Compliance tab's banner. Reads the population and
/// the patch families off the displayed result, so it always describes the numbers
/// actually on screen rather than the controls' current state.
#[component]
fn ComplianceScopeNote() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <p class="scope-note">
            {move || {
                state
                    .query
                    .result
                    .with(|r| {
                        r.as_ref()
                            .map(|r| {
                                util::compliance_scope_note(
                                    r.devices_offline,
                                    r.devices_unpatchable,
                                    r.patch_families,
                                )
                            })
                            .unwrap_or_default()
                    })
            }}
        </p>
    }
}

#[component]
pub(super) fn ComplianceTab() -> impl IntoView {
    let state = expect_context::<AppState>();
    let has_result = move || state.query.result.with(|r| r.is_some());
    let org_rows: Signal<Vec<ComplianceRow>> = Signal::derive(move || {
        state.query.result.with(|r| {
            r.as_ref()
                .map(|r| r.compliance.iter().map(ComplianceRow::from).collect())
                .unwrap_or_default()
        })
    });
    let os_rows: Signal<Vec<ComplianceRow>> = Signal::derive(move || {
        state.query.result.with(|r| {
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
                reflects="the pending backlog for the selected device scope."
                // `Type` is listed because it genuinely narrows these numbers: only
                // the patch families the query fetched are in the rollups, and an
                // OS-only query cannot see a third-party backlog. Claiming it was
                // ignored here let "100% compliant" mean "no pending OS patches"
                // with nothing on screen saying so. The chip row agrees: `Type` is a
                // device-tier chip (`filter_chips`), so it is never struck through.
                filters="Device scope (Org / Location / Role / OS Type / OS name) and patch Type. Status, Severity, Search, First-seen and Installed-within are ignored here."
            />
            <ComplianceScopeNote/>
            <ComplianceCharts/>
            <ComplianceRollupTable first_col="Organization" rows=org_rows drill=RollupDrill::Organization/>
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

/// What clicking a rollup row's label should narrow to, or `None` for a rollup whose
/// grouping key isn't a filter facet.
///
/// The per-OS rollup buckets on `os.name`, and the only OS control is the free-text
/// `os_name` substring facet — close enough to look clickable but not the same thing,
/// so it stays inert rather than shipping a drill-down that quietly matches the wrong
/// devices.
#[derive(Clone, Copy, PartialEq)]
enum RollupDrill {
    Organization,
}

/// Shared table for the two compliance rollups (per-organization and per-OS):
/// identical columns, differing only in the grouping column's header and values.
#[component]
fn ComplianceRollupTable(
    first_col: &'static str,
    #[prop(into)] rows: Signal<Vec<ComplianceRow>>,
    #[prop(optional)] drill: Option<RollupDrill>,
) -> impl IntoView {
    let state = expect_context::<AppState>();
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
                            // Canonical spellings from `rows::ComplianceBucket::COLUMNS`.
                            <th scope="col">"Devices"</th>
                            <th scope="col">"Compliant"</th>
                            <th scope="col">"Compliance %"</th>
                            <th scope="col">"Pending Critical/Important"</th>
                            <th scope="col">"Aged (past SLA)"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            rows.get()
                                .into_iter()
                                .map(|b| {
                                    let pct = util::format_pct(b.compliance_pct);
                                    let (aged_class, aged_label, aged_title) = aged_badge(
                                        b.aged_critical,
                                    );
                                    let label_cell = match drill {
                                        Some(RollupDrill::Organization) => {
                                            let org = b.label.clone();
                                            let title = format!(
                                                "Show {}'s patch rows",
                                                b.label,
                                            );
                                            view! {
                                                <td>
                                                    <button
                                                        class="drill"
                                                        title=title
                                                        on:click=move |_| {
                                                            state.drill_to_org(org.clone())
                                                        }
                                                    >
                                                        {b.label.clone()}
                                                    </button>
                                                </td>
                                            }
                                                .into_any()
                                        }
                                        None => view! { <td>{b.label.clone()}</td> }.into_any(),
                                    };
                                    view! {
                                        <tr>
                                            {label_cell}
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
