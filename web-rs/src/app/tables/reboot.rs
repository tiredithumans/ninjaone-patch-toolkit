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
    // The column the list is sorted by. Sorted backend-side over every flagged
    // device, since this tab holds one page — or here, when the summary carries the
    // whole list (`util::reboot_page_source`).
    let sort = RwSignal::new(None::<util::ColumnSort>);
    let source = move || {
        util::reboot_page_source(
            current(),
            PATCHES_PAGE_SIZE,
            seeded(),
            total(),
            sort.with(Option::is_some),
        )
    };
    // The page last fetched, by index; `stamp` drops a reply that a newer page or a
    // newer result overtook, as the Patches pager's view stamp does.
    let fetched = RwSignal::new(None::<(usize, Vec<DeviceSummary>)>);
    let stamp = RwSignal::new(0u64);
    // A page that failed to load, and the Retry that re-runs the fetch below. Shown
    // in place: with only a toast, the empty table read as "nothing on this page".
    let failed = RwSignal::new(None::<String>);
    let retry = RwSignal::new(0u32);
    Effect::new(move |_| {
        retry.track();
        failed.set(None);
        // Re-runs when the result or the page changes; either retires the last page.
        let util::RebootPage::Fetch { offset, limit } = source() else {
            return;
        };
        let at = current();
        let by = util::reboot_sort_request(sort.get());
        let mine = util::next_query_seq(stamp.get_untracked());
        stamp.set(mine);
        fetched.set(None);
        spawn_local(async move {
            let outcome = api::get_reboot_devices(offset, limit, by).await;
            // The tab may have closed during the read, which disposes `stamp`.
            let Some(current) = stamp.try_get_untracked() else {
                return;
            };
            if util::is_superseded(current, mine) {
                return;
            }
            match outcome {
                Ok(devices) => fetched.set(Some((at, devices))),
                Err(e) => failed.set(Some(e)),
            }
        });
    });
    // A new order starts from its first page: page 3 of another order is no place.
    Effect::new(move |was: Option<Option<util::ColumnSort>>| {
        let now = sort.get();
        if was.is_some_and(|was| was != now) {
            page.set(0);
        }
        now
    });
    // The page on screen.
    let devices = move || match source() {
        util::RebootPage::Seeded { start, end } => state.query.result.with(|r| {
            let Some(r) = r.as_ref() else {
                return Vec::new();
            };
            match sort.get() {
                // Unsorted, only the visible window is cloned.
                None => r
                    .reboot_devices
                    .get(start..end)
                    .map(<[DeviceSummary]>::to_vec)
                    .unwrap_or_default(),
                // Sorted locally only when the summary holds the whole list.
                by => {
                    let mut all = r.reboot_devices.clone();
                    util::sort_by_column(&mut all, by, util::reboot_sort_value);
                    all.get(start..end).map(<[_]>::to_vec).unwrap_or_default()
                }
            }
        }),
        util::RebootPage::Fetch { .. } => fetched.with(|f| match f {
            Some((at, devices)) if *at == current() => devices.clone(),
            _ => Vec::new(),
        }),
    };
    let has_devices = move || total() > 0;
    let page_state = move || state.device_page_selection_state(&devices());
    // The device whose box was clicked last, for a Shift-click range on this page.
    let range_anchor = RwSignal::new(None::<i64>);

    view! {
        <Show
            when=has_devices
            fallback=move || {
                // Before the first run nothing is known, which is not "none".
                if state.query.result.with(Option::is_none) {
                    view! { <NoResult idle="Run a query to see which devices need a reboot."/> }
                        .into_any()
                } else {
                    view! { <p class="empty">"No devices flagged for reboot."</p> }.into_any()
                }
            }
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
            <Show when=move || failed.with(Option::is_some)>
                <div class="error-banner" role="alert">
                    <span>
                        {move || {
                            format!(
                                "Couldn't load this page of devices: {}",
                                failed.get().unwrap_or_default(),
                            )
                        }}
                    </span>
                    <button class="link-btn" on:click=move |_| retry.update(|n| *n += 1)>
                        "Retry"
                    </button>
                </div>
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
                            {util::REBOOT_HEADERS
                                .iter()
                                .enumerate()
                                .map(|(col, &label)| header_cell(label, "", col, Some(sort)))
                                .collect_view()}
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
                                                    // Before `change`, with the box
                                                    // already flipped: a Shift-click
                                                    // takes every device from the last
                                                    // one clicked to this one with it.
                                                    on:click=move |ev| {
                                                        if ev.shift_key() {
                                                            let page = devices();
                                                            let at = |id: i64| {
                                                                page.iter().position(|d| d.device_id == id)
                                                            };
                                                            let range = util::shift_click_range(
                                                                range_anchor.get_untracked().and_then(at),
                                                                at(id),
                                                            );
                                                            if let Some(range) = range {
                                                                state
                                                                    .toggle_device_page(
                                                                        &page[range],
                                                                        event_target_checked(&ev),
                                                                    );
                                                            }
                                                        }
                                                        range_anchor.set(Some(id));
                                                    }
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
