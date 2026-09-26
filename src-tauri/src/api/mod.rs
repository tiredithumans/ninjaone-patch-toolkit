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
use std::fmt;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::auth::AuthState;
use crate::error::truncate_body;
use crate::model::{Device, Location, Organization, Patch, Role};

const DEFAULT_PAGE_SIZE: u32 = 500;
/// Page size for the high-volume `/queries/*` reporting endpoints (patches and
/// install history). These are cursor-paginated, so a larger page only means
/// fewer *sequential* round trips on a big fleet — the cursor (not the page size)
/// decides when paging stops, so an API that silently caps the page still returns
/// every row (the `Value::Object` envelope branch never compares page length to the
/// requested size). The four patch endpoints declare `pageSize` as a bare
/// `integer/int32` with **no** documented maximum — the `maximum: 10000, default:
/// 1000` this comment used to cite is declared on `/queries/logged-on-users`, which
/// this app never calls — so `5000` rests on the envelope branch tolerating a
/// server-side cap, not on a documented ceiling. That tolerance is the actual
/// safety property here; keep it if you change the value. The `after`-paginated list
/// endpoints stay at `DEFAULT_PAGE_SIZE` — their stop condition compares page length
/// to the requested size, so over-requesting there would end paging early and drop
/// the rest of the fleet.
const REPORTING_PAGE_SIZE: u32 = 5000;
const MAX_RETRIES: u8 = 3;
/// The longest `Retry-After` this client sits out. The header is server-controlled,
/// and an uncapped value parked the request — and, on a dispatch, the operator's
/// whole batch — for as long as the server cared to ask, with nothing on screen.
const MAX_RETRY_AFTER_SECS: u64 = 60;

/// The failure of a [`ReplaySafety::ActOnce`] request that may nonetheless have
/// reached NinjaOne: a timeout or a connection lost after the request was sent, a
/// 5xx (the gateway may have failed *after* the job reached the device queue), or a
/// 2xx whose body could not be read.
///
/// The dispatch site turns this into `JobState::Unknown` — polled, never replayed —
/// rather than a terminal `Failed`. It is a type rather than a phrase in the message
/// because the classification used to be `msg.contains("may already")`, which only
/// the timeout arm produced, so a 5xx on an apply read as a definite failure while
/// the device could be installing patches. Test with [`is_outcome_unknown`], which
/// sees through any `.context()` layered on top.
#[derive(Debug)]
pub struct OutcomeUnknown(pub String);

impl fmt::Display for OutcomeUnknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} — the action may already be queued in NinjaOne. It was NOT retried; check \
             the device's activity feed before trying again",
            self.0
        )
    }
}

impl std::error::Error for OutcomeUnknown {}

/// Whether `err` (or anything it wraps) is an [`OutcomeUnknown`].
pub fn is_outcome_unknown(err: &anyhow::Error) -> bool {
    err.downcast_ref::<OutcomeUnknown>().is_some()
}

