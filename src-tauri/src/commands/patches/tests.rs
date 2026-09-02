use super::*;
use crate::auth::AuthState;
use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn empty_summary() -> QuerySummary {
    QuerySummary::from_result(
        &QueryResult {
            rows: Vec::new(),
            devices: Vec::new(),
            compliance: Vec::new(),
            compliance_by_os: Vec::new(),
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            devices_total: 0,
            devices_offline: 0,
            devices_unpatchable: 0,
            patch_families: PatchFamilies {
                os: true,
                software: true,
            },
            scope: Default::default(),
            generated_at: "2026-01-01 00:00:00 UTC".into(),
            data_fetched_at: "2026-01-01 00:00:00 UTC".into(),
        },
        FIRST_PAGE_ROWS,
    )
}

/// The frontend discards a superseded response itself (it applies only its own
/// newest `query_seq`), so this must not become an error the operator never
/// caused — a manual Run overtaken by an auto-refresh tick is routine.
#[test]
fn a_superseded_query_still_returns_its_summary() {
    assert!(summary_for(StoreOutcome::Superseded, empty_summary(), 7).is_ok());
    assert!(summary_for(StoreOutcome::Stored, empty_summary(), 7).is_ok());
}

/// The frontend has no guard for this: `query_seq` counts runs it *starts*, and
/// switching instance never bumps it, so a returned summary was rendered — the
/// previous tenant's rows and rollups over the new tenant's empty cache, while
/// paging and export read the miss. Returning the summary here is the bug.
#[test]
fn a_tenant_switch_mid_query_refuses_to_hand_back_a_renderable_summary() {
    let err = summary_for(StoreOutcome::TenantChanged, empty_summary(), 7)
        .expect_err("a result the cache refused must not be rendered");
    assert!(
        err.message.contains("instance changed"),
        "the operator just switched instance; say so instead of a generic failure: {}",
        err.message
    );
}

/// Same shape: the rows would be on screen while every path that re-reads them
/// (paging, export, the HTML report) fails.
#[test]
fn a_poisoned_cache_refuses_to_hand_back_a_renderable_summary() {
    assert!(summary_for(StoreOutcome::Poisoned, empty_summary(), 7).is_err());
    // Same rule, sharper case: the operator signed out (or re-authorized as
    // someone else) mid-query, so these rows belong to the session that just
    // ended and the cache deliberately refused them. Returning the summary would
    // paint them over a cache that cannot serve a single page of them.
    let err = summary_for(StoreOutcome::SessionCleared, empty_summary(), 7)
        .expect_err("a cleared session must not yield a renderable summary");
    assert!(
        err.message.contains("session ended"),
        "the message must name the cause: {}",
        err.message
    );
}

/// `FIRST_PAGE_ROWS` and the frontend's `PATCHES_PAGE_SIZE` must agree: the
/// summary seeds page 0 inline, and the frontend renders that seed directly
/// rather than fetching it. If the backend sends more rows than a page holds,
/// the surplus is invisible until the operator pages away and back; if it sends
/// fewer, page 0 is short while `rows_total` promises more.
///
/// The two crates share no code (native vs wasm32 targets), so the only thing
/// linking them was a doc comment saying "must match". This reads the frontend's
/// declaration and fails on drift.
#[test]
fn the_seeded_first_page_matches_the_frontend_page_size() {
    const FRONTEND_APP_RS: &str = include_str!("../../../../web-rs/src/app.rs");

    let declared = FRONTEND_APP_RS
        .lines()
        .find_map(|l| l.trim().strip_prefix("const PATCHES_PAGE_SIZE: usize = "))
        .and_then(|v| v.trim_end_matches(';').parse::<usize>().ok())
        .expect("web-rs/src/app.rs must declare `const PATCHES_PAGE_SIZE: usize = N;`");

    assert_eq!(
        FIRST_PAGE_ROWS, declared,
        "FIRST_PAGE_ROWS ({FIRST_PAGE_ROWS}) and the frontend's PATCHES_PAGE_SIZE \
         ({declared}) must be the same number"
    );
}

