//! The Needs Reboot tab: the needs-reboot device subset, paged, with a device-level
//! selection the shared `ActionBar` can reboot or scan.

use super::*;

#[component]
pub(super) fn RebootTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Paged. "Subset" is not "small" — on a fleet mid patch cycle it can be thousands
    // of devices — so the summary carries only the first page, and later pages are
    // read from the backend's cached result (`util::reboot_page_source`). A summary
    // that carries the whole list (demo mode, a small fleet) is paged locally.
    let page = RwSignal::new(0usize);
    let total = move || {
        state
            .query
            .result
            .with(|r| r.as_ref().map_or(0, |r| r.reboot_devices_total))
    };
    let seeded = move || {
        state
            .query
            .result
            .with(|r| r.as_ref().map_or(0, |r| r.reboot_devices.len()))
    };
    let page_count = move || util::page_count(total(), PATCHES_PAGE_SIZE);
    let current = move || util::clamp_page(page.get(), page_count());
    let source = move || util::reboot_page_source(current(), PATCHES_PAGE_SIZE, seeded(), total());
    // The page last fetched, by index; `stamp` drops a reply that a newer page or a
    // newer result overtook, as the Patches pager's view stamp does.
    let fetched = RwSignal::new(None::<(usize, Vec<DeviceSummary>)>);
    let stamp = RwSignal::new(0u64);
    Effect::new(move |_| {
        // Re-runs when the result or the page changes; either retires the last page.
        let util::RebootPage::Fetch { offset, limit } = source() else {
            return;
        };
        let at = current();
        let mine = util::next_query_seq(stamp.get_untracked());
        stamp.set(mine);
        fetched.set(None);
        spawn_local(async move {
            let outcome = api::get_reboot_devices(offset, limit).await;
            if util::is_superseded(stamp.get_untracked(), mine) {
                return;
            }
            match outcome {
                Ok(devices) => fetched.set(Some((at, devices))),
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    });
    // Only the visible window is cloned, not the whole subset.
    let devices = move || match source() {
        util::RebootPage::Seeded { start, end } => state.query.result.with(|r| {
            r.as_ref()
                .and_then(|r| r.reboot_devices.get(start..end))
                .map(<[DeviceSummary]>::to_vec)
                .unwrap_or_default()
        }),
        util::RebootPage::Fetch { .. } => fetched.with(|f| match f {
            Some((at, devices)) if *at == current() => devices.clone(),
            _ => Vec::new(),
        }),
    };
    let has_devices = move || total() > 0;
    let page_state = move || state.device_page_selection_state(&devices());

    view! {
        <Show
            when=has_devices
            fallback=|| view! { <p class="empty">"No devices flagged for reboot."</p> }
        >
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
            <div class="table-wrap table-wrap--fill">
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
                            <th scope="col">"Reboot Reason"</th>
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
                                            <td>
                                                <DeviceLink device_id=d.device_id name=d.device_name/>
                                                <RunningJobChip device_id=d.device_id/>
                                            </td>
                                            <td>{d.os_name.unwrap_or_default()}</td>
                                            <td>{d.pending_count}</td>
                                            <td>{d.reboot_reason.unwrap_or_default()}</td>
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
