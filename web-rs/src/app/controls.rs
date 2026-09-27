use leptos::task::spawn_local;

use super::*;

#[component]
pub(crate) fn RunControls() -> impl IntoView {
    let state = expect_context::<AppState>();
    // One save dialog at a time, shared by both exports: each opens a native Save
    // dialog, and a double-click used to stack two of them over the same workbook.
    let exporting = RwSignal::new(false);

    view! {
        <section class="panel">
            <div class="controls">
                // Enabled during an auto-refresh on purpose: a click then queues
                // behind it (see `util::run_decision`) and says so.
                <button
                    class="btn btn-primary"
                    prop:disabled=move || state.run.busy.get()
                    on:click=move |_| state.run_query()
                >
                    {move || {
                        if state.run.busy.get() {
                            "Running…"
                        } else if state.run.queued.get().is_some() {
                            "Queued…"
                        } else {
                            "Run query"
                        }
                    }}
                </button>
                <ExportButton kind=Export::Workbook exporting=exporting/>
                <ExportButton kind=Export::Csv exporting=exporting/>
                <ExportButton kind=Export::Report exporting=exporting/>
                <Show when=move || state.run.refreshing.get()>
                    <span class="chips-label">"↻ refreshing…"</span>
                </Show>
                <label class="inline">
                    "Auto-refresh"
                    <select on:change=move |ev| {
                        state.set_refresh_cadence(event_target_value(&ev).parse().unwrap_or(0))
                    }>
                        // No sub-minute cadence: a tick re-pages the whole-fleet
                        // patch feeds, which on a large tenant takes longer than
                        // 30s to finish — the option only ever produced a
                        // back-to-back download loop. The backend enforces its own
                        // floor regardless (`FORCE_MIN_INTERVAL`).
                        {[("0", "Off"), ("60", "1m"), ("300", "5m"), ("900", "15m")]
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
                <AutoRefreshStatus/>
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
                // Presets persist through the backend's settings file, which the
                // browser demo does not have: every save or delete could only fail.
                <Show when=move || !state.session.web_mode.get() && !state.session.demo.get()>
                    <PresetRow/>
                </Show>
            </div>
            <ViewLinkRow/>
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

/// The exports of the cached result. All write through a native Save dialog and
/// need a live query in the desktop app; they differ only in the command and the
/// wording.
#[derive(Clone, Copy)]
enum Export {
    Workbook,
    Csv,
    Report,
}

impl Export {
    fn label(self) -> &'static str {
        match self {
            Self::Workbook => "Export to Excel",
            Self::Csv => "Export CSV",
            Self::Report => "Export report",
        }
    }

    /// Tooltip in the browser demo, where there is no backend to export from.
    fn web_title(self) -> &'static str {
        match self {
            Self::Workbook => "Excel export needs a live query in the desktop app",
            Self::Csv => "CSV export needs a live query in the desktop app",
            Self::Report => "The HTML report needs a live query in the desktop app",
        }
    }

    /// Tooltip in the desktop app. Only the CSV needs one: it holds the detail rows
    /// alone, and says where its provenance went.
    fn title(self) -> &'static str {
        match self {
            Self::Csv => {
                "The Patches rows as CSV. The file name records the device scope, \
                 statuses and data times (UTC); use Excel export for the full filter list."
            }
            Self::Workbook | Self::Report => "",
        }
    }

    fn saved(self, path: &str) -> String {
        match self {
            Self::Workbook | Self::Csv => format!("Exported to {path}"),
            Self::Report => format!("Report saved to {path}"),
        }
    }

    /// `Ok(None)` is the operator cancelling the Save dialog.
    async fn save(self) -> Result<Option<String>, String> {
        match self {
            Self::Workbook => api::export_patches().await,
            Self::Csv => api::export_csv().await,
            Self::Report => api::export_report().await,
        }
    }
}

/// One export button. `exporting` is owned by the caller and shared by every
/// export, so a click on either is refused while any Save dialog is open.
#[component]
fn ExportButton(kind: Export, exporting: RwSignal<bool>) -> impl IntoView {
    let state = expect_context::<AppState>();
    let web_only = move || state.session.web_mode.get() || state.session.demo.get();
    view! {
        <button
            class="btn"
            prop:disabled=move || {
                exporting.get() || state.query.result.with(|r| r.is_none()) || web_only()
            }
            title=move || if web_only() { kind.web_title() } else { kind.title() }
            on:click=move |_| {
                if exporting.get_untracked() {
                    return;
                }
                exporting.set(true);
                spawn_local(async move {
                    match kind.save().await {
                        Ok(Some(p)) => state.notify(Toast::ok(kind.saved(&p))),
                        Ok(None) => {}
                        Err(e) => state.notify(Toast::err(e)),
                    }
                    exporting.set(false);
                });
            }
        >
            {kind.label()}
        </button>
    }
}