fn args_with(statuses: Vec<PatchStatus>, patch_type: PatchType) -> PatchQueryArgs {
    PatchQueryArgs {
        filter: FilterParams::default(),
        patch_type,
        statuses,
        install_after_days: None,
    }
}

/// Both `Installed` *and* `Failed` are install results and must route to the
/// history endpoints. Routing `Failed` to the current feed was a real bug — it
/// never appears there ("patches for which there were no installation
/// attempts"), so a FAILED query returned nothing at all.
#[test]
fn install_results_route_to_history_and_the_rest_narrow_the_current_feed() {
    let history = QueryPlan::build(
        args_with(vec![PatchStatus::Failed], PatchType::All),
        30,
        fixed_now(),
    );
    assert!(history.want_installs);
    // The set carries FAILED too: it narrows only the *rows* built from the
    // current feed (the rollups take the unnarrowed feed), and a FAILED record
    // that does arrive there has to be visible under the Failed selection.
    assert_eq!(
        history.current_status_set,
        HashSet::from(["FAILED"]),
        "the current-feed row filter carries every selected status"
    );

    let pending = QueryPlan::build(
        args_with(vec![PatchStatus::Pending], PatchType::All),
        30,
        fixed_now(),
    );
    assert!(
        !pending.want_installs,
        "no history fetch for a pending query"
    );
    assert!(!pending.current_status_set.is_empty());
}

/// The pushdown is what keeps the failure dashboard from downloading the
/// window's successful installs just to drop them — but with both statuses
/// requested it must be absent, or the other kind of record never arrives.
#[test]
fn a_single_install_status_is_pushed_down_and_two_are_not() {
    let one = QueryPlan::build(
        args_with(vec![PatchStatus::Failed], PatchType::All),
        30,
        fixed_now(),
    );
    assert_eq!(one.install_status, Some("FAILED"));

    let both = QueryPlan::build(
        args_with(
            vec![PatchStatus::Failed, PatchStatus::Installed],
            PatchType::All,
        ),
        30,
        fixed_now(),
    );
    assert_eq!(both.install_status, None);
    assert_eq!(both.install_status_set.len(), 2);
}

/// `Duration::days` panics on an out-of-range count, and a settings.json
/// predating the range validation (or edited by hand) can still carry one. A
/// zero or negative lookback is worse than useless: it inverts into a *future*
/// lower bound that matches no install history at all.
#[test]
fn the_install_window_is_clamped_against_hand_edited_settings() {
    let now = fixed_now();
    for (requested, expect_days) in [(Some(0), 1), (Some(-5), 1), (Some(7), 7)] {
        let plan = QueryPlan::build(
            PatchQueryArgs {
                install_after_days: requested,
                ..args_with(vec![PatchStatus::Installed], PatchType::All)
            },
            30,
            now,
        );
        assert_eq!(
            plan.installed_after,
            (now - Duration::days(expect_days)).timestamp(),
            "requested {requested:?} should clamp to {expect_days} day(s)"
        );
    }
    // An absurd stored window must not reach `Duration::days` unclamped.
    let huge = QueryPlan::build(
        PatchQueryArgs {
            install_after_days: Some(i64::MAX),
            ..args_with(vec![PatchStatus::Installed], PatchType::All)
        },
        30,
        now,
    );
    assert_eq!(
        huge.installed_after,
        (now - Duration::days(MAX_WINDOW_DAYS)).timestamp()
    );
}

