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
