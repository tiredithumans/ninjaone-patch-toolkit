//! The Compliance tab's per-device and install-time sections: the SLA policy the
//! result was computed under, the worst devices, the offline backlog, and first
//! seen → installed. Every sentence and number format comes from `util`.

use super::*;

/// The SLA policy behind this result's "past SLA" figures — read off the result,
/// not Settings, since the policy may have changed since the query ran — and what
/// its first-seen times are.
#[component]
pub(super) fn SlaPolicyNote() -> impl IntoView {
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
                                format!(
                                    "SLA policy: {}. Urgent Past SLA counts Critical, Important and Security patches first seen longer ago than their severity's SLA; undated patches count as past SLA.",
                                    util::sla_policy_summary(&r.sla_policy),
                                )
                            })
                            .unwrap_or_default()
                    })
            }}
        </p>
        {move || {
            state
                .query
                .result
                .with(|r| r.as_ref().and_then(|r| util::first_seen_note(&r.first_seen)))
                .map(|note| {
                    view! {
                        <p class="scope-note">
                            <strong>"First seen: "</strong>
                            {note}
                        </p>
                    }
                })
        }}
    }
}

/// Which of the two device lists a table renders.
#[derive(Clone, Copy, PartialEq)]
enum DeviceListKind {
    Worst,
    Offline,
}

/// The worst online devices and the offline backlog, one section each.
#[component]
pub(super) fn DeviceBacklogSections() -> impl IntoView {
    view! {
        <section class="compliance-os">
            <h3 class="chart-title">"Worst devices"</h3>
            <p class="scope-note">{util::WORST_DEVICES_NOTE}</p>
            <DeviceBacklogTable kind=DeviceListKind::Worst/>
        </section>
        <section class="compliance-os">
            <h3 class="chart-title">"Offline devices with a pending backlog"</h3>
            <p class="scope-note">{util::OFFLINE_BACKLOG_NOTE}</p>
            <DeviceBacklogTable kind=DeviceListKind::Offline/>
        </section>
    }
}

#[component]
fn DeviceBacklogTable(kind: DeviceListKind) -> impl IntoView {
    let state = expect_context::<AppState>();
    let list = move || {
        state.query.result.with(|r| {
            r.as_ref()
                .map(|r| match kind {
                    DeviceListKind::Worst => r.worst_devices.clone(),
                    DeviceListKind::Offline => r.offline_backlog.clone(),
                })
                .unwrap_or_default()
        })
    };
    let empty = match kind {
        DeviceListKind::Worst => "No online device in scope has a pending patch.",
        DeviceListKind::Offline => "No offline device in scope is listed with pending patches.",
    };
    // Spelled as `rows::DeviceBacklog::{WORST,OFFLINE}_COLUMNS`, which
    // `headers_match_the_backend_columns` holds them to.
    let columns: &'static [util::HelpedColumn] = match kind {
        DeviceListKind::Worst => &util::WORST_HEADERS,
        DeviceListKind::Offline => &util::OFFLINE_HEADERS,
    };
    view! {
        <Show
            when=move || !list().devices.is_empty()
            fallback=move || view! { <p class="empty">{empty}</p> }
        >
            <div class="table-wrap">
                <table>
                    <thead>
                        <HelpHeaderRow columns=columns/>
                    </thead>
                    <tbody>
                        {move || {
                            list()
                                .devices
                                .into_iter()
                                .map(|d| {
                                    let middle: Vec<String> = match kind {
                                        DeviceListKind::Worst => vec![
                                            d.past_sla.to_string(),
                                            d.pending_total.to_string(),
                                            d.oldest_first_seen.clone().unwrap_or_default(),
                                        ],
                                        DeviceListKind::Offline => vec![
                                            d.latest_collected.clone().unwrap_or_default(),
                                            d.last_contact.clone().unwrap_or_default(),
                                            d.pending_total.to_string(),
                                            d.past_sla.to_string(),
                                        ],
                                    };
                                    view! {
                                        <tr>
                                            <td>{d.organization.clone()}</td>
                                            <td>{d.device_name.clone()}</td>
                                            <td>{d.os_name.clone().unwrap_or_default()}</td>
                                            {middle
                                                .into_iter()
                                                .map(|cell| view! { <td>{cell}</td> })
                                                .collect_view()}
                                            <td>
                                                <SeverityBreakdownCell counts=d.pending/>
                                            </td>
                                        </tr>
                                    }
                                })
                                .collect_view()
                        }}
                    </tbody>
                </table>
            </div>
            {move || {
                util::device_list_caption(&list()).map(|c| view! { <p class="scope-note">{c}</p> })
            }}
        </Show>
    }
}

/// A device's pending backlog inline: one swatch and count per non-zero band.
#[component]
fn SeverityBreakdownCell(counts: SeverityCounts) -> impl IntoView {
    view! {
        <ul class="chart-legend breakdown">
            {charts::severity_breakdown(&counts)
                .into_iter()
                .map(|(label, class, n)| {
                    let swatch = format!("chart-swatch {class}");
                    view! {
                        <li>
                            <span class=swatch aria-hidden="true"></span>
                            {format!("{label} {n}")}
                        </li>
                    }
                })
                .collect_view()}
        </ul>
    }
}

/// First seen → installed: overall, by organization and by severity.
#[component]
pub(super) fn TimeToInstallSection() -> impl IntoView {
    let state = expect_context::<AppState>();
    let tti = move || {
        state.query.result.with(|r| {
            r.as_ref()
                .map(|r| r.time_to_install.clone())
                .unwrap_or_default()
        })
    };
    view! {
        <section class="compliance-os">
            <h3 class="chart-title">"First seen \u{2192} installed"</h3>
            // The one section on this tab built from the detail rows, so it is the
            // one the patch filters *do* reach — the banner above says otherwise
            // for everything else here.
            <p class="scope-note">
                "Unlike the sections above, this follows every patch filter (Status, Severity, Search, First-seen, Installed-within): it measures the installed rows this query returned."
            </p>
            {move || {
                let t = tti();
                if let Some(reason) = util::time_to_install_empty_reason(&t) {
                    return view! { <p class="empty">{reason}</p> }.into_any();
                }
                let rows: Vec<(&'static str, InstallLatency)> = t
                    .overall
                    .clone()
                    .map(|o| ("Overall", o))
                    .into_iter()
                    .chain(t.by_organization.iter().cloned().map(|l| ("Organization", l)))
                    .chain(t.by_severity.iter().cloned().map(|l| ("Severity", l)))
                    .collect();
                view! {
                    <div class="table-wrap">
                        <table>
                            // Spelled as `rows::InstallLatency::COLUMNS`, less the
                            // "(Days)" the cells here carry themselves.
                            <thead>
                                <HelpHeaderRow columns=&util::TIME_TO_INSTALL_HEADERS/>
                            </thead>
                            <tbody>
                                {rows
                                    .into_iter()
                                    .map(|(group, l)| {
                                        view! {
                                            <tr>
                                                <td>{group}</td>
                                                <td>{l.label}</td>
                                                <td>{l.samples}</td>
                                                <td>{util::format_days(l.median_days)}</td>
                                                <td>{util::format_days(l.p90_days)}</td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()}
                            </tbody>
                        </table>
                    </div>
                    <p class="scope-note">{util::time_to_install_sample_note(&t)}</p>
                    <p class="scope-note">{util::TIME_TO_INSTALL_NOTE}</p>
                }
                    .into_any()
            }}
        </section>
    }
}