/// The relative first-seen window is resolved to an absolute bound here because
/// `build_rows`, which applies it, has no clock.
#[test]
fn a_relative_detection_window_becomes_an_absolute_lower_bound() {
    let now = fixed_now();
    let plan = QueryPlan::build(
        PatchQueryArgs {
            filter: FilterParams {
                detected_within_days: Some(7),
                ..FilterParams::default()
            },
            ..args_with(vec![PatchStatus::Pending], PatchType::All)
        },
        30,
        now,
    );
    assert_eq!(
        plan.filter.detected_after,
        Some((now - Duration::days(7)).timestamp())
    );
}

/// A family the query cannot display is never worth a whole-fleet page-through.
#[test]
fn the_requested_patch_type_decides_which_families_are_fetched() {
    let os = QueryPlan::build(
        args_with(vec![PatchStatus::Pending], PatchType::Os),
        30,
        fixed_now(),
    );
    assert!(os.include_os && !os.include_sw);

    let all = QueryPlan::build(
        args_with(vec![PatchStatus::Pending], PatchType::All),
        30,
        fixed_now(),
    );
    assert!(all.include_os && all.include_sw);
}

/// A fixed clock so the release/install windows, SLA aging, and `generated_at`
/// are deterministic regardless of when the test runs.
fn fixed_now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap() // 2023-11-14T22:13:20Z
}

/// Lookups resolved up front — the org/location/role *fetch* is covered by the
/// `api::mod` tests; here they only need to label rows so the join is assertable.
fn lookups() -> Lookups {
    (
        Arc::new(vec![Organization {
            id: 1,
            name: "Alpha".into(),
        }]),
        Arc::new(vec![]),
        Arc::new(vec![]),
    )
}

fn client(server: &MockServer) -> NinjaApiClient {
    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    NinjaApiClient::new(http, auth)
}

/// The whole-fleet device future `run_query` now expects, backed by the test's
/// `/devices-detailed` mock (the caching itself lives in `AppState` and is
/// exercised separately). Keeps the existing per-test device mocks in play.
async fn fleet_devices_via(c: &NinjaApiClient) -> anyhow::Result<Arc<Vec<Device>>> {
    Ok(Arc::new(c.devices(None, None).await?))
}

/// The whole-fleet current-patches future, backed by the test's
/// `/queries/os-patches` mock (software-patches is left empty — the OS feed is
/// what these joins assert). `fetched_at` is fixed for determinism.
async fn fleet_current_via(c: &NinjaApiClient) -> anyhow::Result<CurrentPatches> {
    Ok(CurrentPatches {
        os: Arc::new(c.fleet_os_patches(None, None, None).await?),
        sw: Arc::new(Vec::new()),
        fetched_at: fixed_now(),
    })
}

fn args(patch_type: PatchType, statuses: Vec<PatchStatus>) -> PatchQueryArgs {
    PatchQueryArgs {
        filter: FilterParams::default(),
        patch_type,
        statuses,
        install_after_days: None,
    }
}

fn dev(id: i64, org: i64) -> Device {
    Device {
        id,
        system_name: Some(format!("srv{id}")),
        display_name: None,
        organization_id: Some(org),
        location_id: None,
        node_role_id: None,
        node_class: Some("WINDOWS_SERVER".into()),
        offline: Some(false),
        os: None,
    }
}

fn cur(device_id: i64, kb: &str, status: &str, severity: &str) -> Patch {
    Patch {
        device_id: Some(device_id),
        kb_number: Some(kb.into()),
        name: None,
        version: None,
        product_vendor: None,
        severity: Some(severity.into()),
        status: Some(status.into()),
        patch_type: None,
        collected_timestamp: Some(fixed_now().timestamp() as f64),
        installed_timestamp: None,
    }
}