#[component]
fn PresetRow() -> impl IntoView {
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
            <input
                class="preset-name"
                aria-label="Preset name"
                placeholder="Preset name"
                prop:value=move || state.settings.preset_name.get()
                on:input=move |ev| state.settings.preset_name.set(event_target_value(&ev))
            />
            <button class="btn btn-ghost" prop:disabled=move || saving.get() on:click=save_preset>
                "Save preset"
            </button>
        </div>
    }
}

/// The countdown to the next automatic refresh and its Pause / Resume button.
/// Rendered only while a cadence is set and a refresh could actually run.
#[component]
fn AutoRefreshStatus() -> impl IntoView {
    let state = expect_context::<AppState>();
    let active = move || state.run.refresh_secs.get() > 0 && state.is_authed();
    let paused = move || state.run.refresh_paused.get();
    view! {
        <Show when=active>
            <span class="refresh-countdown">
                {move || {
                    let hold = state.refresh_hold();
                    // A run in flight already has its own indicator.
                    if hold == Some(util::RefreshHold::Running) {
                        return String::new();
                    }
                    util::countdown_label(state.run.refresh_clock.get().remaining_ms, hold)
                }}
            </span>
            <button
                class="btn btn-ghost"
                aria-pressed=move || paused().to_string()
                title="Pause or resume auto-refresh (p)"
                on:click=move |_| state.toggle_refresh_pause()
            >
                {move || if paused() { "Resume" } else { "Pause" }}
            </button>
        </Show>
    }
}

/// Share the current view: filters, results tab, grouping and sort — never the
/// selection or anything about the sign-in. The web demo shares its URL (whose
/// fragment always carries the view); the desktop, which has no URL, shares a
/// code, and applies one pasted into "Open view…".
#[component]
fn ViewLinkRow() -> impl IntoView {
    let state = expect_context::<AppState>();
    let opening = RwSignal::new(false);
    let pasted = RwSignal::new(String::new());
    let apply = move || {
        let text = pasted.get_untracked();
        state.open_view_link(&text);
        if util::decode_view(&text).is_ok() {
            pasted.set(String::new());
            opening.set(false);
        }
    };
    view! {
        <div class="row view-link">
            <button
                class="btn btn-ghost"
                title="Copy a link to these filters, tab, grouping and sort (no selection, no credentials)"
                on:click=move |_| state.copy_view_link()
            >
                "Copy view link"
            </button>
            <button
                class="btn btn-ghost"
                aria-expanded=move || opening.get().to_string()
                on:click=move |_| opening.update(|o| *o = !*o)
            >
                "Open view…"
            </button>
            <Show when=move || state.ui.view_code.with(Option::is_some)>
                <input
                    class="view-code"
                    readonly
                    aria-label="View link"
                    prop:value=move || state.ui.view_code.get().unwrap_or_default()
                    on:focus=move |ev| select_all(&ev)
                />
            </Show>
            <Show when=move || opening.get()>
                <input
                    class="view-code"
                    aria-label="Paste a view link or code"
                    placeholder="Paste a view link or code"
                    prop:value=move || pasted.get()
                    on:input=move |ev| pasted.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" {
                            apply();
                        }
                    }
                />
                <button class="btn" on:click=move |_| apply()>
                    "Apply view"
                </button>
            </Show>
        </div>
        {move || {
            state
                .ui
                .pending_view
                .get()
                .map(|pending| {
                    let view = pending.view.clone();
                    view! {
                        <div class="stale-banner view-link-warning" role="alert">
                            <span>{pending.warning}</span>
                            <button
                                class="link-btn"
                                on:click=move |_| state.apply_view(view.clone())
                            >
                                "Apply anyway"
                            </button>
                            <button
                                class="link-btn"
                                on:click=move |_| state.ui.pending_view.set(None)
                            >
                                "Cancel"
                            </button>
                        </div>
                    }
                })
        }}
    }
}

/// Selects a read-only field's text on focus, so a refused clipboard costs one
/// Ctrl/Cmd+C rather than a careful drag across a long code.
fn select_all(ev: &leptos::ev::FocusEvent) {
    let Some(target) = ev.target() else {
        return;
    };
    let target: &wasm_bindgen::JsValue = target.as_ref();
    if let Ok(select) = js_sys::Reflect::get(target, &wasm_bindgen::JsValue::from_str("select"))
        && let Some(f) = wasm_bindgen::JsCast::dyn_ref::<js_sys::Function>(&select)
    {
        let _ = f.call0(target);
    }
}
