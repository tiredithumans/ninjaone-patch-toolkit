//! Filter-panel helpers: the `FilterParams` mapping behind every query, the
//! settings-number clamps, civil-date arithmetic for the date inputs, the
//! lookup-name helpers behind the multi-select controls, and the applied-filter
//! chip row.

use std::collections::BTreeMap;

use crate::types::{FilterParams, Location, Organization, Role};

use super::super::AppliedFilters;

pub(crate) fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Parses a `yyyy-mm-dd` date (the only shape an `<input type="date">` produces)
/// to Unix seconds at UTC midnight. `None` for empty or malformed input.
///
/// Deliberately arithmetic rather than `js_sys::Date::parse`. Two things follow.
/// It host-tests, which is what lets `filter_params` — the mapping behind *every*
/// query — be tested at all. And it is now the only civil-date implementation in
/// the crate: `demo.rs` carried a second, byte-identical one because its own
/// filtering had to run in the browser demo, so the two could disagree about a
/// month or leap boundary with nothing to catch it.
pub(crate) fn date_to_epoch(date: &str) -> Option<i64> {
    let mut parts = date.trim().split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // `1..=31` alone accepts dates that do not exist — 2026-02-31, 2026-04-31 — and
    // `days_from_civil` is happy to convert them, silently landing on a day in the
    // *next* month. That reaches the query as a date bound the operator never chose.
    if d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400)
}

