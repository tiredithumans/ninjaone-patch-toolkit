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
    let disabled_reason = move || action_disabled_reason(blocked(), counts().0, busy());

    view! {
        <div class="action-bar">
            <div class="action-bar-summary">
                {move || {
                    let (devices, rows, offline) = counts();
                    let Some(text) = selection_summary(devices, rows, offline) else {
                        return view! {
                            <span class="action-bar-hint">
                                "Select patch rows to act on their devices."
                            </span>
                        }
                            .into_any();
                    };
                    {
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
                {ACTION_GROUPS
                    .iter()
                    .map(|(heading, kinds)| {
                        view! {
                            <div class="action-group">
                                <span class="action-group-label">{*heading}</span>
                                {kinds
                                    .iter()
                                    .map(|(kind, label)| {
                                        let kind = *kind;
                                        // Two reasons stack: the ones that block every
                                        // action, then the ones specific to this kind
                                        // (no remediation script, nothing of its family
                                        // ticked). The tooltip always says which.
                                        let why = move || {
                                            disabled_reason()
                                                .or_else(|| {
                                                    util::kind_disabled_reason(
                                                        kind,
                                                        state.remediation_script_configured(kind),
                                                        state.remediation_targets(kind).len(),
                                                    )
                                                })
                                        };
                                        view! {
                                            <button
                                                class="btn btn-sm"
                                                title=move || {
                                                    why().unwrap_or_else(|| kind.label().to_string())
                                                }
                                                prop:disabled=move || why().is_some()
                                                on:click=move |_| state.open_plan(kind)
                                            >
                                                {*label}
                                            </button>
                                        }
                                    })
                                    .collect_view()}
                            </div>
                        }
                    })
                    .collect_view()}
                <div class="action-group">
                    <span class="action-group-label">"Restart"</span>
                    <button
                        class="btn btn-sm"
                        title=move || {
                            disabled_reason()
                                .unwrap_or_else(|| ActionKind::Reboot.label().to_string())
                        }
                        prop:disabled=move || disabled_reason().is_some()
                        on:click=move |_| state.open_plan(ActionKind::Reboot)
                    >
                        "Reboot…"
                    </button>
                </div>
            </div>

            // Which patches "Install only the selected patches" would actually send,
            // per device. The distinction between the two Install rows is only
            // trustworthy if the operator can see the target list it derives from.
            <Show when=any>
                <div class="action-bar-targets">
                    {move || {
                        [ActionKind::OsPatchRemediate, ActionKind::SoftwarePatchRemediate]
                            .into_iter()
                            .filter_map(|kind| {
                                let targets = state.remediation_targets(kind);
                                let summary = util::remediation_summary(kind, &targets)?;
                                Some(view! { <span class="action-target-chip">{summary}</span> })
                            })
                            .collect_view()
                    }}
                </div>
            </Show>

            // Everything below applies to whichever action is dispatched next, so it
            // is rendered once. These used to be split across the action bar and the
            // Jobs tab's script picker, bound to the same signals — so ticking "Dry
            // run" one tab away silently changed what an Apply button would do.
            <Show when=any>
                <div class="action-bar-options">
                    <span class="action-options-label">"Applies to every action"</span>
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

                // The native endpoints take no parameters, have no preview mode (a dry
                // run of one is a `plan()` blocker) and run as NinjaOne's agent, so
                // these three reach only the script-driven actions. Saying which is
                // what stops "Dry run" from reading as protection it isn't giving.
                <div class="action-bar-options">
                    <span class="action-options-label">
                        "Applies to installs of selected patches, and to scripts"
                    </span>
                    <label class="action-bar-runas">
                        "Run as"
                        <input
                            type="text"
                            list="run-as-roles"
                            placeholder="system"
                            prop:value=move || state.actions.run_as.get()
                            on:input=move |ev| state.actions.run_as.set(event_target_value(&ev))
                        />
                    </label>
                    <RunAsRoles/>
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
                        "Restart the device after installing"
                    </label>
                    <label class="checkbox">
                        <input
                            type="checkbox"
                            prop:checked=move || state.actions.dry_run.get()
                            on:change=move |ev| state.actions.dry_run.set(event_target_checked(&ev))
                        />
                        "Dry run (the script reports what it would install)"
                    </label>
                </div>

                // Reboot needs its mode and reason chosen before planning: the reason
                // is recorded in NinjaOne's own activity feed, and the backend refuses
                // a reboot without one.
                <div class="action-bar-options">
                    <span class="action-options-label">"Applies to Reboot"</span>
                    <label>
                        "Mode"
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
                        "Reason"
                        <input
                            type="text"
                            placeholder="e.g. July patch cycle"
                            prop:value=move || state.actions.reason.get()
                            on:input=move |ev| state.actions.reason.set(event_target_value(&ev))
                        />
                    </label>
                </div>

                // Collapsed: the two "Install only the selected patches" buttons cover
                // the common case, and this is the escape hatch for any other library
                // script. It lives here rather than in the Jobs tab so that dispatch
                // happens where the selection it targets is visible.
                <details class="action-bar-script">
                    <summary>"Run a library script…"</summary>
                    <ScriptPicker/>
                </details>
            </Show>
            <Show when=move || blocked().is_some()>
                <p class="action-bar-blocked" role="note">
                    {move || blocked().unwrap_or_default()}
                    <ReauthorizeLink/>
                </p>
            </Show>
        </div>
    }
}

/// The one way out of a read-only grant once actions are enabled: a fresh OAuth
/// consent that requests the `management` scope. Renders nothing unless that is
/// the situation.
///
/// Rendered wherever the read-only verdict is stated — the action bar, the Jobs
/// tab's blocked note and the Settings hint that tells the operator to
/// "Re-authorize". It used to exist only in the action bar, which sits inside the
/// Patches table and renders only once a query has returned rows, so an operator
/// who enabled actions and read "read-only sign-in — Re-authorize…" had nowhere to
/// click until a populated query ran.
#[component]
pub(crate) fn ReauthorizeLink() -> impl IntoView {
    let state = expect_context::<AppState>();
    let needed = move || {
        state.session.auth.with(|a| {
            a.as_ref()
                .is_some_and(|a| a.authenticated && a.actions_enabled && !a.write_enabled)
        })
    };
    view! {
        <Show when=needed>
            <button
                class="link-btn"
                on:click=move |_| {
                    leptos::task::spawn_local(async move {
                        match api::reauthorize().await {
                            Ok(()) => {
                                // The backend dropped the session's result and
                                // jobs for the fresh grant; storing the new status
                                // is what clears the blocked reason.
                                state.clear_session();
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
    }
}

/// The actions offered, grouped so the choice the operator actually has to make —
/// *all* approved patches vs *only* the ones they ticked — is the visible one.
///
/// The two are genuinely different mechanisms: "all" is NinjaOne's native apply
/// endpoint, which has no per-patch variant, and "selected" is a library script that
/// receives a target list. Presenting them as one "Apply" button meant the ticked
/// rows looked like they narrowed an apply that in fact ignored them.
const ACTION_GROUPS: [(&str, &[(ActionKind, &str)]); 3] = [
    (
        "Scan",
        &[
            (ActionKind::OsPatchScan, "OS"),
            (ActionKind::SoftwarePatchScan, "Software"),
        ],
    ),
    (
        "Install all approved patches",
        &[
            (ActionKind::OsPatchApply, "OS"),
            (ActionKind::SoftwarePatchApply, "Software"),
        ],
    ),
    (
        "Install only the selected patches",
        &[
            (ActionKind::OsPatchRemediate, "OS"),
            (ActionKind::SoftwarePatchRemediate, "Software"),
        ],
    ),
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
            // Created per dialog, so its cleanup (focus back to the opener) runs
            // when this plan's view is dropped, not when the component is.
            let (dialog, on_tab) = modal::focus_trap();
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
                        tabindex="-1"
                        node_ref=dialog
                        on:keydown=move |ev| on_tab(&ev)
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
                                // Both script paths target per device, so the preview
                                // is one line per device whenever the strings differ.
                                // Keyed off the rendering rather than the kind: a
                                // batch where every device happens to get the same
                                // string collapses to one line, and calling that
                                // "per device" would be misleading.
                                let per_device = params.contains('\n');
                                view! {
                                    <>
                                        <p class="modal-label">
                                            {if per_device {
                                                "Parameters sent to each device"
                                            } else {
                                                "Parameters sent to the script"
                                            }}
                                        </p>
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

/// Suggestions for the shared "Run as" field, as a bare `<datalist>`.
///
/// Its own component because the input it feeds now lives in the action bar's shared
/// options while the fetch is driven by the selection — and because credential roles
/// are a *per-device* property, so this samples the first selected device and offers
/// its roles as suggestions rather than a closed list: a role valid there may not
/// exist on every other target.
#[component]
fn RunAsRoles() -> impl IntoView {
    let state = expect_context::<AppState>();
    let roles = RwSignal::new(Vec::<String>::new());

    Effect::new(move |_| {
        let first = state.actions.selected.with(|s| s.keys().next().copied());
        let Some(device_id) = first.filter(|_| state.can_act()) else {
            roles.set(Vec::new());
            return;
        };
        leptos::task::spawn_local(async move {
            match api::list_run_as_options(device_id).await {
                Ok(opts) => roles.set(opts.roles),
                // Every other call in this file routes failures through a Toast.
                // Swallowing this one left an empty "Run as" datalist that looked
                // exactly like a device with no configured credentials, so the
                // operator had no way to tell a failed lookup from a real answer.
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    });

    view! {
        <datalist id="run-as-roles">
            {move || {
                roles
                    .get()
                    .into_iter()
                    .map(|role| view! { <option value=role></option> })
                    .collect_view()
            }}
        </datalist>
    }
}

/// Script picker: choose a library script and set its parameters.
///
/// Deliberately *not* a self-contained dispatch surface. Run-as, the reboot behavior
/// and the dry-run flag are shared with the "Install only the selected patches"
/// actions and rendered once above it — they mean the same thing for any
/// script-driven dispatch, and duplicating them here is what let the two disagree.
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

    view! {
        <fieldset class="script-picker" prop:disabled=disabled>
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
                // Same per-device targeting the remediation actions use, so the
                // operator can see it is the ticked rows and not their union.
                <Show when=move || state.actions.use_kb_targeting.get()>
                    <p class="script-picker-targets">
                        {move || util::kb_targeting_summary(&state.script_kb_targets())}
                    </p>
                </Show>
            </Show>

            <label class="script-picker-params">
                "Parameters (sent verbatim to every device; leave empty to compose from the selection)"
                <textarea
                    rows="2"
                    prop:value=move || state.actions.script_params.get()
                    on:input=move |ev| state.actions.script_params.set(event_target_value(&ev))
                ></textarea>
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
                    <ReauthorizeLink/>
                </p>
            </Show>

            // Dispatch lives in the action bar on the Patches tab, next to the
            // selection it targets. This tab is history.
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
                            "No actions dispatched yet. Select patch rows on the Patches tab, then choose an action from the bar above the table."
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
