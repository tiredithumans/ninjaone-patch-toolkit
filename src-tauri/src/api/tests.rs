use super::paging::*;
use super::*;
use serde_json::json;

use crate::model::{Organization, Patch};

/// The 5xx arm is decided by the replay policy. A read is retried; an acting POST
/// never is, or it could be replayed into a second reboot or script run (the
/// gateway may have failed *after* the job reached the device queue); a cursor page
/// never is either (`a_cursor_page_is_not_retried_on_a_5xx_but_is_on_a_rejection`).
/// 429 and 401 are safe for all three — the request was rejected before it could
/// reach a device or move a cursor.
#[test]
fn a_5xx_is_retried_for_reads_but_never_for_writes() {
    for status in [
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::BAD_GATEWAY,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        assert!(
            matches!(
                retry_for(status, ReplaySafety::Idempotent, 0, None),
                Retry::Wait(_)
            ),
            "{status} should be retried for a read"
        );
        assert_eq!(
            retry_for(status, ReplaySafety::ActOnce, 0, None),
            Retry::No,
            "{status} must not replay an action that may already be queued"
        );
    }
}

/// A cursor page is a read, but it moves a server-side position keyed by the
/// cursor's stable name: a 5xx that landed after the backend advanced would answer
/// the replay with the next page. Rejections never reached the handler, so they
/// stay retried in place.
#[test]
fn a_cursor_page_is_not_retried_on_a_5xx_but_is_on_a_rejection() {
    for status in [
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::BAD_GATEWAY,
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::GATEWAY_TIMEOUT,
    ] {
        assert_eq!(
            retry_for(status, ReplaySafety::CursorPage, 0, None),
            Retry::No,
            "{status} must not re-send a cursor the server may have advanced"
        );
    }
    assert_eq!(
        retry_for(
            StatusCode::TOO_MANY_REQUESTS,
            ReplaySafety::CursorPage,
            0,
            Some(7)
        ),
        Retry::Wait(Duration::from_secs(7))
    );
    assert_eq!(
        retry_for(StatusCode::UNAUTHORIZED, ReplaySafety::CursorPage, 0, None),
        Retry::Reauth
    );
}

/// The pagination loop restarts a scan on the typed marker, so it has to survive
/// the context `request_page` and its callers layer on top.
#[test]
fn the_cursor_replay_marker_survives_added_context() {
    let wrapped = anyhow::Error::new(std::io::Error::other("timed out"))
        .context(CursorReplayUnsafe("the cursor page timed out".into()))
        .context("software patches");
    assert!(is_cursor_replay_unsafe(&wrapped), "{wrapped:#}");
    assert!(!is_cursor_replay_unsafe(&anyhow::anyhow!(
        "400 Invalid cursor."
    )));
}

#[test]
fn rate_limiting_honors_retry_after_and_applies_to_writes_too() {
    assert_eq!(
        retry_for(
            StatusCode::TOO_MANY_REQUESTS,
            ReplaySafety::ActOnce,
            0,
            Some(30)
        ),
        Retry::Wait(Duration::from_secs(30)),
        "the server's own backoff is honored verbatim — second-guessing it turns \
         a soft rate limit into a hard one"
    );
    // No usable header: fall back rather than hammering.
    assert_eq!(
        retry_for(
            StatusCode::TOO_MANY_REQUESTS,
            ReplaySafety::Idempotent,
            0,
            None
        ),
        Retry::Wait(Duration::from_secs(5))
    );
}

#[test]
fn a_401_forces_a_token_refresh_rather_than_a_plain_retry() {
    assert_eq!(
        retry_for(StatusCode::UNAUTHORIZED, ReplaySafety::ActOnce, 0, None),
        Retry::Reauth
    );
}

/// A client error that is not 401/429 is the server rejecting *what we asked
/// for*; retrying it just repeats the same rejection.
#[test]
fn ordinary_client_errors_and_successes_are_not_retried() {
    for status in [
        StatusCode::BAD_REQUEST,
        StatusCode::FORBIDDEN,
        StatusCode::NOT_FOUND,
        StatusCode::OK,
        StatusCode::NO_CONTENT,
    ] {
        assert_eq!(
            retry_for(status, ReplaySafety::Idempotent, 0, None),
            Retry::No,
            "{status}"
        );
    }
}

#[test]
fn the_retry_budget_is_finite() {
    assert_eq!(
        retry_for(
            StatusCode::TOO_MANY_REQUESTS,
            ReplaySafety::Idempotent,
            MAX_RETRIES,
            Some(1)
        ),
        Retry::No,
        "a server that always 429s must not loop forever"
    );
    assert_eq!(
        retry_for(
            StatusCode::UNAUTHORIZED,
            ReplaySafety::Idempotent,
            MAX_RETRIES,
            None
        ),
        Retry::No
    );
}

