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
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

/// Organizations kept per record, largest (by devices in scope) first.
///
/// One line is ~400 bytes; a per-org entry is ~60. Uncapped, an MSP with a few
/// hundred organizations would write tens of kilobytes per query and push the
/// [`MAX_LINES`] file toward a hundred megabytes. At this cap the worst line is
/// ~6 KB. [`RunRecord::orgs_total`] records how many were cut.
const MAX_ORGS: usize = 100;

/// Records, newest first, whose per-org detail [`read_all`] returns.
///
/// The Trend tab's per-org table compares the newest run with the previous
/// comparable one; it never reads per-org numbers from further back. Returning them
/// for every line would push up to the whole file's org detail over IPC to render
/// two rows of it. Two hundred lines leaves room for other scopes run in between.
const ORG_DETAIL_RECORDS: usize = 200;

/// The calculation method a run's fleet-health figures were computed under. A run
/// is only charted against runs of the same method (`RunRecord::comparable_with` in
/// `web-rs`), because a change in what a figure *means* would otherwise read as the
/// fleet moving.
///
/// - **0** — every line written before the field existed: "compliant" was zero
///   pending of any severity, the urgent backlog was Critical and Important only,
///   and every band had an SLA window.
/// - **1** — Within SLA is recorded, Security counts as urgent, a band can have No
///   SLA (Optional by default), and first seen is the earlier of NinjaOne's time and
///   this app's own first observation.
pub const METHOD: u32 = 1;

/// Serializes every append and trim in this process, and holds the line count of
/// the file it last touched.
///
/// Queries overlap by design (an auto-refresh tick during a manual Run), and each
/// records from its own `spawn_blocking` task. Unserialized, one task's trim could
/// read the file, the other append a line, and the trim's rename then replace the
/// file with a copy that never saw that line.
///
/// The count is what decides when to trim. Counting by reading the file back after
/// every append read up to ~27 MB per query (4,500 lines of an MSP's per-org detail)
/// to learn one number; now the file is counted once per process and then tracked.
static HISTORY_LOCK: Mutex<Option<LineCount>> = Mutex::new(None);

/// Lines in the file at `path`, as of the last append or trim through this process.
struct LineCount {
    path: PathBuf,
    lines: usize,
}

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
    /// The tenant — instance *and* client id, hashed ([`tenant_key`]) — the numbers
    /// describe. Two client ids on one instance are two tenants, and with only
    /// `instance` to go on their runs shared one trend line and one org table.
    /// Empty on lines written before it existed, which [`read_all`] matches by
    /// `instance` alone, as before.
    #[serde(default)]
    pub tenant: String,
    /// Devices in scope, and the two exclusions every compliance surface states.
    pub devices_total: usize,
    pub devices_offline: usize,
    pub devices_unpatchable: usize,
    /// Devices with no pending patches (Fully patched), over the
    /// `rows::rollup_device` population — the same numerator and denominator the
    /// Compliance tab shows.
    pub devices_compliant: usize,
    pub devices_in_scope: usize,
    /// Devices Within SLA, over the same population. `None` on lines written before
    /// [`METHOD`] 1.
    #[serde(default)]
    pub devices_within_sla: Option<usize>,
    /// Pending records of every severity over the same population — the fleet
    /// backlog. `None` on lines written before [`METHOD`] 1, which recorded only
    /// `rows_total`, a different number.
    #[serde(default)]
    pub pending_total: Option<usize>,
    /// The [`METHOD`] this line's figures were computed under; 0 on older lines.
    #[serde(default)]
    pub method: u32,
    /// Detail rows the query produced, across **every** selected status and after
    /// the patch facets (severity, search, first-seen window) — not only pending
    /// rows. Comparable across runs only under the same [`scope_key`](Self::scope_key).
    pub rows_total: usize,
    /// Pending urgent patches (Critical, Important and — from calculation method 1 —
    /// Security), and the subset past the SLA window. Stored under their original
    /// keys so every line already on disk still reads.
    #[serde(rename = "pendingCritical")]
    pub urgent_pending: usize,
    #[serde(rename = "agedCritical")]
    pub urgent_past_sla: usize,
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
    /// Per-organization rollup, at most [`MAX_ORGS`] entries. Empty on lines
    /// written before it existed, and on lines [`read_all`] returns without detail.
    #[serde(default)]
    pub orgs: Vec<OrgRun>,
    /// Organizations in the result before the [`MAX_ORGS`] cap.
    #[serde(default)]
    pub orgs_total: usize,
}