/// Formats Unix seconds back to a `yyyy-mm-dd` date string (UTC), or "" for `None`.
pub(crate) fn epoch_to_date(epoch: Option<i64>) -> String {
    let Some(e) = epoch else {
        return String::new();
    };
    // Floor-divide so pre-1970 epochs land on the right day rather than rounding
    // toward zero into the next one.
    let days = e.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Length of `m` in `y`, proleptic Gregorian. Only used to reject impossible civil
/// dates before they reach [`days_from_civil`], which normalises rather than fails.
fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since the Unix epoch for a proleptic-Gregorian date (Howard Hinnant's
/// `days_from_civil`).
pub(super) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`], as `(year, month, day)`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The filter panel's fields as plain values, lifted out of the signals holding
/// them so the mapping below can be tested without a reactive runtime.
#[derive(Clone, Debug, Default)]
pub(crate) struct FilterInputs {
    pub organization_ids: Vec<i64>,
    pub location_ids: Vec<i64>,
    pub role_ids: Vec<i64>,
    pub node_classes: Vec<String>,
    pub severities: Vec<String>,
    pub os_name: String,
    pub search: String,
    /// "" (any), "1"/"7"/"30"/"90" (last N days), or "custom".
    pub detected_window: String,
    pub detected_after: String,
    pub detected_before: String,
}

/// Builds the `FilterParams` sent over IPC for a query.
///
/// Extracted from `FilterState::current_filter`, which built it inline in
/// `state.rs` — a file with no test module at all. This is the mapping behind
/// *every* query the app runs, and until now the only thing exercising it was the
/// type checker. The date-window branch in particular is easy to get quietly
/// wrong: the three outputs are mutually exclusive, an unrecognized window must
/// clear all of them rather than fall through to the custom dates, and the
/// "before" bound has to cover the whole selected day.
pub(crate) fn filter_params(i: FilterInputs) -> FilterParams {
    let (detected_within_days, detected_after, detected_before) = match i.detected_window.as_str() {
        "1" | "7" | "30" | "90" => (i.detected_window.parse::<i64>().ok(), None, None),
        "custom" => (
            None,
            date_to_epoch(&i.detected_after),
            // Include the whole "before" day: the picker yields midnight, so
            // without this a patch first seen at 09:00 on the chosen day falls
            // outside a window the operator believes includes it.
            date_to_epoch(&i.detected_before).map(|e| e + 86_399),
        ),
        _ => (None, None, None),
    };
    FilterParams {
        organization_ids: i.organization_ids,
        location_ids: i.location_ids,
        role_ids: i.role_ids,
        node_classes: i.node_classes,
        os_name_contains: non_empty(i.os_name),
        search: non_empty(i.search),
        severities: i.severities,
        detected_within_days,
        detected_after,
        detected_before,
    }
}

/// Parses a settings number field, clamping to `[min, max]` and falling back to
/// `current` when the field does not parse.
///
/// `<input type="number">` does not stop a user typing `0`, `abc` or pasting
/// `999999` — `min`/`max` are advisory in the DOM — so every one of these fields
/// carried its own inline `.parse().unwrap_or_else(...).clamp(..)` chain inside an
/// `on:change` closure in `settings.rs`, a file with no tests, where a component
/// body can only ever be compile-checked. The clamp bounds are the interesting
/// part: the backend re-validates them, so a frontend that clamps to the wrong
/// range turns a typo into a rejected save with no field-level explanation.
/// Parsing goes through `i64` rather than through `T` so a number that overflows
/// the field's own type still clamps. Typing `99999` into the port box used to
/// fail `parse::<u16>()` and revert the field to its previous value — indis-
/// tinguishable, to the operator, from the input being ignored. It now lands on
/// 65535, which is what `max` already promises.
pub(crate) fn parse_clamped<T>(raw: &str, current: T, min: T, max: T) -> T
where
    T: Copy + TryFrom<i64> + TryInto<i64>,
{
    let (Ok(lo), Ok(hi)): (Result<i64, _>, Result<i64, _>) = (min.try_into(), max.try_into())
    else {
        return current;
    };
    match raw.trim().parse::<i64>() {
        Ok(v) => T::try_from(v.clamp(lo, hi)).unwrap_or(current),
        Err(_) => current,
    }
}

/// Parses an optional numeric settings field: blank clears it, a bad value keeps
/// what was there. Used for the two remediation script IDs, where "unset" is a
/// meaningful state (the action is simply not offered) and must stay reachable by
/// clearing the box.
pub(crate) fn parse_optional_id(raw: &str, current: Option<i64>) -> Option<i64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    // A script id is a positive integer from the library URL; 0 and negatives are
    // not addressable, so treat them as "leave it alone" rather than storing an id
    // that can only ever 404 at dispatch time.
    match t.parse::<i64>() {
        Ok(v) if v > 0 => Some(v),
        _ => current,
    }
}

/// One applied-filter chip. `patch` marks a patch-tier facet so the view can grey it
/// out on Fleet-health tabs, where patch filters don't apply.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FilterChip {
    pub label: String,
    pub patch: bool,
}

/// Humanizes the first-seen filter into a chip label, or `None` when no window is
/// set. Pure (no `js_sys`), unlike `date_to_epoch`/`epoch_to_date`, so it host-tests.
pub(crate) fn detected_label(window: &str, after: &str, before: &str) -> Option<String> {
    match window {
        "1" => Some("last 24 hours".to_string()),
        "7" => Some("last 7 days".to_string()),
        "30" => Some("last 30 days".to_string()),
        "90" => Some("last 90 days".to_string()),
        "custom" => {
            let (a, b) = (after.trim(), before.trim());
            match (a.is_empty(), b.is_empty()) {
                (false, false) => Some(format!("{a} \u{2192} {b}")),
                (false, true) => Some(format!("after {a}")),
                (true, false) => Some(format!("before {b}")),
                (true, true) => None,
            }
        }
        _ => None,
    }
}

/// Anything the scope multi-selects can list: an id and a display name.
///
/// The three lookups (`Organization`/`Location`/`Role`) are separate types with the
/// same two fields, so one trait keeps the picker, the chip row and the snapshot
/// generic over all three instead of triplicating each of them.
pub(crate) trait Named {
    fn id(&self) -> i64;
    fn name(&self) -> &str;
}

impl Named for Organization {
    fn id(&self) -> i64 {
        self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
}

impl Named for Location {
    fn id(&self) -> i64 {
        self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
}

impl Named for Role {
    fn id(&self) -> i64 {
        self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
}

/// Resolves selected ids to display names, in the lookup's own order.
///
/// Ordered by the lookup rather than by the selection so the chip reads the same way
/// the picker does. An id with no matching lookup row is **named, not dropped**:
/// `#4711 (not found)`, after the resolved names. It used to be dropped on the
/// theory that a bare id is not a useful label — but a dropped id is still in the
/// query. A preset whose organization has since been deleted, or a scope carried
/// across a tenant switch, then ran a query that matched no device while the chip
/// row read "No filters — whole fleet" beside zero rows. The backend's provenance
/// block prints the same case as `id 4711` for the same reason.
pub(crate) fn names_for<T: Named>(
    selected: &[i64],
    options: impl Iterator<Item = T>,
) -> Vec<String> {
    let mut seen: Vec<i64> = Vec::new();
    let mut names: Vec<String> = options
        .filter(|o| selected.contains(&o.id()))
        .map(|o| {
            seen.push(o.id());
            o.name().to_string()
        })
        .collect();
    names.extend(
        selected
            .iter()
            .filter(|id| !seen.contains(id))
            .map(|id| format!("#{id} (not found)")),
    );
    names
}

/// Filters a lookup list by a case-insensitive substring of the display name, for
/// the picker's search box. An empty needle matches everything.
pub(crate) fn matching_options<T: Named + Clone>(options: &[T], needle: &str) -> Vec<T> {
    let needle = needle.trim().to_lowercase();
    options
        .iter()
        .filter(|o| needle.is_empty() || o.name().to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

/// Qualifies location names that are ambiguous across the selected organizations.
///
/// With the organization facet multi-select, the location list spans several orgs at
/// once, and "HQ" or "Main Office" is the same string under most of them — a picker
/// offering three identical entries is not a choice anyone can make. Only genuinely
/// duplicated names are qualified, so the common single-org case stays unchanged.
pub(crate) fn disambiguate_locations(
    locations: &[Location],
    orgs: &[Organization],
) -> Vec<Location> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for l in locations {
        *seen.entry(l.name.as_str()).or_default() += 1;
    }
    locations
        .iter()
        .map(|l| {
            let ambiguous = seen.get(l.name.as_str()).is_some_and(|n| *n > 1);
            let org = l
                .organization_id
                .and_then(|id| orgs.iter().find(|o| o.id == id))
                .map(|o| o.name.as_str());
            match (ambiguous, org) {
                (true, Some(org)) => Location {
                    name: format!("{} \u{00b7} {org}", l.name),
                    ..l.clone()
                },
                _ => l.clone(),
            }
        })
        .collect()
}

/// The picker's collapsed summary: what the current selection covers.
///
/// Names the selection while it is short enough to read, and falls back to a count
/// once it isn't — a scope of eleven sites is not legible as a comma list, and the
/// chip row below the results carries the full list anyway.
pub(crate) fn selection_label(selected_names: &[String], all_label: &str) -> String {
    match selected_names.len() {
        0 => all_label.to_string(),
        1..=2 => selected_names.join(", "),
        n => format!("{n} selected"),
    }
}

/// Builds the glanceable chip row from a Run-time snapshot — one chip per non-default
/// facet (an empty vec ⇒ the caller shows a "whole fleet" placeholder). Device-scope
/// facets come first (`patch: false`), then the patch-tier facets (`patch: true`).
pub(crate) fn filter_chips(f: &AppliedFilters) -> Vec<FilterChip> {
    let mut out = Vec::new();
    for (label, names) in [
        ("Org", &f.organizations),
        ("Location", &f.locations),
        ("Role", &f.roles),
    ] {
        if !names.is_empty() {
            out.push(FilterChip {
                label: format!("{label}: {}", names.join(", ")),
                patch: false,
            });
        }
    }
    if !f.os_types.is_empty() {
        out.push(FilterChip {
            label: format!("OS Type: {}", f.os_types.join(", ")),
            patch: false,
        });
    }
    if let Some(n) = &f.os_name {
        out.push(FilterChip {
            label: format!("OS name: {n}"),
            patch: false,
        });
    }
    // Device-tier, not patch-tier: only the families the query fetched are in the
    // fleet-health rollups, so Type narrows Compliance and Needs Reboot as much as it
    // narrows the rows. Marked `patch: true` it was struck through on those tabs
    // with "Ignored on this tab" directly above a banner saying the opposite.
    if matches!(f.patch_type.as_str(), "OS" | "SOFTWARE") {
        out.push(FilterChip {
            label: format!("Type: {}", f.patch_type),
            patch: false,
        });
    }
    if !f.statuses.is_empty() {
        out.push(FilterChip {
            label: format!("Status: {}", f.statuses.join(", ")),
            patch: true,
        });
    }
    if !f.severities.is_empty() {
        out.push(FilterChip {
            label: format!("Severity: {}", f.severities.join(", ")),
            patch: true,
        });
    }
    if let Some(s) = &f.search {
        out.push(FilterChip {
            label: format!("Search: {s}"),
            patch: true,
        });
    }
    if let Some(rl) = detected_label(&f.detected_window, &f.detected_after, &f.detected_before) {
        out.push(FilterChip {
            label: format!("First seen: {rl}"),
            patch: true,
        });
    }
    if let Some(d) = f.install_days {
        // Covers FAILED as well as INSTALLED — the backend bounds the whole
        // install-history pull by this window, so "Installed within" understated
        // what it applies to on a failures-only run.
        out.push(FilterChip {
            label: format!("Install history: last {d}d"),
            patch: true,
        });
    }
    out
}
