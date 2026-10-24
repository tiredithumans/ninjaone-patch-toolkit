//! Device-action surface: the selection bar above the Patches table, the
//! confirmation modal, and the Jobs tab.
//!
//! Everything here restates guardrails the backend already enforces. That
//! duplication is deliberate — the UI explains *why* an action is unavailable,
//! while `commands::actions` is what actually refuses it. A confirmation dialog
//! that exists only in WASM is not a guardrail.

use super::menu::Popover;
use super::*;

/// Reboot modes offered in the confirm dialog, as (value, label).
const REBOOT_MODES: [(&str, &str); 2] = [("NORMAL", "Normal"), ("FORCED", "Forced")];

/// The bar above the Patches table — and above the Needs Reboot table, where it
/// dispatches against a device-level selection. One row whatever is selected: the
/// running total, the action groups, the run options behind an Options button that
/// says what they will do, and the script picker behind its own button. It used to
/// grow by ~300px of option rows on the first tick, pushing the ticked row out of
/// view and, once scrolled, covering ~40% of the window.
///
/// One component on both tabs rather than a second surface: the same buttons, the
/// same run-option signals, the same plan → confirm path. `source` only decides
/// which selection it reads and which kinds that selection can reach; from the
/// device-level one, everything needing patch rows is disabled with the reason, and
/// the script-only options (Run as, restart, Dry run, the script picker) are not
/// rendered because nothing reachable from there reads them.
#[component]
pub(crate) fn ActionBar(#[prop(optional)] source: SelectionSource) -> impl IntoView {
    let state = expect_context::<AppState>();

    let blocked = move || state.blocked_reason();
    let busy = move || state.actions.dispatching.get();
    let counts = move || state.selection_counts_for(source);
    let any = move || counts().0 > 0;
    let rows_source = source == SelectionSource::PatchRows;

    // One disabled reason for every button, so the tooltip always explains itself.
    let disabled_reason = move || action_disabled_reason(blocked(), source, counts().0, busy());

    // The per-device targets of the two remediation kinds, computed once per
    // selection change and shared by their buttons and the target count rather
    // than walked again for every button prop. Only meaningful for a remediation
    // kind; the callers check that first.
    let os_targets = Memo::new(move |_| state.remediation_targets(ActionKind::OsPatchRemediate));
    let sw_targets =
        Memo::new(move |_| state.remediation_targets(ActionKind::SoftwarePatchRemediate));
    let targets_of = move |kind: ActionKind| {
        if kind.is_os_family() {
            os_targets
        } else {
            sw_targets
        }
    };
    // Why `kind`'s button is disabled, stacked in `util::action_button_reason`'s
    // pinned order; one Memo per button, so its title and its disabled prop share
    // a single evaluation per change.
    let reason_for = move |kind: ActionKind| {
        Memo::new(move |_| {
            util::action_button_reason(
                disabled_reason(),
                source,
                kind,
                || state.remediation_script_configured(kind),
                || targets_of(kind).with(|t| t.len()),
                || state.dry_run_reason(kind),
            )
        })
    };
    let full_label = move |kind: ActionKind| format!("{}. {}", kind.label(), kind.blast_radius());

    view! {
        <div class="action-bar">
            <div class="action-bar-summary">
                {move || {
                    let (devices, rows, offline) = counts();
                    let summary = if rows_source {
                        selection_summary(devices, rows, offline)
                    } else {
                        util::device_selection_summary(devices, offline)
                    };
                    let Some(text) = summary else {
                        return view! {
                            <span class="action-bar-hint">
                                {if rows_source {
                                    "Select patch rows to act on their devices."
                                } else {
                                    "Select devices to reboot or scan them."
                                }}
                            </span>
                        }
                            .into_any();
                    };
                    view! {
                        <>
                            <strong>{text}</strong>
                            <button
                                class="link-btn"
                                on:click=move |_| state.clear_selection_for(source)
                            >
                                "Clear"
                            </button>
                        </>
                    }
                        .into_any()
                }}
            </div>
            {ACTION_GROUPS
                .iter()
                .map(|(heading, kinds)| {
                    let heading = *heading;
                    // "Apply selected" is only trustworthy if the operator can see
                    // what it would send: the distinct patches per family, with the
                    // per-device detail on hover and a marker for ticked OS rows it
                    // cannot send. A device-level selection has no targets.
                    let selected_group = rows_source
                        && kinds.iter().any(|(k, _)| *k == ActionKind::OsPatchRemediate);
                    let target_count = move || {
                        (selected_group && any())
                            .then(|| {
                                os_targets.with(|os| sw_targets.with(|sw| util::remediation_short(os, sw)))
                            })
                            .flatten()
                    };
                    let target_detail = move || {
                        [ActionKind::OsPatchRemediate, ActionKind::SoftwarePatchRemediate]
                            .into_iter()
                            .filter_map(|k| targets_of(k).with(|t| util::remediation_summary(k, t)))
                            .collect::<Vec<_>>()
                            .join("; ")
                    };
                    let untargetable = move || {
                        selected_group
                            .then(|| state.actions.selected.with(util::untargetable_os_note))
                            .flatten()
                    };
                    view! {
                        <div class="action-group" role="group" aria-label=heading>
                            <span class="action-group-label" title=target_detail>
                                {heading}
                                {move || target_count().map(|c| format!(" ({c})"))}
                            </span>
                            {move || {
                                untargetable()
                                    .map(|note| {
                                        view! {
                                            <span class="action-group-warn" title=note.clone() aria-label=note>
                                                "⚠"
                                            </span>
                                        }
                                    })
                            }}
                            {kinds
                                .iter()
                                .map(|(kind, label)| {
                                    let (kind, label) = (*kind, *label);
                                    let why = reason_for(kind);
                                    let title = move || why.get().unwrap_or_else(|| full_label(kind));
                                    if kind == ActionKind::Reboot {
                                        return view! {
                                            <RebootPopover source=source why=why label=label/>
                                        }
                                            .into_any();
                                    }
                                    view! {
                                        <button
                                            // The two untargeted applies reach past the
                                            // selection, so they read differently from
                                            // every other button rather than relying on
                                            // the heading beside them.
                                            class=if kind.exceeds_selection() {
                                                "btn btn-sm btn-wide-radius"
                                            } else {
                                                "btn btn-sm"
                                            }
                                            // The accessible name is the full sentence,
                                            // so a screen reader says what the button
                                            // reaches instead of just "OS", and the
                                            // tooltip agrees.
                                            aria-label=move || full_label(kind)
                                            title=title
                                            prop:disabled=move || why.with(Option::is_some)
                                            on:click=move |_| state.open_plan(kind, source)
                                        >
                                            {label}
                                        </button>
                                    }
                                        .into_any()
                                })
                                .collect_view()}
                        </div>
                    }
                })
                .collect_view()}
            <RunOptions source=source/>
            // The escape hatch for any other library script, in its own dialog. It
            // stays on this bar so dispatch happens where the selection it targets
            // is visible.
            <Show when=move || rows_source>
                <button
                    type="button"
                    class="btn btn-sm btn-ghost"
                    prop:disabled=move || blocked().is_some()
                    on:click=move |_| state.actions.script_dialog.set(true)
                >
                    "Run a script…"
                </button>
            </Show>
            <Show when=move || blocked().is_some()>
                <p class="action-bar-blocked" role="note">
                    {move || blocked().unwrap_or_default()}
                    <ReauthorizeLink/>
                    <EnableActionsLink/>
                </p>
            </Show>
        </div>
    }
}