#[tokio::test]
async fn pending_query_joins_current_feed_and_maps_manual_to_pending() {
    let server = MockServer::start().await;

    // Two online devices in org Alpha; device 10 needs a reboot.
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 10, "systemName": "web-01", "organizationId": 1,
                "offline": false, "os": { "name": "Windows Server 2022", "needsReboot": true }
            },
            {
                "id": 20, "systemName": "web-02", "organizationId": 1,
                "offline": false, "os": { "name": "Windows Server 2019", "needsReboot": false }
            }
        ])))
        .mount(&server)
        .await;

    // Current OS-patch feed: one MANUAL (pending) Critical aged past SLA, one
    // APPROVED Low. With statuses=[Pending] only the MANUAL one becomes a row.
    let aged = fixed_now().timestamp() - 60 * 86_400; // 60 days old
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "deviceId": 10, "kbNumber": "KB1", "status": "MANUAL",
                  "severity": "CRITICAL", "timestamp": aged },
                { "deviceId": 20, "kbNumber": "KB2", "status": "APPROVED",
                  "severity": "LOW", "timestamp": aged }
            ],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let progress = |_: &'static str, _: usize| {};
    let result = run_query(
        &client(&server),
        async { Ok::<_, anyhow::Error>(lookups()) },
        fleet_devices_via(&client(&server)),
        fleet_current_via(&client(&server)),
        30,
        30,
        args(PatchType::Os, vec![PatchStatus::Pending]),
        fixed_now(),
        &progress,
    )
    .await
    .expect("query");

    // Only the MANUAL patch survives the Pending filter, displayed as PENDING.
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert_eq!(row.kb.as_deref(), Some("KB1"));
    assert_eq!(&*row.status, "PENDING");
    assert_eq!(&*row.organization, "Alpha");
    assert_eq!(row.severity, "Critical");
    assert!(row.needs_reboot);

    // Both devices counted; only device 10 lands in the reboot subset.
    assert_eq!(result.devices_total, 2);
    let summary = QuerySummary::from_result(&result, FIRST_PAGE_ROWS);
    assert_eq!(summary.reboot_devices.len(), 1);
    assert_eq!(summary.reboot_devices[0].device_id, 10);

    // Compliance: both online, both carry a pending/approved patch → 0% compliant.
    // The MANUAL Critical (aged) lands in pending_critical AND aged_critical; the
    // APPROVED Low is below the Important rank so neither counts it.
    assert_eq!(result.compliance.len(), 1);
    let alpha = &result.compliance[0];
    assert_eq!(&*alpha.organization, "Alpha");
    assert_eq!(alpha.devices_total, 2);
    assert_eq!(alpha.devices_compliant, 0);
    assert_eq!(alpha.compliance_pct, 0.0);
    assert_eq!(alpha.pending_critical, 1);
    assert_eq!(alpha.aged_critical, 1);
}

#[tokio::test]
async fn installed_query_routes_to_history_endpoint_not_current_feed() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false }
        ])))
        .mount(&server)
        .await;

    // The current feed is still fetched (include_os) but must contribute no rows
    // for an install-only status — this MANUAL record has to be ignored.
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "deviceId": 10, "kbNumber": "KBCURRENT", "status": "MANUAL",
                  "severity": "CRITICAL" }
            ],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    // The install-history endpoint returns one INSTALLED and one FAILED record;
    // statuses=[Installed] keeps only the INSTALLED one.
    let installed = fixed_now().timestamp() - 5 * 86_400;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patch-installs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "deviceId": 10, "kbNumber": "KBOK", "status": "INSTALLED",
                  "installedAt": installed },
                { "deviceId": 10, "kbNumber": "KBBAD", "status": "FAILED" }
            ],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let progress = |_: &'static str, _: usize| {};
    let result = run_query(
        &client(&server),
        async { Ok::<_, anyhow::Error>(lookups()) },
        fleet_devices_via(&client(&server)),
        fleet_current_via(&client(&server)),
        30,
        30,
        args(PatchType::Os, vec![PatchStatus::Installed]),
        fixed_now(),
        &progress,
    )
    .await
    .expect("query");

    // Exactly the install-history INSTALLED record — the current MANUAL row and
    // the FAILED install are both excluded.
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert_eq!(row.kb.as_deref(), Some("KBOK"));
    assert_eq!(&*row.status, "INSTALLED");
    assert!(row.installed_date.is_some());

    // No FAILED status was requested, so the failure rollup is empty.
    assert!(result.failures.is_empty());
}

