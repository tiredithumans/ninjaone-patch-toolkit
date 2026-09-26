use super::paging::*;
use super::*;
use serde_json::json;

use crate::model::{Organization, Patch};

/// The `Idempotent`-only guard on 5xx is what keeps an acting POST from being
/// replayed into a second reboot or script run: the gateway may have failed
/// *after* the job reached the device queue. 429 and 401 are safe for both —
/// the request was rejected before it could reach a device.
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

#[tokio::test]
async fn get_5xx_is_retried_and_keeps_the_accumulated_pages() {
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // Page 1 succeeds and hands back a live cursor.
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "id": 1, "kbNumber": "KB1" }],
            "cursor": "tok-2"
        })))
        .mount(&server)
        .await;

    // Page 2 fails once with a 502 — the shape of a gateway hiccup partway
    // through a long reporting pull. Before the 5xx arm existed this discarded
    // page 1 as well and the operator re-ran the whole fetch.
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param("cursor", "tok-2"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/queries/os-patches"))
        .and(query_param("cursor", "tok-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "id": 2, "kbNumber": "KB2" }],
            "cursor": ""
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
    let patches = NinjaApiClient::new(http, auth)
        .fleet_os_patches(None, None, None)
        .await
        .expect("a transient 502 must be retried, not fail the whole fetch");
    assert_eq!(patches.len(), 2, "both pages must survive the retry");
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
