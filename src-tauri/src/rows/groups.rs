//! Sorting, paging and grouping over the cached rows. The frontend only ever
//! holds one page, so all three happen here: `sort_order` builds an index
//! permutation, `page_rows` slices it, and the group functions key rows by an
//! opaque `group_key` the frontend echoes back.

use std::cmp::{Ordering, Reverse};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::model::PatchRow;

use super::ORPHAN_DEVICE_ID;

/// Sort key for the paged detail rows (`get_patch_rows`). Deserialized from the
/// frontend's camelCase IPC args; mirrored in `web-rs/src/types.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RowSortKey {
    Organization,
    Location,
    Role,
    Device,
    Os,
    PatchType,
    Kb,
    Name,
    Severity,
    Status,
    FirstSeenDate,
    InstalledDate,
}

/// `PartialEq` so the cache can tell whether its memoized order still answers the
/// sort being asked for — the same check `with_grouped_result` makes on [`GroupBy`].
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowSort {
    pub key: RowSortKey,
    pub desc: bool,
}

/// Which key the Patches view groups its rows by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum GroupBy {
    Device,
    Patch,
    /// Third-party rows by `productIdentifier` — every version of one product in
    /// one group — and every other row by its [`GroupBy::Patch`] key, so OS
    /// patches (which carry no product) group exactly as they do by patch.
    Product,
}

/// The first field of a product group's key. A product key has two fields and a
/// patch key three, so [`GroupKeyMatcher`] tells them apart by shape and a product
/// key cannot be mistaken for a patch key (or the reverse).
const PRODUCT_KEY_TAG: &str = "SOFTWARE";

/// The product a row groups under in [`GroupBy::Product`]: its product identifier,
/// when it is a third-party row carrying a non-blank one.
fn product_of(row: &PatchRow) -> Option<&str> {
    row.product_identifier
        .as_deref()
        .filter(|p| row.patch_type == "SOFTWARE" && !p.trim().is_empty())
}

/// Separator joining the fields of a composite group key. A unit separator can't
/// occur in a device or patch name, so one group's key can never collide with or
/// forge another's.
const GROUP_KEY_SEP: char = '\u{1f}';

/// The stable identity the frontend echoes back to fetch a group's members and to
/// key its expand state. Keyed on the same tuple the group is built from, so it
/// round-trips without the backend holding per-request state.
#[cfg(test)]
pub fn group_key(row: &PatchRow, group_by: GroupBy) -> String {
    let mut buf = String::new();
    write_group_key(row, group_by, &mut buf);
    buf
}

/// The same key, into a caller-owned buffer which it clears first. This is the form
/// production uses; the owned `group_key` above remains as the readable statement of
/// the encoding, which the tests hold `GroupKeyMatcher` against.
///
/// [`build_groups`] needs a key for every row purely to look one up, and almost all
/// of those lookups hit an existing group. A reused buffer turns that into zero
/// allocations per row; only a row that opens a *new* group pays for a `String`.
fn write_group_key(row: &PatchRow, group_by: GroupBy, buf: &mut String) {
    use std::fmt::Write as _;
    buf.clear();
    match group_by {
        GroupBy::Device => {
            let _ = write!(buf, "{}", row.device_id);
        }
        GroupBy::Product if product_of(row).is_some() => {
            let _ = write!(
                buf,
                "{PRODUCT_KEY_TAG}{GROUP_KEY_SEP}{}",
                product_of(row).unwrap_or_default()
            );
        }
        GroupBy::Patch | GroupBy::Product => {
            let _ = write!(
                buf,
                "{}{GROUP_KEY_SEP}{}{GROUP_KEY_SEP}{}",
                row.patch_type,
                row.kb.as_deref().unwrap_or(""),
                row.name
            );
        }
    }
}

/// A stable, version-free display name for a product group, from its members'
/// titles as `(title, row count)`.
///
/// NinjaOne's third-party feed folds the version into the title ("Google Chrome
/// 141.0.7390.55") and declares no product-name field, so the name has to be
/// derived: each title loses one trailing version token (see
/// [`strip_version_token`]), and the most common result wins, ties broken
/// case-insensitively then bytewise so the label does not depend on row order.
/// Mirrored in `web-rs/src/app/util/` (`product_display_name`) for the demo; the
/// grouping fixture pins the two together.
pub fn product_display_name<'a>(titles: impl IntoIterator<Item = (&'a str, usize)>) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (title, n) in titles {
        *counts.entry(strip_version_token(title)).or_default() += n;
    }
    counts
        .into_iter()
        .max_by(|(a, na), (b, nb)| na.cmp(nb).then_with(|| cmp_ci(b, a)).then_with(|| b.cmp(a)))
        .map(|(name, _)| name.to_string())
        .unwrap_or_default()
}