/// The current feed's own endpoint titles promise "Pending, Failed and Rejected"
/// records and `status` has no enum, so a FAILED or untyped record can arrive
/// there. Both must count against compliance **and** be visible as rows — the
/// old allow list scored the device compliant and showed nothing.
#[tokio::test]
async fn failed_and_untyped_current_records_count_as_pending_and_show_as_rows() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false }
        ])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "deviceId": 10, "kbNumber": "KBFAILED", "status": "FAILED",
                  "severity": "CRITICAL" },
                { "deviceId": 10, "kbNumber": "KBUNTYPED", "severity": "CRITICAL" },
                { "deviceId": 10, "kbNumber": "KBREJECTED", "status": "REJECTED",
                  "severity": "CRITICAL" }
            ],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patch-installs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let progress = |_: &'static str, _: usize| {};
    let result = run_query(
        &client(&server),
        async { Ok::<_, anyhow::Error>(lookups()) },
        fleet_devices_via(&client(&server)),
        fleet_current_via(&client(&server)),
        30,
        30,
        args(
            PatchType::Os,
            vec![PatchStatus::Pending, PatchStatus::Failed],
        ),
        fixed_now(),
        &progress,
    )
    .await
    .expect("query");

    let mut statuses: Vec<(Option<&str>, &str)> = result
        .rows
        .iter()
        .map(|r| (r.kb.as_deref(), &*r.status))
        .collect();
    statuses.sort();
    assert_eq!(
        statuses,
        vec![(Some("KBFAILED"), "FAILED"), (Some("KBUNTYPED"), "PENDING")],
        "the FAILED record shows under Failed, the untyped one under Pending, \
         and the REJECTED one under neither"
    );

    let org = &result.compliance[0];
    assert_eq!(org.devices_total, 1);
    assert_eq!(
        org.devices_compliant, 0,
        "two pending CRITICAL patches make the device non-compliant"
    );
    assert_eq!(
        org.pending_critical, 2,
        "REJECTED is the only status excluded"
    );
    assert_eq!(result.devices[0].pending_count, 2);
}

/// The exports print "Install history since <date>" on the strength of the
/// `installedAfter` parameter, whose format the spec never states. The window is
/// therefore re-applied client-side: a record the server returns from outside it
/// is dropped, and an undated one is kept because the window cannot prove it out.
#[tokio::test]
async fn install_records_outside_the_lookback_window_are_dropped_client_side() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false }
        ])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let now = fixed_now().timestamp();
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patch-installs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "deviceId": 10, "kbNumber": "KBRECENT", "status": "FAILED",
                  "installedAt": now - 5 * 86_400 },
                { "deviceId": 10, "kbNumber": "KBSTALE", "status": "FAILED",
                  "installedAt": now - 45 * 86_400 },
                { "deviceId": 10, "kbNumber": "KBUNDATED", "status": "FAILED" }
            ],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let progress = |_: &'static str, _: usize| {};
    let result = run_query(
        &client(&server),
        async { Ok::<_, anyhow::Error>(lookups()) },
        fleet_devices_via(&client(&server)),
        fleet_current_via(&client(&server)),
        30,
        30,
        args(PatchType::Os, vec![PatchStatus::Failed]),
        fixed_now(),
        &progress,
    )
    .await
    .expect("query");

    let mut kbs: Vec<&str> = result.rows.iter().filter_map(|r| r.kb.as_deref()).collect();
    kbs.sort();
    assert_eq!(
        kbs,
        vec!["KBRECENT", "KBUNDATED"],
        "a 45-day-old record is outside the 30-day window whatever the server sent"
    );
}

