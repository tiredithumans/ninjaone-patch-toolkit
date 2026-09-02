use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use crate::model::{Device, Patch, PatchRow, PatchStatus};

use super::*;
use crate::filter::FilterParams;

/// Composes the two halves of paging the way `AppState::with_sorted_result`
/// does — build the order once, then slice it — so these assertions still
/// describe what a real page request produces. Keeping the composition in one
/// helper is also what makes a divergence between `sort_order` and `page_rows`
/// visible here rather than only in the app.
fn sorted_page(
    rows: &[PatchRow],
    offset: usize,
    limit: usize,
    sort: Option<RowSort>,
) -> Vec<PatchRow> {
    let order = sort.map(|s| sort_order(rows, s));
    page_rows(rows, order.as_deref(), offset, limit)
}

/// Borrows an owned patch fixture into the `&[&Patch]` shape the rollups take.
/// Production builds these by filtering the `Arc` cache; the tests own theirs.
fn refs(patches: &[Patch]) -> Vec<&Patch> {
    patches.iter().collect()
}

/// Guards the one hand-maintained pairing left in the severity vocabulary: a
/// field added to `SeverityCounts` but not to `BANDS` makes `total()` disagree
/// with the fields, and everything derived from `BANDS` (the report's chart, its
/// legend, its denominator) would silently drop that band.
///
/// Distinct prime-ish values so a duplicated or transposed accessor is caught
/// too, not just a missing one.
#[test]
fn total_severity_is_the_sum_of_its_bands() {
    let c = SeverityCounts {
        critical: 2,
        important: 3,
        security: 5,
        moderate: 7,
        recommended: 11,
        low: 13,
        optional: 17,
        unknown: 19,
    };
    assert_eq!(
        c.total(),
        2 + 3 + 5 + 7 + 11 + 13 + 17 + 19,
        "SeverityCounts::BANDS must cover every field exactly once"
    );

    // Each accessor must read a distinct field, in the declared order.
    let read: Vec<usize> = SeverityCounts::BANDS
        .iter()
        .map(|(_, get)| get(&c))
        .collect();
    assert_eq!(read, vec![2, 3, 5, 7, 11, 13, 17, 19]);

    let labels: Vec<&str> = SeverityCounts::BANDS.iter().map(|(l, _)| *l).collect();
    assert_eq!(
        labels,
        vec![
            "Critical",
            "Important",
            "Security",
            "Moderate",
            "Recommended",
            "Low",
            "Optional",
            "Unknown"
        ],
        "band order mirrors Severity::rank(), most urgent first"
    );
}

/// `AddAssign` is the other field-wise site; it must agree with `BANDS` too.
#[test]
fn severity_counts_add_assign_sums_every_band() {
    let a = SeverityCounts {
        critical: 1,
        important: 2,
        security: 3,
        moderate: 4,
        recommended: 5,
        low: 6,
        optional: 7,
        unknown: 8,
    };
    let mut sum = SeverityCounts::default();
    sum += &a;
    sum += &a;
    assert_eq!(sum.total(), a.total() * 2);
    for ((_, get), expected) in SeverityCounts::BANDS
        .iter()
        .zip([2, 4, 6, 8, 10, 12, 14, 16])
    {
        assert_eq!(get(&sum), expected);
    }
}
use crate::model::OsInfo;

fn device(id: i64, org: i64, os: &str) -> Device {
    Device {
        id,
        system_name: Some(format!("srv{id}")),
        display_name: Some(format!("srv{id}")),
        organization_id: Some(org),
        location_id: Some(100),
        node_role_id: Some(2),
        node_class: Some("WINDOWS_SERVER".into()),
        offline: Some(false),
        os: Some(OsInfo {
            name: Some(os.into()),
            needs_reboot: Some(id % 2 == 0),
        }),
    }
}

fn patch(device_id: i64, status: &str, sev: &str, released_days_ago: Option<i64>) -> Patch {
    Patch {
        device_id: Some(device_id),
        kb_number: Some("KB5040434".into()),
        name: Some("Cumulative Update".into()),
        version: None,
        product_vendor: None,
        severity: Some(sev.into()),
        status: Some(status.into()),
        patch_type: None,
        collected_timestamp: released_days_ago
            .map(|d| (Utc::now() - Duration::days(d)).timestamp() as f64),
        installed_timestamp: None,
    }
}

fn maps() -> LookupMaps {
    LookupMaps {
        orgs: HashMap::from([(10, "Contoso".to_string())]),
        locations: HashMap::from([(100, "HQ".to_string())]),
        roles: HashMap::from([(2, "Domain Controller".to_string())]),
    }
}

/// A row must not disagree with itself. Some NinjaOne endpoints return these
/// `*At` fields in **milliseconds**; the displayed date goes through
/// `unix_to_datetime` (which normalises), so writing the raw value into the sort
/// timestamp made a millisecond-valued record render as 2026 while sorting as a
/// year-58000 date — always winning "latest failure" and the First-seen sort.
#[test]
fn row_timestamps_are_normalised_like_the_dates_beside_them() {
    let seconds = 1_777_000_000_f64;
    let mut ms_patch = patch(1, "FAILED", "CRITICAL", None);
    ms_patch.collected_timestamp = Some(seconds * 1000.0);
    ms_patch.installed_timestamp = Some(seconds * 1000.0);

    let devices = [device(1, 10, "Windows Server 2022")];
    let by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();
    let patches = vec![ms_patch];
    let rows = build_rows(
        &by_id,
        &maps(),
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: None,
            status_filter: None,
        }],
        &FilterParams::default().prepare(),
    );

    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(
        r.first_seen_ts,
        Some(seconds as i64),
        "a millisecond value must be normalised, not stored raw"
    );
    assert_eq!(r.installed_ts, Some(seconds as i64));
    // And the timestamp must agree with the date rendered next to it.
    let from_ts = DateTime::<Utc>::from_timestamp(r.first_seen_ts.unwrap(), 0).unwrap();
    assert_eq!(
        r.first_seen_date,
        fmt_dt(Some(from_ts)),
        "the sort timestamp and the displayed date must describe the same instant"
    );
}

#[test]
fn build_rows_resolves_names_and_applies_os_filter() {
    let d1 = device(1, 10, "Windows Server 2022");
    let d2 = device(2, 10, "Windows Server 2019");
    let by_id = HashMap::from([(1, &d1), (2, &d2)]);
    let patches = vec![
        patch(1, "PENDING", "CRITICAL", Some(5)),
        patch(2, "PENDING", "LOW", Some(5)),
    ];
    let maps = maps();
    let filter = FilterParams {
        os_name_contains: Some("2022".into()),
        ..Default::default()
    };
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: None,
            status_filter: None,
        }],
        &filter.prepare(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(&*rows[0].organization, "Contoso");
    assert_eq!(rows[0].location.as_deref(), Some("HQ"));
    assert_eq!(rows[0].device_role.as_deref(), Some("Domain Controller"));
    assert_eq!(rows[0].patch_type, "OS");
}

#[test]
fn first_seen_filter_narrows_rows() {
    let d1 = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d1)]);
    let maps = maps();
    let patches = vec![
        patch(1, "PENDING", "CRITICAL", Some(2)), // released 2 days ago → kept
        patch(1, "PENDING", "CRITICAL", Some(100)), // released 100 days ago → dropped
    ];
    let cutoff = (Utc::now() - Duration::days(10)).timestamp();
    let filter = FilterParams {
        detected_after: Some(cutoff),
        ..Default::default()
    };
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: None,
            status_filter: None,
        }],
        &filter.prepare(),
    );
    assert_eq!(rows.len(), 1);
}

#[test]
fn node_class_filter_drops_patches_without_a_matched_device() {
    // The patch query isn't class-filtered server-side, so build_rows narrows
    // it to patches whose device is in the (class-filtered) device set.
    let d1 = device(1, 10, "Linux"); // matched the class → in the device map
    let by_id = HashMap::from([(1, &d1)]);
    let patches = vec![
        patch(1, "PENDING", "CRITICAL", Some(5)), // device 1 matched → kept
        patch(2, "PENDING", "CRITICAL", Some(5)), // device 2 not in set → dropped
    ];
    let maps = maps();
    let filter = FilterParams {
        node_classes: vec!["LINUX_SERVER".into()],
        ..Default::default()
    };
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: None,
            status_filter: None,
        }],
        &filter.prepare(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].device_id, 1);
}