/// The run options, rendered once, behind a button whose label says what they will
/// do to the next dispatch ("Options: Dry run", warn-coloured at "Live"). They used
/// to be three always-open rows that appeared under the bar on the first tick.
///
/// Each row is labelled with the actions it reaches: the native endpoints take no
/// parameters, have no preview mode and run as NinjaOne's agent, so an unlabelled
/// "Dry run" beside them reads as protection it isn't giving. The panel stays
/// mounted while closed (`menu::Popover`): `RunAsSelect` fetches the sampled
/// device's roles as it mounts, and remounting on every open refetched them.
#[component]
fn RunOptions(source: SelectionSource) -> impl IntoView {
    let state = expect_context::<AppState>();
    let rows_source = source == SelectionSource::PatchRows;
    let open = RwSignal::new(false);
    let label = Memo::new(move |_| {
        util::options_label(
            rows_source,
            state.actions.dry_run.get(),
            state.actions.override_window.get()
                && state.settings.f_actions.with(util::window_override_offered),
            state.actions.include_offline.get()
                && state.settings.f_actions.with(util::offline_targets_offered),
        )
    });
    view! {
        <Popover
            label=Signal::derive(move || label.get().0)
            class=Signal::derive(move || {
                if label.get().1 { "btn-sm options-live".to_string() } else { "btn-sm".to_string() }
            })
            title=Signal::derive(|| "Options for the next dispatch".to_string())
            disabled=Signal::derive(move || state.blocked_reason().is_some())
            open=open
        >
            <div class="action-bar-options">
                <span class="action-options-label">"Applies to every action"</span>
                // Disabled rather than hidden while Settings forbids offline targets:
                // the backend would skip them whatever this says, and the note says
                // where the switch is.
                <label class="checkbox">
                    <input
                        type="checkbox"
                        prop:disabled=move || !state.settings.f_actions.with(util::offline_targets_offered)
                        prop:checked=move || {
                            state.actions.include_offline.get()
                                && state.settings.f_actions.with(util::offline_targets_offered)
                        }
                        on:change=move |ev| state.actions.include_offline.set(event_target_checked(&ev))
                    />
                    "Include offline devices"
                    <Show when=move || !state.settings.f_actions.with(util::offline_targets_offered)>
                        <span class="action-options-note">
                            "(switched off in "
                            <SettingsLink label="Settings → Patch actions"/>
                            ")"
                        </span>
                    </Show>
                </label>
                // Rendered only while the window is enforced and Settings permits
                // overriding it; the backend decides whether the window is actually
                // closed, and audits the dispatch only when it was. Cleared after
                // every dispatch and when the session ends, so it never outlives the
                // one it was ticked for.
                <Show when=move || state.settings.f_actions.with(util::window_override_offered)>
                    <label
                        class="checkbox"
                        title="Only needed outside the window. The backend checks the window at plan and again at confirm, and records an override on the audit trail."
                    >
                        <input
                            type="checkbox"
                            prop:checked=move || state.actions.override_window.get()
                            on:change=move |ev| state.actions.override_window.set(event_target_checked(&ev))
                        />
                        "Override the maintenance window for this dispatch"
                        <span class="action-options-note">
                            {move || {
                                format!(
                                    "(window: {}, this computer's local time)",
                                    state.settings.f_actions.with(util::window_summary),
                                )
                            }}
                        </span>
                    </label>
                </Show>
            </div>
            // Not rendered for a device-level selection: none of the kinds it can
            // reach reads them.
            <Show when=move || rows_source>
                <div class="action-bar-options">
                    <span class="action-options-label">
                        "Applies to installs of selected patches, and to scripts"
                    </span>
                    <label class="action-bar-runas">
                        "Run as"
                        <RunAsSelect/>
                    </label>
                    <label class="checkbox">
                        <input
                            type="checkbox"
                            prop:checked=move || state.actions.script_reboot.get() == RebootChoice::Auto
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
                        "Restart afterwards if a restart is pending"
                    </label>
                    <label class="checkbox">
                        <input
                            type="checkbox"
                            prop:checked=move || state.actions.dry_run.get()
                            on:change=move |ev| state.actions.dry_run.set(event_target_checked(&ev))
                        />
                        "Dry run (installs nothing; the script lists what it would install in its NinjaOne activity)"
                    </label>
                    // A dry run is only real for a script that reads `dryRun`; say
                    // which of the scripts this checkbox reaches cannot, rather than
                    // leaving it to a blocker after the operator clicks.
                    {move || {
                        state
                            .actions
                            .dry_run
                            .get()
                            .then(|| util::dry_run_caveat(&state.dry_run_scripts()))
                            .flatten()
                            .map(|note| view! { <span class="action-options-note">{note}</span> })
                    }}
                </div>
            </Show>
        </Popover>
    }
}

