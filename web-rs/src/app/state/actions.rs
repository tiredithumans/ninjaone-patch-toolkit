//! Device-action dispatch: remediation-script config, the script library, the
//! blocked/visible verdicts, building and planning a request, the confirm/re-plan
//! flow, and the Jobs list.

use super::*;

impl AppState {
    /// Whether a library script id is configured for this remediation kind's patch
    /// family. Advisory — the backend re-reads the same setting and blocks without
    /// one; this is what lets the button explain itself instead of failing on click.
    pub(in crate::app) fn remediation_script_configured(self, kind: ActionKind) -> bool {
        self.settings.f_actions.with(|a| {
            if kind.is_os_family() {
                a.os_patch_script_id.is_some()
            } else {
                a.software_patch_script_id.is_some()
            }
        })
    }

    pub(in crate::app) fn load_scripts(self) {
        if !self.can_act() {
            return;
        }
        self.actions.scripts_loading.set(true);
        spawn_local(async move {
            match api::list_scripts().await {
                Ok(list) => self.actions.scripts.set(list),
                Err(e) => self.notify(Toast::err(format!("Couldn't load scripts: {e}"))),
            }
            self.actions.scripts_loading.set(false);
        });
    }

    /// Why actions are unavailable, if they are — tracked, so a view reading this
    /// re-renders when the operator signs in or flips the Settings toggle.
    pub(in crate::app) fn blocked_reason(self) -> Option<String> {
        self.session.auth.with(|auth| {
            action_blocked_reason(
                self.session.web_mode.get(),
                self.session.demo.get(),
                auth.as_ref(),
            )
        })
    }

    /// Same verdict without subscribing — for event handlers, which run outside a
    /// reactive scope and would otherwise register a spurious dependency.
    pub(in crate::app) fn blocked_reason_untracked(self) -> Option<String> {
        self.session.auth.with_untracked(|auth| {
            action_blocked_reason(
                self.session.web_mode.get_untracked(),
                self.session.demo.get_untracked(),
                auth.as_ref(),
            )
        })
    }

    /// Whether the action bar should be rendered at all.
    ///
    /// Distinct from [`can_act`](Self::can_act), which is whether the buttons work.
    /// An install that never switched patch actions on in Settings is a read-only
    /// reporting tool, and ~95px of permanently disabled dispatch controls sat above
    /// the table on every query — in a layout where the table was already below the
    /// fold. Demo and browser mode keep it: the hosted page showing an action
    /// surface it cannot use is an honest advertisement, and hiding it there would
    /// make the demo misrepresent the product.
    pub(in crate::app) fn action_surface_visible(self) -> bool {
        self.session.web_mode.get()
            || self.session.demo.get()
            || self
                .session
                .auth
                .with(|a| a.as_ref().is_some_and(|a| a.actions_enabled))
    }

    /// Whether the action affordances should be live. The backend re-checks all of
    /// this — this only decides what the UI offers.
    pub(in crate::app) fn can_act(self) -> bool {
        self.blocked_reason_untracked().is_none()
    }

    /// Builds the request for `kind` from the current selection and form state.
    /// Reads the run options out of the signals and hands them to the pure
    /// `util::build_action_request`.
    ///
    /// The branching this used to do inline decides which devices get dispatched to
    /// and what each is told to install — including the rule that a remediation kind
    /// skips devices with nothing ticked of its family, which exists because handing
    /// one an empty allow list produces a job that reports success having installed
    /// nothing. That belongs somewhere a test can reach it; this file has no test
    /// module, and the crate's only gates are a compile check and clippy.
    pub(in crate::app) fn build_request(self, kind: ActionKind) -> ActionRequest {
        let opts = util::RunOptions {
            use_kb_targeting: self.actions.use_kb_targeting.get_untracked(),
            include_offline: self.actions.include_offline.get_untracked(),
            override_window: self.actions.override_window.get_untracked(),
            dry_run: self.actions.dry_run.get_untracked(),
            script_reboot: self.actions.script_reboot.get_untracked(),
            run_as: self.actions.run_as.get_untracked(),
            reboot_mode_forced: self.actions.reboot_mode.get_untracked() == "FORCED",
            reason: self.actions.reason.get_untracked(),
            script_id: self.actions.script_id.get_untracked(),
            script_name: {
                let id = self.actions.script_id.get_untracked();
                self.actions
                    .scripts
                    .with_untracked(|s| s.iter().find(|s| Some(s.id) == id).map(|s| s.name.clone()))
            },
            script_params: self.actions.script_params.get_untracked(),
        };
        self.actions
            .selected
            .with_untracked(|sel| util::build_action_request(kind, sel, &opts))
    }