/// The body-shape dispatch is a pure function now that the rows deserialize
/// straight into `T`, so the branches that used to be reachable only through a
/// wiremock round trip are asserted directly.
#[test]
fn parse_page_reads_both_pagination_shapes_and_every_empty_form() {
    let array: PageBody<Organization> = parse_page(br#"[{"id":1,"name":"Alpha"}]"#).unwrap();
    let PageBody::Array(rows) = array else {
        panic!("a bare array must decode as the after-paginated shape");
    };
    assert_eq!(rows[0].name, "Alpha");

    let env: PageBody<Organization> =
        parse_page(br#"{"results":[{"id":2,"name":"Beta"}],"cursor":"tok"}"#).unwrap();
    let PageBody::Envelope { results, cursor } = env else {
        panic!("a results/cursor object must decode as the enveloped shape");
    };
    assert_eq!(results[0].name, "Beta");
    assert_eq!(
        next_cursor(cursor.as_ref()).unwrap().map(|c| c.name),
        Some("tok".to_string())
    );

    // 204 is handled before the body is read; these are the on-the-wire forms.
    for empty in [b"".as_slice(), b"   ".as_slice(), b"null".as_slice()] {
        assert!(
            matches!(parse_page::<Organization>(empty).unwrap(), PageBody::Empty),
            "{:?} must read as an empty page",
            String::from_utf8_lossy(empty)
        );
    }

    // Leading whitespace must not change which branch is taken.
    assert!(matches!(
        parse_page::<Organization>(b"\n  [] ").unwrap(),
        PageBody::Array(_)
    ));
}

/// A malformed envelope must fail loudly rather than read as zero rows: the
/// caller cannot tell a truncated fleet from a complete one, and every
/// compliance number derived from it would be silently wrong. The message names
/// the shape that arrived, which is why the check is on the raw slice rather
/// than left to serde's type error.
#[test]
fn parse_page_rejects_a_malformed_envelope_and_a_non_json_body() {
    let err = parse_page::<Organization>(br#"{"results":"not-an-array","cursor":""}"#)
        .expect_err("a non-array `results` must error");
    assert!(err.to_string().contains("was not an array"), "{err}");

    let missing = parse_page::<Organization>(br#"{"cursor":"tok"}"#)
        .expect_err("an envelope with no `results` must error");
    assert!(
        missing.to_string().contains("missing `results`"),
        "{missing}"
    );

    // A proxy's HTML error page reached this path before as a `Value::String`.
    let html = parse_page::<Organization>(b"<html>gateway error</html>")
        .expect_err("a non-JSON body must error");
    assert!(
        html.to_string().contains("unexpected paginated body"),
        "{html}"
    );
}

/// The cursor advance and boundary de-dup read the id off the row itself now.
/// `Patch` genuinely has none — the feeds that carry it are cursor-enveloped —
/// and saying so is what keeps it out of the `after` branch.
#[test]
fn paged_rows_report_the_id_the_after_cursor_advances_by() {
    assert_eq!(
        Organization {
            id: 9,
            name: "Alpha".into()
        }
        .row_id(),
        Some(9)
    );
    // Every `Patch` field is optional, so an empty object is a valid one.
    let patch: Patch = serde_json::from_str("{}").unwrap();
    assert_eq!(patch.row_id(), None);
}

#[test]
fn next_cursor_reads_string() {
    assert_eq!(
        next_cursor(Some(&json!("abc"))).unwrap(),
        Some(PageCursor {
            name: "abc".to_string(),
            offset: None
        })
    );
    assert_eq!(next_cursor(Some(&json!(""))).unwrap(), None);
}

#[test]
fn next_cursor_reads_object_name() {
    let v = json!({ "name": "tok-42", "offset": 500, "count": 500 });
    assert_eq!(
        next_cursor(Some(&v)).unwrap(),
        Some(PageCursor {
            name: "tok-42".to_string(),
            offset: Some(500)
        })
    );
    // An explicitly empty name is a real end-of-pages signal.
    let done = json!({ "name": "", "offset": 1000 });
    assert_eq!(next_cursor(Some(&done)).unwrap(), None);
}

/// The position is what separates an advancing scan from a stalled one, so it
/// has to survive into the value the loop compares. NinjaOne keys the scan
/// server-side by `name` and moves `offset`, so a cursor read as its name alone
/// compares equal on page 2 and stops the fetch there.
#[test]
fn an_object_cursor_carries_its_offset_into_the_progress_check() {
    let page1 = next_cursor(Some(&json!({ "name": "scan-7", "offset": 5000 })))
        .unwrap()
        .expect("a live cursor");
    let page2 = next_cursor(Some(&json!({ "name": "scan-7", "offset": 10000 })))
        .unwrap()
        .expect("a live cursor");
    assert_eq!(page1.name, page2.name, "the handle is stable by design");
    assert_ne!(page1, page2, "an advancing offset must read as progress");

    // A genuinely stalled scan — same handle, same position — must still compare
    // equal, so the loop-prevention guard keeps working.
    let stalled = next_cursor(Some(&json!({ "name": "scan-7", "offset": 5000 })))
        .unwrap()
        .expect("a live cursor");
    assert_eq!(page1, stalled);

    // A bare string has no position to advance, so the name carries it alone.
    let bare = next_cursor(Some(&json!("tok"))).unwrap();
    assert_eq!(bare, next_cursor(Some(&json!("tok"))).unwrap());
    assert_ne!(bare, next_cursor(Some(&json!("tok-2"))).unwrap());
}

#[test]
fn next_cursor_none_when_absent() {
    assert_eq!(next_cursor(None).unwrap(), None);
    assert_eq!(next_cursor(Some(&json!(null))).unwrap(), None);
}

/// A cursor shape this client cannot read is not "finished".
///
/// `next_cursor` is only consulted after a page that returned rows (the
/// caller stops on an empty page first), so treating an unreadable cursor as
/// end-of-pages ended the fetch mid-fleet and handed back a partial result
/// that looked complete — every compliance percentage computed from it would
/// be wrong, with nothing to indicate why. The sibling `results` handling has
/// always bailed loudly on a malformed envelope; this now matches it.
#[test]
fn an_unreadable_cursor_is_an_error_not_a_silent_end_of_pages() {
    for shape in [
        json!({ "offset": 0 }),
        json!({ "name": 42 }),
        json!(7),
        json!([]),
        json!(true),
    ] {
        assert!(
            next_cursor(Some(&shape)).is_err(),
            "cursor {shape} should be reported, not read as end-of-pages"
        );
    }
}

#[tokio::test]
async fn organizations_paginate_across_cursor_envelope() {
    use crate::auth::AuthState;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // Page 1 (no cursor yet) returns a nested cursor object.
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "id": 1, "name": "Alpha" }],
            "cursor": { "name": "tok-2", "offset": 1, "count": 1 }
        })))
        .mount(&server)
        .await;

    // Page 2 (cursor=tok-2) returns an empty cursor name → stop.
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations"))
        .and(query_param("cursor", "tok-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "id": 2, "name": "Beta" }],
            "cursor": { "name": "" }
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);

    let orgs = client.organizations().await.expect("organizations call");
    let names: Vec<_> = orgs.into_iter().map(|o| o.name).collect();
    assert_eq!(names, vec!["Alpha", "Beta"]);
}