#[test]
fn install_source_applies_status_override() {
    let d1 = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d1)]);
    let mut p = patch(1, "PENDING", "CRITICAL", None);
    p.status = None;
    let patches = vec![p];
    let maps = maps();
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: Some("INSTALLED"),
            status_filter: None,
        }],
        &FilterParams::default().prepare(),
    );
    assert_eq!(&*rows[0].status, "INSTALLED");
}

#[test]
fn manual_status_matches_pending_filter_and_displays_as_pending() {
    use crate::model::PatchStatus;
    // The "Pending" status maps to NinjaOne's "MANUAL"; a MANUAL patch must pass
    // the Pending filter and render with a "PENDING" label.
    let d = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d)]);
    let maps = maps();
    let patches = vec![patch(1, "MANUAL", "CRITICAL", Some(1))];
    let pending_set = HashSet::from([PatchStatus::Pending.api_value()]);
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: None,
            status_filter: Some(&pending_set),
        }],
        &FilterParams::default().prepare(),
    );
    assert_eq!(rows.len(), 1, "a MANUAL patch matches the Pending filter");
    assert_eq!(&*rows[0].status, "PENDING", "MANUAL renders as PENDING");
}

#[test]
fn failed_filter_keeps_failed_installs_and_drops_installed() {
    use crate::model::PatchStatus;
    // FAILED is an install *result*: it comes from the install-history source
    // (which returns both INSTALLED and FAILED records), narrowed to the
    // requested install statuses. A FAILED-only query must keep the FAILED
    // record and drop the INSTALLED one — the bug was routing FAILED to the
    // current feed, where it never appears, so nothing was returned.
    let d1 = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d1)]);
    let maps = maps();
    let mut failed = patch(1, "FAILED", "CRITICAL", Some(1));
    failed.installed_timestamp = Some((Utc::now() - Duration::days(1)).timestamp() as f64);
    let installed = patch(1, "INSTALLED", "CRITICAL", Some(1));
    let patches = vec![failed, installed];
    let failed_set = HashSet::from([PatchStatus::Failed.api_value()]);
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: Some("INSTALLED"),
            status_filter: Some(&failed_set),
        }],
        &FilterParams::default().prepare(),
    );
    assert_eq!(rows.len(), 1, "only the FAILED install record is kept");
    assert_eq!(&*rows[0].status, "FAILED");
}

#[test]
fn install_filter_falls_back_to_override_for_missing_status() {
    use crate::model::PatchStatus;
    // An install record that omits its own status falls back to the source's
    // override (INSTALLED) for both matching and display, so an INSTALLED query
    // still keeps it.
    let d1 = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d1)]);
    let maps = maps();
    let mut p = patch(1, "INSTALLED", "CRITICAL", Some(1));
    p.status = None;
    let patches = vec![p];
    let installed_set = HashSet::from([PatchStatus::Installed.api_value()]);
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: Some("INSTALLED"),
            status_filter: Some(&installed_set),
        }],
        &FilterParams::default().prepare(),
    );
    assert_eq!(rows.len(), 1, "missing status falls back to the override");
    assert_eq!(&*rows[0].status, "INSTALLED");
}

/// `is_pending` is an exclude list. The allow list it replaced scored a device
/// whose only patch sat in the current feed as FAILED — or under any value the
/// crate had not anticipated — as compliant, which is the one direction this
/// predicate must never fail in.
#[test]
fn a_current_feed_record_is_pending_unless_rejected_or_installed() {
    for pending in [
        Some("MANUAL"),
        Some("APPROVED"),
        Some("FAILED"),
        Some("SOMETHING_NEW"),
        None,
    ] {
        assert!(is_pending(pending), "{pending:?} must count as pending");
    }
    assert!(!is_pending(Some("REJECTED")));
    assert!(!is_pending(Some("INSTALLED")));
}

/// The row join has to agree with the rollups about an untyped current-feed
/// record: `pending_counts` counts it, so the Pending selection must show it.
/// `assemble_result` labels the current sources MANUAL for exactly this.
#[test]
fn an_untyped_current_record_is_a_pending_row_under_the_pending_override() {
    use crate::model::PatchStatus;
    let d = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d)]);
    let maps = maps();
    let mut untyped = patch(1, "MANUAL", "CRITICAL", Some(1));
    untyped.status = None;
    let patches = vec![untyped];
    let refs = refs(&patches);
    assert_eq!(pending_counts(&refs).get(&1), Some(&1));

    let pending_set = HashSet::from([PatchStatus::Pending.api_value()]);
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs,
            type_label: "OS",
            status_override: Some(PatchStatus::Pending.api_value()),
            status_filter: Some(&pending_set),
        }],
        &FilterParams::default().prepare(),
    );
    assert_eq!(rows.len(), 1, "the record the rollups count is also a row");
    assert_eq!(&*rows[0].status, "PENDING");
}

#[test]
fn compliance_counts_compliant_and_aged_backlog() {
    let d1 = device(1, 10, "Windows Server 2022"); // has pending
    let d2 = device(2, 10, "Windows Server 2019"); // compliant
    let by_id = HashMap::from([(1, &d1), (2, &d2)]);
    let maps = maps();
    let current = vec![
        patch(1, "MANUAL", "CRITICAL", Some(45)), // pending (MANUAL), aged
        patch(1, "APPROVED", "IMPORTANT", Some(2)), // approved, fresh
    ];
    let counts = pending_counts(&refs(&current));
    let summaries = build_device_summaries(&[&d1, &d2], &counts, &maps);
    let buckets = build_compliance(&summaries, &refs(&current), &by_id, &maps, 30, Utc::now());
    assert_eq!(buckets.len(), 1);
    let b = &buckets[0];
    assert_eq!(b.devices_total, 2);
    assert_eq!(b.devices_compliant, 1);
    assert_eq!(b.pending_critical, 2);
    assert_eq!(b.aged_critical, 1);
    assert!((b.compliance_pct - 50.0).abs() < 1e-9);
}

/// The two halves of a compliance bucket must describe the *same* devices.
///
/// The device half already skipped offline devices; the patch half did not, so an
/// org whose devices were all offline produced a row reading "0 devices,
/// 100% compliant, 40 pending Critical/Important" — a full green bar drawn from
/// the backlog of the very devices the percentage refused to look at.
#[test]
fn an_excluded_devices_backlog_is_excluded_too() {
    let mut offline = device(1, 10, "Windows Server 2022");
    offline.offline = Some(true);
    let by_id = HashMap::from([(1, &offline)]);
    let maps = maps();
    let current = vec![
        patch(1, "MANUAL", "CRITICAL", Some(90)),
        patch(1, "APPROVED", "IMPORTANT", Some(90)),
    ];
    let counts = pending_counts(&refs(&current));
    let summaries = build_device_summaries(&[&offline], &counts, &maps);
    let buckets = build_compliance(&summaries, &refs(&current), &by_id, &maps, 30, Utc::now());
    assert!(
        buckets.is_empty(),
        "a bucket with no devices in it must not be emitted at all, \
         let alone one reporting 100% beside a backlog: {buckets:?}"
    );
}

/// A patch whose device isn't in the scoped inventory has no device to be
/// non-compliant, so it must not open a bucket of its own. It used to create an
/// `(unknown)` organization with zero devices and — via `pct()`'s empty-bucket
/// rule — 100% compliance.
#[test]
fn an_orphan_patch_does_not_invent_an_organization() {
    let d = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d)]);
    let maps = maps();
    let mut orphan = patch(1, "MANUAL", "CRITICAL", Some(90));
    orphan.device_id = Some(999); // not in the scoped inventory
    let current = vec![orphan];
    let counts = pending_counts(&refs(&current));
    let summaries = build_device_summaries(&[&d], &counts, &maps);
    let buckets = build_compliance(&summaries, &refs(&current), &by_id, &maps, 30, Utc::now());
    assert_eq!(buckets.len(), 1, "only the real organization: {buckets:?}");
    assert_eq!(buckets[0].pending_critical, 0);
}

