use super::*;
use crate::actions::{ActionKind, JobState};
use crate::rows::PatchFamilies;
use crate::rows::{QueryResult, page_rows};

fn sample_result() -> QueryResult {
    QueryResult {
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
    }
}

#[test]
fn constant_time_eq_agrees_with_ordinary_equality() {
    for (a, b, want) in [
        (&b"abc"[..], &b"abc"[..], true),
        (&b"abc"[..], &b"abd"[..], false),
        (&b"abc"[..], &b"Abc"[..], false), // differs in the first byte
        (&b"abc"[..], &b"ab"[..], false),  // length mismatch
        (&b""[..], &b""[..], true),
    ] {
        assert_eq!(constant_time_eq(a, b), want, "{a:?} vs {b:?}");
    }
}

/// A result whose rows differ only in the field the sorts below key on, so the
/// order a test asserts can only have come from the sort.
fn result_with_devices(names: &[&str]) -> QueryResult {
    QueryResult {
        rows: names
            .iter()
            .enumerate()
            .map(|(i, n)| PatchRow {
                device_id: i as i64,
                device_name: (*n).into(),
                organization: "Alpha".into(),
                location: None,
                device_role: None,
                os_name: None,
                node_class: None,
                needs_reboot: false,
                offline: false,
                patch_type: "OS",
                kb: None,
                name: format!("patch-{n}").into(),
                severity: "Critical",
                severity_rank: 7,
                status: "PENDING".into(),
                first_seen_date: None,
                installed_date: None,
                first_seen_ts: None,
                installed_ts: None,
            })
            .collect(),
        ..sample_result()
    }
}

fn device_sort(desc: bool) -> Option<RowSort> {
    Some(RowSort {
        key: crate::rows::RowSortKey::Device,
        desc,
    })
}

/// Paging a sorted view must be consistent across pages *and* must follow the
/// sort it was asked for, not the one it memoized last.
///
/// The memo is the whole point of `with_sorted_result` — before it, every page
/// request re-sorted the full row set — so the failure it could introduce is a
/// stale order surviving a sort change. Paging forward and then flipping the
/// direction is exactly the sequence an operator produces by clicking a column
/// header twice.
#[test]
fn a_memoized_sort_order_pages_consistently_and_is_rebuilt_when_the_sort_changes() {
    let state = AppState::seeded("http://example.test".into());
    state.store_last_result_if_current(
        state.begin_query(),
        result_with_devices(&["delta", "alpha", "charlie", "bravo"]),
    );

    let page = |sort, offset, limit| {
        state
            .with_sorted_result(sort, |rows, order| page_rows(rows, order, offset, limit))
            .expect("cache readable")
            .expect("a result is cached")
            .into_iter()
            .map(|r| r.device_name.to_string())
            .collect::<Vec<_>>()
    };

    // Two successive pages of one sort must partition the sorted set, not
    // re-sort independently.
    assert_eq!(page(device_sort(false), 0, 2), ["alpha", "bravo"]);
    assert_eq!(page(device_sort(false), 2, 2), ["charlie", "delta"]);

    // Flipping direction must not serve the memo built for the other one.
    assert_eq!(page(device_sort(true), 0, 2), ["delta", "charlie"]);

    // No sort reproduces the cache order exactly.
    assert_eq!(
        page(None, 0, 4),
        ["delta", "alpha", "charlie", "bravo"],
        "an unsorted page must be the canonical cache order"
    );
}

/// The memo lives inside the cache slot, so replacing the result must drop it —
/// otherwise a fresh query would be paged through the previous one's order, and
/// the indices would not even refer to the same rows.
#[test]
fn replacing_the_result_drops_the_memoized_sort_order() {
    let state = AppState::seeded("http://example.test".into());
    state.store_last_result_if_current(
        state.begin_query(),
        result_with_devices(&["delta", "alpha"]),
    );
    let sorted = |state: &AppState| {
        state
            .with_sorted_result(device_sort(false), |rows, order| {
                page_rows(rows, order, 0, 10)
            })
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|r| r.device_name.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(sorted(&state), ["alpha", "delta"]);

    state.store_last_result_if_current(
        state.begin_query(),
        result_with_devices(&["zulu", "yankee", "xray"]),
    );
    assert_eq!(
        sorted(&state),
        ["xray", "yankee", "zulu"],
        "the new result must be sorted on its own rows"
    );
}

/// A mock exposing both current-patch feeds, each counting its own hits.
async fn patch_feed_server() -> wiremock::MockServer {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    for p in [
        "/api/v2/queries/os-patches",
        "/api/v2/queries/software-patches",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{ "id": 1, "deviceId": 1, "kbNumber": "KB1" }],
                "cursor": ""
            })))
            .mount(&server)
            .await;
    }
    server
}

