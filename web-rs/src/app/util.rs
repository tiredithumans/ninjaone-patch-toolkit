//! Small, standalone view helpers shared across the app components: option/date
//! parsing, number formatting, and CSS-class pickers. They touch no `AppState`, so
//! they live here rather than bloating `app.rs`. Every helper here is JS-free and
//! unit-tests on the host target — the date pair used to reach for `js_sys::Date`
//! and so could not be tested at all; it is now plain arithmetic.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use super::state::{DeviceSelection, SelectedPatch};
use super::{AppliedFilters, Tab};
use crate::types::{
    ActionKind, AuthStatus, FilterParams, JobReport, PatchRow, RebootMode, RowSort, RowSortKey,
};

/// What [`AppState::run_query_inner`] should do, decided from the flags alone.
///
/// Lifted out of the component-adjacent method so the guard chain is reachable by a
/// test: `state.rs` has no test module, and this ordering is load-bearing — a demo
/// run must be checked *before* the auth guard (the demo has no session and would
/// otherwise be told to sign in), and the busy guard before both (an auto-refresh
/// tick firing during a manual Run must not start a second one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunDecision {
    /// A run is already in flight; do nothing.
    AlreadyRunning,
    /// No backend — filter the sample locally.
    Demo,
    NotSignedIn,
    NoStatusSelected,
    Run,
}

pub(crate) fn run_decision(
    busy: bool,
    refreshing: bool,
    demo: bool,
    authed: bool,
    statuses_empty: bool,
) -> RunDecision {
    if busy || refreshing {
        RunDecision::AlreadyRunning
    } else if demo {
        RunDecision::Demo
    } else if !authed {
        RunDecision::NotSignedIn
    } else if statuses_empty {
        RunDecision::NoStatusSelected
    } else {
        RunDecision::Run
    }
}

/// The stamp identifying one run. Wrapping on purpose: only equality is ever asked
/// of it, so an overflow after 2^64 runs is harmless, whereas a plain `+ 1` would
/// panic in debug.
pub(crate) fn next_query_seq(current: u64) -> u64 {
    current.wrapping_add(1)
}

/// Whether a completed run has been overtaken by a newer one and must not paint.
///
/// Queries overlap routinely — an auto-refresh tick fires while a manual Run is
/// still paging the fleet — and they do not resolve in start order, so without this
/// a superseded response could overwrite a newer one on screen while the backend,
/// which drops the superseded *cache* write, kept the newer rows.
pub(crate) fn is_superseded(current_seq: u64, my_seq: u64) -> bool {
    current_seq != my_seq
}

/// Applies one row's checkbox to the selection map.
///
/// Pure map surgery, lifted out of the signal closure so it can be tested: this is
/// the load-bearing half of the selection model. A device enters the selection with
/// its first ticked row and leaves with its last, and ticking one row must affect
/// **only** that row — an earlier shape swept every KB on the device into the
/// selection, which made the one path capable of per-patch targeting unable to
/// receive a subset.
pub(crate) fn apply_row_selection(
    sel: &mut BTreeMap<i64, DeviceSelection>,
    row: &PatchRow,
    checked: bool,
) {
    let key = patch_key(row);
    if checked {
        sel.entry(row.device_id)
            .or_insert_with(|| DeviceSelection {
                name: row.device_name.clone(),
                organization: row.organization.clone(),
                offline: row.offline,
                patches: BTreeMap::new(),
            })
            .patches
            .insert(
                key,
                SelectedPatch {
                    kb: row.kb.clone().filter(|k| !k.is_empty()),
                    name: row.name.clone(),
                    is_os: row.patch_type.eq_ignore_ascii_case("OS"),
                },
            );
    } else if let Some(entry) = sel.get_mut(&row.device_id) {
        entry.patches.remove(&key);
        // The device leaves with its last ticked row, so a device with nothing
        // ticked is never dispatched against.
        if entry.patches.is_empty() {
            sel.remove(&row.device_id);
        }
    }
}

/// Identity of a patch *within a device's selection*.
///
/// The same `(patch_type, kb, name)` tuple the backend groups patches by, so a row
/// ticked in the flat view and the same patch ticked inside a grouped view refer to
/// one thing. Joined with a unit separator, which can't occur in a patch name, so
/// two distinct patches can never produce the same key.
pub(crate) fn patch_key(row: &PatchRow) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        row.patch_type,
        row.kb.as_deref().unwrap_or(""),
        row.name
    )
}

/// The per-device target list a script dispatch would send, keyed by device id.
///
/// Devices with nothing ticked *of that family* are omitted entirely rather than
/// mapped to an empty list — for a remediation action they are not dispatched to at
/// all, so the confirmation dialog's device count is the number of devices that will
/// actually install something. Third-party patches carry no KB, so an OS target list
/// silently skips them and vice versa; that is the same asymmetry the two NinjaOne
/// feeds have.
///
/// Every path that sends an allow list goes through this — the two remediation kinds
/// and the hand-picked script's "Target only the selected KBs". Nothing hands a
/// device the batch-wide union of the selection any more.
pub(crate) fn targets_by_device(
    selected: &BTreeMap<i64, DeviceSelection>,
    want_os: bool,
) -> BTreeMap<i64, Vec<String>> {
    selected
        .iter()
        .filter_map(|(id, device)| {
            let targets: Vec<String> = device
                .patches
                .values()
                .filter(|p| p.is_os == want_os)
                // OS patches are targeted by KB, software by product title.
                .filter_map(|p| {
                    if want_os {
                        p.kb.clone()
                    } else {
                        Some(p.name.clone())
                    }
                })
                .filter(|t| !t.trim().is_empty())
                // The same patch can appear on a device via two rows (an install
                // attempt and the pending record); the allow list wants it once.
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            (!targets.is_empty()).then_some((*id, targets))
        })
        .collect()
}

/// [`targets_by_device`] for a remediation kind, which picks the family. Empty for
/// any other kind — the native endpoints take no target list at all.
pub(crate) fn remediation_targets(
    selected: &BTreeMap<i64, DeviceSelection>,
    kind: ActionKind,
) -> BTreeMap<i64, Vec<String>> {
    if !kind.is_remediation() {
        return BTreeMap::new();
    }
    targets_by_device(selected, kind.is_os_family())
}

/// What "install only the selected patches" would send, in one line per family.
///
/// `None` when the family has nothing ticked, so a selection of pure OS patches
/// doesn't render an empty "Software: 0" next to it.
pub(crate) fn remediation_summary(
    kind: ActionKind,
    targets: &BTreeMap<i64, Vec<String>>,
) -> Option<String> {
    if targets.is_empty() {
        return None;
    }
    let family = if kind.is_os_family() {
        "OS"
    } else {
        "Software"
    };
    // Distinct patches, not the sum of per-device lists: the same KB ticked on ten
    // devices is one patch going to ten devices, and reporting "10 patches" for it
    // is the batch-wide reading this whole path exists to replace.
    let distinct: BTreeSet<&String> = targets.values().flatten().collect();
    Some(format!(
        "{family}: {} patch(es) on {} device(s)",
        distinct.len(),
        targets.len()
    ))
}

/// What the script picker's "Target only the selected KBs" would actually send.
///
/// Spells out *per device* because that is the whole correction: the checkbox used to
/// hand every device the combined list from the entire selection.
pub(crate) fn kb_targeting_summary(targets: &BTreeMap<i64, Vec<String>>) -> String {
    if targets.is_empty() {
        return "No KBs selected — every device would be sent an empty allow list.".to_string();
    }
    let distinct: BTreeSet<&String> = targets.values().flatten().collect();
    format!(
        "Each device is sent only its own KBs — {} distinct KB(s) across {} device(s).",
        distinct.len(),
        targets.len()
    )
}

/// Why a specific action is unavailable, beyond the reasons that block all of them.
///
/// Separate from [`action_disabled_reason`] because these depend on the kind and on
/// what is ticked: the remediation actions need a configured script *and* a ticked
/// patch of their own family, and an operator who has ticked only software rows
/// needs to be told that, not left with a button that looks broken.
pub(crate) fn kind_disabled_reason(
    kind: ActionKind,
    script_configured: bool,
    matching_targets: usize,
) -> Option<String> {
    if !kind.is_remediation() {
        return None;
    }
    let family = if kind.is_os_family() {
        "OS"
    } else {
        "software"
    };
    if !script_configured {
        return Some(format!(
            "No {family} remediation script configured. NinjaOne has no per-patch apply endpoint, \
             so installing specific patches needs a library script that accepts a target list — \
             add its id in Settings → Patch actions."
        ));
    }
    if matching_targets == 0 {
        return Some(format!(
            "No {family} patches selected. Tick the {family} patch rows to install."
        ));
    }
    None
}