/// A current-feed record with no `status` is pending by construction — that feed
/// is defined as the patches with no installation attempt — and `status` is not a
/// required property. Dropping it understated the backlog and *raised* the
/// compliance percentage.
#[test]
fn a_current_patch_with_no_status_still_counts_as_pending() {
    let d = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d)]);
    let maps = maps();
    let mut untyped = patch(1, "MANUAL", "CRITICAL", Some(90));
    untyped.status = None;
    let current = vec![untyped];
    let counts = pending_counts(&refs(&current));
    assert_eq!(counts.get(&1).copied(), Some(1));
    let summaries = build_device_summaries(&[&d], &counts, &maps);
    let buckets = build_compliance(&summaries, &refs(&current), &by_id, &maps, 30, Utc::now());
    assert_eq!(buckets[0].devices_compliant, 0);
    assert_eq!(buckets[0].pending_critical, 1);
    // 90 days old → the "61-90 days" bucket.
    assert_eq!(
        build_age_buckets(&refs(&current), &by_id, Utc::now())[2].count,
        1
    );
}

/// Rounding must never manufacture a clean fleet.
#[test]
fn a_compliance_percentage_never_rounds_up_to_a_hundred() {
    assert_eq!(format_pct(100.0), "100%");
    assert_eq!(format_pct(99.9), "99%");
    assert_eq!(format_pct(99.5), "99%");
    assert_eq!(format_pct(94.6), "95%");
    assert_eq!(format_pct(0.4), "0%");
    // Same rule at the workbook's one decimal.
    let cell = |pct| match pct_cell(pct) {
        TableCell::Number(n) => n,
        _ => unreachable!("pct_cell always yields a number"),
    };
    assert!((cell(100.0) - 100.0).abs() < 1e-9);
    assert!((cell(99.99) - 99.9).abs() < 1e-9);
}

#[test]
fn compliance_excludes_offline_devices_from_the_denominator() {
    let online = device(1, 10, "Windows Server 2022"); // online, has a pending patch
    let mut offline = device(2, 10, "Windows Server 2019");
    offline.offline = Some(true); // offline → unknown, must not count
    let by_id = HashMap::from([(1, &online), (2, &offline)]);
    let maps = maps();
    let current = vec![patch(1, "MANUAL", "CRITICAL", Some(1))];
    let counts = pending_counts(&refs(&current));
    let summaries = build_device_summaries(&[&online, &offline], &counts, &maps);
    let buckets = build_compliance(&summaries, &refs(&current), &by_id, &maps, 30, Utc::now());
    assert_eq!(buckets.len(), 1);
    let b = &buckets[0];
    assert_eq!(
        b.devices_total, 1,
        "offline device excluded from denominator"
    );
    assert_eq!(
        b.devices_compliant, 0,
        "the online device has a pending patch"
    );
}

#[test]
fn compliance_by_os_groups_devices_and_patches_by_os() {
    let d1 = device(1, 10, "Windows Server 2022"); // pending → not compliant
    let d2 = device(2, 10, "Windows 11 Pro"); // no pending → compliant
    let by_id = HashMap::from([(1, &d1), (2, &d2)]);
    let maps = maps();
    let current = vec![patch(1, "MANUAL", "CRITICAL", Some(45))]; // aged, on d1
    let counts = pending_counts(&refs(&current));
    let summaries = build_device_summaries(&[&d1, &d2], &counts, &maps);
    let buckets = build_compliance_by_os(&summaries, &refs(&current), &by_id, 30, Utc::now());
    assert_eq!(buckets.len(), 2, "one bucket per distinct OS");
    // Sorted by OS name (case-insensitive): "Windows 11 Pro" before "Windows Server 2022".
    let win11 = &buckets[0];
    assert_eq!(win11.os, "Windows 11 Pro");
    assert_eq!(win11.devices_total, 1);
    assert_eq!(win11.devices_compliant, 1);
    assert_eq!(win11.pending_critical, 0);
    assert!((win11.compliance_pct - 100.0).abs() < 1e-9);
    let server = &buckets[1];
    assert_eq!(server.os, "Windows Server 2022");
    assert_eq!(server.devices_total, 1);
    assert_eq!(
        server.devices_compliant, 0,
        "the device has a pending patch"
    );
    assert_eq!(server.pending_critical, 1);
    assert_eq!(
        server.aged_critical, 1,
        "released 45d ago, past the 30d SLA"
    );
}

#[test]
fn query_result_serializes_camel_case_for_the_frontend() {
    // web-rs/src/types.rs deserializes the query result with
    // rename_all = "camelCase"; serializing snake_case here breaks decoding
    // with `missing field deviceName`. Guard the IPC contract.
    let d = device(2, 10, "Windows Server 2022");
    let by_id = HashMap::from([(2, &d)]);
    let patches = vec![patch(2, "PENDING", "CRITICAL", Some(1))];
    let maps = maps();
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: None,
            status_filter: None,
        }],
        &FilterParams::default().prepare(),
    );
    let counts = pending_counts(&refs(&patches));
    let devices = build_device_summaries(&[&d], &counts, &maps);
    let compliance = build_compliance(&devices, &refs(&patches), &by_id, &maps, 30, Utc::now());
    let result = QueryResult {
        rows,
        devices,
        compliance,
        compliance_by_os: Vec::new(),
        failures: Vec::new(),
        severity_by_org: Vec::new(),
        age_buckets: Vec::new(),
        devices_total: 1,
        devices_offline: 0,
        devices_unpatchable: 0,
        patch_families: PatchFamilies {
            os: true,
            software: true,
        },
        scope: Default::default(),
        generated_at: "2026-01-01 00:00 UTC".into(),
        data_fetched_at: "2026-01-01 00:00 UTC".into(),
    };

    let json = serde_json::to_string(&result).expect("serialize QueryResult");
    for key in [
        "\"deviceName\"",
        "\"deviceRole\"",
        "\"osName\"",
        "\"patchType\"",
        "\"needsReboot\"",
        "\"pendingCount\"",
        "\"devicesTotal\"",
        "\"generatedAt\"",
        "\"compliancePct\"",
    ] {
        assert!(json.contains(key), "missing {key} in {json}");
    }
    assert!(!json.contains("device_name"), "snake_case leaked: {json}");
}

#[test]
fn query_summary_trims_to_first_page_and_reboot_subset() {
    // Two rows, two devices (one needing reboot). A first page of 1 keeps a
    // single row but reports the true total; only the reboot device is carried.
    let d1 = device(1, 10, "Windows Server 2022"); // id 1 → needs_reboot = false
    let d2 = device(2, 10, "Windows Server 2019"); // id 2 → needs_reboot = true
    let by_id = HashMap::from([(1, &d1), (2, &d2)]);
    let maps = maps();
    let patches = vec![
        patch(1, "MANUAL", "CRITICAL", Some(1)),
        patch(2, "MANUAL", "CRITICAL", Some(1)),
    ];
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: None,
            status_filter: None,
        }],
        &FilterParams::default().prepare(),
    );
    let counts = pending_counts(&refs(&patches));
    let devices = build_device_summaries(&[&d1, &d2], &counts, &maps);
    let compliance = build_compliance(&devices, &refs(&patches), &by_id, &maps, 30, Utc::now());
    let result = QueryResult {
        rows,
        devices,
        compliance,
        compliance_by_os: Vec::new(),
        failures: Vec::new(),
        severity_by_org: Vec::new(),
        age_buckets: Vec::new(),
        devices_total: 2,
        devices_offline: 0,
        devices_unpatchable: 0,
        patch_families: PatchFamilies {
            os: true,
            software: true,
        },
        scope: Default::default(),
        generated_at: "2026-01-01 00:00 UTC".into(),
        data_fetched_at: "2026-01-01 00:00 UTC".into(),
    };

    let summary = QuerySummary::from_result(&result, 1);
    assert_eq!(
        summary.rows.len(),
        1,
        "first page is capped at `first_page`"
    );
    assert_eq!(summary.rows_total, 2, "total reflects the full row set");
    assert_eq!(
        summary.reboot_devices.len(),
        1,
        "only the needs-reboot device is carried"
    );
    assert!(summary.reboot_devices.iter().all(|d| d.needs_reboot));
    assert_eq!(summary.devices_total, 2);

    // The IPC contract is camelCase, same as QueryResult.
    let json = serde_json::to_string(&summary).expect("serialize QuerySummary");
    for key in ["\"rowsTotal\"", "\"rebootDevices\"", "\"devicesTotal\""] {
        assert!(json.contains(key), "missing {key} in {json}");
    }
}