/// Whether a request may be replayed after a failure whose outcome is ambiguous.
///
/// Reads are naturally idempotent. A POST that *acts* — reboot, script run, patch
/// apply — is not, and NinjaOne v2 offers no idempotency-key header, so a replayed
/// dispatch runs the script a second time on the device.
///
/// It governs the arms that cannot tell "rejected" from "accepted": a transport
/// failure after send, a 5xx, an unreadable 2xx body. An `ActOnce` request fails
/// those with [`OutcomeUnknown`] instead of replaying. Server *rejections* (429,
/// 401) are replayed regardless of the policy: the gateway refused the request
/// before it ever reached the device queue, so re-sending cannot double-execute
/// anything.
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
    /// For an `ActOnce` request a body that cannot be read is an [`OutcomeUnknown`]:
    /// the server already answered 2xx, so the action was accepted even though we
    /// cannot say what it reported.
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
        match decode_response(resp).await {
            Err(err) if replay == ReplaySafety::ActOnce => Err(err.context(OutcomeUnknown(
                "NinjaOne accepted the request but its response could not be read".into(),
            ))),
            other => other,
        }
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
    /// (`ActOnce` → [`OutcomeUnknown`] on an in-flight failure or a 5xx,
    /// `Idempotent`-only retries on 5xx/connect, 429/401 for both) are what keep an
    /// acting POST from being replayed into a second reboot.
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
                // The body may already have been on the wire when the clock ran out or
                // the connection dropped, so the action may have been queued even
                // though we never saw the response. Replaying would risk a second
                // reboot / script run, and calling it Failed would invite the operator
                // to do the same by hand. Only a connect failure (no connection was
                // established) or a request that could not be built is known not to
                // have left; those fall through to the plain error below.
                Err(e) if replay == ReplaySafety::ActOnce && !e.is_connect() && !e.is_builder() => {
                    warn!(%method, %url, ?e, "acting request failed in flight; not retried");
                    let why = if e.is_timeout() {
                        "the request timed out after it was sent"
                    } else {
                        "the connection failed while the request was in flight"
                    };
                    return Err(anyhow::Error::new(e).context(OutcomeUnknown(why.into())));
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
                // See `retry_for`: a 5xx on an acting POST is not a rejection but an
                // unknown outcome, so it is polled rather than reported failed. A 4xx
                // is NinjaOne refusing the request and stays a plain error.
                if status.is_server_error() && replay == ReplaySafety::ActOnce {
                    return Err(anyhow::Error::new(OutcomeUnknown(format!(
                        "NinjaOne answered {status} to {method} {url}: {text}"
                    ))));
                }
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
        let mut cursor: Option<PageCursor> = None;
        let mut after: Option<i64> = None;
        // Reported at every exit. A short read is otherwise indistinguishable from a
        // complete one at every call site above this function, and a whole-fleet feed
        // that stops early understates every number derived from it.
        let mut pages: u32 = 0;

        loop {
            let mut query: Vec<(&str, String)> = base_query.to_vec();
            query.push(("pageSize", page_size.to_string()));
            if let Some(c) = &cursor {
                // `cursor` is the only paging parameter these endpoints accept — the
                // offset that rides beside the name in the response is server-side
                // state, keyed by that name. See [`PageCursor`].
                query.push(("cursor", c.name.clone()));
            }
            if let Some(a) = after {
                query.push(("after", a.to_string()));
            }

            pages += 1;
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
                        info!(path, rows = all.len(), pages, exit = "short page", "paged");
                        return Ok(all);
                    }
                    match max_id {
                        Some(id) if Some(id) != after => after = Some(id),
                        // A *full* page that cannot move the `after` cursor — no ids
                        // on the rows, or none newer than the ones already seen — is
                        // a stalled scan, not the end of one. Reported rather than
                        // returned: `all` holds a partial fleet here, and handing it
                        // back as `Ok` is what makes an undercount invisible.
                        _ => bail!(
                            "{path} returned a full page of {len} rows that did not advance the \
                             `after` cursor; stopping at {} rows rather than reporting a partial \
                             fleet as complete",
                            all.len()
                        ),
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
                        info!(path, rows = all.len(), pages, exit = "empty page", "paged");
                        return Ok(all);
                    }
                    match next_cursor(next.as_ref())? {
                        // Forward progress is required here for the same reason the
                        // `after` branch above requires it: an endpoint that echoes
                        // the cursor it was handed alongside a *full* page would
                        // otherwise loop forever, re-fetching the same rows and
                        // growing `all` without bound. The array branch was hardened
                        // against exactly this; the envelope branch stopped only on
                        // an empty page.
                        //
                        // Compared as a whole rather than by `name`: NinjaOne's cursor
                        // is a stable handle plus an advancing `offset`, so matching on
                        // the name alone read an advancing scan as a stalled one and
                        // cut every feed off after its second page. See [`PageCursor`].
                        Some(c) if Some(&c) == cursor.as_ref() => bail!(
                            "{path} returned a full page of {page_len} rows alongside the very \
                             cursor it was handed ({c:?}); stopping at {} rows rather than \
                             reporting a partial fleet as complete",
                            all.len()
                        ),
                        Some(c) => cursor = Some(c),
                        None => {
                            info!(
                                path,
                                rows = all.len(),
                                pages,
                                exit = "cursor exhausted",
                                "paged"
                            );
                            return Ok(all);
                        }
                    }
                }
                PageBody::Empty => {
                    info!(path, rows = all.len(), pages, exit = "empty body", "paged");
                    return Ok(all);
                }
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
        // turns a soft rate limit into a hard one. It is still capped: the value is
        // server-controlled, and one absurd header must not hang a dispatch.
        StatusCode::TOO_MANY_REQUESTS => Retry::Wait(Duration::from_secs(
            retry_after.unwrap_or(5).min(MAX_RETRY_AFTER_SECS),
        )),
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

/// One page-to-page cursor: the token the next request echoes back, plus the
/// position that rides alongside it.
///
/// NinjaOne's `/queries/*` cursor is `{ name, offset, count, expires }`, and those
/// endpoints accept exactly one paging parameter — `cursor`, documented as "Cursor
/// name". The position therefore lives *server-side*, keyed by that name, which
/// makes `name` a stable handle for the whole scan rather than a per-page token.
///
/// That is why forward progress is measured against the **pair**. Comparing `name`
/// alone made an advancing scan look like a stalled one, so the loop stopped at its
/// second page and handed back 2 × `pageSize` rows as if they were the whole feed —
/// invisible on a short OS feed, and a ~10x undercount on a six-figure third-party
/// one, on every surface that reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PageCursor {
    /// Echoed back as `cursor`; the only paging parameter these endpoints take.
    name: String,
    /// The server's position within the scan. `None` for a bare-string cursor,
    /// which carries no position and so can only signal progress by changing.
    offset: Option<i64>,
}

/// Extracts the next-page cursor from a `cursor` field that may be a string or an
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
fn next_cursor(value: Option<&Value>) -> Result<Option<PageCursor>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let named = |name: &str, offset: Option<i64>| {
        (!name.is_empty()).then(|| PageCursor {
            name: name.to_string(),
            offset,
        })
    };
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(named(s, None)),
        Value::Object(obj) => match obj.get("name") {
            // `offset` is read for the forward-progress check only — it is never
            // sent back, because these endpoints have no `offset` parameter. A
            // missing or non-integer offset simply leaves the name to carry the
            // comparison on its own, exactly as a bare-string cursor does.
            Some(Value::String(s)) => Ok(named(s, obj.get("offset").and_then(Value::as_i64))),
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
mod tests;
