pub mod actions;
pub mod activities;
pub mod devices;
pub mod lookups;
pub mod patches;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration;
use tracing::{debug, warn};

use crate::auth::AuthState;
use crate::error::truncate_body;

const DEFAULT_PAGE_SIZE: u32 = 500;
/// Page size for the high-volume `/queries/*` reporting endpoints (patches and
/// install history). These are cursor-paginated, so a larger page only means
/// fewer *sequential* round trips on a big fleet — the cursor (not the page size)
/// decides when paging stops, so an API that silently caps the page still returns
/// every row (the `Value::Object` envelope branch never compares page length to the
/// requested size). The NinjaOne spec documents this reporting family with a
/// `pageSize` max of `10000` (default `1000`); `5000` is a safe margin under that
/// cap that cuts round trips ~5× versus the old `1000`. The `after`-paginated list
/// endpoints stay at `DEFAULT_PAGE_SIZE` — their stop condition compares page length
/// to the requested size, so over-requesting there would end paging early and drop
/// the rest of the fleet.
const REPORTING_PAGE_SIZE: u32 = 5000;
const MAX_RETRIES: u8 = 3;

/// Whether a request may be replayed after a *transport* failure whose outcome is
/// unknown (a client-side timeout: the body was sent, but no response came back).
///
/// Reads are naturally idempotent. A POST that *acts* — reboot, script run, patch
/// apply — is not, and NinjaOne v2 offers no idempotency-key header, so a replayed
/// dispatch runs the script a second time on the device.
///
/// This only governs the timeout arm. Server *rejections* (429, 401) are replayed
/// regardless of the policy: the gateway refused the request before it ever reached
/// the device queue, so re-sending cannot double-execute anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplaySafety {
    Idempotent,
    ActOnce,
}

/// Sink for incremental pagination progress: invoked with the cumulative row
/// count after each page is accumulated. Callers that don't stream progress to
/// the UI pass `None`.
pub type ProgressFn<'a> = dyn Fn(usize) + Send + Sync + 'a;

#[derive(Clone)]
pub struct NinjaApiClient {
    http: reqwest::Client,
    auth: AuthState,
}

impl NinjaApiClient {
    pub fn new(http: reqwest::Client, auth: AuthState) -> Self {
        Self { http, auth }
    }