#[test]
fn unmapped_org_and_missing_device_fall_back_to_placeholders() {
    let maps = maps(); // only org 10 ("Contoso") is mapped
    // Device 1 belongs to org 999, which is absent from the lookup map.
    let d1 = device(1, 999, "Windows Server 2022");
    let devices = [d1];
    let by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();
    // One patch on the unmapped-org device, one on a device id not in inventory.
    let patches = vec![
        patch(1, "MANUAL", "CRITICAL", Some(1)),
        patch(404, "MANUAL", "CRITICAL", Some(1)),
    ];
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: None,
            status_filter: None,
        }],
        &FilterParams::default().prepare(),
    );

    assert_eq!(rows.len(), 2);
    let mapped = rows.iter().find(|r| r.device_id == 1).unwrap();
    assert_eq!(
        &*mapped.organization, "(unknown)",
        "an org id absent from the lookup map renders as (unknown)"
    );
    assert_eq!(&*mapped.device_name, "srv1");
    let orphan = rows.iter().find(|r| r.device_id == 404).unwrap();
    assert_eq!(
        &*orphan.device_name, "(unknown)",
        "a patch for a device not in inventory has no resolvable name"
    );
    assert_eq!(&*orphan.organization, "(unknown)");
}

/// Rows that report the same value must *share* it, not each own a copy.
///
/// This is the whole point of the interner, and it is invisible in every other
/// assertion — a row set that duplicated every string would satisfy all of them
/// while costing the cached result an allocation per field per row. On a fleet
/// where one patch is missing from thousands of devices, and every device in an
/// org repeats its org name, that difference is most of the result's memory.
#[test]
fn rows_share_one_allocation_per_distinct_string() {
    let maps = maps();
    let devices = [device(1, 10, "Windows Server 2022")];
    let by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();
    // The same device and the same patch identity, twice.
    let patches = vec![
        patch(1, "MANUAL", "CRITICAL", Some(1)),
        patch(1, "MANUAL", "CRITICAL", Some(1)),
    ];
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: None,
            status_filter: None,
        }],
        &FilterParams::default().prepare(),
    );
    assert_eq!(rows.len(), 2);
    let (a, b) = (&rows[0], &rows[1]);
    for (what, x, y) in [
        ("organization", &a.organization, &b.organization),
        ("device_name", &a.device_name, &b.device_name),
        ("name", &a.name, &b.name),
        ("status", &a.status, &b.status),
    ] {
        assert!(
            Arc::ptr_eq(x, y),
            "`{what}` must be one shared allocation across rows, not a copy each"
        );
    }
    assert!(Arc::ptr_eq(
        a.os_name.as_ref().unwrap(),
        b.os_name.as_ref().unwrap()
    ));
}

#[test]
fn empty_inputs_yield_no_rows_or_compliance() {
    let maps = maps();
    let by_id: HashMap<i64, &Device> = HashMap::new();
    let rows = build_rows(&by_id, &maps, &[], &FilterParams::default().prepare());
    assert!(rows.is_empty());
    let compliance = build_compliance(&[], &[], &by_id, &maps, 30, Utc::now());
    assert!(compliance.is_empty());
}

fn assert_keys_present(value: &serde_json::Value, required: &[&str], what: &str) {
    let obj = value
        .as_object()
        .unwrap_or_else(|| panic!("{what} did not serialize to a JSON object"));
    for key in required {
        assert!(
            obj.contains_key(*key),
            "{what} is missing frontend-required key `{key}` — web-rs/src/types.rs and the \
             backend struct have drifted (a renamed/dropped field would silently break the UI)"
        );
    }
}

/// Pins the IPC wire contract: every key the frontend's mirror DTOs in
/// `web-rs/src/types.rs` deserialize must be present in the backend's serialized
/// output. Renaming/removing a backend field the UI reads fails here, before a
/// user's session silently loses a column, instead of relying on a manual review
/// of the two independent crates staying in sync.
#[test]
fn serialized_shapes_carry_every_frontend_required_key() {
    let d = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d)]);
    let maps = maps();
    let patches = vec![patch(1, "MANUAL", "CRITICAL", Some(1))];
    let rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&patches),
            type_label: "OS",
            status_override: None,
            status_filter: None,
        }],
        &FilterParams::default().prepare(),
    );
    assert_keys_present(
        &serde_json::to_value(&rows[0]).unwrap(),
        &[
            // The row's identity for action selection — the frontend keys its
            // checkboxes on this, so dropping it silently breaks selection.
            "deviceId",
            "deviceName",
            "organization",
            "location",
            "deviceRole",
            "osName",
            "offline",
            "patchType",
            "kb",
            "name",
            "severity",
            "status",
            "firstSeenDate",
            "installedDate",
        ],
        "PatchRow",
    );

    // The frontend mirror (`web-rs/src/types.rs`) declares these as `String` /
    // `Option<String>`, and the two crates share no code — nothing but this
    // checks that the wire still carries strings. The backend fields behind them
    // are now a mix of `String`, `Arc<str>` and `&'static str` so that a row set
    // shares its repeated values; all three must serialize identically, and a
    // future field that stops doing so has to fail here rather than at runtime in
    // a webview.
    let row_json = serde_json::to_value(&rows[0]).unwrap();
    for key in [
        "deviceName",
        "organization",
        "location",
        "deviceRole",
        "osName",
        "patchType",
        "kb",
        "name",
        "severity",
        "status",
        "firstSeenDate",
    ] {
        let value = &row_json[key];
        assert!(
            value.is_string(),
            "PatchRow.{key} must reach the frontend as a JSON string, got {value}"
        );
    }
    assert!(
        row_json["deviceId"].is_i64() && row_json["offline"].is_boolean(),
        "the non-string row fields must keep their JSON types too"
    );

    let summaries = build_device_summaries(&[&d], &pending_counts(&refs(&patches)), &maps);
    assert_keys_present(
        &serde_json::to_value(&summaries[0]).unwrap(),
        &[
            "deviceName",
            "organization",
            "location",
            "deviceRole",
            "osName",
            "pendingCount",
        ],
        "DeviceSummary",
    );

    let compliance = build_compliance(&summaries, &refs(&patches), &by_id, &maps, 30, Utc::now());
    assert_keys_present(
        &serde_json::to_value(&compliance[0]).unwrap(),
        &[
            "organization",
            "devicesTotal",
            "devicesCompliant",
            "compliancePct",
            "pendingCritical",
            "agedCritical",
        ],
        "ComplianceBucket",
    );

    let by_os = build_compliance_by_os(&summaries, &refs(&patches), &by_id, 30, Utc::now());
    assert_keys_present(
        &serde_json::to_value(&by_os[0]).unwrap(),
        &[
            "os",
            "devicesTotal",
            "devicesCompliant",
            "compliancePct",
            "pendingCritical",
            "agedCritical",
        ],
        "OsCompliance",
    );

    // The nested aggregates. The fixture used to leave every one of them empty,
    // so a renamed field in `FailureGroup`, `OrgSeverity`, `AgeBucket` or
    // `PatchGroup` reached the webview as a decode error with CI green.
    let failed_patches = {
        let mut p = patch(1, "FAILED", "CRITICAL", Some(1));
        p.installed_timestamp = Some(Utc::now().timestamp() as f64);
        vec![p]
    };
    let failed_rows = build_rows(
        &by_id,
        &maps,
        &[PatchSource {
            patches: &refs(&failed_patches),
            type_label: "OS",
            status_override: None,
            status_filter: None,
        }],
        &FilterParams::default().prepare(),
    );
    let failures = build_failures(&failed_rows);
    assert_keys_present(
        &serde_json::to_value(&failures[0]).unwrap(),
        &[
            "patchType",
            "kb",
            "name",
            "severity",
            "affectedDevices",
            "deviceNames",
            "latestFailure",
        ],
        "FailureGroup",
    );

    let severity_by_org = build_severity_by_org(&refs(&patches), &by_id, &maps);
    let org_json = serde_json::to_value(&severity_by_org[0]).unwrap();
    assert_keys_present(&org_json, &["organization", "counts"], "OrgSeverity");
    assert_keys_present(
        &org_json["counts"],
        &[
            "critical",
            "important",
            "security",
            "moderate",
            "recommended",
            "low",
            "optional",
            "unknown",
        ],
        "SeverityCounts",
    );

    let age_buckets = build_age_buckets(&refs(&patches), &by_id, Utc::now());
    assert_keys_present(
        &serde_json::to_value(&age_buckets[0]).unwrap(),
        &["label", "count"],
        "AgeBucket",
    );

    let groups = build_groups(&rows, GroupBy::Device);
    let page = slice_groups(&groups, 0, 10);
    let page_json = serde_json::to_value(&page).unwrap();
    assert_keys_present(&page_json, &["groups", "total"], "GroupPage");
    assert_keys_present(
        &page_json["groups"][0],
        &[
            "key",
            "label",
            "sublabel",
            "rows",
            "devices",
            "severity",
            "severityRank",
            "offline",
            "needsReboot",
        ],
        "PatchGroup",
    );

    let result = QueryResult {
        rows,
        devices: summaries,
        compliance,
        compliance_by_os: by_os,
        failures,
        severity_by_org,
        age_buckets,
        devices_total: 1,
        devices_offline: 0,
        devices_unpatchable: 0,
        patch_families: PatchFamilies {
            os: true,
            software: true,
        },
        scope: Default::default(),
        generated_at: "2026-01-01 00:00:00 UTC".into(),
        data_fetched_at: "2026-01-01 00:00:00 UTC".into(),
    };
    assert_keys_present(
        &serde_json::to_value(QuerySummary::from_result(&result, 100)).unwrap(),
        &[
            "rows",
            "rowsTotal",
            "rebootDevices",
            "compliance",
            "complianceByOs",
            "failures",
            "severityByOrg",
            "ageBuckets",
            "devicesTotal",
            "devicesOffline",
            "devicesUnpatchable",
            "patchFamilies",
            "generatedAt",
            "dataFetchedAt",
        ],
        "QuerySummary",
    );
}

