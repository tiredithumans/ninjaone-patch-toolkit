//! When this app first saw each pending patch on each device — its own record of
//! "first seen", kept because NinjaOne's may not be one.
//!
//! Every SLA and age figure is measured from a pending record's first-seen time, and
//! the only time NinjaOne puts on a record is `timestamp`, which the spec describes as
//! "Date/Time when data was collected/updated". If a rescan refreshes it, every
//! pending patch looks a day old and nothing is ever past SLA. Rather than bet the
//! headline figure on which it is, this module remembers, per tenant, the fetch at
//! which each pending `(device, patch)` first appeared in the current feed, and the
//! rollups age a record from the **earlier** of the two
//! ([`PatchRecord::observed`](crate::rows::PatchRecord::observed)). When NinjaOne's
//! time is the older one nothing changes; when it is not, this record is. The result
//! says how many records it dated, which is how an operator finds out which kind of
//! time NinjaOne sends.
//!
//! One file per tenant under `first-seen/`, per patch family, rebuilt from the
//! whole-fleet feed each time a newer fetch of that family wins the result cache.
//! Best-effort throughout, like `changes`: a failed write costs the next run some
//! history, never the operator their fleet.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::changes::write_patch_key;
use crate::model::{Device, Patch};
use crate::paths;
use crate::rows::is_pending;

const LEDGER_DIR: &str = "first-seen";

/// Bumped when the on-disk shape changes; a file of another version reads as empty.
const LEDGER_VERSION: u32 = 1;

/// Entries one family keeps. A whole-fleet third-party feed runs to six figures of
/// pending records; past this the newest observations are dropped, because the
/// oldest are the ones that carry the SLA signal.
const MAX_ENTRIES_PER_FAMILY: usize = 1_000_000;

/// Tenants remembered at once, and the byte budget for the directory. The newest
/// file is always kept, even alone over budget.
const MAX_LEDGERS: usize = 8;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

/// Serializes every commit (load-latest, rebuild, write, publish) in this process,
/// so two overlapping queries cannot each rebuild from the same base and have the
/// later write lose the earlier one's observations.
static COMMIT_LOCK: Mutex<()> = Mutex::new(());

/// The ledger this process last loaded or committed — the next query's base, so a
/// refresh cadence never re-reads and re-parses the file. One tenant at a time; a
/// read is checked against the tenant, so a tenant switch needs no hook here.
static SLOT: Mutex<Option<Arc<TenantLedger>>> = Mutex::new(None);

/// A patch family, as the row join labels it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Os,
    Software,
}

impl Family {
    fn type_label(self) -> &'static str {
        match self {
            Self::Os => "OS",
            Self::Software => "SOFTWARE",
        }
    }
}

/// One family's record: when each pending `(device, patch)` was first seen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FamilyLedger {
    /// Unix seconds this family was first recorded — "kept since".
    since: i64,
    /// Fetch instant (Unix seconds) of the feed last merged in. A feed no newer is
    /// never merged, so an older run can never roll this back.
    merged_at: i64,
    /// Distinct patch keys (`changes::write_patch_key`), stored once each.
    keys: Vec<Arc<str>>,
    index: HashMap<Arc<str>, u32>,
    /// `(device id, key index)` → first observed, Unix seconds.
    entries: HashMap<(i64, u32), i64>,
}

impl FamilyLedger {
    fn get(&self, device_id: i64, key: &str) -> Option<i64> {
        let ix = *self.index.get(key)?;
        self.entries.get(&(device_id, ix)).copied()
    }

    fn intern(&mut self, key: &str) -> u32 {
        if let Some(&ix) = self.index.get(key) {
            return ix;
        }
        let shared: Arc<str> = Arc::from(key);
        let ix = self.keys.len() as u32;
        self.keys.push(Arc::clone(&shared));
        self.index.insert(shared, ix);
        ix
    }
}

/// Everything this app has recorded for one tenant.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TenantLedger {
    tenant: String,
    os: Option<Arc<FamilyLedger>>,
    sw: Option<Arc<FamilyLedger>>,
}

impl TenantLedger {
    /// A tenant with nothing recorded yet.
    pub fn empty(tenant: &str) -> Self {
        Self {
            tenant: tenant.to_string(),
            ..Self::default()
        }
    }

    fn family(&self, family: Family) -> Option<&FamilyLedger> {
        match family {
            Family::Os => self.os.as_deref(),
            Family::Software => self.sw.as_deref(),
        }
    }

