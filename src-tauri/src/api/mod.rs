pub mod actions;
pub mod activities;
pub mod devices;
pub mod lookups;
pub mod patches;

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::value::RawValue;
use std::collections::HashSet;
use std::time::Duration;
use tracing::{debug, warn};

use crate::auth::AuthState;
use crate::error::truncate_body;
use crate::model::{Device, Location, Organization, Patch, Role};

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
    /// and retrying per `replay` (see [`ReplaySafety`]), and decodes the success
    /// body into a [`Value`].
    ///
    /// The paginated fetchers deliberately do **not** go through here — see
    /// [`Self::request_page`]. Everything else (the single-shot GETs and the acting
    /// POSTs) returns small bodies where a `Value` costs nothing.
    async fn request_raw(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
        replay: ReplaySafety,
    ) -> Result<Value> {
        let resp = self
            .send_with_retry(method, path, query, body, replay)
            .await?;
        decode_response(resp).await
    }

    /// Issues a request and decodes the success body as one page of rows,
    /// deserialized **straight into `T`** rather than through a [`Value`] tree.
    ///
    /// This is the whole reason [`Self::send_with_retry`] is split out of
    /// [`Self::request_raw`]. A whole-fleet third-party patch feed runs to six
    /// figures, and routing it through `Value` meant serde built a
    /// `Map<String, Value>` for every row — allocating a `String` for each of its
    /// ~10 JSON keys — and then walked the tree a second time to produce the
    /// `Patch`. The rows were parsed twice and the intermediate was discarded
    /// immediately. Here the row array is handed to `serde_json` once, as `Vec<T>`.
    async fn request_page<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<PageBody<T>> {
        let resp = self
            .send_with_retry(Method::GET, path, query, None, ReplaySafety::Idempotent)
            .await?;
        decode_page(resp).await
    }

    /// The shared request + retry loop, returning the successful response with its
    /// body unread so the caller can decode it however it likes.
    ///
    /// Split from [`Self::request_raw`] so the typed page decoder can reuse the
    /// retry policy verbatim instead of growing a second copy of it — the arms here
    /// (`ActOnce` on timeout, `Idempotent`-only on 5xx/connect, 429/401 for both) are
    /// what keep an acting POST from being replayed into a second reboot.
    async fn send_with_retry(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
        replay: ReplaySafety,
    ) -> Result<reqwest::Response> {
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
            match retry_for(status, replay, attempt, retry_after_secs(&resp)) {
                Retry::Wait(delay) => {
                    attempt += 1;
                    warn!(%method, %url, %status, attempt, ?delay, "retrying");
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Retry::Reauth => {
                    // The token was rejected server-side. Staleness is time-based,
                    // so invalidate the cached token to force access_token() to
                    // refresh on the next attempt instead of resending the same
                    // dead token.
                    //
                    // Named explicitly: a query fans out many concurrent requests,
                    // so this 401 may be answering a token that a sibling's refresh
                    // has already replaced. Only the token *this* request actually
                    // sent is invalidated, so a burst of lagging 401s can't chain
                    // into a run of redundant grants.
                    self.auth.invalidate_access_token(&token);
                    attempt += 1;
                    continue;
                }
                Retry::No => {}
            }

            if !status.is_success() {
                let text = truncate_body(&resp.text().await.unwrap_or_default());
                warn!(%method, %url, %status, body = %text, "http error");
                bail!("{method} {url} failed ({status}): {text}");
            }
            return Ok(resp);
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
    pub async fn get_paginated<T: DeserializeOwned + PagedRow>(
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
    pub async fn get_paginated_reporting<T: DeserializeOwned + PagedRow>(
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

            match self.request_page::<T>(path, &query).await? {
                PageBody::Array(items) => {
                    let len = items.len();
                    let mut max_id = after;
                    for item in items {
                        let id = item.row_id();
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
                        all.push(item);
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
                PageBody::Envelope {
                    results,
                    cursor: next,
                } => {
                    let page_len = results.len();
                    all.extend(results);

                    if let Some(report) = on_progress {
                        report(all.len());
                    }
                    // No rows on this page means the cursor is exhausted even if the
                    // server echoes a stale token — stop to avoid a loop. Checked
                    // *before* the cursor is interpreted, so a terminal
                    // `{"cursor": {}}` ends the fetch rather than tripping the
                    // malformed-shape error below.
                    if page_len == 0 {
                        return Ok(all);
                    }
                    match next_cursor(next.as_ref())? {
                        Some(c) => cursor = Some(c),
                        None => return Ok(all),
                    }
                }
                PageBody::Empty => return Ok(all),
            }
        }
    }
}

/// Row identity for the `after`-paginated list endpoints.
///
/// `get_paginated` advances its cursor by the largest `id` on a page and
/// de-duplicates the inclusive boundary row, which used to read `item["id"]` off the
/// intermediate `Value`. With the rows deserialized straight into `T` that field is
/// no longer reachable generically, so the types say what their id is — which also
/// makes it a compile error for a new paged type to forget.
///
/// `None` means "this row carries no id", which is the honest answer for a patch
/// record. It cannot move the cursor, so a bare-array endpoint returning such rows
/// stops after one full page exactly as it did before — the reporting feeds that
/// return patches are cursor-enveloped and never take that branch.
pub trait PagedRow {
    fn row_id(&self) -> Option<i64>;
}

impl PagedRow for Device {
    fn row_id(&self) -> Option<i64> {
        Some(self.id)
    }
}

impl PagedRow for Organization {
    fn row_id(&self) -> Option<i64> {
        Some(self.id)
    }
}

impl PagedRow for Location {
    fn row_id(&self) -> Option<i64> {
        Some(self.id)
    }
}

impl PagedRow for Role {
    fn row_id(&self) -> Option<i64> {
        Some(self.id)
    }
}

impl PagedRow for Patch {
    /// The `/queries/*` patch feeds carry no row id — they page by cursor.
    fn row_id(&self) -> Option<i64> {
        None
    }
}

/// One decoded page of a paginated response, with its rows already in their final
/// type.
#[derive(Debug)]
enum PageBody<T> {
    /// A bare JSON array — the `after`-paginated list endpoints
    /// (`/devices-detailed`, `/organizations`, `/locations`, `/roles`).
    Array(Vec<T>),
    /// The `{ results, cursor }` envelope — the `/queries/*` reporting endpoints.
    /// The cursor stays a [`Value`] because it is one small field per page, so
    /// nothing is gained by typing it and [`next_cursor`] already reads every shape
    /// NinjaOne sends.
    Envelope {
        results: Vec<T>,
        cursor: Option<Value>,
    },
    /// `204`, an empty body, or a literal `null` — no rows and no cursor.
    Empty,
}

/// The envelope's own two fields, left **unparsed**.
///
/// [`RawValue`] borrows the original bytes rather than building a tree, so the
/// shape checks below cost nothing and `results` is handed to serde exactly once,
/// as `Vec<T>`.
#[derive(Deserialize)]
struct RawEnvelope<'a> {
    #[serde(borrow, default)]
    results: Option<&'a RawValue>,
    #[serde(borrow, default)]
    cursor: Option<&'a RawValue>,
}

/// Reads a successful response as one page of `T`.
async fn decode_page<T: DeserializeOwned>(resp: reqwest::Response) -> Result<PageBody<T>> {
    if resp.status() == StatusCode::NO_CONTENT {
        return Ok(PageBody::Empty);
    }
    let bytes = resp.bytes().await.context("read body")?;
    parse_page(&bytes)
}

/// Decides which of NinjaOne's two pagination shapes a body is and deserializes it.
///
/// Dispatching on the first non-whitespace byte rather than on `Content-Type`
/// matches what [`decode_response`] already tolerated: some endpoints return JSON
/// without a JSON content type, and a body that isn't JSON at all (a proxy's HTML
/// error page) falls through to the same "unexpected shape" error it did before.
fn parse_page<T: DeserializeOwned>(bytes: &[u8]) -> Result<PageBody<T>> {
    let trimmed = bytes.trim_ascii();
    match trimmed.first() {
        None => Ok(PageBody::Empty),
        Some(b'[') => Ok(PageBody::Array(
            serde_json::from_slice(trimmed).context("deserialize page item")?,
        )),
        Some(b'{') => {
            let env: RawEnvelope =
                serde_json::from_slice(trimmed).context("decode paginated envelope")?;
            let Some(results) = env.results else {
                bail!("paginated response missing `results`");
            };
            // `results` must be an array. A non-array (string/object/number) is a
            // malformed envelope, not an empty page — fail loudly rather than
            // silently treating it as zero rows and stopping, which would return a
            // truncated fleet as if it were complete. Checked on the raw slice so the
            // message names the shape that arrived rather than serde's type error.
            let raw = results.get();
            if !raw.trim_start().starts_with('[') {
                bail!(
                    "paginated `results` was not an array: {}",
                    truncate_body(raw)
                );
            }
            Ok(PageBody::Envelope {
                results: serde_json::from_str(raw).context("deserialize page item")?,
                cursor: env
                    .cursor
                    .map(|c| serde_json::from_str::<Value>(c.get()))
                    .transpose()
                    .context("decode cursor")?,
            })
        }
        Some(b'n') if trimmed == b"null" => Ok(PageBody::Empty),
        _ => bail!(
            "unexpected paginated body shape: {}",
            truncate_body(&String::from_utf8_lossy(trimmed))
        ),
    }
}

/// What to do about a response status before its body is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retry {
    /// Give up retrying; the caller reports success or the error status.
    No,
    /// Sleep this long, then re-issue the request.
    Wait(Duration),
    /// Force a token refresh and re-issue immediately.
    Reauth,
}

/// The retry policy, as a pure decision so it can be tested without a server.
///
/// The `Idempotent`-only guard on 5xx is the load-bearing part and the reason this
/// is worth reading on its own. A reporting pull is dozens of *sequential* cursor
/// pages, so a gateway 502 on a late page used to discard every page already
/// accumulated — 5xx is by far the most common transient failure on that path. But
/// a 5xx on an acting POST is exactly the ambiguity [`ReplaySafety::ActOnce`]
/// exists for: the gateway may have failed *after* the job reached the device
/// queue, so writes fail through to `JobState::Unknown` and are polled rather than
/// replayed. 429 and 401 stay retryable for both — the gateway rejected the
/// request before it could reach a device.
fn retry_for(
    status: StatusCode,
    replay: ReplaySafety,
    attempt: u8,
    retry_after: Option<u64>,
) -> Retry {
    if attempt >= MAX_RETRIES {
        return Retry::No;
    }
    match status {
        // The server tells us how long to wait; second-guessing it is how a client
        // turns a soft rate limit into a hard one.
        StatusCode::TOO_MANY_REQUESTS => Retry::Wait(Duration::from_secs(retry_after.unwrap_or(5))),
        StatusCode::UNAUTHORIZED => Retry::Reauth,
        s if s.is_server_error() && replay == ReplaySafety::Idempotent => {
            Retry::Wait(backoff(attempt + 1))
        }
        _ => Retry::No,
    }
}

/// The `Retry-After` header in seconds, if the server sent a usable one.
fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get("Retry-After")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

/// Reads a successful response into a `Value`.
///
/// NinjaOne is inconsistent about what a success body looks like: `204` and an
/// empty body both mean "nothing to report", and some endpoints return JSON
/// without a JSON content type. A plain string that doesn't parse is preserved
/// as `Value::String` rather than discarded, so an unexpected body still reaches
/// the caller's error message.
async fn decode_response(resp: reqwest::Response) -> Result<Value> {
    if resp.status() == StatusCode::NO_CONTENT {
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
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
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
///
/// `Ok(None)` means "no more pages"; `Err` means the cursor is a shape we cannot
/// interpret. The distinction matters because this is only consulted after a page
/// that *did* return rows, so an uninterpretable cursor is a fetch that stops
/// early — and the caller has no way to tell a truncated fleet from a complete
/// one. Reporting a partial fleet as complete understates every compliance number
/// derived from it. The sibling `results` handling already bails loudly on a
/// malformed envelope for exactly this reason; this arm used to return `None` and
/// stop silently.
fn next_cursor(value: Option<&Value>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone()).filter(|s| !s.is_empty())),
        Value::Object(obj) => match obj.get("name") {
            Some(Value::String(s)) => Ok(Some(s.clone()).filter(|s| !s.is_empty())),
            // An object cursor whose `name` is absent or not a string is not
            // "finished" — it is a shape this client does not understand.
            other => bail!(
                "cursor object has no usable `name`: {}",
                truncate_body(
                    &serde_json::to_string(other.unwrap_or(&Value::Null)).unwrap_or_default()
                )
            ),
        },
        other => bail!(
            "unexpected cursor shape: {}",
            truncate_body(&other.to_string())
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            next_cursor(cursor.as_ref()).unwrap().as_deref(),
            Some("tok")
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
            Some("abc".to_string())
        );
        assert_eq!(next_cursor(Some(&json!(""))).unwrap(), None);
    }

    #[test]
    fn next_cursor_reads_object_name() {
        let v = json!({ "name": "tok-42", "offset": 500, "count": 500 });
        assert_eq!(next_cursor(Some(&v)).unwrap(), Some("tok-42".to_string()));
        // An explicitly empty name is a real end-of-pages signal.
        let done = json!({ "name": "", "offset": 1000 });
        assert_eq!(next_cursor(Some(&done)).unwrap(), None);
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
