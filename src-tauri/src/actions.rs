//! Device actions: what may be dispatched, what the guardrails allow, and how a
//! dispatched job is tracked to a terminal state.
//!
//! The API layer (`api::actions`) knows how to *send* an action. This module owns
//! everything around that: the [`plan`] function that decides whether an action is
//! allowed to go out at all, the [`JobReport`] rows the UI watches, the parameter
//! string handed to a library script, and the audit trail.
//!
//! [`plan`] is deliberately pure — the clock is injected — so every guardrail is
//! unit-testable without a tenant, a network, or a wall clock.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{Activity, Device, RebootMode};
use crate::settings::ActionSettings;

pub mod audit;

/// How long a dispatched job may stay unresolved before the poller gives up.
pub const JOB_TIMEOUT_MINUTES: i64 = 45;
/// Most recent jobs kept in memory. Terminal rows are evicted first.
pub const MAX_JOBS: usize = 500;

/// What the operator asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionKind {
    OsPatchScan,
    SoftwarePatchScan,
    OsPatchApply,
    SoftwarePatchApply,
    Reboot,
    Script,
}

impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OsPatchScan => "Scan for OS patches",
            Self::SoftwarePatchScan => "Scan for software patches",
            Self::OsPatchApply => "Apply OS patches",
            Self::SoftwarePatchApply => "Apply software patches",
            Self::Reboot => "Reboot",
            Self::Script => "Run script",
        }
    }

    /// Whether this changes the device. Drives the confirmation gate, the
    /// blast-radius cap, and the maintenance-window check — a scan only refreshes
    /// NinjaOne's view of what the device needs, so it is exempt from all three.
    pub fn is_mutating(self) -> bool {
        !matches!(self, Self::OsPatchScan | Self::SoftwarePatchScan)
    }

    /// Whether NinjaOne offers a real preview for this action. Only a library
    /// script does (via its own `dryRun` parameter); the native endpoints have no
    /// preview mode at all, so a "dry run" of them dispatches nothing.
    pub fn supports_dry_run(self) -> bool {
        matches!(self, Self::Script)
    }
}

/// Whether the dispatched *script* should restart the device when it finishes.
/// Distinct from [`RebootMode`], which addresses the reboot endpoint directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RebootChoice {
    #[default]
    Never,
    Auto,
}

impl RebootChoice {
    /// The token the PowerShell side expects for `rebootBehavior`.
    pub fn script_value(self) -> &'static str {
        match self {
            Self::Never => "Never",
            Self::Auto => "Auto",
        }
    }
}

/// Where a dispatched job has got to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state", content = "detail")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed(String),
    TimedOut,
    /// The dispatch POST timed out *after* the body was sent, so NinjaOne may or
    /// may not have queued it. Never auto-retried — a replay could run the script
    /// twice — but still polled, in case the activity feed resolves it.
    Unknown(String),
    /// A guardrail stopped this target before anything was sent.
    Skipped(String),
}

impl JobState {
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
#[derive(Debug, Clone, Serialize)]
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
    /// What was dispatched, in operator terms — the script name, "Apply OS
    /// patches", "Reboot (FORCED)".
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

/// A device the action will be dispatched to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedTarget {
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    pub offline: bool,
}

/// A device that was asked for but will not be dispatched to, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedTarget {
    pub device_id: i64,
    pub device_name: String,
    pub reason: String,
}

/// The outcome of planning an action: exactly what would happen, and whether it is
/// allowed to happen at all.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlan {
    pub summary: String,
    pub eligible: Vec<PlannedTarget>,
    pub skipped: Vec<SkippedTarget>,
    pub organizations: Vec<String>,
    /// Soft advisories — the action proceeds.
    pub warnings: Vec<String>,
    /// Hard stops. Non-empty means nothing will be dispatched and no confirmation
    /// token is issued.
    pub blockers: Vec<String>,
    pub reboot_expected: bool,
    pub dry_run: bool,
    /// The exact `parameters` string that will be sent, for scripts. Shown verbatim
    /// in the confirmation dialog — the toolkit never sends a string the operator
    /// has not seen.
    pub parameters_preview: Option<String>,
    pub confirm_token: Option<String>,
}