/// How many requests the mock saw for `path`.
async fn hits(server: &wiremock::MockServer, path: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == path)
        .count()
}

#[tokio::test]
async fn an_os_only_query_never_fetches_the_third_party_feed() {
    let server = patch_feed_server().await;
    let state = AppState::seeded(server.uri());

    let current = state
        .fleet_current_patches(false, true, false, None, None)
        .await
        .expect("os-only fetch");

    assert_eq!(current.os.len(), 1, "the requested family is fetched");
    assert!(current.sw.is_empty(), "the unrequested family stays empty");
    // The whole point: a whole-fleet third-party feed runs to six figures and is
    // usually the largest fetch in a query, so an OS-only query must not page it
    // just to discard it.
    assert_eq!(
        hits(&server, "/api/v2/queries/software-patches").await,
        0,
        "software-patches must not be requested at all"
    );
}

#[tokio::test]
async fn widening_to_both_families_reuses_the_already_warm_one() {
    let server = patch_feed_server().await;
    let state = AppState::seeded(server.uri());

    state
        .fleet_current_patches(false, true, false, None, None)
        .await
        .expect("os-only fetch");
    let both = state
        .fleet_current_patches(false, true, true, None, None)
        .await
        .expect("widened fetch");

    assert_eq!(both.os.len(), 1);
    assert_eq!(both.sw.len(), 1);
    // Splitting the cache per family must not cost a refetch of the family that
    // was already warm.
    assert_eq!(
        hits(&server, "/api/v2/queries/os-patches").await,
        1,
        "the warm OS family must be served from cache"
    );
    assert_eq!(hits(&server, "/api/v2/queries/software-patches").await, 1);
}

#[tokio::test]
async fn a_forced_refetch_inside_the_floor_is_served_from_cache() {
    let server = patch_feed_server().await;
    let state = AppState::seeded(server.uri());

    for _ in 0..3 {
        state
            .fleet_current_patches(true, true, false, None, None)
            .await
            .expect("forced fetch");
    }

    // `force` bypasses CURRENT_PATCHES_TTL so a patching operator can pull fresh
    // state — but unbounded it makes the cache decorative on the auto-refresh
    // path, which runs unattended for hours. FORCE_MIN_INTERVAL is the floor.
    assert_eq!(
        hits(&server, "/api/v2/queries/os-patches").await,
        1,
        "forced refetches inside FORCE_MIN_INTERVAL must collapse onto the cache"
    );
}

/// A whole-fleet feed takes long enough that a mutating action routinely lands
/// mid-fetch. `invalidate_current_patches` exists precisely so the next query
/// re-reads post-action state — but the fetch stored unconditionally after its
/// await, so it wrote the pre-action rows straight back and CURRENT_PATCHES_TTL
/// restarted on them. The tenant stamp cannot catch this: it is the same tenant.
#[tokio::test]
async fn an_invalidation_during_a_fetch_is_not_undone_by_that_fetch() {
    use std::time::Duration as StdDuration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "results": [{ "id": 1, "deviceId": 1, "kbNumber": "KB1" }],
                    "cursor": ""
                }))
                // Long enough for the action below to land while this is in flight.
                .set_delay(StdDuration::from_millis(150)),
        )
        .mount(&server)
        .await;
    let state = Arc::new(AppState::seeded(server.uri()));

    let fetching = {
        let state = state.clone();
        tokio::spawn(async move {
            state
                .fleet_current_patches(false, true, false, None, None)
                .await
                .expect("in-flight fetch")
        })
    };
    // Stand in for `run_action` completing an apply while the fetch is paging.
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    state.invalidate_current_patches();
    fetching.await.expect("join");

    // The in-flight fetch may still return its rows to its own caller — it has
    // them — but it must not leave them in the cache for the *next* query.
    state
        .fleet_current_patches(false, true, false, None, None)
        .await
        .expect("post-action fetch");
    assert_eq!(
        hits(&server, "/api/v2/queries/os-patches").await,
        2,
        "the query after the action must re-fetch, not be served pre-action rows"
    );
}