/// The sentence every compliance surface prints beside its percentages. It has to
/// name both things a bare percentage hides: the excluded devices and the patch
/// families actually counted.
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
}

/// A switch, a printer or a hypervisor is online and carries no patch records,
/// so under the offline-only exclusion it scored *compliant* — a fleet of one
/// server with a backlog and one switch read 50%, and with no OS-type facet the
/// by-OS table opened an "(unknown)" bucket at 100%. The device stays in
/// `devices_total` (it is in scope) and is named in the scope note instead.
#[test]
fn devices_ninjaone_cannot_patch_are_excluded_from_every_fleet_health_rollup() {
    let server = device(1, 10, "Windows Server 2022");
    let mut switch = device(2, 10, "Cisco IOS");
    switch.node_class = Some("NMS_SWITCH".into());
    let mut unclassed = device(3, 10, "Windows 11");
    unclassed.node_class = None;
    let devices = [server, switch, unclassed];
    let by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();
    let maps = maps();
    let patches = vec![patch(1, "MANUAL", "CRITICAL", Some(5))];
    let refs = refs(&patches);
    let summaries = build_device_summaries(
        &devices.iter().collect::<Vec<_>>(),
        &pending_counts(&refs),
        &maps,
    );

    let compliance = build_compliance(&summaries, &refs, &by_id, &maps, 30, Utc::now());
    let org = &compliance[0];
    assert_eq!(
        org.devices_total, 2,
        "the server and the unclassed device count; the switch does not"
    );
    assert_eq!(
        org.devices_compliant, 1,
        "only the unclassed device is clean"
    );

    let by_os = build_compliance_by_os(&summaries, &refs, &by_id, 30, Utc::now());
    assert!(
        by_os.iter().all(|b| b.os != "Cisco IOS"),
        "the switch opens no by-OS bucket: {by_os:?}"
    );
    assert!(rollup_device(&by_id, Some(2)).is_none());
    assert!(
        rollup_device(&by_id, Some(3)).is_some(),
        "no class = nothing to prove it out on"
    );
}

fn sortable_row(device: &str, sev_rank: u8, installed_ts: Option<i64>) -> PatchRow {
    PatchRow {
        severity_rank: sev_rank,
        ..failed_row(1, device, "KB1", installed_ts)
    }
}

/// A row on `device`, carrying `name`/`kb` so grouping can be exercised both ways.
fn group_row(device_id: i64, device: &str, kb: Option<&str>, name: &str, rank: u8) -> PatchRow {
    PatchRow {
        device_id,
        device_name: device.into(),
        kb: kb.map(Into::into),
        name: name.into(),
        severity_rank: rank,
        patch_type: if kb.is_some() { "OS" } else { "SOFTWARE" },
        ..failed_row(device_id, device, "KB1", None)
    }
}

#[test]
fn build_groups_by_device_rolls_up_rows_and_worst_severity() {
    let rows = vec![
        group_row(1, "web-01", Some("KB1"), "Cumulative Update", 3),
        group_row(1, "web-01", None, "Google Chrome 138", 7),
        group_row(2, "web-02", Some("KB1"), "Cumulative Update", 4),
    ];
    let groups = build_groups(&rows, GroupBy::Device);
    assert_eq!(groups.len(), 2, "one group per device");

    // Highest severity in the group wins, so a collapsed row still reads as
    // urgent as its worst member — and that ordering puts web-01 first.
    assert_eq!(&*groups[0].label, "web-01");
    assert_eq!(groups[0].severity_rank, 7);
    assert_eq!(groups[0].rows, 2);
    assert_eq!(groups[0].devices, 1, "a device group is exactly one device");
    assert_eq!(groups[0].device_id, Some(1));
    assert_eq!(&*groups[1].label, "web-02");
}

#[test]
fn build_groups_by_patch_leads_with_blast_radius() {
    let rows = vec![
        // A critical patch on one device...
        group_row(1, "web-01", Some("KB9"), "Rare Critical", 7),
        // ...versus a less severe one missing on three.
        group_row(1, "web-01", None, "Google Chrome 138", 3),
        group_row(2, "web-02", None, "Google Chrome 138", 3),
        group_row(3, "web-03", None, "Google Chrome 138", 3),
    ];
    let groups = build_groups(&rows, GroupBy::Patch);
    assert_eq!(groups.len(), 2);
    // Blast radius leads: "missing on 3 machines" outranks "critical on 1".
    assert_eq!(&*groups[0].label, "Google Chrome 138");
    assert_eq!(groups[0].devices, 3);
    assert_eq!(groups[0].rows, 3);
    assert_eq!(
        groups[0].sublabel, None,
        "third-party patches carry no KB, so the sublabel stays empty"
    );
    assert_eq!(&*groups[1].label, "Rare Critical");
    assert_eq!(groups[1].sublabel.as_deref(), Some("KB9"));
    assert_eq!(groups[1].device_id, None, "a patch group spans devices");
}

/// The matcher reimplements `group_key`'s encoding, so the two must agree for
/// every row and both groupings — otherwise expanding a group silently returns
/// the wrong members (or none).
#[test]
fn group_key_and_matcher_agree() {
    let rows = vec![
        group_row(1, "web-01", Some("KB1"), "Cumulative Update", 5),
        group_row(2, "web-02", Some("KB1"), "Cumulative Update", 5),
        group_row(1, "web-01", None, "Google Chrome 138", 3),
        group_row(3, "db-01", Some("KB2"), "Security Update", 7),
    ];
    for group_by in [GroupBy::Device, GroupBy::Patch] {
        for row in &rows {
            let key = group_key(row, group_by);
            let matcher = GroupKeyMatcher::new(group_by, &key);
            for other in &rows {
                assert_eq!(
                    matcher.matches(other),
                    group_key(other, group_by) == key,
                    "matcher disagreed with group_key for {:?} on {:?}",
                    other.name,
                    group_by
                );
            }
        }
    }
}

/// A key the frontend echoed back from a previous result must match nothing
/// rather than panicking or matching everything.
#[test]
fn an_unparseable_group_key_matches_nothing() {
    let rows = [
        group_row(1, "web-01", Some("KB1"), "Cumulative Update", 5),
        group_row(2, "web-02", None, "Google Chrome 138", 3),
    ];
    for (group_by, key) in [
        (GroupBy::Device, "not-a-number"),
        (GroupBy::Patch, "too\u{1f}few"),
        (GroupBy::Patch, "a\u{1f}b\u{1f}c\u{1f}d"),
    ] {
        let matcher = GroupKeyMatcher::new(group_by, key);
        assert!(
            !rows.iter().any(|r| matcher.matches(r)),
            "{key:?} must match nothing under {group_by:?}"
        );
    }
}