impl ActionPlan {
    pub fn is_blocked(&self) -> bool {
        !self.blockers.is_empty()
    }
}

/// Everything [`plan`] needs. Borrowed rather than owned so the caller can pass
/// slices straight out of the warm fleet cache.
pub struct PlanInput<'a> {
    pub kind: ActionKind,
    pub device_ids: &'a [i64],
    pub devices: &'a [Device],
    pub org_names: &'a HashMap<i64, String>,
    pub settings: &'a ActionSettings,
    pub include_offline: bool,
    pub override_window: bool,
    pub reboot_mode: Option<RebootMode>,
    pub dry_run: bool,
    /// Injected so the maintenance-window check is testable.
    pub now: DateTime<Local>,
}

/// Decides what an action would do and whether the guardrails permit it.
///
/// Pure: no I/O, no ambient clock. Every guardrail lives here rather than in the
/// webview, so a stale or modified frontend cannot talk the backend into a bigger
/// blast radius than Settings allows.
pub fn plan(input: PlanInput<'_>) -> ActionPlan {
    let by_id: HashMap<i64, &Device> = input.devices.iter().map(|d| (d.id, d)).collect();
    let s = input.settings;

    let mut eligible: Vec<PlannedTarget> = Vec::new();
    let mut skipped: Vec<SkippedTarget> = Vec::new();

    for id in input.device_ids {
        let Some(device) = by_id.get(id) else {
            skipped.push(SkippedTarget {
                device_id: *id,
                device_name: format!("Device {id}"),
                reason: "not in the current device inventory".into(),
            });
            continue;
        };
        let name = device.label().to_string();
        let offline = device.is_offline();
        // NinjaOne *queues* work for an offline device rather than rejecting it, so
        // an action dispatched now can restart the machine hours later when it
        // reconnects — long after the operator stopped watching.
        if offline && !(input.include_offline && s.allow_offline_targets) {
            skipped.push(SkippedTarget {
                device_id: *id,
                device_name: name,
                reason: "device is offline — NinjaOne would queue this until it reconnects".into(),
            });
            continue;
        }
        eligible.push(PlannedTarget {
            device_id: *id,
            device_name: name,
            organization: org_name(input.org_names, device),
            offline,
        });
    }

    let organizations: Vec<String> = eligible
        .iter()
        .map(|t| t.organization.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut warnings = Vec::new();
    let mut blockers = Vec::new();

    if eligible.is_empty() {
        blockers.push("No eligible devices — nothing would be dispatched.".into());
    }
    if input.kind.is_mutating() {
        if eligible.len() > s.max_devices_per_action {
            blockers.push(format!(
                "{} devices exceeds the {}-device limit for one action. Narrow the selection, \
                 or raise the limit in Settings → Patch actions.",
                eligible.len(),
                s.max_devices_per_action
            ));
        }
        if organizations.len() > s.max_orgs_per_action {
            blockers.push(format!(
                "Selection spans {} organizations ({}), above the limit of {}. \
                 Dispatch one organization at a time.",
                organizations.len(),
                organizations.join(", "),
                s.max_orgs_per_action
            ));
        }
        if s.require_maintenance_window && !window_is_open(s, input.now) {
            if input.override_window && s.allow_window_override {
                warnings.push(
                    "Outside the maintenance window — proceeding because the override is enabled."
                        .into(),
                );
            } else {
                blockers.push(format!(
                    "Outside the maintenance window ({}). Wait for the window, or enable the \
                     override in Settings → Patch actions.",
                    window_label(s)
                ));
            }
        }
    }

    // A dry run of a native endpoint would be a lie: there is no preview mode, so
    // nothing is sent at all. Say so rather than letting the operator believe they
    // previewed something.
    let dry_run = input.dry_run;
    if dry_run && !input.kind.supports_dry_run() {
        blockers.push(format!(
            "\"{}\" has no preview mode in the NinjaOne API — a dry run would dispatch nothing. \
             Turn off Dry run to send it for real.",
            input.kind.label()
        ));
    }

    let reboot_expected = !dry_run
        && matches!(
            input.kind,
            ActionKind::Reboot | ActionKind::OsPatchApply | ActionKind::SoftwarePatchApply
        );
    if reboot_expected {
        warnings.push(match input.reboot_mode {
            Some(RebootMode::Forced) => format!(
                "Forced reboot discards unsaved work on {} device(s).",
                eligible.len()
            ),
            _ => format!("{} device(s) may restart.", eligible.len()),
        });
    }
    let offline_count = eligible.iter().filter(|t| t.offline).count();
    if offline_count > 0 {
        warnings.push(format!(
            "{offline_count} offline device(s) included — NinjaOne will queue the action until \
             they reconnect."
        ));
    }

    ActionPlan {
        summary: format!("{} on {} device(s)", input.kind.label(), eligible.len()),
        organizations,
        eligible,
        skipped,
        warnings,
        blockers,
        reboot_expected,
        dry_run,
        parameters_preview: None,
        confirm_token: None,
    }
}

fn org_name(names: &HashMap<i64, String>, device: &Device) -> String {
    device
        .organization_id
        .and_then(|id| names.get(&id).cloned())
        .unwrap_or_else(|| "(unknown organization)".to_string())
}

/// Whether `now` falls inside the configured window. A start later than the end
/// means the window wraps past midnight (e.g. 22:00–04:00), in which case the day
/// check applies to the day the window *opened*.
fn window_is_open(s: &ActionSettings, now: DateTime<Local>) -> bool {
    if s.window_days.is_empty() {
        return false;
    }
    let minute = (now.hour() * 60 + now.minute()) as u16;
    let today = now.weekday().num_days_from_sunday() as u8;
    let yesterday = (today + 6) % 7;

    if s.window_start_minute <= s.window_end_minute {
        s.window_days.contains(&today)
            && minute >= s.window_start_minute
            && minute < s.window_end_minute
    } else {
        // Wrapping window: before midnight belongs to today, after midnight to the
        // day the window opened.
        (s.window_days.contains(&today) && minute >= s.window_start_minute)
            || (s.window_days.contains(&yesterday) && minute < s.window_end_minute)
    }
}

fn window_label(s: &ActionSettings) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let days: Vec<&str> = s
        .window_days
        .iter()
        .filter_map(|d| DAYS.get(*d as usize).copied())
        .collect();
    let hhmm = |m: u16| format!("{:02}:{:02}", m / 60, m % 60);
    format!(
        "{} {}–{} local",
        if days.is_empty() {
            "no days".to_string()
        } else {
            days.join("/")
        },
        hhmm(s.window_start_minute),
        hhmm(s.window_end_minute)
    )
}

