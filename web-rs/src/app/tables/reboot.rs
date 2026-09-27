//! The Needs Reboot tab: the needs-reboot device subset, paged client-side, with a
//! device-level selection the shared `ActionBar` can reboot or scan.

use super::*;

#[component]
pub(super) fn RebootTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Paged client-side. The backend trims the list to the needs-reboot subset and
    // ships it whole in the summary, but "subset" is not "small" — on a fleet mid
    // patch cycle it can be thousands of devices, and this used to clone the entire
    // vector and materialise a <tr> for every one of them on every reactive re-run,
    // in a UI whose every other table is built around paging.
    let page = RwSignal::new(0usize);
    let total = move || {
        state
            .query
            .result
            .with(|r| r.as_ref().map_or(0, |r| r.reboot_devices.len()))
    };
    let page_count = move || util::page_count(total(), PATCHES_PAGE_SIZE);
    let current = move || util::clamp_page(page.get(), page_count());
    // Only the visible window is cloned, not the whole subset.
    let devices = move || {
        let (start, end) = util::page_bounds(current(), PATCHES_PAGE_SIZE, total());
        state.query.result.with(|r| {
            r.as_ref()
                .map(|r| r.reboot_devices[start..end].to_vec())
                .unwrap_or_default()
        })
    };
    let has_devices = move || total() > 0;
    let page_state = move || state.device_page_selection_state(&devices());

    view! {
        <Show
            when=has_devices
            fallback=|| view! { <p class="empty">"No devices flagged for reboot."</p> }
        >
            <ScopeBanner
                kind="fleet"
                tier="Fleet health"
                reflects="devices in the selected device scope flagged for reboot."
                filters="Device scope and patch Type (the pending count covers only the patch families this query fetched). Status, Severity, Search, First-seen and Installed-within are ignored here."
            />
            <Pager
                page=Signal::derive(current)
                page_count=Signal::derive(page_count)
                summary=Signal::derive(move || {
                    util::pager_summary("Devices", current(), PATCHES_PAGE_SIZE, total())
                })
                on_page=Callback::new(move |target| page.set(target))
            />
            // The same action bar as the Patches tab, reading this tab's device
            // selection: a reboot or a scan straight from the list that says which
            // devices need one.
            <Show when=move || state.action_surface_visible()>
                <ActionBar source=SelectionSource::Devices/>
            </Show>
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            // Not a `DeviceSummary::COLUMNS` column: the workbook and
                            // the HTML report have nothing to select.
                            <th scope="col" class="col-select">
                                <input
                                    type="checkbox"
                                    aria-label="Select every device on this page"
                                    prop:checked=move || page_state().0
                                    prop:indeterminate=move || page_state().1
                                    on:change=move |ev| {
                                        state
                                            .toggle_device_page(&devices(), event_target_checked(&ev))
                                    }
                                />
                            </th>
                            // Spelled as `rows::DeviceSummary::COLUMNS` spells them,
                            // so the app, the workbook and the HTML report name the
                            // same column the same way.
                            <th scope="col">"Organization"</th>
                            <th scope="col">"Location"</th>
                            <th scope="col">"Device Role"</th>
                            <th scope="col">"Device"</th>
                            <th scope="col">"OS"</th>
                            <th scope="col">"Pending Patches"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            devices()
                                .into_iter()
                                .map(|d| {
                                    let id = d.device_id;
                                    let label = format!("Select {} to reboot or scan", d.device_name);
                                    let row = d.clone();
                                    view! {
                                        <tr>
                                            <td class="col-select">
                                                <input
                                                    type="checkbox"
                                                    aria-label=label
                                                    prop:checked=move || state.is_device_selected(id)
                                                    on:change=move |ev| {
                                                        state
                                                            .toggle_device_selection(
                                                                &row,
                                                                event_target_checked(&ev),
                                                            )
                                                    }
                                                />
                                            </td>
                                            <td>{d.organization}</td>
                                            <td>{d.location.unwrap_or_default()}</td>
                                            <td>{d.device_role.unwrap_or_default()}</td>
                                            <td><DeviceLink device_id=d.device_id name=d.device_name/></td>
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
