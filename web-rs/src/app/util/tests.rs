use std::collections::BTreeMap;

use super::super::state::{DeviceSelection, SelectedPatch};
use super::super::{AppliedFilters, Tab};
use super::*;
use crate::types::{
    ActionKind, AuthStatus, JobReport, JobState, Location, Organization, PatchFamilies, PatchRow,
    RebootChoice, RebootMode, RowSort, RowSortKey, RunRecord,
};

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
fn the_paged_total_follows_the_view_not_the_row_count() {
    // Grouping only ever collapses rows, so `groups_total <= rows_total` and the
    // row-derived bound is always the looser of the two. Clamping a grouped page
    // against it is what let a silent refresh ask for a group page past the end.
    assert_eq!(paged_total(false, 40_000, 400), 40_000, "flat pages rows");
    assert_eq!(paged_total(true, 40_000, 400), 400, "grouped pages headers");

    // The bug this pins: 40,000 rows is 400 pages, 400 groups is 4. A stored index
    // of 7 is in range for the row bound and three pages past the end for the group
    // bound — the grouped table renders nothing while the pager reads "Page 8 of 400".
    let stale = 7;
    assert_eq!(
        clamp_page(stale, page_count(paged_total(true, 40_000, 400), 100)),
        3,
        "the grouped bound pulls the stale index back to the last real page"
    );
    assert_eq!(
        clamp_page(stale, page_count(paged_total(false, 40_000, 400), 100)),
        stale,
        "the same index is legitimately in range for the flat view"
    );

    // An empty result of either shape still has one page, index 0.
    assert_eq!(paged_total(true, 0, 0), 0);
    assert_eq!(clamp_page(9, page_count(paged_total(true, 0, 0), 100)), 0);
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

/// `days_from_civil` normalises rather than fails, so 2026-02-31 silently became
/// a day in March and reached the query as a bound the operator never chose.
#[test]
fn impossible_civil_dates_are_rejected() {
    assert_eq!(date_to_epoch("2026-02-31"), None, "February has no 31st");
    assert_eq!(date_to_epoch("2026-04-31"), None, "April has no 31st");
    assert_eq!(date_to_epoch("2026-02-29"), None, "2026 is not a leap year");
    assert_eq!(date_to_epoch("2026-13-01"), None, "no thirteenth month");
    assert_eq!(date_to_epoch("2026-00-10"), None, "no zeroth month");

    // Real dates still parse, including the leap-year cases the rule must not
    // over-reject.
    assert!(date_to_epoch("2026-02-28").is_some());
    assert!(date_to_epoch("2024-02-29").is_some(), "2024 is a leap year");
    assert!(
        date_to_epoch("2000-02-29").is_some(),
        "400-divisible is leap"
    );
    assert_eq!(date_to_epoch("1900-02-29"), None, "100-divisible is not");
    assert!(date_to_epoch("2026-12-31").is_some());
}

/// A date that parses must round-trip, or a bound the operator set comes back as
/// a different day in the control.
#[test]
fn valid_dates_round_trip_through_the_epoch() {
    for d in ["2026-01-01", "2024-02-29", "2026-06-15", "2026-12-31"] {
        assert_eq!(epoch_to_date(date_to_epoch(d)), d, "round trip for {d}");
    }
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
        organization_ids: vec![7, 11],
        location_ids: vec![8],
        role_ids: vec![9],
        node_classes: vec!["WINDOWS_SERVER".into()],
        severities: vec!["CRITICAL".into()],
        os_name: "  Windows 11  ".into(),
        search: "   ".into(),
        ..Default::default()
    });
    assert_eq!(f.organization_ids, vec![7, 11]);
    assert_eq!(f.location_ids, vec![8]);
    assert_eq!(f.role_ids, vec![9]);
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
    const CSS: &str = include_str!("../../../styles.css");

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
    const CSS: &str = include_str!("../../../styles.css");

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
        failing_devices: 5,
        orgs: 3,
        reboot: 12,
    };
    assert_eq!(
        summary_line(Tab::Patches, &c, "2026-06-28"),
        "12,300 patch rows across 540 devices \u{00b7} generated 2026-06-28"
    );
    assert_eq!(
        summary_line(Tab::Failures, &c, "2026-06-28"),
        // Against the devices that actually failed, not the fleet.
        "7 failing patches on 5 devices \u{00b7} generated 2026-06-28"
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
fn a_sparkline_maps_values_into_a_unit_box_with_y_inverted() {
    // Two points, ascending: first at the bottom (y=1), last at the top (y=0).
    let p = sparkline_points(&[0.0, 10.0]);
    assert_eq!(p, vec![(0.0, 1.0), (1.0, 0.0)]);

    // A flat series has no range to divide by; draw it down the middle rather than
    // producing NaN and an empty path.
    let flat = sparkline_points(&[5.0, 5.0, 5.0]);
    assert_eq!(flat, vec![(0.0, 0.5), (0.5, 0.5), (1.0, 0.5)]);

    assert!(sparkline_points(&[]).is_empty());
    assert_eq!(sparkline_points(&[7.0]), vec![(0.5, 0.5)]);
}

/// History accumulates every completed query, and they did not all measure the same
/// thing. Charting an OS-only run beside an ALL run would read as the backlog
/// halving overnight when all that changed was the Type chip.
#[test]
fn a_trend_line_only_carries_runs_that_measured_the_same_thing() {
    let base = RunRecord {
        instance: "https://app.ninjarmm.com".into(),
        os_patches: true,
        software_patches: true,
        scoped: false,
        rows_total: 100,
        ..RunRecord::default()
    };
    let os_only = RunRecord {
        software_patches: false,
        rows_total: 40,
        ..base.clone()
    };
    let filtered = RunRecord {
        scoped: true,
        rows_total: 10,
        ..base.clone()
    };
    let other_tenant = RunRecord {
        instance: "https://eu.ninjarmm.com".into(),
        ..base.clone()
    };
    let newest = RunRecord {
        rows_total: 90,
        ..base.clone()
    };

    // The newest record defines the series.
    let history = vec![
        os_only,
        other_tenant,
        base.clone(),
        filtered,
        newest.clone(),
    ];
    let series = trend_series(&history, 60);
    assert_eq!(
        series.len(),
        2,
        "only the two comparable whole-fleet ALL runs"
    );
    assert_eq!(series[0].rows_total, 100);
    assert_eq!(series[1].rows_total, 90, "newest last");

    assert!(trend_series(&[], 60).is_empty());
}

/// The limit keeps the most recent runs, not the first ones.
#[test]
fn a_long_history_keeps_its_newest_runs() {
    let runs: Vec<RunRecord> = (0..10)
        .map(|i| RunRecord {
            rows_total: i,
            ..RunRecord::default()
        })
        .collect();
    let series = trend_series(&runs, 3);
    assert_eq!(
        series.iter().map(|r| r.rows_total).collect::<Vec<_>>(),
        vec![7, 8, 9]
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
    // Trend reads the run-history file, which spans many queries — the current
    // query's filters describe none of it.
    assert!(!is_fleet_tab(Tab::Trend));
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

fn org(id: i64, name: &str) -> Organization {
    Organization {
        id,
        name: name.to_string(),
    }
}

fn loc(id: i64, name: &str, org_id: i64) -> Location {
    Location {
        id,
        name: name.to_string(),
        organization_id: Some(org_id),
    }
}

/// Selected ids resolve in the lookup's order, and an id with no lookup row is
/// dropped rather than rendered as a bare number.
#[test]
fn names_for_resolves_in_lookup_order_and_names_unknown_ids() {
    let orgs = vec![org(1, "Acme"), org(2, "Globex"), org(3, "Initech")];
    // An id the lookup cannot resolve is still in the query, so it is still on
    // the chip — dropping it read as "whole fleet" beside a result it narrowed
    // to nothing.
    assert_eq!(
        names_for(&[3, 1, 99], orgs.clone().into_iter()),
        vec![
            "Acme".to_string(),
            "Initech".to_string(),
            "#99 (not found)".to_string()
        ]
    );
    assert!(names_for(&[], orgs.into_iter()).is_empty());
}

#[test]
fn the_picker_search_matches_case_insensitively() {
    let orgs = vec![org(1, "Acme Corp"), org(2, "Globex"), org(3, "acme labs")];
    let hits: Vec<String> = matching_options(&orgs, " ACME ")
        .into_iter()
        .map(|o| o.name)
        .collect();
    assert_eq!(hits, vec!["Acme Corp".to_string(), "acme labs".to_string()]);
    // An empty needle is "no filter", not "match nothing".
    assert_eq!(matching_options(&orgs, "").len(), 3);
    assert!(matching_options(&orgs, "zzz").is_empty());
}

/// The collapsed summary names a short selection and counts a long one.
#[test]
fn the_selection_summary_names_a_short_selection_and_counts_a_long_one() {
    assert_eq!(
        selection_label(&[], "All organizations"),
        "All organizations"
    );
    assert_eq!(selection_label(&["Acme".into()], "All"), "Acme");
    assert_eq!(
        selection_label(&["Acme".into(), "Globex".into()], "All"),
        "Acme, Globex"
    );
    assert_eq!(
        selection_label(&["Acme".into(), "Globex".into(), "Initech".into()], "All"),
        "3 selected"
    );
}

/// Only genuinely colliding location names get qualified — an MSP's three "HQ"
/// entries are unpickable otherwise, but a unique name must stay as it is.
#[test]
fn only_ambiguous_location_names_are_qualified_by_organization() {
    let orgs = vec![org(1, "Acme"), org(2, "Globex")];
    let locations = vec![loc(10, "HQ", 1), loc(20, "HQ", 2), loc(30, "Depot", 1)];
    let names: Vec<String> = disambiguate_locations(&locations, &orgs)
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(
        names,
        vec![
            "HQ \u{00b7} Acme".to_string(),
            "HQ \u{00b7} Globex".to_string(),
            "Depot".to_string(),
        ]
    );
    // Ids survive the rewrite — they are what the selection is keyed on.
    assert_eq!(
        disambiguate_locations(&locations, &orgs)
            .iter()
            .map(|l| l.id)
            .collect::<Vec<_>>(),
        vec![10, 20, 30]
    );
}

/// A compliance report must never print "100%" for a fleet that isn't clean.
/// Mirrors the backend assertion on `rows::format_pct`.
#[test]
fn a_compliance_percentage_never_rounds_up_to_a_hundred() {
    assert_eq!(format_pct(100.0), "100%");
    assert_eq!(format_pct(99.9), "99%");
    assert_eq!(format_pct(99.5), "99%");
    assert_eq!(format_pct(94.6), "95%");
    assert_eq!(format_pct(0.0), "0%");
}

/// The scope sentence must name both things a bare percentage hides. Mirrors the
/// backend's `rows::compliance_scope_note`, which the workbook and report print.
#[test]
fn the_scope_note_states_the_population_and_the_families() {
    let both = PatchFamilies {
        os: true,
        software: true,
    };
    assert_eq!(
        compliance_scope_note(0, 0, both),
        "Compliance covers online Windows, macOS and Linux devices only."
    );
    assert_eq!(
        compliance_scope_note(1, 0, both),
        "Compliance covers online Windows, macOS and Linux devices only \
         (1 offline device excluded)."
    );
    assert_eq!(
        compliance_scope_note(0, 1, both),
        "Compliance covers online Windows, macOS and Linux devices only \
         (1 non-patchable device excluded)."
    );
    assert_eq!(
        compliance_scope_note(3, 12, both),
        "Compliance covers online Windows, macOS and Linux devices only \
         (3 offline and 12 non-patchable devices excluded)."
    );
    assert_eq!(
        compliance_scope_note(
            12,
            0,
            PatchFamilies {
                os: true,
                software: false
            }
        ),
        "Compliance covers online Windows, macOS and Linux devices only \
         (12 offline devices excluded), and counts OS patches only."
    );
    // A missing `patchFamilies` on the wire defaults to neither family, which
    // must read as an incomplete scope rather than a silent whole-backlog claim.
    assert!(compliance_scope_note(0, 0, PatchFamilies::default()).contains("no patch families"));
}

#[test]
fn filter_chips_emits_only_non_default_facets() {
    // A default snapshot (no facets, ALL/empty) yields no chips.
    assert!(filter_chips(&AppliedFilters::default()).is_empty());

    let scope_only = AppliedFilters {
        organizations: vec!["Acme".to_string()],
        patch_type: "ALL".to_string(),
        ..Default::default()
    };
    let chips = filter_chips(&scope_only);
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].label, "Org: Acme");
    assert!(!chips[0].patch);

    let full = AppliedFilters {
        organizations: vec!["Acme".to_string(), "Globex".to_string()],
        locations: vec!["HQ".to_string()],
        roles: vec!["Server".to_string()],
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
            // One chip per facet, listing every selection in it.
            "Org: Acme, Globex",
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
    // The first six facets reach every tab (Type included: only the families
    // fetched are in the fleet rollups); the rest narrow the rows only.
    assert!(chips.iter().take(6).all(|c| !c.patch));
    assert!(chips.iter().skip(6).all(|c| c.patch));
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
/// A remediation dispatches only to devices with a ticked patch **of its own
/// family**. Handing a device with only software rows ticked an empty OS allow
/// list produces a job that reports success having installed nothing — and the
/// operator sees a green row.
#[test]
fn a_remediation_skips_devices_with_nothing_ticked_of_its_family() {
    let mut selected = BTreeMap::new();
    selected.insert(1, selection(vec![os("KB5040434")]));
    selected.insert(2, selection(vec![sw("Google Chrome")]));

    let req = build_action_request(
        ActionKind::OsPatchRemediate,
        &selected,
        &RunOptions::default(),
    );

    assert_eq!(
        req.device_ids,
        vec![1],
        "device 2 has no OS patch ticked, so it must not be dispatched to at all"
    );
    assert_eq!(req.device_targets.len(), 1);
    assert!(!req.device_targets.contains_key(&2));

    // ...and the software remediation is the mirror image.
    let req = build_action_request(
        ActionKind::SoftwarePatchRemediate,
        &selected,
        &RunOptions::default(),
    );
    assert_eq!(req.device_ids, vec![2]);
}

/// A hand-picked script keeps every selected device: the operator chose them, and
/// the script may do something useful with no target list. `plan()` warns about
/// the ones that get an empty one rather than silently dropping them.
#[test]
fn a_hand_picked_script_keeps_every_selected_device() {
    let mut selected = BTreeMap::new();
    selected.insert(1, selection(vec![os("KB5040434")]));
    selected.insert(2, selection(vec![sw("Google Chrome")]));

    let req = build_action_request(ActionKind::Script, &selected, &RunOptions::default());
    assert_eq!(req.device_ids, vec![1, 2]);
    assert!(
        req.device_targets.is_empty(),
        "no allow list unless KB targeting is on"
    );

    let opts = RunOptions {
        use_kb_targeting: true,
        ..RunOptions::default()
    };
    let req = build_action_request(ActionKind::Script, &selected, &opts);
    assert_eq!(req.device_ids, vec![1, 2]);
    assert!(req.device_targets.contains_key(&1));
}

/// The three shared run options reach every `runs_a_script()` kind and no other.
/// The native endpoints take no parameters, have no preview mode and run as
/// NinjaOne's agent, so setting them there would claim protection they cannot
/// give — which is exactly what the labelled options rows in the ActionBar say.
#[test]
fn shared_run_options_reach_script_kinds_only() {
    let mut selected = BTreeMap::new();
    selected.insert(1, selection(vec![os("KB5040434")]));
    let opts = RunOptions {
        dry_run: true,
        run_as: "SYSTEM".into(),
        script_reboot: RebootChoice::Auto,
        ..RunOptions::default()
    };

    for kind in [ActionKind::Script, ActionKind::OsPatchRemediate] {
        let req = build_action_request(kind, &selected, &opts);
        assert!(req.dry_run, "{kind:?} runs a script, so dry run applies");
        assert_eq!(req.run_as.as_deref(), Some("SYSTEM"), "{kind:?}");
        assert_eq!(req.reboot, RebootChoice::Auto, "{kind:?}");
    }

    for kind in [
        ActionKind::OsPatchApply,
        ActionKind::OsPatchScan,
        ActionKind::Reboot,
    ] {
        let req = build_action_request(kind, &selected, &opts);
        assert!(!req.dry_run, "{kind:?} has no preview mode");
        assert_eq!(req.run_as, None, "{kind:?} runs as NinjaOne's agent");
    }
}

/// Every kind must describe its own reach, and the two that install more than the
/// operator ticked must say so. This is the affordance that replaced an 11px muted
/// group heading as the only thing distinguishing "installs the three KBs you
/// ticked" from "installs this device's entire approved backlog".
#[test]
fn every_action_states_its_blast_radius_and_the_wide_ones_say_so() {
    for kind in ActionKind::ALL {
        let radius = kind.blast_radius();
        assert!(!radius.is_empty(), "{kind:?} has no blast radius sentence");
        assert!(
            radius.ends_with('.'),
            "{kind:?} blast radius reads as a sentence in the confirm dialog"
        );
        if kind.exceeds_selection() {
            assert!(
                radius.contains("EVERY"),
                "{kind:?} reaches past the selection, so its sentence must say so \
                 in a way that survives being skimmed: {radius}"
            );
        }
        if !kind.is_mutating() {
            assert!(
                radius.contains("nothing"),
                "{kind:?} changes nothing and should say so: {radius}"
            );
        }
    }
    // Both native applies, and only those.
    let wide: Vec<_> = ActionKind::ALL
        .into_iter()
        .filter(|k| k.exceeds_selection())
        .collect();
    assert_eq!(
        wide.len(),
        2,
        "only the two native applies exceed the selection"
    );
}

/// A blank Run-as must not become `Some("")` — the backend hashes it into the
/// confirm token and would send an empty execution identity.
#[test]
fn a_blank_run_as_is_omitted_rather_than_sent_empty() {
    let mut selected = BTreeMap::new();
    selected.insert(1, selection(vec![os("KB5040434")]));
    let opts = RunOptions {
        run_as: "   ".into(),
        script_params: "  ".into(),
        ..RunOptions::default()
    };
    let req = build_action_request(ActionKind::Script, &selected, &opts);
    assert_eq!(req.run_as, None);
    assert_eq!(req.parameters, None);
}

/// These four predicates decide whether the operator sees a confirmation dialog
/// and how the blast radius is described. They are a hand-mirrored copy of the
/// backend's `actions::ActionKind` with a doc comment as the only drift guard,
/// in a crate that had no test module outside this file — so a drifted mirror
/// silently dropped the warning that distinguishes Apply-all from Apply-selected.
#[test]
fn the_mirrored_action_predicates_match_the_backend_table() {
    // kind, is_mutating, can_reboot, is_remediation, runs_a_script, exceeds_selection
    let table = [
        (ActionKind::OsPatchScan, false, false, false, false, false),
        (
            ActionKind::SoftwarePatchScan,
            false,
            false,
            false,
            false,
            false,
        ),
        // The two native applies are the only kinds that reach past the selection:
        // NinjaOne has no per-patch apply endpoint.
        (ActionKind::OsPatchApply, true, true, false, false, true),
        (
            ActionKind::SoftwarePatchApply,
            true,
            true,
            false,
            false,
            true,
        ),
        (ActionKind::OsPatchRemediate, true, true, true, true, false),
        (
            ActionKind::SoftwarePatchRemediate,
            true,
            true,
            true,
            true,
            false,
        ),
        (ActionKind::Reboot, true, true, false, false, false),
        (ActionKind::Script, true, true, false, true, false),
    ];
    for (kind, mutating, reboot, remediation, script, wide) in table {
        assert_eq!(kind.is_mutating(), mutating, "{kind:?} is_mutating");
        assert_eq!(kind.can_reboot(), reboot, "{kind:?} can_reboot");
        assert_eq!(
            kind.is_remediation(),
            remediation,
            "{kind:?} is_remediation"
        );
        assert_eq!(kind.runs_a_script(), script, "{kind:?} runs_a_script");
        assert_eq!(kind.exceeds_selection(), wide, "{kind:?} exceeds_selection");
    }
    assert_eq!(
        table.len(),
        ActionKind::ALL.len(),
        "a new ActionKind must be added to this table — it is the only thing \
         asserting the frontend mirror still matches the backend"
    );
}
