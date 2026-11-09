//! What the operator (and, through them, the maintainer) can see about what this
//! app did.
//!
//! Three read-only surfaces, all over files the app already writes:
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
//! * [`read_run_history`] renders `run-history.jsonl`, the one-rollup-per-query
//!   trend `history` appends, so "is the backlog shrinking" has an answer.
//!
//! None of the commands touches the network or `AppState`, and none mutates
//! anything, so none is gated on `require_actions_enabled` — reading your own audit
//! trail must not require the write feature to be switched on.

use serde::Serialize;
use tauri::State;
use tracing::warn;

use crate::error::UiError;
use crate::paths;
use crate::state::AppState;

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
    /// The dispatch overrode a closed maintenance window. Absent (false) on every
    /// record written before the field existed and on every ordinary dispatch.
    pub window_override: bool,
    pub batch_id: Option<u64>,
    pub exit_code: Option<i32>,
    /// True when this record came from the pre-`paths::app_dir` location. Surfaced
    /// so the view can say why history stops rather than looking like data loss.
    pub legacy: bool,
}

/// Reads the newest [`AUDIT_TAIL`] records, newest first.
///
/// Reads the live file, its rotated generations, and the file older builds wrote to
/// a different config directory — see [`paths::legacy_audit_path`] — each only while
/// the tail is still short (see [`read_audit_tail`]). A malformed line is skipped
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
    let live = paths::audit_path().ok();
    let legacy = paths::legacy_audit_path();
    read_audit_tail(live.as_deref(), legacy.as_deref(), AUDIT_TAIL)
}

/// The newest `tail` records across the live log, as many of its rotated
/// generations as that takes, and the legacy file, newest first.
///
/// The writer moves the live file aside by time (`actions::audit::rotate_if_full`), so
/// every record in a generation is older than every record in the files after it.
/// Once `tail` records are in hand an older generation cannot add a newer one, so it
/// is not read; nor is the legacy file, which predates them all. Reading every file
/// whole on each visit to the Jobs tab is what the rotation exists to avoid.
fn read_audit_tail(
    live: Option<&std::path::Path>,
    legacy: Option<&std::path::Path>,
    tail: usize,
) -> Vec<AuditRecord> {
    let mut files: Vec<(std::path::PathBuf, bool)> = Vec::new();
    if let Some(live) = live {
        files.push((live.to_path_buf(), false));
        files.extend(
            crate::actions::audit::rotated_generations(live)
                .into_iter()
                .map(|generation| (generation, false)),
        );
    }
    if let Some(legacy) = legacy {
        files.push((legacy.to_path_buf(), true));
    }
    let mut records = Vec::new();
    for (path, is_legacy) in files {
        if records.len() >= tail {
            break;
        }
        // Each file is append-ordered, oldest first. Reversed, records that share a
        // timestamp keep newest-first order through the stable sort below.
        let mut found = parse_audit_file(&path, is_legacy);
        found.reverse();
        records.extend(found);
    }
    records.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    records.truncate(tail);
    records
}

fn parse_audit_file(path: &std::path::Path, legacy: bool) -> Vec<AuditRecord> {
    let body = match std::fs::read(path) {
        Ok(body) => body,
        // A missing file is the normal state for an install that has never
        // dispatched anything — not worth a warning.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            warn!(?err, path = %path.display(), "could not read the action audit log");
            return Vec::new();
        }
    };
    // Split as bytes and decoded a line at a time. Read as one `String`, a single
    // invalid UTF-8 byte (an append torn inside a multi-byte character, say in a
    // device name) failed the whole read and hid every record in the file.
    body.split(|b| *b == b'\n')
        .filter_map(|line| parse_line(&String::from_utf8_lossy(line), legacy))
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
        window_override: value
            .get("windowOverride")
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

