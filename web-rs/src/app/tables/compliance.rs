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
            <StuckApprovals/>
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
    awaiting_approval: usize,
    approved_not_installed: usize,
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
            awaiting_approval: b.awaiting_approval,
            approved_not_installed: b.approved_not_installed,
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
            awaiting_approval: b.awaiting_approval,
            approved_not_installed: b.approved_not_installed,
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
                            // Vendor status MANUAL vs APPROVED, any severity — see
                            // `rows::approval_state` for why untyped records are in
                            // neither.
                            <th scope="col">"Awaiting Approval"</th>
                            <th scope="col">"Approved, Not Installed"</th>
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
                                            <td>{b.awaiting_approval}</td>
                                            <td>{b.approved_not_installed}</td>
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

/// The approval workflow card: fleet totals of the two approval columns, then the
/// devices whose *approved* patches have not installed past the SLA window. No
/// operator decision is holding those up, so the list points at agents rather than
/// at a backlog. Like the rest of the tab it reads the unnarrowed current feed.
#[component]
fn StuckApprovals() -> impl IntoView {
    let state = expect_context::<AppState>();
    let approvals = move || {
        state
            .query
            .result
            .with(|r| r.as_ref().map(|r| r.approvals.clone()).unwrap_or_default())
    };
    view! {
        <section class="compliance-approvals">
            <h3 class="chart-title">"Approvals"</h3>
            {move || {
                let a = approvals();
                let totals = util::approval_totals_line(
                    a.awaiting_approval,
                    a.approved_not_installed,
                );
                let caption = util::stuck_approvals_caption(
                    a.stuck_devices.len(),
                    a.stuck_devices_total,
                    a.stuck_patches,
                    a.stuck_after_days,
                );
                let body = match caption {
                    None => {
                        view! {
                            <p class="empty">
                                {format!(
                                    "No approved patch has gone uninstalled for more than {} days since first seen.",
                                    a.stuck_after_days,
                                )}
                            </p>
                        }
                            .into_any()
                    }
                    Some(caption) => {
                        view! {
                            <p class="scope-note">{caption}</p>
                            <div class="table-wrap">
                                <table>
                                    <thead>
                                        <tr>
                                            // Spelled as `rows::StuckDevice::COLUMNS`.
                                            <th scope="col">"Device"</th>
                                            <th scope="col">"Organization"</th>
                                            <th scope="col">"Approved, Not Installed"</th>
                                            <th scope="col">"Oldest First Seen"</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {a
                                            .stuck_devices
                                            .into_iter()
                                            .map(|d| {
                                                view! {
                                                    <tr>
                                                        <td>{d.device_name}</td>
                                                        <td>{d.organization}</td>
                                                        <td>{d.patches}</td>
                                                        <td>
                                                            {d
                                                                .oldest_first_seen
                                                                .unwrap_or_else(|| "(undated)".to_string())}
                                                        </td>
                                                    </tr>
                                                }
                                            })
                                            .collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        }
                            .into_any()
                    }
                };
                view! {
                    <p class="scope-note">{totals}</p>
                    {body}
                }
            }}
        </section>
    }
}
