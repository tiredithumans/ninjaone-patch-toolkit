//! "Changed since last run": which patches became pending, stopped being pending,
//! or newly failed between this query and the previous *comparable* one.
//!
//! `history` answers "is the backlog shrinking" with one rollup line per query; it
//! deliberately holds no rows, so it cannot say *which* patches moved. This module
//! keeps the smallest thing that can: per scope, the set of `(device, patch)`
//! identities that were pending and that had failed on the last stored run, plus
//! enough display text for the pending ones that a resolved patch — absent from the
//! current rows by definition — can still be listed by name.
//!
//! **Comparable** means same tenant, same facets ([`scope_key`]: the
//! `QueryScope::fingerprint`, the patch families and the install lookback). Diffing
//! org A against org B would report org A's whole backlog as resolved.
//!
//! One file per scope under `run-snapshots/`, rewritten atomically after a result
//! wins the cache, capped by count and bytes (oldest mtime evicted first).
//! Best-effort throughout, like `history`: a failed snapshot costs the next run
//! its diff, never the operator their fleet.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::model::{PatchRow, PatchStatus};
use crate::paths;
use crate::rows::{PatchFamilies, TableCell, TableColumn, cmp_ci};

const SNAPSHOT_DIR: &str = "run-snapshots";

/// Bumped when the on-disk shape changes; a file of another version reads as "no
/// previous run" rather than as a misparsed one.
const SNAPSHOT_VERSION: u32 = 1;

/// Scopes remembered at once. An operator flips between a handful of saved presets;
/// twenty covers that with room, and each extra scope is one more file to keep.
const MAX_SNAPSHOTS: usize = 20;

/// Byte budget for the whole directory. A whole-fleet third-party scope can hold a
/// six-figure pending set (~11 bytes an item), so the count cap alone does not bound
/// the disk. The newest snapshot is always kept, even alone over budget.
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Identities one snapshot will hold. Past this the run is reported as too large to
/// track rather than written as a multi-hundred-megabyte file.
const MAX_ITEMS: usize = 1_000_000;

/// Entries per list on the wire. The counts are always exact; the lists are for
/// reading, and a six-figure "resolved" list is not read.
pub const CHANGE_LIST_LIMIT: usize = 200;

/// Serializes every write + prune in this process: two overlapping queries of
/// different scopes must not have one's prune delete the other's temp file.
static SNAPSHOT_LOCK: Mutex<()> = Mutex::new(());

/// The one definition of "the same patch" across runs. Everything that compares
/// runs goes through it, so it can adopt NinjaOne's `productIdentifier` for
/// third-party patches in one place once the row carries it.
///
/// An OS patch is its KB (normalized: NinjaOne spells it with and without the `KB`
/// prefix); a KB-less OS patch and a third-party patch fall back to the title.
pub fn patch_key(row: &PatchRow) -> String {
    let kb = row
        .kb
        .as_deref()
        .map(str::trim)
        .map(|k| {
            k.strip_prefix("KB")
                .or_else(|| k.strip_prefix("kb"))
                .unwrap_or(k)
        })
        .filter(|k| !k.is_empty());
    match kb {
        Some(kb) if row.patch_type == "OS" => format!("OS|kb{}", kb.to_ascii_lowercase()),
        _ => format!("{}|{}", row.patch_type, row.name.trim().to_lowercase()),
    }
}

/// Which side of the diff a row lands on, from its display status (`PENDING` is
/// NinjaOne's `MANUAL`). Mirrors `rows::is_pending`'s exclude list, except that a
/// `FAILED` row is its own category: it is the "newly failed" signal, and counting
/// it as pending too would list one failure in two places.
fn category(status: &str) -> Option<Category> {
    match status {
        "FAILED" => Some(Category::Failed),
        "INSTALLED" | "REJECTED" => None,
        _ => Some(Category::Pending),
    }
}

#[derive(Clone, Copy)]
enum Category {
    Pending,
    Failed,
}