    /// Issues a request against `{base}/api/v2{path}`, refreshing the bearer token
    /// and retrying per `replay` (see [`ReplaySafety`]).
    async fn request_raw(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
        replay: ReplaySafety,
    ) -> Result<Value> {
        let base = self.auth.base_url();
        let url = format!("{base}/api/v2{path}");
        let mut attempt = 0u8;
        loop {
            let token = self.auth.access_token().await?;
            debug!(%method, %url, "http request");
            let mut req = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(&token)
                .header("Accept", "application/json");
            if !query.is_empty() {
                req = req.query(query);
            }
            if let Some(b) = &body {
                req = req.json(b);
            }

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e)
                    if e.is_timeout()
                        && attempt < MAX_RETRIES
                        && replay == ReplaySafety::Idempotent =>
                {
                    attempt += 1;
                    warn!(?e, attempt, "request timed out, retrying");
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                // The body was already on the wire when the clock ran out, so the
                // action may have been queued even though we never saw the response.
                // Replaying would risk a second reboot / script run.
                Err(e) if e.is_timeout() && replay == ReplaySafety::ActOnce => {
                    warn!(%method, %url, "acting request timed out; not retried");
                    return Err(e).context(
                        "the request timed out after the body was sent — the action may already \
                         be queued in NinjaOne. It was NOT retried; check the device's activity \
                         feed before trying again",
                    );
                }
                // A connect failure means the request never reached the server, so
                // replaying it can't double-execute — but only reads take this arm,
                // because `is_connect()` can be reported for a connection that died
                // mid-flight and we won't re-dispatch an action on a maybe.
                Err(e)
                    if e.is_connect()
                        && attempt < MAX_RETRIES
                        && replay == ReplaySafety::Idempotent =>
                {
                    attempt += 1;
                    warn!(?e, attempt, "connect failed, retrying");
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(e).context("http send"),
            };

            let status = resp.status();
            if status == StatusCode::TOO_MANY_REQUESTS && attempt < MAX_RETRIES {
                attempt += 1;
                let wait = resp
                    .headers()
                    .get("Retry-After")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(5);
                warn!(attempt, wait, "429 rate limited, backing off");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            if status == StatusCode::UNAUTHORIZED && attempt < MAX_RETRIES {
                // The token was rejected server-side. Staleness is time-based, so
                // invalidate the cached token to force access_token() to refresh
                // on the next attempt instead of resending the same dead token.
                self.auth.invalidate_access_token();
                attempt += 1;
                continue;
            }
            // A 5xx is the gateway or the reporting backend failing to produce this
            // page, not a rejection of what we asked for — and it is the *most*
            // common transient failure on the long reporting pulls, where a
            // six-figure feed gives dozens of sequential chances to hit one.
            // Without this arm a single 502 on page 63 of 80 discards every page
            // already accumulated by `get_paginated_reporting` and the operator
            // re-runs the whole fetch from scratch.
            //
            // Reads only: a 5xx on an acting POST is exactly the ambiguous case
            // `ReplaySafety::ActOnce` exists for — the gateway may have failed
            // *after* handing the job to the device queue, so the dispatch stays
            // `JobState::Unknown` and is polled rather than replayed.
            if status.is_server_error()
                && attempt < MAX_RETRIES
                && replay == ReplaySafety::Idempotent
            {
                attempt += 1;
                warn!(%method, %url, %status, attempt, "server error, retrying");
                tokio::time::sleep(backoff(attempt)).await;
                continue;
            }
            if !status.is_success() {
                let text = truncate_body(&resp.text().await.unwrap_or_default());
                warn!(%method, %url, %status, body = %text, "http error");
                bail!("{method} {url} failed ({status}): {text}");
            }

            if status == StatusCode::NO_CONTENT {
                return Ok(Value::Null);
            }

            let ctype = resp
                .headers()
                .get("Content-Type")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .to_string();

            if ctype.contains("application/json") {
                return resp.json().await.context("decode json body");
            }

            let text = resp.text().await.context("read body")?;
            if text.is_empty() {
                return Ok(Value::Null);
            }
            return Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)));
        }
    }

    /// Single-shot GET for the endpoints that return one object rather than a
    /// paginated collection (`/device/{id}/scripting/options`, `/automation/scripts`).
    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let raw = self
            .request_raw(Method::GET, path, query, None, ReplaySafety::Idempotent)
            .await?;
        serde_json::from_value(raw).context("deserialize response body")
    }

    /// POST to an endpoint whose success is a bare `204 No Content` — the patch
    /// scan/apply family. Never replayed on timeout ([`ReplaySafety::ActOnce`]).
    async fn post_action(&self, path: &str, body: Option<Value>) -> Result<()> {
        self.request_raw(Method::POST, path, &[], body, ReplaySafety::ActOnce)
            .await
            .map(|_| ())
    }

    /// POST that returns a body worth reading (`/device/{id}/script/run`). Never
    /// replayed on timeout ([`ReplaySafety::ActOnce`]).
    async fn post_json(&self, path: &str, body: Value) -> Result<Value> {
        self.request_raw(Method::POST, path, &[], Some(body), ReplaySafety::ActOnce)
            .await
    }

    /// Cursor-paginated GET covering NinjaOne's two pagination styles. The
    /// `/queries/*` endpoints return a `{ results, cursor }` envelope (cursor is a
    /// bare string or a `{ name, offset, ... }` object, fed back as `cursor`); the
    /// core list endpoints (`/devices-detailed`, `/organizations`, `/locations`, …)
    /// return a bare array and page via `after=<id>` and `pageSize`, ending when a
    /// page is shorter than `pageSize`. Without the `after` paging a fleet with
    /// more than `pageSize` devices would load only the first page, so the
    /// device-to-patch join would miss every device after the first page.
    ///
    /// The `after` cursor advances by the **maximum** id on a page (not the last
    /// one) so an endpoint that doesn't return ids in ascending order can't stop
    /// short, and ids are de-duplicated so an inclusive-`after` boundary row isn't
    /// counted twice. Forward progress is required (the max id must advance), so a
    /// misbehaving endpoint can't loop forever.
    pub async fn get_paginated<T: DeserializeOwned + Clone>(
        &self,
        path: &str,
        base_query: &[(&str, String)],
    ) -> Result<Vec<T>> {
        self.get_paginated_reporting(path, base_query, DEFAULT_PAGE_SIZE, None)
            .await
    }

    /// Like [`get_paginated`](Self::get_paginated), reporting the cumulative row
    /// count to `on_progress` after each page so a long fetch can stream progress
    /// to the UI.
    pub async fn get_paginated_reporting<T: DeserializeOwned + Clone>(
        &self,
        path: &str,
        base_query: &[(&str, String)],
        page_size: u32,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<T>> {
        let mut all: Vec<T> = Vec::new();
        let mut seen_ids: HashSet<i64> = HashSet::new();
        let mut cursor: Option<String> = None;
        let mut after: Option<i64> = None;

        loop {
            let mut query: Vec<(&str, String)> = base_query.to_vec();
            query.push(("pageSize", page_size.to_string()));
            if let Some(c) = &cursor {
                query.push(("cursor", c.clone()));
            }
            if let Some(a) = after {
                query.push(("after", a.to_string()));
            }

            let raw: Value = self
                .request_raw(Method::GET, path, &query, None, ReplaySafety::Idempotent)
                .await?;

            match raw {
                Value::Array(items) => {
                    let len = items.len();
                    let mut max_id = after;
                    for item in items {
                        let id = item.get("id").and_then(Value::as_i64);
                        // Skip a row already seen on a prior page (an inclusive
                        // `after` cursor re-returns the boundary row).
                        if let Some(id) = id
                            && !seen_ids.insert(id)
                        {
                            continue;
                        }
                        if let Some(id) = id {
                            max_id = Some(max_id.map_or(id, |m| m.max(id)));
                        }
                        let v: T = serde_json::from_value(item).context("deserialize page item")?;
                        all.push(v);
                    }
                    if let Some(report) = on_progress {
                        report(all.len());
                    }
                    // A short page is the last page. Otherwise advance the cursor to
                    // the largest id seen; stop if it can't move forward (no id, or
                    // no new rows) so a misbehaving endpoint can't loop forever.
                    if len < page_size as usize {
                        return Ok(all);
                    }
                    match max_id {
                        Some(id) if Some(id) != after => after = Some(id),
                        _ => return Ok(all),
                    }
                }
                Value::Object(mut obj) => {
                    let results = obj
                        .remove("results")
                        .ok_or_else(|| anyhow!("paginated response missing `results`"))?;
                    // `results` must be an array. A non-array (string/object/number)
                    // is a malformed envelope, not an empty page — fail loudly
                    // rather than silently treating it as zero rows and stopping,
                    // which would return a truncated fleet as if it were complete.
                    let Value::Array(items) = results else {
                        bail!(
                            "paginated `results` was not an array: {}",
                            truncate_body(&results.to_string())
                        );
                    };
                    let page_len = items.len();
                    for item in items {
                        let v: T = serde_json::from_value(item).context("deserialize page item")?;
                        all.push(v);
                    }

                    if let Some(report) = on_progress {
                        report(all.len());
                    }
                    let next = next_cursor(obj.get("cursor"));
                    match next {
                        // No rows on this page means the cursor is exhausted even if
                        // the server echoes a stale token — stop to avoid a loop.
                        Some(c) if page_len > 0 => cursor = Some(c),
                        _ => return Ok(all),
                    }
                }
                Value::Null => return Ok(all),
                other => bail!(
                    "unexpected paginated body shape: {}",
                    truncate_body(&other.to_string())
                ),
            }
        }
    }
}