#[test]
fn group_members_returns_only_that_groups_rows() {
    let rows = vec![
        group_row(1, "web-01", Some("KB1"), "Cumulative Update", 5),
        group_row(2, "web-02", Some("KB1"), "Cumulative Update", 5),
        group_row(1, "web-01", None, "Google Chrome 138", 3),
    ];
    // A patch group's members are the affected devices...
    let key = group_key(&rows[0], GroupBy::Patch);
    let members = group_member_page(&rows, GroupBy::Patch, &key, 0, 10);
    assert_eq!(members.len(), 2);
    assert!(members.iter().all(|r| &*r.name == "Cumulative Update"));

    // ...and a device group's members are that device's patches.
    let key = group_key(&rows[0], GroupBy::Device);
    let members = group_member_page(&rows, GroupBy::Device, &key, 0, 10);
    assert_eq!(members.len(), 2);
    assert!(members.iter().all(|r| r.device_id == 1));

    // Paging and a stale/unknown key both behave.
    assert_eq!(
        group_member_page(&rows, GroupBy::Device, &key, 1, 10).len(),
        1
    );
    assert!(group_member_page(&rows, GroupBy::Device, "nope", 0, 10).is_empty());
}

#[test]
fn group_keys_cannot_collide_across_distinct_patches() {
    // The key joins patch_type/kb/name; a name containing the joiner would
    // otherwise be able to impersonate another group's key.
    let a = group_row(1, "web-01", Some("KB1"), "Update", 5);
    let b = group_row(1, "web-01", None, "KB1\u{1f}Update", 5);
    assert_ne!(group_key(&a, GroupBy::Patch), group_key(&b, GroupBy::Patch));
}

#[test]
fn group_page_slices_and_reports_the_total() {
    let rows: Vec<PatchRow> = (1..=5)
        .map(|i| group_row(i, &format!("srv{i}"), Some("KB1"), "Cumulative Update", 5))
        .collect();
    let all = build_groups(&rows, GroupBy::Device);
    let page = slice_groups(&all, 2, 2);
    assert_eq!(page.total, 5, "total counts every group, not the page");
    assert_eq!(page.groups.len(), 2);
    assert!(
        slice_groups(&all, 99, 2).groups.is_empty(),
        "an offset past the end is an empty page, not a panic"
    );
    assert_eq!(
        slice_groups(&all, 99, 2).total,
        5,
        "and the total still describes the whole grouping"
    );
}

#[test]
fn page_rows_without_sort_matches_cache_order() {
    let rows: Vec<PatchRow> = (0..5)
        .map(|i| failed_row(i, &format!("srv{i}"), "KB1", None))
        .collect();
    let page = sorted_page(&rows, 1, 2, None);
    assert_eq!(page.len(), 2);
    assert_eq!(&*page[0].device_name, "srv1");
    assert_eq!(&*page[1].device_name, "srv2");
    assert!(
        sorted_page(&rows, 10, 2, None).is_empty(),
        "offset past end"
    );
}

/// Every `RowSortKey` variant round-trips: each sorts ascending, and reverses
/// under `desc`. Only 3 of the 12 were covered, so a key wired to the wrong
/// field — or one added without a `compare_rows` arm — went unnoticed.
#[test]
fn every_sort_key_orders_and_reverses() {
    // `lo` and `hi` differ in exactly one field, and `lo` must come first when
    // ascending. `device_id` (1 = lo, 2 = hi) is the discriminator, so a key
    // wired to the wrong field is caught by position rather than by re-asking
    // the comparator under test.
    let base = |id: i64| PatchRow {
        device_id: id,
        ..failed_row(1, "dev", "KB1", None)
    };
    macro_rules! case {
        ($key:expr, $field:ident, $lo:expr, $hi:expr) => {{
            let mut l = base(1);
            l.$field = $lo;
            let mut h = base(2);
            h.$field = $hi;
            ($key, l, h)
        }};
    }

    let cases: Vec<(RowSortKey, PatchRow, PatchRow)> = vec![
        case!(
            RowSortKey::Organization,
            organization,
            "alpha".into(),
            "Beta".into()
        ),
        case!(
            RowSortKey::Location,
            location,
            Some("aisle".into()),
            Some("Bay".into())
        ),
        case!(
            RowSortKey::Role,
            device_role,
            Some("app".into()),
            Some("DB".into())
        ),
        case!(
            RowSortKey::Device,
            device_name,
            "alpha".into(),
            "Beta".into()
        ),
        case!(
            RowSortKey::Os,
            os_name,
            Some("alpine".into()),
            Some("Windows".into())
        ),
        case!(RowSortKey::PatchType, patch_type, "OS", "SOFTWARE"),
        case!(RowSortKey::Kb, kb, Some("KB1".into()), Some("KB2".into())),
        case!(RowSortKey::Name, name, "aardvark".into(), "Zebra".into()),
        // Ascending severity is most-urgent-first, so the HIGHER rank is `lo`.
        case!(RowSortKey::Severity, severity_rank, 7, 2),
        case!(
            RowSortKey::Status,
            status,
            "Approved".into(),
            "Failed".into()
        ),
        case!(
            RowSortKey::FirstSeenDate,
            first_seen_ts,
            Some(100),
            Some(200)
        ),
        case!(
            RowSortKey::InstalledDate,
            installed_ts,
            Some(100),
            Some(200)
        ),
    ];

    assert_eq!(
        cases.len(),
        12,
        "every RowSortKey variant needs a case here"
    );

    for (key, lo, hi) in cases {
        // Fed in reverse so an unsorted passthrough fails.
        let rows = vec![hi, lo];
        let ids = |desc: bool| -> Vec<i64> {
            sorted_page(&rows, 0, 10, Some(RowSort { key, desc }))
                .iter()
                .map(|r| r.device_id)
                .collect()
        };
        assert_eq!(ids(false), vec![1, 2], "{key:?} did not order ascending");
        assert_eq!(ids(true), vec![2, 1], "{key:?} did not reverse under desc");
    }
}

/// `PatchType` and `Status` compare case-sensitively (byte order), unlike the
/// name-ish keys. Both are backend-normalised values, so this pins the current
/// behavior rather than leaving it accidental.
#[test]
fn patch_type_and_status_sort_by_byte_order() {
    let lower = PatchRow {
        patch_type: "os",
        ..failed_row(1, "a", "KB1", None)
    };
    let upper = PatchRow {
        patch_type: "OS",
        ..failed_row(2, "b", "KB1", None)
    };
    let sort = RowSort {
        key: RowSortKey::PatchType,
        desc: false,
    };
    assert_eq!(
        compare_rows(&upper, &lower, sort),
        Ordering::Less,
        "uppercase sorts before lowercase — byte order, not case-insensitive"
    );
}

#[test]
fn page_rows_sorts_case_insensitively_then_slices() {
    let rows = vec![
        sortable_row("bravo", 5, None),
        sortable_row("Alpha", 5, None),
        sortable_row("charlie", 5, None),
    ];
    let sort = Some(RowSort {
        key: RowSortKey::Device,
        desc: false,
    });
    let names: Vec<_> = sorted_page(&rows, 0, 10, sort)
        .into_iter()
        .map(|r| r.device_name.to_string())
        .collect();
    assert_eq!(names, ["Alpha", "bravo", "charlie"]);
    // The offset/limit slice applies after the sort.
    assert_eq!(&*sorted_page(&rows, 1, 1, sort)[0].device_name, "bravo");
}

#[test]
fn page_rows_desc_reverses_but_missing_values_stay_last() {
    let rows = vec![
        sortable_row("a", 5, Some(100)),
        sortable_row("b", 5, None),
        sortable_row("c", 5, Some(200)),
    ];
    let key = RowSortKey::InstalledDate;
    let names = |desc: bool| -> Vec<String> {
        sorted_page(&rows, 0, 10, Some(RowSort { key, desc }))
            .into_iter()
            .map(|r| r.device_name.to_string())
            .collect()
    };
    assert_eq!(names(false), ["a", "c", "b"]);
    assert_eq!(
        names(true),
        ["c", "a", "b"],
        "None still sorts last on desc"
    );
}

#[test]
fn page_rows_severity_ascending_is_most_urgent_first() {
    let rows = vec![
        sortable_row("low", 2, None),
        sortable_row("crit", 5, None),
        sortable_row("mod", 3, None),
    ];
    let names: Vec<_> = sorted_page(
        &rows,
        0,
        10,
        Some(RowSort {
            key: RowSortKey::Severity,
            desc: false,
        }),
    )
    .into_iter()
    .map(|r| r.device_name.to_string())
    .collect();
    assert_eq!(names, ["crit", "mod", "low"]);
}

