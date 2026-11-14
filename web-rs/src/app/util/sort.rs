//! Column-sort state transitions and the client-side comparator used for the
//! one page the frontend holds.

use std::cmp::Ordering;

use crate::types::{PatchRow, RowSort, RowSortKey};

/// Next state when a sortable column header is clicked: none → asc → desc → none
/// on the same key; clicking a different key starts it ascending.
pub(crate) fn next_sort(current: Option<RowSort>, key: RowSortKey) -> Option<RowSort> {
    match current {
        Some(s) if s.key == key && !s.desc => Some(RowSort { key, desc: true }),
        Some(s) if s.key == key => None,
        _ => Some(RowSort { key, desc: false }),
    }
}

/// `aria-sort` value for a column header under the current sort.
pub(crate) fn aria_sort(sort: Option<RowSort>, key: RowSortKey) -> &'static str {
    match sort {
        Some(s) if s.key == key => {
            if s.desc {
                "descending"
            } else {
                "ascending"
            }
        }
        _ => "none",
    }
}

/// Direction glyph suffix for a sorted column header ("" when not sorted by it).
pub(crate) fn sort_glyph(sort: Option<RowSort>, key: RowSortKey) -> &'static str {
    match sort {
        Some(s) if s.key == key => {
            if s.desc {
                " ▼"
            } else {
                " ▲"
            }
        }
        _ => "",
    }
}

/// The sort of a table the frontend holds whole (Failures, the Compliance tables,
/// Jobs): the column, by its index in the table's header array, and the direction.
/// The Patches table sorts backend-side instead, by [`RowSort`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColumnSort {
    pub col: usize,
    pub desc: bool,
}

/// [`next_sort`]'s cycle for a [`ColumnSort`]: none → ascending → descending → none
/// on the same column; another column starts ascending.
pub(crate) fn next_column_sort(current: Option<ColumnSort>, col: usize) -> Option<ColumnSort> {
    match current {
        Some(s) if s.col == col && !s.desc => Some(ColumnSort { col, desc: true }),
        Some(s) if s.col == col => None,
        _ => Some(ColumnSort { col, desc: false }),
    }
}

/// `aria-sort` for a column header under a [`ColumnSort`].
pub(crate) fn column_aria_sort(sort: Option<ColumnSort>, col: usize) -> &'static str {
    match sort {
        Some(s) if s.col == col && s.desc => "descending",
        Some(s) if s.col == col => "ascending",
        _ => "none",
    }
}

/// Direction glyph for a column header under a [`ColumnSort`].
pub(crate) fn column_sort_glyph(sort: Option<ColumnSort>, col: usize) -> &'static str {
    match sort {
        Some(s) if s.col == col && s.desc => " ▼",
        Some(s) if s.col == col => " ▲",
        _ => "",
    }
}

/// What one cell sorts by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SortValue<'a> {
    /// Compared case-insensitively. ISO dates sort chronologically this way.
    Text(&'a str),
    Number(f64),
    /// A blank cell: last in either direction, as in the Patches table.
    Missing,
}

impl<'a> SortValue<'a> {
    /// A cell that may be blank; an empty string is blank too.
    pub(crate) fn text(value: Option<&'a str>) -> Self {
        match value {
            Some(v) if !v.is_empty() => Self::Text(v),
            _ => Self::Missing,
        }
    }

    pub(crate) fn count(n: usize) -> Self {
        Self::Number(n as f64)
    }
}

fn compare_values(a: SortValue<'_>, b: SortValue<'_>, desc: bool) -> Ordering {
    use SortValue::*;
    let dir = |o: Ordering| if desc { o.reverse() } else { o };
    match (a, b) {
        (Missing, Missing) => Ordering::Equal,
        (Missing, _) => Ordering::Greater,
        (_, Missing) => Ordering::Less,
        (Text(x), Text(y)) => dir(cmp_ci(x, y)),
        (Number(x), Number(y)) => dir(x.total_cmp(&y)),
        // A column is one kind throughout; mixed values keep numbers first.
        (Number(_), Text(_)) => Ordering::Less,
        (Text(_), Number(_)) => Ordering::Greater,
    }
}

/// Sorts `rows` in place by one column, reading each cell through `value`. Stable,
/// so rows that tie keep the order the backend sent them in, and no sort at all
/// leaves that order untouched.
pub(crate) fn sort_by_column<T>(
    rows: &mut [T],
    sort: Option<ColumnSort>,
    value: impl for<'a> Fn(&'a T, usize) -> SortValue<'a>,
) {
    let Some(ColumnSort { col, desc }) = sort else {
        return;
    };
    rows.sort_by(|a, b| compare_values(value(a, col), value(b, col), desc));
}

/// Client-side counterpart of the backend row sort, for demo mode only (the demo
/// holds its full sample locally; there is no backend cache to re-page from).
/// Deliberately mirrors `src-tauri/src/rows.rs::compare_rows` — duplicating small
/// logic across the crates is the sanctioned pattern (no shared crate over wasm).
pub(crate) fn sort_patch_rows(rows: &mut [PatchRow], sort: RowSort) {
    rows.sort_by(|a, b| compare_rows(a, b, sort));
}