    /// A lookup for one family's records, holding the two buffers each key is built
    /// in so a six-figure feed costs no allocation per record.
    pub fn resolver(&self, family: Family) -> Resolver<'_> {
        Resolver {
            section: self.family(family),
            family,
            name: String::new(),
            key: String::new(),
        }
    }

    /// When this tenant's record of the given families began: the later of their
    /// `since`, since a figure over both is only as long-tracked as the newer one. A
    /// family with no record yet starts at `fallback` (this run's fetch). `None`
    /// when neither family is included.
    pub fn tracking_since(
        &self,
        families: [(Family, bool); 2],
        fallback: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        families
            .into_iter()
            .filter(|(_, included)| *included)
            .map(|(f, _)| {
                self.family(f)
                    .and_then(|l| DateTime::from_timestamp(l.since, 0))
                    .unwrap_or(fallback)
            })
            .max()
    }
}

#[cfg(test)]
impl TenantLedger {
    /// A ledger whose OS family recorded every pending record in `feed` at `at` —
    /// what a first query at `at` over that feed would have committed.
    pub fn recorded(tenant: &str, feed: &[Patch], at: DateTime<Utc>) -> Self {
        let os = rebuild(None, feed, Family::Os, at.timestamp(), &HashSet::new())
            .map(|r| Arc::new(r.section));
        Self {
            tenant: tenant.to_string(),
            os,
            sw: None,
        }
    }
}

/// Looks up when this app first saw a record, for [`PatchRecord::observed`](crate::rows::PatchRecord::observed).
pub struct Resolver<'l> {
    section: Option<&'l FamilyLedger>,
    family: Family,
    name: String,
    key: String,
}

impl Resolver<'_> {
    /// When this app first saw `p` pending on its device, if it has.
    pub fn first_observed(&mut self, p: &Patch) -> Option<DateTime<Utc>> {
        let section = self.section?;
        let device_id = p.device_id?;
        key_into(&mut self.key, &mut self.name, self.family, p);
        section
            .get(device_id, &self.key)
            .and_then(|t| DateTime::from_timestamp(t, 0))
    }
}

/// `p`'s patch key, through the same writer the changes diff uses, from the same
/// display name its row would carry.
fn key_into(key: &mut String, name: &mut String, family: Family, p: &Patch) {
    p.write_display_name(name);
    write_patch_key(
        key,
        family.type_label(),
        p.kb_number.as_deref(),
        name,
        p.product_identifier.as_deref(),
    );
}

/// What one query fetched that the ledger is rebuilt from: the whole-fleet device
/// inventory and each family's whole-fleet feed with its own fetch instant. Whole
/// fleet, not the query's scope — an org-scoped query sees every org's records, so
/// it can stamp them and can never prune them for being out of its view.
pub struct Observation {
    pub devices: Arc<Vec<Device>>,
    pub os: Option<(Arc<Vec<Patch>>, DateTime<Utc>)>,
    pub sw: Option<(Arc<Vec<Patch>>, DateTime<Utc>)>,
}

/// The base a query ages its records against: the ledger this process holds for
/// `tenant`, else its file, else an empty one.
///
/// **Synchronous file I/O — call from a blocking thread.**
pub fn base(tenant: &str) -> Arc<TenantLedger> {
    if let Some(held) = held(tenant) {
        return held;
    }
    let loaded = Arc::new(
        ledger_dir()
            .and_then(|dir| load_from(&dir, tenant))
            .unwrap_or_else(|| TenantLedger::empty(tenant)),
    );
    // Only into an empty or other-tenant slot: a commit that landed meanwhile is
    // newer than this read.
    let mut slot = SLOT.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_none_or(|l| l.tenant != tenant) {
        *slot = Some(Arc::clone(&loaded));
    }
    loaded
}

fn held(tenant: &str) -> Option<Arc<TenantLedger>> {
    SLOT.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|l| l.tenant == tenant)
        .cloned()
}

