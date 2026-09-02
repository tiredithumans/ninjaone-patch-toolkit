//! Append-only record of what each completed query measured.
//!
//! Every question a patching team actually has is a delta question — is the backlog
//! shrinking, did last night's window land, is this org regressing — and the app
//! could only ever render *now*. `store_last_result_if_current` destructively
//! replaces one slot, so the previous query was gone the moment the next one
//! finished, and nothing on disk held fleet state at all.
//!
//! **This deliberately stores rollups, not rows.** Storing snapshots of the joined
//! row set would answer more questions (which patch regressed on which machine last
//! Tuesday) and cost about a thousand times more: a normalized row snapshot of a
//! large fleet measures ~16 MB, and the app's own auto-refresh offers a 1-minute
//! cadence. One rollup line is ~15 KB — 5.6 MB per *year* at daily cadence — and it
//! answers both questions above directly. Per-device history is a real capability
//! and a real database's job; it is not what this file is for, and the cheap version
//! should not pretend otherwise.
//!
//! Best-effort throughout, on the same reasoning as `actions::audit`: a failed
//! history write must never be able to stop an operator from seeing their fleet.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::paths;
use crate::rows::QueryResult;

const HISTORY_FILE: &str = "run-history.jsonl";

/// Lines kept before the file is trimmed from the front.
///
/// At a 15-minute cadence round the clock this is about five weeks, which covers
/// "did last month's patch window work" without letting an install that never
/// closes the app grow the file without bound.
const MAX_LINES: usize = 4_000;

/// One completed query's fleet-health numbers.
///
/// A projection of [`QueryResult`], not the type itself: this is written by every
/// version of the app from now on and read back by every later one, so it holds only
/// scalars whose meaning is stable. Nothing derived is stored — compliance
/// percentages, family labels and the rule for which runs belong on one trend line
/// are all computed on read, in `web-rs`, where they are used and tested. Freezing a
/// derived value here would mean a fix to that rule could not reach records already
/// on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// When the query ran (RFC 3339, UTC).
    pub at: String,
    /// The instance the numbers describe. A record from another tenant must never
    /// be charted as this one's history.
    pub instance: String,
    /// Devices in scope, and the two exclusions every compliance surface states.
    pub devices_total: usize,
    pub devices_offline: usize,
    pub devices_unpatchable: usize,
    /// Devices with no pending patches, over the `rows::rollup_device` population —
    /// the same numerator and denominator the Compliance tab shows.
    pub devices_compliant: usize,
    pub devices_in_scope: usize,
    /// Total pending detail rows behind those numbers.
    pub rows_total: usize,
    /// Pending criticals, and the subset past the SLA window.
    pub pending_critical: usize,
    pub aged_critical: usize,
    /// Distinct patches with at least one FAILED install.
    pub failures: usize,
    /// Devices flagged as needing a reboot.
    pub needs_reboot: usize,
    /// Which patch families the numbers cover. A run scoped to OS patches only is
    /// not comparable with an ALL run, and a trend that silently mixes them lies.
    pub os_patches: bool,
    pub software_patches: bool,
    /// Whether a device scope was active. A filtered run measures a slice, so it is
    /// not comparable with a whole-fleet one either.
    pub scoped: bool,
}

impl RunRecord {
    /// Projects a completed result. `instance` comes from settings rather than the
    /// result, which carries no tenant of its own.
    pub fn from_result(result: &QueryResult, instance: &str) -> Self {
        let devices_compliant = result.compliance.iter().map(|c| c.devices_compliant).sum();
        let devices_in_scope = result.compliance.iter().map(|c| c.devices_total).sum();
        Self {
            at: result.generated_at.clone(),
            instance: instance.to_string(),
            devices_total: result.devices_total,
            devices_offline: result.devices_offline,
            devices_unpatchable: result.devices_unpatchable,
            devices_compliant,
            devices_in_scope,
            rows_total: result.rows.len(),
            pending_critical: result.compliance.iter().map(|c| c.pending_critical).sum(),
            aged_critical: result.compliance.iter().map(|c| c.aged_critical).sum(),
            failures: result.failures.len(),
            needs_reboot: result.devices.iter().filter(|d| d.needs_reboot).count(),
            os_patches: result.patch_families.os,
            software_patches: result.patch_families.software,
            // `facets` always carries at least the patch type, so "scoped" means the
            // operator narrowed beyond that — the second entry onwards.
            scoped: result.scope.facets.len() > 1,
        }
    }
}