/// `title` without one trailing version token, trimmed: "Google Chrome
/// 141.0.7390.55" → "Google Chrome", "OpenSSL 3.0.16-1ubuntu1" → "OpenSSL".
///
/// A version token starts with a digit (after an optional `v`), contains a `.`, and
/// holds nothing but ASCII alphanumerics and `.-_+`. The dot is what keeps
/// "Microsoft Office 2016" and "Java 8 Update 451" whole — a bare number is as
/// likely to be part of the product's name as its version. A title that is nothing
/// but a version is returned as-is rather than emptied.
pub fn strip_version_token(title: &str) -> &str {
    let title = title.trim();
    let Some((head, last)) = title.rsplit_once(char::is_whitespace) else {
        return title;
    };
    let body = last.strip_prefix(['v', 'V']).unwrap_or(last);
    let is_version = body.starts_with(|c: char| c.is_ascii_digit())
        && body.contains('.')
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    let head = head.trim_end();
    if is_version && !head.is_empty() {
        head
    } else {
        title
    }
}

/// One collapsed group header. Members are **not** carried: a patch group can span
/// the whole fleet (a single Chrome update covers every device), so members are
/// paged separately via [`group_member_page`] when the operator expands it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchGroup {
    pub key: String,
    pub label: Arc<str>,
    /// Organization for a device group; KB for a patch group (blank when absent —
    /// third-party patches carry no KB).
    pub sublabel: Option<Arc<str>>,
    pub rows: usize,
    /// Distinct devices in the group: 1 for a device group, and the
    /// affected-device count for a patch group. Records with no device id
    /// ([`ORPHAN_DEVICE_ID`]) are not counted, so their bucket reads 0.
    pub devices: usize,
    /// Highest severity among the members, so a collapsed group still shows how
    /// urgent its worst patch is.
    pub severity: &'static str,
    pub severity_rank: u8,
    /// Device groups only — the id actions dispatch against, and its state. `None`
    /// for patch groups and for the bucket of records that carried no device id.
    pub device_id: Option<i64>,
    pub offline: bool,
    pub needs_reboot: bool,
}

