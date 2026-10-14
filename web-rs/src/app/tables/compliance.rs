//! The Compliance tab: the headline figures, what they mean, the charts, and the
//! per-organization and per-OS rollup tables (the organization one drills down to
//! its rows).

use super::backlog::{DeviceBacklogSections, SlaPolicyNote, TimeToInstallSection};
use super::*;

/// The four headline figures as tiles, each with its "n of N" and a one-line
/// definition. Read off the displayed result, like everything else on the tab.
#[component]
fn ComplianceKpis() -> impl IntoView {
    let state = expect_context::<AppState>();
    let kpis = move || {
        state.query.result.with(|r| {
            r.as_ref().map(|r| {
                (
                    util::compliance_kpis(
                        &r.compliance_total,
                        r.devices_offline,
                        r.devices_unpatchable,
                    ),
                    r.compliance_total.clone(),
                )
            })
        })
    };
    view! {
        {move || {
            kpis()
                .map(|(kpis, total)| {
                    let tiles = kpis
                        .into_iter()
                        .enumerate()
                        .map(|(i, k)| {
                            // Within SLA takes its band's colour; the urgent tile turns
                            // red once anything is past SLA.
                            let tone = match i {
                                0 if total.devices_total > 0 => {
                                    util::pct_band_class(total.within_sla_pct)
                                }
                                2 if total.urgent_past_sla > 0 => "kpi-alert",
                                _ => "",
                            };
                            view! {
                                <div class="kpi">
                                    <dt>{k.label}</dt>
                                    <dd class=format!("kpi-value {tone}")>{k.value}</dd>
                                    <dd class="kpi-detail">{k.detail}</dd>
                                    <dd class="kpi-def">{k.definition}</dd>
                                </div>
                            }
                        })
                        .collect_view();
                    view! { <dl class="kpis">{tiles}</dl> }
                })
        }}
    }
}

/// "How these numbers are calculated": every term the tab uses, defined from the
/// displayed result's own policy, families and first-seen record. Collapsed by
/// default; only its body is reactive, so an auto-refresh does not close it.
#[component]
fn CalculationNotes() -> impl IntoView {
    let state = expect_context::<AppState>();
    let notes = move || {
        state.query.result.with(|r| {
            r.as_ref()
                .map(|r| util::calculation_notes(&r.sla_policy, r.patch_families, &r.first_seen))
                .unwrap_or_default()
        })
    };
    view! {
        <details class="calc-notes">
            <summary>"How these numbers are calculated"</summary>
            <dl>
                {move || {
                    notes()
                        .into_iter()
                        .map(|(term, meaning)| {
                            view! {
                                <dt>{term}</dt>
                                <dd>{meaning}</dd>
                            }
                        })
                        .collect_view()
                }}
            </dl>
        </details>
    }
}

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
    // The device-weighted fleet total, under the organization table only — the OS
    // table's total would be the same numbers again.
    let org_total: Signal<Option<ComplianceRow>> = Signal::derive(move || {
        state
            .query
            .result
            .with(|r| r.as_ref().map(|r| ComplianceRow::from(&r.compliance_total)))
    });
    let uncounted = move || {
        state.query.result.with(|r| {
            r.as_ref()
                .and_then(|r| util::uncounted_orgs_note(&r.uncounted_orgs))
        })
    };
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
            <ComplianceKpis/>
            <ComplianceScopeNote/>
            <SlaPolicyNote/>
            <CalculationNotes/>
            <ComplianceCharts/>
            <ComplianceRollupTable
                columns=&util::COMPLIANCE_ORG_HEADERS
                rows=org_rows
                total=org_total
                drill=RollupDrill::Organization
            />
            {move || uncounted().map(|note| view! { <p class="scope-note">{note}</p> })}
            <StuckApprovals/>
            <section class="compliance-os">
                <h3 class="chart-title">"Compliance by OS"</h3>
                <div class="chart-card">
                    <ComplianceByOsBars/>
                </div>
                <ComplianceRollupTable columns=&util::COMPLIANCE_OS_HEADERS rows=os_rows/>
            </section>
            <DeviceBacklogSections/>
            <TimeToInstallSection/>
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
    devices_within_sla: usize,
    within_sla_pct: f64,
    devices_fully_patched: usize,
    fully_patched_pct: f64,
    urgent_pending: usize,
    urgent_past_sla: usize,
    awaiting_approval: usize,
    approved_not_installed: usize,
}