fn history_path() -> Option<std::path::PathBuf> {
    paths::app_dir().ok().map(|d| d.join(HISTORY_FILE))
}

/// Appends one record. Never fails the caller.
///
/// **Synchronous file I/O — call from `spawn_blocking`, not an async task.**
pub fn record(entry: &RunRecord) {
    let Some(path) = history_path() else {
        warn!("no config directory available; run-history record dropped");
        return;
    };
    append(&path, entry);
}

/// The half of [`record`] that takes its destination, so the append, the trim and
/// the 0600 mode are testable without touching the real config directory.
fn append(path: &Path, entry: &RunRecord) {
    let line = match serde_json::to_string(entry) {
        Ok(l) => l,
        Err(err) => {
            warn!(?err, "could not serialize a run-history record");
            return;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        warn!(?err, "could not create the run-history directory");
        return;
    }
    // Owner-only, for the same reason the audit log is: these numbers name the
    // operator's organizations and the size of their unpatched backlog.
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut file) => {
            if let Err(err) = writeln!(file, "{line}") {
                warn!(?err, path = %path.display(), "could not append run history");
                return;
            }
        }
        Err(err) => {
            warn!(?err, path = %path.display(), "could not open the run-history file");
            return;
        }
    }
    trim(path);
}

/// Drops the oldest lines once the file exceeds [`MAX_LINES`].
///
/// Rewrites in place rather than rotating: this is one small file, and a rotation
/// scheme would mean the reader had to merge across generations for no benefit.
fn trim(path: &Path) {
    let Ok(body) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = body.lines().collect();
    if lines.len() <= MAX_LINES {
        return;
    }
    let keep = &lines[lines.len() - MAX_LINES..];
    if let Err(err) = std::fs::write(path, format!("{}\n", keep.join("\n"))) {
        warn!(?err, "could not trim the run-history file");
    }
}

/// Reads every record, oldest first. A malformed line is skipped rather than
/// failing the read — the file is append-only and a crash can tear the last line.
pub fn read_all() -> Vec<RunRecord> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    parse(&std::fs::read_to_string(&path).unwrap_or_default())
}

fn parse(body: &str) -> Vec<RunRecord> {
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(at: &str, compliant: usize, scope: usize) -> RunRecord {
        RunRecord {
            at: at.into(),
            instance: "https://app.ninjarmm.com".into(),
            devices_total: 100,
            devices_offline: 5,
            devices_unpatchable: 3,
            devices_compliant: compliant,
            devices_in_scope: scope,
            rows_total: 40,
            pending_critical: 7,
            aged_critical: 2,
            failures: 1,
            needs_reboot: 4,
            os_patches: true,
            software_patches: true,
            scoped: false,
        }
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("npt-history-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn records_append_and_read_back_in_order() {
        let dir = temp_dir("append");
        let path = dir.join(HISTORY_FILE);
        append(&path, &rec("2026-09-01T10:00:00Z", 5, 10));
        append(&path, &rec("2026-09-02T10:00:00Z", 7, 10));

        let body = std::fs::read_to_string(&path).expect("history file");
        let parsed = parse(&body);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].at, "2026-09-01T10:00:00Z", "oldest first");
        assert_eq!(parsed[1].devices_compliant, 7);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600,
                "the backlog size of an operator's fleet is not world-readable"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_torn_final_line_does_not_hide_the_history_above_it() {
        let good = serde_json::to_string(&rec("2026-09-01T10:00:00Z", 5, 10)).unwrap();
        let parsed = parse(&format!("{good}\n{{\"at\":\"2026-09-02T\n"));
        assert_eq!(parsed.len(), 1, "one torn line costs only itself");
    }

    #[test]
    fn the_file_is_trimmed_from_the_front_once_it_is_full() {
        let dir = temp_dir("trim");
        let path = dir.join(HISTORY_FILE);
        std::fs::create_dir_all(&dir).unwrap();
        // One over the cap, each identifiable by its timestamp.
        let lines: Vec<String> = (0..=MAX_LINES)
            .map(|i| serde_json::to_string(&rec(&format!("t{i}"), i, 10)).unwrap())
            .collect();
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

        trim(&path);

        let kept = parse(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(kept.len(), MAX_LINES);
        assert_eq!(kept[0].at, "t1", "the oldest line is the one dropped");
        assert_eq!(kept[kept.len() - 1].at, format!("t{MAX_LINES}"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