#[tokio::test]
async fn non_array_results_envelope_is_an_error_not_a_truncated_fleet() {
    use crate::auth::AuthState;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // A `results` that isn't an array must fail, not be read as an empty page.
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": "not-an-array",
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);

    let err = client
        .fleet_os_patches(None, None, None)
        .await
        .expect_err("a non-array results envelope must error");
    assert!(
        err.to_string().contains("was not an array"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn patch_queries_request_the_reporting_page_size_and_follow_the_cursor() {
    use crate::auth::AuthState;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // The /queries/* fetchers must request the larger reporting page size.
    // Page 1 (no cursor) returns fewer rows than the requested page size but a
    // live cursor — proving the cursor (not the page length) drives paging, so
    // an API that caps the page below REPORTING_PAGE_SIZE still returns every
    // row instead of stopping after the first short page.
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param("pageSize", REPORTING_PAGE_SIZE.to_string()))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "id": 1, "name": "KB1" }],
            "cursor": "tok-2"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param("pageSize", REPORTING_PAGE_SIZE.to_string()))
        .and(query_param("cursor", "tok-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "id": 2, "name": "KB2" }],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);

    let patches = client
        .fleet_os_patches(None, None, None)
        .await
        .expect("os patches call");
    assert_eq!(
        patches.len(),
        2,
        "must follow the cursor past the first page"
    );
}

/// The whole third-party undercount, in one fixture.
///
/// NinjaOne keys a `/queries/*` scan server-side by a **stable** cursor `name`
/// and advances the `offset` beside it — `cursor` is the only paging parameter
/// the endpoint accepts, so the position cannot travel any other way. Reading
/// that cursor as its name alone made page 2 compare equal to page 1, which
/// tripped the forward-progress guard and returned two pages as if they were the
/// whole feed. Every other cursor fixture in this file changes the name per page,
/// which is exactly why nothing caught it: on a short OS feed the guard is never
/// reached, while a six-figure third-party feed was cut to 2 x `pageSize`.
///
/// The rows are real `DeviceSoftwarePatch` bodies, so this also pins the wire
/// shape: `title` -> `name`, `impact` -> `severity`, and no `kbNumber` anywhere.
#[tokio::test]
async fn a_stable_cursor_name_with_an_advancing_offset_is_progress_not_a_stall() {
    use crate::auth::AuthState;
    use crate::model::software_patch_json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// A server whose cursor name never changes, because the scan it names is
    /// server-side state rather than a per-page token.
    struct StableNameCursor {
        served: AtomicUsize,
    }

    impl Respond for StableNameCursor {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            let n = self.served.fetch_add(1, Ordering::SeqCst);
            let cursor = match n {
                // Two live pages, then a terminal empty name.
                0 | 1 => json!({
                    "name": "scan-7",
                    "offset": (n + 1) * 5000,
                    "count": 5000,
                }),
                _ => json!({ "name": "", "offset": 15000 }),
            };
            ResponseTemplate::new(200).set_body_json(json!({
                "results": [software_patch_json(
                    (n + 1) as i64,
                    "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
                    "Google Chrome 141.0.7390.55",
                    "RECOMMENDED",
                    "APPROVED",
                )],
                "cursor": cursor,
            }))
        }
    }

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/software-patches"))
        .respond_with(StableNameCursor {
            served: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);

    let patches = client
        .fleet_software_patches(None, None, None)
        .await
        .expect("software patches call");

    assert_eq!(
        patches.len(),
        3,
        "a stable cursor name whose offset advances must keep paging; \
         stopping at 2 pages is the third-party undercount"
    );
    // The vendor's software keys, through the aliases that are the only ones
    // that bind: `title` and `impact`. No fixture exercised these before.
    assert_eq!(
        patches[0].name.as_deref(),
        Some("Google Chrome 141.0.7390.55")
    );
    assert_eq!(
        patches[0].severity_enum(),
        crate::model::Severity::Recommended
    );
    assert_eq!(patches[0].status.as_deref(), Some("APPROVED"));
    assert_eq!(patches[0].device_id, Some(1));
    // Third-party records carry no KB, and no product version or vendor field
    // exists on the schema at all — the display name is the title alone.
    assert!(patches[0].kb_number.is_none());
    assert!(patches[0].version.is_none());
    assert!(patches[0].product_vendor.is_none());
}

/// The guard still has to stop a genuinely stalled scan, or an endpoint that
/// echoes its cursor back unchanged loops forever, re-fetching the same rows.
/// It reports rather than returning, because the rows in hand are a partial
/// fleet and every count derived from them would be understated silently.
#[tokio::test]
async fn a_cursor_that_never_advances_is_an_error_not_a_short_read() {
    use crate::auth::AuthState;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/software-patches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "deviceId": 1, "title": "7-Zip 24.09" }],
            "cursor": { "name": "scan-7", "offset": 5000 },
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);

    let err = client
        .fleet_software_patches(None, None, None)
        .await
        .expect_err("a stalled cursor must be reported, not returned as a short read");
    assert!(
        err.to_string().contains("cursor it was handed"),
        "unexpected error: {err}"
    );
}

/// The echo check compared only with the previous cursor, so one that oscillates
/// A -> B -> A never matched it and the scan re-fetched the same two pages forever,
/// growing the row vector without bound. A revisited cursor is the same stall.
#[tokio::test]
async fn a_cursor_that_oscillates_is_an_error_not_an_endless_scan() {
    use crate::auth::AuthState;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // Each page answers once, so a loop that re-requests `scan-a` hits a 404 and
    // fails this test instead of hanging it.
    let page = |next: &str| {
        ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "deviceId": 1, "title": "7-Zip 24.09" }],
            "cursor": next,
        }))
    };
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/software-patches"))
        .and(query_param_is_missing("cursor"))
        .respond_with(page("scan-a"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/software-patches"))
        .and(query_param("cursor", "scan-a"))
        .respond_with(page("scan-b"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/software-patches"))
        .and(query_param("cursor", "scan-b"))
        .respond_with(page("scan-a"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);

    let err = client
        .fleet_software_patches(None, None, None)
        .await
        .expect_err("a revisited cursor must be reported, not paged forever");
    assert!(
        err.to_string().contains("already handed out"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn retries_with_refreshed_token_after_401() {
    use crate::auth::AuthState;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // The cached (but server-invalidated) token is rejected.
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .and(header("authorization", "Bearer stale-token"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    // The 401 must drive a refresh that exchanges the refresh token for a new
    // access token (no refresh_token in the response → no keyring write).
    Mock::given(method("POST"))
        .and(path("/ws/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-token",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    // The retry must use the refreshed token, not the stale one.
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .and(header("authorization", "Bearer fresh-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": 7 }])))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded_refreshable(
        http.clone(),
        server.uri(),
        "stale-token",
        "refresh-abc",
        "client-1",
    );
    let client = NinjaApiClient::new(http, auth);

    let devices = client.devices(None, None).await.expect("devices call");
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, 7, "must retry with the refreshed token");
}

#[tokio::test]
async fn devices_send_df_and_bearer_token() {
    use crate::auth::AuthState;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // Bare-array response exercises the non-envelope branch of get_paginated.
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .and(query_param("df", "org = 5"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "systemName": "srv10", "nodeClass": "WINDOWS_SERVER" }
        ])))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);

    let devices = client
        .devices(Some("org = 5"), None)
        .await
        .expect("devices call");
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, 10);
}

#[tokio::test]
async fn devices_detailed_paginates_via_after_cursor() {
    use crate::auth::AuthState;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // Page 1: a full page (DEFAULT_PAGE_SIZE devices, ids 1..=500), no `after`.
    let page1: Vec<_> = (1..=DEFAULT_PAGE_SIZE as i64)
        .map(|i| json!({ "id": i }))
        .collect();
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .and(query_param_is_missing("after"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page1))
        .mount(&server)
        .await;

    // Page 2: after=<last id of page 1> returns a short page → stop.
    let page2: Vec<_> = (501..=503).map(|i| json!({ "id": i })).collect();
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .and(query_param("after", DEFAULT_PAGE_SIZE.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(page2))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);

    let devices = client.devices(None, None).await.expect("devices call");
    assert_eq!(
        devices.len(),
        DEFAULT_PAGE_SIZE as usize + 3,
        "must page past the first 500 instead of stopping"
    );
    assert_eq!(devices.first().unwrap().id, 1);
    assert_eq!(devices.last().unwrap().id, 503);
}

#[tokio::test]
async fn after_pagination_uses_max_id_and_dedupes_boundary() {
    use crate::auth::AuthState;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // Page 1: a full page whose ids descend (last id = 1, max id = 500). The
    // cursor must advance by the max (500), not the last (1), or an unsorted
    // endpoint would page from the wrong id and re-fetch / drop rows.
    let page1: Vec<_> = (1..=DEFAULT_PAGE_SIZE as i64)
        .rev()
        .map(|i| json!({ "id": i }))
        .collect();
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .and(query_param_is_missing("after"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page1))
        .mount(&server)
        .await;

    // Page 2 at after=500 re-includes id 500 (inclusive boundary) plus 501/502;
    // the duplicate must be dropped and the short page ends paging.
    let page2 = json!([{ "id": 500 }, { "id": 501 }, { "id": 502 }]);
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .and(query_param("after", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page2))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);

    let devices = client.devices(None, None).await.expect("devices call");
    assert_eq!(
        devices.len(),
        DEFAULT_PAGE_SIZE as usize + 2,
        "boundary row 500 must be de-duplicated"
    );
    let n500 = devices.iter().filter(|d| d.id == 500).count();
    assert_eq!(n500, 1, "id 500 must appear exactly once");
    assert!(devices.iter().any(|d| d.id == 502));
}

/// A client with a timeout short enough that a delayed mock always trips it.
fn timing_out_client(server: &wiremock::MockServer) -> NinjaApiClient {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_millis(150))
        .build()
        .expect("build client");
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    NinjaApiClient::new(http, auth)
}

