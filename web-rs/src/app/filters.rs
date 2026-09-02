use super::*;

#[component]
pub(crate) fn Filters() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Both install-history statuses are bounded by the lookback window, not just
    // INSTALLED: the backend sets `installed_after` whenever any `is_install_history()`
    // status is requested. Gating this control on INSTALLED alone hid the window from
    // the one view most likely to be truncated by it — a FAILED-only failure dashboard
    // — so the operator could neither see nor widen the bound narrowing their results.
    let install_history_selected = move || {
        state
            .filters
            .statuses
            .get()
            .iter()
            .any(|s| s == "INSTALLED" || s == "FAILED")
    };
    // Fleet-health tabs (Compliance / Needs Reboot) ignore the patch filters, so hide
    // those controls there rather than imply they'd change the device-scope numbers.
    let fleet = move || is_fleet_tab(state.ui.active_tab.get());

    view! {
        <section class="panel">
            <div class="row">
                <h2>"Filters"</h2>
                <Show when=move || state.lookups.loading_lookups()>
                    <span class="chips-label">"Loading…"</span>
                </Show>
                <button
                    class="btn btn-ghost filters-toggle"
                    aria-expanded=move || (!state.ui.filters_collapsed.get()).to_string()
                    on:click=move |_| state.ui.filters_collapsed.update(|c| *c = !*c)
                >
                    {move || {
                        if state.ui.filters_collapsed.get() { "Show ▸" } else { "Hide ▾" }
                    }}
                </button>
            </div>
            <Show when=move || !state.ui.filters_collapsed.get()>
            <div class="subhead">
                "Device scope"
                <span class="subhead-note">"applies to every tab"</span>
            </div>
            // While the lookups load, the scope selects are disabled (their options
            // aren't there yet) and the grid reports busy to assistive tech.
            <div class="grid" aria-busy=move || state.lookups.loading_lookups().to_string()>
                <ScopePicker
                    label="Organizations"
                    all_label="All organizations"
                    options=Signal::derive(move || state.lookups.orgs.get())
                    selected=state.filters.org_ids
                    // Changing the org scope changes which locations exist, so this
                    // one facet routes through the state method that reloads them
                    // (and drops any selected location that just went away).
                    on_toggle=Callback::new(move |id: i64| state.toggle_org(id))
                    disabled=Signal::derive(move || state.lookups.loading_lookups())
                />
                <ScopePicker
                    label="Locations"
                    all_label="All locations"
                    options=Signal::derive(move || {
                        // Names are qualified only where they'd otherwise collide —
                        // with several organizations in scope, "HQ" appears once per
                        // org and is unpickable without saying whose.
                        util::disambiguate_locations(
                            &state.lookups.locations.get(),
                            &state.lookups.orgs.get(),
                        )
                    })
                    selected=state.filters.loc_ids
                    on_toggle=Callback::new(move |id: i64| {
                        state.filters.toggle_id(state.filters.loc_ids, id)
                    })
                    disabled=Signal::derive(move || {
                        state.lookups.loading_lookups() || state.lookups.locations.get().is_empty()
                    })
                />
                <ScopePicker
                    label="Device Roles"
                    all_label="All roles"
                    options=Signal::derive(move || state.lookups.roles.get())
                    selected=state.filters.role_ids
                    on_toggle=Callback::new(move |id: i64| {
                        state.filters.toggle_id(state.filters.role_ids, id)
                    })
                    disabled=Signal::derive(move || state.lookups.loading_lookups())
                />
            </div>
            <div class="stacked-filters">
                <div class="control-group">
                    <span class="chips-label">"OS Type:"</span>
                    {move || {
                        state.lookups.node_classes
                            .get()
                            .into_iter()
                            .map(|nc| {
                                let value = nc.value.clone();
                                let checked = move || state.filters.selected_classes.get().contains(&value);
                                let toggle_value = nc.value.clone();
                                view! {
                                    <label class="chip">
                                        <input
                                            type="checkbox"
                                            prop:checked=checked
                                            on:change=move |_| {
                                                state.filters.toggle_in(state.filters.selected_classes, toggle_value.clone())
                                            }
                                        />
                                        {nc.label}
                                    </label>
                                }
                            })
                            .collect_view()
                    }}
                </div>
                <div class="control-group">
                    <span class="chips-label">"OS name contains:"</span>
                    <input
                        placeholder="e.g. Server 2022"
                        prop:value=move || state.filters.os_name.get()
                        on:input=move |ev| state.filters.os_name.set(event_target_value(&ev))
                    />
                </div>
            </div>
            <div class="subhead">
                "Patch filters"
                <span class="subhead-note">
                    {move || {
                        if fleet() {
                            "Type applies to every tab; the rest are hidden \u{2014} Patches & Failures tabs only"
                        } else {
                            "Type applies to every tab; the rest to Patches & Failures only"
                        }
                    }}
                </span>
            </div>
            // Type sits outside the fleet-tab fold because it is not a row-only
            // facet: only the families the query fetched are in the compliance,
            // severity, age and reboot rollups, so an OS-only query cannot see a
            // third-party backlog on any tab. Hiding it here under "Patch filters
            // don't affect Compliance" said the opposite of what the banner on that
            // tab says.
            <div class="stacked-filters">
                <div class="control-group">
                    <span class="chips-label">"Type:"</span>
                    {["ALL", "OS", "SOFTWARE"]
                        .iter()
                        .map(|t| {
                            let t = t.to_string();
                            let val = t.clone();
                            let active = move || state.filters.patch_type.get() == val;
                            let set = t.clone();
                            view! {
                                <button
                                    class=move || if active() { "seg seg-on" } else { "seg" }
                                    on:click=move |_| state.filters.patch_type.set(set.clone())
                                >
                                    {t}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </div>
            <Show
                when=move || !fleet()
                fallback=|| {
                    view! {
                        <p class="filters-hidden-note">
                            "Status, Severity, Search and the date windows don't affect Compliance or Needs Reboot. Switch to the Patches or Failures tab to use them."
                        </p>
                    }
                }
            >
            <div class="stacked-filters">
                <div class="control-group">
                    <span class="chips-label">"Status:"</span>
                    {STATUS_OPTIONS
                        .iter()
                        .map(|s| {
                            let s = s.to_string();
                            let value = s.clone();
                            let checked = move || state.filters.statuses.get().contains(&value);
                            let toggle = s.clone();
                            view! {
                                <label class="chip">
                                    <input
                                        type="checkbox"
                                        prop:checked=checked
                                        on:change=move |_| {
                                            state.filters.toggle_in(state.filters.statuses, toggle.clone())
                                        }
                                    />
                                    {s}
                                </label>
                            }
                        })
                        .collect_view()}
                </div>
                <div class="control-group">
                    <span class="chips-label">"Severity:"</span>
                    {SEVERITY_OPTIONS
                        .iter()
                        .map(|&(value, label)| {
                            let v_checked = value.to_string();
                            let checked = move || {
                                state.filters.selected_severities.get().contains(&v_checked)
                            };
                            let v_toggle = value.to_string();
                            view! {
                                <label class="chip">
                                    <input
                                        type="checkbox"
                                        prop:checked=checked
                                        on:change=move |_| {
                                            state.filters.toggle_in(state.filters.selected_severities, v_toggle.clone())
                                        }
                                    />
                                    {label}
                                </label>
                            }
                        })
                        .collect_view()}
                </div>
                <div class="control-group">
                    <span class="chips-label">"Search (KB or name):"</span>
                    <input
                        placeholder="e.g. KB5040434"
                        prop:value=move || state.filters.search.get()
                        on:input=move |ev| state.filters.search.set(event_target_value(&ev))
                    />
                </div>
                <div class="control-group">
                    <span class="chips-label">"First seen:"</span>
                    <select
                        prop:value=move || state.filters.detected_window.get()
                        on:change=move |ev| state.filters.detected_window.set(event_target_value(&ev))
                    >
                        <option value="">"Any time"</option>
                        <option value="1">"Last 24 hours"</option>
                        <option value="7">"Last 7 days"</option>
                        <option value="30">"Last 30 days"</option>
                        <option value="90">"Last 90 days"</option>
                        <option value="custom">"Custom range…"</option>
                    </select>
                    <Show when=move || state.filters.detected_window.get() == "custom">
                        <label class="inline">
                            "After"
                            <input
                                type="date"
                                prop:value=move || state.filters.detected_after_date.get()
                                on:change=move |ev| {
                                    state.filters.detected_after_date.set(event_target_value(&ev))
                                }
                            />
                        </label>
                        <label class="inline">
                            "Before"
                            <input
                                type="date"
                                prop:value=move || state.filters.detected_before_date.get()
                                on:change=move |ev| {
                                    state.filters.detected_before_date.set(event_target_value(&ev))
                                }
                            />
                        </label>
                    </Show>
                </div>
                // Install-window field: only relevant when INSTALLED is selected.
                // Lives inside `stacked-filters` as a control-group so its label
                // shares the same aligned column (and row gap) as the rows above.
                <Show when=install_history_selected>
                    <div class="control-group">
                        <span class="chips-label">"Install history window (days):"</span>
                        <input
                            type="number"
                            class="narrow"
                            min="1"
                            max="3650"
                            prop:value=move || state.filters.install_days.get().to_string()
                            on:change=move |ev| {
                                let v = event_target_value(&ev)
                                    .parse::<i64>()
                                    .unwrap_or_else(|_| state.filters.install_days.get_untracked());
                                state.filters.install_days.set(v.clamp(1, 3650));
                            }
                        />
                    </div>
                </Show>
            </div>
            </Show>
            </Show>
        </section>
    }
}

/// One multi-select scope facet: a summary line that opens a scrollable, searchable
/// checkbox list.
///
/// A `<select multiple>` was the obvious alternative and is the wrong tool here — it
/// needs ctrl-click to add to a selection and silently discards the whole selection
/// on a plain click, which for a scope facet means quietly re-running against one
/// organization when the operator meant to add a second. Checkboxes cannot be
/// mis-clicked that way, and the search box is what keeps the pattern usable for a
/// tenant with hundreds of organizations.
///
/// Generic over the three lookup types via [`util::Named`], since they differ only in
/// which struct holds the id and name.
#[component]
fn ScopePicker<T>(
    label: &'static str,
    /// Shown when nothing is selected — the facet's "everything" state.
    all_label: &'static str,
    options: Signal<Vec<T>>,
    selected: RwSignal<Vec<i64>>,
    on_toggle: Callback<i64>,
    disabled: Signal<bool>,
) -> impl IntoView
where
    T: util::Named + Clone + Send + Sync + 'static,
{
    let open = RwSignal::new(false);
    let search = RwSignal::new(String::new());

    // The collapsed summary names the selection while it is short enough to read.
    let summary = move || {
        let sel = selected.get();
        let names = options.with(|opts| util::names_for(&sel, opts.iter().cloned()));
        util::selection_label(&names, all_label)
    };
    let count = move || selected.get().len();

    view! {
        <div class="scope-picker">
            <span class="scope-picker__label">{label}</span>
            <button
                type="button"
                class="scope-picker__summary"
                prop:disabled=move || disabled.get()
                aria-expanded=move || open.get().to_string()
                on:click=move |_| open.update(|o| *o = !*o)
            >
                <span class="scope-picker__value">{summary}</span>
                <span class="scope-picker__caret">{move || if open.get() { "▾" } else { "▸" }}</span>
            </button>
            <Show when=move || open.get()>
                <div class="scope-picker__panel">
                    <div class="scope-picker__tools">
                        <input
                            class="scope-picker__search"
                            placeholder="Filter…"
                            aria-label=format!("Filter {label}")
                            prop:value=move || search.get()
                            on:input=move |ev| search.set(event_target_value(&ev))
                        />
                        <button
                            type="button"
                            class="btn btn-ghost"
                            prop:disabled=move || count() == 0
                            on:click=move |_| selected.set(Vec::new())
                        >
                            "Clear"
                        </button>
                    </div>
                    <div class="scope-picker__list" role="group" aria-label=label>
                        {move || {
                            let needle = search.get();
                            let matching = options.with(|o| util::matching_options(o, &needle));
                            if matching.is_empty() {
                                return view! {
                                    <p class="scope-picker__empty">"No matches"</p>
                                }
                                    .into_any();
                            }
                            matching
                                .into_iter()
                                .map(|o| {
                                    let id = o.id();
                                    let checked = move || selected.get().contains(&id);
                                    view! {
                                        <label class="chip">
                                            <input
                                                type="checkbox"
                                                prop:checked=checked
                                                on:change=move |_| on_toggle.run(id)
                                            />
                                            {o.name().to_string()}
                                        </label>
                                    }
                                })
                                .collect_view()
                                .into_any()
                        }}
                    </div>
                </div>
            </Show>
        </div>
    }
}