/// The lookups slot had an epoch gate but no single-flight gate, so concurrent
/// callers on a cold cache each paged the three reference lists independently.
/// Recorded as a deferred finding ("real but low impact — the three lookup calls
/// are small and the 5-minute TTL means the cold window is rare") and closed for
/// free by `TenantCache`: the gate is part of the protocol, so a slot cannot be
/// declared without it.
#[tokio::test]
async fn concurrent_lookups_on_a_cold_cache_fetch_once() {
    use std::time::Duration as StdDuration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    for p in [
        "/api/v2/organizations",
        "/api/v2/locations",
        "/api/v2/roles",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([]))
                    // Long enough that all three callers are inside the fetch
                    // window together; without the gate each would issue its own.
                    .set_delay(StdDuration::from_millis(150)),
            )
            .mount(&server)
            .await;
    }
    let state = Arc::new(AppState::seeded(server.uri()));

    let mut tasks = Vec::new();
    for _ in 0..3 {
        let state = state.clone();
        tasks.push(tokio::spawn(async move {
            state.lookups().await.expect("concurrent lookups")
        }));
    }
    for t in tasks {
        t.await.expect("join");
    }

    assert_eq!(
        hits(&server, "/api/v2/organizations").await,
        1,
        "three concurrent callers on a cold cache must page the lookups once"
    );
}

/// Same race, on the lookups slot. `clear_lookups_cache` cleared it bare while
/// `lookups()` stored unconditionally after its await, so a fetch already in
/// flight wrote its pre-invalidation rows straight back and restarted LOOKUP_TTL
/// on them. The tenant stamp cannot catch this: the case it happens on — a
/// same-tenant sign-out — carries the same stamp.
#[tokio::test]
async fn a_lookups_invalidation_during_a_fetch_is_not_undone_by_that_fetch() {
    use std::time::Duration as StdDuration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    for p in [
        "/api/v2/organizations",
        "/api/v2/locations",
        "/api/v2/roles",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([]))
                    .set_delay(StdDuration::from_millis(150)),
            )
            .mount(&server)
            .await;
    }
    let state = Arc::new(AppState::seeded(server.uri()));

    let fetching = {
        let state = state.clone();
        tokio::spawn(async move { state.lookups().await.expect("in-flight lookups") })
    };
    // Stand in for a sign-out landing while the lookups fetch is in flight.
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    state.clear_lookups_cache();
    fetching.await.expect("join");

    state.lookups().await.expect("post-clear lookups");
    assert_eq!(
        hits(&server, "/api/v2/organizations").await,
        2,
        "the lookups after the clear must re-fetch, not be served the rows the \
         in-flight fetch wrote back"
    );
}

/// Queries overlap by design (an auto-refresh tick fires while a manual Run is
/// still paging), so on a cold cache both used to page the entire inventory
/// independently — the largest fetch in the app, run twice.
#[tokio::test]
async fn concurrent_cold_fetches_collapse_onto_one_request() {
    use std::time::Duration as StdDuration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([{ "id": 1, "systemName": "web-01" }]))
                .set_delay(StdDuration::from_millis(100)),
        )
        .mount(&server)
        .await;
    let state = Arc::new(AppState::seeded(server.uri()));

    let (a, b) = tokio::join!(
        {
            let state = state.clone();
            async move { state.fleet_devices(None).await.expect("first") }
        },
        {
            let state = state.clone();
            async move { state.fleet_devices(None).await.expect("second") }
        }
    );

    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert_eq!(
        hits(&server, "/api/v2/devices-detailed").await,
        1,
        "the second caller must wait on the first fetch, not start its own"
    );
}

#[test]
fn last_result_cache_starts_empty_and_clears() {
    let state = AppState::new().expect("build state");
    // A fresh state has no cached result, so export errors with "Run a query
    // before exporting" rather than writing a stale workbook.
    assert!(state.with_current_result(|_| ()).unwrap().is_none());

    state.store_last_result_if_current(state.begin_query(), sample_result());
    assert!(state.with_current_result(|_| ()).unwrap().is_some());

    // Sign-out / instance change drops the cache so a later export can't leak a
    // previous tenant's rows.
    state.clear_last_result();
    assert!(state.with_current_result(|_| ()).unwrap().is_none());
}