#[tokio::test]
async fn post_timeout_is_not_replayed() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/1/reboot/NORMAL"))
        .respond_with(ResponseTemplate::new(204).set_delay(Duration::from_secs(30)))
        // The whole point: exactly one attempt reaches the server. A replay
        // could reboot the device twice.
        .expect(1)
        .mount(&server)
        .await;

    let err = timing_out_client(&server)
        .device_reboot(1, crate::model::RebootMode::Normal, "patching")
        .await
        .expect_err("a timed-out reboot must surface as an error");
    assert!(
        err.to_string().contains("may already"),
        "the operator must be told the action may have landed, got: {err}"
    );
    assert!(
        is_outcome_unknown(&err),
        "a timed-out acting POST is an unknown outcome, not a rejection: {err}"
    );
}

/// The typed marker, not the message, is what the dispatch site classifies on —
/// so it has to survive whatever context a caller layers on top, and a message
/// that merely mentions the phrase must not count.
#[test]
fn the_outcome_unknown_marker_survives_added_context() {
    let wrapped = anyhow::Error::new(std::io::Error::other("reset"))
        .context(OutcomeUnknown("the connection failed".into()))
        .context("dispatching reboot to srv-1");
    assert!(is_outcome_unknown(&wrapped), "{wrapped:#}");

    let rejected = anyhow::anyhow!("the action may already be queued (said the 400 body)");
    assert!(!is_outcome_unknown(&rejected));
}

/// The connection is accepted, the request read, and the socket closed with no
/// response: the POST reached the server, so it may have been queued. It used to
/// fall to the generic `http send` arm and be reported as a definite failure.
#[tokio::test]
async fn post_connection_lost_in_flight_is_unknown_and_not_replayed() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncReadExt as _;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let accepted = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            drop(sock);
        }
    });

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), format!("http://{addr}"), "test-token");
    let err = NinjaApiClient::new(http, auth)
        .device_reboot(1, crate::model::RebootMode::Normal, "patching")
        .await
        .expect_err("a dropped connection must surface");
    assert!(is_outcome_unknown(&err), "{err:#}");
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "never replayed");
}

/// A connect failure never reached the server, so it is a plain failure.
#[tokio::test]
async fn post_connect_failure_is_not_an_unknown_outcome() {
    // Bind then drop, so the port is (almost certainly) refusing connections.
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("bind");
    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), format!("http://{addr}"), "test-token");
    let err = NinjaApiClient::new(http, auth)
        .device_patch_scan(1, crate::model::PatchType::Os)
        .await
        .expect_err("nothing is listening");
    assert!(!is_outcome_unknown(&err), "{err:#}");
}

#[test]
fn retry_after_is_capped() {
    assert_eq!(
        retry_for(
            StatusCode::TOO_MANY_REQUESTS,
            ReplaySafety::Idempotent,
            0,
            Some(MAX_RETRY_AFTER_SECS)
        ),
        Retry::Wait(Duration::from_secs(MAX_RETRY_AFTER_SECS))
    );
    assert_eq!(
        retry_for(
            StatusCode::TOO_MANY_REQUESTS,
            ReplaySafety::ActOnce,
            0,
            Some(86_400)
        ),
        Retry::Wait(Duration::from_secs(MAX_RETRY_AFTER_SECS)),
        "a server-controlled header must not park a dispatch for a day"
    );
}

/// The HTTP-date form of `Retry-After` used to fall back to the 5 s default, so a
/// server asking for 30 s was hit again after 5. Both forms are read now, and the
/// date form is held to the same cap.
#[test]
fn retry_after_reads_the_http_date_form_too() {
    let now = chrono::DateTime::parse_from_rfc2822("Fri, 23 Oct 2026 12:00:00 GMT")
        .expect("fixed now")
        .with_timezone(&chrono::Utc);
    assert_eq!(parse_retry_after("30", now), Some(30));
    assert_eq!(parse_retry_after(" 7 ", now), Some(7));
    assert_eq!(
        parse_retry_after("Fri, 23 Oct 2026 12:00:30 GMT", now),
        Some(30)
    );
    assert_eq!(
        parse_retry_after(
            "Fri, 23 Oct 2026 12:00:30 GMT",
            now + chrono::Duration::milliseconds(500)
        ),
        Some(30),
        "a part second rounds up, so the retry never lands early"
    );
    assert_eq!(
        parse_retry_after("Fri, 23 Oct 2026 11:59:00 GMT", now),
        Some(0),
        "a date already past means now"
    );
    assert_eq!(
        parse_retry_after("Sat, 24 Oct 2026 12:00:00 GMT", now),
        Some(MAX_RETRY_AFTER_SECS),
        "a server-controlled date is capped like the seconds form"
    );
    assert_eq!(parse_retry_after("soon", now), None);
}

#[test]
fn the_shared_pause_counts_down_to_none() {
    let now = Instant::now();
    assert_eq!(pause_remaining(None, now), None);
    assert_eq!(
        pause_remaining(Some(now + Duration::from_secs(2)), now),
        Some(Duration::from_secs(2))
    );
    assert_eq!(pause_remaining(Some(now), now), None);
    assert_eq!(
        pause_remaining(Some(now), now + Duration::from_secs(1)),
        None
    );
}

/// A second 429 with a shorter `Retry-After` must not lift the wait the first one
/// asked for, and the guarded clone a dispatch uses must see the same pause.
#[test]
fn a_later_429_never_shortens_the_pause_and_clones_share_it() {
    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), "http://unused.invalid".into(), "t");
    let client = NinjaApiClient::new(http, auth);
    client.pause_until(Instant::now() + Duration::from_secs(60));
    client.pause_until(Instant::now() + Duration::from_secs(1));
    assert!(client.pause_left().expect("paused") > Duration::from_secs(50));

    let guarded = client.with_send_guard(Arc::new(|| true));
    assert!(
        guarded
            .pause_left()
            .expect("the guarded clone is paused too")
            > Duration::from_secs(50)
    );
}