/// Builds every group over the cached rows, ordered most-urgent-first.
///
/// Device groups keep the canonical severity → org → device order the flat view
/// uses; patch groups lead with blast radius (affected devices) then severity,
/// matching [`build_failures`], because "this update is missing on 212 machines"
/// is the thing worth seeing first.
pub fn build_groups(rows: &[PatchRow], group_by: GroupBy) -> Vec<PatchGroup> {
    struct Acc {
        /// Insertion order, which the stable display sorts fall back to on a tie.
        seq: usize,
        label: Arc<str>,
        sublabel: Option<Arc<str>>,
        rows: usize,
        devices: HashSet<i64>,
        severity: &'static str,
        severity_rank: u8,
        device_id: Option<i64>,
        offline: bool,
        needs_reboot: bool,
        /// Product groups only: each member title and how many rows carry it, for
        /// [`product_display_name`]. Keyed by the rows' shared `Arc`s, so this is
        /// one entry per distinct version, not per row.
        titles: HashMap<Arc<str>, usize>,
    }
    let mut groups: HashMap<String, Acc> = HashMap::new();
    // One reusable key buffer for the whole pass — see `write_group_key`. Grouping
    // used to cost three `String`s per row (the key, and a clone each for the map
    // entry and the insertion-order list); it now costs one per *group*.
    let mut key_buf = String::new();
    for r in rows {
        write_group_key(r, group_by, &mut key_buf);
        // Insertion order is load-bearing: the display sorts below are stable, so
        // tied groups fall back to it, and it follows the canonical row order.
        // Carrying it on the accumulator replaces the parallel `order` vec.
        let seq = groups.len();
        let acc = match groups.get_mut(&key_buf) {
            Some(acc) => acc,
            None => groups
                .entry(key_buf.clone())
                .or_insert_with(|| match group_by {
                    GroupBy::Device => Acc {
                        seq,
                        label: r.device_name.clone(),
                        sublabel: Some(r.organization.clone()),
                        rows: 0,
                        devices: HashSet::new(),
                        severity: r.severity,
                        severity_rank: r.severity_rank,
                        // An id-less orphan bucket names no device, so it offers
                        // nothing to dispatch against.
                        device_id: (r.device_id != ORPHAN_DEVICE_ID).then_some(r.device_id),
                        offline: r.offline,
                        needs_reboot: r.needs_reboot,
                        titles: HashMap::new(),
                    },
                    // A product group's label and sublabel are settled once every
                    // member has been seen, below; a fallback (non-product) group is
                    // a patch group in every respect.
                    GroupBy::Patch | GroupBy::Product => Acc {
                        seq,
                        label: r.name.clone(),
                        sublabel: r.kb.clone().filter(|k| !k.is_empty()),
                        rows: 0,
                        devices: HashSet::new(),
                        severity: r.severity,
                        severity_rank: r.severity_rank,
                        device_id: None,
                        offline: false,
                        needs_reboot: false,
                        titles: HashMap::new(),
                    },
                }),
        };
        acc.rows += 1;
        if group_by == GroupBy::Product && product_of(r).is_some() {
            *acc.titles.entry(Arc::clone(&r.name)).or_default() += 1;
        }
        // Id-less records are not one shared device; counting the sentinel would add
        // a phantom machine to every patch group they appear in.
        if r.device_id != ORPHAN_DEVICE_ID {
            acc.devices.insert(r.device_id);
        }
        // Records for the same group can disagree; surface the worst.
        if r.severity_rank > acc.severity_rank {
            acc.severity_rank = r.severity_rank;
            acc.severity = r.severity;
        }
    }

    let mut accumulated: Vec<(String, Acc)> = groups.into_iter().collect();
    accumulated.sort_unstable_by_key(|(_, a)| a.seq);
    let mut out: Vec<PatchGroup> = accumulated
        .into_iter()
        .map(|(key, a)| {
            // A product group names the product, not whichever version it happened
            // to meet first, and says how many versions it spans instead of a KB
            // (third-party records carry none).
            let (label, sublabel) = if a.titles.is_empty() {
                (a.label, a.sublabel)
            } else {
                let versions = a.titles.len();
                (
                    Arc::from(product_display_name(
                        a.titles.iter().map(|(t, n)| (&**t, *n)),
                    )),
                    (versions > 1).then(|| Arc::from(format!("{versions} versions"))),
                )
            };
            PatchGroup {
                key,
                label,
                sublabel,
                rows: a.rows,
                devices: a.devices.len(),
                severity: a.severity,
                severity_rank: a.severity_rank,
                device_id: a.device_id,
                offline: a.offline,
                needs_reboot: a.needs_reboot,
            }
        })
        .collect();

    match group_by {
        GroupBy::Device => out.sort_by_cached_key(|g| {
            (
                Reverse(g.severity_rank),
                g.sublabel.as_deref().unwrap_or_default().to_lowercase(),
                g.label.to_lowercase(),
            )
        }),
        GroupBy::Patch | GroupBy::Product => {
            out.sort_by_cached_key(|g| (Reverse(g.devices), Reverse(g.severity_rank)))
        }
    }
    out
}

/// One page of group headers plus the total, so the frontend can page groups the
/// same way it pages flat rows.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupPage {
    pub groups: Vec<PatchGroup>,
    pub total: usize,
}

/// Slices an already-built grouping into one page.
///
/// Split from the building so the caller can memoize the expensive half: this used
/// to be `group_page`, which rebuilt the whole grouping on every paging request. The
/// arithmetic stays here, where it is unit-testable, rather than inline in the
/// command.
pub fn slice_groups(all: &[PatchGroup], offset: usize, limit: usize) -> GroupPage {
    GroupPage {
        total: all.len(),
        groups: all.iter().skip(offset).take(limit).cloned().collect(),
    }
}

/// One device as the drill-down shows it: its facts and per-device rollup, plus
/// its detail rows. Built from the cached result on request; never cached itself.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDetail {
    /// `None` for a device that has rows but is not in the scoped inventory — only
    /// possible for an orphan patch, which has no device facts to show.
    pub device: Option<super::DeviceSummary>,
    /// The device's detail rows — the Patches tab's rows, so every patch filter
    /// applies — in the cache's canonical order, at most `limit` of them.
    pub rows: Vec<PatchRow>,
    /// How many detail rows the device has in all, so a capped list says so.
    pub rows_total: usize,
}