/// The file-name half of a scope: everything that shapes which identities a run can
/// see. The facet fingerprint already covers device scope, statuses and patch
/// facets; the families and the install lookback are the two inputs it leaves out.
/// The lookback only matters when installs were fetched, so it is omitted otherwise
/// — changing it on a Pending-only query must not orphan that scope's baseline.
pub fn scope_key(fingerprint: &str, families: PatchFamilies, install_days: Option<i64>) -> String {
    let installs = install_days.map(|d| d.to_string()).unwrap_or_default();
    serde_json::to_string(&[
        ("facets", fingerprint.to_string()),
        ("os", families.os.to_string()),
        ("sw", families.software.to_string()),
        ("installs", installs),
    ])
    .unwrap_or_default()
}

/// A device in a snapshot: its id (the identity) and name (for display).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SnapDevice(i64, String);

/// A patch in a snapshot, once per distinct [`patch_key`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapPatch {
    key: String,
    patch_type: String,
    kb: Option<String>,
    name: String,
    severity: String,
    rank: u8,
}

/// What one run observed, in comparable form. Items are `(device index, patch
/// index)` into the two tables, so a device or title repeated across a thousand
/// rows is stored once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSnapshot {
    version: u32,
    /// Instance + client id the rows were fetched under. Checked on load, so a
    /// tenant switch can never diff against another tenant's fleet even if two
    /// scope hashes collided.
    tenant: String,
    scope: String,
    /// `QueryResult::generated_at` of the run — the "since <time>" the next diff
    /// prints.
    pub at: String,
    tracks_pending: bool,
    tracks_failed: bool,
    devices: Vec<SnapDevice>,
    patches: Vec<SnapPatch>,
    pending: Vec<(u32, u32)>,
    failed: Vec<(u32, u32)>,
}

impl RunSnapshot {
    /// Projects the detail rows. The rows already carry every facet (scope, status,
    /// severity, search, first-seen window), so the diff describes exactly the
    /// population the Patches tab lists.
    pub fn build(
        rows: &[PatchRow],
        tenant: &str,
        scope: String,
        at: &str,
        statuses: &[PatchStatus],
    ) -> Self {
        let mut devices = Vec::new();
        let mut device_ix: HashMap<i64, u32> = HashMap::new();
        let mut patches = Vec::new();
        let mut patch_ix: HashMap<String, u32> = HashMap::new();
        let mut pending = HashSet::new();
        let mut failed = HashSet::new();
        for row in rows {
            let Some(cat) = category(&row.status) else {
                continue;
            };
            let d = *device_ix.entry(row.device_id).or_insert_with(|| {
                devices.push(SnapDevice(row.device_id, row.device_name.to_string()));
                (devices.len() - 1) as u32
            });
            let key = patch_key(row);
            let p = match patch_ix.get(&key) {
                Some(&p) => p,
                None => {
                    patches.push(SnapPatch {
                        key: key.clone(),
                        patch_type: row.patch_type.to_string(),
                        kb: row.kb.as_deref().map(str::to_string),
                        name: row.name.to_string(),
                        severity: row.severity.to_string(),
                        rank: row.severity_rank,
                    });
                    let p = (patches.len() - 1) as u32;
                    patch_ix.insert(key, p);
                    p
                }
            };
            match cat {
                Category::Pending => pending.insert((d, p)),
                Category::Failed => failed.insert((d, p)),
            };
        }
        let mut pending: Vec<_> = pending.into_iter().collect();
        let mut failed: Vec<_> = failed.into_iter().collect();
        pending.sort_unstable();
        failed.sort_unstable();
        Self {
            version: SNAPSHOT_VERSION,
            tenant: tenant.to_string(),
            scope,
            at: at.to_string(),
            tracks_pending: statuses
                .iter()
                .any(|s| matches!(s, PatchStatus::Pending | PatchStatus::Approved)),
            tracks_failed: statuses.contains(&PatchStatus::Failed),
            devices,
            patches,
            pending,
            failed,
        }
    }

    /// Whether this run is past [`MAX_ITEMS`] and so will not be written.
    pub fn too_large(&self) -> bool {
        self.pending.len() + self.failed.len() > MAX_ITEMS
    }