/// Fixed 2/4/8 s delays put every leg that failed together back on the wire
/// together. The jitter only ever lengthens a delay, by at most a quarter.
#[test]
fn backoff_is_jittered_by_at_most_a_quarter() {
    assert_eq!(jittered_backoff(1, 0.0), Duration::from_secs(2));
    assert_eq!(
        jittered_backoff(3, MAX_BACKOFF_JITTER),
        Duration::from_secs(10)
    );
    for attempt in 1..=MAX_RETRIES {
        let base = Duration::from_secs(2u64.pow(attempt as u32));
        let samples: Vec<Duration> = (0..200).map(|_| backoff(attempt)).collect();
        for d in &samples {
            assert!(
                *d >= base && *d <= base.mul_f64(1.0 + MAX_BACKOFF_JITTER),
                "attempt {attempt}: {d:?}"
            );
        }
        assert!(
            samples.iter().any(|d| *d != samples[0]),
            "attempt {attempt}: 200 identical delays are not jitter"
        );
    }
    for _ in 0..200 {
        assert!(rate_limit_jitter() <= Duration::from_millis(MAX_RATE_LIMIT_JITTER_MS));
    }
}

/// Answers `first` to the first request and `rest` to every later one, noting
/// when each arrived.
struct Arrivals {
    at: Arc<Mutex<Vec<Instant>>>,
    first: wiremock::ResponseTemplate,
    rest: wiremock::ResponseTemplate,
}

impl wiremock::Respond for Arrivals {
    fn respond(&self, _: &wiremock::Request) -> wiremock::ResponseTemplate {
        let mut at = self.at.lock().expect("arrivals");
        at.push(Instant::now());
        if at.len() == 1 {
            self.first.clone()
        } else {
            self.rest.clone()
        }
    }
}

/// Waits until a 429 on `client` (or any clone) has set the shared pause.
async fn until_paused(client: &NinjaApiClient) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.pause_left().is_none() {
        assert!(Instant::now() < deadline, "no 429 ever paused the client");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A 429 is the tenant's rate limit, but only the request that drew it used to
/// wait while every other leg kept sending. A request started on a clone during
/// the pause — here the guarded clone a dispatch uses — must not reach the server
/// until the pause is over.
#[tokio::test]
async fn a_429_pauses_every_request_the_client_sends() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let a_hits = Arc::new(Mutex::new(Vec::new()));
    let b_hits = Arc::new(Mutex::new(Vec::new()));
    let ok = ResponseTemplate::new(200).set_body_json(json!({}));
    Mock::given(method("GET"))
        .and(path("/api/v2/a"))
        .respond_with(Arrivals {
            at: Arc::clone(&a_hits),
            first: ResponseTemplate::new(429).insert_header("Retry-After", "1"),
            rest: ok.clone(),
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/b"))
        .respond_with(Arrivals {
            at: Arc::clone(&b_hits),
            first: ok.clone(),
            rest: ok,
        })
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);
    let a = {
        let client = client.clone();
        tokio::spawn(async move { client.get_json::<Value>("/a", &[]).await })
    };
    until_paused(&client).await;
    client
        .with_send_guard(Arc::new(|| true))
        .get_json::<Value>("/b", &[])
        .await
        .expect("B goes through once the pause is over");
    a.await
        .expect("join")
        .expect("A is retried through to success");

    let a_hits = a_hits.lock().expect("arrivals").clone();
    let b_first = b_hits.lock().expect("arrivals")[0];
    assert_eq!(a_hits.len(), 2, "A: the 429, then the retry");
    assert!(
        b_first.duration_since(a_hits[0]) >= Duration::from_secs(1),
        "B reached the server {:?} after A's 429, inside the 1 s it asked for",
        b_first.duration_since(a_hits[0])
    );
    assert!(a_hits[1].duration_since(a_hits[0]) >= Duration::from_secs(1));
}

/// A tenant switch during a 429 wait used to send the retry to the old host with
/// the new tenant's freshly refreshed token, and the old host's 401 then
/// invalidated it. The retry is abandoned instead: the old host sees nothing more,
/// the new one sees no refresh, and an action stopped this way is "not sent".
#[tokio::test]
async fn a_tenant_switch_during_a_429_wait_abandons_the_request() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let host_a = MockServer::start().await;
    let host_b = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/a"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .expect(1)
        .mount(&host_a)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/2/patch/os/scan"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .expect(1)
        .mount(&host_a)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), host_a.uri(), "token-a");
    let client = NinjaApiClient::new(http, auth.clone());
    let read = {
        let client = client.clone();
        tokio::spawn(async move { client.get_json::<Value>("/a", &[]).await })
    };
    let action = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .device_patch_scan(2, crate::model::PatchType::Os)
                .await
        })
    };
    until_paused(&client).await;
    // Both requests have drawn their 429 once the server has seen two.
    let deadline = Instant::now() + Duration::from_secs(5);
    while host_a.received_requests().await.unwrap_or_default().len() < 2 {
        assert!(Instant::now() < deadline, "both requests reach host A once");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    auth.apply_settings(host_b.uri(), Some("client-b".into()), 0, false);

    let read_err = read
        .await
        .expect("join")
        .expect_err("the read is abandoned");
    assert!(crate::auth::is_tenant_moved(&read_err), "{read_err:#}");
    let action_err = action
        .await
        .expect("join")
        .expect_err("the action is not sent");
    assert!(crate::auth::is_tenant_moved(&action_err), "{action_err:#}");
    assert!(is_send_refused(&action_err), "{action_err:#}");
    assert!(!is_outcome_unknown(&action_err), "{action_err:#}");

    host_a.verify().await;
    assert!(
        host_b
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "nothing — not even a token refresh — goes to the tenant switched to"
    );
}

/// A resolver that never answers, so the connect budget runs out without any
/// network: an unroutable address can fail fast with "network unreachable" on a
/// CI runner instead of timing out, which would test nothing.
struct NeverResolves(Arc<std::sync::atomic::AtomicUsize>);

impl reqwest::dns::Resolve for NeverResolves {
    fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}