#[tokio::test]
async fn failed_query_populates_the_failure_rollup_grouped_by_patch() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false },
            { "id": 20, "systemName": "web-02", "organizationId": 1, "offline": false }
        ])))
        .mount(&server)
        .await;

    // Current feed contributes nothing for a FAILED-only query.
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [], "cursor": ""
        })))
        .mount(&server)
        .await;

    // The same KB fails on two devices; a different KB fails on one.
    let failed_at = fixed_now().timestamp() - 2 * 86_400;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patch-installs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "deviceId": 10, "kbNumber": "KBFAIL", "status": "FAILED",
                  "severity": "CRITICAL", "installedAt": failed_at },
                { "deviceId": 20, "kbNumber": "KBFAIL", "status": "FAILED",
                  "severity": "CRITICAL", "installedAt": failed_at },
                { "deviceId": 10, "kbNumber": "KBOTHER", "status": "FAILED",
                  "severity": "IMPORTANT", "installedAt": failed_at }
            ],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let progress = |_: &'static str, _: usize| {};
    let result = run_query(
        &client(&server),
        async { Ok::<_, anyhow::Error>(lookups()) },
        fleet_devices_via(&client(&server)),
        fleet_current_via(&client(&server)),
        30,
        30,
        args(PatchType::Os, vec![PatchStatus::Failed]),
        fixed_now(),
        &progress,
    )
    .await
    .expect("query");

    assert_eq!(result.failures.len(), 2, "one group per failing patch");
    let top = &result.failures[0];
    assert_eq!(top.kb.as_deref(), Some("KBFAIL"));
    assert_eq!(top.affected_devices, 2, "KBFAIL failed on two devices");
}

/// The provenance block must describe what the query *did*, so it is built from
/// the `QueryPlan` the fetch ran under rather than echoed from the request. This
/// runs the whole path to prove the wiring, not just `build_query_scope`: the
/// install lookback in particular is a plan-derived value the request never
/// carries, and it must appear only when the status selection actually reached
/// the history endpoints.
#[tokio::test]
async fn a_query_records_the_facets_it_ran_under() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [], "cursor": ""
        })))
        .mount(&server)
        .await;

    let mut scoped = args(PatchType::Os, vec![PatchStatus::Pending]);
    scoped.filter.organization_ids = vec![1];
    scoped.filter.severities = vec!["CRITICAL".into()];

    let progress = |_: &'static str, _: usize| {};
    let result = run_query(
        &client(&server),
        async { Ok::<_, anyhow::Error>(lookups()) },
        fleet_devices_via(&client(&server)),
        fleet_current_via(&client(&server)),
        30,
        30,
        scoped,
        fixed_now(),
        &progress,
    )
    .await
    .expect("query");

    // Both tiers, flattened: the tiering itself is pinned in `rows.rs`.
    let facets: Vec<(&str, &str)> = result
        .scope
        .facets
        .iter()
        .chain(&result.scope.patch_facets)
        .map(|(l, v)| (*l, v.as_str()))
        .collect();
    // Resolved through the same lookups the rows are labelled with, so the block
    // and the table name the organization identically.
    assert!(facets.contains(&("Organizations", "Alpha")), "{facets:?}");
    assert!(
        facets.contains(&("Patch type", "OS patches only")),
        "{facets:?}"
    );
    assert!(facets.contains(&("Status", "Pending")), "{facets:?}");
    assert!(facets.contains(&("Severity", "CRITICAL")), "{facets:?}");
    assert!(
        !facets.iter().any(|(l, _)| *l == "Install history since"),
        "a Pending-only query never fetched install history: {facets:?}"
    );
    assert!(
        !facets.iter().any(|(l, _)| *l == "Scope"),
        "the whole-fleet sentence must not appear on a narrowed query: {facets:?}"
    );
}