/// Reboot's mode and reason, asked for before it is planned: the reason is recorded
/// in NinjaOne's own activity feed and the backend refuses a reboot without one, so
/// the Review button waits for it (`util::reboot_reason_missing`, the backend's
/// trim rule) instead of the dialog opening only to say so. Rendered only here.
#[component]
fn RebootPopover(
    source: SelectionSource,
    why: Memo<Option<String>>,
    label: &'static str,
) -> impl IntoView {
    let state = expect_context::<AppState>();
    let open = RwSignal::new(false);
    let missing = move || {
        state
            .actions
            .reason
            .with(|r| util::reboot_reason_missing(r))
    };
    view! {
        <Popover
            label=Signal::derive(move || label.to_string())
            class=Signal::derive(|| "btn-sm".to_string())
            title=Signal::derive(move || {
                why.get().unwrap_or_else(|| {
                    format!("{}. {}", ActionKind::Reboot.label(), ActionKind::Reboot.blast_radius())
                })
            })
            disabled=Signal::derive(move || why.with(Option::is_some))
            open=open
        >
            <div class="reboot-options">
                <label>
                    "Mode"
                    // `prop:selected` per option rather than `prop:value`: on a
                    // remount the value was set before the options existed — the
                    // control read "Normal" while a stored "Forced" was what the next
                    // plan would send.
                    <select on:change=move |ev| state.actions.reboot_mode.set(event_target_value(&ev))>
                        {REBOOT_MODES
                            .into_iter()
                            .map(|(value, label)| {
                                let selected = move || state.actions.reboot_mode.get() == value;
                                view! {
                                    <option value=value prop:selected=selected>
                                        {label}
                                    </option>
                                }
                            })
                            .collect_view()}
                    </select>
                </label>
                <label class="action-bar-reason">
                    "Reason (recorded in NinjaOne's activity feed)"
                    <input
                        type="text"
                        placeholder="e.g. July patch cycle"
                        prop:value=move || state.actions.reason.get()
                        on:input=move |ev| state.actions.reason.set(event_target_value(&ev))
                    />
                </label>
                <button
                    type="button"
                    class="btn btn-primary btn-sm"
                    title=move || if missing() { "Give a reason first".to_string() } else { String::new() }
                    prop:disabled=move || missing() || why.with(Option::is_some)
                    on:click=move |_| {
                        open.set(false);
                        state.open_plan(ActionKind::Reboot, source);
                    }
                >
                    "Review reboot…"
                </button>
            </div>
        </Popover>
    }
}