/// A connect timeout means no connection was ever made, so an acting POST that
/// hits one is a plain failure: not replayed, and not an unknown outcome that would
/// send the operator to check a device the request never reached.
#[tokio::test]
async fn a_connect_timeout_on_an_acting_post_is_a_plain_failure_and_not_replayed() {
    let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let http = reqwest::Client::builder()
        .no_proxy()
        .dns_resolver(Arc::new(NeverResolves(Arc::clone(&lookups))))
        .connect_timeout(Duration::from_millis(300))
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build client");
    let auth = AuthState::seeded(http.clone(), "http://ninja.invalid".into(), "test-token");
    let err = NinjaApiClient::new(http, auth)
        .device_reboot(1, crate::model::RebootMode::Normal, "patching")
        .await
        .expect_err("nothing ever connects");

    let cause = err
        .chain()
        .find_map(|e| e.downcast_ref::<reqwest::Error>())
        .expect("a reqwest error underneath");
    assert!(cause.is_connect() && cause.is_timeout(), "{cause:?}");
    assert!(!is_outcome_unknown(&err), "{err:#}");
    assert_eq!(
        lookups.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "an action is never re-sent on a connect failure"
    );
}
#[tokio::test]
async fn get_timeout_is_replayed() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;

    // Reads stay retried; only the acting POSTs changed. This guards against
    // the idempotency fix accidentally disabling retries everywhere.
    let _ = timing_out_client(&server).devices(None, None).await;
    let attempts = server.received_requests().await.unwrap_or_default().len();
    assert!(
        attempts > 1,
        "an idempotent GET must still retry; saw {attempts} attempt(s)"
    );
}

/// A gateway 502 partway through a long reporting pull must not fail the whole
/// fetch — but it must not re-send the cursor either. NinjaOne keeps the scan's
/// position server-side under the cursor's stable name, so a gateway that failed
/// after the backend advanced it would answer the replay with the next page. This
/// test used to pass with a replay only because its cursor changed name per page,
/// which the real one never does; the scan now starts over instead.
#[tokio::test]
async fn a_5xx_on_a_cursor_page_restarts_the_scan_instead_of_replaying_the_cursor() {
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // Every scan opens with one row and a live cursor.
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "id": 1, "kbNumber": "KB1" }],
            "cursor": { "name": "scan", "offset": 1, "count": 2 }
        })))
        .mount(&server)
        .await;

    // The first scan's cursor page fails once with a 502.
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param("cursor", "scan"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param("cursor", "scan"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "id": 2, "kbNumber": "KB2" }],
            "cursor": { "name": "", "offset": 2, "count": 2 }
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let patches = NinjaApiClient::new(http, auth)
        .fleet_os_patches(None, None, None)
        .await
        .expect("a transient 502 must not fail the whole fetch");
    assert_eq!(
        patches.len(),
        2,
        "the restarted scan's rows only, each once"
    );
    assert_eq!(
        request_cursors(&server).await,
        [None, Some("scan".into()), None, Some("scan".into())],
        "the 502 is followed by a fresh scan, not a replay of the cursor"
    );
}

/// The request that opens a scan carries no cursor, so there is no server-side
/// position for a replay to skip past: a 5xx there is retried in place as before.
#[tokio::test]
async fn a_5xx_on_the_opening_page_is_retried_in_place() {
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "id": 1, "kbNumber": "KB1" }],
            "cursor": { "name": "scan", "offset": 1, "count": 2 }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param("cursor", "scan"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "id": 2, "kbNumber": "KB2" }],
            "cursor": { "name": "", "offset": 2, "count": 2 }
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let patches = NinjaApiClient::new(http, auth)
        .fleet_os_patches(None, None, None)
        .await
        .expect("a transient 502 on the opening page is retried");
    assert_eq!(patches.len(), 2);
    assert_eq!(
        request_cursors(&server).await,
        [None, None, Some("scan".into())]
    );
}

#[tokio::test]
async fn post_5xx_is_not_retried() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // A 5xx on an acting POST is ambiguous — the gateway may have failed after
    // the job reached the device queue — so it must stay `ActOnce`. Exactly one
    // attempt may reach the server.
    Mock::given(method("POST"))
        .and(path("/api/v2/device/3/patch/os/apply"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let err = NinjaApiClient::new(http, auth)
        .device_patch_apply(3, crate::model::PatchType::Os)
        .await
        .expect_err("a 5xx on an acting POST must not be replayed");
    assert!(
        is_outcome_unknown(&err),
        "a 5xx on an acting POST may have queued the job, so it is unknown: {err}"
    );
}

/// A 4xx is NinjaOne refusing the request, so nothing was queued and the job is
/// a definite failure — it must not be dressed up as an unknown outcome.
#[tokio::test]
async fn post_4xx_is_a_rejection_not_an_unknown_outcome() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/3/patch/os/apply"))
        .respond_with(ResponseTemplate::new(400).set_body_string("not applicable"))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let err = NinjaApiClient::new(http, auth)
        .device_patch_apply(3, crate::model::PatchType::Os)
        .await
        .expect_err("a 400 must surface");
    assert!(!is_outcome_unknown(&err), "{err}");
}

/// The server said 2xx — the action was accepted — but the body is not what its
/// content type promised. That is not a rejection either.
#[tokio::test]
async fn an_unreadable_2xx_body_on_an_acting_post_is_an_unknown_outcome() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/3/script/run"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{not json", "application/json"))
        .expect(1)
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let err = NinjaApiClient::new(http, auth)
        .run_script(
            3,
            &crate::api::actions::ScriptRef::Script { id: 1 },
            "",
            "system",
        )
        .await
        .expect_err("an undecodable body must surface");
    assert!(is_outcome_unknown(&err), "{err:#}");
}

#[tokio::test]
async fn post_429_is_still_replayed() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // A 429 is a gateway rejection — the request never reached the device
    // queue, so replaying it cannot double-execute anything.
    Mock::given(method("POST"))
        .and(path("/api/v2/device/2/patch/os/scan"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/2/patch/os/scan"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    NinjaApiClient::new(http, auth)
        .device_patch_scan(2, crate::model::PatchType::Os)
        .await
        .expect("a 429 must be retried through to success");
}

/// A redirect is never followed. reqwest's default policy re-sends the body on a
/// 307/308, so an acting POST would reach whatever host `Location` named — a
/// second dispatch the operator never confirmed.
#[tokio::test]
async fn an_acting_post_is_not_replayed_to_a_redirect_target() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let elsewhere = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&elsewhere)
        .await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(307).insert_header(
            "Location",
            format!("{}/elsewhere?sig=secret", elsewhere.uri()),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let http = crate::state::build_http_client().expect("build client");
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let err = NinjaApiClient::new(http, auth)
        .post_action("/device/1/reboot/NORMAL", None)
        .await
        .expect_err("a redirect is an error, not a success");
    assert_redirect_named(&format!("{err:#}"), &elsewhere.uri());
}