#[tokio::test]
async fn failed_only_query_pushes_status_filter_to_the_install_endpoint() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false }
        ])))
        .mount(&server)
        .await;

    // The current feed is still fetched (it drives compliance) but contributes
    // no rows for a FAILED-only query.
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [], "cursor": ""
        })))
        .mount(&server)
        .await;

    // This install mock matches ONLY when status=FAILED is present. If the
    // server-side pushdown regressed (no status param sent), nothing would match
    // the install request and run_query would error on the 404 instead of
    // returning the FAILED row — so the assertion below is the pushdown proof.
    let failed_at = fixed_now().timestamp() - 2 * 86_400;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patch-installs"))
        .and(query_param("status", "FAILED"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "deviceId": 10, "kbNumber": "KBFAIL", "status": "FAILED",
                  "severity": "CRITICAL", "installedAt": failed_at }
            ],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let progress = |_: &'static str, _: usize| {};
    let result = run_query(
        &client(&server),
        async { Ok::<_, anyhow::Error>(lookups()) },
        fleet_devices_via(&client(&server)),
        fleet_current_via(&client(&server)),
        30,
        30,
        args(PatchType::Os, vec![PatchStatus::Failed]),
        fixed_now(),
        &progress,
    )
    .await
    .expect("a FAILED-only query must send status=FAILED to the install endpoint");

    assert_eq!(result.rows.len(), 1);
    assert_eq!(&*result.rows[0].status, "FAILED");
    assert_eq!(result.failures.len(), 1);
}

#[tokio::test]
async fn installed_and_failed_query_omits_the_server_side_status_filter() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false }
        ])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [], "cursor": ""
        })))
        .mount(&server)
        .await;

    // Both INSTALLED and FAILED are requested, so neither can be dropped
    // server-side — the call must omit `status`, and this mock matches only then.
    let ts = fixed_now().timestamp() - 86_400;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patch-installs"))
        .and(query_param_is_missing("status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "deviceId": 10, "kbNumber": "KBOK", "status": "INSTALLED", "installedAt": ts },
                { "deviceId": 10, "kbNumber": "KBBAD", "status": "FAILED", "installedAt": ts }
            ],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let progress = |_: &'static str, _: usize| {};
    let result = run_query(
        &client(&server),
        async { Ok::<_, anyhow::Error>(lookups()) },
        fleet_devices_via(&client(&server)),
        fleet_current_via(&client(&server)),
        30,
        30,
        args(
            PatchType::Os,
            vec![PatchStatus::Installed, PatchStatus::Failed],
        ),
        fixed_now(),
        &progress,
    )
    .await
    .expect("an INSTALLED+FAILED query must omit the server-side status filter");

    // Both records survive: one INSTALLED, one FAILED.
    assert_eq!(result.rows.len(), 2);
    assert!(result.rows.iter().any(|r| &*r.status == "INSTALLED"));
    assert!(result.rows.iter().any(|r| &*r.status == "FAILED"));
}