fn compare_rows(a: &PatchRow, b: &PatchRow, sort: RowSort) -> Ordering {
    use RowSortKey::*;
    let dir = |o: Ordering| if sort.desc { o.reverse() } else { o };
    match sort.key {
        Organization => dir(cmp_ci(&a.organization, &b.organization)),
        Location => cmp_opt_last(a.location.as_deref(), b.location.as_deref(), sort.desc),
        Role => cmp_opt_last(
            a.device_role.as_deref(),
            b.device_role.as_deref(),
            sort.desc,
        ),
        Device => dir(cmp_ci(&a.device_name, &b.device_name)),
        Os => cmp_opt_last(a.os_name.as_deref(), b.os_name.as_deref(), sort.desc),
        PatchType => dir(a.patch_type.cmp(&b.patch_type)),
        Kb => cmp_opt_last(a.kb.as_deref(), b.kb.as_deref(), sort.desc),
        Name => dir(cmp_ci(&a.name, &b.name)),
        // Most urgent first on ascending, like the backend's presentation ordinal.
        Severity => dir(sev_ordinal(&a.severity).cmp(&sev_ordinal(&b.severity))),
        Cves => dir(cve_key(b).cmp(&cve_key(a))),
        Status => dir(a.status.cmp(&b.status)),
        // The mirror carries dates as ISO `yyyy-mm-dd` strings — lexicographic
        // order is chronological.
        FirstSeenDate => cmp_opt_last(
            a.first_seen_date.as_deref(),
            b.first_seen_date.as_deref(),
            sort.desc,
        ),
        InstalledDate => cmp_opt_last(
            a.installed_date.as_deref(),
            b.installed_date.as_deref(),
            sort.desc,
        ),
    }
}

/// Exploited first, then the most CVEs — the backend's `rows::cve_key`.
fn cve_key(r: &PatchRow) -> (bool, Option<u32>) {
    (r.exploited_cves.is_some(), r.cve_count)
}

/// Maps a severity's display label ("Critical") back to the raw API value the
/// severity filter holds ("CRITICAL"), via the same `SEVERITY_OPTIONS` table the
/// filter chips are built from.
///
/// Looked up rather than uppercased: uppercasing happens to work for the current
/// vocabulary, but it would silently produce an unmatchable value the moment a label
/// gains a space or a hyphen, and the failure mode — a drill-down that filters to
/// nothing — looks like "no patches" rather than a bug.
pub(crate) fn severity_raw(label: &str) -> Option<&'static str> {
    super::super::SEVERITY_OPTIONS
        .iter()
        .find(|(_, display)| *display == label)
        .map(|(raw, _)| *raw)
}

/// Severity rank mirroring the backend's `Severity::rank()` (`src-tauri/src/model.rs`):
/// Critical 7 → Unknown 0. Accepts the display label or NinjaOne's raw value in any
/// case, including the MSRC-style aliases the backend maps (`HIGH`, `MEDIUM`, `NONE`).
///
/// The one copy on the frontend. The demo's group sort kept its own table beside
/// [`sev_ordinal`]'s, so the two orderings could drift apart with nothing failing.
pub(crate) fn severity_rank(sev: &str) -> u8 {
    match sev.to_ascii_uppercase().as_str() {
        "CRITICAL" => 7,
        "IMPORTANT" | "HIGH" => 6,
        "SECURITY" => 5,
        "MODERATE" | "MEDIUM" => 4,
        "RECOMMENDED" => 3,
        "LOW" => 2,
        "OPTIONAL" | "NONE" => 1,
        _ => 0,
    }
}

/// Whether a pending patch of this severity is urgent — Critical, Important or
/// Security. Mirrors `model::Severity::is_urgent`, for the demo's rollups.
pub(crate) fn is_urgent(sev: &str) -> bool {
    severity_rank(sev) >= severity_rank("Security")
}

/// Severity ordinal (0 = most urgent) — the exact inverse of [`severity_rank`].
///
/// `Security` and `Recommended` were once missing, so both fell through to the
/// catch-all and tied with `Unknown` *below* `Optional` — the opposite of the
/// documented order, on the two bands NinjaOne uses most for third-party patches.
/// This only drives the browser demo (the desktop app sorts backend-side via
/// `RowSort`), which is also the README screenshot source.
pub(super) fn sev_ordinal(sev: &str) -> u8 {
    7 - severity_rank(sev)
}

/// Case-insensitive (ASCII) ordering without a per-comparison allocation.
fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// Missing values sort last regardless of direction (blanks never lead).
fn cmp_opt_last(a: Option<&str>, b: Option<&str>, desc: bool) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => {
            let o = cmp_ci(x, y);
            if desc { o.reverse() } else { o }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
