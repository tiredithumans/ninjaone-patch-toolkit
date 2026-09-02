//! What the operator (and, through them, the maintainer) can see about what this
//! app did.
//!
//! Two read-only surfaces, both over files the app already writes:
//!
//! * [`open_diagnostics_folder`] reveals the directory holding the rolling logs, so
//!   a bug report can carry evidence. Before this, `init_tracing` wrote to stdout
//!   only — which a bundled `.app` launched from Finder discards — so a report like
//!   "compliance showed 94% but we're at 71%" arrived with nothing to go on.
//! * [`read_action_audit`] renders `action-audit.jsonl` in the app. The write path
//!   has always kept a careful append-only, redacted, owner-only record of every
//!   dispatch; nothing could read it back, so restarting the app after rebooting 25
//!   servers left no in-app trace of it. The in-memory job list is per-session and
//!   has a "Clear history" button; this is the durable half.
//!
//! Neither command touches the network or `AppState`, and neither mutates anything,
//! so neither is gated on `require_actions_enabled` — reading your own audit trail
//! must not require the write feature to be switched on.

use serde::Serialize;
use tracing::warn;

use crate::error::UiError;
use crate::paths;

/// How many audit records to return, newest first.
///
/// The file is append-only and unbounded; an install that has been dispatching for a
/// year should not push a year of history through IPC to render one screen.
const AUDIT_TAIL: usize = 500;

/// One dispatched action as the Activity view shows it.
///
/// A projection of `actions::audit::AuditEntry`, not that type re-exported: the
/// on-disk record is an append-only format written by every past version of the app,
/// so the reader has to tolerate fields that a record predates. Every field here is
/// therefore optional except the ones the view cannot render without.
#[derive(Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuditRecord {
    pub timestamp: String,
    pub kind: String,
    pub device_name: String,
    pub organization: String,
    pub detail: String,
    pub outcome: String,
    pub dry_run: bool,
    pub batch_id: Option<u64>,
    pub exit_code: Option<i32>,
    /// True when this record came from the pre-`paths::app_dir` location. Surfaced
    /// so the view can say why history stops rather than looking like data loss.
    pub legacy: bool,
}

/// Reads the newest [`AUDIT_TAIL`] records, newest first.
///
/// Reads the current file and, if present, the one older builds wrote to a different
/// config directory — see [`paths::legacy_audit_path`]. A malformed line is skipped
/// rather than failing the whole read: this is an append-only log that a crash can
/// truncate mid-line, and one bad tail must not hide the history above it.
#[tauri::command]
pub async fn read_action_audit() -> Result<Vec<AuditRecord>, UiError> {
    // Synchronous file I/O, and the file can be large.
    tokio::task::spawn_blocking(read_audit_files)
        .await
        .map_err(|e| UiError::new(format!("reading the action audit log panicked: {e}")))
}

fn read_audit_files() -> Vec<AuditRecord> {
    let mut records = Vec::new();
    if let Ok(path) = paths::audit_path() {
        records.extend(parse_audit_file(&path, false));
    }
    if let Some(legacy) = paths::legacy_audit_path() {
        records.extend(parse_audit_file(&legacy, true));
    }
    // Newest first. The two files are each append-ordered but interleave in time
    // only at the upgrade boundary, so sorting the union is simpler than merging and
    // costs nothing at this size.
    records.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    records.truncate(AUDIT_TAIL);
    records
}

fn parse_audit_file(path: &std::path::Path, legacy: bool) -> Vec<AuditRecord> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        // A missing file is the normal state for an install that has never
        // dispatched anything — not worth a warning.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            warn!(?err, path = %path.display(), "could not read the action audit log");
            return Vec::new();
        }
    };
    body.lines()
        .filter_map(|line| parse_line(line, legacy))
        .collect()
}

fn parse_line(line: &str, legacy: bool) -> Option<AuditRecord> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let get = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    Some(AuditRecord {
        timestamp: get("timestamp"),
        kind: get("kind"),
        device_name: get("deviceName"),
        organization: get("organization"),
        detail: get("detail"),
        outcome: get("outcome"),
        dry_run: value
            .get("dryRun")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        batch_id: value.get("batchId").and_then(|v| v.as_u64()),
        exit_code: value
            .get("exitCode")
            .and_then(|v| v.as_i64())
            .and_then(|v| i32::try_from(v).ok()),
        legacy,
    })
}

/// Opens the diagnostics directory in the platform file manager.
///
/// Reveals the folder rather than a single file: which log a maintainer needs
/// depends on when the problem happened, and the operator should not have to be
/// told a filename over email. Returns the path so the UI can also display it —
/// "we opened a window somewhere" is not a useful thing to tell someone whose
/// window did not open.
#[tauri::command]
pub async fn open_diagnostics_folder() -> Result<String, UiError> {
    let dir = paths::log_dir().map_err(UiError::from)?;
    let display = dir.display().to_string();
    tokio::task::spawn_blocking(move || {
        // The directory may not exist yet if the file layer failed to initialise;
        // create it so the reveal lands somewhere rather than erroring.
        std::fs::create_dir_all(&dir)?;
        open::that_detached(&dir)
    })
    .await
    .map_err(|e| UiError::new(format!("opening the diagnostics folder panicked: {e}")))?
    .map_err(|e| UiError::new(format!("could not open {display}: {e}")))?;
    Ok(display)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_truncated_tail_line_does_not_hide_the_history_above_it() {
        let good = r#"{"timestamp":"2026-09-02T10:00:00Z","kind":"Reboot","deviceName":"srv-01","organization":"Acme","detail":"Restart","outcome":"Succeeded","dryRun":false,"batchId":7,"exitCode":0}"#;
        // A crash mid-append leaves a partial line; it must cost only itself.
        let torn = r#"{"timestamp":"2026-09-02T10:05:00Z","kind":"Reb"#;
        let parsed: Vec<_> = [good, torn, ""]
            .iter()
            .filter_map(|l| parse_line(l, false))
            .collect();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].device_name, "srv-01");
        assert_eq!(parsed[0].exit_code, Some(0));
        assert_eq!(parsed[0].batch_id, Some(7));
    }

    #[test]
    fn a_record_missing_newer_fields_still_renders() {
        // The log is append-only across versions, so the reader meets records
        // written before a field existed. Those must not drop the whole line.
        let old =
            r#"{"timestamp":"2026-06-01T09:00:00Z","kind":"OsPatchScan","deviceName":"srv-09"}"#;
        let record = parse_line(old, true).expect("an older record still parses");
        assert_eq!(record.device_name, "srv-09");
        assert_eq!(record.organization, "", "a missing field reads as empty");
        assert_eq!(record.exit_code, None);
        assert!(!record.dry_run, "a missing dryRun is not a live dispatch");
        assert!(record.legacy, "legacy provenance is carried to the view");
    }

    #[test]
    fn a_non_json_line_is_skipped_rather_than_failing_the_read() {
        assert_eq!(parse_line("not json at all", false), None);
        assert_eq!(parse_line("   ", false), None);
    }
}
