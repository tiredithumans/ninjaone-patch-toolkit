use super::*;
use crate::filter::FilterParams;
use crate::rows::{LookupMaps, build_query_scope};

const TENANT: &str = "https://app.ninjarmm.com\nclient-a";
const ALL: PatchFamilies = PatchFamilies {
    os: true,
    software: true,
};

fn row(device_id: i64, kb: Option<&str>, name: &str, status: &str) -> PatchRow {
    PatchRow {
        device_id,
        product_identifier: None,
        device_name: format!("srv{device_id:02}").into(),
        organization: "Contoso".into(),
        location: None,
        device_role: None,
        os_name: None,
        node_class: None,
        needs_reboot: false,
        offline: false,
        patch_type: if kb.is_some() { "OS" } else { "SOFTWARE" },
        kb: kb.map(Into::into),
        name: name.into(),
        severity: "Critical",
        severity_rank: 5,
        status: status.into(),
        first_seen_date: None,
        installed_date: None,
        first_seen_ts: None,
        installed_ts: None,
    }
}

const STATUSES: [PatchStatus; 2] = [PatchStatus::Pending, PatchStatus::Failed];

fn snap(rows: &[PatchRow], at: &str) -> RunSnapshot {
    RunSnapshot::build(rows, TENANT, "scope".into(), at, &STATUSES)
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("npt-snapshots-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn the_first_run_of_a_scope_reports_no_previous_run_not_zero_changes() {
    let now = snap(&[row(1, Some("KB1"), "CU", "PENDING")], "t1");
    let changes = diff(None, &now);
    assert_eq!(changes.previous_at, None);
    assert_eq!(
        (changes.new_pending, changes.resolved, changes.newly_failed),
        (0, 0, 0)
    );
    assert!(changes.tracks_pending && changes.tracks_failed);
}

#[test]
fn new_resolved_and_newly_failed_are_told_apart() {
    let before = snap(
        &[
            row(1, Some("KB1"), "CU", "PENDING"),
            row(1, Some("KB2"), "Net", "PENDING"),
            row(2, Some("KB1"), "CU", "PENDING"),
            row(3, None, "Chrome", "FAILED"),
        ],
        "2026-09-01 10:00:00 UTC",
    );
    let after = snap(
        &[
            // Still pending: no change.
            row(1, Some("KB1"), "CU", "PENDING"),
            // KB2 on srv01 installed → resolved (it's simply absent).
            // KB1 on srv02 now failed → newly failed, *not* resolved.
            row(2, Some("KB1"), "CU", "FAILED"),
            // Already failing last time → not newly failed.
            row(3, None, "Chrome", "FAILED"),
            // Brand new.
            row(4, Some("KB3"), "SSU", "APPROVED"),
        ],
        "2026-09-02 10:00:00 UTC",
    );
    let c = diff(Some(&before), &after);
    assert_eq!(c.previous_at.as_deref(), Some("2026-09-01 10:00:00 UTC"));
    assert_eq!(c.new_pending, 1);
    assert_eq!(c.new_pending_items[0].device_id, 4);
    assert_eq!(c.resolved, 1);
    assert_eq!(c.resolved_items[0].kb.as_deref(), Some("KB2"));
    assert_eq!(
        c.resolved_items[0].device_name, "srv01",
        "the resolved patch is listed from the snapshot's own display text"
    );
    assert_eq!(c.newly_failed, 1);
    assert_eq!(c.newly_failed_items[0].device_id, 2);
}

#[test]
fn installed_and_rejected_rows_are_not_pending() {
    let before = snap(&[row(1, Some("KB1"), "CU", "PENDING")], "t1");
    let after = snap(
        &[
            row(1, Some("KB1"), "CU", "INSTALLED"),
            row(2, Some("KB9"), "X", "REJECTED"),
        ],
        "t2",
    );
    let c = diff(Some(&before), &after);
    assert_eq!((c.new_pending, c.resolved), (0, 1));
}

#[test]
fn the_patch_identity_normalizes_the_kb_and_the_title() {
    assert_eq!(
        patch_key(&row(1, Some("KB5040434"), "A", "PENDING")),
        patch_key(&row(1, Some("5040434"), "renamed", "PENDING")),
        "an OS patch is its KB, spelled either way"
    );
    assert_eq!(
        patch_key(&row(1, None, " Google Chrome ", "PENDING")),
        patch_key(&row(1, None, "google chrome", "PENDING")),
    );
    assert_ne!(
        patch_key(&row(1, None, "KB1", "PENDING")),
        patch_key(&row(1, Some("KB1"), "KB1", "PENDING")),
        "a software title that looks like a KB is not the OS patch"
    );
}

#[test]
fn lists_are_capped_but_counts_are_exact() {
    let rows: Vec<PatchRow> = (0..(CHANGE_LIST_LIMIT as i64 + 25))
        .map(|i| row(i + 1, Some("KB1"), "CU", "PENDING"))
        .collect();
    let c = diff(Some(&snap(&[], "t1")), &snap(&rows, "t2"));
    assert_eq!(c.new_pending, CHANGE_LIST_LIMIT + 25);
    assert_eq!(c.new_pending_items.len(), CHANGE_LIST_LIMIT);
}

/// `capped` selects the listed entries over borrowed keys instead of building
/// every `ChangeItem` and sorting them all. Pinned against that original
/// build-everything / stable-sort / truncate, on lists well past the cap whose
/// keys collide on every field but the last (case-insensitive device and patch
/// names, shared severities).
#[test]
fn the_capped_lists_match_a_full_sort_and_the_counts_stay_exact() {
    fn reference<'a>(
        snap: &RunSnapshot,
        ixs: impl Iterator<Item = &'a (u32, u32)>,
    ) -> (usize, Vec<ChangeItem>) {
        let mut items: Vec<ChangeItem> = ixs.filter_map(|&ix| snap.item(ix)).collect();
        let total = items.len();
        items.sort_by(|a, b| {
            b.severity_rank
                .cmp(&a.severity_rank)
                .then_with(|| cmp_ci(&a.device_name, &b.device_name))
                .then_with(|| cmp_ci(&a.name, &b.name))
                .then_with(|| a.device_id.cmp(&b.device_id))
        });
        items.truncate(CHANGE_LIST_LIMIT);
        (total, items)
    }

    const BANDS: [(&str, u8); 4] = [
        ("Critical", 5),
        ("Important", 4),
        ("Moderate", 2),
        ("Low", 1),
    ];
    let fleet = |status: fn(i64) -> Option<&'static str>| -> Vec<PatchRow> {
        (1..=120i64)
            .filter_map(|d| status(d).map(|s| (d, s)))
            .flat_map(|(d, s)| {
                (0..8usize).map(move |j| {
                    let (severity, rank) = BANDS[j % 4];
                    let kb = format!("KB{}", 1000 + j);
                    let case = if j % 2 == 0 {
                        "Cumulative"
                    } else {
                        "cumulative"
                    };
                    PatchRow {
                        device_name: if d % 3 == 0 {
                            format!("SRV{:03}", d / 2)
                        } else {
                            format!("srv{:03}", d / 2)
                        }
                        .into(),
                        severity,
                        severity_rank: rank,
                        ..row(d, Some(&kb), &format!("{case} Update {}", j % 3), s)
                    }
                })
            })
            .collect()
    };
    let before = snap(&fleet(|d| (d % 2 == 0).then_some("PENDING")), "t1");
    let after = snap(
        &fleet(|d| match d % 4 {
            1 | 3 => Some("PENDING"),
            0 => Some("FAILED"),
            _ => None,
        }),
        "t2",
    );
    let c = diff(Some(&before), &after);

    let now_pending = after.identities(&after.pending);
    let now_failed = after.identities(&after.failed);
    let prev_pending = before.identities(&before.pending);
    let prev_failed = before.identities(&before.failed);
    let new_pending = reference(
        &after,
        now_pending
            .iter()
            .filter(|(id, _)| !prev_pending.contains_key(*id))
            .map(|(_, ix)| ix),
    );
    let resolved = reference(
        &before,
        prev_pending
            .iter()
            .filter(|(id, _)| !now_pending.contains_key(*id) && !now_failed.contains_key(*id))
            .map(|(_, ix)| ix),
    );
    let newly_failed = reference(
        &after,
        now_failed
            .iter()
            .filter(|(id, _)| !prev_failed.contains_key(*id))
            .map(|(_, ix)| ix),
    );

    assert_eq!(
        (c.new_pending, c.resolved, c.newly_failed),
        (480, 240, 240),
        "every count is exact, past the cap"
    );
    assert_eq!((c.new_pending, c.new_pending_items), new_pending);
    assert_eq!((c.resolved, c.resolved_items), resolved);
    assert_eq!((c.newly_failed, c.newly_failed_items), newly_failed);
}