    fn identities<'a>(&'a self, items: &'a [(u32, u32)]) -> HashMap<(i64, &'a str), (u32, u32)> {
        items
            .iter()
            .filter_map(|&(d, p)| {
                let dev = self.devices.get(d as usize)?;
                let patch = self.patches.get(p as usize)?;
                Some(((dev.0, patch.key.as_str()), (d, p)))
            })
            .collect()
    }

    fn item(&self, (d, p): (u32, u32)) -> Option<ChangeItem> {
        let dev = self.devices.get(d as usize)?;
        let patch = self.patches.get(p as usize)?;
        Some(ChangeItem {
            device_id: dev.0,
            device_name: dev.1.clone(),
            patch_type: patch.patch_type.clone(),
            kb: patch.kb.clone(),
            name: patch.name.clone(),
            severity: patch.severity.clone(),
            severity_rank: patch.rank,
        })
    }
}

/// One patch on one device in a change list.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeItem {
    pub device_id: i64,
    pub device_name: String,
    pub patch_type: String,
    pub kb: Option<String>,
    pub name: String,
    pub severity: String,
    #[serde(skip)]
    pub severity_rank: u8,
}

/// The diff against the previous comparable run — a compact aggregate on both
/// `QueryResult` and `QuerySummary`. Counts are exact; the lists are capped at
/// [`CHANGE_LIST_LIMIT`], worst severity first.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunChanges {
    /// `generated_at` of the run compared against. `None` means there was no
    /// previous comparable run, which every surface says in words rather than
    /// showing three zeros that read as "nothing changed".
    pub previous_at: Option<String>,
    /// Whether the status selection includes a pending status (Pending/Approved).
    /// Without one the rows hold no pending patches and new/resolved are not
    /// measured.
    pub tracks_pending: bool,
    /// Whether FAILED was selected. Without it a failed install just leaves the
    /// pending set, so it reads as resolved — the surfaces say so.
    pub tracks_failed: bool,
    /// This run is too large to snapshot, so the next run will have no baseline.
    pub too_large: bool,
    pub new_pending: usize,
    pub resolved: usize,
    pub newly_failed: usize,
    pub new_pending_items: Vec<ChangeItem>,
    pub resolved_items: Vec<ChangeItem>,
    pub newly_failed_items: Vec<ChangeItem>,
}

impl RunChanges {
    /// The headline both exports print: what was compared against, and the three
    /// counts. Stated in words when there is no baseline, because three zeros read
    /// as "nothing changed".
    pub fn headline(&self) -> String {
        match &self.previous_at {
            None => "No previous comparable run \u{2014} changes are reported from the next run \
                     of this scope."
                .to_string(),
            Some(at) => format!(
                "Since {at}: {} newly pending, {} resolved, {} newly failed.",
                self.new_pending, self.resolved, self.newly_failed
            ),
        }
    }

    /// The caveats that decide how the counts may be read. Every surface prints
    /// them; a bare "12 resolved" on a query without FAILED selected includes the
    /// installs that failed.
    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.too_large {
            notes.push(
                "This scope is too large to remember, so the next run has no baseline to \
                 compare against."
                    .to_string(),
            );
        }
        if !self.tracks_pending {
            notes.push(
                "New and resolved are not measured: the status selection includes neither \
                 Pending nor Approved."
                    .to_string(),
            );
        } else {
            notes.push(
                "Resolved means no longer pending \u{2014} installed, rejected, or no longer in \
                 scope."
                    .to_string(),
            );
        }
        if !self.tracks_failed {
            notes.push(
                "Failed is not selected, so an install that failed reads as resolved and \
                 newly failed is not measured."
                    .to_string(),
            );
        }
        if [self.new_pending, self.resolved, self.newly_failed]
            .iter()
            .any(|&n| n > CHANGE_LIST_LIMIT)
        {
            notes.push(format!(
                "Each list shows at most {CHANGE_LIST_LIMIT} entries, worst severity first; \
                 the counts are exact."
            ));
        }
        notes
    }

    /// Every listed change as one flat table, in new / resolved / newly-failed order.
    pub fn table_rows(&self) -> Vec<ChangeRow> {
        [
            ("Newly pending", &self.new_pending_items),
            ("Resolved", &self.resolved_items),
            ("Newly failed", &self.newly_failed_items),
        ]
        .into_iter()
        .flat_map(|(change, items)| {
            items.iter().map(move |item| ChangeRow {
                change,
                item: item.clone(),
            })
        })
        .collect()
    }
}