/// The token grant shares the client, so a 307 from `/ws/oauth/token` must not
/// re-POST the refresh token (and, for a Web client, the secret) to `Location`.
#[tokio::test]
async fn a_token_grant_is_not_replayed_to_a_redirect_target() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let elsewhere = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&elsewhere)
        .await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/devices-detailed"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ws/oauth/token"))
        .respond_with(ResponseTemplate::new(307).insert_header(
            "Location",
            format!("{}/ws/oauth/token?sig=secret", elsewhere.uri()),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let http = crate::state::build_http_client().expect("build client");
    let auth = AuthState::seeded_refreshable(
        http.clone(),
        server.uri(),
        "stale-token",
        "refresh-abc",
        "client-1",
    );
    let err = NinjaApiClient::new(http, auth)
        .devices(None, None)
        .await
        .expect_err("a redirected grant is a failed refresh");
    assert_redirect_named(&format!("{err:#}"), &elsewhere.uri());
}

/// Without redirects followed, an Instance that redirects (an old regional host,
/// `http` -> `https`) would fail as a bare 3xx with an empty body. The error names
/// the target's scheme and host and what to change — and never its path or query.
fn assert_redirect_named(err: &str, origin: &str) {
    assert!(err.contains("307"), "{err}");
    assert!(
        err.contains(&format!(
            "NinjaOne redirected to {origin}; set Instance in Settings to that address"
        )),
        "{err}"
    );
    assert!(
        !err.contains("sig=secret"),
        "the Location query leaked: {err}"
    );
    assert!(
        !err.contains("/elsewhere"),
        "the Location path leaked: {err}"
    );
}

/// A scripted `/queries/*` server: one response per request, in order, so a test
/// can state exactly which pages a scan was handed. The row is a software patch
/// built from the vendor's keys; only its count per page matters here.
struct ScriptedPages {
    pages: Vec<serde_json::Value>,
    served: std::sync::atomic::AtomicUsize,
}

impl ScriptedPages {
    fn new(pages: Vec<serde_json::Value>) -> Self {
        Self {
            pages,
            served: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl wiremock::Respond for ScriptedPages {
    fn respond(&self, _: &wiremock::Request) -> wiremock::ResponseTemplate {
        let n = self
            .served
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.pages.get(n) {
            Some(body) => wiremock::ResponseTemplate::new(200).set_body_json(body),
            // Past the script: a loop that keeps asking fails loudly here rather
            // than hanging the test.
            None => wiremock::ResponseTemplate::new(404),
        }
    }
}

fn rows(n: usize) -> serde_json::Value {
    let rows: Vec<_> = (0..n)
        .map(|i| {
            crate::model::software_patch_json(
                i as i64,
                "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
                "7-Zip 24.09",
                "RECOMMENDED",
                "APPROVED",
            )
        })
        .collect();
    serde_json::Value::Array(rows)
}

/// Runs one scan against `pages` and returns its result plus the `cursor` each
/// request carried (`None` for a request that opened a scan).
async fn scan_software(pages: Vec<serde_json::Value>) -> (Result<Vec<Patch>>, Vec<Option<String>>) {
    use crate::auth::AuthState;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/software-patches"))
        .respond_with(ScriptedPages::new(pages))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let client = NinjaApiClient::new(http, auth);
    let result = client.fleet_software_patches(None, None, None).await;
    (result, request_cursors(&server).await)
}

/// The field case: the cursor announced 6721 rows, the first page held 5000 and the
/// next request answered with none. Taking that as the end ran every rollup on
/// three quarters of the OS feed, and asking the same cursor again was answered
/// `400 Invalid cursor.` So the scan starts over without a cursor, and only the
/// fresh scan's rows are kept.
#[tokio::test]
async fn an_empty_page_short_of_the_announced_count_restarts_the_scan() {
    let (result, cursors) = scan_software(vec![
        json!({ "results": rows(2), "cursor": { "name": "broken", "offset": 2, "count": 3 } }),
        json!({ "results": [], "cursor": { "name": "broken", "offset": 2, "count": 3 } }),
        json!({ "results": rows(2), "cursor": { "name": "fresh", "offset": 2, "count": 3 } }),
        json!({ "results": rows(1), "cursor": { "name": "", "offset": 3, "count": 3 } }),
    ])
    .await;
    assert_eq!(
        result.expect("the restarted scan completes").len(),
        3,
        "the broken scan's rows are dropped, not added to the fresh one's"
    );
    assert_eq!(
        cursors,
        [None, Some("broken".into()), None, Some("fresh".into())],
        "the restart opens a new scan instead of re-sending the broken cursor"
    );
}

/// A scan whose cursor keeps breaking is a stall, reported like the other stalls:
/// the rows in hand are a partial fleet.
#[tokio::test]
async fn an_empty_page_that_keeps_breaking_the_scan_is_an_error() {
    let first =
        json!({ "results": rows(2), "cursor": { "name": "scan", "offset": 2, "count": 3 } });
    let empty = json!({ "results": [], "cursor": { "name": "scan", "offset": 2, "count": 3 } });
    let pages = (0..=SCAN_RESTARTS)
        .flat_map(|_| [first.clone(), empty.clone()])
        .collect();
    let (result, cursors) = scan_software(pages).await;
    let err = result.expect_err("a persistent short read must be reported");
    assert!(
        err.to_string().contains("2 of the 3 rows"),
        "unexpected error: {err}"
    );
    assert_eq!(cursors.len(), 2 * (1 + SCAN_RESTARTS as usize));
}

/// An empty page once every announced row has arrived is the end it always was —
/// no restart, no error — as is one from an endpoint that announces no count.
#[tokio::test]
async fn an_empty_page_at_or_without_an_announced_count_ends_the_scan() {
    let (at_count, requests) = scan_software(vec![
        json!({ "results": rows(2), "cursor": { "name": "scan", "offset": 2, "count": 2 } }),
        json!({ "results": [], "cursor": { "name": "scan", "offset": 2, "count": 2 } }),
    ])
    .await;
    assert_eq!(at_count.expect("ends").len(), 2);
    assert_eq!(requests.len(), 2, "no restart when the count is met");

    let (no_count, requests) = scan_software(vec![
        json!({ "results": rows(2), "cursor": "scan" }),
        json!({ "results": [], "cursor": "scan" }),
    ])
    .await;
    assert_eq!(no_count.expect("ends").len(), 2);
    assert_eq!(requests.len(), 2, "no restart when nothing was announced");
}

/// A terminal cursor under fewer rows than announced is the server ending the scan,
/// which a patch installed mid-scan legitimately produces; it is logged, not failed.
#[tokio::test]
async fn an_exhausted_cursor_short_of_the_count_is_a_warning_not_an_error() {
    let (result, _) = scan_software(vec![json!({
        "results": rows(2),
        "cursor": { "name": "", "offset": 2, "count": 5 },
    })])
    .await;
    assert_eq!(result.expect("the server ended the scan").len(), 2);
}

#[test]
fn cursor_count_reads_only_a_non_negative_integer() {
    assert_eq!(cursor_count(Some(&json!({ "count": 6721 }))), Some(6721));
    assert_eq!(cursor_count(Some(&json!({ "count": -1 }))), None);
    assert_eq!(cursor_count(Some(&json!({ "count": "6721" }))), None);
    assert_eq!(cursor_count(Some(&json!({ "name": "scan" }))), None);
    assert_eq!(cursor_count(Some(&json!("scan"))), None);
    assert_eq!(cursor_count(None), None);
}

/// How [`LiveScan`] loses the response to a scan's first cursor page.
#[derive(Clone, Copy)]
enum LostPage {
    /// Answered only after the client has given up.
    TimedOut,
    /// Answered with this gateway error.
    Status(u16),
}

/// The rows every [`LiveScan`] announces in its cursor's `count`.
const LIVE_COUNT: i64 = 6;

/// A `/queries/*` server that keeps each scan's position server-side, keyed by a
/// stable cursor name, the way NinjaOne does. A request with no `cursor` opens scan
/// N (`scan-N`, `count` 6) and serves its first two rows; every cursor request
/// serves the next two and advances the offset *before* answering. The first cursor
/// request of a scan advances like any other, but its response is lost — on the
/// first scan only, or on every scan — so re-sending that cursor is answered with
/// the page after the lost one.
///
/// [`ScriptedPages`] cannot show the bug: it answers by request count, not by
/// where the scan stands, so a replay there gets the very page that was lost.
struct LiveScan {
    lost: LostPage,
    every_scan: bool,
    state: std::sync::Mutex<LiveScanState>,
}

#[derive(Default)]
struct LiveScanState {
    scans: u32,
    offset: i64,
    cursor_pages: u32,
}

impl wiremock::Respond for LiveScan {
    fn respond(&self, req: &wiremock::Request) -> wiremock::ResponseTemplate {
        let mut s = self.state.lock().expect("scan state");
        if req.url.query_pairs().any(|(k, _)| k == "cursor") {
            s.cursor_pages += 1;
        } else {
            s.scans += 1;
            s.offset = 0;
            s.cursor_pages = 0;
        }
        let start = s.offset;
        let end = (start + 2).min(LIVE_COUNT);
        s.offset = end;
        let name = if end < LIVE_COUNT {
            format!("scan-{}", s.scans)
        } else {
            String::new()
        };
        let rows: Vec<_> = (start..end)
            .map(|id| {
                crate::model::software_patch_json(
                    id,
                    "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
                    "7-Zip 24.09",
                    "RECOMMENDED",
                    "APPROVED",
                )
            })
            .collect();
        let page = wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "results": rows,
            "cursor": { "name": name, "offset": end, "count": LIVE_COUNT },
        }));
        if s.cursor_pages != 1 || !(self.every_scan || s.scans == 1) {
            return page;
        }
        match self.lost {
            LostPage::TimedOut => page.set_delay(Duration::from_secs(5)),
            LostPage::Status(code) => wiremock::ResponseTemplate::new(code),
        }
    }
}

/// The `cursor` each request `server` received carried, in order (`None` for a
/// request that opened a scan).
async fn request_cursors(server: &wiremock::MockServer) -> Vec<Option<String>> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| {
            r.url
                .query_pairs()
                .find(|(k, _)| k == "cursor")
                .map(|(_, v)| v.into_owned())
        })
        .collect()
}