#[test]
fn a_snapshot_of_another_tenant_or_scope_is_never_diffed() {
    let now = snap(&[row(1, Some("KB1"), "CU", "PENDING")], "t2");
    let other_tenant = RunSnapshot::build(
        &[],
        "https://eu.ninjarmm.com\nclient-a",
        "scope".into(),
        "t1",
        &STATUSES,
    );
    let other_scope = RunSnapshot::build(&[], TENANT, "other".into(), "t1", &STATUSES);
    assert_eq!(diff(Some(&other_tenant), &now).previous_at, None);
    assert_eq!(diff(Some(&other_scope), &now).previous_at, None);
}

#[test]
fn a_snapshot_round_trips_and_stays_per_tenant() {
    let dir = temp_dir("roundtrip");
    let s = snap(
        &[
            row(1, Some("KB1"), "CU", "PENDING"),
            row(2, None, "Chrome", "FAILED"),
        ],
        "t1",
    );
    save_to(&dir, &s);
    assert_eq!(load_from(&dir, TENANT, "scope"), Some(s.clone()));
    assert_eq!(
        load_from(&dir, "https://eu.ninjarmm.com\nclient-a", "scope"),
        None,
        "another tenant's identical scope has no baseline"
    );
    assert_eq!(load_from(&dir, TENANT, "other"), None);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(file_name(TENANT, "scope"));
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tampered_tenant_stamp_is_refused_on_load() {
    let dir = temp_dir("stamp");
    let s = snap(&[row(1, Some("KB1"), "CU", "PENDING")], "t1");
    save_to(&dir, &s);
    // Same file name, different stamp inside: e.g. a hash collision or a copied file.
    let path = dir.join(file_name(TENANT, "scope"));
    let body = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, body.replace("client-a", "client-b")).unwrap();
    assert_eq!(load_from(&dir, TENANT, "scope"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_directory_keeps_only_the_newest_scopes() {
    let dir = temp_dir("prune");
    for i in 0..(MAX_SNAPSHOTS + 5) {
        let s = RunSnapshot::build(&[], TENANT, format!("scope-{i}"), "t", &STATUSES);
        save_to(&dir, &s);
        // Distinct mtimes on filesystems with coarse timestamps.
        let path = dir.join(file_name(TENANT, &format!("scope-{i}")));
        let when =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
    let kept = std::fs::read_dir(&dir).unwrap().count();
    assert_eq!(kept, MAX_SNAPSHOTS);
    assert!(
        load_from(&dir, TENANT, "scope-0").is_none(),
        "the oldest scope was evicted"
    );
    assert!(load_from(&dir, TENANT, &format!("scope-{}", MAX_SNAPSHOTS + 4)).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_newest_snapshot_survives_even_over_the_byte_budget() {
    let dir = temp_dir("budget");
    std::fs::create_dir_all(&dir).unwrap();
    // A pre-existing oversized file, older than what is about to be written.
    let big = dir.join("0000.json");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(MAX_TOTAL_BYTES + 1)
        .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&big)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    let s = snap(&[row(1, Some("KB1"), "CU", "PENDING")], "t1");
    save_to(&dir, &s);
    assert!(!big.exists(), "the older file over budget is evicted");
    assert!(load_from(&dir, TENANT, "scope").is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The scope key must not depend on the order facets were picked in, and must
/// separate everything that changes which rows a run can see.
#[test]
fn the_scope_key_is_order_insensitive_and_separates_what_it_must() {
    let maps = LookupMaps::build(&[], &[], &[]);
    let key = |filter: FilterParams, statuses: &[PatchStatus], families, days| {
        let scope = build_query_scope(&filter, &maps, families, statuses, None);
        scope_key(&scope.fingerprint, families, days)
    };
    let pending = [PatchStatus::Pending];
    let a = key(
        FilterParams {
            organization_ids: vec![2, 1],
            severities: vec!["IMPORTANT".into(), "CRITICAL".into()],
            ..Default::default()
        },
        &[PatchStatus::Failed, PatchStatus::Pending],
        ALL,
        Some(30),
    );
    let b = key(
        FilterParams {
            organization_ids: vec![1, 2],
            severities: vec!["CRITICAL".into(), "IMPORTANT".into()],
            ..Default::default()
        },
        &[PatchStatus::Pending, PatchStatus::Failed],
        ALL,
        Some(30),
    );
    assert_eq!(a, b, "facet order is not scope");

    let base = key(FilterParams::default(), &pending, ALL, None);
    assert_eq!(
        base,
        key(FilterParams::default(), &pending, ALL, None),
        "stable"
    );
    let os_only = PatchFamilies {
        os: true,
        software: false,
    };
    assert_ne!(base, key(FilterParams::default(), &pending, os_only, None));
    assert_ne!(
        base,
        key(
            FilterParams {
                organization_ids: vec![1],
                ..Default::default()
            },
            &pending,
            ALL,
            None
        )
    );
    assert_ne!(
        key(FilterParams::default(), &pending, ALL, Some(30)),
        key(FilterParams::default(), &pending, ALL, Some(60)),
        "a longer install lookback sees more failures"
    );
    // And the tenant separates the files even for one scope key.
    assert_ne!(
        file_name(TENANT, &base),
        file_name("https://eu.ninjarmm.com\nclient-a", &base)
    );
}