    /// Asks the backend what `kind` would do and opens the confirmation modal.
    pub(in crate::app) fn open_plan(self, kind: ActionKind) {
        if !self.can_act() {
            if let Some(reason) = self.blocked_reason_untracked() {
                self.notify(Toast::err(reason));
            }
            return;
        }
        let request = self.build_request(kind);
        if request.device_ids.is_empty() {
            self.notify(Toast::err("Select at least one device first"));
            return;
        }
        self.actions.confirm_input.set(String::new());
        self.actions.dispatch_error.set(None);
        self.actions.dispatching.set(true);
        spawn_local(async move {
            match api::plan_action(request.clone()).await {
                Ok(plan) => self
                    .actions
                    .pending
                    .set(Some(PendingAction { request, plan })),
                Err(e) => self.notify(Toast::err(e)),
            }
            self.actions.dispatching.set(false);
        });
    }

    pub(in crate::app) fn cancel_plan(self) {
        self.actions.pending.set(None);
        self.actions.confirm_input.set(String::new());
        self.actions.dispatch_error.set(None);
    }

    /// Asks for a fresh plan — and so a fresh confirm token — for the request held
    /// in the dialog, after a dispatch from it failed and spent the old token.
    ///
    /// Re-plans the *same* request rather than rebuilding it from the selection:
    /// the dialog is still showing what the operator approved, and the backend
    /// re-checks every guardrail against current state either way.
    pub(in crate::app) fn replan(self) {
        let Some(pending) = self.actions.pending.get_untracked() else {
            return;
        };
        let mut request = pending.request;
        request.confirm_token = None;
        self.actions.dispatching.set(true);
        spawn_local(async move {
            match api::plan_action(request.clone()).await {
                Ok(plan) => {
                    self.actions.dispatch_error.set(None);
                    self.actions.confirm_input.set(String::new());
                    self.actions
                        .pending
                        .set(Some(PendingAction { request, plan }));
                }
                Err(e) => self
                    .actions
                    .dispatch_error
                    .set(Some(format!("Couldn't re-plan: {e}"))),
            }
            self.actions.dispatching.set(false);
        });
    }

    /// Dispatches the plan currently held in the modal.
    pub(in crate::app) fn confirm_plan(self) {
        let Some(pending) = self.actions.pending.get_untracked() else {
            return;
        };
        let mut request = pending.request;
        request.confirm_token = pending.plan.confirm_token.clone();
        // A dry run changes nothing on the device, so the results on screen are not
        // stale after it. `dry_run` defaults on, so without this every default
        // "Preview on N devices" raised the amber banner and its Refresh link forced
        // a whole-fleet refetch for nothing.
        let mutating = request.kind.is_mutating() && !request.dry_run;

        self.actions.dispatching.set(true);
        spawn_local(async move {
            match api::run_action(request).await {
                Ok(batch) => {
                    self.actions.pending.set(None);
                    self.actions.confirm_input.set(String::new());
                    // Seed from the response rather than re-fetching; the backend
                    // poller advances these rows over `action:progress`, which may
                    // already have delivered them — hence merge, not append.
                    self.actions
                        .jobs
                        .update(|jobs| util::merge_jobs(jobs, batch.jobs.clone()));
                    self.ui.active_tab.set(Tab::Jobs);
                    if mutating && batch.dispatched > 0 {
                        // The on-screen result predates the change we just made.
                        self.actions.results_stale.set(true);
                    }
                    let msg = if batch.skipped > 0 {
                        format!(
                            "Dispatched to {} device(s); {} skipped",
                            batch.dispatched, batch.skipped
                        )
                    } else {
                        format!("Dispatched to {} device(s)", batch.dispatched)
                    };
                    self.notify(Toast::ok(msg));
                }
                // Kept in the dialog, which stays open: a toast behind the overlay
                // vanished after a few seconds and left a Run button that could
                // only fail again, its single-use token already spent.
                Err(e) => self.actions.dispatch_error.set(Some(e)),
            }
            self.actions.dispatching.set(false);
            self.actions.dispatch_progress.set(None);
        });
    }

    pub(in crate::app) fn refresh_jobs(self) {
        if self.session.web_mode.get_untracked() || self.session.demo.get_untracked() {
            return;
        }
        spawn_local(async move {
            if let Ok(jobs) = api::list_jobs().await {
                self.actions.jobs.set(jobs);
            }
        });
    }

    pub(in crate::app) fn clear_job_history(self) {
        spawn_local(async move {
            match api::clear_jobs().await {
                Ok(jobs) => self.actions.jobs.set(jobs),
                Err(e) => self.notify(Toast::err(e)),
            }
        });
    }
}
