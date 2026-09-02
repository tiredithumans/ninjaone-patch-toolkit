//! Pager arithmetic for the results tables. Inline in a component this once
//! left 98% of groups unreachable; here it is host-tested.

use super::*;

/// Number of pages needed for `total` items, never less than 1 so "Page 1 of 1"
/// is what an empty result reads as rather than "Page 1 of 0".
pub(crate) fn page_count(total: usize, page_size: usize) -> usize {
    if page_size == 0 {
        return 1;
    }
    total.div_ceil(page_size).max(1)
}

/// Clamps a stored page index into range. The stored index outlives the result it
/// was chosen against — an auto-refresh returning fewer rows, or switching between
/// flat and grouped view, can both leave it past the end.
pub(crate) fn clamp_page(stored: usize, page_count: usize) -> usize {
    stored.min(page_count.saturating_sub(1))
}

/// Half-open `[start, end)` item range shown on `page`, clamped to `total`.
pub(crate) fn page_bounds(page: usize, page_size: usize, total: usize) -> (usize, usize) {
    let start = (page * page_size).min(total);
    let end = start.saturating_add(page_size).min(total);
    (start, end)
}

/// The pager caption, e.g. `Rows 101–200 of 12,300 · Page 2 of 123`.
///
/// `unit` names what is being paged, which is *not* always rows: the grouped view
/// pages group headers. Sizing this from the row total while grouped is what once
/// left ~98% of groups unreachable — it read "Page 1 of 400" off 40,000 rows while
/// the view only ever rendered the first 100 groups.
pub(crate) fn pager_summary(unit: &str, page: usize, page_size: usize, total: usize) -> String {
    let (start, end) = page_bounds(page, page_size, total);
    format!(
        "{} {}\u{2013}{} of {} \u{00b7} Page {} of {}",
        unit,
        if total == 0 { 0 } else { start + 1 },
        end,
        group_thousands(total),
        page + 1,
        page_count(total, page_size),
    )
}

/// Page index one step back, saturating at the first page.
pub(crate) fn prev_page(current: usize) -> usize {
    current.saturating_sub(1)
}

/// Page index one step forward, saturating at the last page. Takes `page_count`
/// rather than a total so the caller cannot disagree with `page_count` about how
/// many pages exist.
pub(crate) fn next_page(current: usize, page_count: usize) -> usize {
    let last = page_count.saturating_sub(1);
    current.min(last).saturating_add(1).min(last)
}