/// Builds the `parameters` string NinjaOne forwards to a library script.
///
/// NinjaOne splits `parameters` on **spaces** into `key=value` tokens, so a target
/// list whose entries contain spaces (third-party product titles like
/// "Google Chrome") cannot be sent literally. OS patches are KB numbers and are
/// safe as a bare comma list; software targets are base64-encoded into a single
/// space-free token that the script decodes and splits on `|`.
pub fn build_parameters(
    kind: ActionKind,
    targets: &[String],
    reboot: RebootChoice,
    dry_run: bool,
) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let reboot = reboot.script_value();
    match kind {
        ActionKind::SoftwarePatchApply | ActionKind::SoftwarePatchScan => {
            let encoded = STANDARD.encode(targets.join("|"));
            format!("productAllowListB64={encoded} rebootBehavior={reboot} dryRun={dry_run}")
        }
        _ => {
            let kbs = targets
                .iter()
                .map(|k| k.trim().trim_start_matches("KB").trim_start_matches("kb"))
                .filter(|k| !k.is_empty())
                .collect::<Vec<_>>()
                .join(",");
            format!("kbAllowList={kbs} rebootBehavior={reboot} dryRun={dry_run}")
        }
    }
}

/// Finds the activity that corresponds to a dispatched job.
///
/// Three tiers, most to least certain: the exact activity id the dispatch returned,
/// the activity series uid, then the newest script activity on that device since
/// dispatch. The middle tier is what makes an id-less dispatch response usable —
/// several tenants return only a `jobUid`.
pub fn match_activity<'a>(activities: &'a [Activity], job: &JobReport) -> Option<&'a Activity> {
    if let Some(id) = job.activity_id
        && let Some(a) = activities.iter().find(|a| a.id == Some(id))
    {
        return Some(a);
    }
    if let Some(uid) = job.series_uid.as_deref()
        && let Some(a) = activities
            .iter()
            .find(|a| a.series_uid.as_deref() == Some(uid))
    {
        return Some(a);
    }
    // Allow a little slack around the dispatch timestamp for clock skew between
    // this machine and the NinjaOne backend.
    let floor = (job.dispatched_ts - 5) as f64;
    activities
        .iter()
        .filter(|a| {
            matches!(
                a.activity_type.as_deref(),
                Some("SCRIPT") | Some("ACTION") | Some("ACTIONSET")
            )
        })
        .filter(|a| a.activity_time.unwrap_or(0.0) >= floor)
        .max_by(|a, b| {
            a.activity_time
                .unwrap_or(0.0)
                .total_cmp(&b.activity_time.unwrap_or(0.0))
        })
}

