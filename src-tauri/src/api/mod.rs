pub mod actions;
pub mod activities;
pub mod devices;
pub mod lookups;
mod paging;
pub mod patches;

pub use paging::ProgressFn;

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fmt;
use std::time::Duration;
use tracing::{debug, warn};

use crate::auth::AuthState;
use crate::error::truncate_body;

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

#[cfg(test)]
mod tests;
