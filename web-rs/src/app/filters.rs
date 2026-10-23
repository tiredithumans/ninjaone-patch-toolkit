//! The filters: the device scope bar under the top bar (every tab), and the patch
//! filter bar at the top of the Patches and Failures tabs (the only tabs whose
//! rows those filters reach).
//!
//! They used to be one ~490px panel above the tabs whose lower half vanished on
//! Compliance and Needs Reboot, so the content under it jumped on every tab switch
//! and the panel said in a sentence which of its own controls did nothing there.

use leptos::task::spawn_local;

use super::menu::{MenuButton, dismiss_on_outside};
use super::*;

/// The First-seen choices as (stored value, label). `custom` reveals the two date
/// inputs; the numeric values are days, read by `util::filter_params`.
const DETECTED_WINDOWS: [(&str, &str); 6] = [
    ("", "Any time"),
    ("1", "Last 24 hours"),
    ("7", "Last 7 days"),
    ("30", "Last 30 days"),
    ("90", "Last 90 days"),
    ("custom", "Custom range…"),
];

/// Patch type choices as (value, label).
const PATCH_TYPES: [(&str, &str); 3] = [("ALL", "All"), ("OS", "OS"), ("SOFTWARE", "Software")];

/// The device scope: organizations, locations, roles, OS type and name, and the
/// patch type — every filter that narrows every tab. One row of compact pickers.
#[component]
pub(crate) fn ScopeBar() -> impl IntoView {
    let state = expect_context::<AppState>();
    let loading = Signal::derive(move || state.lookups.loading_lookups());
    view! {
        <section
            class="scope-bar"
            id=super::shortcuts::FILTERS_PANEL_ID
            aria-label="Device scope"
            // While the lookups load, the pickers are disabled (their options aren't
            // there yet) and the bar reports busy to assistive tech.
            aria-busy=move || loading.get().to_string()
        >
            <Show when=move || !state.session.web_mode.get() && !state.session.demo.get()>
                <PresetsMenu/>
            </Show>
            <Picker
                label="Organizations"
                all_label="All"
                options=Signal::derive(move || state.lookups.orgs.get())
                selected=state.filters.org_ids
                // Changing the org scope changes which locations exist, so this one
                // facet routes through the state method that reloads them (and drops
                // any selected location that just went away).
                on_toggle=Callback::new(move |id: i64| state.toggle_org(id))
                // Clear goes through the same reload, or the location list kept
                // offering (and the selection kept) only the cleared orgs' sites.
                on_clear=Callback::new(move |()| state.clear_orgs())
                disabled=loading
                searchable=true
            />
            <Picker
                label="Locations"
                all_label="All"
                options=Signal::derive(move || {
                    // Names are qualified only where they'd otherwise collide — with
                    // several organizations in scope, "HQ" appears once per org and is
                    // unpickable without saying whose.
                    util::disambiguate_locations(
                        &state.lookups.locations.get(),
                        &state.lookups.orgs.get(),
                    )
                })
                selected=state.filters.loc_ids
                on_toggle=Callback::new(move |id: i64| {
                    state.filters.toggle_id(state.filters.loc_ids, id)
                })
                on_clear=Callback::new(move |()| state.filters.loc_ids.set(Vec::new()))
                disabled=Signal::derive(move || {
                    loading.get() || state.lookups.locations.with(Vec::is_empty)
                })
                searchable=true
            />
            <Picker
                label="Roles"
                all_label="All"
                options=Signal::derive(move || state.lookups.roles.get())
                selected=state.filters.role_ids
                on_toggle=Callback::new(move |id: i64| {
                    state.filters.toggle_id(state.filters.role_ids, id)
                })
                on_clear=Callback::new(move |()| state.filters.role_ids.set(Vec::new()))
                disabled=loading
                searchable=true
            />
            // NinjaOne device groups (saved searches). Disabled while the tenant has
            // none, or the token cannot list them.
            <Picker
                label="Groups"
                all_label="All"
                options=Signal::derive(move || state.lookups.device_groups.get())
                selected=state.filters.group_ids
                on_toggle=Callback::new(move |id: i64| {
                    state.filters.toggle_id(state.filters.group_ids, id)
                })
                on_clear=Callback::new(move |()| state.filters.group_ids.set(Vec::new()))
                disabled=Signal::derive(move || {
                    loading.get() || state.lookups.device_groups.with(Vec::is_empty)
                })
                searchable=true
            />
            <Picker
                label="OS type"
                all_label="All"
                options=Signal::derive(move || state.lookups.node_classes.get())
                selected=state.filters.selected_classes
                on_toggle=Callback::new(move |v: String| {
                    state.filters.toggle_in(state.filters.selected_classes, v)
                })
                on_clear=Callback::new(move |()| state.filters.selected_classes.set(Vec::new()))
                disabled=Signal::derive(move || state.lookups.node_classes.with(Vec::is_empty))
            />
            <input
                class="scope-bar__os-name"
                aria-label="OS name contains"
                placeholder="OS name contains…"
                prop:value=move || state.filters.os_name.get()
                on:input=move |ev| state.filters.os_name.set(event_target_value(&ev))
            />
            // Type is a scope facet, not a row filter: only the families the query
            // fetched are in the compliance, severity, age and reboot rollups, so an
            // OS-only query cannot see a third-party backlog on any tab.
            <div
                class="seg-group"
                role="group"
                aria-label="Patch type"
                title="Which patch families the query fetches. Narrows every tab, the fleet-health rollups included."
            >
                <span class="seg-group__label">"Type"</span>
                {PATCH_TYPES
                    .into_iter()
                    .map(|(value, label)| {
                        let active = move || state.filters.patch_type.with(|t| t == value);
                        view! {
                            <button
                                type="button"
                                class=move || if active() { "seg seg-on" } else { "seg" }
                                aria-pressed=move || active().to_string()
                                on:click=move |_| state.filters.patch_type.set(value.to_string())
                            >
                                {label}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
            <Show when=move || loading.get()>
                <span class="chips-label">"Loading…"</span>
            </Show>
            <FiltersChanged/>
        </section>
    }
}

/// "● Filters changed" while the controls describe a different query from the one
/// on screen (`util::filters_changed`); a click runs it. Nothing applies a filter
/// edit until a run — the old chip row showed the last run's filters with nothing
/// to say they no longer matched the controls above it. Its tooltip names what the
/// results on screen were filtered by.
#[component]
fn FiltersChanged() -> impl IntoView {
    let state = expect_context::<AppState>();
    let changed = Memo::new(move |_| {
        state.filters.track();
        let live = state.filters.query_args();
        state
            .query
            .applied_args
            .with(|applied| util::filters_changed(applied.as_ref(), &live))
    });
    let applied = move || {
        state.query.applied_filters.with(|a| {
            let chips = a.as_ref().map(filter_chips).unwrap_or_default();
            if chips.is_empty() {
                "The results on screen: no filters (whole fleet).".to_string()
            } else {
                let labels: Vec<String> = chips.into_iter().map(|c| c.label).collect();
                format!("The results on screen: {}.", labels.join(" · "))
            }
        })
    };
    view! {
        <Show when=move || changed.get()>
            <button
                type="button"
                class="filters-changed"
                title=applied
                on:click=move |_| state.run_query()
            >
                "● Filters changed · Apply"
            </button>
        </Show>
    }
}

/// The patch filters — Status, Severity, Search, First seen and, for an install
/// status, the install-history window — at the top of the Patches and Failures
/// tabs. Rendered outside those tabs' result guards, so they can be set before the
/// first run and widened when nothing matched.
///
/// The bar remounts on every tab switch, so each select's choice rides on its
/// options (`prop:selected`): a remount set a select's value before its options
/// existed, and the control showed "Any time" over a window still filtering.
#[component]
pub(crate) fn PatchFilterBar() -> impl IntoView {
    let state = expect_context::<AppState>();
    let install_history = move || {
        state
            .filters
            .statuses
            .with(|s| util::needs_install_window(s))
    };
    let severity_choices: Vec<util::Choice> = SEVERITY_OPTIONS
        .iter()
        .map(|&(value, label)| util::Choice { value, label })
        .collect();
    view! {
        <div class="patch-filter-bar" role="group" aria-label="Patch filters">
            <Picker
                label="Status"
                all_label="None"
                options=Signal::derive(|| util::STATUS_CHOICES.to_vec())
                selected=state.filters.statuses
                on_toggle=Callback::new(move |v: String| {
                    state.filters.toggle_in(state.filters.statuses, v)
                })
                // A query needs a status: the last one cannot be unticked, so "Select
                // at least one status" is never an error raised from another tab.
                keep_one=true
                disabled=false
            />
            <Picker
                label="Severity"
                all_label="All"
                options=Signal::derive(move || severity_choices.clone())
                selected=state.filters.selected_severities
                on_toggle=Callback::new(move |v: String| {
                    state.filters.toggle_in(state.filters.selected_severities, v)
                })
                on_clear=Callback::new(move |()| state.filters.selected_severities.set(Vec::new()))
                disabled=false
            />
            <input
                id=super::shortcuts::SEARCH_INPUT_ID
                class="patch-filter-bar__search"
                type="search"
                aria-label="Search (KB or name)"
                placeholder="Search KB or name"
                prop:value=move || state.filters.search.get()
                on:input=move |ev| state.filters.search.set(event_target_value(&ev))
            />
            <label class="inline">
                "First seen"
                <select on:change=move |ev| state.filters.detected_window.set(event_target_value(&ev))>
                    {DETECTED_WINDOWS
                        .into_iter()
                        .map(|(value, label)| {
                            let selected = move || state.filters.detected_window.get() == value;
                            view! {
                                <option value=value prop:selected=selected>
                                    {label}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
            </label>
            <Show when=move || state.filters.detected_window.get() == "custom">
                <label class="inline">
                    "After"
                    <input
                        type="date"
                        prop:value=move || state.filters.detected_after_date.get()
                        on:change=move |ev| state.filters.detected_after_date.set(event_target_value(&ev))
                    />
                </label>
                <label class="inline">
                    "Before"
                    <input
                        type="date"
                        prop:value=move || state.filters.detected_before_date.get()
                        on:change=move |ev| state.filters.detected_before_date.set(event_target_value(&ev))
                    />
                </label>
            </Show>
            // Only while an install status is picked: the window bounds the whole
            // install-history pull, so it is meaningless otherwise.
            <Show when=install_history>
                <label class="inline">
                    "Install history"
                    // "Custom range" replaces the relative lookback (backend-side too),
                    // so a past patch cycle can be reviewed on its own dates.
                    <select on:change=move |ev| {
                        state.filters.install_custom.set(event_target_value(&ev) == "custom")
                    }>
                        <option value="" prop:selected=move || !state.filters.install_custom.get()>
                            "Last N days"
                        </option>
                        <option value="custom" prop:selected=move || state.filters.install_custom.get()>
                            "Custom range…"
                        </option>
                    </select>
                </label>
                <Show
                    when=move || state.filters.install_custom.get()
                    fallback=move || {
                        view! {
                            <label class="inline">
                                <input
                                    type="number"
                                    aria-label="Install history window (days)"
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
                                "days"
                            </label>
                        }
                    }
                >
                    <label class="inline">
                        "From"
                        <input
                            type="date"
                            prop:value=move || state.filters.install_after_date.get()
                            on:change=move |ev| state.filters.install_after_date.set(event_target_value(&ev))
                        />
                    </label>
                    <label class="inline">
                        "To"
                        <input
                            type="date"
                            prop:value=move || state.filters.install_before_date.get()
                            on:change=move |ev| state.filters.install_before_date.set(event_target_value(&ev))
                        />
                    </label>
                    {move || {
                        util::install_range_problem(
                            &state.filters.install_after_date.get(),
                            &state.filters.install_before_date.get(),
                        )
                            .map(|msg| view! { <span class="field-hint" role="alert">{msg}</span> })
                    }}
                </Show>
            </Show>
        </div>
    }
}

/// One multi-select facet: a compact button naming the facet and its value, which
/// opens a checkbox list (searchable for the long lookups).
///
/// A `<select multiple>` was the obvious alternative and is the wrong tool here — it
/// needs ctrl-click to add to a selection and silently discards the whole selection
/// on a plain click, which for a scope facet means quietly re-running against one
/// organization when the operator meant to add a second. Checkboxes cannot be
/// mis-clicked that way, and the search box is what keeps the pattern usable for a
/// tenant with hundreds of organizations.
///
/// Generic over every facet via [`util::Named`]: the id-keyed lookups and the
/// string-keyed value/label choices.
#[component]
fn Picker<T>(
    label: &'static str,
    /// Shown when nothing is selected — the facet's "everything" state.
    all_label: &'static str,
    options: Signal<Vec<T>>,
    selected: RwSignal<Vec<T::Key>>,
    on_toggle: Callback<T::Key>,
    /// Empties the selection; `None` offers no Clear. A callback rather than a
    /// direct write so a facet with side effects (the org scope reloads the
    /// locations) keeps them.
    #[prop(optional)]
    on_clear: Option<Callback<()>>,
    #[prop(into)] disabled: Signal<bool>,
    /// A search box over the options, for the tenant-sized lookups.
    #[prop(optional)]
    searchable: bool,
    /// The last ticked option cannot be unticked.
    #[prop(optional)]
    keep_one: bool,
) -> impl IntoView
where
    T: util::Named + Clone + Send + Sync + 'static,
{
    let open = RwSignal::new(false);
    let search = RwSignal::new(String::new());
    let root: NodeRef<leptos::html::Div> = NodeRef::new();
    dismiss_on_outside(open, root);

    let names = move || {
        let sel = selected.get();
        options.with(|opts| util::names_for(&sel, opts.iter().cloned()))
    };
    // The button names the selection while it is short enough to read; the tooltip
    // always names all of it.
    let summary = move || util::selection_label(&names(), all_label);
    let full = move || {
        let n = names();
        if n.is_empty() {
            format!("{label}: {all_label}")
        } else {
            format!("{label}: {}", n.join(", "))
        }
    };
    let count = move || selected.with(Vec::len);

    view! {
        <div class="picker" node_ref=root>
            <button
                type="button"
                class=move || {
                    if count() > 0 { "picker__button picker__button--set" } else { "picker__button" }
                }
                title=full
                aria-haspopup="true"
                aria-expanded=move || open.get().to_string()
                prop:disabled=move || disabled.get()
                on:click=move |_| open.update(|o| *o = !*o)
            >
                <span class="picker__label">{label}</span>
                <span class="picker__value">{summary}</span>
                <span class="picker__caret" aria-hidden="true">"▾"</span>
            </button>
            <Show when=move || open.get()>
                <div class="picker__panel">
                    <Show when=move || searchable || on_clear.is_some()>
                        <div class="picker__tools">
                            <Show when=move || searchable>
                                <input
                                    class="picker__search"
                                    placeholder="Filter…"
                                    aria-label=format!("Filter {label}")
                                    prop:value=move || search.get()
                                    on:input=move |ev| search.set(event_target_value(&ev))
                                />
                            </Show>
                            {on_clear.map(|clear| view! {
                                <button
                                    type="button"
                                    class="btn btn-ghost"
                                    prop:disabled=move || count() == 0
                                    on:click=move |_| clear.run(())
                                >
                                    "Clear"
                                </button>
                            })}
                        </div>
                    </Show>
                    <div class="picker__list" role="group" aria-label=label>
                        {move || {
                            let needle = search.get();
                            let matching = options.with(|o| util::matching_options(o, &needle));
                            if matching.is_empty() {
                                return view! { <p class="picker__empty">"No matches"</p> }.into_any();
                            }
                            matching
                                .into_iter()
                                .map(|o| {
                                    let key = o.key();
                                    let (k_checked, k_lock, k_toggle) = (key.clone(), key.clone(), key);
                                    let checked = move || selected.with(|s| s.contains(&k_checked));
                                    let locked = move || {
                                        keep_one && selected.with(|s| s.len() == 1 && s.contains(&k_lock))
                                    };
                                    view! {
                                        <label class="picker__option">
                                            <input
                                                type="checkbox"
                                                prop:checked=checked
                                                prop:disabled=locked
                                                on:change=move |_| on_toggle.run(k_toggle.clone())
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

/// Saved filter presets, in a menu at the head of the scope bar. Applying one sets
/// the controls (and says "Filters changed" until it is run). Presets persist
/// through the backend's settings file, so the browser demo has none.
#[component]
fn PresetsMenu() -> impl IntoView {
    let state = expect_context::<AppState>();
    let saving = RwSignal::new(false);

    let save_preset = move |_| {
        if saving.get_untracked() {
            return;
        }
        let name = state.settings.preset_name.get_untracked();
        if name.trim().is_empty() {
            state.notify(Toast::err("Name the preset first"));
            return;
        }
        let preset = Preset {
            name: name.trim().to_string(),
            filter: state.current_filter(),
            patch_type: Some(state.filters.patch_type.get_untracked()),
            statuses: Some(state.filters.statuses.get_untracked()),
            install_days: Some(state.filters.install_days.get_untracked()),
        };
        saving.set(true);
        spawn_local(async move {
            match api::save_preset(preset).await {
                Ok(p) => {
                    state.settings.presets.set(p);
                    state.settings.preset_name.set(String::new());
                    state.notify(Toast::ok("Preset saved"));
                }
                Err(e) => state.notify(Toast::err(e)),
            }
            saving.set(false);
        });
    };

    view! {
        <MenuButton
            label=Signal::derive(move || {
                let n = state.settings.presets.with(Vec::len);
                if n == 0 { "Presets ▾".to_string() } else { format!("Presets ({n}) ▾") }
            })
            class="btn-ghost"
            title=String::from("Apply or save a set of filters")
        >
            <div class="presets">
            {move || {
                state.settings.presets
                    .get()
                    .into_iter()
                    .map(|p| {
                        let name = p.name.clone();
                        let label_name = p.name.clone();
                        let p2 = p.clone();
                        let del_name = p.name.clone();
                        // Two-click confirm: first click arms, second deletes;
                        // mouseleave/blur disarm. Component-local so the signal is
                        // disposed with this chip when the preset list re-renders.
                        let armed = RwSignal::new(false);
                        view! {
                            <span class="preset">
                                <button
                                    type="button"
                                    class="menu__item"
                                    on:click=move |_| state.apply_preset(p2.clone())
                                >
                                    {name}
                                </button>
                                // Kept open across the arming click.
                                <button
                                    type="button"
                                    data-keep-open=""
                                    class=move || if armed.get() { "x x-armed" } else { "x" }
                                    aria-label=move || {
                                        if armed.get() {
                                            format!("Confirm delete preset {label_name}")
                                        } else {
                                            format!("Delete preset {label_name}")
                                        }
                                    }
                                    on:click=move |_| {
                                        if !armed.get_untracked() {
                                            armed.set(true);
                                            return;
                                        }
                                        let n = del_name.clone();
                                        spawn_local(async move {
                                            match api::delete_preset(n).await {
                                                Ok(p) => state.settings.presets.set(p),
                                                // It used to fail silently, leaving a
                                                // chip that looked deleted-then-not.
                                                Err(e) => state.notify(Toast::err(format!(
                                                    "Couldn't delete the preset: {e}"
                                                ))),
                                            }
                                        });
                                    }
                                    on:mouseleave=move |_| armed.set(false)
                                    on:blur=move |_| armed.set(false)
                                >
                                    {move || if armed.get() { "Delete?" } else { "×" }}
                                </button>
                            </span>
                        }
                    })
                    .collect_view()
            }}
            <Show when=move || state.settings.presets.with(Vec::is_empty)>
                <p class="menu__note">"No presets yet."</p>
            </Show>
            </div>
            <div class="presets-save">
                <input
                    class="preset-name"
                    aria-label="Preset name"
                    placeholder="Name these filters"
                    prop:value=move || state.settings.preset_name.get()
                    on:input=move |ev| state.settings.preset_name.set(event_target_value(&ev))
                />
                <button
                    type="button"
                    class="btn"
                    data-keep-open=""
                    prop:disabled=move || saving.get()
                    on:click=save_preset
                >
                    "Save"
                </button>
            </div>
        </MenuButton>
    }
}
