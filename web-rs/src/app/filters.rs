use leptos::prelude::*;

use super::*;

#[component]
pub(crate) fn Filters() -> impl IntoView {
    let state = expect_context::<AppState>();
    let installed_selected = move || {
        state
            .filters
            .statuses
            .get()
            .iter()
            .any(|s| s == "INSTALLED")
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
                <label>
                    "Organization"
                    <select
                        prop:disabled=move || state.lookups.loading_lookups()
                        prop:value=move || {
                            state.filters.org_id.get().map(|id| id.to_string()).unwrap_or_default()
                        }
                        on:change=move |ev| {
                            state.select_org(parse_opt(&event_target_value(&ev)));
                        }
                    >
                        <option value="">"All organizations"</option>
                        {move || {
                            state.lookups.orgs
                                .get()
                                .into_iter()
                                .map(|o| {
                                    view! { <option value=o.id.to_string()>{o.name}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Location"
                    <select
                        prop:disabled=move || {
                            state.lookups.loading_lookups() || state.lookups.locations.get().is_empty()
                        }
                        prop:value=move || {
                            state.filters.loc_id.get().map(|id| id.to_string()).unwrap_or_default()
                        }
                        on:change=move |ev| state.filters.loc_id.set(parse_opt(&event_target_value(&ev)))
                    >
                        <option value="">"All locations"</option>
                        {move || {
                            state.lookups.locations
                                .get()
                                .into_iter()
                                .map(|l| {
                                    view! { <option value=l.id.to_string()>{l.name}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Device Role"
                    <select
                        prop:disabled=move || state.lookups.loading_lookups()
                        prop:value=move || {
                            state.filters.role_id.get().map(|id| id.to_string()).unwrap_or_default()
                        }
                        on:change=move |ev| {
                            state.filters.role_id.set(parse_opt(&event_target_value(&ev)))
                        }
                    >
                        <option value="">"All roles"</option>
                        {move || {
                            state.lookups.roles
                                .get()
                                .into_iter()
                                .map(|r| {
                                    view! { <option value=r.id.to_string()>{r.name}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
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
                            "hidden \u{2014} not applicable to Fleet-health tabs"
                        } else {
                            "Patches & Failures tabs only"
                        }
                    }}
                </span>
            </div>
            <Show
                when=move || !fleet()
                fallback=|| {
                    view! {
                        <p class="filters-hidden-note">
                            "Patch filters don't affect Compliance or Needs Reboot. Switch to the Patches or Failures tab to use them."
                        </p>
                    }
                }
            >
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
                    <span class="chips-label">"Released:"</span>
                    <select
                        prop:value=move || state.filters.release_window.get()
                        on:change=move |ev| state.filters.release_window.set(event_target_value(&ev))
                    >
                        <option value="">"Any time"</option>
                        <option value="1">"Last 24 hours"</option>
                        <option value="7">"Last 7 days"</option>
                        <option value="30">"Last 30 days"</option>
                        <option value="90">"Last 90 days"</option>
                        <option value="custom">"Custom range…"</option>
                    </select>
                    <Show when=move || state.filters.release_window.get() == "custom">
                        <label class="inline">
                            "After"
                            <input
                                type="date"
                                prop:value=move || state.filters.release_after_date.get()
                                on:change=move |ev| {
                                    state.filters.release_after_date.set(event_target_value(&ev))
                                }
                            />
                        </label>
                        <label class="inline">
                            "Before"
                            <input
                                type="date"
                                prop:value=move || state.filters.release_before_date.get()
                                on:change=move |ev| {
                                    state.filters.release_before_date.set(event_target_value(&ev))
                                }
                            />
                        </label>
                    </Show>
                </div>
                // Install-window field: only relevant when INSTALLED is selected.
                // Lives inside `stacked-filters` as a control-group so its label
                // shares the same aligned column (and row gap) as the rows above.
                <Show when=installed_selected>
                    <div class="control-group">
                        <span class="chips-label">"Installed within (days):"</span>
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