/// One line of the exported change table.
pub struct ChangeRow {
    pub change: &'static str,
    pub item: ChangeItem,
}

impl ChangeRow {
    /// The shared column definition the workbook's Changes sheet and the report's
    /// changes section both render.
    pub const COLUMNS: [TableColumn<ChangeRow>; 6] = [
        ("Change", |r| TableCell::text(r.change)),
        ("Device", |r| TableCell::text(&r.item.device_name)),
        ("Patch Type", |r| TableCell::text(&r.item.patch_type)),
        ("KB", |r| TableCell::opt_text(r.item.kb.as_deref())),
        ("Patch", |r| TableCell::text(&r.item.name)),
        ("Severity", |r| TableCell::text(&r.item.severity)),
    ];
}

/// Compares two snapshots. A `previous` from another tenant or scope is ignored
/// (the loader already refuses one; this keeps the pure function honest too).
///
/// * **new pending** — pending now, not pending last time;
/// * **resolved** — pending last time, neither pending nor failed now (installed,
///   rejected, or gone from scope — the rows cannot tell which);
/// * **newly failed** — failed now, not failed last time.
pub fn diff(previous: Option<&RunSnapshot>, now: &RunSnapshot) -> RunChanges {
    let mut out = RunChanges {
        tracks_pending: now.tracks_pending,
        tracks_failed: now.tracks_failed,
        too_large: now.too_large(),
        ..RunChanges::default()
    };
    let Some(prev) = previous
        .filter(|p| p.tenant == now.tenant && p.scope == now.scope && p.version == now.version)
    else {
        return out;
    };
    out.previous_at = Some(prev.at.clone());

    let now_pending = now.identities(&now.pending);
    let now_failed = now.identities(&now.failed);
    let prev_pending = prev.identities(&prev.pending);
    let prev_failed = prev.identities(&prev.failed);

    let new_pending = now_pending
        .iter()
        .filter(|(id, _)| !prev_pending.contains_key(*id))
        .map(|(_, &ix)| ix);
    let resolved = prev_pending
        .iter()
        .filter(|(id, _)| !now_pending.contains_key(*id) && !now_failed.contains_key(*id))
        .map(|(_, &ix)| ix);
    let newly_failed = now_failed
        .iter()
        .filter(|(id, _)| !prev_failed.contains_key(*id))
        .map(|(_, &ix)| ix);

    (out.new_pending, out.new_pending_items) = capped(now, new_pending);
    (out.resolved, out.resolved_items) = capped(prev, resolved);
    (out.newly_failed, out.newly_failed_items) = capped(now, newly_failed);
    out
}

/// One change, borrowed from its snapshot, for ordering before anything is cloned.
struct Pick<'a> {
    device: &'a SnapDevice,
    patch: &'a SnapPatch,
    /// Position in the set's iteration order: the tiebreak a stable full sort gave
    /// implicitly, so selecting the top entries keeps exactly the ones — in exactly
    /// the order — that sorting everything and truncating did.
    pos: usize,
    ix: (u32, u32),
}

/// Worst severity first, then device and patch name, so identical diffs list
/// identically (the sets iterate in hash order). Returns the exact count and the
/// first [`CHANGE_LIST_LIMIT`] entries.
///
/// Orders borrowed keys and builds a [`ChangeItem`] (four `String` clones) only
/// for the listed entries: a whole-fleet first diff can be six figures of
/// changes, and all but 200 of them were cloned only to be sorted and dropped.
fn capped(snap: &RunSnapshot, picks: impl Iterator<Item = (u32, u32)>) -> (usize, Vec<ChangeItem>) {
    let mut picks: Vec<Pick<'_>> = picks
        .filter_map(|ix @ (d, p)| {
            Some((
                snap.devices.get(d as usize)?,
                snap.patches.get(p as usize)?,
                ix,
            ))
        })
        .enumerate()
        .map(|(pos, (device, patch, ix))| Pick {
            device,
            patch,
            pos,
            ix,
        })
        .collect();
    let total = picks.len();
    let order = |a: &Pick<'_>, b: &Pick<'_>| {
        b.patch
            .rank
            .cmp(&a.patch.rank)
            .then_with(|| cmp_ci(&a.device.1, &b.device.1))
            .then_with(|| cmp_ci(&a.patch.name, &b.patch.name))
            .then_with(|| a.device.0.cmp(&b.device.0))
            .then_with(|| a.pos.cmp(&b.pos))
    };
    if picks.len() > CHANGE_LIST_LIMIT {
        picks.select_nth_unstable_by(CHANGE_LIST_LIMIT, order);
        picks.truncate(CHANGE_LIST_LIMIT);
    }
    // `pos` makes the order total, so an unstable sort is deterministic here.
    picks.sort_unstable_by(order);
    let items = picks.iter().filter_map(|p| snap.item(p.ix)).collect();
    (total, items)
}