impl From<&ComplianceBucket> for ComplianceRow {
    fn from(b: &ComplianceBucket) -> Self {
        Self {
            label: b.organization.clone(),
            devices_total: b.devices_total,
            devices_within_sla: b.devices_within_sla,
            within_sla_pct: b.within_sla_pct,
            devices_fully_patched: b.devices_fully_patched,
            fully_patched_pct: b.fully_patched_pct,
            urgent_pending: b.urgent_pending,
            urgent_past_sla: b.urgent_past_sla,
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
            devices_within_sla: b.devices_within_sla,
            within_sla_pct: b.within_sla_pct,
            devices_fully_patched: b.devices_fully_patched,
            fully_patched_pct: b.fully_patched_pct,
            urgent_pending: b.urgent_pending,
            urgent_past_sla: b.urgent_past_sla,
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
    /// The table's headers and their definitions; the first names the grouping.
    columns: &'static [util::HelpedColumn],
    #[prop(into)] rows: Signal<Vec<ComplianceRow>>,
    /// The fleet total, drawn as a footer row once there are two rows to total.
    #[prop(optional, into)]
    total: Option<Signal<Option<ComplianceRow>>>,
    #[prop(optional)] drill: Option<RollupDrill>,
) -> impl IntoView {
    let state = expect_context::<AppState>();
    let footer = move || {
        let total = total?.get()?;
        (rows.with(Vec::len) >= 2).then(|| {
            view! {
                <tfoot>
                    <tr class="total-row">
                        <th scope="row">
                            <span
                                class="col-help"
                                title="Every counted device once, so its percentages are not the average of the rows above."
                            >
                                {total.label.clone()}
                            </span>
                        </th>
                        <ComplianceCells row=total/>
                    </tr>
                </tfoot>
            }
        })
    };
    view! {
        <Show
            when=move || rows.with(|r| !r.is_empty())
            fallback=|| view! { <p class="empty">"No online, patchable device in scope."</p> }
        >
            <div class="table-wrap">
                <table>
                    // Spelled as `rows::{ComplianceBucket,OsCompliance}::COLUMNS`, which
                    // `headers_match_the_backend_columns` holds them to.
                    <thead>
                        <HelpHeaderRow columns=columns/>
                    </thead>
                    <tbody>
                        {move || {
                            rows.get()
                                .into_iter()
                                .map(|b| {
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
                                            <ComplianceCells row=b/>
                                        </tr>
                                    }
                                })
                                .collect_view()
                        }}
                    </tbody>
                    {footer}
                </table>
            </div>
        </Show>
    }
}

/// Every cell of a compliance row after its label, shared by the body rows and the
/// fleet-total footer so the two cannot render a figure differently.
#[component]
fn ComplianceCells(row: ComplianceRow) -> impl IntoView {
    let (aged_class, aged_label, aged_title) = aged_badge(row.urgent_past_sla);
    view! {
        <td>{row.devices_total}</td>
        <td>{row.devices_within_sla}</td>
        <td>{util::format_pct(row.within_sla_pct)}</td>
        <td>{row.devices_fully_patched}</td>
        <td>{util::format_pct(row.fully_patched_pct)}</td>
        <td>{row.urgent_pending}</td>
        <td>
            <span class=aged_class title=aged_title>
                {aged_label}
            </span>
        </td>
        <td>{row.awaiting_approval}</td>
        <td>{row.approved_not_installed}</td>
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
                                        <HelpHeaderRow columns=&util::STUCK_HEADERS/>
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