/// Builds the drill-down for `device_id` from the cached result, or `None` when the
/// result has neither a summary nor a row for it (a stale click after a re-query,
/// or an id that was never in scope). A scan of every row, so the command runs it
/// off the async runtime.
pub fn device_detail(
    result: &super::QueryResult,
    device_id: i64,
    limit: usize,
) -> Option<DeviceDetail> {
    if device_id == ORPHAN_DEVICE_ID {
        return None;
    }
    let device = result
        .devices
        .iter()
        .find(|d| d.device_id == device_id)
        .cloned();
    let mut rows_total = 0;
    let mut rows = Vec::new();
    for r in result.rows.iter().filter(|r| r.device_id == device_id) {
        rows_total += 1;
        if rows.len() < limit {
            rows.push(r.clone());
        }
    }
    if device.is_none() && rows_total == 0 {
        return None;
    }
    Some(DeviceDetail {
        device,
        rows,
        rows_total,
    })
}

/// One page of a single group's member rows, in the cache's canonical order.
/// Filtering by key rather than storing members on the group keeps a fleet-wide
/// patch group (one entry per device) off the wire until it's actually expanded.
pub fn group_member_page(
    rows: &[PatchRow],
    group_by: GroupBy,
    key: &str,
    offset: usize,
    limit: usize,
) -> Vec<PatchRow> {
    // The key is parsed once and each row compared field-by-field. Building
    // `group_key(r, group_by)` per row allocated a `format!`ed String for every row
    // in the cache on every expand and every page of a group — a whole-fleet
    // allocation to serve twenty rows.
    let matcher = GroupKeyMatcher::new(group_by, key);
    rows.iter()
        .filter(|r| matcher.matches(r))
        .skip(offset)
        .take(limit)
        .cloned()
        .collect()
}

/// A parsed [`group_key`], so membership is an equality check rather than a fresh
/// `String` per row. Mirrors `group_key`'s encoding exactly — a change to one is a
/// change to the other, which `group_key_and_matcher_agree` pins.
pub(super) enum GroupKeyMatcher<'a> {
    Device(Option<i64>),
    Patch {
        patch_type: &'a str,
        kb: &'a str,
        name: &'a str,
        /// Parsed under `GroupBy::Product`, where this key is the fallback for rows
        /// with no product — so a row that *has* one never matches it.
        product_mode: bool,
    },
    /// `GroupBy::Product`'s two-field key.
    Product(&'a str),
    /// A key that does not parse matches nothing, exactly as a non-equal string did.
    None,
}

impl<'a> GroupKeyMatcher<'a> {
    pub(super) fn new(group_by: GroupBy, key: &'a str) -> Self {
        match group_by {
            GroupBy::Device => Self::Device(key.parse().ok()),
            GroupBy::Patch | GroupBy::Product => {
                let mut parts = key.split(GROUP_KEY_SEP);
                match (parts.next(), parts.next(), parts.next(), parts.next()) {
                    (Some(patch_type), Some(kb), Some(name), None) => Self::Patch {
                        patch_type,
                        kb,
                        name,
                        product_mode: group_by == GroupBy::Product,
                    },
                    // Only product mode issues two-field keys; under Patch the same
                    // shape is simply unparseable.
                    (Some(PRODUCT_KEY_TAG), Some(product), None, None)
                        if group_by == GroupBy::Product =>
                    {
                        Self::Product(product)
                    }
                    _ => Self::None,
                }
            }
        }
    }

    pub(super) fn matches(&self, row: &PatchRow) -> bool {
        match self {
            Self::Device(Some(id)) => row.device_id == *id,
            Self::Device(None) | Self::None => false,
            Self::Patch {
                patch_type,
                kb,
                name,
                product_mode,
            } => {
                // In product mode a row with a product is in its product group,
                // never in a patch-keyed one, even if its fields would match — the
                // same partition `write_group_key` draws.
                !(*product_mode && product_of(row).is_some())
                    && row.patch_type == *patch_type
                    && row.kb.as_deref().unwrap_or("") == *kb
                    && &*row.name == *name
            }
            Self::Product(product) => product_of(row) == Some(*product),
        }
    }
}