/// Exponential backoff for a retryable *transport* or 5xx failure: 2s, 4s, 8s.
///
/// Deliberately not used for 429 — there the server tells us how long to wait via
/// `Retry-After`, and second-guessing it is how a client turns a soft rate limit
/// into a hard one.
fn backoff(attempt: u8) -> Duration {
    Duration::from_secs(2u64.pow(attempt as u32))
}

/// Extracts the next-page token from a `cursor` field that may be a string or an
/// object `{ "name": "...", "offset": N }`.
fn next_cursor(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()).filter(|s| !s.is_empty()),
        Value::Object(obj) => obj
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn next_cursor_reads_string() {
        assert_eq!(next_cursor(Some(&json!("abc"))), Some("abc".to_string()));
        assert_eq!(next_cursor(Some(&json!(""))), None);
    }

    #[test]
    fn next_cursor_reads_object_name() {
        let v = json!({ "name": "tok-42", "offset": 500, "count": 500 });
        assert_eq!(next_cursor(Some(&v)), Some("tok-42".to_string()));
    }

    #[test]
    fn next_cursor_none_when_missing() {
        assert_eq!(next_cursor(None), None);
        assert_eq!(next_cursor(Some(&json!({ "offset": 0 }))), None);
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
        NinjaApiClient::new(http, auth)
            .device_patch_apply(3, crate::model::PatchType::Os)
            .await
            .expect_err("a 5xx on an acting POST must not be replayed");
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
}
