//! Live NinjaOne job status: what NinjaOne reports running on the devices in the
//! current result, for display beside them. Read-only. A dispatched action still
//! resolves from `/activities` alone (`commands::actions::poller`).

use chrono::{DateTime, Utc};
use serde::Serialize;
use tauri::State;

use crate::error::UiError;
use crate::model::RunningJob;
use crate::rows::DeviceSummary;
use crate::state::AppState;

/// The job types shown: the ones patching operations start or wait on. Monitoring,
/// antivirus, remote-control and backup jobs run all day on a healthy fleet, and
/// marking every device "job running" because of them would hide the one patch run
/// worth seeing.
pub const PATCH_OPS_JOB_TYPES: [&str; 6] = [
    "PATCH_MANAGEMENT",
    "SOFTWARE_PATCH_MANAGEMENT",
    "SCRIPTING",
    "ACTION",
    "ACTIONSET",
    "SYSTEM",
];

/// `jobStatus` values that mean the job is over. `/jobs` lists running jobs, but a
/// job that just ended can still be listed, and calling it "running" would be wrong.
const FINISHED_STATUSES: [&str; 3] = ["COMPLETED", "CANCELLED", "SUPERSEDED"];

/// One running job on an in-scope device, labelled for display.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningJobRow {
    /// NinjaOne's job uid (the activity series uid).
    pub uid: Option<String>,
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    /// [`job_type_label`] of `jobType`.
    pub job_type: String,
    /// [`job_status_label`] of `jobStatus`.
    pub status: String,
    /// The job's `subject`, or its `message` when it has none.
    pub detail: Option<String>,
    /// The policy or configuration element that started it.
    pub source_name: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
}

/// What NinjaOne reported running, and when it was asked.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningJobs {
    pub fetched_at: String,
    pub jobs: Vec<RunningJobRow>,
}

/// The operator-facing name of a `jobType`; a value this build does not know reads
/// as itself.
pub fn job_type_label(raw: &str) -> String {
    match raw {
        "PATCH_MANAGEMENT" => "OS patching",
        "SOFTWARE_PATCH_MANAGEMENT" => "Software patching",
        "SCRIPTING" => "Script",
        "ACTION" => "Action",
        "ACTIONSET" => "Action set",
        "SYSTEM" => "System",
        other => other,
    }
    .to_string()
}

/// The operator-facing name of a `jobStatus`; a value this build does not know reads
/// as itself.
pub fn job_status_label(raw: &str) -> String {
    match raw {
        "START_REQUESTED" => "Starting",
        "STARTED" => "Started",
        "IN_PROCESS" => "In progress",
        "WAITING_FOR_USER_RESPONSE" => "Waiting for user",
        "CANCEL_REQUESTED" => "Cancelling",
        other => other,
    }
    .to_string()
}

fn fmt_time(at: Option<DateTime<Utc>>) -> Option<String> {
    at.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
}

/// The running patch-ops jobs on `devices` (the current result's in-scope devices),
/// labelled from them and ordered by device, then oldest first.
pub fn running_job_rows(jobs: Vec<RunningJob>, devices: &[DeviceSummary]) -> Vec<RunningJobRow> {
    let mut rows: Vec<RunningJobRow> = jobs
        .into_iter()
        .filter(|j| {
            j.job_type
                .as_deref()
                .is_some_and(|t| PATCH_OPS_JOB_TYPES.contains(&t))
                && !j
                    .job_status
                    .as_deref()
                    .is_some_and(|s| FINISHED_STATUSES.contains(&s))
        })
        .filter_map(|j| {
            let device = devices.iter().find(|d| Some(d.device_id) == j.device_id)?;
            Some(RunningJobRow {
                uid: j.uid.clone(),
                device_id: device.device_id,
                device_name: device.device_name.clone(),
                organization: device.organization.clone(),
                job_type: job_type_label(j.job_type.as_deref().unwrap_or_default()),
                status: job_status_label(j.job_status.as_deref().unwrap_or("Running")),
                detail: j
                    .subject
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| j.message.clone().filter(|m| !m.trim().is_empty())),
                source_name: j.source_name.clone().filter(|s| !s.trim().is_empty()),
                started_at: fmt_time(j.started_at()),
                updated_at: fmt_time(j.updated_at()),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        a.device_name
            .to_lowercase()
            .cmp(&b.device_name.to_lowercase())
            .then_with(|| a.started_at.cmp(&b.started_at))
    });
    rows
}

/// The patch-ops jobs NinjaOne reports running on the current result's devices.
/// Empty when there is no result to scope to, like the paging commands.
///
/// The result is read again after the fetch: a sign-out or tenant switch while it
/// was in flight clears the result, and the jobs are then dropped rather than
/// labelled with the previous tenant's devices.
#[tauri::command]
pub async fn list_running_jobs(state: State<'_, AppState>) -> Result<RunningJobs, UiError> {
    let fetched_at = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    if state.current_result_handle()?.is_none() {
        return Ok(RunningJobs {
            fetched_at,
            jobs: Vec::new(),
        });
    }
    let jobs = state.api.running_jobs().await.map_err(UiError::from)?;
    let jobs = match state.current_result_handle()? {
        Some(result) => running_job_rows(jobs, &result.devices),
        None => Vec::new(),
    };
    Ok(RunningJobs { fetched_at, jobs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(device_id: i64, job_type: &str, status: &str) -> RunningJob {
        RunningJob {
            uid: Some(format!("uid-{device_id}-{job_type}")),
            device_id: Some(device_id),
            job_type: Some(job_type.into()),
            job_status: Some(status.into()),
            subject: None,
            message: Some("Installing updates".into()),
            source_name: Some("Patch window".into()),
            create_time: Some(1_782_480_000.0),
            update_time: None,
        }
    }

    fn device(id: i64, name: &str) -> DeviceSummary {
        DeviceSummary {
            device_id: id,
            device_name: name.into(),
            organization: "Contoso".into(),
            location: None,
            device_role: None,
            os_name: None,
            node_class: None,
            needs_reboot: false,
            reboot_reason: None,
            offline: false,
            pending_count: 0,
            rollup_scope: crate::rows::RollupScope::Included,
            pending_by_severity: Default::default(),
            past_sla: 0,
            urgent_past_sla: 0,
            failed_installs: None,
            last_contact: None,
            last_contact_ts: None,
        }
    }

    /// Only patch-ops jobs that are still running, on devices in the result, are
    /// listed — named from the result, labelled for reading.
    #[test]
    fn only_running_patch_jobs_on_in_scope_devices_are_listed() {
        let devices = [device(10, "web-01"), device(20, "db-01")];
        let rows = running_job_rows(
            vec![
                job(10, "PATCH_MANAGEMENT", "IN_PROCESS"),
                job(10, "MONITOR", "TRIGGERED"),
                job(20, "SCRIPTING", "COMPLETED"),
                job(20, "SYSTEM", "NEW_STATUS"),
                job(99, "PATCH_MANAGEMENT", "STARTED"),
            ],
            &devices,
        );
        let seen: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| {
                (
                    r.device_name.as_str(),
                    r.job_type.as_str(),
                    r.status.as_str(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("db-01", "System", "NEW_STATUS"),
                ("web-01", "OS patching", "In progress")
            ]
        );
        assert_eq!(rows[1].detail.as_deref(), Some("Installing updates"));
        assert_eq!(rows[1].started_at.as_deref(), Some("2026-06-26 13:20 UTC"));
    }
}
