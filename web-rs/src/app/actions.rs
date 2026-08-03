//! Device-action surface: the selection bar above the Patches table, the
//! confirmation modal, and the Jobs tab.
//!
//! Everything here restates guardrails the backend already enforces. That
//! duplication is deliberate — the UI explains *why* an action is unavailable,
//! while `commands::actions` is what actually refuses it. A confirmation dialog
//! that exists only in WASM is not a guardrail.

use leptos::prelude::*;

use super::*;

/// Reboot modes offered in the confirm dialog, as (value, label).
const REBOOT_MODES: [(&str, &str); 2] = [("NORMAL", "Normal"), ("FORCED", "Forced")];

/// Sticky bar above the Patches table. Always renders its running total, because
/// a selection spanning several pages is otherwise invisible.
#[component]
pub(crate) fn ActionBar() -> impl IntoView {
    let state = expect_context::<AppState>();

    let blocked = move || state.blocked_reason();
    let busy = move || state.actions.dispatching.get();
    let counts = move || state.selection_counts();
    let any = move || counts().0 > 0;

    // One disabled reason for every button, so the tooltip always explains itself.
    let disabled_reason = move || {
        if let Some(reason) = blocked() {
            Some(reason)
        } else if !any() {
            Some("Select at least one device first".to_string())
        } else if busy() {
            Some("An action is already being dispatched".to_string())
        } else {
            None
        }
    };

    view! {
        <div class="action-bar">
            <div class="action-bar-summary">
                {move || {
                    let (devices, rows, offline) = counts();
                    if devices == 0 {
                        view! {
                            <span class="action-bar-hint">
                                "Select patch rows to act on their devices."
                            </span>
                        }
                            .into_any()
                    } else {
                        let mut text = format!(
                            "{} device(s) selected · {} patch row(s)",
                            group_thousands(devices),
                            group_thousands(rows),
                        );
                        if offline > 0 {
                            text.push_str(&format!(" · {offline} offline"));
                        }
                        view! {
                            <>
                                <strong>{text}</strong>
                                <button
                                    class="link-btn"
                                    on:click=move |_| state.clear_selection()
                                >
                                    "Clear"
                                </button>
                            </>
                        }
                            .into_any()
                    }
                }}
            </div>
            <div class="action-bar-buttons">
                {ACTION_BUTTONS
                    .iter()
                    .map(|(kind, label)| {
                        let kind = *kind;
                        view! {
                            <button
                                class="btn btn-sm"
                                title=move || disabled_reason().unwrap_or_else(|| kind.label().to_string())
                                prop:disabled=move || disabled_reason().is_some()
                                on:click=move |_| state.open_plan(kind)
                            >
                                {*label}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>

            // Reboot needs its mode and reason chosen before planning: the reason is
            // recorded in NinjaOne's own activity feed, and the backend refuses a
            // reboot without one.
            <Show when=any>
                <div class="action-bar-reboot">
                    <label>
                        "Reboot mode"
                        <select
                            prop:value=move || state.actions.reboot_mode.get()
                            on:change=move |ev| {
                                state.actions.reboot_mode.set(event_target_value(&ev))
                            }
                        >
                            {REBOOT_MODES
                                .iter()
                                .map(|(value, label)| {
                                    view! { <option value=*value>{*label}</option> }
                                })
                                .collect_view()}
                        </select>
                    </label>
                    <label class="action-bar-reason">
                        "Reboot reason"
                        <input
                            type="text"
                            placeholder="e.g. July patch cycle"
                            prop:value=move || state.actions.reason.get()
                            on:input=move |ev| state.actions.reason.set(event_target_value(&ev))
                        />
                    </label>
                    <label class="checkbox">
                        <input
                            type="checkbox"
                            prop:checked=move || state.actions.include_offline.get()
                            on:change=move |ev| {
                                state.actions.include_offline.set(event_target_checked(&ev))
                            }
                        />
                        "Include offline devices"
                    </label>
                </div>
            </Show>
            <Show when=move || blocked().is_some()>
                <p class="action-bar-blocked" role="note">
                    {move || blocked().unwrap_or_default()}
                    <Show when=move || {
                        state
                            .session
                            .auth
                            .with(|a| {
                                a.as_ref().is_some_and(|a| a.authenticated && a.actions_enabled
                                    && !a.write_enabled)
                            })
                    }>
                        <button
                            class="link-btn"
                            on:click=move |_| {
                                leptos::task::spawn_local(async move {
                                    match api::reauthorize().await {
                                        Ok(()) => {
                                            // Storing the fresh status is enough —
                                            // the blocked reason derives from it.
                                            state.session.refresh_auth();
                                            state.notify(Toast::ok("Re-authorized"));
                                        }
                                        Err(e) => state.notify(Toast::err(e)),
                                    }
                                });
                            }
                        >
                            "Re-authorize"
                        </button>
                    </Show>
                </p>
            </Show>
        </div>
    }
}

/// The actions offered, in escalating order of consequence.
const ACTION_BUTTONS: [(ActionKind, &str); 5] = [
    (ActionKind::OsPatchScan, "Scan OS"),
    (ActionKind::SoftwarePatchScan, "Scan software"),
    (ActionKind::OsPatchApply, "Apply OS patches"),
    (ActionKind::SoftwarePatchApply, "Apply software patches"),
    (ActionKind::Reboot, "Reboot…"),
];

/// Confirmation modal. Shows exactly what will happen — including the literal
/// parameter string — and gates a forced reboot behind typing the device count.
#[component]
pub(crate) fn ConfirmActionModal() -> impl IntoView {
    let state = expect_context::<AppState>();

    window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape"
            && state.actions.pending.with_untracked(|p| p.is_some())
            && !state.actions.dispatching.get_untracked()
        {
            state.cancel_plan();
        }
    });

    view! {
        {move || {
            let Some(pending) = state.actions.pending.get() else {
                return ().into_any();
            };
            let plan = pending.plan;
            let kind = pending.request.kind;
            let device_count = plan.eligible.len();
            let blocked = plan.is_blocked();
            // A forced reboot is the one action here with unrecoverable data loss,
            // so it needs more than a click to confirm. Both rules live in `util`
            // so they are host-tested rather than only exercised by clicking.
            let needs_typed =
                util::needs_typed_confirmation(blocked, kind, pending.request.reboot_mode);
            let dry_run = plan.dry_run;
            let expected = device_count.to_string();
            // A `Signal` rather than a bare closure: `Show` renders its children
            // through an `Fn`, and a non-Copy closure captured into one would only
            // be `FnOnce`.
            let can_confirm = Signal::derive({
                let expected = expected.clone();
                move || {
                    state.actions.confirm_input.with(|typed| {
                        util::can_confirm_action(
                            blocked,
                            state.actions.dispatching.get(),
                            needs_typed,
                            typed,
                            &expected,
                        )
                    })
                }
            });

            view! {
                <div class="modal-overlay" role="presentation">
                    <div
                        // A dispatch that will restart machines gets a visual
                        // accent, not just a line of warning text.
                        class=if plan.reboot_expected {
                            "modal modal-danger"
                        } else {
                            "modal"
                        }
                        role="dialog"
                        aria-modal="true"
                        aria-labelledby="confirm-action-title"
                    >
                        <h3 id="confirm-action-title">{plan.summary.clone()}</h3>

                        {(!plan.organizations.is_empty())
                            .then(|| {
                                view! {
                                    <p class="modal-sub">
                                        {format!(
                                            "Organization(s): {}",
                                            plan.organizations.join(", "),
                                        )}
                                    </p>
                                }
                            })}

                        {(!plan.blockers.is_empty())
                            .then(|| {
                                view! {
                                    <ul class="modal-blockers">
                                        {plan
                                            .blockers
                                            .iter()
                                            .map(|b| view! { <li>{b.clone()}</li> })
                                            .collect_view()}
                                    </ul>
                                }
                            })}
                        {(!plan.warnings.is_empty())
                            .then(|| {
                                view! {
                                    <ul class="modal-warnings">
                                        {plan
                                            .warnings
                                            .iter()
                                            .map(|w| view! { <li>{w.clone()}</li> })
                                            .collect_view()}
                                    </ul>
                                }
                            })}

                        // The toolkit never sends a string the operator hasn't seen.
                        {plan
                            .parameters_preview
                            .clone()
                            .filter(|p| !p.is_empty())
                            .map(|params| {
                                view! {
                                    <>
                                        <p class="modal-label">"Parameters sent to the script"</p>
                                        <pre class="modal-params">{params}</pre>
                                    </>
                                }
                            })}

                        // The operator should be able to verify the exact target list
                        // before approving, not just its size.
                        {(!plan.eligible.is_empty())
                            .then(|| {
                                let items = plan
                                    .eligible
                                    .iter()
                                    .map(|t| {
                                        let suffix = if t.offline { " (offline)" } else { "" };
                                        view! {
                                            <li>
                                                {format!(
                                                    "{} — {}{suffix}",
                                                    t.device_name,
                                                    t.organization,
                                                )}
                                            </li>
                                        }
                                    })
                                    .collect_view();
                                view! {
                                    <details class="modal-targets">
                                        <summary>
                                            {format!("{device_count} device(s) targeted")}
                                        </summary>
                                        <ul>{items}</ul>
                                    </details>
                                }
                            })}

                        {(!plan.skipped.is_empty())
                            .then(|| {
                                let items = plan
                                    .skipped
                                    .iter()
                                    .map(|s| {
                                        view! {
                                            <li>{format!("{} — {}", s.device_name, s.reason)}</li>
                                        }
                                    })
                                    .collect_view();
                                view! {
                                    <details class="modal-skipped">
                                        <summary>
                                            {format!("{} device(s) skipped", plan.skipped.len())}
                                        </summary>
                                        <ul>{items}</ul>
                                    </details>
                                }
                            })}

                        <Show when=move || needs_typed>
                            <label class="modal-typed">
                                <span>
                                    {format!(
                                        "Forced reboot discards unsaved work. Type {expected} to confirm.",
                                    )}
                                </span>
                                <input
                                    type="text"
                                    inputmode="numeric"
                                    prop:value=move || state.actions.confirm_input.get()
                                    on:input=move |ev| {
                                        state.actions.confirm_input.set(event_target_value(&ev))
                                    }
                                />
                            </label>
                        </Show>

                        <div class="modal-actions">
                            <button
                                class="btn"
                                prop:disabled=move || state.actions.dispatching.get()
                                on:click=move |_| state.cancel_plan()
                            >
                                "Cancel"
                            </button>
                            <Show when=move || !blocked>
                                <button
                                    class="btn btn-primary"
                                    prop:disabled=move || !can_confirm.get()
                                    on:click=move |_| state.confirm_plan()
                                >
                                    {move || {
                                        if state.actions.dispatching.get() {
                                            match state.actions.dispatch_progress.get() {
                                                Some((sent, total)) => {
                                                    format!("Dispatching {sent}/{total}…")
                                                }
                                                None => "Dispatching…".to_string(),
                                            }
                                        } else if dry_run {
                                            format!("Preview on {device_count} device(s)")
                                        } else {
                                            format!("Run on {device_count} device(s)")
                                        }
                                    }}
                                </button>
                            </Show>
                        </div>
                    </div>
                </div>
            }
                .into_any()
        }}
    }
}

/// Script picker: choose a library script, set its parameters and run-as identity.
#[component]
pub(crate) fn ScriptPicker() -> impl IntoView {
    let state = expect_context::<AppState>();

    let selected_script = move || {
        let id = state.actions.script_id.get();
        state
            .actions
            .scripts
            .with(|list| list.iter().find(|s| Some(s.id) == id).cloned())
    };
    // Per-KB targeting is only honest for a script that declares a kbAllowList;
    // anything else installs whatever the device needs.
    let supports_kb = move || selected_script().is_some_and(|s| s.accepts_kb_allow_list);
    let disabled = move || state.blocked_reason().is_some();

    // Credential roles are a *per-device* property, so this samples the first
    // selected device and offers its roles as suggestions rather than a closed
    // list — a role valid there may not exist on every other target.
    let run_as_roles = RwSignal::new(Vec::<String>::new());
    Effect::new(move |_| {
        let first = state.actions.selected.with(|s| s.keys().next().copied());
        let Some(device_id) = first.filter(|_| state.can_act()) else {
            run_as_roles.set(Vec::new());
            return;
        };
        leptos::task::spawn_local(async move {
            if let Ok(opts) = api::list_run_as_options(device_id).await {
                run_as_roles.set(opts.roles);
            }
        });
    });

    view! {
        <fieldset class="script-picker" prop:disabled=disabled>
            <legend>"Run a script"</legend>

            <label>
                "Script"
                <select
                    prop:value=move || {
                        state.actions.script_id.get().map(|i| i.to_string()).unwrap_or_default()
                    }
                    on:change=move |ev| {
                        state.actions.script_id.set(event_target_value(&ev).parse().ok())
                    }
                >
                    <option value="">
                        {move || {
                            if state.actions.scripts_loading.get() {
                                "Loading…"
                            } else {
                                "Select a script…"
                            }
                        }}
                    </option>
                    {move || {
                        state
                            .actions
                            .scripts
                            .get()
                            .into_iter()
                            .map(|s| {
                                view! { <option value=s.id.to_string()>{s.name}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            </label>

            // What the picked script actually is, so the operator can tell two
            // similarly-named library entries apart before dispatching one.
            {move || {
                selected_script()
                    .map(|s| {
                        let mut meta = s.language.clone().unwrap_or_default();
                        if !s.operating_systems.is_empty() {
                            if !meta.is_empty() {
                                meta.push_str(" \u{00b7} ");
                            }
                            meta.push_str(&s.operating_systems.join(", "));
                        }
                        view! {
                            <p class="script-picker-meta">
                                {s.description.clone().unwrap_or_default()}
                                {(!meta.is_empty())
                                    .then(|| {
                                        view! { <span class="script-picker-tags">{meta}</span> }
                                    })}
                            </p>
                        }
                    })
            }}

            <label>
                "Run as"
                <input
                    type="text"
                    list="run-as-roles"
                    placeholder="system"
                    prop:value=move || state.actions.run_as.get()
                    on:input=move |ev| state.actions.run_as.set(event_target_value(&ev))
                />
                <datalist id="run-as-roles">
                    {move || {
                        run_as_roles
                            .get()
                            .into_iter()
                            .map(|role| view! { <option value=role></option> })
                            .collect_view()
                    }}
                </datalist>
            </label>

            <Show when=supports_kb>
                <label class="checkbox">
                    <input
                        type="checkbox"
                        prop:checked=move || state.actions.use_kb_targeting.get()
                        on:change=move |ev| {
                            state.actions.use_kb_targeting.set(event_target_checked(&ev))
                        }
                    />
                    "Target only the selected KBs"
                </label>
            </Show>

            <label class="checkbox">
                <input
                    type="checkbox"
                    prop:checked=move || {
                        state.actions.script_reboot.get() == RebootChoice::Auto
                    }
                    on:change=move |ev| {
                        state
                            .actions
                            .script_reboot
                            .set(
                                if event_target_checked(&ev) {
                                    RebootChoice::Auto
                                } else {
                                    RebootChoice::Never
                                },
                            )
                    }
                />
                "Reboot the device after installing"
            </label>

            <label class="script-picker-params">
                "Parameters (sent verbatim; leave empty to compose from the selection)"
                <textarea
                    rows="2"
                    prop:value=move || state.actions.script_params.get()
                    on:input=move |ev| state.actions.script_params.set(event_target_value(&ev))
                ></textarea>
            </label>

            <label class="checkbox">
                <input
                    type="checkbox"
                    prop:checked=move || state.actions.dry_run.get()
                    on:change=move |ev| state.actions.dry_run.set(event_target_checked(&ev))
                />
                "Dry run (preview — the script reports what it would install)"
            </label>

            <button
                class="btn btn-primary btn-sm"
                prop:disabled=move || {
                    state.actions.script_id.with(|s| s.is_none())
                        || state.actions.dispatching.get()
                }
                on:click=move |_| state.open_plan(ActionKind::Script)
            >
                "Run script…"
            </button>
        </fieldset>
    }
}

/// Jobs tab: what has been dispatched and how it ended.
#[component]
pub(crate) fn JobsTable() -> impl IntoView {
    let state = expect_context::<AppState>();

    view! {
        <div class="jobs">
            <Show when=move || state.blocked_reason().is_some()>
                <p class="empty" role="note">
                    {move || state.blocked_reason().unwrap_or_default()}
                </p>
            </Show>

            <ScriptPicker/>

            <div class="jobs-toolbar">
                <button class="btn btn-sm" on:click=move |_| state.refresh_jobs()>
                    "Refresh"
                </button>
                <button
                    class="btn btn-sm"
                    prop:disabled=move || state.actions.jobs.with(Vec::is_empty)
                    on:click=move |_| state.clear_job_history()
                >
                    "Clear history"
                </button>
            </div>

            {move || {
                let jobs = state.actions.jobs.get();
                if jobs.is_empty() {
                    return view! {
                        <p class="empty">
                            "No actions dispatched yet. Select patch rows, then choose an action."
                        </p>
                    }
                        .into_any();
                }
                view! {
                    <div class="table-wrap">
                        <table class="data-table">
                            <thead>
                                <tr>
                                    <th scope="col">"Device"</th>
                                    <th scope="col">"Organization"</th>
                                    <th scope="col">"Action"</th>
                                    <th scope="col">"Mode"</th>
                                    <th scope="col">"Status"</th>
                                    <th scope="col">"Exit"</th>
                                    <th scope="col">"Dispatched"</th>
                                    <th scope="col">"Duration"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {jobs
                                    .into_iter()
                                    .rev()
                                    .map(|j| {
                                        view! {
                                            <tr>
                                                <td>{j.device_name.clone()}</td>
                                                <td>{j.organization.clone()}</td>
                                                <td>{j.detail.clone()}</td>
                                                <td>{job_mode_label(&j)}</td>
                                                <td>
                                                    // NinjaOne v2 exposes no script
                                                    // output, so the correlator is
                                                    // how you find the run in the
                                                    // NinjaOne console.
                                                    <span
                                                        class=j.state.css_class()
                                                        title=j.correlator()
                                                    >
                                                        {j.state.label()}
                                                    </span>
                                                </td>
                                                <td>
                                                    {j
                                                        .exit_code
                                                        .map(|c| c.to_string())
                                                        .unwrap_or_default()}
                                                </td>
                                                <td>{j.dispatched_at.clone()}</td>
                                                <td>{format_duration(j.duration_seconds)}</td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()}
                            </tbody>
                        </table>
                    </div>
                }
                    .into_any()
            }}
        </div>
    }
}