#[tokio::test]
async fn org_scope_filters_cached_fleet_client_side_without_a_df() {
    // The whole-fleet devices + current patches are supplied directly (as the
    // caches would hand them over), spanning two orgs. An org=1 scope must narrow
    // them client-side — no `df`, no API call (no install status is requested, so
    // the api client is never touched) — leaving only Alpha's device and patch in
    // the rows AND the compliance rollup.
    let devices = Arc::new(vec![dev(10, 1), dev(20, 2)]);
    let os_current = Arc::new(vec![
        cur(10, "KB1", "MANUAL", "CRITICAL"), // org 1 (Alpha) — in scope
        cur(20, "KB2", "MANUAL", "CRITICAL"), // org 2 (Beta) — out of scope
    ]);
    let lookups = (
        Arc::new(vec![
            Organization {
                id: 1,
                name: "Alpha".into(),
            },
            Organization {
                id: 2,
                name: "Beta".into(),
            },
        ]),
        Arc::new(vec![]),
        Arc::new(vec![]),
    );

    let mut a = args(PatchType::Os, vec![PatchStatus::Pending]);
    a.filter.organization_ids = vec![1];

    let http = reqwest::Client::new();
    let api = NinjaApiClient::new(
        http.clone(),
        AuthState::seeded(http, "http://127.0.0.1:0".into(), "t"),
    );
    let progress = |_: &'static str, _: usize| {};
    let result = run_query(
        &api,
        async { Ok::<_, anyhow::Error>(lookups) },
        async { Ok::<_, anyhow::Error>(devices) },
        async {
            Ok::<_, anyhow::Error>(CurrentPatches {
                os: os_current,
                sw: Arc::new(Vec::new()),
                fetched_at: fixed_now(),
            })
        },
        30,
        30,
        a,
        fixed_now(),
        &progress,
    )
    .await
    .expect("query");

    assert_eq!(
        result.devices_total, 1,
        "only the in-scope org's device counts"
    );
    assert_eq!(result.rows.len(), 1, "only Alpha's patch becomes a row");
    assert_eq!(result.rows[0].kb.as_deref(), Some("KB1"));
    assert_eq!(&*result.rows[0].organization, "Alpha");
    assert_eq!(
        result.compliance.len(),
        1,
        "only Alpha in the compliance roll"
    );
    assert_eq!(&*result.compliance[0].organization, "Alpha");
    assert_eq!(result.compliance[0].pending_critical, 1);
}
/// `status` is not required on an install record. Under the single-status
/// pushdown the server has already filtered, so labelling an untyped record
/// INSTALLED on a FAILED-only query meant the client-side FAILED backstop then
/// dropped it — the failure dashboard silently lost rows, and the wiremock
/// fixtures always set an explicit status so nothing caught it.
#[test]
fn an_untyped_install_record_takes_the_pushed_down_status() {
    let failed_only = QueryPlan::build(
        args(PatchType::All, vec![PatchStatus::Failed]),
        30,
        Utc::now(),
    );
    assert_eq!(
        failed_only.install_status,
        Some("FAILED"),
        "a single install status is pushed to the server"
    );

    let installed_only = QueryPlan::build(
        args(PatchType::All, vec![PatchStatus::Installed]),
        30,
        Utc::now(),
    );
    assert_eq!(installed_only.install_status, Some("INSTALLED"));

    // With both requested nothing is narrowed, so the label falls back and the
    // client-side set does the filtering.
    let both = QueryPlan::build(
        args(
            PatchType::All,
            vec![PatchStatus::Installed, PatchStatus::Failed],
        ),
        30,
        Utc::now(),
    );
    assert_eq!(both.install_status, None);
    assert_eq!(
        both.install_status.unwrap_or("INSTALLED"),
        "INSTALLED",
        "the fallback is what assemble_result applies"
    );
}

/// The paging surface had no test at all despite 700+ test lines in this file,
/// and `MAX_PAGE_LIMIT` is the only thing standing between a hand-built IPC
/// payload and a request for the whole fleet in one page.
#[test]
fn the_page_limit_is_capped() {
    // 0 is passed through, not rebased: an empty window returns no rows, which is
    // harmless and is exactly what the caller asked for. The cap is the point.
    assert_eq!(clamp_page(0), 0);
    assert_eq!(clamp_page(50), 50);
    assert_eq!(clamp_page(FIRST_PAGE_ROWS), FIRST_PAGE_ROWS);
    assert_eq!(clamp_page(MAX_PAGE_LIMIT), MAX_PAGE_LIMIT);
    assert_eq!(
        clamp_page(MAX_PAGE_LIMIT + 1),
        MAX_PAGE_LIMIT,
        "a caller cannot ask for more than the cap"
    );
    assert_eq!(clamp_page(usize::MAX), MAX_PAGE_LIMIT);
}