/// A whole-fleet query runs for minutes, so one is routinely still in flight when
/// the operator signs out. Its token's generation and tenant are both still
/// current at that point — nothing else started a query, and a second operator on
/// the same instance is the same tenant — so before the result epoch existed the
/// store simply put the departing operator's rows straight back, and export, the
/// HTML report and all three paging commands then served them.
#[test]
fn a_query_in_flight_at_sign_out_cannot_restore_the_cleared_rows() {
    let state = AppState::new().expect("build state");
    let token = state.begin_query();

    // The operator signs out while that query is still fetching.
    state.clear_last_result();

    assert_eq!(
        state.store_last_result_if_current(token, sample_result()),
        StoreOutcome::SessionCleared,
        "a result fetched before the session was cleared must not be stored after it"
    );
    assert!(
        state.with_current_result(|_| ()).unwrap().is_none(),
        "the cache must still read empty for the next operator"
    );
}

/// The epoch must not make ordinary back-to-back queries fail: only a clear bumps
/// it, so a query that starts after the clear stores normally.
#[test]
fn a_query_started_after_the_clear_stores_normally() {
    let state = AppState::new().expect("build state");
    state.clear_last_result();

    let token = state.begin_query();
    assert_eq!(
        state.store_last_result_if_current(token, sample_result()),
        StoreOutcome::Stored
    );
}

/// An auto-refresh tick and a manual Run overlap routinely and do not finish in
/// start order. The run that started last owns the cache, because that is the one
/// whose summary the frontend renders — otherwise the visible table and the rows
/// behind paging/export come from different queries.
#[test]
fn a_superseded_query_does_not_clobber_a_newer_one() {
    let state = AppState::new().expect("build state");

    let first = state.begin_query();
    let second = state.begin_query();

    // The newer run finishes first.
    assert_eq!(
        state.store_last_result_if_current(second, sample_result()),
        StoreOutcome::Stored
    );
    // The older run finishes second and must NOT overwrite it.
    assert_eq!(
        state.store_last_result_if_current(first, sample_result()),
        StoreOutcome::Superseded,
        "a query superseded before it finished must drop its cache write"
    );
    assert!(state.with_current_result(|_| ()).unwrap().is_some());
}

/// Starting a run retires the previous generation even if that run never
/// completes, so a late arrival from an abandoned query is dropped rather than
/// resurrecting rows the operator has moved on from.
#[test]
fn a_query_retired_by_a_later_start_is_dropped_even_if_the_later_one_never_finishes() {
    let state = AppState::new().expect("build state");

    let abandoned = state.begin_query();
    let _newer = state.begin_query(); // started, never stored

    assert_eq!(
        state.store_last_result_if_current(abandoned, sample_result()),
        StoreOutcome::Superseded
    );
    assert!(state.with_current_result(|_| ()).unwrap().is_none());
}

#[test]
fn a_lone_query_stores_normally() {
    let state = AppState::new().expect("build state");
    let only = state.begin_query();
    assert_eq!(
        state.store_last_result_if_current(only, sample_result()),
        StoreOutcome::Stored
    );
    assert!(state.with_current_result(|_| ()).unwrap().is_some());
}

/// A whole-fleet fetch runs for minutes. If the operator switches instance while
/// one is in flight, the result belongs to the tenant it was *fetched* under —
/// stamping it with whatever tenant is current at write time would file another
/// tenant's rows under the new one, which is the one way the tenant check can be
/// wrong rather than merely miss.
#[test]
fn a_query_that_spans_a_tenant_switch_is_dropped_not_restamped() {
    let state = AppState::new().expect("build state");
    let token = state.begin_query();

    // The operator switches instance while the query is still running.
    state.settings.lock().unwrap().instance_base_url = "https://other.example.com".into();

    assert_eq!(
        state.store_last_result_if_current(token, sample_result()),
        StoreOutcome::TenantChanged,
        "a result fetched under the previous tenant must not be stored, and the              caller must be able to tell that apart from supersession"
    );
    assert!(
        state.with_current_result(|_| ()).unwrap().is_none(),
        "and it must not be readable under the new tenant either"
    );
}

#[test]
fn last_result_invisible_after_instance_switch() {
    let state = AppState::new().expect("build state");
    state.store_last_result_if_current(state.begin_query(), sample_result());
    assert!(state.with_current_result(|_| ()).unwrap().is_some());

    // Switch the instance WITHOUT calling clear_* — the read must still miss, so a
    // forgotten invalidation can't serve the previous tenant's rows.
    state.settings.lock().unwrap().instance_base_url = "https://other.example.com".into();
    assert!(
        state.with_current_result(|_| ()).unwrap().is_none(),
        "a tenant switch must invalidate the cached result at read time"
    );
}