/// Merges one query's observation into the tenant's ledger, writes it if what it
/// records changed, and holds it as the next base. Never fails the caller.
///
/// Called only once the query's result has won the cache — a superseded run's feed
/// is no newer than the winner's, and the `merged_at` check would skip it anyway.
///
/// **Synchronous file I/O — call from a blocking thread.**
pub fn commit(tenant: &str, obs: Observation) {
    let _guard = COMMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let latest = held(tenant).unwrap_or_else(|| {
        Arc::new(
            ledger_dir()
                .and_then(|dir| load_from(&dir, tenant))
                .unwrap_or_else(|| TenantLedger::empty(tenant)),
        )
    });
    let offline: HashSet<i64> = obs
        .devices
        .iter()
        .filter(|d| d.is_offline())
        .map(|d| d.id)
        .collect();
    let mut next = (*latest).clone();
    let mut rebuilt_any = false;
    let mut changed = false;
    for (family, feed, slot) in [
        (Family::Os, &obs.os, &mut next.os),
        (Family::Software, &obs.sw, &mut next.sw),
    ] {
        let Some((patches, at)) = feed else {
            continue;
        };
        if let Some(r) = rebuild(slot.as_deref(), patches, family, at.timestamp(), &offline) {
            rebuilt_any = true;
            changed |= r.membership_changed;
            *slot = Some(Arc::new(r.section));
        }
    }
    if !rebuilt_any {
        return;
    }
    if changed {
        let Some(dir) = ledger_dir() else {
            warn!("no config directory available; first-seen ledger not saved");
            return;
        };
        if !save_to(&dir, &next) {
            // Memory keeps mirroring disk: the next base must be one the next launch
            // can also find.
            return;
        }
    }
    // Only `merged_at` moved when nothing changed: re-merging the same feed after a
    // restart is a no-op, so memory may run ahead of the file there.
    *SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(next));
}

/// A rebuilt family and whether its recorded `(device, patch)` set changed.
struct Rebuilt {
    section: FamilyLedger,
    membership_changed: bool,
}

/// One family rebuilt from its whole-fleet feed fetched at `at`: every pending
/// record keeps the first observation it already had, or is stamped `at`; a record
/// no longer pending is dropped, unless its device is offline — an offline device's
/// absence from the feed proves nothing, the same reasoning that keeps it out of the
/// rollups. `None` when there is nothing to merge: the feed is no newer than the
/// last one merged, or it holds no pending record while the ledger holds some (a
/// successful empty page must not wipe the history).
fn rebuild(
    prev: Option<&FamilyLedger>,
    feed: &[Patch],
    family: Family,
    at: i64,
    offline: &HashSet<i64>,
) -> Option<Rebuilt> {
    if prev.is_some_and(|p| at <= p.merged_at) {
        return None;
    }
    let mut next = FamilyLedger {
        since: prev.map_or(at, |p| p.since),
        merged_at: at,
        ..FamilyLedger::default()
    };
    let (mut name, mut key) = (String::new(), String::new());
    let mut added = 0usize;
    let mut any_pending = false;
    for p in feed {
        let Some(device_id) = p.device_id else {
            continue;
        };
        if !is_pending(p.status.as_deref()) {
            continue;
        }
        any_pending = true;
        key_into(&mut key, &mut name, family, p);
        let first = match prev.and_then(|l| l.get(device_id, &key)) {
            Some(t) => t,
            None => {
                added += 1;
                at
            }
        };
        let ix = next.intern(&key);
        next.entries
            .entry((device_id, ix))
            .and_modify(|t| *t = (*t).min(first))
            .or_insert(first);
    }
    let prev_len = prev.map_or(0, |l| l.entries.len());
    if !any_pending && prev_len > 0 {
        return None;
    }
    if let Some(prev) = prev {
        for (&(device_id, ix), &first) in &prev.entries {
            if !offline.contains(&device_id) {
                continue;
            }
            let ix = next.intern(&prev.keys[ix as usize]);
            next.entries.entry((device_id, ix)).or_insert(first);
        }
    }
    if next.entries.len() > MAX_ENTRIES_PER_FAMILY {
        warn!(
            entries = next.entries.len(),
            "first-seen ledger past its cap; keeping the oldest observations"
        );
        let mut all: Vec<((i64, u32), i64)> = next.entries.drain().collect();
        all.sort_unstable_by_key(|&(id, first)| (first, id));
        all.truncate(MAX_ENTRIES_PER_FAMILY);
        next.entries = all.into_iter().collect();
    }
    let membership_changed = prev.is_none() || added > 0 || next.entries.len() != prev_len;
    Some(Rebuilt {
        section: next,
        membership_changed,
    })
}