/// Why the patch-action affordances are unavailable, or `None` when they're live.
///
/// Pure and derived on demand rather than cached in a signal: it was previously
/// stored and recomputed by hand at two call sites, so signing in (or enabling
/// actions in Settings) left the startup verdict — "Sign in to run patch actions."
/// — on screen until the app was restarted. Deriving it means every input change
/// is picked up for free. The backend re-checks all of this; this only decides
/// what the UI offers.
///
/// `auth` is `None` before the first `auth_status` reply, which is treated as
/// not-yet-signed-in so the controls read as blocked while we don't know, rather
/// than briefly offering actions that would be rejected.
pub(crate) fn action_blocked_reason(
    web_mode: bool,
    demo: bool,
    auth: Option<&AuthStatus>,
) -> Option<String> {
    if web_mode || demo {
        return Some("Patch actions run only in the desktop app.".to_string());
    }
    let Some(status) = auth.filter(|a| a.authenticated) else {
        return Some("Sign in to run patch actions.".to_string());
    };
    if !status.actions_enabled {
        return Some("Patch actions are disabled — enable them in Settings.".to_string());
    }
    if !status.write_enabled {
        // Distinguish "we know it's read-only" from "we can't tell", so the
        // operator isn't told their consent was wrong when it may be fine.
        return Some(if status.scope_known {
            "Your NinjaOne sign-in is read-only. Re-authorize to enable actions.".to_string()
        } else {
            "Couldn't confirm your sign-in grants the Management scope. Re-authorize to be sure."
                .to_string()
        });
    }
    None
}

pub(crate) fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

pub(crate) fn parse_opt(s: &str) -> Option<i64> {
    s.trim().parse().ok()
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

/// Days since the Unix epoch for a proleptic-Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
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
    pub organization_id: Option<i64>,
    pub location_id: Option<i64>,
    pub role_id: Option<i64>,
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
        organization_id: i.organization_id,
        location_id: i.location_id,
        role_id: i.role_id,
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

/// Why every action button is disabled, or `None` when they are live.
///
/// The precedence is the point and is why this is not inline: the tooltip must
/// name the most *fundamental* obstacle, so "sign in" outranks "select a device",
/// which outranks "an action is already running". Reordering these silently tells
/// an operator to pick devices when the real problem is that they are signed out.
/// `blocked` is [`action_blocked_reason`]'s verdict.
pub(crate) fn action_disabled_reason(
    blocked: Option<String>,
    selected_devices: usize,
    dispatching: bool,
) -> Option<String> {
    if blocked.is_some() {
        return blocked;
    }
    if selected_devices == 0 {
        return Some("Select at least one device first".to_string());
    }
    if dispatching {
        return Some("An action is already being dispatched".to_string());
    }
    None
}

/// The action bar's selection summary. `None` when nothing is selected, so the
/// caller renders the hint instead.
///
/// The offline clause is load-bearing for the operator's expectations: an action
/// against an offline device is *queued*, not run, so the count has to be visible
/// before they confirm — but only when it is non-zero, or every selection carries
/// a distracting "0 offline".
pub(crate) fn selection_summary(devices: usize, rows: usize, offline: usize) -> Option<String> {
    if devices == 0 {
        return None;
    }
    let mut text = format!(
        "{} device(s) selected · {} patch row(s)",
        group_thousands(devices),
        group_thousands(rows),
    );
    if offline > 0 {
        text.push_str(&format!(" · {offline} offline"));
    }
    Some(text)
}

/// Formats a count with thousands separators (e.g. `12300` → `12,300`).
pub(crate) fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub(crate) fn tab_class(active: Tab, this: Tab) -> &'static str {
    if active == this { "tab tab-on" } else { "tab" }
}

/// The count shown on a group header, which counts the *other* axis from the one
/// the rows are grouped by: a by-device group is summarised by how many patches it
/// holds, a by-patch group by how many devices it spans.
///
/// Extracted because the two are trivially invertible and the inverted form still
/// renders a plausible-looking number — the kind of mistake only an assertion
/// catches.
pub(crate) fn group_count_label(by_device: bool, rows: usize, devices: usize) -> String {
    if by_device {
        format!("{} patches", group_thousands(rows))
    } else {
        format!("{} devices", group_thousands(devices))
    }
}

/// Whether the confirm dialog must demand a typed device count rather than a
/// single click. A forced reboot is the one action here that destroys unsaved work
/// on machines the operator may not own, so it is deliberately harder to fire.
///
/// A blocked plan never needs it: the dispatch button is disabled anyway, and
/// asking someone to type a count that cannot be submitted reads as a bug.
pub(crate) fn needs_typed_confirmation(
    blocked: bool,
    kind: ActionKind,
    reboot_mode: Option<RebootMode>,
) -> bool {
    !blocked && kind == ActionKind::Reboot && reboot_mode == Some(RebootMode::Forced)
}

/// Whether the confirm button may fire. Extracted from the modal body so the rule
/// that guards the destructive path is host-testable rather than only reachable by
/// clicking through a browser.
pub(crate) fn can_confirm_action(
    blocked: bool,
    dispatching: bool,
    needs_typed: bool,
    typed: &str,
    expected: &str,
) -> bool {
    !blocked && !dispatching && (!needs_typed || typed.trim() == expected)
}

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

/// The counts the tier-aware results summary needs — all already on `QueryResult`,
/// so the summary can describe whichever tab is active without extra backend data.
pub(crate) struct SummaryCounts {
    pub rows_total: usize,
    pub devices_total: usize,
    pub failures: usize,
    pub orgs: usize,
    pub reboot: usize,
}

/// Builds the results-summary line for the active tab. The old shared line read
/// "{rows} patch rows …" across every tab, which is the Patches detail-row count —
/// misleading on Compliance/Reboot, whose scope is devices, not patch rows.
pub(crate) fn summary_line(tab: Tab, c: &SummaryCounts, generated_at: &str) -> String {
    let head = match tab {
        Tab::Patches => format!(
            "{} patch rows across {} devices",
            group_thousands(c.rows_total),
            group_thousands(c.devices_total),
        ),
        Tab::Failures => format!(
            "{} failing patches across {} devices",
            group_thousands(c.failures),
            group_thousands(c.devices_total),
        ),
        Tab::Compliance => format!(
            "{} organizations \u{00b7} {} devices",
            group_thousands(c.orgs),
            group_thousands(c.devices_total),
        ),
        Tab::Reboot => format!(
            "{} of {} devices need reboot",
            group_thousands(c.reboot),
            group_thousands(c.devices_total),
        ),
        // Jobs describes dispatched actions, not the query result, so the query's
        // counts and generation time would be actively misleading here.
        Tab::Jobs => return "Dispatched actions".to_string(),
    };
    format!("{head} \u{00b7} generated {generated_at}")
}

/// Fleet-health tabs (Compliance, Needs Reboot) reflect the device scope only and
/// ignore the patch filters; Filtered-results tabs (Patches, Failures) honor them.
/// Jobs is neither — it shows dispatch history, not query output.
pub(crate) fn is_fleet_tab(tab: Tab) -> bool {
    matches!(tab, Tab::Compliance | Tab::Reboot)
}

/// Short mode label for a dispatched job. A dry run applies — and reboots —
/// nothing, so it wins over the reboot indication.
pub(crate) fn job_mode_label(job: &JobReport) -> &'static str {
    if job.dry_run {
        "Dry run"
    } else if job.kind.can_reboot() {
        "Live + reboot"
    } else {
        "Live"
    }
}