/// Reads the run-history trend, oldest first.
///
/// The app can otherwise only render *now*: each query destructively replaces the
/// single cached result, so "is the backlog shrinking" and "did last night's window
/// work" — the two questions a patching team actually has — were unanswerable.
/// Rollups only, deliberately; see `history` for why storing rows instead would cost
/// about a thousand times more for a capability nobody asked for.
///
/// Only the signed-in tenant's runs: the file holds every tenant this install has
/// queried, and the Trend tab charts whichever record is newest.
#[tauri::command]
pub async fn read_run_history(
    state: State<'_, AppState>,
) -> Result<Vec<crate::history::RunRecord>, UiError> {
    let instance = state.settings_snapshot().instance_base_url;
    let tenant = state.tenant_label();
    tokio::task::spawn_blocking(move || crate::history::read_all(&instance, &tenant))
        .await
        .map_err(|e| UiError::new(format!("reading the run history panicked: {e}")))
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
        assert!(
            !record.window_override,
            "nor is a missing windowOverride an override"
        );
        assert!(record.legacy, "legacy provenance is carried to the view");
    }

    #[test]
    fn a_maintenance_window_override_is_read_back() {
        let line = r#"{"timestamp":"2026-09-02T23:00:00Z","kind":"OS_PATCH_APPLY","deviceName":"srv-01","windowOverride":true}"#;
        assert!(parse_line(line, false).expect("parses").window_override);
    }

    #[test]
    fn a_non_json_line_is_skipped_rather_than_failing_the_read() {
        assert_eq!(parse_line("not json at all", false), None);
        assert_eq!(parse_line("   ", false), None);
    }

    /// A fresh directory for one test's audit files.
    fn audit_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("njp-diag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn record(timestamp: &str, device: &str) -> String {
        format!(r#"{{"timestamp":"{timestamp}","kind":"REBOOT","deviceName":"{device}"}}"#)
    }

    fn write_lines(path: &std::path::Path, lines: &[String]) {
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(path, body).expect("write");
    }

    fn devices(records: &[AuditRecord]) -> Vec<&str> {
        records.iter().map(|r| r.device_name.as_str()).collect()
    }

    /// The file used to be read as one `String`, so one invalid UTF-8 byte (an
    /// append torn inside "Café", say) failed the read and the Jobs tab showed no
    /// history at all. Each line is decoded on its own now.
    #[test]
    fn one_bad_byte_costs_only_its_own_line() {
        let dir = audit_dir("bad-byte");
        let path = dir.join("action-audit.jsonl");
        let mut body = Vec::new();
        body.extend_from_slice(record("2026-09-02 10:00:00 UTC", "Café-01").as_bytes());
        body.push(b'\n');
        body.extend_from_slice(br#"{"timestamp":"2026-09-02 10:05:00 UTC","deviceName":"Caf"#);
        body.push(0xC3); // the first byte of "é", and nothing after it
        body.push(b'\n');
        body.extend_from_slice(record("2026-09-02 10:10:00 UTC", "srv-02").as_bytes());
        body.push(b'\n');
        std::fs::write(&path, body).expect("write");

        let records = parse_audit_file(&path, false);
        assert_eq!(devices(&records), ["Café-01", "srv-02"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The newest records are found across the live file and its generations, newest
    /// first, and a generation older than the tail needs is not read at all. The
    /// oldest one here holds a record stamped later than everything else, which only
    /// a reader that opened it could return.
    #[test]
    fn the_tail_reads_generations_newest_first_and_only_as_far_as_it_needs() {
        let dir = audit_dir("generations");
        let live = dir.join("action-audit.jsonl");
        write_lines(
            &live,
            &[
                record("2026-10-30 09:00:00 UTC", "live-1"),
                record("2026-10-30 10:00:00 UTC", "live-2"),
            ],
        );
        write_lines(
            &dir.join("action-audit-20261029T120000.jsonl"),
            &[
                record("2026-10-29 08:00:00 UTC", "newer-gen-1"),
                record("2026-10-29 11:00:00 UTC", "newer-gen-2"),
            ],
        );
        write_lines(
            &dir.join("action-audit-20260101T000000.jsonl"),
            &[record("2027-01-01 00:00:00 UTC", "never-read")],
        );

        let records = read_audit_tail(Some(&live), None, 3);
        assert_eq!(devices(&records), ["live-2", "live-1", "newer-gen-2"]);

        // Asked for more than the newer files hold, the older generation is read.
        let all = read_audit_tail(Some(&live), None, 10);
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].device_name, "never-read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The legacy file predates everything in the current location, so it fills in
    /// only when those are short, and its records say where they came from.
    #[test]
    fn the_legacy_file_fills_in_only_while_the_tail_is_short() {
        let dir = audit_dir("legacy");
        let live = dir.join("action-audit.jsonl");
        let legacy = dir.join("legacy").join("action-audit.jsonl");
        std::fs::create_dir_all(legacy.parent().expect("parent")).expect("legacy dir");
        write_lines(&live, &[record("2026-10-30 09:00:00 UTC", "current")]);
        write_lines(&legacy, &[record("2026-05-01 09:00:00 UTC", "old-build")]);

        let records = read_audit_tail(Some(&live), Some(&legacy), 10);
        assert_eq!(devices(&records), ["current", "old-build"]);
        assert!(!records[0].legacy && records[1].legacy);

        assert_eq!(
            devices(&read_audit_tail(Some(&live), Some(&legacy), 1)),
            ["current"]
        );
        // An install that never wrote the current file still shows its history.
        let missing = dir.join("nothing-here.jsonl");
        assert_eq!(
            devices(&read_audit_tail(Some(&missing), Some(&legacy), 10)),
            ["old-build"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