/// The on-disk form: keys once each, entries as index triples.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerFile {
    version: u32,
    /// Instance + client id. Checked on load, so a copied file or a hash collision
    /// can never date one tenant's patches from another's.
    tenant: String,
    os: Option<FamilyFile>,
    sw: Option<FamilyFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FamilyFile {
    since: i64,
    merged_at: i64,
    keys: Vec<String>,
    entries: Vec<(i64, u32, i64)>,
}

impl FamilyFile {
    fn from_ledger(l: &FamilyLedger) -> Self {
        // Only keys still referenced, renumbered, so the file never accumulates the
        // keys of patches long since installed.
        let mut keys = Vec::new();
        let mut remap: HashMap<u32, u32> = HashMap::new();
        let mut entries: Vec<(i64, u32, i64)> = l
            .entries
            .iter()
            .map(|(&(device_id, ix), &first)| {
                let new_ix = *remap.entry(ix).or_insert_with(|| {
                    keys.push(l.keys[ix as usize].to_string());
                    (keys.len() - 1) as u32
                });
                (device_id, new_ix, first)
            })
            .collect();
        entries.sort_unstable();
        Self {
            since: l.since,
            merged_at: l.merged_at,
            keys,
            entries,
        }
    }

    fn into_ledger(self) -> Option<FamilyLedger> {
        let mut l = FamilyLedger {
            since: self.since,
            merged_at: self.merged_at,
            ..FamilyLedger::default()
        };
        for key in &self.keys {
            l.intern(key);
        }
        for (device_id, ix, first) in self.entries {
            // A malformed index means a damaged file; read it as no record rather
            // than misdate anything.
            if ix as usize >= l.keys.len() {
                return None;
            }
            l.entries.insert((device_id, ix), first);
        }
        Some(l)
    }
}

fn file_name(tenant: &str) -> String {
    let digest = Sha256::digest(tenant.as_bytes());
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex}.json")
}

fn ledger_dir() -> Option<PathBuf> {
    paths::local_subdir(LEDGER_DIR).ok()
}

fn load_from(dir: &Path, tenant: &str) -> Option<TenantLedger> {
    let body = std::fs::read(dir.join(file_name(tenant))).ok()?;
    let file = match serde_json::from_slice::<LedgerFile>(&body) {
        Ok(f) if f.version == LEDGER_VERSION && f.tenant == tenant => f,
        Ok(_) => return None,
        Err(err) => {
            warn!(?err, "unreadable first-seen ledger; starting a new one");
            return None;
        }
    };
    let section = |f: Option<FamilyFile>| match f {
        None => Some(None),
        Some(f) => f.into_ledger().map(|l| Some(Arc::new(l))),
    };
    let (Some(os), Some(sw)) = (section(file.os), section(file.sw)) else {
        warn!("damaged first-seen ledger; starting a new one");
        return None;
    };
    Some(TenantLedger {
        tenant: file.tenant,
        os,
        sw,
    })
}

/// Whether the ledger was written. Atomic (temp + rename) and owner-only, like the
/// run snapshots: it names every device's unpatched backlog.
fn save_to(dir: &Path, ledger: &TenantLedger) -> bool {
    let file = LedgerFile {
        version: LEDGER_VERSION,
        tenant: ledger.tenant.clone(),
        os: ledger.os.as_deref().map(FamilyFile::from_ledger),
        sw: ledger.sw.as_deref().map(FamilyFile::from_ledger),
    };
    let body = match serde_json::to_vec(&file) {
        Ok(b) => b,
        Err(err) => {
            warn!(?err, "could not serialize the first-seen ledger");
            return false;
        }
    };
    let name = file_name(&ledger.tenant);
    let path = dir.join(&name);
    let tmp = dir.join(format!("{name}.tmp"));
    let written = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let mut opts = OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)
    })();
    if let Err(err) = written {
        warn!(?err, "could not write the first-seen ledger");
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    prune(dir, &path);
    true
}

/// Evicts the least recently written tenants past [`MAX_LEDGERS`] or
/// [`MAX_TOTAL_BYTES`], always keeping the one just written.
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
    files.sort_by(|a, b| (b.2 == keep).cmp(&(a.2 == keep)).then(b.0.cmp(&a.0)));
    let mut bytes = 0u64;
    for (i, (_, len, path)) in files.iter().enumerate() {
        bytes += len;
        if i > 0
            && (i >= MAX_LEDGERS || bytes > MAX_TOTAL_BYTES)
            && let Err(err) = std::fs::remove_file(path)
        {
            warn!(?err, "could not evict a first-seen ledger");
        }
    }
}

#[cfg(test)]
mod tests;
