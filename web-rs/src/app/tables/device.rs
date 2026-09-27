//! The device drill-down: a read-only dialog over one device's facts, its share of
//! every fleet-health number, and its detail rows. Opened from a device name on
//! the Patches and Needs Reboot tabs; served by `device_detail` from the backend's
//! cached result (or by `demo::device_detail`). No action buttons — dispatch stays
//! on the one `ActionBar`.

use super::super::charts::SEV_BANDS;
use super::*;

/// A device name that opens its drill-down. A row with no device (the orphan
/// sentinel) renders as plain text: there is nothing to open.
#[component]
pub(super) fn DeviceLink(device_id: i64, name: String) -> impl IntoView {
    let state = expect_context::<AppState>();
    if device_id == util::ORPHAN_DEVICE_ID {
        return view! { <span>{name}</span> }.into_any();
    }
    let title = format!("Show {name}'s details");
    let label = name.clone();
    view! {
        <button
            class="drill"
            title=title
            on:click=move |_| state.open_device(device_id, name.clone())
        >
            {label}
        </button>
    }
    .into_any()
}

#[component]
pub(crate) fn DeviceDetailModal() -> impl IntoView {
    let state = expect_context::<AppState>();
    window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" && state.query.drill.with_untracked(|d| d.is_some()) {
            state.close_device();
        }
    });
    // Keyed on the device alone, so the dialog (and its focus trap) is created once
    // per opened device: the load landing, or a refresh reloading it, re-renders the
    // body without re-creating the dialog — a re-created trap would take the
    // outgoing dialog as its opener and return focus to a detached node on close.
    let open = Memo::new(move |_| {
        state
            .query
            .drill
            .with(|d| d.as_ref().map(|d| (d.device_id, d.name.clone())))
    });
    view! {
        {move || {
            let (_, name) = open.get()?;
            let (dialog, on_tab) = modal::focus_trap();
            // Per dialog, so a new device opens in the canonical order.
            let sort = RwSignal::new(None::<RowSort>);
            Some(view! {
                <div class="modal-overlay">
                    <div
                        class="modal modal-wide device-drill"
                        role="dialog"
                        aria-modal="true"
                        aria-labelledby="device-drill-title"
                        tabindex="-1"
                        node_ref=dialog
                        on:keydown=move |ev| on_tab(&ev)
                    >
                        <div class="device-drill-head">
                            <h2 id="device-drill-title">{name}</h2>
                            <button
                                class="x"
                                aria-label="Close device details"
                                on:click=move |_| state.close_device()
                            >
                                "×"
                            </button>
                        </div>
                        {move || {
                            let load = state.query.drill.with(|d| d.as_ref().map(|d| d.load.clone()));
                            match load {
                                None | Some(DrillLoad::Loading) => {
                                    view! { <p class="empty">"Loading…"</p> }.into_any()
                                }
                                Some(DrillLoad::Gone) => {
                                    view! {
                                        <p class="empty">
                                            "This device is not in the current results. Run the query again to see it."
                                        </p>
                                    }
                                        .into_any()
                                }
                                Some(DrillLoad::Failed(e)) => {
                                    view! { <p class="modal-error" role="alert">{e}</p> }.into_any()
                                }
                                Some(DrillLoad::Ready(detail)) => {
                                    view! { <DrillBody detail=detail sort=sort/> }.into_any()
                                }
                            }
                        }}
                    </div>
                </div>
            })
        }}
    }
}

