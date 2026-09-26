//! The Patches tab: the flat detail table, the view-mode switch, the grouped
//! view with its on-demand members, and the per-row select checkbox both share.

use std::sync::Arc;

use super::*;

/// Patches-table columns as (header label, sort key), in display order.
const PATCH_COLUMNS: [(&str, RowSortKey); 12] = [
    ("Organization", RowSortKey::Organization),
    ("Location", RowSortKey::Location),
    ("Role", RowSortKey::Role),
    ("Device", RowSortKey::Device),
    ("OS", RowSortKey::Os),
    ("Type", RowSortKey::PatchType),
    ("KB", RowSortKey::Kb),
    ("Patch", RowSortKey::Name),
    ("Severity", RowSortKey::Severity),
    ("Status", RowSortKey::Status),
    ("First seen", RowSortKey::FirstSeenDate),
    ("Installed", RowSortKey::InstalledDate),
];

#[component]
pub(super) fn PatchesTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    let grouped = move || state.query.group_by.get().is_some();
    // Whether the query matched anything at all. Keyed on `rows_total` in both view
    // modes: it's the query-level truth, and it's already populated when the result
    // arrives, whereas `groups_total` is only filled once a header page lands — so
    // gating the empty state on the grouped total would flash "no patches matched"
    // over every grouped refetch.
    let rows_total = move || {
        state
            .query
            .result
            .with(|r| r.as_ref().map_or(0, |r| r.rows_total))
    };
    // The pager, by contrast, paginates whichever unit the active view mode shows:
    // flat rows or group headers. Sizing it from `rows_total` while grouped is what
    // left ~98% of groups unreachable — it read "Page 1 of 400" off 40,000 rows
    // while the grouped view only ever rendered the first 100 groups, and every
    // Next click fetched rows that were rendered nowhere.
    let total = move || util::paged_total(grouped(), rows_total(), state.query.groups_total.get());
    // Pager arithmetic lives in `util` so it can be host-tested; this component only
    // wires signals to it. The clamp matters because the stored page outlives the
    // result it was chosen against (an auto-refresh returning fewer rows, or a
    // switch between flat and grouped view, can both strand it past the end).
    let page_count = move || util::page_count(total(), PATCHES_PAGE_SIZE);
    let page = move || util::clamp_page(state.query.patches_page.get(), page_count());
    let rows = move || state.query.page_rows.get();
    let pager_summary = move || {
        util::pager_summary(
            if grouped() { "Groups" } else { "Rows" },
            page(),
            PATCHES_PAGE_SIZE,
            total(),
        )
    };
    // Page navigation updates the index and fetches that page on demand — of
    // headers or of rows, matching what the view is actually showing.
    let go_to = move |target: usize| {
        state.query.patches_page.set(target);
        if grouped() {
            state.fetch_groups(target);
        } else {
            state.fetch_page(target);
        }
    };
    let go_prev = move |_| go_to(util::prev_page(page()));
    let go_next = move |_| go_to(util::next_page(page(), page_count()));

    view! {
        <Show
            when=move || state.query.result.with(|r| r.is_some())
            fallback=|| view! { <p class="empty">"Run a query to list patches."</p> }
        >
            <ScopeBanner
                kind="filtered"
                tier="Filtered results"
                reflects="every patch matching your device scope and all patch filters."
                filters="Device scope + Type, Status, Severity, Search, First-seen and Installed-within are all applied."
            />
            <Show
                when=move || { rows_total() > 0 }
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
                <Show when=move || state.action_surface_visible()>
                    <ActionBar/>
                </Show>
                <ViewModeSwitch/>
                <Show when=move || state.query.group_by.get().is_some()>
                    <GroupedPatches/>
                </Show>
                <Show when=move || state.query.group_by.get().is_none()>
                <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            // Deliberately outside PATCH_COLUMNS: the select column
                            // is not sortable, so it has no sort key.
                            <th scope="col" class="col-select">
                                <input
                                    type="checkbox"
                                    aria-label="Select every patch row on this page"
                                    prop:checked=move || state.page_selection_state().0
                                    prop:indeterminate=move || state.page_selection_state().1
                                    on:change=move |ev| {
                                        state.toggle_page_selection(event_target_checked(&ev))
                                    }
                                />
                            </th>
                            {PATCH_COLUMNS
                                .iter()
                                .map(|&(label, key)| {
                                    view! {
                                        <th
                                            scope="col"
                                            aria-sort=move || {
                                                aria_sort(state.query.patches_sort.get(), key)
                                            }
                                        >
                                            <button
                                                class="th-sort"
                                                on:click=move |_| state.cycle_sort(key)
                                            >
                                                {label}
                                                <span aria-hidden="true">
                                                    {move || {
                                                        sort_glyph(state.query.patches_sort.get(), key)
                                                    }}
                                                </span>
                                            </button>
                                        </th>
                                    }
                                })
                                .collect_view()}
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            rows()
                                .into_iter()
                                .map(|r| {
                                    let sev = sev_class(&r.severity);
                                    let stat = status_class(&r.status);
                                    let row = Arc::new(r.clone());
                                    view! {
                                        <tr>
                                            <RowCheckbox row=row/>
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
                                            <td>{r.first_seen_date.unwrap_or_default()}</td>
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
        </Show>
    }
}

/// Flat / By device / By patch switch for the Patches tab. Grouping is a backend
/// re-query over the cached rows, not a client-side regroup of the visible page.
#[component]
fn ViewModeSwitch() -> impl IntoView {
    let state = expect_context::<AppState>();
    const MODES: [(Option<GroupBy>, &str); 3] = [
        (None, "Flat"),
        (Some(GroupBy::Device), "By device"),
        (Some(GroupBy::Patch), "By patch"),
    ];
    view! {
        <div class="view-modes" role="group" aria-label="Patch view mode">
            <span class="chips-label">"View"</span>
            {MODES
                .iter()
                .map(|(mode, label)| {
                    let mode = *mode;
                    let active = move || state.query.group_by.get() == mode;
                    view! {
                        <button
                            class=move || if active() { "chip chip-on" } else { "chip" }
                            aria-pressed=move || active().to_string()
                            on:click=move |_| state.set_group_by(mode)
                        >
                            {*label}
                        </button>
                    }
                })
                .collect_view()}
        </div>
    }
}

/// The grouped Patches view: one expandable row per group, its members loaded on
/// demand. Selection stays per patch row — expanding a group and ticking one of
/// its members selects exactly that patch, the same as in the flat table.
#[component]
fn GroupedPatches() -> impl IntoView {
    let state = expect_context::<AppState>();
    let groups = move || state.query.groups.get();
    let by_device = move || state.query.group_by.get() == Some(GroupBy::Device);
    view! {
        <Show
            when=move || !groups().is_empty()
            fallback=|| view! { <p class="empty">"No groups to show."</p> }
        >
            <ul class="groups">
                {move || {
                    groups()
                        .into_iter()
                        .map(|g| {
                            let key = g.key.clone();
                            // Each closure below outlives the others, so every one
                            // takes its own clone of the key.
                            let (k_all, k_some, k_tick, tog_key, mem_key) = (
                                key.clone(),
                                key.clone(),
                                key.clone(),
                                key.clone(),
                                key.clone(),
                            );
                            // Signal::derive is Copy, so the caret and the
                            // aria-expanded attribute can both read it.
                            let open = Signal::derive(move || {
                                state.query.expanded.with(|e| e.contains(&key))
                            });
                            let sev = sev_class(&g.severity);
                            let count = util::group_count_label(by_device(), g.rows, g.devices);
                            let sub = g.sublabel.clone().unwrap_or_default();
                            let label = g.label.clone();
                            let aria = format!("Select all loaded patches in {label}");
                            let tick_label = label.clone();
                            view! {
                                <li class="group">
                                    <div class="group-head">
                                        <input
                                            type="checkbox"
                                            aria-label=aria
                                            prop:checked=move || {
                                                state.group_selection_state(&k_all).0
                                            }
                                            prop:indeterminate=move || {
                                                state.group_selection_state(&k_some).1
                                            }
                                            on:change=move |ev| {
                                                state
                                                    .toggle_group_selection(
                                                        &k_tick,
                                                        tick_label.clone(),
                                                        event_target_checked(&ev),
                                                    )
                                            }
                                        />
                                        <button
                                            class="group-toggle"
                                            aria-expanded=move || open.get().to_string()
                                            on:click=move |_| state.toggle_group(tog_key.clone())
                                        >
                                            <span class="group-caret">
                                                {move || if open.get() { "▾" } else { "▸" }}
                                            </span>
                                            <span class="group-label">{g.label}</span>
                                            <span class="group-sub">{sub}</span>
                                            <span class=sev>{g.severity}</span>
                                            <span class="group-count">{count}</span>
                                            <Show when=move || g.needs_reboot>
                                                <span class="chip-note">"needs reboot"</span>
                                            </Show>
                                            <Show when=move || g.offline>
                                                <span class="chip-note">"offline"</span>
                                            </Show>
                                        </button>
                                    </div>
                                    <Show when=move || open.get()>
                                        {
                                            let mem_key = mem_key.clone();
                                            move || {
                                                let rows = state
                                                    .query
                                                    .members
                                                    .with(|m| m.get(&mem_key).cloned());
                                                match rows {
                                                    None => {
                                                        view! {
                                                            <p class="empty">"Loading…"</p>
                                                        }
                                                            .into_any()
                                                    }
                                                    Some(rows) if rows.is_empty() => {
                                                        view! {
                                                            <p class="empty">"No patches here."</p>
                                                        }
                                                            .into_any()
                                                    }
                                                    Some(rows) => {
                                                        let capped = rows.len() >= GROUP_MEMBER_LIMIT;
                                                        view! {
                                                            <>
                                                                <GroupMembers rows=rows/>
                                                                // Never imply a partial
                                                                // list is the whole group.
                                                                <Show when=move || capped>
                                                                    <p class="empty">
                                                                        "Showing the first "
                                                                        {group_thousands(
                                                                            GROUP_MEMBER_LIMIT,
                                                                        )}
                                                                        " — narrow the filters to see the rest."
                                                                    </p>
                                                                </Show>
                                                            </>
                                                        }
                                                            .into_any()
                                                    }
                                                }
                                            }
                                        }
                                    </Show>
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ul>
        </Show>
    }
}

/// The select cell of one patch row — the flat table and a group's members render
/// the same checkbox, which ticks exactly this patch on this device. `Apply` still
/// installs everything approved on the device; the per-row detail is what a
/// `kbAllowList` script is given.
///
/// Takes the row behind an `Arc`: both closures need an owned `'static` row, and
/// `PatchRow` carries a dozen-plus `String`s, so cloning it per closure meant ~200
/// row copies for every reactive re-render of a 100-row page. An `Arc` makes each
/// closure's copy a refcount bump.
#[component]
fn RowCheckbox(row: Arc<PatchRow>) -> impl IntoView {
    let state = expect_context::<AppState>();
    let label = format!(
        "Select {} on {}",
        row.kb
            .clone()
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| row.name.clone()),
        row.device_name,
    );
    let checked_row = Arc::clone(&row);
    view! {
        <td class="col-select">
            <input
                type="checkbox"
                aria-label=label
                prop:disabled=row.device_id == util::ORPHAN_DEVICE_ID
                prop:checked=move || state.is_row_selected(&checked_row)
                on:change=move |ev| state.toggle_row_selection(&row, event_target_checked(&ev))
            />
        </td>
    }
}

/// The member rows inside an expanded group. Deliberately a compact table rather
/// than the full detail grid — the group header already carries the shared columns.
#[component]
fn GroupMembers(rows: Vec<PatchRow>) -> impl IntoView {
    let state = expect_context::<AppState>();
    let by_device = move || state.query.group_by.get() == Some(GroupBy::Device);
    view! {
        <div class="table-wrap">
            <table class="group-members">
                <tbody>
                    {rows
                        .into_iter()
                        .map(|r| {
                            let sev = sev_class(&r.severity);
                            let stat = status_class(&r.status);
                            let row = Arc::new(r.clone());
                            view! {
                                <tr>
                                    <RowCheckbox row=row/>
                                    // In a device group the members are patches; in a
                                    // patch group they are the devices it's missing on.
                                    <td>
                                        {move || {
                                            if by_device() {
                                                r.kb.clone().unwrap_or_default()
                                            } else {
                                                r.device_name.clone()
                                            }
                                        }}
                                    </td>
                                    <td class="patch-name">
                                        {move || {
                                            if by_device() {
                                                r.name.clone()
                                            } else {
                                                r.organization.clone()
                                            }
                                        }}
                                    </td>
                                    <td><span class=sev>{r.severity.clone()}</span></td>
                                    <td><span class=stat>{r.status.clone()}</span></td>
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
        </div>
    }
}
