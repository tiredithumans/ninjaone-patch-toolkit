//! Live NinjaOne job status: the cadence tick and the read behind it.

use super::*;

impl AppState {
    /// Reads the running jobs when `util::live_jobs_due` says so. Driven by the
    /// one-second ticker in `app.rs`; unlike auto-refresh it does not hold while a
    /// dialog is open, because the device drill-down shows these jobs.
    pub(in crate::app) fn tick_live_jobs(self) {
        // Untracked: this runs from a timer, outside any reactive context.
        let authed = self
            .session
            .auth
            .with_untracked(|a| a.as_ref().is_some_and(|a| a.authenticated));
        if !api::is_tauri() || !authed {
            return;
        }
        let result_key = self
            .query
            .result
            .with_untracked(|r| r.as_ref().map(|r| r.generated_at.clone()));
        let due = self.live_jobs.last_poll.with_untracked(|last| {
            util::live_jobs_due(
                last.as_ref(),
                result_key.as_deref(),
                js_sys::Date::now(),
                api::document_hidden(),
                self.live_jobs.in_flight.get_untracked(),
            )
        });
        if due {
            self.refresh_live_jobs();
        }
    }

    /// Reads the running jobs now — the cadence's read and the Refresh button's.
    pub(in crate::app) fn refresh_live_jobs(self) {
        if !api::is_tauri() {
            return;
        }
        let Some(result_key) = self
            .query
            .result
            .with_untracked(|r| r.as_ref().map(|r| r.generated_at.clone()))
        else {
            return;
        };
        let seq = self.live_jobs.seq.get_untracked().wrapping_add(1);
        self.live_jobs.seq.set(seq);
        self.live_jobs.in_flight.set(true);
        let at_ms = js_sys::Date::now();
        spawn_local(async move {
            let outcome = api::list_running_jobs().await;
            if self.live_jobs.seq.get_untracked() != seq {
                return;
            }
            let failed = outcome.is_err();
            let empty = outcome.as_ref().ok().map(|r| r.jobs.is_empty());
            let empty_streak = self
                .live_jobs
                .last_poll
                .with_untracked(|last| util::next_empty_streak(last.as_ref(), &result_key, empty));
            match outcome {
                Ok(jobs) => {
                    self.live_jobs.jobs.set(Some(jobs));
                    self.live_jobs.error.set(None);
                }
                Err(e) => self.live_jobs.error.set(Some(e.to_string())),
            }
            self.live_jobs.in_flight.set(false);
            self.live_jobs.last_poll.set(Some(util::LivePoll {
                at_ms,
                failed,
                result_key,
                empty_streak,
            }));
        });
    }
}