fn failed_row(device_id: i64, device: &str, kb: &str, installed_ts: Option<i64>) -> PatchRow {
    PatchRow {
        device_id,
        device_name: device.into(),
        organization: "Contoso".into(),
        location: None,
        device_role: None,
        os_name: None,
        node_class: None,
        needs_reboot: false,
        offline: false,
        patch_type: "OS",
        kb: Some(kb.into()),
        name: "Cumulative Update".into(),
        severity: "Critical",
        severity_rank: 5,
        status: "FAILED".into(),
        first_seen_date: None,
        installed_date: installed_ts.map(|_| "2026-01-01 00:00 UTC".into()),
        first_seen_ts: None,
        installed_ts,
    }
}

#[test]
fn build_failures_groups_by_patch_and_counts_distinct_devices() {
    let rows = vec![
        failed_row(1, "srv1", "KB1", Some(100)),
        failed_row(2, "srv2", "KB1", Some(200)), // same patch, second device
        failed_row(1, "srv1", "KB1", Some(50)),  // duplicate device + older
        failed_row(3, "srv3", "KB2", Some(10)),
        // A non-FAILED row in the same set must be ignored.
        PatchRow {
            status: "PENDING".into(),
            ..failed_row(9, "srv9", "KB1", Some(999))
        },
    ];
    let groups = build_failures(&rows);
    assert_eq!(groups.len(), 2, "two distinct failing patches");
    // KB1 fails on 2 distinct devices → sorted ahead of KB2 (1 device).
    let kb1 = &groups[0];
    assert_eq!(kb1.kb.as_deref(), Some("KB1"));
    assert_eq!(kb1.affected_devices, 2, "distinct devices, not records");
    assert_eq!(kb1.latest_failure_ts, Some(200), "most recent failure");
    assert_eq!(kb1.device_names.len(), 2, "full deduped device list");
    assert_eq!(groups[1].affected_devices, 1);
}

fn scope_filter() -> FilterParams {
    FilterParams {
        organization_ids: Vec::new(),
        location_ids: Vec::new(),
        role_ids: Vec::new(),
        node_classes: Vec::new(),
        os_name_contains: None,
        search: None,
        severities: Vec::new(),
        detected_within_days: None,
        detected_after: None,
        detected_before: None,
    }
}

const BOTH_FAMILIES: PatchFamilies = PatchFamilies {
    os: true,
    software: true,
};

fn facet<'a>(scope: &'a QueryScope, label: &str) -> Option<&'a str> {
    scope
        .facets
        .iter()
        .chain(&scope.patch_facets)
        .find(|(l, _)| *l == label)
        .map(|(_, v)| v.as_str())
}

fn labels(list: &[(&'static str, String)]) -> Vec<&'static str> {
    list.iter().map(|(l, _)| *l).collect()
}

/// An unfiltered export must *say* it is unfiltered. On a printed artifact the
/// absence of narrowing lines is indistinguishable from a renderer that dropped
/// them, and the two readings differ by the whole fleet.
#[test]
fn an_unnarrowed_query_states_that_it_covers_the_whole_fleet() {
    let scope = build_query_scope(
        &scope_filter(),
        &maps(),
        BOTH_FAMILIES,
        &[PatchStatus::Pending],
        None,
    );
    assert_eq!(
        facet(&scope, "Scope"),
        Some("Whole fleet \u{2014} no device or patch filters applied")
    );
    // The two facets that always apply are still stated.
    assert_eq!(
        facet(&scope, "Patch type"),
        Some("OS and third-party patches")
    );
    assert_eq!(facet(&scope, "Status"), Some("Pending"));
    assert_eq!(
        facet(&scope, "Install history since"),
        None,
        "a Pending-only query never reached the history endpoints"
    );
}

/// Ids resolve to the names the operator picked them by, and an id the lookups
/// can't resolve prints as `id N` — two unresolved ids rendering as
/// "(unknown), (unknown)" would say neither how many were selected nor which.
#[test]
fn the_scope_block_names_every_active_facet() {
    let mut filter = scope_filter();
    filter.organization_ids = vec![10, 77];
    filter.location_ids = vec![100];
    filter.role_ids = vec![2];
    filter.node_classes = vec!["WINDOWS_SERVER".into()];
    filter.os_name_contains = Some("Server 2019".into());
    filter.severities = vec!["CRITICAL".into(), "IMPORTANT".into()];
    filter.search = Some("KB5040434".into());
    filter.detected_within_days = Some(30);
    filter.detected_after = Some(1_777_000_000);
    filter.detected_before = Some(1_779_000_000);

    let scope = build_query_scope(
        &filter,
        &maps(),
        PatchFamilies {
            os: true,
            software: false,
        },
        &[PatchStatus::Pending, PatchStatus::Failed],
        Some(1_776_000_000),
    );

    assert_eq!(facet(&scope, "Scope"), None, "something was narrowed");
    assert_eq!(facet(&scope, "Organizations"), Some("Contoso, id 77"));
    assert_eq!(facet(&scope, "Locations"), Some("HQ"));
    assert_eq!(facet(&scope, "Device roles"), Some("Domain Controller"));
    assert_eq!(facet(&scope, "OS type"), Some("WINDOWS_SERVER"));
    assert_eq!(facet(&scope, "OS name contains"), Some("Server 2019"));
    assert_eq!(facet(&scope, "Patch type"), Some("OS patches only"));
    assert_eq!(facet(&scope, "Status"), Some("Pending, Failed"));
    assert_eq!(facet(&scope, "Severity"), Some("CRITICAL, IMPORTANT"));
    assert_eq!(facet(&scope, "Search"), Some("KB5040434"));
    // Absolute, so it still means the same thing when the report is read months
    // later; the relative window the operator picked rides along in parentheses.
    assert_eq!(
        facet(&scope, "First seen after"),
        Some("2026-04-24 03:06 UTC (last 30 days)")
    );
    assert_eq!(
        facet(&scope, "First seen before"),
        Some("2026-05-17 06:40 UTC")
    );
    assert_eq!(
        facet(&scope, "Install history since"),
        Some("2026-04-12 13:20 UTC")
    );

    // The two tiers, so the exports can say which facets reach the fleet sheets.
    // `Patch type` is fleet-wide: only the families fetched are in the rollups.
    assert_eq!(
        labels(&scope.facets),
        vec![
            "Organizations",
            "Locations",
            "Device roles",
            "OS type",
            "OS name contains",
            "Patch type",
        ]
    );
    assert_eq!(
        labels(&scope.patch_facets),
        vec![
            "Status",
            "Severity",
            "Search",
            "First seen after",
            "First seen before",
            "Install history since",
        ]
    );
}

/// A window entered as an absolute date carries no "(last N days)" tail — that
/// parenthetical describes the control the operator used, not the bound.
#[test]
fn an_absolute_first_seen_bound_is_not_labelled_as_a_relative_window() {
    let mut filter = scope_filter();
    filter.detected_after = Some(1_777_000_000);
    let scope = build_query_scope(
        &filter,
        &maps(),
        BOTH_FAMILIES,
        &[PatchStatus::Pending],
        None,
    );
    assert_eq!(
        facet(&scope, "First seen after"),
        Some("2026-04-24 03:06 UTC")
    );
}

/// Every fleet-health rollup describes one population. The HTML report prints
/// the compliance sections and these two charts in sequence under a header
/// stating "Compliance covers online devices only (N offline devices excluded)",
/// so a rollup that counts a wider set makes that sentence false for part of its
/// own document — and the gap is exactly the excluded backlog, which is the one
/// thing a reader cannot recover from the numbers on the page.
#[test]
fn severity_and_age_rollups_cover_the_same_devices_compliance_does() {
    let online = device(1, 10, "Windows Server 2022");
    let mut offline = device(2, 10, "Windows Server 2022");
    offline.offline = Some(true);
    let devices = [online, offline];
    let by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();
    let maps = maps();

    // One pending Critical on the online device; three on the offline one, plus
    // an orphan with no device in inventory at all.
    let mut orphan = patch(1, "MANUAL", "CRITICAL", Some(5));
    orphan.device_id = Some(999);
    let current = vec![
        patch(1, "MANUAL", "CRITICAL", Some(5)),
        patch(2, "MANUAL", "CRITICAL", Some(5)),
        patch(2, "MANUAL", "CRITICAL", Some(5)),
        patch(2, "MANUAL", "CRITICAL", Some(5)),
        orphan,
    ];
    let current = refs(&current);

    let counts = pending_counts(&current);
    let summaries = build_device_summaries(&devices.iter().collect::<Vec<_>>(), &counts, &maps);
    let compliance = build_compliance(&summaries, &current, &by_id, &maps, 30, Utc::now());
    let severity = build_severity_by_org(&current, &by_id, &maps);
    let age = build_age_buckets(&current, &by_id, Utc::now());

    assert_eq!(
        compliance.len(),
        1,
        "one organization, not an (unknown) too"
    );
    assert_eq!(
        compliance[0].devices_total, 1,
        "the offline device is excluded"
    );
    assert_eq!(compliance[0].pending_critical, 1, "and so is its backlog");

    let severity_total: usize = severity.iter().map(|o| o.counts.total()).sum();
    assert_eq!(
        severity_total, compliance[0].pending_critical,
        "the severity breakdown counted the devices compliance excluded"
    );
    assert_eq!(
        severity.len(),
        1,
        "an orphan patch must not open its own (unknown) organization here \
         when compliance drops it"
    );
    let age_total: usize = age.iter().map(|b| b.count).sum();
    assert_eq!(
        age_total, compliance[0].pending_critical,
        "the age histogram counted them too"
    );
}

#[test]
fn build_severity_by_org_buckets_pending_patches() {
    let d1 = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d1)]);
    let maps = maps();
    let current = vec![
        patch(1, "MANUAL", "CRITICAL", Some(1)),
        patch(1, "APPROVED", "IMPORTANT", Some(1)),
        patch(1, "REJECTED", "CRITICAL", Some(1)), // not pending → ignored
    ];
    let sev = build_severity_by_org(&refs(&current), &by_id, &maps);
    assert_eq!(sev.len(), 1);
    assert_eq!(&*sev[0].organization, "Contoso");
    assert_eq!(sev[0].counts.critical, 1);
    assert_eq!(sev[0].counts.important, 1);
    assert_eq!(sev[0].counts.moderate, 0);
}