#[test]
fn last_result_invisible_after_client_id_switch() {
    // Pre-refactor, only an instance-URL change invalidated the result, so
    // switching to a different client id (app registration) left the prior rows
    // exportable. Tenant-keyed reads close that gap.
    let state = AppState::new().expect("build state");
    state.store_last_result_if_current(state.begin_query(), sample_result());
    assert!(state.with_current_result(|_| ()).unwrap().is_some());

    state.settings.lock().unwrap().client_id = Some("different-client".into());
    assert!(state.with_current_result(|_| ()).unwrap().is_none());
}

fn sample_job(id: u64, state: JobState) -> JobReport {
    JobReport {
        id,
        batch_id: 1,
        device_id: 7,
        device_name: "srv-1".into(),
        organization: "Contoso".into(),
        kind: ActionKind::OsPatchApply,
        detail: "Apply OS patches".into(),
        dry_run: false,
        state,
        dispatched_at: "2026-01-01 00:00:00 UTC".into(),
        dispatched_ts: 0,
        finished_at: None,
        duration_seconds: None,
        activity_id: None,
        series_uid: None,
        exit_code: None,
    }
}

#[test]
fn jobs_are_invisible_after_an_instance_switch() {
    let state = AppState::new().expect("build state");
    state.append_jobs(vec![sample_job(1, JobState::Running)]);
    assert_eq!(state.jobs_snapshot().len(), 1);

    // Same guarantee as `last_result`: switching tenant WITHOUT calling clear_*
    // must read as a miss, so a forgotten invalidation can't surface another
    // tenant's dispatch history.
    state.settings.lock().unwrap().instance_base_url = "https://other.example.com".into();
    assert!(state.jobs_snapshot().is_empty());
    assert!(state.pending_jobs().is_empty());
}

#[test]
fn job_updates_key_on_job_id_not_device_id() {
    let state = AppState::new().expect("build state");
    // Two rows for the SAME device, as happens when batches overlap.
    state.append_jobs(vec![
        sample_job(1, JobState::Running),
        sample_job(2, JobState::Running),
    ]);

    state.apply_job_updates(vec![sample_job(2, JobState::Completed)]);
    let jobs = state.jobs_snapshot();
    assert_eq!(jobs[0].state, JobState::Running, "row 1 must be untouched");
    assert_eq!(jobs[1].state, JobState::Completed);
    assert_eq!(state.pending_jobs().len(), 1);
}

#[test]
fn job_history_evicts_terminal_rows_before_in_flight_ones() {
    let state = AppState::new().expect("build state");
    // One in-flight row, then enough terminal rows to overflow the cap.
    state.append_jobs(vec![sample_job(0, JobState::Running)]);
    let filler: Vec<JobReport> = (1..=MAX_JOBS as u64)
        .map(|i| sample_job(i, JobState::Completed))
        .collect();
    state.append_jobs(filler);

    let jobs = state.jobs_snapshot();
    assert_eq!(jobs.len(), MAX_JOBS);
    assert!(
        jobs.iter().any(|j| j.id == 0),
        "an in-flight job must never be evicted out from under the poller"
    );
}

#[test]
fn confirm_token_is_single_use_and_bound_to_the_request() {
    let state = AppState::new().expect("build state");
    state.store_pending_confirm("tok".into(), "hash-a".into());

    // A token that doesn't match the request it was issued for is refused.
    assert!(!state.consume_confirm_token("tok", "hash-b"));
    // ...and that attempt already consumed the slot, so the correct pair now
    // fails too. Failing closed is the right direction for a dispatch gate.
    assert!(!state.consume_confirm_token("tok", "hash-a"));

    state.store_pending_confirm("tok2".into(), "hash-a".into());
    assert!(state.consume_confirm_token("tok2", "hash-a"));
    // Single use: a double-click can't dispatch twice.
    assert!(!state.consume_confirm_token("tok2", "hash-a"));
}