/// One organization's numbers in one run — the same population and counts as its
/// Compliance-tab row. Keys are short because this repeats per org on every line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgRun {
    #[serde(rename = "o")]
    pub organization: String,
    #[serde(rename = "n", default)]
    pub devices_in_scope: usize,
    #[serde(rename = "c", default)]
    pub devices_compliant: usize,
    /// Devices Within SLA; `None` on lines written before [`METHOD`] 1.
    #[serde(rename = "w", default)]
    pub devices_within_sla: Option<usize>,
    /// Pending patches of every severity (`rows::build_severity_by_org`).
    #[serde(rename = "p", default)]
    pub pending: usize,
    #[serde(rename = "pc", default)]
    pub urgent_pending: usize,
    #[serde(rename = "ac", default)]
    pub urgent_past_sla: usize,
}

/// The stamp a record carries for `tenant_label` (instance + client id, as
/// `QueryToken::tenant_label` spells it): a truncated SHA-256, since the line only
/// needs to tell tenants apart and the client id has no business in it.
pub fn tenant_key(tenant_label: &str) -> String {
    let digest = Sha256::digest(tenant_label.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

impl RunRecord {
    /// Projects a completed result. `instance` comes from settings rather than the
    /// result, which carries no tenant of its own; `tenant_label` is the tenant the
    /// rows were fetched under.
    pub fn from_result(result: &QueryResult, instance: &str, tenant_label: &str) -> Self {
        let total = &result.compliance_total;
        let pending_by_org: std::collections::HashMap<&str, usize> = result
            .severity_by_org
            .iter()
            .map(|o| (o.organization.as_str(), o.counts.total()))
            .collect();
        let mut orgs: Vec<OrgRun> = result
            .compliance
            .iter()
            .map(|c| OrgRun {
                organization: c.organization.clone(),
                devices_in_scope: c.devices_total,
                devices_compliant: c.devices_fully_patched,
                devices_within_sla: Some(c.devices_within_sla),
                pending: pending_by_org
                    .get(c.organization.as_str())
                    .copied()
                    .unwrap_or(0),
                urgent_pending: c.urgent_pending,
                urgent_past_sla: c.urgent_past_sla,
            })
            .collect();
        orgs.sort_by(|a, b| {
            b.devices_in_scope
                .cmp(&a.devices_in_scope)
                .then_with(|| crate::rows::cmp_ci(&a.organization, &b.organization))
        });
        let orgs_total = orgs.len();
        orgs.truncate(MAX_ORGS);

        Self {
            at: result.generated_at.clone(),
            instance: instance.to_string(),
            tenant: tenant_key(tenant_label),
            devices_total: result.devices_total,
            devices_offline: result.devices_offline,
            devices_unpatchable: result.devices_unpatchable,
            devices_compliant: total.devices_fully_patched,
            devices_in_scope: total.devices_total,
            devices_within_sla: Some(total.devices_within_sla),
            pending_total: Some(
                result
                    .severity_by_org
                    .iter()
                    .map(|o| o.counts.total())
                    .sum(),
            ),
            method: METHOD,
            rows_total: result.rows.len(),
            urgent_pending: total.urgent_pending,
            urgent_past_sla: total.urgent_past_sla,
            failures: result.failures.len(),
            needs_reboot: result.devices.iter().filter(|d| d.needs_reboot).count(),
            os_patches: result.patch_families.os,
            software_patches: result.patch_families.software,
            // From the device tier explicitly. Counting `facets` entries read an
            // unfiltered run as scoped (it carries the whole-fleet line *and* the
            // patch type) and a severity-only run as whole-fleet only by accident.
            scoped: result.scope.device_scoped,
            scope_key: result.scope.fingerprint.clone(),
            orgs,
            orgs_total,
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
    let mut count = HISTORY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    opts.create(true).append(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut file) => {
            // One write for record + newline: `writeln!` on an unbuffered File is two
            // syscalls, so a crash between them left a line with no terminator. A
            // line a crash *did* tear gets its end first, or this record would fuse
            // onto it and be lost with it.
            let mut record = Vec::with_capacity(line.len() + 2);
            if !ends_with_newline(&mut file) {
                record.push(b'\n');
            }
            record.extend_from_slice(line.as_bytes());
            record.push(b'\n');
            if let Err(err) = file.write_all(&record) {
                warn!(?err, path = %path.display(), "could not append run history");
                *count = None;
                return;
            }
        }
        Err(err) => {
            warn!(?err, path = %path.display(), "could not open the run-history file");
            return;
        }
    }
    let lines = match count.as_ref().filter(|c| c.path == path) {
        Some(c) => c.lines + 1,
        // Counted once per process (or per file, in tests); tracked from then on.
        None => std::fs::read(path).map_or(0, |body| lines(&body).count()),
    };
    let lines = if lines > MAX_LINES + TRIM_SLACK {
        trim(path).unwrap_or(lines)
    } else {
        lines
    };
    *count = Some(LineCount {
        path: path.to_path_buf(),
        lines,
    });
}

/// Whether the file is empty or ends in a newline — a crash can leave its last line
/// without one. An unreadable end is treated as fine: the append goes ahead.
fn ends_with_newline(file: &mut std::fs::File) -> bool {
    if matches!(file.seek(SeekFrom::End(0)), Ok(0) | Err(_)) {
        return true;
    }
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1))
        .and_then(|_| file.read_exact(&mut last))
        .map_or(true, |()| last[0] == b'\n')
}

/// The file's non-blank lines, as bytes. Split before decoding, so one invalid UTF-8
/// byte costs its own line rather than the whole file.
fn lines(body: &[u8]) -> impl Iterator<Item = &[u8]> {
    body.split(|b| *b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
}

/// Drops the oldest lines once the file exceeds [`MAX_LINES`] by more than
/// [`TRIM_SLACK`], keeping the newest [`MAX_LINES`]. Returns how many lines the
/// file holds afterwards, or `None` if it could not be read or rewritten.
///
/// Rewrites rather than rotating: this is one small file, and a rotation scheme
/// would mean the reader had to merge across generations for no benefit. The
/// rewrite goes to a sibling temp file that is then renamed over the original, so a
/// crash mid-write leaves the old file intact instead of a truncated one. Callers
/// hold [`HISTORY_LOCK`].
fn trim(path: &Path) -> Option<usize> {
    let body = std::fs::read(path).ok()?;
    let lines: Vec<&[u8]> = lines(&body).collect();
    if lines.len() <= MAX_LINES + TRIM_SLACK {
        return Some(lines.len());
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
        let mut file = std::io::BufWriter::new(opts.open(&tmp)?);
        for line in keep {
            file.write_all(line)?;
            file.write_all(b"\n")?;
        }
        file.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    match written {
        Ok(()) => Some(keep.len()),
        Err(err) => {
            warn!(?err, "could not trim the run-history file");
            let _ = std::fs::remove_file(&tmp);
            None
        }
    }
}

/// Reads this tenant's records, oldest first: the lines stamped with its key
/// ([`tenant_key`]), plus lines from before the stamp existed whose instance
/// matches — the only test those lines can be held to. A malformed line is skipped
/// rather than failing the read: the file is append-only and a crash can tear the
/// last line, mid-character even.
pub fn read_all(instance: &str, tenant_label: &str) -> Vec<RunRecord> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    read_from(&path, instance, tenant_label)
}

/// [`read_all`] from a given file.
fn read_from(path: &Path, instance: &str, tenant_label: &str) -> Vec<RunRecord> {
    let key = tenant_key(tenant_label);
    let mut records: Vec<RunRecord> = parse(std::fs::read(path).unwrap_or_default())
        .into_iter()
        .filter(|r| r.tenant == key || (r.tenant.is_empty() && r.instance == instance))
        .collect();
    strip_old_org_detail(&mut records);
    records
}

/// Drops the per-org detail from all but the newest [`ORG_DETAIL_RECORDS`].
fn strip_old_org_detail(records: &mut [RunRecord]) {
    let old = records.len().saturating_sub(ORG_DETAIL_RECORDS);
    for record in &mut records[..old] {
        record.orgs = Vec::new();
    }
}

fn parse(body: impl AsRef<[u8]>) -> Vec<RunRecord> {
    lines(body.as_ref())
        .filter_map(|l| serde_json::from_str(&String::from_utf8_lossy(l)).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TENANT: &str = "https://app.ninjarmm.com\nclient-a";

    fn rec(at: &str, compliant: usize, scope: usize) -> RunRecord {
        RunRecord {
            at: at.into(),
            instance: "https://app.ninjarmm.com".into(),
            tenant: tenant_key(TENANT),
            devices_total: 100,
            devices_offline: 5,
            devices_unpatchable: 3,
            devices_compliant: compliant,
            devices_in_scope: scope,
            devices_within_sla: Some(compliant),
            pending_total: Some(40),
            method: METHOD,
            rows_total: 40,
            urgent_pending: 7,
            urgent_past_sla: 2,
            failures: 1,
            needs_reboot: 4,
            os_patches: true,
            software_patches: true,
            scoped: false,
            scope_key: String::new(),
            orgs: Vec::new(),
            orgs_total: 0,
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
        let parsed = parse(format!("{good}\n{{\"at\":\"2026-09-02T\n"));
        assert_eq!(parsed.len(), 1, "one torn line costs only itself");
    }

    /// The Trend tab reads one tenant's runs: two client ids on one instance are two
    /// tenants, and a line from before the stamp is matched by its instance alone.
    #[test]
    fn the_history_read_is_one_tenants() {
        let dir = temp_dir("tenant");
        let path = dir.join(HISTORY_FILE);
        let other = "https://app.ninjarmm.com\nclient-b";
        append(&path, &rec("t1", 1, 10));
        append(
            &path,
            &RunRecord {
                tenant: tenant_key(other),
                ..rec("t2", 2, 10)
            },
        );
        append(
            &path,
            &RunRecord {
                tenant: String::new(),
                ..rec("t0-legacy", 3, 10)
            },
        );
        append(
            &path,
            &RunRecord {
                tenant: String::new(),
                instance: "https://eu.ninjarmm.com".into(),
                ..rec("eu-legacy", 4, 10)
            },
        );
        let ats = |records: Vec<RunRecord>| records.into_iter().map(|r| r.at).collect::<Vec<_>>();
        assert_eq!(
            ats(read_from(&path, "https://app.ninjarmm.com", TENANT)),
            vec!["t1", "t0-legacy"]
        );
        assert_eq!(
            ats(read_from(&path, "https://app.ninjarmm.com", other)),
            vec!["t2", "t0-legacy"]
        );
        assert!(
            read_from(
                &path,
                "https://us2.ninjarmm.com",
                "https://us2.ninjarmm.com\n"
            )
            .is_empty()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One invalid UTF-8 byte — a crash tearing a multi-byte org name — costs its own
    /// line, not the whole history; and the next record is not fused onto the torn
    /// line, it starts its own.
    #[test]
    fn a_torn_multibyte_line_costs_only_itself() {
        let dir = temp_dir("utf8");
        let path = dir.join(HISTORY_FILE);
        append(&path, &rec("t1", 1, 10));
        let mut torn = std::fs::read(&path).unwrap();
        torn.extend_from_slice(b"{\"at\":\"Z\xc3");
        std::fs::write(&path, &torn).unwrap();
        append(&path, &rec("t2", 2, 10));
        let ats: Vec<String> = read_from(&path, "https://app.ninjarmm.com", TENANT)
            .into_iter()
            .map(|r| r.at)
            .collect();
        assert_eq!(ats, vec!["t1", "t2"]);
        let _ = std::fs::remove_dir_all(&dir);
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
            parse(std::fs::read_to_string(&path).unwrap()).len(),
            MAX_LINES + TRIM_SLACK,
            "within the slack the file is left alone"
        );

        // One past the slack: trimmed back to the cap, oldest lines dropped.
        let over = MAX_LINES + TRIM_SLACK + 1;
        write_lines(&path, over);
        trim(&path);
        let kept = parse(std::fs::read_to_string(&path).unwrap());
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

        let kept = parse(std::fs::read_to_string(&path).unwrap());
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
        let parsed = parse(old.to_string());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].scope_key, "");
    }

    /// A line from before the calculation method was versioned reads back as method
    /// 0 with no Within SLA or pending total — never as a method-1 line whose
    /// figures happen to be zero — and keeps its urgent counts under the keys it was
    /// written with.
    #[test]
    fn a_record_from_before_the_method_version_reads_as_method_zero() {
        let mut old = serde_json::to_value(rec("2026-01-01 00:00:00 UTC", 5, 10)).unwrap();
        let obj = old.as_object_mut().unwrap();
        for key in ["method", "devicesWithinSla", "pendingTotal"] {
            obj.remove(key);
        }
        assert_eq!(obj["pendingCritical"], 7, "the original on-disk key");
        let parsed = parse(old.to_string());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].method, 0);
        assert_eq!(parsed[0].devices_within_sla, None);
        assert_eq!(parsed[0].pending_total, None);
        assert_eq!(parsed[0].urgent_pending, 7);
    }

    /// Lines written before the per-org rollup existed read back with none.
    #[test]
    fn a_record_from_before_the_org_rollup_still_parses() {
        let mut old = serde_json::to_value(rec("2026-01-01 00:00:00 UTC", 5, 10)).unwrap();
        old.as_object_mut().unwrap().remove("orgs");
        old.as_object_mut().unwrap().remove("orgsTotal");
        let parsed = parse(old.to_string());
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].orgs.is_empty());
        assert_eq!(parsed[0].orgs_total, 0);
    }

    #[test]
    fn only_the_newest_records_keep_their_org_detail() {
        let org = OrgRun {
            organization: "Contoso".into(),
            devices_in_scope: 10,
            devices_compliant: 5,
            devices_within_sla: Some(8),
            pending: 3,
            urgent_pending: 1,
            urgent_past_sla: 0,
        };
        let mut records: Vec<RunRecord> = (0..ORG_DETAIL_RECORDS + 3)
            .map(|i| RunRecord {
                orgs: vec![org.clone()],
                ..rec(&format!("t{i}"), 5, 10)
            })
            .collect();
        strip_old_org_detail(&mut records);
        assert!(records[..3].iter().all(|r| r.orgs.is_empty()));
        assert!(records[3..].iter().all(|r| r.orgs.len() == 1));
    }

    /// The per-org rollup is the Compliance tab's rows plus each org's pending
    /// count, largest orgs first, capped.
    #[test]
    fn from_result_carries_a_capped_per_org_rollup() {
        use crate::rows::{ComplianceBucket, OrgSeverity, PatchFamilies, SeverityCounts};
        let bucket = |i: usize| ComplianceBucket {
            organization: format!("org-{i:03}"),
            devices_total: i,
            devices_within_sla: i / 2,
            within_sla_pct: 50.0,
            devices_fully_patched: i / 2,
            fully_patched_pct: 50.0,
            urgent_pending: 1,
            urgent_past_sla: 0,
            awaiting_approval: 0,
            approved_not_installed: 0,
        };
        let result = QueryResult {
            rows: Vec::new(),
            devices: Vec::new(),
            compliance: (1..=MAX_ORGS + 5).map(bucket).collect(),
            compliance_total: Default::default(),
            uncounted_orgs: Vec::new(),
            compliance_by_os: Vec::new(),
            failures: Vec::new(),
            severity_by_org: vec![OrgSeverity {
                organization: format!("org-{:03}", MAX_ORGS + 5),
                counts: SeverityCounts {
                    critical: 2,
                    low: 3,
                    ..Default::default()
                },
            }],
            age_buckets: Vec::new(),
            worst_devices: Default::default(),
            offline_backlog: Default::default(),
            time_to_install: Default::default(),
            first_seen: Default::default(),
            sla_policy: Default::default(),
            instance: String::new(),
            approvals: Default::default(),
            devices_total: 0,
            devices_offline: 0,
            devices_unpatchable: 0,
            patch_families: PatchFamilies {
                os: true,
                software: true,
            },
            scope: Default::default(),
            changes: Default::default(),
            generated_at: "2026-09-01 10:00:00 UTC".into(),
            data_fetched_at: "2026-09-01 10:00:00 UTC".into(),
            exploit_data: Default::default(),
        };
        let record = RunRecord::from_result(&result, "https://app.ninjarmm.com", TENANT);
        assert_eq!(record.orgs_total, MAX_ORGS + 5);
        assert_eq!(record.orgs.len(), MAX_ORGS);
        let largest = &record.orgs[0];
        assert_eq!(largest.devices_in_scope, MAX_ORGS + 5, "largest first");
        assert_eq!(largest.pending, 5, "pending sums every severity band");
        assert_eq!(record.orgs[1].pending, 0, "an org with no backlog has none");
        assert!(
            record.orgs.iter().all(|o| o.devices_in_scope > 5),
            "the smallest orgs are the ones cut"
        );
    }

    /// `scoped` means a *device* facet narrowed the run. It used to count facet
    /// lines, so an unfiltered run (whole-fleet line + patch type) read as scoped
    /// and a severity-only run read as whole-fleet.
    #[test]
    fn from_result_takes_the_device_scope_from_the_query_scope() {
        use crate::filter::FilterParams;
        use crate::model::PatchStatus;
        use crate::rows::{LookupMaps, PatchFamilies, build_query_scope};

        let maps = LookupMaps::build(&[], &[], &[], &[]);
        let families = PatchFamilies {
            os: true,
            software: true,
        };
        let run = |filter: FilterParams| {
            let result = QueryResult {
                rows: Vec::new(),
                devices: Vec::new(),
                compliance: Vec::new(),
                compliance_total: Default::default(),
                uncounted_orgs: Vec::new(),
                compliance_by_os: Vec::new(),
                failures: Vec::new(),
                severity_by_org: Vec::new(),
                age_buckets: Vec::new(),
                worst_devices: Default::default(),
                offline_backlog: Default::default(),
                time_to_install: Default::default(),
                first_seen: Default::default(),
                sla_policy: Default::default(),
                instance: "https://app.ninjarmm.com".into(),
                approvals: Default::default(),
                devices_total: 0,
                devices_offline: 0,
                devices_unpatchable: 0,
                patch_families: families,
                scope: build_query_scope(&filter, &maps, families, &[PatchStatus::Pending], None),
                changes: Default::default(),
                generated_at: "2026-09-01 10:00:00 UTC".into(),
                data_fetched_at: "2026-09-01 10:00:00 UTC".into(),
                exploit_data: Default::default(),
            };
            RunRecord::from_result(&result, "https://app.ninjarmm.com", TENANT)
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
