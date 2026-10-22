use leptos::task::spawn_local;

use super::menu::MenuButton;
use super::*;

/// The run cluster in the top bar: Run query, what the data on screen is and how
/// old, Refresh, and the auto-refresh cadence. "Last run took Ns" and the result's
/// generation time moved into the data line's tooltip: two timestamps side by side
/// (data fetched, result generated) read as one said twice.
#[component]
pub(crate) fn RunControls() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <div class="run-controls">
            // Enabled during an auto-refresh on purpose: a click then queues behind
            // it (see `util::run_decision`) and says so.
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
            <span class="run-status" role="status">
                {move || {
                    if state.run.busy.get() {
                        return progress_label(state);
                    }
                    if state.run.refreshing.get() {
                        return "↻ refreshing…".to_string();
                    }
                    state
                        .query
                        .result
                        .with(|r| r.as_ref().map(|r| format!("Data as of {}", r.data_fetched_at)))
                        .unwrap_or_default()
                }}
            </span>
            <button
                class="btn btn-ghost btn-icon"
                aria-label="Refresh from NinjaOne"
                prop:disabled=move || {
                    state.run.busy.get() || state.run.refreshing.get() || state.session.web_mode.get()
                        || state.session.demo.get() || state.query.result.get().is_none()
                }
                title=move || {
                    let base = "Refetch live patch data from NinjaOne for the current filter";
                    state.query.result.with(|r| match r {
                        Some(r) => format!(
                            "{base}. These results were generated {}{}.",
                            r.generated_at,
                            state
                                .run
                                .last_duration_ms
                                .get()
                                .map(|ms| format!("; the last run took {:.0}s", ms / 1000.0))
                                .unwrap_or_default(),
                        ),
                        None => base.to_string(),
                    })
                }
                on:click=move |_| state.refresh_now()
            >
                "↻"
            </button>
            <label class="inline">
                "Auto-refresh"
                <select on:change=move |ev| {
                    state.set_refresh_cadence(event_target_value(&ev).parse().unwrap_or(0))
                }>
                    // No sub-minute cadence: a tick re-pages the whole-fleet patch
                    // feeds, which on a large tenant takes longer than 30s to finish
                    // — the option only ever produced a back-to-back download loop.
                    // The backend enforces its own floor regardless
                    // (`FORCE_MIN_INTERVAL`).
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
        </div>
    }
}

/// "Running… 12s · loaded 4,210 records", or the rollup stage.
fn progress_label(state: AppState) -> String {
    let p = state.run.progress.get();
    let secs = state.run.elapsed_secs();
    if p.joining {
        format!("Running… {secs:.0}s · computing rollups…")
    } else {
        let n = p.records();
        if n > 0 {
            format!(
                "Running… {secs:.0}s · loaded {} records",
                group_thousands(n)
            )
        } else {
            format!("Running… {secs:.0}s")
        }
    }
}

/// A thin progress line along the top bar's bottom edge while a query runs —
/// estimated from the last run's duration when there is one, indeterminate
/// otherwise.
#[component]
pub(crate) fn QueryProgressLine() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <Show when=move || state.run.busy.get()>
            <div class="topbar-progress" aria-hidden="true">
                {move || match state.run.progress_estimate() {
                    Some(p) => {
                        view! {
                            <div class="progress-bar" style=format!("width:{:.1}%", p * 100.0)></div>
                        }
                            .into_any()
                    }
                    None => {
                        view! { <div class="progress-bar progress-indeterminate"></div> }
                            .into_any()
                    }
                }}
            </div>
        </Show>
    }
}

/// The three exports of the cached result, in one menu. One save dialog at a time,
/// shared by every export: each opens a native Save dialog, and a double-click used
/// to stack two of them over the same workbook.
#[component]
pub(crate) fn ExportMenu() -> impl IntoView {
    let exporting = RwSignal::new(false);
    view! {
        <MenuButton label=String::from("Export ▾") title=String::from("Export the results on screen")>
            <ExportButton kind=Export::Workbook exporting=exporting/>
            <ExportButton kind=Export::Csv exporting=exporting/>
            <ExportButton kind=Export::Report exporting=exporting/>
        </MenuButton>
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
            Self::Workbook => "Excel workbook",
            Self::Csv => "CSV of the patch rows",
            Self::Report => "HTML report",
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

/// One export, as a menu item. `exporting` is owned by the caller and shared by
/// every export, so a click on either is refused while any Save dialog is open.
#[component]
fn ExportButton(kind: Export, exporting: RwSignal<bool>) -> impl IntoView {
    let state = expect_context::<AppState>();
    let web_only = move || state.session.web_mode.get() || state.session.demo.get();
    view! {
        <button
            type="button"
            class="menu__item"
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

/// The view-link notices under the top bar, each shown only while it has
/// something to say: the code just copied (kept on screen in case the clipboard
/// refused it), the "Open view…" paste field, and a link from another instance
/// waiting for "Apply anyway". Sharing itself lives in the ⋯ menu; these cannot,
/// because a closed menu would hide a code the operator still has to copy and a
/// pending view that holds auto-refresh (`refresh_hold`).
#[component]
pub(crate) fn ViewNotices() -> impl IntoView {
    let state = expect_context::<AppState>();
    let opening = state.ui.opening_view;
    let pasted = RwSignal::new(String::new());
    let paste_field: NodeRef<leptos::html::Input> = NodeRef::new();
    // Focus the paste field as it opens, so "Open view…" then Cmd/Ctrl+V works.
    Effect::new(move |_| {
        if opening.get()
            && let Some(input) = paste_field.get()
        {
            let _ = input.focus();
        }
    });
    let apply = move || {
        let text = pasted.get_untracked();
        state.open_view_link(&text);
        if util::decode_view(&text).is_ok() {
            pasted.set(String::new());
            opening.set(false);
        }
    };
    view! {
        <Show when=move || state.ui.view_code.with(Option::is_some)>
            <div class="notice-row">
                <span class="chips-label">"View link"</span>
                <input
                    class="view-code"
                    readonly
                    aria-label="View link"
                    prop:value=move || state.ui.view_code.get().unwrap_or_default()
                    on:focus=move |ev| select_all(&ev)
                />
                <button
                    class="x"
                    aria-label="Dismiss the view link"
                    on:click=move |_| state.ui.view_code.set(None)
                >
                    "×"
                </button>
            </div>
        </Show>
        <Show when=move || opening.get()>
            <div class="notice-row">
                <input
                    class="view-code"
                    aria-label="Paste a view link or code"
                    placeholder="Paste a view link or code"
                    node_ref=paste_field
                    prop:value=move || pasted.get()
                    on:input=move |ev| pasted.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" {
                            apply();
                        } else if ev.key() == "Escape" {
                            opening.set(false);
                        }
                    }
                />
                <button class="btn" on:click=move |_| apply()>
                    "Apply view"
                </button>
                <button class="btn btn-ghost" on:click=move |_| opening.set(false)>
                    "Cancel"
                </button>
            </div>
        </Show>
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
