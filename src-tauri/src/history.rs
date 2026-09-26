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
//! cadence. One rollup line is ~400 bytes — the whole [`MAX_LINES`] cap is under
//! 2 MB — and it answers both questions above directly. Per-device history is a real capability
//! and a real database's job; it is not what this file is for, and the cheap version
//! should not pretend otherwise.
//!
//! Best-effort throughout, on the same reasoning as `actions::audit`: a failed
//! history write must never be able to stop an operator from seeing their fleet.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

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

/// How far past [`MAX_LINES`] the file may grow before it is trimmed back.
///
/// Without the slack, a full file was read and rewritten on *every* append — a
/// whole-file rewrite per query, forever, to drop one line. With it the rewrite
/// happens once per `TRIM_SLACK` appends.
const TRIM_SLACK: usize = 500;

/// Serializes every append and trim in this process.
///
/// Queries overlap by design (an auto-refresh tick during a manual Run), and each
/// records from its own `spawn_blocking` task. Unserialized, one task's trim could
/// read the file, the other append a line, and the trim's rename then replace the
/// file with a copy that never saw that line.
static HISTORY_LOCK: Mutex<()> = Mutex::new(());

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
    /// When the query ran — `QueryResult::generated_at`, formatted
    /// `%Y-%m-%d %H:%M:%S UTC` (not RFC 3339). Lexically ordered, which is all the
    /// reader relies on.
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
    /// Detail rows the query produced, across **every** selected status and after
    /// the patch facets (severity, search, first-seen window) — not only pending
    /// rows. Comparable across runs only under the same [`scope_key`](Self::scope_key).
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
    /// Whether a device-tier facet (org/location/role/OS type/OS name) was active.
    /// A filtered run measures a slice, so it is not comparable with a whole-fleet
    /// one either. Patch-tier facets (severity, search, dates) do not set it — the
    /// fleet-health numbers still cover the whole fleet under them.
    pub scoped: bool,
    /// Canonical fingerprint of every facet the query ran under
    /// (`rows::QueryScope::fingerprint`). `scoped` alone could not tell org A's runs
    /// from org B's, so both sat on one trend line. Records written before this
    /// field existed read back as an empty string, which matches no current run.
    #[serde(default)]
    pub scope_key: String,
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
            // From the device tier explicitly. Counting `facets` entries read an
            // unfiltered run as scoped (it carries the whole-fleet line *and* the
            // patch type) and a severity-only run as whole-fleet only by accident.
            scoped: result.scope.device_scoped,
            scope_key: result.scope.fingerprint.clone(),
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
    // Held across the append *and* the trim — see [`HISTORY_LOCK`]. A poisoned lock
    // only means another append panicked; the file itself is still usable.
    let _guard = HISTORY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
            // One write for record + newline: `writeln!` on an unbuffered File is two
            // syscalls, so a crash between them left a line with no terminator and
            // the next append fused onto it.
            let mut record = line.into_bytes();
            record.push(b'\n');
            if let Err(err) = file.write_all(&record) {
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

/// Drops the oldest lines once the file exceeds [`MAX_LINES`] by more than
/// [`TRIM_SLACK`], keeping the newest [`MAX_LINES`].
///
/// Rewrites rather than rotating: this is one small file, and a rotation scheme
/// would mean the reader had to merge across generations for no benefit. The
/// rewrite goes to a sibling temp file that is then renamed over the original, so a
/// crash mid-write leaves the old file intact instead of a truncated one. Callers
/// hold [`HISTORY_LOCK`].
fn trim(path: &Path) {
    let Ok(body) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = body.lines().collect();
    if lines.len() <= MAX_LINES + TRIM_SLACK {
        return;
    }
    let keep = &lines[lines.len() - MAX_LINES..];
    let tmp = path.with_extension("jsonl.tmp");
    let written = (|| -> std::io::Result<()> {
        let mut opts = OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(format!("{}\n", keep.join("\n")).as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(err) = written {
        warn!(?err, "could not trim the run-history file");
        let _ = std::fs::remove_file(&tmp);
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
            scope_key: String::new(),
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

    fn write_lines(path: &Path, n: usize) {
        let lines: Vec<String> = (0..n)
            .map(|i| serde_json::to_string(&rec(&format!("t{i}"), i, 10)).unwrap())
            .collect();
        std::fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
    }

    #[test]
    fn the_file_is_trimmed_from_the_front_once_it_is_well_past_full() {
        let dir = temp_dir("trim");
        let path = dir.join(HISTORY_FILE);
        std::fs::create_dir_all(&dir).unwrap();

        // Full, and inside the slack: no rewrite on every append.
        write_lines(&path, MAX_LINES + TRIM_SLACK);
        trim(&path);
        assert_eq!(
            parse(&std::fs::read_to_string(&path).unwrap()).len(),
            MAX_LINES + TRIM_SLACK,
            "within the slack the file is left alone"
        );

        // One past the slack: trimmed back to the cap, oldest lines dropped.
        let over = MAX_LINES + TRIM_SLACK + 1;
        write_lines(&path, over);
        trim(&path);
        let kept = parse(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(kept.len(), MAX_LINES);
        assert_eq!(kept[0].at, format!("t{}", over - MAX_LINES));
        assert_eq!(kept[kept.len() - 1].at, format!("t{}", over - 1));
        assert!(
            !path.with_extension("jsonl.tmp").exists(),
            "the temp file is renamed into place, not left behind"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600,
                "the rewritten file keeps the owner-only mode"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Concurrent appends across a trim boundary must not lose a record: every
    /// line written after the trim is still there.
    #[test]
    fn concurrent_appends_across_a_trim_lose_nothing() {
        let dir = temp_dir("concurrent");
        let path = dir.join(HISTORY_FILE);
        std::fs::create_dir_all(&dir).unwrap();
        write_lines(&path, MAX_LINES + TRIM_SLACK);

        let threads: Vec<_> = (0..8)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..10 {
                        append(&path, &rec(&format!("new-{t}-{i}"), 0, 10));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        let kept = parse(&std::fs::read_to_string(&path).unwrap());
        let new = kept.iter().filter(|r| r.at.starts_with("new-")).count();
        assert_eq!(new, 80, "every concurrent append survived the trim");
        assert!(kept.len() <= MAX_LINES + TRIM_SLACK);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lines written before `scopeKey` existed must still read back.
    #[test]
    fn a_record_from_before_the_scope_key_still_parses() {
        let mut old = serde_json::to_value(rec("2026-01-01 00:00:00 UTC", 5, 10)).unwrap();
        old.as_object_mut().unwrap().remove("scopeKey");
        let parsed = parse(&old.to_string());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].scope_key, "");
    }

    /// `scoped` means a *device* facet narrowed the run. It used to count facet
    /// lines, so an unfiltered run (whole-fleet line + patch type) read as scoped
    /// and a severity-only run read as whole-fleet.
    #[test]
    fn from_result_takes_the_device_scope_from_the_query_scope() {
        use crate::filter::FilterParams;
        use crate::model::PatchStatus;
        use crate::rows::{LookupMaps, PatchFamilies, build_query_scope};

        let maps = LookupMaps::build(&[], &[], &[]);
        let families = PatchFamilies {
            os: true,
            software: true,
        };
        let run = |filter: FilterParams| {
            let result = QueryResult {
                rows: Vec::new(),
                devices: Vec::new(),
                compliance: Vec::new(),
                compliance_by_os: Vec::new(),
                failures: Vec::new(),
                severity_by_org: Vec::new(),
                age_buckets: Vec::new(),
                devices_total: 0,
                devices_offline: 0,
                devices_unpatchable: 0,
                patch_families: families,
                scope: build_query_scope(&filter, &maps, families, &[PatchStatus::Pending], None),
                generated_at: "2026-09-01 10:00:00 UTC".into(),
                data_fetched_at: "2026-09-01 10:00:00 UTC".into(),
            };
            RunRecord::from_result(&result, "https://app.ninjarmm.com")
        };

        let whole = run(FilterParams::default());
        assert!(!whole.scoped, "an unfiltered run is the whole fleet");

        let severity_only = run(FilterParams {
            severities: vec!["CRITICAL".into()],
            ..Default::default()
        });
        assert!(!severity_only.scoped, "a patch facet is not a device scope");
        assert_ne!(
            severity_only.scope_key, whole.scope_key,
            "but it counts different rows, so it is a different series"
        );

        let org = |id: i64| {
            run(FilterParams {
                organization_ids: vec![id],
                ..Default::default()
            })
        };
        let (org_a, org_b) = (org(1), org(2));
        assert!(org_a.scoped && org_b.scoped);
        assert_ne!(org_a.scope_key, org_b.scope_key, "two orgs, two series");
        assert_eq!(org_a.scope_key, org(1).scope_key, "one scope, one series");
    }
}