/// An approval is for one instance. `request_hash` destructures `ActionRequest`
/// exhaustively, but `ActionRequest` has no instance field to hash — and
/// `run_action` re-reads `instance_base_url` from settings *after* consuming the
/// token. So a plan approved against instance A could dispatch against instance B
/// inside the 5-minute window, on devices the operator never saw. Every other
/// cache here is tenant-stamped; the slot that authorizes writes to real devices
/// has to be too.
#[test]
fn a_confirm_token_does_not_survive_an_instance_switch() {
    let state = AppState::new().expect("build state");
    state.store_pending_confirm("tok".into(), "hash-a".into());

    // The operator changes instance while the confirmation dialog is open.
    if let Ok(mut settings) = state.settings.lock() {
        settings.instance_base_url = "https://other.ninjarmm.com".into();
    }

    assert!(
        !state.consume_confirm_token("tok", "hash-a"),
        "an approval granted against one instance must not dispatch against another"
    );
}

#[test]
fn the_job_poller_slot_admits_only_one_claimant() {
    let state = AppState::new().expect("build state");
    let claim = state.try_claim_job_poller().expect("first claim");
    assert!(
        state.try_claim_job_poller().is_none(),
        "a second batch must join the running poller, not spawn another"
    );
    // Idle (no jobs recorded), so the claim is released and re-claimable.
    assert!(state.release_job_poller_if_idle(claim).is_none());
    assert!(state.try_claim_job_poller().is_some());
}

/// Dropping the claim on *any* path releases the slot. The flag used to be
/// cleared only inside `release_job_poller_if_idle`, so a panic or a cancelled
/// task leaked it permanently and every later poller returned immediately —
/// silently ending all job polling for the life of the process.
#[test]
fn dropping_the_claim_releases_the_slot() {
    let state = AppState::new().expect("build state");
    {
        let _claim = state.try_claim_job_poller().expect("first claim");
        assert!(state.try_claim_job_poller().is_none(), "held while alive");
    }
    assert!(
        state.try_claim_job_poller().is_some(),
        "an unwound poller must not strand the slot"
    );
}

/// The claim outlives a panic, which is the case a plain `store(false)` at one
/// call site cannot cover.
#[test]
fn a_panicking_poller_still_releases_the_slot() {
    let state = AppState::new().expect("build state");
    let claim = state.try_claim_job_poller().expect("first claim");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _held = claim;
        panic!("advance_job blew up");
    }));
    assert!(result.is_err(), "the panic must actually have happened");
    assert!(
        state.try_claim_job_poller().is_some(),
        "unwinding drops the claim, so the next dispatch can poll"
    );
}

/// The lost-wakeup race: the poller finds its pending set empty and moves to
/// release, but a batch is dispatched in that gap. Its `try_claim` fails because
/// the flag is still set, so if the poller released unconditionally those jobs
/// would never be polled by anyone.
#[test]
fn the_poller_keeps_its_claim_when_work_arrives_during_release() {
    let state = AppState::new().expect("build state");
    let claim = state.try_claim_job_poller().expect("first claim");

    // A batch lands: its jobs are recorded before it tries to claim.
    state.append_jobs(vec![sample_job(1, JobState::Running)]);
    assert!(
        state.try_claim_job_poller().is_none(),
        "the running poller still holds the claim"
    );

    let claim = state
        .release_job_poller_if_idle(claim)
        .expect("an unresolved job must keep the poller alive rather than orphan it");
    assert!(
        state.try_claim_job_poller().is_none(),
        "and the claim must still be held"
    );

    // Once the job settles, the poller may retire.
    state.apply_job_updates(vec![sample_job(1, JobState::Completed)]);
    assert!(state.release_job_poller_if_idle(claim).is_none());
    assert!(state.try_claim_job_poller().is_some());
}
/// The export and the HTML report hold the whole result for the length of a
/// `spawn_blocking` write. Taking a handle rather than a deep copy is what keeps
/// the result mutex — the same one all three paging commands take — held for a
/// refcount bump instead of an O(rows) copy of a six-figure fleet.
#[test]
fn the_result_handle_shares_rather_than_copies() {
    let state = AppState::new().expect("build state");
    state.store_last_result_if_current(state.begin_query(), sample_result());

    let a = state
        .current_result_handle()
        .expect("not poisoned")
        .expect("cached for this tenant");
    let b = state
        .current_result_handle()
        .expect("not poisoned")
        .expect("cached for this tenant");

    assert!(
        Arc::ptr_eq(&a, &b),
        "two handles must point at the same allocation, not two copies"
    );
}

/// Same tenant check as every other read path: a miss must read as `None`, never
/// as another tenant's rows.
#[test]
fn the_result_handle_is_empty_with_nothing_cached() {
    let state = AppState::new().expect("build state");
    assert!(
        state
            .current_result_handle()
            .expect("not poisoned")
            .is_none()
    );
}
