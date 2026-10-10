//! What a dispatch records: `JobState`, `JobReport`, `JobRequest`.

use super::*;
use crate::model::RebootMode;
use chrono::{DateTime, Utc};
use serde::Serialize;

/// Where a dispatched job has got to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state", content = "detail")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed(String),
    TimedOut,
    /// The dispatch POST failed in a way that cannot tell "rejected" from
    /// "accepted" — a timeout or a connection lost after send, a 5xx, an unreadable
    /// 2xx body (`api::OutcomeUnknown`) — so NinjaOne may or may not have queued it.
    /// Never auto-retried — a replay could run the script twice — but still polled,
    /// in case the activity feed resolves it.
    Unknown(String),
    /// A guardrail stopped this target before anything was sent.
    Skipped(String),
}

impl JobState {
    /// Every variant (with an empty detail), for the IPC fixture's enum list and the
    /// Jobs rows it emits. A new variant breaks the match in
    /// `all_lists_every_job_state_once` until it is listed here.
    #[cfg(test)]
    pub const ALL: [Self; 7] = [
        Self::Queued,
        Self::Running,
        Self::Completed,
        Self::Failed(String::new()),
        Self::TimedOut,
        Self::Unknown(String::new()),
        Self::Skipped(String::new()),
    ];

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed(_) | Self::TimedOut | Self::Skipped(_)
        )
    }

    pub fn label(&self) -> String {
        match self {
            Self::Queued => "Queued".into(),
            Self::Running => "Running".into(),
            Self::Completed => "Completed".into(),
            Self::Failed(msg) => format!("Failed: {msg}"),
            Self::TimedOut => "Timed out".into(),
            Self::Unknown(msg) => format!("Unknown: {msg}"),
            Self::Skipped(why) => format!("Skipped: {why}"),
        }
    }
}

/// One dispatched row, serialized straight to the frontend.
///
/// Dates carry both a formatted label and a raw epoch, the same convention
/// `PatchRow` uses, so the UI can display and sort without re-parsing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobReport {
    /// Unique per dispatched *row*, not per device: batches accumulate in the Jobs
    /// tab and one device can appear in several concurrent batches, so status
    /// updates key on this rather than `device_id`.
    pub id: u64,
    pub batch_id: u64,
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    pub kind: ActionKind,
    /// What was dispatched, in operator terms — the script name with the id the plan
    /// resolved ("Name (#42)"), "Apply all OS patches", "Apply selected OS patches
    /// (#7)", "Reboot (FORCED)". See `dispatch::action_detail`.
    pub detail: String,
    pub dry_run: bool,
    pub state: JobState,
    pub dispatched_at: String,
    pub dispatched_ts: i64,
    pub finished_at: Option<String>,
    pub duration_seconds: Option<i64>,
    pub activity_id: Option<i64>,
    pub series_uid: Option<String>,
    pub exit_code: Option<i32>,
    /// What this device was sent, so the Jobs tab can offer a *Retry* that goes back
    /// through `plan_action` → confirm. `None` on a row built without a request.
    pub request: Option<JobRequest>,
}

/// The inputs of the dispatch that produced one job, for this one device — enough to
/// rebuild its `ActionRequest` for a retry. `kind` and `dry_run` are on the
/// [`JobReport`] itself.
///
/// A retry is *not* a replay: the frontend rebuilds the request from this and sends
/// it through `plan_action`, so it gets a fresh payload-bound confirm token and every
/// `plan()` guardrail runs again against current state. Only a definite `Failed` is
/// offered one — an `Unknown` job may already have acted, which is exactly what
/// `ReplaySafety::ActOnce` exists to refuse.
///
/// `override_window` is deliberately **not** carried: an override approved for the
/// original dispatch says nothing about the moment of the retry, so the maintenance
/// window is re-evaluated from scratch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRequest {
    pub script_id: Option<i64>,
    pub script_uid: Option<String>,
    pub script_name: Option<String>,
    /// The hand-typed string of a `Script` dispatch, verbatim. In memory only, like
    /// the request it came from — the audit log records the redacted form.
    pub parameters: Option<String>,
    /// The identity it actually ran as (the *resolved* value, so a changed Settings
    /// default does not silently change who a retry runs as). `None` for the native
    /// endpoints, which run as NinjaOne's agent.
    pub run_as: Option<String>,
    pub reboot: RebootChoice,
    pub reboot_mode: Option<RebootMode>,
    pub reason: Option<String>,
    pub include_offline: bool,
    /// This device's own targets (KBs or product titles) — never the batch's union.
    pub targets: Vec<String>,
}

impl JobReport {
    /// Marks the job finished, stamping the wall clock and elapsed time.
    pub fn finish(&mut self, state: JobState, now: DateTime<Utc>) {
        self.state = state;
        self.finished_at = Some(fmt_ts(now));
        self.duration_seconds = Some((now.timestamp() - self.dispatched_ts).max(0));
    }

    /// Whether the poller has waited long enough to call this job dead.
    pub fn is_past_timeout(&self, now: DateTime<Utc>) -> bool {
        now.timestamp() - self.dispatched_ts >= JOB_TIMEOUT_MINUTES * 60
    }
}

pub fn fmt_ts(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}