/// Renders an elapsed-seconds count as `1m 20s` / `45s`, or blank while running.
pub(crate) fn format_duration(seconds: Option<i64>) -> String {
    match seconds {
        None => String::new(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) => format!("{}m {}s", s / 60, s % 60),
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

/// Builds the glanceable chip row from a Run-time snapshot — one chip per non-default
/// facet (an empty vec ⇒ the caller shows a "whole fleet" placeholder). Device-scope
/// facets come first (`patch: false`), then the patch-tier facets (`patch: true`).
pub(crate) fn filter_chips(f: &AppliedFilters) -> Vec<FilterChip> {
    let mut out = Vec::new();
    if let Some(o) = &f.organization {
        out.push(FilterChip {
            label: format!("Org: {o}"),
            patch: false,
        });
    }
    if let Some(l) = &f.location {
        out.push(FilterChip {
            label: format!("Location: {l}"),
            patch: false,
        });
    }
    if let Some(r) = &f.role {
        out.push(FilterChip {
            label: format!("Role: {r}"),
            patch: false,
        });
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
    if matches!(f.patch_type.as_str(), "OS" | "SOFTWARE") {
        out.push(FilterChip {
            label: format!("Type: {}", f.patch_type),
            patch: true,
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

pub(crate) fn sev_class(sev: &str) -> &'static str {
    match sev {
        "Critical" => "sev sev-critical",
        "Important" => "sev sev-important",
        "Security" => "sev sev-security",
        "Moderate" => "sev sev-moderate",
        "Recommended" => "sev sev-recommended",
        "Low" => "sev sev-low",
        "Optional" => "sev sev-optional",
        "Unknown" => "sev sev-unknown",
        // Anything the backend's `Severity::label()` doesn't produce. Distinct from
        // `sev-unknown`, which is a severity NinjaOne *did* send and `from_raw`
        // couldn't map — that distinction is the difference between "low priority"
        // and "we don't know what this is", and both used to render identically.
        _ => "sev sev-none",
    }
}

pub(crate) fn status_class(status: &str) -> &'static str {
    match status {
        "INSTALLED" => "stat stat-installed",
        "APPROVED" => "stat stat-approved",
        "PENDING" => "stat stat-pending",
        "REJECTED" => "stat stat-rejected",
        "FAILED" => "stat stat-failed",
        _ => "stat",
    }
}

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
    super::SEVERITY_OPTIONS
        .iter()
        .find(|(_, display)| *display == label)
        .map(|(raw, _)| *raw)
}

/// Severity ordinal (0 = most urgent) — the exact inverse of the backend's
/// `Severity::rank()` (`src-tauri/src/model.rs`), which runs Critical 7 → Unknown 0.
///
/// `Security` and `Recommended` were missing, so both fell through to the catch-all
/// and tied with `Unknown` *below* `Optional` — the opposite of the documented order,
/// on the two bands NinjaOne uses most for third-party patches. This only drives the
/// browser demo (the desktop app sorts backend-side via `RowSort`), which is also the
/// README screenshot source.
fn sev_ordinal(sev: &str) -> u8 {
    match sev {
        "Critical" => 0,
        "Important" => 1,
        "Security" => 2,
        "Moderate" => 3,
        "Recommended" => 4,
        "Low" => 5,
        "Optional" => 6,
        _ => 7,
    }
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

/// Presentation for an "Aged (past SLA)" table cell as (CSS class, label, title).
/// An aged backlog gets a ⚠ prefix so it reads without relying on color.
pub(crate) fn aged_badge(aged: usize) -> (&'static str, String, &'static str) {
    if aged > 0 {
        (
            "sev-critical",
            format!("⚠ {aged}"),
            "Past SLA — needs attention",
        )
    } else {
        ("", aged.to_string(), "")
    }
}

/// One inline run within a changelog line: plain text or a `**bold**` span.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MdSpan {
    Text(String),
    Strong(String),
}

/// One rendered block of the update changelog (a `CHANGELOG.md` version section).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MdBlock {
    /// A `#`/`##`/`###` section heading (e.g. "Added", "Fixed").
    Heading(String),
    /// A bullet list; each item is its sequence of inline spans.
    List(Vec<Vec<MdSpan>>),
    /// A free-text paragraph (the GitHub fallback note, or any non-list text).
    Paragraph(Vec<MdSpan>),
}

/// Splits one line into `**bold**` and plain-text runs. An unterminated `**` is
/// left as literal text so we never drop content.
pub(crate) fn parse_inline(text: &str) -> Vec<MdSpan> {
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("**") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("**") else {
            break; // no closing marker — the rest is plain text
        };
        if open > 0 {
            spans.push(MdSpan::Text(rest[..open].to_string()));
        }
        let bold = &after[..close];
        if !bold.is_empty() {
            spans.push(MdSpan::Strong(bold.to_string()));
        }
        rest = &after[close + 2..];
    }
    if !rest.is_empty() {
        spans.push(MdSpan::Text(rest.to_string()));
    }
    spans
}

/// Parses the changelog subset the updater notes use — `#` headings, `-`/`*` bullet
/// lists (wrapped continuation lines fold into the bullet), `**bold**`, and plain
/// paragraphs — into renderable blocks. Anything unrecognized falls through as text,
/// so the GitHub fallback note ("See the release notes …") renders as a paragraph.
pub(crate) fn parse_changelog(src: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let mut items: Vec<String> = Vec::new(); // raw text of the bullets in the open list
    let mut para: Vec<String> = Vec::new(); // raw lines of the open paragraph

    for raw in src.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        if trimmed.is_empty() {
            flush_para(&mut blocks, &mut para);
            flush_list(&mut blocks, &mut items);
        } else if trimmed.starts_with('#') {
            flush_para(&mut blocks, &mut para);
            flush_list(&mut blocks, &mut items);
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() {
                blocks.push(MdBlock::Heading(heading.to_string()));
            }
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            flush_para(&mut blocks, &mut para); // a list and paragraph never overlap
            items.push(item.trim().to_string());
        } else if let Some(last) = items.last_mut() {
            // A non-blank, non-marker line under a bullet is a wrapped continuation.
            last.push(' ');
            last.push_str(trimmed);
        } else {
            para.push(trimmed.to_string());
        }
    }
    flush_para(&mut blocks, &mut para);
    flush_list(&mut blocks, &mut items);
    blocks
}

fn flush_list(blocks: &mut Vec<MdBlock>, items: &mut Vec<String>) {
    if !items.is_empty() {
        let spans = items.drain(..).map(|i| parse_inline(&i)).collect();
        blocks.push(MdBlock::List(spans));
    }
}

fn flush_para(blocks: &mut Vec<MdBlock>, para: &mut Vec<String>) {
    if !para.is_empty() {
        let text = para.join(" ");
        para.clear();
        blocks.push(MdBlock::Paragraph(parse_inline(&text)));
    }
}

// Host-target unit tests for the JS-free pure helpers. The wasm build excludes this
// module (`cfg(test)` is never set there); the date helpers call `js_sys::Date`,
// which only runs in the browser, so they're deliberately not covered here.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ActionKind, JobState};

    /// A group header counts the axis it is NOT grouped by. Inverting these still
    /// renders a plausible number, so assert the direction explicitly.
    #[test]
    fn group_header_counts_the_opposite_axis() {
        // Grouped by device: the header says how many patches that device has.
        assert_eq!(group_count_label(true, 1_200, 1), "1,200 patches");
        // Grouped by patch: the header says how many devices need it.
        assert_eq!(group_count_label(false, 1_200, 340), "340 devices");
        assert_eq!(group_count_label(true, 0, 0), "0 patches");
        assert_eq!(group_count_label(false, 0, 0), "0 devices");
    }

    /// The typed-count gate exists to make a forced reboot hard to fire by accident.
    #[test]
    fn only_a_forced_reboot_demands_a_typed_confirmation() {
        assert!(needs_typed_confirmation(
            false,
            ActionKind::Reboot,
            Some(RebootMode::Forced)
        ));
        // A normal reboot, and every non-reboot action, confirm with a click.
        assert!(!needs_typed_confirmation(
            false,
            ActionKind::Reboot,
            Some(RebootMode::Normal)
        ));
        assert!(!needs_typed_confirmation(false, ActionKind::Reboot, None));
        assert!(!needs_typed_confirmation(
            false,
            ActionKind::OsPatchApply,
            Some(RebootMode::Forced)
        ));
        // A blocked plan can't be dispatched at all, so demanding a typed count
        // would just be a dead end.
        assert!(!needs_typed_confirmation(
            true,
            ActionKind::Reboot,
            Some(RebootMode::Forced)
        ));
    }

    #[test]
    fn confirm_is_refused_until_the_device_count_is_typed_exactly() {
        // No typed count required: a click is enough.
        assert!(can_confirm_action(false, false, false, "", "12"));

        // Typed count required.
        assert!(can_confirm_action(false, false, true, "12", "12"));
        assert!(
            can_confirm_action(false, false, true, "  12  ", "12"),
            "surrounding whitespace is forgiven"
        );
        assert!(!can_confirm_action(false, false, true, "", "12"));
        assert!(!can_confirm_action(false, false, true, "1", "12"));
        assert!(!can_confirm_action(false, false, true, "13", "12"));
        assert!(
            !can_confirm_action(false, false, true, "1 2", "12"),
            "interior whitespace is not stripped"
        );

        // A blocked plan or an in-flight dispatch overrides everything, so a
        // correct count cannot double-fire an action.
        assert!(!can_confirm_action(true, false, true, "12", "12"));
        assert!(!can_confirm_action(false, true, true, "12", "12"));
        assert!(!can_confirm_action(true, false, false, "", "12"));
        assert!(!can_confirm_action(false, true, false, "", "12"));
    }

    #[test]
    fn page_count_never_reports_zero_pages() {
        assert_eq!(
            page_count(0, 100),
            1,
            "an empty result is still 'Page 1 of 1'"
        );
        assert_eq!(page_count(1, 100), 1);
        assert_eq!(
            page_count(100, 100),
            1,
            "an exact fill must not spill a page"
        );
        assert_eq!(page_count(101, 100), 2);
        assert_eq!(page_count(40_000, 100), 400);
        // Degenerate page size must not divide by zero.
        assert_eq!(page_count(50, 0), 1);
    }

    /// The stored page index outlives the result it was chosen against, so every
    /// read has to clamp. Not clamping is what stranded the view past the end when
    /// an auto-refresh returned fewer rows.
    #[test]
    fn clamp_page_pulls_a_stale_index_back_into_range() {
        assert_eq!(clamp_page(0, 1), 0);
        assert_eq!(clamp_page(7, 400), 7, "in range is left alone");
        assert_eq!(clamp_page(399, 400), 399, "the last page is in range");
        assert_eq!(clamp_page(400, 400), 399, "one past the end clamps back");
        assert_eq!(clamp_page(9_999, 3), 2);
        // A result that shrank to empty still has one page, index 0.
        assert_eq!(clamp_page(12, 1), 0);
        assert_eq!(clamp_page(12, 0), 0, "no pages must not underflow");
    }

    #[test]
    fn page_bounds_clamp_to_the_total() {
        assert_eq!(page_bounds(0, 100, 250), (0, 100));
        assert_eq!(page_bounds(1, 100, 250), (100, 200));
        // Last page is partial.
        assert_eq!(page_bounds(2, 100, 250), (200, 250));
        // Exact fill: the last page is full, not empty.
        assert_eq!(page_bounds(1, 100, 200), (100, 200));
        // Past the end yields an empty, non-panicking range.
        assert_eq!(page_bounds(9, 100, 250), (250, 250));
        assert_eq!(page_bounds(0, 100, 0), (0, 0));
    }

    #[test]
    fn pager_summary_is_one_based_and_names_its_unit() {
        assert_eq!(
            pager_summary("Rows", 0, 100, 12_300),
            "Rows 1\u{2013}100 of 12,300 \u{00b7} Page 1 of 123"
        );
        assert_eq!(
            pager_summary("Groups", 1, 100, 250),
            "Groups 101\u{2013}200 of 250 \u{00b7} Page 2 of 3"
        );
        // The last page reports the real end, not the page size.
        assert_eq!(
            pager_summary("Rows", 2, 100, 250),
            "Rows 201\u{2013}250 of 250 \u{00b7} Page 3 of 3"
        );
        // An empty result must not read "1–0 of 0".
        assert_eq!(
            pager_summary("Devices", 0, 100, 0),
            "Devices 0\u{2013}0 of 0 \u{00b7} Page 1 of 1"
        );
    }

    #[test]
    fn page_steps_saturate_at_both_ends() {
        assert_eq!(prev_page(0), 0, "Prev on the first page stays put");
        assert_eq!(prev_page(5), 4);
        assert_eq!(next_page(0, 3), 1);
        assert_eq!(next_page(2, 3), 2, "Next on the last page stays put");
        // A stale index past the end still lands on the last page, not beyond it.
        assert_eq!(next_page(99, 3), 2);
        assert_eq!(next_page(0, 1), 0, "a single page has nowhere to go");
        assert_eq!(next_page(0, 0), 0, "no pages must not underflow");
    }

    #[test]
    fn group_thousands_inserts_separators() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(42), "42");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(12_300), "12,300");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn parse_opt_trims_and_rejects_non_numbers() {
        assert_eq!(parse_opt("  42 "), Some(42));
        assert_eq!(parse_opt("-7"), Some(-7));
        assert_eq!(parse_opt(""), None);
        assert_eq!(parse_opt("abc"), None);
    }

    #[test]
    fn non_empty_collapses_blank_to_none() {
        assert_eq!(non_empty("   ".to_string()), None);
        assert_eq!(non_empty(" hi ".to_string()), Some("hi".to_string()));
    }

    #[test]
    fn next_sort_cycles_none_asc_desc_none() {
        let key = RowSortKey::Device;
        let asc = next_sort(None, key);
        assert_eq!(asc, Some(RowSort { key, desc: false }));
        let desc = next_sort(asc, key);
        assert_eq!(desc, Some(RowSort { key, desc: true }));
        assert_eq!(next_sort(desc, key), None);
        // A different key restarts ascending.
        assert_eq!(
            next_sort(desc, RowSortKey::Kb),
            Some(RowSort {
                key: RowSortKey::Kb,
                desc: false
            })
        );
    }

    #[test]
    fn aria_sort_and_glyph_follow_the_active_key() {
        let key = RowSortKey::Severity;
        assert_eq!(aria_sort(None, key), "none");
        let asc = Some(RowSort { key, desc: false });
        assert_eq!(aria_sort(asc, key), "ascending");
        assert_eq!(aria_sort(asc, RowSortKey::Kb), "none");
        assert_eq!(sort_glyph(asc, key), " ▲");
        assert_eq!(sort_glyph(Some(RowSort { key, desc: true }), key), " ▼");
        assert_eq!(sort_glyph(asc, RowSortKey::Kb), "");
    }

    fn sortable(device: &str, sev: &str, installed: Option<&str>) -> PatchRow {
        PatchRow {
            device_id: 1,
            device_name: device.into(),
            organization: "Org".into(),
            location: None,
            device_role: None,
            os_name: None,
            offline: false,
            patch_type: "OS".into(),
            kb: None,
            name: "Patch".into(),
            severity: sev.into(),
            status: "PENDING".into(),
            first_seen_date: None,
            installed_date: installed.map(Into::into),
        }
    }

    fn sel_row(device_id: i64, device: &str, kb: Option<&str>, name: &str, ty: &str) -> PatchRow {
        PatchRow {
            device_id,
            device_name: device.into(),
            organization: "Org".into(),
            location: None,
            device_role: None,
            os_name: None,
            offline: false,
            patch_type: ty.into(),
            kb: kb.map(Into::into),
            name: name.into(),
            severity: "Critical".into(),
            status: "PENDING".into(),
            first_seen_date: None,
            installed_date: None,
        }
    }

    /// The guard chain's *order* is load-bearing and was only reachable by reading
    /// `run_query_inner`: a demo run must be decided before the auth guard (the demo
    /// has no session and would otherwise be told to sign in), and the busy guard
    /// before both (an auto-refresh tick during a manual Run must not start a second).
    #[test]
    fn run_decision_orders_its_guards() {
        use RunDecision::*;

        // Busy wins over everything, including demo.
        assert_eq!(run_decision(true, false, true, true, false), AlreadyRunning);
        assert_eq!(run_decision(false, true, true, true, false), AlreadyRunning);

        // Demo beats the auth and status guards — it needs neither.
        assert_eq!(run_decision(false, false, true, false, true), Demo);

        assert_eq!(run_decision(false, false, false, false, false), NotSignedIn);
        assert_eq!(
            run_decision(false, false, false, true, true),
            NoStatusSelected
        );
        assert_eq!(run_decision(false, false, false, true, false), Run);
    }

    /// Only equality is ever asked of the stamp, so wrapping is safe — and a plain
    /// `+ 1` would panic in debug at the boundary.
    #[test]
    fn query_seq_wraps_and_compares_by_equality() {
        assert_eq!(next_query_seq(0), 1);
        assert_eq!(next_query_seq(u64::MAX), 0, "wraps rather than panicking");

        assert!(!is_superseded(7, 7), "my own run is not superseded");
        assert!(is_superseded(8, 7), "a newer run supersedes mine");
        // Across the wrap the comparison still behaves.
        let mine = u64::MAX;
        assert!(is_superseded(next_query_seq(mine), mine));
    }

    /// The load-bearing half of the selection model. Ticking one row must affect
    /// only that row: an earlier shape swept every KB on the device into the
    /// selection, which made the one path capable of per-patch targeting unable to
    /// receive a subset.
    #[test]
    fn ticking_one_row_does_not_tick_the_devices_other_rows() {
        let mut sel = BTreeMap::new();
        let a = sel_row(1, "web-01", Some("KB1"), "Cumulative Update", "OS");
        let b = sel_row(1, "web-01", Some("KB2"), "Security Update", "OS");

        apply_row_selection(&mut sel, &a, true);

        assert_eq!(sel.len(), 1, "the device entered the selection");
        let device = sel.get(&1).expect("device present");
        assert_eq!(device.patches.len(), 1, "only the ticked row");
        assert!(!device.patches.contains_key(&patch_key(&b)));
    }

    /// A device enters with its first ticked row and leaves with its last, so a
    /// device with nothing ticked is never dispatched against.
    #[test]
    fn a_device_leaves_the_selection_with_its_last_row() {
        let mut sel = BTreeMap::new();
        let a = sel_row(1, "web-01", Some("KB1"), "Cumulative Update", "OS");
        let b = sel_row(1, "web-01", Some("KB2"), "Security Update", "OS");

        apply_row_selection(&mut sel, &a, true);
        apply_row_selection(&mut sel, &b, true);
        assert_eq!(sel[&1].patches.len(), 2);

        apply_row_selection(&mut sel, &a, false);
        assert_eq!(sel[&1].patches.len(), 1, "device stays while a row remains");

        apply_row_selection(&mut sel, &b, false);
        assert!(
            sel.is_empty(),
            "the device leaves with its last row, so it is never dispatched empty"
        );
    }

    /// The two families are targeted differently — OS by KB, software by product
    /// title — so a row has to remember which it is. Keying only by KB made software
    /// rows indistinguishable from OS rows that happen to lack one.
    #[test]
    fn selection_records_the_patch_family_and_drops_an_empty_kb() {
        let mut sel = BTreeMap::new();
        let os = sel_row(1, "web-01", Some("KB1"), "Cumulative Update", "OS");
        let sw = sel_row(1, "web-01", None, "Google Chrome 138", "SOFTWARE");
        let blank_kb = sel_row(2, "web-02", Some(""), "Firefox 130", "SOFTWARE");

        apply_row_selection(&mut sel, &os, true);
        apply_row_selection(&mut sel, &sw, true);
        apply_row_selection(&mut sel, &blank_kb, true);

        let d1 = &sel[&1].patches;
        assert_eq!(d1.len(), 2, "both families coexist on one device");
        assert!(d1[&patch_key(&os)].is_os);
        assert_eq!(d1[&patch_key(&os)].kb.as_deref(), Some("KB1"));
        assert!(!d1[&patch_key(&sw)].is_os);
        assert_eq!(d1[&patch_key(&sw)].kb, None, "software carries no KB");

        assert_eq!(
            sel[&2].patches[&patch_key(&blank_kb)].kb,
            None,
            "an empty KB string is not a KB"
        );
    }

    /// Unticking a row on a device that was never selected must not panic or create
    /// an entry.
    #[test]
    fn unticking_an_unselected_row_is_a_no_op() {
        let mut sel = BTreeMap::new();
        let a = sel_row(9, "ghost", Some("KB1"), "Update", "OS");
        apply_row_selection(&mut sel, &a, false);
        assert!(sel.is_empty());
    }

    #[test]
    fn sev_ordinal_inverts_the_backend_rank_for_every_band() {
        // The backend's `Severity::rank()` is Critical 7, Important 6, Security 5,
        // Moderate 4, Recommended 3, Low 2, Optional 1, Unknown 0. This ordinal must
        // be its exact inverse. Security and Recommended were absent and tied with
        // Unknown below Optional, so a demo sort put ungraded security updates last.
        let ordered = [
            "Critical",
            "Important",
            "Security",
            "Moderate",
            "Recommended",
            "Low",
            "Optional",
            "Unknown",
        ];
        let ordinals: Vec<u8> = ordered.iter().map(|s| sev_ordinal(s)).collect();
        assert_eq!(
            ordinals,
            (0..8).collect::<Vec<u8>>(),
            "every band needs a distinct ordinal in backend rank order"
        );
        assert!(
            sev_ordinal("Security") < sev_ordinal("Optional"),
            "Security must outrank Optional"
        );
        assert!(
            sev_ordinal("Recommended") < sev_ordinal("Unknown"),
            "Recommended must outrank Unknown"
        );
    }

    #[test]
    fn severity_raw_round_trips_every_chart_band() {
        // The severity chart labels bands with display names; the filter holds raw API
        // values. Every band the chart can draw must map to something the filter
        // accepts, or its drill-down silently filters to nothing.
        for (raw, display) in super::super::SEVERITY_OPTIONS {
            assert_eq!(
                severity_raw(display),
                Some(raw),
                "band {display} must map back to {raw}"
            );
        }
        assert_eq!(severity_raw("Nonexistent"), None);
    }

    /// Severity styling lives in three independent CSS rule families plus the
    /// custom-property palette they read, and CSS cannot produce a compile error
    /// for a missing rule. This is the substitute: the stylesheet is compiled in
    /// and every band is checked for all four definitions.
    ///
    /// It exists because each omission has already shipped. `.chart .seg-*` sets
    /// `fill` and is scoped to `.chart`, so it does nothing for a legend `<span>` —
    /// Security and Recommended had bar fills but no `.chart-swatch` rule and
    /// painted transparent holes next to their counts. Separately,
    /// `.sev-optional`/`.sev-unknown` were missing and both collapsed into
    /// `.sev-none`, rendering "low priority" and "unmapped" identically.
    fn inputs(window: &str, after: &str, before: &str) -> FilterInputs {
        FilterInputs {
            detected_window: window.into(),
            detected_after: after.into(),
            detected_before: before.into(),
            ..Default::default()
        }
    }

    /// The three date outputs are mutually exclusive — a relative window and an
    /// explicit range must never both reach the backend, which would narrow the
    /// query twice over.
    #[test]
    fn a_relative_window_sets_days_and_clears_the_explicit_range() {
        for days in ["1", "7", "30", "90"] {
            let f = filter_params(inputs(days, "2026-01-01", "2026-02-01"));
            assert_eq!(f.detected_within_days, days.parse::<i64>().ok());
            assert_eq!(
                (f.detected_after, f.detected_before),
                (None, None),
                "a relative window must not also send the custom dates that are still \
                 sitting in the (hidden) date fields"
            );
        }
    }

    #[test]
    fn a_custom_window_sends_the_range_and_no_day_count() {
        let f = filter_params(inputs("custom", "2026-01-01", "2026-01-31"));
        assert_eq!(f.detected_within_days, None);
        assert_eq!(f.detected_after, Some(1_767_225_600));
        // The whole "before" day is included: midnight + 86_399.
        assert_eq!(f.detected_before, Some(1_769_903_999));
    }

    /// An unrecognized window must clear every date bound rather than fall through
    /// to whatever the custom fields happen to hold — otherwise clearing the
    /// dropdown silently keeps filtering.
    #[test]
    fn an_empty_or_unknown_window_clears_every_date_bound() {
        for window in ["", "nonsense", "365"] {
            let f = filter_params(inputs(window, "2026-01-01", "2026-02-01"));
            assert_eq!(
                (f.detected_within_days, f.detected_after, f.detected_before),
                (None, None, None),
                "window {window:?} must not filter by date at all"
            );
        }
    }

    /// A half-filled custom range is legitimate ("everything since March"), so one
    /// blank bound must not discard the other.
    #[test]
    fn a_custom_window_tolerates_one_open_end() {
        let f = filter_params(inputs("custom", "2026-03-01", ""));
        assert!(f.detected_after.is_some());
        assert_eq!(f.detected_before, None);

        let f = filter_params(inputs("custom", "", "2026-03-01"));
        assert_eq!(f.detected_after, None);
        assert!(f.detected_before.is_some());
    }

    #[test]
    fn identity_and_text_facets_pass_through_with_blanks_dropped() {
        let f = filter_params(FilterInputs {
            organization_id: Some(7),
            location_id: Some(8),
            role_id: Some(9),
            node_classes: vec!["WINDOWS_SERVER".into()],
            severities: vec!["CRITICAL".into()],
            os_name: "  Windows 11  ".into(),
            search: "   ".into(),
            ..Default::default()
        });
        assert_eq!(f.organization_id, Some(7));
        assert_eq!(f.location_id, Some(8));
        assert_eq!(f.role_id, Some(9));
        assert_eq!(f.node_classes, vec!["WINDOWS_SERVER".to_string()]);
        assert_eq!(f.severities, vec!["CRITICAL".to_string()]);
        // Trimmed, and a whitespace-only box is "no filter" rather than a search
        // for spaces that would match nothing.
        assert_eq!(f.os_name_contains.as_deref(), Some("Windows 11"));
        assert_eq!(f.search, None);
    }

    #[test]
    fn dates_round_trip_through_epoch_including_leap_days() {
        for date in [
            "1970-01-01",
            "2000-02-29",
            "2024-02-29",
            "2026-12-31",
            "2100-03-01",
        ] {
            let epoch = date_to_epoch(date).expect("parses");
            assert_eq!(epoch_to_date(Some(epoch)), date, "round trip for {date}");
        }
        assert_eq!(epoch_to_date(None), "");
        assert_eq!(date_to_epoch(""), None);
        assert_eq!(date_to_epoch("2026-13-01"), None);
        assert_eq!(date_to_epoch("2026-01-32"), None);
        assert_eq!(date_to_epoch("2026-01-01-01"), None);
    }

    /// `<input type="number">` treats `min`/`max` as advisory — a user can type or
    /// paste anything — so the clamp is the real guard, and a value that does not
    /// parse must leave the field as it was rather than resetting it to a bound.
    #[test]
    fn settings_numbers_clamp_and_keep_the_old_value_on_garbage() {
        assert_eq!(parse_clamped("8080", 11434u16, 1024, 65535), 8080);
        assert_eq!(
            parse_clamped("80", 11434u16, 1024, 65535),
            1024,
            "below min"
        );
        assert_eq!(parse_clamped("99999", 11434u16, 1024, 65535), 65535);
        assert_eq!(
            parse_clamped("abc", 11434u16, 1024, 65535),
            11434,
            "unparseable input must not silently rewrite the field"
        );
        assert_eq!(parse_clamped("", 30i64, 1, 3650), 30);
        assert_eq!(parse_clamped("  45  ", 30i64, 1, 3650), 45);
        assert_eq!(parse_clamped("0", 8usize, 1, 16), 1);
    }

    /// Clearing the box is how an operator un-sets a remediation script, so blank
    /// must reach `None`; a typo must not, or the action would be offered against
    /// an id that can only 404 at dispatch.
    #[test]
    fn an_optional_script_id_clears_on_blank_and_holds_on_garbage() {
        assert_eq!(parse_optional_id("123", None), Some(123));
        assert_eq!(parse_optional_id("", Some(123)), None);
        assert_eq!(parse_optional_id("   ", Some(123)), None);
        assert_eq!(parse_optional_id("abc", Some(123)), Some(123));
        assert_eq!(parse_optional_id("0", Some(123)), Some(123));
        assert_eq!(parse_optional_id("-4", Some(123)), Some(123));
    }

    /// Precedence, not just presence: telling a signed-out operator to "select a
    /// device" sends them to fix the wrong thing.
    #[test]
    fn the_disabled_reason_names_the_most_fundamental_obstacle_first() {
        let blocked = Some("Sign in to run patch actions.".to_string());
        assert_eq!(
            action_disabled_reason(blocked.clone(), 0, true).as_deref(),
            Some("Sign in to run patch actions."),
            "auth outranks both selection and dispatch state"
        );
        assert_eq!(
            action_disabled_reason(None, 0, true).as_deref(),
            Some("Select at least one device first"),
            "an empty selection outranks an in-flight dispatch"
        );
        assert_eq!(
            action_disabled_reason(None, 3, true).as_deref(),
            Some("An action is already being dispatched")
        );
        assert_eq!(action_disabled_reason(None, 3, false), None);
    }

    #[test]
    fn the_selection_summary_counts_and_mentions_offline_only_when_present() {
        assert_eq!(selection_summary(0, 0, 0), None);
        assert_eq!(
            selection_summary(2, 5, 0).as_deref(),
            Some("2 device(s) selected · 5 patch row(s)"),
            "a zero offline count must not clutter every selection"
        );
        assert_eq!(
            selection_summary(2, 5, 1).as_deref(),
            Some("2 device(s) selected · 5 patch row(s) · 1 offline")
        );
        // An action against an offline device is queued rather than run, so the
        // counts stay legible at fleet scale.
        assert_eq!(
            selection_summary(1200, 34500, 7).as_deref(),
            Some("1,200 device(s) selected · 34,500 patch row(s) · 7 offline")
        );
    }

    #[test]
    fn severity_css_defines_every_band() {
        const CSS: &str = include_str!("../../styles.css");

        for (raw, display) in super::super::SEVERITY_OPTIONS {
            let slug = raw.to_ascii_lowercase();
            for required in [
                format!("--sev-{slug}:"),
                format!("--sev-{slug}-fg:"),
                format!(".sev-{slug} {{"),
                format!(".chart .seg-{slug} {{"),
                format!(".chart-swatch.seg-{slug} {{"),
            ] {
                assert!(
                    CSS.contains(&required),
                    "severity band {display} ({raw}) is missing `{required}` in styles.css"
                );
            }
        }
    }

    /// The palette is the single source of the eight band colors; the three rule
    /// families must read it rather than re-stating hex values, which is what let
    /// the bar fill and the legend swatch drift apart in the first place.
    #[test]
    fn severity_rules_read_the_palette_instead_of_restating_colors() {
        const CSS: &str = include_str!("../../styles.css");

        for (raw, _) in super::super::SEVERITY_OPTIONS {
            let slug = raw.to_ascii_lowercase();
            for (rule, expected) in [
                (
                    format!(".chart .seg-{slug} {{"),
                    format!("var(--sev-{slug})"),
                ),
                (
                    format!(".chart-swatch.seg-{slug} {{"),
                    format!("var(--sev-{slug})"),
                ),
            ] {
                let body = CSS
                    .split_once(&rule)
                    .and_then(|(_, rest)| rest.split_once('}'))
                    .map(|(body, _)| body)
                    .unwrap_or_else(|| panic!("no rule body for `{rule}`"));
                assert!(
                    body.contains(&expected),
                    "`{rule}` should use {expected} rather than its own copy of the color"
                );
            }
        }
    }

    #[test]
    fn sev_class_distinguishes_optional_from_unknown() {
        // Both used to collapse into `sev-none`, rendering "low priority" and
        // "NinjaOne sent a value we couldn't map" identically.
        assert_eq!(sev_class("Optional"), "sev sev-optional");
        assert_eq!(sev_class("Unknown"), "sev sev-unknown");
        assert_eq!(sev_class("Security"), "sev sev-security");
        assert_eq!(sev_class("Recommended"), "sev sev-recommended");
        assert_eq!(sev_class("something-else"), "sev sev-none");
    }

    #[test]
    fn sort_patch_rows_matches_backend_semantics() {
        // Severity ascending surfaces the most urgent first.
        let mut rows = vec![
            sortable("low", "Low", None),
            sortable("crit", "Critical", None),
            sortable("mod", "Moderate", None),
        ];
        sort_patch_rows(
            &mut rows,
            RowSort {
                key: RowSortKey::Severity,
                desc: false,
            },
        );
        let names: Vec<_> = rows.iter().map(|r| r.device_name.as_str()).collect();
        assert_eq!(names, ["crit", "mod", "low"]);

        // Missing dates sort last even on a descending sort.
        let mut rows = vec![
            sortable("a", "Low", Some("2026-01-05")),
            sortable("b", "Low", None),
            sortable("c", "Low", Some("2026-03-01")),
        ];
        sort_patch_rows(
            &mut rows,
            RowSort {
                key: RowSortKey::InstalledDate,
                desc: true,
            },
        );
        let names: Vec<_> = rows.iter().map(|r| r.device_name.as_str()).collect();
        assert_eq!(names, ["c", "a", "b"]);
    }

    #[test]
    fn aged_badge_flags_only_nonzero_backlogs() {
        assert_eq!(aged_badge(0), ("", "0".to_string(), ""));
        assert_eq!(
            aged_badge(3),
            (
                "sev-critical",
                "⚠ 3".to_string(),
                "Past SLA — needs attention"
            )
        );
    }

    #[test]
    fn severity_and_status_classes_map_known_and_unknown_values() {
        assert_eq!(sev_class("Critical"), "sev sev-critical");
        assert_eq!(sev_class("nonsense"), "sev sev-none");
        assert_eq!(status_class("PENDING"), "stat stat-pending");
        assert_eq!(status_class("FAILED"), "stat stat-failed");
        assert_eq!(status_class("???"), "stat");
    }

    #[test]
    fn tab_class_marks_only_the_active_tab() {
        assert_eq!(tab_class(Tab::Patches, Tab::Patches), "tab tab-on");
        assert_eq!(tab_class(Tab::Patches, Tab::Reboot), "tab");
    }

    #[test]
    fn summary_line_is_tab_aware() {
        let c = SummaryCounts {
            rows_total: 12_300,
            devices_total: 540,
            failures: 7,
            orgs: 3,
            reboot: 12,
        };
        assert_eq!(
            summary_line(Tab::Patches, &c, "2026-06-28"),
            "12,300 patch rows across 540 devices \u{00b7} generated 2026-06-28"
        );
        assert_eq!(
            summary_line(Tab::Failures, &c, "2026-06-28"),
            "7 failing patches across 540 devices \u{00b7} generated 2026-06-28"
        );
        assert_eq!(
            summary_line(Tab::Compliance, &c, "2026-06-28"),
            "3 organizations \u{00b7} 540 devices \u{00b7} generated 2026-06-28"
        );
        // Jobs shows dispatch history, not query output, so the query's counts and
        // generation time would be actively misleading there.
        assert_eq!(
            summary_line(Tab::Jobs, &c, "2026-06-28"),
            "Dispatched actions"
        );
        assert_eq!(
            summary_line(Tab::Reboot, &c, "2026-06-28"),
            "12 of 540 devices need reboot \u{00b7} generated 2026-06-28"
        );
    }

    #[test]
    fn is_fleet_tab_flags_compliance_and_reboot() {
        assert!(is_fleet_tab(Tab::Compliance));
        assert!(is_fleet_tab(Tab::Reboot));
        assert!(!is_fleet_tab(Tab::Patches));
        assert!(!is_fleet_tab(Tab::Failures));
        // Jobs is neither tier — it doesn't reflect the query at all.
        assert!(!is_fleet_tab(Tab::Jobs));
    }

    fn job(kind: ActionKind, dry_run: bool) -> JobReport {
        JobReport {
            id: 1,
            device_name: "srv-1".into(),
            organization: "Contoso".into(),
            kind,
            detail: "Apply OS patches".into(),
            dry_run,
            state: JobState::Running,
            dispatched_at: "2026-06-28 10:00:00 UTC".into(),
            duration_seconds: None,
            activity_id: None,
            series_uid: None,
            exit_code: None,
        }
    }

    #[test]
    fn job_mode_label_distinguishes_dispatch_kinds() {
        assert_eq!(job_mode_label(&job(ActionKind::OsPatchScan, false)), "Live");
        assert_eq!(
            job_mode_label(&job(ActionKind::Reboot, false)),
            "Live + reboot"
        );
        assert_eq!(
            job_mode_label(&job(ActionKind::OsPatchApply, false)),
            "Live + reboot"
        );
        // A preview applies — and reboots — nothing, so it wins over the reboot
        // indication rather than reading "Live + reboot" for a run that does neither.
        assert_eq!(job_mode_label(&job(ActionKind::Reboot, true)), "Dry run");
    }

    #[test]
    fn format_duration_is_blank_while_running() {
        assert_eq!(format_duration(None), "");
        assert_eq!(format_duration(Some(0)), "0s");
        assert_eq!(format_duration(Some(45)), "45s");
        assert_eq!(format_duration(Some(60)), "1m 0s");
        assert_eq!(format_duration(Some(3_725)), "62m 5s");
    }

    #[test]
    fn correlator_names_where_to_find_the_run_in_ninjaone() {
        // With no script-output endpoint in v2, this tooltip is how an operator
        // gets from a job row to the run in the NinjaOne console.
        let mut j = job(ActionKind::Script, false);
        assert_eq!(j.correlator(), "");

        j.series_uid = Some("uid-9".into());
        assert_eq!(j.correlator(), "NinjaOne job uid-9");

        // A numeric activity id is more precise, so it wins.
        j.activity_id = Some(4242);
        assert_eq!(j.correlator(), "NinjaOne activity 4242");
    }

    #[test]
    fn detected_label_humanizes_each_window() {
        assert_eq!(
            detected_label("1", "", ""),
            Some("last 24 hours".to_string())
        );
        assert_eq!(detected_label("7", "", ""), Some("last 7 days".to_string()));
        assert_eq!(
            detected_label("30", "", ""),
            Some("last 30 days".to_string())
        );
        assert_eq!(
            detected_label("90", "", ""),
            Some("last 90 days".to_string())
        );
        assert_eq!(detected_label("", "", ""), None);
        assert_eq!(
            detected_label("custom", "2026-01-01", "2026-02-01"),
            Some("2026-01-01 \u{2192} 2026-02-01".to_string())
        );
        assert_eq!(
            detected_label("custom", "2026-01-01", ""),
            Some("after 2026-01-01".to_string())
        );
        assert_eq!(
            detected_label("custom", "", "2026-02-01"),
            Some("before 2026-02-01".to_string())
        );
        assert_eq!(detected_label("custom", "", ""), None);
    }

    #[test]
    fn parse_inline_splits_bold_runs() {
        assert_eq!(parse_inline("plain"), vec![MdSpan::Text("plain".into())]);
        assert_eq!(
            parse_inline("**Lead.** then text"),
            vec![
                MdSpan::Strong("Lead.".into()),
                MdSpan::Text(" then text".into()),
            ]
        );
        assert_eq!(
            parse_inline("a **b** c **d**"),
            vec![
                MdSpan::Text("a ".into()),
                MdSpan::Strong("b".into()),
                MdSpan::Text(" c ".into()),
                MdSpan::Strong("d".into()),
            ]
        );
        // An unterminated marker stays literal so no content is dropped.
        assert_eq!(
            parse_inline("trailing **oops"),
            vec![MdSpan::Text("trailing **oops".into())]
        );
    }

    #[test]
    fn parse_changelog_handles_headings_lists_and_wrapped_bullets() {
        let src = "### Added\n\n- **Compliance by OS.** A per-OS\n  bar chart and table.\n- Second item.\n\n### Fixed\n\n- A fix.";
        assert_eq!(
            parse_changelog(src),
            vec![
                MdBlock::Heading("Added".into()),
                MdBlock::List(vec![
                    vec![
                        MdSpan::Strong("Compliance by OS.".into()),
                        // The wrapped continuation line folds into the bullet.
                        MdSpan::Text(" A per-OS bar chart and table.".into()),
                    ],
                    vec![MdSpan::Text("Second item.".into())],
                ]),
                MdBlock::Heading("Fixed".into()),
                MdBlock::List(vec![vec![MdSpan::Text("A fix.".into())]]),
            ]
        );
    }

    #[test]
    fn parse_changelog_treats_plain_text_as_a_paragraph() {
        // The GitHub fallback note has no markdown markers.
        assert_eq!(
            parse_changelog("See the release notes on GitHub for what's new in v1.2.3."),
            vec![MdBlock::Paragraph(vec![MdSpan::Text(
                "See the release notes on GitHub for what's new in v1.2.3.".into()
            )])]
        );
        assert!(parse_changelog("   \n\n  ").is_empty());
    }

    #[test]
    fn filter_chips_emits_only_non_default_facets() {
        // A default snapshot (no facets, ALL/empty) yields no chips.
        assert!(filter_chips(&AppliedFilters::default()).is_empty());

        let scope_only = AppliedFilters {
            organization: Some("Acme".to_string()),
            patch_type: "ALL".to_string(),
            ..Default::default()
        };
        let chips = filter_chips(&scope_only);
        assert_eq!(chips.len(), 1);
        assert_eq!(chips[0].label, "Org: Acme");
        assert!(!chips[0].patch);

        let full = AppliedFilters {
            organization: Some("Acme".to_string()),
            location: Some("HQ".to_string()),
            role: Some("Server".to_string()),
            os_types: vec!["Windows Server".to_string()],
            os_name: Some("2022".to_string()),
            patch_type: "OS".to_string(),
            statuses: vec!["INSTALLED".to_string()],
            severities: vec!["Critical".to_string()],
            search: Some("KB5040434".to_string()),
            detected_window: "7".to_string(),
            detected_after: String::new(),
            detected_before: String::new(),
            install_days: Some(30),
        };
        let chips = filter_chips(&full);
        let labels: Vec<&str> = chips.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Org: Acme",
                "Location: HQ",
                "Role: Server",
                "OS Type: Windows Server",
                "OS name: 2022",
                "Type: OS",
                "Status: INSTALLED",
                "Severity: Critical",
                "Search: KB5040434",
                "First seen: last 7 days",
                "Install history: last 30d",
            ]
        );
        // The first five facets are device-scope; the rest are patch-tier.
        assert!(chips.iter().take(5).all(|c| !c.patch));
        assert!(chips.iter().skip(5).all(|c| c.patch));
    }

    /// Builds an `AuthStatus` in the state the named step leaves it in.
    fn auth(authenticated: bool, actions_enabled: bool, write_enabled: bool) -> AuthStatus {
        AuthStatus {
            authenticated,
            instance_base_url: "https://app.ninjarmm.com".into(),
            actions_enabled,
            write_enabled,
            scope_known: true,
        }
    }

    #[test]
    fn blocked_reason_clears_as_the_operator_completes_each_step() {
        // The reported bug, walked step by step. Each of these used to be computed
        // once at startup and cached, so the verdict from step 1 stayed on screen
        // through steps 2 and 3.
        let signed_out = action_blocked_reason(false, false, Some(&auth(false, false, false)));
        assert_eq!(signed_out.as_deref(), Some("Sign in to run patch actions."));

        // 2. Signed in, actions still off in Settings — the message must move on
        // from "sign in", which is now done.
        let signed_in = action_blocked_reason(false, false, Some(&auth(true, false, false)));
        assert_eq!(
            signed_in.as_deref(),
            Some("Patch actions are disabled — enable them in Settings.")
        );

        // 3. Actions enabled but the grant predates them (read-only refresh token).
        let read_only = action_blocked_reason(false, false, Some(&auth(true, true, false)));
        assert!(read_only.is_some_and(|r| r.contains("read-only")));

        // 4. Fully authorized — nothing blocks.
        assert!(action_blocked_reason(false, false, Some(&auth(true, true, true))).is_none());
    }

    #[test]
    fn blocked_reason_distinguishes_unknown_scope_from_denied_scope() {
        // scope_known == false means "we couldn't read the grant", not "denied" —
        // telling the operator their consent was wrong would be a lie.
        let mut unknown = auth(true, true, false);
        unknown.scope_known = false;
        let reason = action_blocked_reason(false, false, Some(&unknown)).expect("blocked");
        assert!(reason.contains("Couldn't confirm"), "{reason}");
        assert!(!reason.contains("read-only"), "{reason}");
    }

    #[test]
    fn blocked_reason_blocks_the_browser_demo_and_the_unknown_startup_state() {
        // web/demo wins over everything, including a fully authorized status.
        let authed = auth(true, true, true);
        for (web, demo) in [(true, false), (false, true)] {
            assert_eq!(
                action_blocked_reason(web, demo, Some(&authed)).as_deref(),
                Some("Patch actions run only in the desktop app.")
            );
        }
        // Before the first auth_status reply we don't know yet, so fail closed
        // rather than briefly offering actions the backend would reject.
        assert_eq!(
            action_blocked_reason(false, false, None).as_deref(),
            Some("Sign in to run patch actions.")
        );
    }

    #[test]
    fn patch_key_identifies_a_patch_not_a_device() {
        // The reported bug: ticking one software patch marked every patch on that
        // device. Two different patches on the same device must key differently...
        let mut a = sortable("web-01", "Critical", None);
        a.device_id = 1;
        a.kb = Some("KB5040434".into());
        a.name = "Cumulative Update".into();
        let mut b = a.clone();
        b.kb = None;
        b.patch_type = "SOFTWARE".into();
        b.name = "Google Chrome 138".into();
        assert_ne!(patch_key(&a), patch_key(&b));

        // ...while the same patch on a different device keys the same, so the
        // by-patch grouped view and the flat view agree on identity.
        let mut same_patch_other_device = a.clone();
        same_patch_other_device.device_id = 2;
        same_patch_other_device.device_name = "web-02".into();
        assert_eq!(patch_key(&a), patch_key(&same_patch_other_device));
    }

    #[test]
    fn patch_key_separator_cannot_be_forged_by_a_name() {
        // A KB-less patch whose name embeds the separator must not collide with a
        // genuine KB'd patch of the same type.
        let mut real = sortable("web-01", "Critical", None);
        real.patch_type = "OS".into();
        real.kb = Some("KB1".into());
        real.name = "Update".into();
        let mut forged = real.clone();
        forged.kb = None;
        forged.name = "KB1\u{1f}Update".into();
        assert_ne!(patch_key(&real), patch_key(&forged));
    }

    use super::super::state::SelectedPatch;

    fn selection(patches: Vec<SelectedPatch>) -> DeviceSelection {
        DeviceSelection {
            name: "srv".into(),
            organization: "Org".into(),
            offline: false,
            patches: patches
                .into_iter()
                .enumerate()
                .map(|(i, p)| (format!("k{i}"), p))
                .collect(),
        }
    }

    fn os(kb: &str) -> SelectedPatch {
        SelectedPatch {
            kb: Some(kb.into()),
            name: "Cumulative Update".into(),
            is_os: true,
        }
    }

    fn sw(name: &str) -> SelectedPatch {
        SelectedPatch {
            kb: None,
            name: name.into(),
            is_os: false,
        }
    }

    /// The whole point of the remediation kinds: each device is told to install the
    /// patches ticked *on it*, and only from its own family.
    #[test]
    fn remediation_targets_are_per_device_and_per_family() {
        let selected = BTreeMap::from([
            (1, selection(vec![os("KB5040434"), os("KB5041580")])),
            (2, selection(vec![os("KB5041580"), sw("Google Chrome")])),
            (3, selection(vec![sw("7-Zip")])),
        ]);

        let os_targets = remediation_targets(&selected, ActionKind::OsPatchRemediate);
        assert_eq!(
            os_targets,
            BTreeMap::from([
                (1, vec!["KB5040434".to_string(), "KB5041580".into()]),
                (2, vec!["KB5041580".to_string()]),
            ]),
            "device 3 has no OS rows ticked, so it must not be dispatched to at all"
        );

        let sw_targets = remediation_targets(&selected, ActionKind::SoftwarePatchRemediate);
        assert_eq!(
            sw_targets,
            BTreeMap::from([
                (2, vec!["Google Chrome".to_string()]),
                (3, vec!["7-Zip".to_string()]),
            ]),
            "software is targeted by product title, since it carries no KB"
        );

        // A third-party row ticked under an OS remediation contributes nothing —
        // it has no KB, so it cannot be named in a kbAllowList.
        let only_software = BTreeMap::from([(1, selection(vec![sw("Google Chrome")]))]);
        assert!(remediation_targets(&only_software, ActionKind::OsPatchRemediate).is_empty());

        // ...and the native kinds have no target list at all.
        assert!(remediation_targets(&selected, ActionKind::OsPatchApply).is_empty());

        // The hand-picked script's "Target only the selected KBs" goes through the
        // same function, so it cannot drift back to a batch-wide union.
        assert_eq!(targets_by_device(&selected, true), os_targets);
    }

    /// An OS row with no KB (rare, but the feed allows it) cannot be targeted, and
    /// must not become an empty entry that dispatches an install of nothing.
    #[test]
    fn remediation_targets_drop_untargetable_rows() {
        let blank = SelectedPatch {
            kb: Some("   ".into()),
            name: "Unnamed".into(),
            is_os: true,
        };
        let missing = SelectedPatch {
            kb: None,
            name: "Unnamed".into(),
            is_os: true,
        };
        let selected = BTreeMap::from([
            (1, selection(vec![blank, missing])),
            (2, selection(vec![os("KB1")])),
        ]);
        assert_eq!(
            remediation_targets(&selected, ActionKind::OsPatchRemediate),
            BTreeMap::from([(2, vec!["KB1".to_string()])])
        );
    }

    #[test]
    fn remediation_summary_counts_distinct_patches_not_dispatches() {
        // One KB ticked on three devices is one patch, not three.
        let targets = BTreeMap::from([
            (1, vec!["KB1".to_string()]),
            (2, vec!["KB1".to_string()]),
            (3, vec!["KB1".to_string(), "KB2".into()]),
        ]);
        assert_eq!(
            remediation_summary(ActionKind::OsPatchRemediate, &targets).as_deref(),
            Some("OS: 2 patch(es) on 3 device(s)")
        );
        assert_eq!(
            remediation_summary(ActionKind::SoftwarePatchRemediate, &BTreeMap::new()),
            None,
            "a family with nothing ticked renders no chip"
        );
    }

    #[test]
    fn kb_targeting_summary_says_per_device() {
        let targets = BTreeMap::from([
            (1, vec!["KB1".to_string(), "KB2".into()]),
            (2, vec!["KB2".to_string()]),
        ]);
        let s = kb_targeting_summary(&targets);
        assert!(s.contains("only its own KBs"), "{s}");
        assert!(s.contains("2 distinct KB(s) across 2 device(s)"), "{s}");

        assert!(kb_targeting_summary(&BTreeMap::new()).contains("empty allow list"));
    }

    #[test]
    fn kind_disabled_reason_distinguishes_no_script_from_no_selection() {
        // Unconfigured wins: it is the one the operator must fix first, and it is
        // true regardless of what they tick.
        let why = kind_disabled_reason(ActionKind::OsPatchRemediate, false, 3).unwrap();
        assert!(why.contains("No OS remediation script configured"), "{why}");
        assert!(why.contains("Settings → Patch actions"), "{why}");

        let why = kind_disabled_reason(ActionKind::SoftwarePatchRemediate, true, 0).unwrap();
        assert!(why.contains("No software patches selected"), "{why}");

        assert_eq!(
            kind_disabled_reason(ActionKind::OsPatchRemediate, true, 1),
            None
        );
        // The native kinds are never gated on either.
        assert_eq!(
            kind_disabled_reason(ActionKind::OsPatchApply, false, 0),
            None
        );
        assert_eq!(kind_disabled_reason(ActionKind::Reboot, false, 0), None);
    }
}