#[test]
fn build_age_buckets_separate_undated_patches_from_genuinely_old_ones() {
    let d = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d)]);
    let mut undated = patch(1, "MANUAL", "CRITICAL", Some(5));
    undated.collected_timestamp = None;
    let current = vec![
        patch(1, "MANUAL", "CRITICAL", Some(5)),   // 0-30
        patch(1, "MANUAL", "CRITICAL", Some(200)), // 180+
        undated,
        patch(1, "INSTALLED", "CRITICAL", Some(5)), // not pending → ignored
    ];
    let buckets = build_age_buckets(&refs(&current), &by_id, Utc::now());
    assert_eq!(buckets.len(), 6, "five age bands plus the undated bucket");
    assert_eq!(buckets[0].count, 1, "0-30 bucket");
    // The undated patch must NOT inflate 180+: folding it in made the tallest,
    // most alarming bar mean "we have no timestamp" rather than "this is old".
    assert_eq!(
        buckets[4].count, 1,
        "180+ holds only the genuinely aged one"
    );
    assert_eq!(&*buckets[5].label, "Unknown");
    assert_eq!(buckets[5].count, 1, "the undated patch lands in Unknown");
}

#[test]
fn aggregate_shapes_carry_camel_case_keys() {
    let failures = build_failures(&[failed_row(1, "srv1", "KB1", Some(1))]);
    assert_keys_present(
        &serde_json::to_value(&failures[0]).unwrap(),
        &[
            "patchType",
            "kb",
            "name",
            "severity",
            "severityRank",
            "affectedDevices",
            "deviceNames",
            "latestFailure",
            "latestFailureTs",
        ],
        "FailureGroup",
    );

    let d1 = device(1, 10, "Windows Server 2022");
    let by_id = HashMap::from([(1, &d1)]);
    let sev = build_severity_by_org(&[&patch(1, "MANUAL", "CRITICAL", Some(1))], &by_id, &maps());
    let sev_json = serde_json::to_value(&sev[0]).unwrap();
    assert_keys_present(&sev_json, &["organization", "counts"], "OrgSeverity");
    assert_keys_present(
        &sev_json["counts"],
        &[
            "critical",
            "important",
            "moderate",
            "low",
            "optional",
            "unknown",
        ],
        "SeverityCounts",
    );

    let buckets = build_age_buckets(
        &[&patch(1, "MANUAL", "CRITICAL", Some(1))],
        &by_id,
        Utc::now(),
    );
    assert_keys_present(
        &serde_json::to_value(&buckets[0]).unwrap(),
        &["label", "count"],
        "AgeBucket",
    );
}

/// Path of the fixture this module generates, relative to the repo root.
const DEMO_MIRROR_FIXTURE: &str = "../web-rs/tests/backend-grouping.json";

/// Emits the grouping fixture `web-rs` asserts its demo implementation against, and
/// fails when the committed copy is out of date.
///
/// `web-rs/src/demo.rs` re-implements `group_key`, `build_groups` and
/// `group_members` by hand, because the browser demo has no backend to ask. Its own
/// tests assert *properties* — that device groups lead with the worst severity, that
/// patch groups partition the rows — and properties are exactly what a divergence
/// can satisfy while still being wrong. Commit cc33b0a is the receipt: demo group
/// headers hardcoded `offline: false` / `needsReboot: false`, and demo search was a
/// substring match where the backend strips a `KB` prefix on both sides. Every
/// property test still passed.
///
/// So the backend emits its real output for a shared input, and the frontend asserts
/// byte-equality against it. Neither side can drift without a red test, and the
/// generator is the backend itself rather than a hand-written expectation.
///
/// Regenerate deliberately: `UPDATE_FIXTURES=1 cargo test --manifest-path
/// src-tauri/Cargo.toml demo_grouping_fixture`. A diff here means the frontend must
/// change too.
#[test]
fn demo_grouping_fixture_is_current() {
    // Deliberately exercises the fields cc33b0a found wrong: a group spanning an
    // offline device, one needing a reboot, mixed families, and a KB-prefixed name.
    // Severity label and rank must agree on every row. The backend groups on the
    // numeric rank; the frontend mirror drops that field and re-derives it from the
    // label, so a fixture that varies one without the other tests nothing and fails
    // for the wrong reason. Real rows always carry both from the same enum.
    let sev = |rank: u8| -> (&'static str, u8) {
        match rank {
            7 => ("Critical", 7),
            5 => ("Security", 5),
            3 => ("Recommended", 3),
            _ => ("Low", 2),
        }
    };
    let mut rows = vec![
        group_row(1, "web-01", Some("KB5040434"), "Cumulative Update", 7),
        group_row(1, "web-01", None, "Google Chrome 138", 3),
        group_row(2, "web-02", Some("KB5040434"), "Cumulative Update", 7),
        group_row(3, "db-01", Some("KB5031234"), "Security Update", 5),
        group_row(3, "db-01", None, "7-Zip 24.09", 2),
    ];
    for row in &mut rows {
        let (label, rank) = sev(row.severity_rank);
        row.severity = label;
        row.severity_rank = rank;
    }
    // Exercise the exact fields cc33b0a found the demo hardcoding.
    rows[1].offline = true;
    rows[2].needs_reboot = true;
    rows[3].offline = true;

    let fixture = serde_json::json!({
        "_comment": "Generated by rows::tests::demo_grouping_fixture_is_current. \
                     Do not edit by hand — see that test for why this exists.",
        "rows": rows,
        "byDevice": build_groups(&rows, GroupBy::Device),
        "byPatch": build_groups(&rows, GroupBy::Patch),
        "keysByDevice": rows.iter().map(|r| group_key(r, GroupBy::Device)).collect::<Vec<_>>(),
        "keysByPatch": rows.iter().map(|r| group_key(r, GroupBy::Patch)).collect::<Vec<_>>(),
    });
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&fixture).expect("serialize")
    );

    if std::env::var("UPDATE_FIXTURES").is_ok() {
        std::fs::write(DEMO_MIRROR_FIXTURE, &rendered).expect("write the fixture");
        return;
    }
    let committed = std::fs::read_to_string(DEMO_MIRROR_FIXTURE).unwrap_or_default();
    assert_eq!(
        committed, rendered,
        "{DEMO_MIRROR_FIXTURE} is stale. The backend's grouping output changed, so \
         web-rs/src/demo.rs must change with it. Regenerate with \
         UPDATE_FIXTURES=1 cargo test --manifest-path src-tauri/Cargo.toml \
         demo_grouping_fixture"
    );
}
