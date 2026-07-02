use leptos::prelude::*;
use leptos::task::spawn_local;

use super::*;

#[component]
pub(crate) fn RunControls() -> impl IntoView {
    let state = expect_context::<AppState>();

    view! {
        <section class="panel">
            <div class="controls">
                <button
                    class="btn btn-primary"
                    prop:disabled=move || state.run.busy.get()
                    on:click=move |_| state.run_query()
                >
                    {move || if state.run.busy.get() { "Running…" } else { "Run query" }}
                </button>
                <button
                    class="btn"
                    prop:disabled=move || {
                        state.query.result.get().is_none() || state.session.web_mode.get() || state.session.demo.get()
                    }
                    title=move || {
                        if state.session.web_mode.get() || state.session.demo.get() {
                            "Excel export needs a live query in the desktop app"
                        } else {
                            ""
                        }
                    }
                    on:click=move |_| {
                        spawn_local(async move {
                            match api::export_patches().await {
                                Ok(Some(p)) => state.notify(Toast::ok(format!("Exported to {p}"))),
                                Ok(None) => {}
                                Err(e) => state.notify(Toast::err(e)),
                            }
                        });
                    }
                >
                    "Export to Excel"
                </button>
                <button
                    class="btn"
                    prop:disabled=move || {
                        state.query.result.get().is_none() || state.session.web_mode.get() || state.session.demo.get()
                    }
                    title=move || {
                        if state.session.web_mode.get() || state.session.demo.get() {
                            "The HTML report needs a live query in the desktop app"
                        } else {
                            ""
                        }
                    }
                    on:click=move |_| {
                        spawn_local(async move {
                            match api::export_report().await {
                                Ok(Some(p)) => {
                                    state.notify(Toast::ok(format!("Report saved to {p}")))
                                }
                                Ok(None) => {}
                                Err(e) => state.notify(Toast::err(e)),
                            }
                        });
                    }
                >
                    "Export report"
                </button>
                <Show when=move || state.run.refreshing.get()>
                    <span class="chips-label">"↻ refreshing…"</span>
                </Show>
                <label class="inline">
                    "Auto-refresh"
                    <select on:change=move |ev| {
                        state.run.refresh_secs.set(event_target_value(&ev).parse().unwrap_or(0))
                    }>
                        {[("0", "Off"), ("30", "30s"), ("60", "1m"), ("300", "5m"), ("900", "15m")]
                            .into_iter()
                            .map(|(val, label)| {
                                let sel = move || state.run.refresh_secs.get().to_string() == val;
                                view! {
                                    <option value=val selected=sel>
                                        {label}
                                    </option>
                                }
                            })
                            .collect_view()}
                    </select>
                </label>
                <button
                    class="btn"
                    prop:disabled=move || {
                        state.run.busy.get() || state.run.refreshing.get() || state.session.web_mode.get()
                            || state.session.demo.get() || state.query.result.get().is_none()
                    }
                    title="Refetch live patch data from NinjaOne for the current filter"
                    on:click=move |_| state.refresh_now()
                >
                    "↻ Refresh"
                </button>
                <Show when=move || state.query.result.get().is_some()>
                    <span class="chips-label">
                        {move || {
                            state.query.result
                                .get()
                                .map(|r| format!("patch data as of {}", r.data_fetched_at))
                                .unwrap_or_default()
                        }}
                    </span>
                </Show>
                <PresetRow/>
            </div>
            <Show when=move || state.run.busy.get()>
                <div class="query-progress">
                    <div class="progress">
                        {move || match state.run.progress_estimate() {
                            Some(p) => {
                                view! {
                                    <div
                                        class="progress-bar"
                                        style=format!("width:{:.1}%", p * 100.0)
                                    ></div>
                                }
                                    .into_any()
                            }
                            None => {
                                view! { <div class="progress-bar progress-indeterminate"></div> }
                                    .into_any()
                            }
                        }}
                    </div>
                    <span class="progress-label">
                        {move || {
                            let p = state.run.progress.get();
                            let secs = state.run.elapsed_secs();
                            if p.joining {
                                format!("Running… {secs:.0}s · computing rollups…")
                            } else {
                                let n = p.records();
                                if n > 0 {
                                    format!(
                                        "Running… {secs:.0}s · loaded {} records",
                                        group_thousands(n),
                                    )
                                } else {
                                    format!("Running… {secs:.0}s")
                                }
                            }
                        }}
                    </span>
                </div>
            </Show>
            <Show when=move || {
                !state.run.busy.get() && state.run.last_duration_ms.get().is_some()
            }>
                <p class="query-hint">
                    {move || {
                        format!(
                            "Last run took {:.0}s",
                            state.run.last_duration_ms.get().unwrap_or(0.0) / 1000.0,
                        )
                    }}
                </p>
            </Show>
        </section>
    }
}

#[component]
fn PresetRow() -> impl IntoView {
    let state = expect_context::<AppState>();

    let save_preset = move |_| {
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
        spawn_local(async move {
            match api::save_preset(preset).await {
                Ok(p) => {
                    state.settings.presets.set(p);
                    state.settings.preset_name.set(String::new());
                    state.notify(Toast::ok("Preset saved"));
                }
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    };

    view! {
        <div class="row presets">
            <span class="chips-label">"Presets:"</span>
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
                            <span class="chip chip-preset">
                                <button
                                    class="link"
                                    on:click=move |_| state.apply_preset(p2.clone())
                                >
                                    {name}
                                </button>
                                <button
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
                                            if let Ok(p) = api::delete_preset(n).await {
                                                state.settings.presets.set(p);
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
            <input
                class="preset-name"
                placeholder="Preset name"
                prop:value=move || state.settings.preset_name.get()
                on:input=move |ev| state.settings.preset_name.set(event_target_value(&ev))
            />
            <button class="btn btn-ghost" on:click=save_preset>
                "Save preset"
            </button>
        </div>
    }
}