/// The library-script picker, in a dialog opened from the action bar's "Run a
/// script…". It runs with the action bar's options, which the dialog names rather
/// than repeating: run options render once.
#[component]
pub(crate) fn ScriptDialog() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <Show when=move || state.actions.script_dialog.get()>
            {move || {
                let (dialog, on_tab) = modal::focus_trap();
                let close = move || state.actions.script_dialog.set(false);
                let esc = window_event_listener(leptos::ev::keydown, move |ev| {
                    if ev.key() == "Escape" && state.actions.pending.with_untracked(Option::is_none) {
                        close();
                    }
                });
                on_cleanup(move || esc.remove());
                view! {
                    <div class="modal-overlay">
                        <div
                            class="modal script-dialog"
                            role="dialog"
                            aria-modal="true"
                            aria-labelledby="script-dialog-title"
                            tabindex="-1"
                            node_ref=dialog
                            on:keydown=move |ev| on_tab(&ev)
                        >
                            <div class="modal-head">
                                <h2 id="script-dialog-title">"Run a library script"</h2>
                                <button class="x" aria-label="Close" on:click=move |_| close()>
                                    "×"
                                </button>
                            </div>
                            <p class="modal-sub">
                                {move || {
                                    let (options, _) = util::options_label(
                                        true,
                                        state.actions.dry_run.get(),
                                        false,
                                        false,
                                    );
                                    format!(
                                        "On the devices of the ticked rows, with the action bar's options ({}). You confirm before anything is sent.",
                                        options.trim_end_matches(" ▾"),
                                    )
                                }}
                            </p>
                            <ScriptPicker/>
                        </div>
                    </div>
                }
            }}
        </Show>
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
                                // is what clears the blocked reason. The sign-in
                                // stays up, so loads the clear drops are reloaded.
                                state.clear_session_and_reload();
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
///
/// Every dispatch button comes from here, Reboot included, so each one gets the
/// same tooltip, accessible name and disabled-reason stacking. Reboot's button
/// opens its mode and reason (`RebootPopover`, the only place they render) rather
/// than planning at once. The headings read like the kind labels the confirm
/// dialog and the audit trail use ("Apply all OS patches", "Apply selected OS
/// patches"); they used to say "Install", so one action had two names.
const ACTION_GROUPS: [(&str, &[(ActionKind, &str)]); 4] = [
    (
        "Scan",
        &[
            (ActionKind::OsPatchScan, "OS"),
            (ActionKind::SoftwarePatchScan, "Software"),
        ],
    ),
    // The two Apply groups differ by blast radius, and that difference used to live
    // only in these headings — at 11px, muted, above two pairs of buttons all
    // reading "OS" and "Software". The button text now carries it too: "All OS"
    // cannot be misread as the targeted one at a glance the way a bare "OS" could.
    (
        "Apply all approved",
        &[
            (ActionKind::OsPatchApply, "All OS"),
            (ActionKind::SoftwarePatchApply, "All Software"),
        ],
    ),
    (
        "Apply selected",
        &[
            (ActionKind::OsPatchRemediate, "Selected OS"),
            (ActionKind::SoftwarePatchRemediate, "Selected Software"),
        ],
    ),
    ("Restart", &[(ActionKind::Reboot, "Reboot…")]),
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
            let mode_line = util::confirm_mode_line(
                kind,
                dry_run,
                pending.dry_run_shown && state.actions.dry_run.get_untracked(),
            );
            let target_lines = util::target_lines(&pending.request, &plan.eligible);
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
                            state.actions.dispatch_error.with(Option::is_some),
                            needs_typed,
                            typed,
                            &expected,
                        )
                    })
                }
            });
            let failed = move || state.actions.dispatch_error.with(Option::is_some);

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
                        <h2 id="confirm-action-title">{plan.summary.clone()}</h2>

                        // First line, before any device list or warning: what this
                        // action reaches. The dialog previously opened with the
                        // organizations affected, which answers "where" but never
                        // "how much" — and for the two native applies the honest
                        // answer is "more than you selected".
                        <p class=if kind.exceeds_selection() {
                            "confirm-radius confirm-radius-wide"
                        } else {
                            "confirm-radius"
                        }>{kind.blast_radius()}</p>
                        // Dry run or live, said up front: the heading reads the same
                        // either way, and Dry run reaches only the script-driven kinds.
                        {mode_line
                            .map(|(text, wide)| {
                                view! {
                                    <p class=if wide {
                                        "confirm-radius confirm-radius-wide"
                                    } else {
                                        "confirm-radius"
                                    }>{text}</p>
                                }
                            })}
                        // A bypassed window is as consequential as the reach, so it
                        // gets the same treatment rather than a line among warnings.
                        {plan
                            .window_overridden
                            .then(|| {
                                view! {
                                    <p class="confirm-radius confirm-radius-wide">
                                        "Overrides the closed maintenance window for this dispatch — recorded on the audit trail."
                                    </p>
                                }
                            })}

                        {(!plan.organizations.is_empty())
                            .then(|| {
                                view! {
                                    <p class="modal-sub">
                                        {format!(
                                            "{}: {}",
                                            if plan.organizations.len() == 1 {
                                                "Organization"
                                            } else {
                                                "Organizations"
                                            },
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

                        // The targets in words, above the literal string: for software
                        // that string is base64, which no operator can check.
                        {(!target_lines.is_empty())
                            .then(|| {
                                view! {
                                    <>
                                        <p class="modal-label">
                                            "What each device will be told to install"
                                        </p>
                                        <ul class="modal-target-lines">
                                            {target_lines
                                                .into_iter()
                                                .map(|line| view! { <li>{line}</li> })
                                                .collect_view()}
                                        </ul>
                                    </>
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

                        // What "Apply all" will actually install, per device. The native
                        // endpoint takes no list, so this is the only place the operator
                        // sees the backlog it is about to approve — and a device with
                        // nothing approved (everything still MANUAL) is the surprise.
                        {plan
                            .apply_preview
                            .clone()
                            .map(|preview| {
                                let summary = util::apply_preview_summary(&preview);
                                let items = preview
                                    .devices
                                    .iter()
                                    .map(|d| {
                                        view! {
                                            <li class:apply-preview-empty=d.approved == 0>
                                                {util::apply_preview_line(d)}
                                            </li>
                                        }
                                    })
                                    .collect_view();
                                // Unknown counts have no per-device list to expand, so
                                // they are a plain note rather than an empty disclosure.
                                if preview.known {
                                    view! {
                                        <details class="modal-apply-preview">
                                            <summary>{summary}</summary>
                                            <ul>{items}</ul>
                                        </details>
                                    }
                                        .into_any()
                                } else {
                                    view! { <p class="modal-sub modal-apply-preview">{summary}</p> }
                                        .into_any()
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
                                            {format!("{} targeted", util::count_of(device_count, "device", "devices"))}
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
                                            {format!("{} skipped", util::count_of(plan.skipped.len(), "device", "devices"))}
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

                        // A failed dispatch keeps the dialog open and says why here,
                        // where the operator is looking. Its token is spent, so the
                        // way forward is a fresh plan (or Cancel), not Run again.
                        <Show when=failed>
                            <div class="modal-error" role="alert">
                                <p>
                                    <strong>"Dispatch failed: "</strong>
                                    {move || state.actions.dispatch_error.get().unwrap_or_default()}
                                </p>
                                <p class="modal-sub">
                                    "Nothing more will be sent from this confirmation. Re-plan to check the targets again and get a fresh confirmation."
                                </p>
                            </div>
                        </Show>

                        <div class="modal-actions">
                            <button
                                class="btn"
                                prop:disabled=move || state.actions.dispatching.get()
                                on:click=move |_| state.cancel_plan()
                            >
                                {move || if failed() { "Close" } else { "Cancel" }}
                            </button>
                            <Show when=failed>
                                <button
                                    class="btn btn-primary"
                                    prop:disabled=move || state.actions.dispatching.get()
                                    on:click=move |_| state.replan()
                                >
                                    {move || {
                                        if state.actions.dispatching.get() {
                                            "Re-planning…"
                                        } else {
                                            "Re-plan"
                                        }
                                    }}
                                </button>
                            </Show>
                            <Show when=move || !blocked && !failed()>
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
                                            format!("Preview on {}", util::count_of(device_count, "device", "devices"))
                                        } else {
                                            format!("Run on {}", util::count_of(device_count, "device", "devices"))
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

/// The shared "Run as" menu: the Settings default, then the roles and stored
/// credentials NinjaOne reports for the first selected device.
///
/// Its own component because it lives in the action bar's shared options while the
/// fetch is driven by the selection. Credential roles are a *per-device* property,
/// so a choice valid on the sampled device may not exist on every other target;
/// NinjaOne refuses those dispatches per device, and the Jobs tab shows which.
#[component]
fn RunAsSelect() -> impl IntoView {
    let state = expect_context::<AppState>();
    let options = RwSignal::new(RunAsOptions::default());
    // Stamp of the newest lookup, so a slow reply for a device no longer sampled
    // cannot land after (and overwrite) the reply for the one that is.
    let request = StoredValue::new(0u64);

    // The one input the lookup depends on. A memo, because the effect used to track
    // the whole selection: every checkbox tick refetched the same device's roles,
    // the replies raced, and a failing lookup toasted once per click.
    let sampled = Memo::new(move |_| {
        state
            .actions
            .selected
            .with(|s| s.keys().next().copied())
            .filter(|_| state.blocked_reason().is_none())
    });

    Effect::new(move |_| {
        let seq = util::next_query_seq(request.get_value());
        request.set_value(seq);
        let Some(device_id) = sampled.get() else {
            options.set(RunAsOptions::default());
            return;
        };
        leptos::task::spawn_local(async move {
            let outcome = api::list_run_as_options(device_id).await;
            if util::is_superseded(request.get_value(), seq) {
                return;
            }
            match outcome {
                Ok(opts) => options.set(opts),
                // Every other call in this file routes failures through a Toast.
                // Swallowing this one left an empty "Run as" list that looked
                // exactly like a device with no configured credentials, so the
                // operator had no way to tell a failed lookup from a real answer.
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    });

    let choices = Memo::new(move |_| {
        let current = state.actions.run_as.get();
        options.with(|o| util::action_run_as_choices(&o.roles, &o.credentials, &current))
    });
    let option = move |c: util::RunAsChoice| {
        let value = c.value.clone();
        view! {
            <option
                value=c.value
                prop:selected=move || state.actions.run_as.with(|v| *v == value)
            >
                {c.label}
            </option>
        }
    };

    view! {
        <select on:change=move |ev| state.actions.run_as.set(event_target_value(&ev))>
            // Blank sends no override; the backend resolves it to the saved default.
            <option value="" prop:selected=move || state.actions.run_as.with(String::is_empty)>
                {move || state.settings.f_actions.with(|a| util::run_as_default_label(&a.run_as))}
            </option>
            {move || choices.get().0.into_iter().map(option).collect_view()}
            {move || {
                let creds = choices.get().1;
                (!creds.is_empty())
                    .then(|| {
                        view! {
                            <optgroup label="Credentials">
                                {creds.into_iter().map(option).collect_view()}
                            </optgroup>
                        }
                    })
            }}
        </select>
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
                // Selection rides on each option (see the reboot Mode select): the
                // picker lives in a collapsible `<details>` under a `<Show>`, and
                // `prop:value` on a remount showed "Select a script…" over a stored
                // script id that the next dispatch would still use.
                <select on:change=move |ev| {
                    state.actions.script_id.set(event_target_value(&ev).parse().ok())
                }>
                    <option value="" prop:selected=move || state.actions.script_id.get().is_none()>
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
                                let id = s.id;
                                let selected = move || state.actions.script_id.get() == Some(id);
                                view! {
                                    <option value=id.to_string() prop:selected=selected>
                                        {s.name}
                                    </option>
                                }
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

            <Show when=move || state.dry_run_reason(ActionKind::Script).is_some()>
                <p class="script-picker-targets" role="note">
                    {move || state.dry_run_reason(ActionKind::Script).unwrap_or_default()}
                </p>
            </Show>
            <button
                class="btn btn-primary btn-sm"
                title=move || state.dry_run_reason(ActionKind::Script).unwrap_or_default()
                prop:disabled=move || {
                    state.actions.script_id.with(|s| s.is_none())
                        || state.actions.dispatching.get()
                        || state.dry_run_reason(ActionKind::Script).is_some()
                }
                on:click=move |_| {
                    state.actions.script_dialog.set(false);
                    state.open_plan(ActionKind::Script, SelectionSource::PatchRows);
                }
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
    // A retry plans like any dispatch, so it waits on the same things.
    let retry_disabled =
        move || state.blocked_reason().is_some() || state.actions.dispatching.get();

    view! {
        <div class="jobs">
            <Show when=move || state.blocked_reason().is_some()>
                <p class="empty" role="note">
                    {move || state.blocked_reason().unwrap_or_default()}
                    <ReauthorizeLink/>
                    <EnableActionsLink/>
                </p>
            </Show>

            // Dispatch lives in the action bar on the Patches and Needs Reboot tabs,
            // next to the selection it targets. This tab is history; its Retry only
            // re-opens that same confirm flow. None of these buttons has a job store
            // to talk to in the browser demo, so they are not offered there.
            // No Refresh: `action:progress` already pushes every job change here, so
            // re-reading the store showed the same rows and looked broken.
            <Show when=move || !state.session.web_mode.get() && !state.session.demo.get()>
            <div class="jobs-toolbar">
                <button
                    class="btn btn-sm"
                    prop:disabled=move || state.actions.jobs.with(Vec::is_empty)
                    title="Removes finished jobs from this session's list. Jobs still running stay until they finish. The audit trail below is permanent and is not affected."
                    on:click=move |_| state.clear_job_history()
                >
                    "Clear finished"
                </button>
                // One button per batch with several definite failures. It re-opens
                // the same plan → confirm dialog as any dispatch — never a direct
                // re-send — and only definite failures are included.
                {move || {
                    state
                        .actions
                        .jobs
                        .with(|jobs| util::retryable_batches(jobs))
                        .into_iter()
                        .map(|batch| {
                            let label = format!(
                                "Retry {} failed · {}",
                                batch.job_ids.len(),
                                batch.detail,
                            );
                            let ids = batch.job_ids;
                            view! {
                                <button
                                    class="btn btn-sm"
                                    title="Re-plan this batch's failed devices with the options they were sent. You confirm again before anything is dispatched."
                                    prop:disabled=retry_disabled
                                    on:click=move |_| state.retry_jobs(&ids)
                                >
                                    {label}
                                </button>
                            }
                        })
                        .collect_view()
                }}
            </div>
            </Show>

            <h3 class="jobs-heading">"This session"</h3>

            // Keyed rather than rebuilt: an `action:progress` event re-renders only
            // the rows it advanced (see `util::job_row_key`) instead of cloning and
            // diffing all of them. The empty check is a `Show`, so a progress tick
            // never re-runs the table either.
            <Show
                when=move || state.actions.jobs.with(|jobs| !jobs.is_empty())
                fallback=|| {
                    view! {
                        <p class="empty">
                            "No actions dispatched yet. Select patch rows on the Patches tab (or devices on the Needs Reboot tab), then choose an action from the bar above the table."
                        </p>
                    }
                }
            >
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
                                    <th scope="col">"Retry"</th>
                                </tr>
                            </thead>
                            <tbody>
                                // Newest first, as before.
                                <For
                                    each=move || state.actions.jobs.get().into_iter().rev()
                                    key=util::job_row_key
                                    children=move |j| {
                                        // Only a definite failure gets a Retry;
                                        // an Unknown one may already have acted.
                                        // A settled job without one says why.
                                        let retry_note = util::retry_cell_note(&j).map(
                                            |(short, full)| {
                                                view! {
                                                    <span class="retry-note" title=full>
                                                        {short}
                                                    </span>
                                                }
                                            },
                                        );
                                        let retry = util::retry_blocked_reason(&j)
                                            .is_none()
                                            .then(|| {
                                                let id = j.id;
                                                let label = format!(
                                                    "Retry {} on {}",
                                                    j.detail,
                                                    j.device_name,
                                                );
                                                view! {
                                                    <button
                                                        class="btn btn-sm"
                                                        aria-label=label
                                                        title="Re-plan this device with the options it was sent. You confirm again before anything is dispatched."
                                                        prop:disabled=retry_disabled
                                                        on:click=move |_| state.retry_jobs(&[id])
                                                    >
                                                        "Retry…"
                                                    </button>
                                                }
                                            });
                                        // The reference scripts' exit codes, in
                                        // words: NinjaOne returns no output, and a
                                        // bare `4` read as a failed install.
                                        let meaning = j
                                            .exit_code
                                            .and_then(|c| util::remediation_exit_meaning(j.kind, c));
                                        let exit_help = meaning.map(|(_, long)| long);
                                        let exit_text = match (j.exit_code, meaning) {
                                            (Some(c), Some((short, _))) => format!("{c} · {short}"),
                                            (Some(c), None) => c.to_string(),
                                            (None, _) => String::new(),
                                        };
                                        view! {
                                            <tr>
                                                <td>{j.device_name.clone()}</td>
                                                <td>{j.organization.clone()}</td>
                                                <td>
                                                    {j.detail.clone()}
                                                    // Which patches this device was
                                                    // sent — the point of a selective
                                                    // install, and on no other surface
                                                    // once the dialog closed.
                                                    {util::job_targets_summary(&j)
                                                        .map(|(short, full)| {
                                                            view! {
                                                                <span class="job-targets" title=full>
                                                                    {short}
                                                                </span>
                                                            }
                                                        })}
                                                </td>
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
                                                <td title=exit_help>{exit_text}</td>
                                                <td>{j.dispatched_at.clone()}</td>
                                                <td>{format_duration(j.duration_seconds)}</td>
                                                <td>{retry}{retry_note}</td>
                                            </tr>
                                        }
                                    }
                                />
                            </tbody>
                        </table>
                    </div>
            </Show>

            <AuditTrail/>
        </div>
    }
}

/// The durable half of the Jobs tab: `action-audit.jsonl` rendered in the app.
///
/// The write path has always kept this record — append-only, parameters redacted,
/// written owner-only before the request goes out so a crash mid-batch still leaves
/// evidence. Nothing could read it back, so restarting the app after rebooting 25
/// servers left no in-app trace that it had happened, and the only durable answer to
/// "what did we do last Tuesday" was to find the file by hand.
#[component]
fn AuditTrail() -> impl IntoView {
    let state = expect_context::<AppState>();
    let records = RwSignal::new(Vec::<AuditRecord>::new());
    let loaded = RwSignal::new(false);

    let load = move || {
        if !api::is_tauri() {
            loaded.set(true);
            return;
        }
        spawn_local(async move {
            match api::read_action_audit().await {
                Ok(rows) => {
                    records.set(rows);
                    loaded.set(true);
                }
                Err(e) => {
                    loaded.set(true);
                    state.notify(Toast::err(e));
                }
            }
        });
    };
    // Read once when the tab first renders; the file only changes when this app
    // writes to it, and a dispatch already refreshes the live list above.
    load();

    view! {
        <h3 class="jobs-heading">
            "Audit trail"
            <button
                class="btn btn-sm"
                title="Re-read action-audit.jsonl from disk"
                on:click=move |_| load()
            >
                "Reload"
            </button>
        </h3>
        <p class="chips-label">
            "This install's most recent dispatch records, newest first; the full log is "
            "action-audit.jsonl. Written before the request goes out, so an action that never "
            "reported back still appears. Not affected by \"Clear finished\"."
        </p>
        {move || {
            if !loaded.get() {
                return view! { <p class="empty">"Reading the audit trail…"</p> }.into_any();
            }
            let rows = records.get();
            if rows.is_empty() {
                return view! {
                    <p class="empty">"No dispatches recorded on this install yet."</p>
                }
                    .into_any();
            }
            view! {
                <div class="table-wrap">
                    <table class="data-table">
                        <thead>
                            <tr>
                                <th scope="col">"When"</th>
                                <th scope="col">"Device"</th>
                                <th scope="col">"Organization"</th>
                                <th scope="col">"Action"</th>
                                <th scope="col">"Mode"</th>
                                <th scope="col">"Outcome"</th>
                                <th scope="col">"Exit"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {rows
                                .into_iter()
                                .map(|r| {
                                    view! {
                                        <tr>
                                            <td>{r.timestamp.clone()}</td>
                                            <td>{r.device_name.clone()}</td>
                                            <td>{r.organization.clone()}</td>
                                            <td>{r.detail.clone()}</td>
                                            <td>
                                                {util::audit_mode_label(r.dry_run, r.window_override)}
                                            </td>
                                            <td>{r.outcome.clone()}</td>
                                            <td>
                                                {r
                                                    .exit_code
                                                    .map(|c| c.to_string())
                                                    .unwrap_or_default()}
                                            </td>
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
    }
}