/// The row order `sort` implies, as a permutation of indices into `rows`.
///
/// Split from the slicing for the same reason [`build_groups`] is split from
/// [`slice_groups`]: this is the expensive half and the caller memoizes it. Paging
/// through a sorted view used to re-sort the entire cached row set on **every page
/// request** — a full `O(n log n)` string comparison sweep per click, under the lock
/// the export also takes — while the identical problem on the grouping side had
/// already been fixed. The permutation is `u32` rather than `&PatchRow` so the memo
/// costs 4 bytes a row instead of a pointer plus a second copy of the set.
///
/// The cached rows themselves are never reordered; their canonical order is
/// load-bearing for the export and for the summary's inline first page.
pub fn sort_order(rows: &[PatchRow], sort: RowSort) -> Vec<u32> {
    let mut order: Vec<u32> = (0..rows.len() as u32).collect();
    // Stable sort: rows that tie keep the canonical cache order.
    order.sort_by(|a, b| compare_rows(&rows[*a as usize], &rows[*b as usize], sort));
    order
}

/// Serves one page of the cached detail rows, in `order` when one is supplied.
///
/// `None` reproduces the cache order exactly (the canonical severity/org/device sort
/// stamped in `run_query`), without materializing an identity permutation for a fleet
/// that never asked to be re-sorted. Only the requested page is cloned either way.
///
/// An index in `order` that is out of range is skipped rather than panicking: the
/// permutation and the rows come from the same cache entry under one lock, so this
/// is unreachable, but a page request is not worth a process abort if that ever
/// stops being true.
pub fn page_rows(
    rows: &[PatchRow],
    order: Option<&[u32]>,
    offset: usize,
    limit: usize,
) -> Vec<PatchRow> {
    match order {
        None => rows.iter().skip(offset).take(limit).cloned().collect(),
        Some(order) => order
            .iter()
            .skip(offset)
            .take(limit)
            .filter_map(|i| rows.get(*i as usize))
            .cloned()
            .collect(),
    }
}

pub(super) fn compare_rows(a: &PatchRow, b: &PatchRow, sort: RowSort) -> Ordering {
    use RowSortKey::*;
    let dir = |o: Ordering| if sort.desc { o.reverse() } else { o };
    match sort.key {
        Organization => dir(cmp_ci(&a.organization, &b.organization)),
        Location => cmp_opt_last(
            a.location.as_deref(),
            b.location.as_deref(),
            sort.desc,
            |x, y| cmp_ci(x, y),
        ),
        Role => cmp_opt_last(
            a.device_role.as_deref(),
            b.device_role.as_deref(),
            sort.desc,
            |x, y| cmp_ci(x, y),
        ),
        Device => dir(cmp_ci(&a.device_name, &b.device_name)),
        Os => cmp_opt_last(
            a.os_name.as_deref(),
            b.os_name.as_deref(),
            sort.desc,
            |x, y| cmp_ci(x, y),
        ),
        PatchType => dir(a.patch_type.cmp(b.patch_type)),
        Kb => cmp_opt_last(a.kb.as_deref(), b.kb.as_deref(), sort.desc, |x, y| {
            cmp_ci(x, y)
        }),
        Name => dir(cmp_ci(&a.name, &b.name)),
        // The severity ordinal is presentation order (Critical → Unknown), so an
        // ascending sort surfaces the most urgent first, like the default view.
        Severity => dir(b.severity_rank.cmp(&a.severity_rank)),
        Status => dir(a.status.cmp(&b.status)),
        FirstSeenDate => cmp_opt_last(a.first_seen_ts, b.first_seen_ts, sort.desc, |x, y| x.cmp(y)),
        InstalledDate => cmp_opt_last(a.installed_ts, b.installed_ts, sort.desc, |x, y| x.cmp(y)),
    }
}

/// Case-insensitive (ASCII) ordering without a per-comparison allocation.
pub fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// Missing values sort last regardless of direction, so a descending sort by
/// e.g. installed date leads with real dates rather than blanks.
fn cmp_opt_last<T, F>(a: Option<T>, b: Option<T>, desc: bool, cmp: F) -> Ordering
where
    F: Fn(&T, &T) -> Ordering,
{
    match (&a, &b) {
        (Some(x), Some(y)) => {
            let o = cmp(x, y);
            if desc { o.reverse() } else { o }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