/// Advances one job given the activities visible for its device.
///
/// A poll that returned *no* activities is not a failure — the feed lags behind a
/// dispatch — so the job holds its state until [`JOB_TIMEOUT_MINUTES`] elapses.
pub fn advance_job(job: &mut JobReport, activities: &[Activity], now: DateTime<Utc>) {
    match match_activity(activities, job) {
        Some(a) if a.is_terminal() => {
            let state = match a.status.as_deref() {
                Some("COMPLETED") => JobState::Completed,
                Some("TIMED_OUT") => JobState::TimedOut,
                Some(other) => JobState::Failed(other.to_string()),
                None => JobState::Failed("unknown terminal state".into()),
            };
            if job.activity_id.is_none() {
                job.activity_id = a.id;
            }
            if job.series_uid.is_none() {
                job.series_uid = a.series_uid.clone();
            }
            job.exit_code = a.exit_code();
            job.finish(state, now);
        }
        Some(_) => job.state = JobState::Running,
        None => {
            if job.is_past_timeout(now) {
                job.finish(JobState::TimedOut, now);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn device(id: i64, name: &str, org: i64, offline: bool) -> Device {
        serde_json::from_value(json!({
            "id": id,
            "systemName": name,
            "organizationId": org,
            "offline": offline,
        }))
        .expect("device")
    }

    fn orgs() -> HashMap<i64, String> {
        HashMap::from([(1, "Contoso".to_string()), (2, "Fabrikam".to_string())])
    }

    /// A Wednesday at 03:00 local — inside the default Mon–Fri 02:00–05:00 window.
    fn inside_window() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 7, 29, 3, 0, 0).unwrap()
    }

    /// Same Wednesday at 13:00 local — outside it.
    fn outside_window() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 7, 29, 13, 0, 0).unwrap()
    }

    fn input<'a>(
        kind: ActionKind,
        ids: &'a [i64],
        devices: &'a [Device],
        names: &'a HashMap<i64, String>,
        settings: &'a ActionSettings,
    ) -> PlanInput<'a> {
        PlanInput {
            kind,
            device_ids: ids,
            devices,
            org_names: names,
            settings,
            include_offline: false,
            override_window: false,
            reboot_mode: None,
            dry_run: false,
            now: inside_window(),
        }
    }

    #[test]
    fn plan_skips_offline_devices_unless_opted_in() {
        let devices = vec![device(1, "srv-a", 1, false), device(2, "srv-b", 1, true)];
        let names = orgs();
        let ids = [1, 2];
        let settings = ActionSettings::default();

        let p = plan(input(
            ActionKind::OsPatchApply,
            &ids,
            &devices,
            &names,
            &settings,
        ));
        assert_eq!(p.eligible.len(), 1);
        assert_eq!(p.skipped.len(), 1);
        assert!(p.skipped[0].reason.contains("offline"));

        // Opting in requires BOTH the per-request flag and the setting.
        let opted = ActionSettings {
            allow_offline_targets: true,
            ..ActionSettings::default()
        };
        let p = plan(PlanInput {
            include_offline: true,
            ..input(ActionKind::OsPatchApply, &ids, &devices, &names, &opted)
        });
        assert_eq!(p.eligible.len(), 2);
        assert!(
            p.warnings.iter().any(|w| w.contains("queue")),
            "including an offline device must still warn that it gets queued"
        );
    }

    #[test]
    fn plan_blocks_over_the_device_cap() {
        let devices: Vec<Device> = (1..=30)
            .map(|i| device(i, &format!("srv{i}"), 1, false))
            .collect();
        let ids: Vec<i64> = (1..=30).collect();
        let names = orgs();
        let settings = ActionSettings::default(); // cap 25

        let p = plan(input(
            ActionKind::OsPatchApply,
            &ids,
            &devices,
            &names,
            &settings,
        ));
        assert!(p.is_blocked());
        assert!(p.blockers[0].contains("25-device limit"));

        // A scan doesn't change the device, so the cap doesn't apply.
        let p = plan(input(
            ActionKind::OsPatchScan,
            &ids,
            &devices,
            &names,
            &settings,
        ));
        assert!(
            !p.is_blocked(),
            "scans are exempt from the blast-radius cap"
        );
    }

    #[test]
    fn plan_blocks_cross_org_beyond_the_org_cap() {
        let devices = vec![device(1, "srv-a", 1, false), device(2, "srv-b", 2, false)];
        let ids = [1, 2];
        let names = orgs();
        let settings = ActionSettings::default(); // 1 org

        let p = plan(input(ActionKind::Reboot, &ids, &devices, &names, &settings));
        assert!(p.is_blocked());
        assert!(
            p.blockers.iter().any(|b| b.contains("2 organizations")),
            "a cross-tenant dispatch must be a blocker: {:?}",
            p.blockers
        );

        let wider = ActionSettings {
            max_orgs_per_action: 2,
            ..ActionSettings::default()
        };
        assert!(!plan(input(ActionKind::Reboot, &ids, &devices, &names, &wider)).is_blocked());
    }

    #[test]
    fn plan_blocks_outside_the_maintenance_window_and_honors_the_override_flag() {
        let devices = vec![device(1, "srv-a", 1, false)];
        let ids = [1];
        let names = orgs();
        let gated = ActionSettings {
            require_maintenance_window: true,
            ..ActionSettings::default()
        };

        // Inside the window: fine.
        assert!(
            !plan(input(
                ActionKind::OsPatchApply,
                &ids,
                &devices,
                &names,
                &gated
            ))
            .is_blocked()
        );

        // Outside it: blocked.
        let p = plan(PlanInput {
            now: outside_window(),
            ..input(ActionKind::OsPatchApply, &ids, &devices, &names, &gated)
        });
        assert!(p.is_blocked());
        assert!(p.blockers[0].contains("maintenance window"));

        // Asking to override without the setting enabled changes nothing.
        let p = plan(PlanInput {
            now: outside_window(),
            override_window: true,
            ..input(ActionKind::OsPatchApply, &ids, &devices, &names, &gated)
        });
        assert!(
            p.is_blocked(),
            "the override must be enabled in Settings too"
        );

        let overridable = ActionSettings {
            allow_window_override: true,
            ..gated
        };
        let p = plan(PlanInput {
            now: outside_window(),
            override_window: true,
            ..input(
                ActionKind::OsPatchApply,
                &ids,
                &devices,
                &names,
                &overridable,
            )
        });
        assert!(!p.is_blocked());
        assert!(p.warnings.iter().any(|w| w.contains("override")));
    }

    #[test]
    fn a_wrapping_window_spans_midnight() {
        let s = ActionSettings {
            require_maintenance_window: true,
            window_days: vec![3], // Wednesday
            window_start_minute: 22 * 60,
            window_end_minute: 4 * 60,
            ..ActionSettings::default()
        };
        // Wednesday 23:00 — after the Wednesday open.
        assert!(window_is_open(
            &s,
            Local.with_ymd_and_hms(2026, 7, 29, 23, 0, 0).unwrap()
        ));
        // Thursday 02:00 — still inside the window that opened Wednesday.
        assert!(window_is_open(
            &s,
            Local.with_ymd_and_hms(2026, 7, 30, 2, 0, 0).unwrap()
        ));
        // Thursday 05:00 — closed.
        assert!(!window_is_open(
            &s,
            Local.with_ymd_and_hms(2026, 7, 30, 5, 0, 0).unwrap()
        ));
        // Wednesday 12:00 — before it opens.
        assert!(!window_is_open(
            &s,
            Local.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap()
        ));
    }

    #[test]
    fn dry_run_is_rejected_for_kinds_with_no_preview() {
        let devices = vec![device(1, "srv-a", 1, false)];
        let ids = [1];
        let names = orgs();
        let settings = ActionSettings::default();

        // The native endpoints have no preview, so a "dry run" would send nothing.
        for kind in [
            ActionKind::OsPatchApply,
            ActionKind::SoftwarePatchApply,
            ActionKind::Reboot,
            ActionKind::OsPatchScan,
        ] {
            let p = plan(PlanInput {
                dry_run: true,
                ..input(kind, &ids, &devices, &names, &settings)
            });
            assert!(p.is_blocked(), "{kind:?} must not pretend to preview");
            assert!(p.blockers.iter().any(|b| b.contains("no preview mode")));
        }

        // A script has a real dry run.
        let p = plan(PlanInput {
            dry_run: true,
            ..input(ActionKind::Script, &ids, &devices, &names, &settings)
        });
        assert!(!p.is_blocked());
        assert!(
            !p.reboot_expected,
            "a preview must not claim it will reboot"
        );
    }

    #[test]
    fn plan_reports_unknown_devices_rather_than_dropping_them() {
        let devices = vec![device(1, "srv-a", 1, false)];
        let ids = [1, 99];
        let names = orgs();
        let settings = ActionSettings::default();

        let p = plan(input(
            ActionKind::OsPatchScan,
            &ids,
            &devices,
            &names,
            &settings,
        ));
        assert_eq!(p.eligible.len(), 1);
        assert_eq!(p.skipped.len(), 1);
        assert_eq!(p.skipped[0].device_id, 99);
    }

    #[test]
    fn build_parameters_sets_dry_run_flag() {
        let targets = vec!["KB5040434".to_string(), "5041580".to_string()];
        assert_eq!(
            build_parameters(
                ActionKind::OsPatchApply,
                &targets,
                RebootChoice::Never,
                true
            ),
            "kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=true"
        );
        assert_eq!(
            build_parameters(
                ActionKind::OsPatchApply,
                &targets,
                RebootChoice::Never,
                false
            ),
            "kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=false"
        );
    }

    #[test]
    fn build_parameters_reflects_reboot_choice() {
        let targets = vec!["KB1".to_string()];
        assert_eq!(
            build_parameters(
                ActionKind::OsPatchApply,
                &targets,
                RebootChoice::Auto,
                false
            ),
            "kbAllowList=1 rebootBehavior=Auto dryRun=false"
        );
    }

    #[test]
    fn build_parameters_software_encodes_product_allow_list() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let targets = vec!["Google Chrome".to_string(), "7-Zip".to_string()];
        let params = build_parameters(
            ActionKind::SoftwarePatchApply,
            &targets,
            RebootChoice::Auto,
            false,
        );

        let encoded = params
            .strip_prefix("productAllowListB64=")
            .and_then(|s| s.split(' ').next())
            .expect("encoded token present");
        let decoded = STANDARD.decode(encoded).expect("valid base64");
        assert_eq!(String::from_utf8(decoded).unwrap(), "Google Chrome|7-Zip");
        // The whole point: no spaces leak into what NinjaOne tokenizes.
        assert!(!encoded.contains(' '));
        assert!(params.ends_with(" rebootBehavior=Auto dryRun=false"));
    }

    fn job(activity_id: Option<i64>, series_uid: Option<&str>, dispatched_ts: i64) -> JobReport {
        JobReport {
            id: 1,
            batch_id: 1,
            device_id: 42,
            device_name: "srv-1".into(),
            organization: "Contoso".into(),
            kind: ActionKind::Script,
            detail: "Install-CriticalSecurityUpdates".into(),
            dry_run: false,
            state: JobState::Running,
            dispatched_at: fmt_ts(Utc::now()),
            dispatched_ts,
            finished_at: None,
            duration_seconds: None,
            activity_id,
            series_uid: series_uid.map(str::to_string),
            exit_code: None,
        }
    }

    fn activity(v: serde_json::Value) -> Activity {
        serde_json::from_value(v).expect("activity")
    }

    #[test]
    fn match_activity_prefers_activity_id() {
        let now = Utc::now().timestamp();
        let j = job(Some(1002), None, now);
        let list = vec![
            activity(
                json!({ "id": 1001, "activityType": "SCRIPT", "status": "COMPLETED",
                             "activityTime": now as f64 }),
            ),
            activity(
                json!({ "id": 1002, "activityType": "SCRIPT", "status": "RUNNING",
                             "activityTime": (now - 2) as f64 }),
            ),
        ];
        assert_eq!(
            match_activity(&list, &j).and_then(|a| a.id),
            Some(1002),
            "the exact id must win over the newer row"
        );
    }

    #[test]
    fn match_activity_falls_back_to_the_series_uid() {
        let now = Utc::now().timestamp();
        let j = job(None, Some("uid-abc"), now);
        let list = vec![
            activity(json!({ "id": 1, "activityType": "SCRIPT", "activityTime": now as f64 })),
            activity(
                json!({ "id": 2, "seriesUid": "uid-abc", "activityType": "SCRIPT",
                             "activityTime": (now - 30) as f64 }),
            ),
        ];
        assert_eq!(match_activity(&list, &j).and_then(|a| a.id), Some(2));
    }

    #[test]
    fn match_activity_ignores_rows_predating_the_dispatch() {
        let now = Utc::now().timestamp();
        let j = job(None, None, now);
        let stale = vec![activity(
            json!({ "id": 9, "activityType": "SCRIPT", "activityTime": (now - 600) as f64 }),
        )];
        assert!(
            match_activity(&stale, &j).is_none(),
            "a run from ten minutes ago is not this job"
        );
    }

    #[test]
    fn advance_marks_timeout_only_when_older_than_the_threshold() {
        let now = Utc::now();

        // Nothing matched yet, but still inside the window: hold state.
        let mut fresh = job(None, None, now.timestamp() - 60);
        advance_job(&mut fresh, &[], now);
        assert_eq!(fresh.state, JobState::Running);
        assert!(fresh.finished_at.is_none());

        let mut old = job(None, None, now.timestamp() - (JOB_TIMEOUT_MINUTES + 1) * 60);
        advance_job(&mut old, &[], now);
        assert_eq!(old.state, JobState::TimedOut);
        assert!(old.finished_at.is_some());
    }

    /// A transient failure to *read* the activity feed must never be recorded as a
    /// failure of the job itself — the reference implementation did this, and it
    /// reported healthy patch runs as failures whenever the API hiccupped.
    #[test]
    fn an_empty_poll_does_not_fail_the_job() {
        let now = Utc::now();
        let mut j = job(None, None, now.timestamp());
        advance_job(&mut j, &[], now);
        assert_eq!(j.state, JobState::Running);
        assert!(!j.state.is_terminal());
    }

    #[test]
    fn advance_captures_exit_code_and_correlators_on_completion() {
        let now = Utc::now();
        let mut j = job(None, None, now.timestamp());
        let list = vec![activity(json!({
            "id": 77, "seriesUid": "uid-z", "activityType": "SCRIPT",
            "status": "COMPLETED", "activityTime": now.timestamp() as f64,
            "result": { "exitCode": 2 },
        }))];
        advance_job(&mut j, &list, now);

        assert_eq!(j.state, JobState::Completed);
        assert_eq!(j.exit_code, Some(2));
        assert_eq!(j.activity_id, Some(77));
        assert_eq!(j.series_uid.as_deref(), Some("uid-z"));
        assert!(j.duration_seconds.is_some());
    }

    /// `Unknown` must stay non-terminal: the dispatch may already be running on the
    /// device, so the poller has to keep trying to correlate it rather than closing
    /// the row out.
    #[test]
    fn unknown_is_not_terminal_but_the_other_end_states_are() {
        assert!(!JobState::Unknown("timeout".into()).is_terminal());
        assert!(!JobState::Queued.is_terminal());
        assert!(!JobState::Running.is_terminal());
        assert!(JobState::Completed.is_terminal());
        assert!(JobState::Failed("boom".into()).is_terminal());
        assert!(JobState::TimedOut.is_terminal());
        assert!(JobState::Skipped("offline".into()).is_terminal());
    }

    /// IPC-drift guard, mirroring `rows::serialized_shapes_carry_every_frontend_required_key`:
    /// `web-rs/src/types.rs` deserializes these exact camelCase keys, and a rename
    /// here would fail silently in the webview rather than at compile time.
    #[test]
    fn job_report_serializes_every_frontend_required_key() {
        let value = serde_json::to_value(job(Some(1), Some("uid"), 0)).expect("serialize");
        for key in [
            "id",
            "batchId",
            "deviceId",
            "deviceName",
            "organization",
            "kind",
            "detail",
            "dryRun",
            "state",
            "dispatchedAt",
            "dispatchedTs",
            "finishedAt",
            "durationSeconds",
            "activityId",
            "seriesUid",
            "exitCode",
        ] {
            assert!(value.get(key).is_some(), "JobReport is missing `{key}`");
        }
        assert_eq!(
            value["kind"], "SCRIPT",
            "ActionKind must stay SCREAMING_SNAKE"
        );
    }

    /// The tagged representation the frontend switches on. A `Failed` row must
    /// carry its message in `detail`, or the Jobs tab shows a bare "Failed".
    #[test]
    fn job_state_serializes_as_a_tagged_state_plus_detail() {
        let plain = serde_json::to_value(JobState::Completed).expect("serialize");
        assert_eq!(plain["state"], "completed");

        let failed = serde_json::to_value(JobState::Failed("boom".into())).expect("serialize");
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["detail"], "boom");

        let skipped = serde_json::to_value(JobState::Skipped("offline".into())).expect("serialize");
        assert_eq!(skipped["state"], "skipped");
        assert_eq!(skipped["detail"], "offline");
    }

    #[test]
    fn scans_are_not_mutating_but_everything_else_is() {
        assert!(!ActionKind::OsPatchScan.is_mutating());
        assert!(!ActionKind::SoftwarePatchScan.is_mutating());
        for k in [
            ActionKind::OsPatchApply,
            ActionKind::SoftwarePatchApply,
            ActionKind::Reboot,
            ActionKind::Script,
        ] {
            assert!(k.is_mutating(), "{k:?} changes the device");
        }
    }
}