/// `run-snapshots/<hash>.json`. Hashed because the scope key is free text (search
/// needles) and file names are not; the tenant is in the hash so two tenants'
/// identical scopes are two files.
fn file_name(tenant: &str, scope: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update((tenant.len() as u64).to_le_bytes());
    hasher.update(tenant.as_bytes());
    hasher.update(scope.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex}.json")
}

fn snapshot_dir() -> Option<PathBuf> {
    paths::app_dir().ok().map(|d| d.join(SNAPSHOT_DIR))
}

/// The previous comparable run for this tenant + scope, if one was stored.
///
/// **Synchronous file I/O — call from `spawn_blocking`.**
pub fn load(tenant: &str, scope: &str) -> Option<RunSnapshot> {
    load_from(&snapshot_dir()?, tenant, scope)
}

fn load_from(dir: &Path, tenant: &str, scope: &str) -> Option<RunSnapshot> {
    let body = std::fs::read(dir.join(file_name(tenant, scope))).ok()?;
    match serde_json::from_slice::<RunSnapshot>(&body) {
        Ok(snap)
            if snap.version == SNAPSHOT_VERSION && snap.tenant == tenant && snap.scope == scope =>
        {
            Some(snap)
        }
        Ok(_) => None,
        Err(err) => {
            warn!(?err, "unreadable run snapshot; treating as no previous run");
            None
        }
    }
}

/// Replaces this scope's snapshot and prunes the directory. Never fails the caller.
///
/// **Synchronous file I/O — call from `spawn_blocking`.**
pub fn save(snapshot: &RunSnapshot) {
    let Some(dir) = snapshot_dir() else {
        warn!("no config directory available; run snapshot dropped");
        return;
    };
    save_to(&dir, snapshot);
}

fn save_to(dir: &Path, snapshot: &RunSnapshot) {
    if snapshot.too_large() {
        warn!("run snapshot past the item cap; not written");
        return;
    }
    let _guard = SNAPSHOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let body = match serde_json::to_vec(snapshot) {
        Ok(b) => b,
        Err(err) => {
            warn!(?err, "could not serialize a run snapshot");
            return;
        }
    };
    let name = file_name(&snapshot.tenant, &snapshot.scope);
    let path = dir.join(&name);
    let tmp = dir.join(format!("{name}.tmp"));
    let written = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let mut opts = OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        // Owner-only: device names and the operator's unpatched backlog.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)
    })();
    if let Err(err) = written {
        warn!(?err, "could not write the run snapshot");
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    prune(dir, &path);
}

/// Evicts the least recently written snapshots past [`MAX_SNAPSHOTS`] or
/// [`MAX_TOTAL_BYTES`]. `keep` (the file just written) always survives.
fn prune(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), e.path()))
        })
        .collect();
    // Newest first, the one just written ahead of everything (a coarse mtime can
    // tie it with an older file).
    files.sort_by(|a, b| (b.2 == keep).cmp(&(a.2 == keep)).then(b.0.cmp(&a.0)));
    let mut bytes = 0u64;
    for (i, (_, len, path)) in files.iter().enumerate() {
        bytes += len;
        if i > 0
            && (i >= MAX_SNAPSHOTS || bytes > MAX_TOTAL_BYTES)
            && let Err(err) = std::fs::remove_file(path)
        {
            warn!(?err, "could not evict a run snapshot");
        }
    }
}

#[cfg(test)]
mod tests;