/// Runs one software scan against a [`LiveScan`] and returns the device id of
/// every row it kept plus the `cursor` each request carried.
async fn scan_live(
    lost: LostPage,
    every_scan: bool,
) -> (Result<Vec<Option<i64>>>, Vec<Option<String>>) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/software-patches"))
        .respond_with(LiveScan {
            lost,
            every_scan,
            state: Default::default(),
        })
        .mount(&server)
        .await;
    let result = timing_out_client(&server)
        .fleet_software_patches(None, None, None)
        .await
        .map(|patches| patches.iter().map(|p| p.device_id).collect());
    (result, request_cursors(&server).await)
}

/// The cursor page's request reached NinjaOne, which advanced the scan, and then
/// the response was lost to the client's timeout. Re-sending the stable cursor name
/// is answered with the *next* page, so the lost page's rows — up to a whole
/// `pageSize` of them per retry — vanish without an error, and the shortfall only
/// shows as a warning at the end. The scan starts over instead.
#[tokio::test]
async fn a_lost_cursor_response_restarts_the_scan_instead_of_skipping_a_page() {
    let (result, cursors) = scan_live(LostPage::TimedOut, false).await;
    assert_eq!(
        result.expect("the restarted scan completes"),
        (0..LIVE_COUNT).map(Some).collect::<Vec<_>>(),
        "every row exactly once; a replayed cursor skips the lost page"
    );
    assert_eq!(
        cursors,
        [
            None,
            Some("scan-1".into()),
            None,
            Some("scan-2".into()),
            Some("scan-2".into())
        ],
        "the request after the lost page opens a new scan instead of re-sending the cursor"
    );
}

/// The same loss through a gateway: the backend advanced the scan and the 502 came
/// from in front of it.
#[tokio::test]
async fn a_5xx_cursor_response_restarts_the_scan_instead_of_skipping_a_page() {
    let (result, cursors) = scan_live(LostPage::Status(502), false).await;
    assert_eq!(
        result.expect("the restarted scan completes"),
        (0..LIVE_COUNT).map(Some).collect::<Vec<_>>()
    );
    assert_eq!(
        cursors,
        [
            None,
            Some("scan-1".into()),
            None,
            Some("scan-2".into()),
            Some("scan-2".into())
        ]
    );
}

/// A 2xx whose body cannot be read has still moved the scan on server-side, so it
/// is as lost as a timeout and restarts the scan rather than failing the query.
#[tokio::test]
async fn an_unreadable_cursor_page_restarts_the_scan() {
    let (result, cursors) = scan_software(vec![
        json!({ "results": rows(2), "cursor": { "name": "broken", "offset": 2, "count": 3 } }),
        json!({ "results": "truncated", "cursor": { "name": "broken", "offset": 4, "count": 3 } }),
        json!({ "results": rows(2), "cursor": { "name": "fresh", "offset": 2, "count": 3 } }),
        json!({ "results": rows(1), "cursor": { "name": "", "offset": 3, "count": 3 } }),
    ])
    .await;
    assert_eq!(result.expect("the restarted scan completes").len(), 3);
    assert_eq!(
        cursors,
        [None, Some("broken".into()), None, Some("fresh".into())]
    );
}

/// A scan that loses a cursor page every time it is opened spends the shared
/// restart budget and is reported like the other stalls, not returned short.
#[tokio::test]
async fn a_cursor_page_lost_on_every_scan_is_an_error_after_the_restart_budget() {
    let (result, cursors) = scan_live(LostPage::TimedOut, true).await;
    let err = result.expect_err("a scan that never completes must be reported");
    assert!(
        err.to_string().contains(&format!(
            "after starting the scan over {SCAN_RESTARTS} times"
        )),
        "unexpected error: {err:#}"
    );
    let scans = 1 + SCAN_RESTARTS as usize;
    assert_eq!(
        cursors,
        (1..=scans)
            .flat_map(|n| [None, Some(format!("scan-{n}"))])
            .collect::<Vec<_>>(),
        "each lost page is followed by a fresh scan, never a replay"
    );
}