/// Facts, rollup and rows for a loaded device.
#[component]
fn DrillBody(detail: DeviceDetail, sort: RwSignal<Option<RowSort>>) -> impl IntoView {
    let summary = detail.device.map(|d| view! { <DeviceHealth device=d/> });
    let note = util::device_rows_note(detail.rows.len(), detail.rows_total);
    let rows = detail.rows;
    let sorted = move || {
        let mut rows = rows.clone();
        if let Some(s) = sort.get() {
            util::sort_patch_rows(&mut rows, s);
        }
        rows
    };
    view! {
        {summary}
        <h3 class="chart-title">"Patch rows"</h3>
        <p class="scope-note">
            "The device's rows on the Patches tab — every patch filter applies. The counts above ignore Status, Severity, Search and the date filters."
        </p>
        {note.map(|n| view! { <p class="scope-note">{n}</p> })}
        <Show
            when={
                let sorted = sorted.clone();
                move || !sorted().is_empty()
            }
            fallback=|| view! { <p class="empty">"No patch rows match the current filters."</p> }
        >
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"KB"</th>
                            <th scope="col">"Patch"</th>
                            <th scope="col">"Type"</th>
                            // The two orderings an operator asks of one device's
                            // patches — most urgent, longest known — are sortable;
                            // the rest are for reading.
                            <SortHeader label="Severity" key=RowSortKey::Severity sort=sort/>
                            <th scope="col">"Status"</th>
                            <SortHeader label="First seen" key=RowSortKey::FirstSeenDate sort=sort/>
                            <th scope="col">"Installed"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {
                            let sorted = sorted.clone();
                            move || {
                                sorted()
                                    .into_iter()
                                    .map(|r| {
                                        let sev = sev_class(&r.severity);
                                        let stat = status_class(&r.status);
                                        view! {
                                            <tr>
                                                <td>{r.kb.unwrap_or_default()}</td>
                                                <td class="patch-name">{r.name}</td>
                                                <td>{r.patch_type}</td>
                                                <td><span class=sev>{r.severity}</span></td>
                                                <td><span class=stat>{r.status}</span></td>
                                                <td>{r.first_seen_date.unwrap_or_default()}</td>
                                                <td>{r.installed_date.unwrap_or_default()}</td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()
                            }
                        }
                    </tbody>
                </table>
            </div>
        </Show>
    }
}

/// A sortable column header in the drill-down's rows table, cycling like the
/// Patches table's headers but over the dialog's own sort.
#[component]
fn SortHeader(
    label: &'static str,
    key: RowSortKey,
    sort: RwSignal<Option<RowSort>>,
) -> impl IntoView {
    view! {
        <th scope="col" aria-sort=move || aria_sort(sort.get(), key)>
            <button class="th-sort" on:click=move |_| sort.update(|s| *s = next_sort(*s, key))>
                {label}
                <span aria-hidden="true">{move || sort_glyph(sort.get(), key)}</span>
            </button>
        </th>
    }
}

/// The device's facts and its share of the fleet-health numbers. The counts are
/// shown only for a device inside the rollup population; for an excluded one they
/// would read as "clean" when they mean "unknown", so the reason is shown instead.
#[component]
fn DeviceHealth(device: DeviceSummary) -> impl IntoView {
    let facts = util::device_facts(&device)
        .into_iter()
        .map(|(label, value)| {
            view! {
                <div class="device-fact">
                    <dt>{label}</dt>
                    <dd>{value}</dd>
                </div>
            }
        })
        .collect_view();
    let excluded = util::rollup_scope_note(device.rollup_scope);
    let counts = device.pending_by_severity;
    let bands = SEV_BANDS
        .iter()
        .filter(|(_, _, read)| read(&counts) > 0)
        .map(|(label, _, read)| {
            view! { <span class=sev_class(label)>{format!("{label} {}", read(&counts))}</span> }
        })
        .collect_view();
    let (aged_class, aged_label, aged_title) = aged_badge(device.aged_critical);
    let failed = util::failed_installs_label(device.failed_installs);
    view! {
        <dl class="device-facts">{facts}</dl>
        {match excluded {
            Some(reason) => view! { <p class="scope-note">{reason}</p> }.into_any(),
            None => {
                view! {
                    <h3 class="chart-title">"Pending patches by severity"</h3>
                    <div class="device-bands">
                        {if device.pending_count == 0 {
                            view! { <span class="chips-label">"None pending"</span> }.into_any()
                        } else {
                            bands.into_any()
                        }}
                    </div>
                    <dl class="device-facts">
                        <div class="device-fact">
                            <dt>"Pending Patches"</dt>
                            <dd>{device.pending_count}</dd>
                        </div>
                        <div class="device-fact">
                            <dt>"Aged (past SLA)"</dt>
                            <dd>
                                <span class=aged_class title=aged_title>{aged_label}</span>
                            </dd>
                        </div>
                        <div class="device-fact">
                            <dt>"Failed installs"</dt>
                            <dd>{failed}</dd>
                        </div>
                    </dl>
                }
                    .into_any()
            }
        }}
    }
}
