pub mod actions;
pub mod activities;
pub mod devices;
pub mod jobs;
pub mod lookups;
mod paging;
pub mod patches;

pub use paging::ProgressFn;

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

use crate::auth::{AuthState, is_tenant_moved};
use crate::error::{redirect_hint, truncate_body};

const MAX_RETRIES: u8 = 3;
/// The longest `Retry-After` this client sits out. The header is server-controlled,
/// and an uncapped value parked the request — and, on a dispatch, the operator's
/// whole batch — for as long as the server cared to ask, with nothing on screen.
const MAX_RETRY_AFTER_SECS: u64 = 60;
/// The most [`backoff`] stretches a delay, as a fraction of it. Without it every
/// leg of a query that failed together retried together, in lockstep, at 2, 4 and
/// 8 s — re-creating the burst that failed them.
const MAX_BACKOFF_JITTER: f64 = 0.25;
/// The most added to a 429's wait, for the same reason: the shared pause releases
/// every waiting request at one instant, and the server would see them all at once.
const MAX_RATE_LIMIT_JITTER_MS: u64 = 1000;

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

/// The failure of a [`ReplaySafety::CursorPage`] request whose response was lost
/// after the request may have reached NinjaOne: a timeout or a connection lost in
/// flight, a 5xx, or a 2xx whose body could not be read.
///
/// NinjaOne keeps a scan's position server-side, keyed by the cursor's stable
/// `name`, so the server may already have advanced past the page we never saw, and
/// re-sending the name would be answered with the *next* page — a whole `pageSize`
/// of rows skipped without an error. The pagination loop turns this into a restart
/// of the scan rather than a replay. Test with [`is_cursor_replay_unsafe`], which
/// sees through any `.context()` layered on top.
#[derive(Debug)]
struct CursorReplayUnsafe(String);

impl fmt::Display for CursorReplayUnsafe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} — the server may already have moved its cursor past this page, so the \
             cursor was not re-sent",
            self.0
        )
    }
}

impl std::error::Error for CursorReplayUnsafe {}

/// Whether `err` (or anything it wraps) is a [`CursorReplayUnsafe`].
fn is_cursor_replay_unsafe(err: &anyhow::Error) -> bool {
    err.downcast_ref::<CursorReplayUnsafe>().is_some()
}

/// Asked before every attempt of an acting request; `false` stops it unsent.
/// See [`NinjaApiClient::with_send_guard`].
pub type SendGuard = Arc<dyn Fn() -> bool + Send + Sync>;

/// An acting request its [`SendGuard`] stopped before an attempt went out. Every
/// earlier attempt was a definite rejection (429, 401), so the action did not
/// reach the device: the dispatch site records it as not sent, never as unknown.
#[derive(Debug)]
pub struct SendRefused;

impl fmt::Display for SendRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not sent: the session ended before the request went out")
    }
}

impl std::error::Error for SendRefused {}

/// Whether `err` (or anything it wraps) is a [`SendRefused`].
pub fn is_send_refused(err: &anyhow::Error) -> bool {
    err.downcast_ref::<SendRefused>().is_some()
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
///
/// A cursor page sits between the two. Reading it changes nothing on a device, but
/// it does move the scan's server-side position, so a lost response cannot be
/// re-asked for: `CursorPage` fails those same arms with [`CursorReplayUnsafe`],
/// and the pagination loop restarts the scan. A connect failure, 429 and 401 never
/// reached the handler and are retried as for `Idempotent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplaySafety {
    Idempotent,
    CursorPage,
    ActOnce,
}

#[derive(Clone)]
pub struct NinjaApiClient {
    http: reqwest::Client,
    auth: AuthState,
    send_guard: Option<SendGuard>,
    /// When the last 429 said this client may send again; shared by every clone.
    ///
    /// A rate limit is the tenant's, not the request's, but only the request that
    /// drew the 429 used to wait: a query's seven concurrent legs, up to eight
    /// poller reads and the `/jobs` poll kept sending into it and drew 429s of
    /// their own. Every attempt now waits this out before it asks for a token. A
    /// `std` mutex because it is only ever held to copy or bump an `Instant`, never
    /// across an await.
    paused_until: Arc<Mutex<Option<Instant>>>,
}

impl NinjaApiClient {
    pub fn new(http: reqwest::Client, auth: AuthState) -> Self {
        Self {
            http,
            auth,
            send_guard: None,
            paused_until: Arc::new(Mutex::new(None)),
        }
    }

    /// A clone whose [`ReplaySafety::ActOnce`] requests ask `guard` before every
    /// attempt, the retries included, and fail with [`SendRefused`] once it says no.
    ///
    /// A dispatch checks its session before it sends, but a 429 then parks the
    /// request for up to a minute per retry, and a 401 re-sends at once with a fresh
    /// token. Every attempt read the token live from the shared `AuthState`, so a
    /// sign-out and another operator's sign-in during that wait re-sent the
    /// departed session's reboot under the new operator's grant. The guard is asked
    /// after the token is in hand, the last point before the bytes leave.
    ///
    /// A tenant switch no longer needs the guard: every request, reads included, is
    /// stamped with the tenant its URL was built for, and
    /// [`AuthState::access_token_for`] refuses a token once that tenant has moved
    /// (see [`Self::send_with_retry`]). The guard remains for what the stamp cannot
    /// see — the session ending under an unchanged tenant.
    ///
    /// The clone shares the rate-limit pause with `self`: a dispatch is as subject
    /// to the tenant's 429 as the query that drew it.
    pub fn with_send_guard(&self, guard: SendGuard) -> Self {
        Self {
            send_guard: Some(guard),
            ..self.clone()
        }
    }

    /// How much of the shared rate-limit pause is left, if any. The lock is held
    /// only to copy the deadline.
    fn pause_left(&self) -> Option<Duration> {
        let until = *self
            .paused_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pause_remaining(until, Instant::now())
    }

    /// Extends the shared pause to `until`. Never shortens it: a later 429 with a
    /// smaller `Retry-After` does not lift the wait an earlier one asked for.
    fn pause_until(&self, until: Instant) {
        let mut paused = self
            .paused_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if paused.is_none_or(|current| current < until) {
            *paused = Some(until);
        }
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
    /// (`ActOnce` → [`OutcomeUnknown`] and `CursorPage` → [`CursorReplayUnsafe`] on
    /// an in-flight failure or a 5xx, `Idempotent`-only retries on timeout/5xx,
    /// connect retries for everything but `ActOnce`, 429/401 for all) are what keep
    /// an acting POST from being replayed into a second reboot and a cursor from
    /// being replayed past a page.
    ///
    /// Each attempt first sits out the client-wide 429 pause (not an attempt in
    /// itself), then asks for a token bound to the tenant the URL was built for. A
    /// tenant switch during a wait aborts the request with
    /// [`crate::auth::TenantMoved`] — no retry, and no token invalidated, because
    /// sending it would have handed the new tenant's bearer token to the old host
    /// and let that host's 401 invalidate the new tenant's live token, up to three
    /// refresh grants per request.
    async fn send_with_retry(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
        replay: ReplaySafety,
    ) -> Result<reqwest::Response> {
        let tenant = self.auth.tenant_stamp();
        let url = format!("{}/api/v2{path}", tenant.base_url());
        let mut attempt = 0u8;
        loop {
            // Re-read after every sleep: another request's 429 may have pushed the
            // deadline out while this one was waiting.
            while let Some(wait) = self.pause_left() {
                debug!(%method, %url, ?wait, "waiting out a rate limit");
                tokio::time::sleep(wait).await;
            }
            let token = match self.auth.access_token_for(&tenant).await {
                Ok(token) => token,
                // Every earlier attempt was a rejection (429, 401), so an action
                // stopped here never reached a device: "not sent", like the guard.
                Err(e) if replay == ReplaySafety::ActOnce && is_tenant_moved(&e) => {
                    warn!(%method, %url, attempt, "acting request stopped: the tenant changed");
                    return Err(e.context(SendRefused));
                }
                Err(e) => {
                    if is_tenant_moved(&e) {
                        warn!(%method, %url, attempt, "request abandoned: the tenant changed");
                    }
                    return Err(e);
                }
            };
            if replay == ReplaySafety::ActOnce
                && let Some(guard) = &self.send_guard
                && !guard()
            {
                warn!(%method, %url, attempt, "acting request stopped: its session ended");
                return Err(anyhow::Error::new(SendRefused));
            }
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
                // The same ambiguity on a cursor page: the server may have advanced
                // the scan before the response was lost, and re-sending its name
                // would be answered with the page after this one.
                Err(e)
                    if replay == ReplaySafety::CursorPage && !e.is_connect() && !e.is_builder() =>
                {
                    warn!(%method, %url, ?e, "cursor page lost in flight; not re-sent");
                    let why = if e.is_timeout() {
                        "the cursor page timed out after it was sent"
                    } else {
                        "the connection failed while the cursor page was in flight"
                    };
                    return Err(anyhow::Error::new(e).context(CursorReplayUnsafe(why.into())));
                }
                // A connect failure is raised before a request is written (connector,
                // pool checkout, TLS, proxy), so replaying it can't double-execute or
                // move a cursor. Actions still never take this arm, as a defensive
                // choice: that guarantee rests on reqwest's internal classification,
                // not on anything this client can observe, and we won't re-dispatch
                // an action on a maybe. A cursor page does take it: a misclassified
                // failure would cost rows the end-of-scan count check logs, against a
                // whole query failed on every refused connection.
                Err(e)
                    if e.is_connect()
                        && attempt < MAX_RETRIES
                        && replay != ReplaySafety::ActOnce =>
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
                // A rate limit applies to every request this client sends, so it
                // becomes the shared pause, which the top of the loop waits out
                // for this request and every other one alike.
                Retry::Wait(delay) if status == StatusCode::TOO_MANY_REQUESTS => {
                    attempt += 1;
                    let delay = delay + rate_limit_jitter();
                    warn!(%method, %url, %status, attempt, ?delay, "rate limited; pausing every request");
                    self.pause_until(Instant::now() + delay);
                    continue;
                }
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
                if let Some(hint) = redirect_hint(&resp) {
                    warn!(%method, %url, %status, "redirect not followed");
                    bail!("{method} {url} failed ({status}): {hint}");
                }
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
                // Likewise a gateway may fail a cursor page after the backend has
                // moved the scan on, so the pagination loop restarts it instead.
                if status.is_server_error() && replay == ReplaySafety::CursorPage {
                    return Err(anyhow::Error::new(CursorReplayUnsafe(format!(
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
/// The 5xx arm is the load-bearing part, and it is decided by [`ReplaySafety`]:
///
/// - **`Idempotent`** — a read that carries no server-side state (a single-shot
///   GET, an `after` page, the request that opens a cursor scan) is simply asked
///   for again; 5xx is by far the most common transient failure on a reporting
///   pull.
/// - **`ActOnce`** — not retried. A 5xx on an acting POST is exactly the ambiguity
///   this policy exists for: the gateway may have failed *after* the job reached
///   the device queue, so writes fail through to `JobState::Unknown` and are polled
///   rather than replayed.
/// - **`CursorPage`** — not retried either, for a different reason. NinjaOne keeps
///   the scan's position server-side, keyed by the cursor's stable name, so a
///   gateway that failed after the backend advanced would answer the replay with
///   the *next* page and a whole `pageSize` of rows would vanish. The caller fails
///   it with [`CursorReplayUnsafe`] and the scan starts over instead.
///
/// 429 and 401 stay retryable for all three — the gateway rejected the request
/// before it could reach a device or move a cursor.
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
        .and_then(|s| parse_retry_after(s, chrono::Utc::now()))
}

/// A `Retry-After` value as seconds from `now`: either form RFC 9110 allows.
///
/// Only delay-seconds used to be read, so a server that sent the HTTP-date form
/// got the 5 s default whatever it asked for. The date is parsed as RFC 2822,
/// which covers the IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) servers send;
/// the gap is rounded up so the retry never lands early, a past date means "now",
/// and the result is clamped like the seconds form.
fn parse_retry_after(value: &str, now: chrono::DateTime<chrono::Utc>) -> Option<u64> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(secs);
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let gap_ms = (at.with_timezone(&chrono::Utc) - now)
        .num_milliseconds()
        .max(0) as u64;
    Some(gap_ms.div_ceil(1000).min(MAX_RETRY_AFTER_SECS))
}

/// How long is left of a pause that ends at `until`, or `None` once it is over.
fn pause_remaining(until: Option<Instant>, now: Instant) -> Option<Duration> {
    until
        .and_then(|until| until.checked_duration_since(now))
        .filter(|left| !left.is_zero())
}

/// The random extra on a 429's wait, up to [`MAX_RATE_LIMIT_JITTER_MS`].
fn rate_limit_jitter() -> Duration {
    Duration::from_millis(rand::random_range(0..=MAX_RATE_LIMIT_JITTER_MS))
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

/// Exponential backoff for a retryable *transport* or 5xx failure: 2s, 4s, 8s,
/// each stretched by up to [`MAX_BACKOFF_JITTER`] so concurrent legs that failed
/// together do not retry together.
///
/// Deliberately not used for 429 — there the server tells us how long to wait via
/// `Retry-After`, and second-guessing it is how a client turns a soft rate limit
/// into a hard one.
fn backoff(attempt: u8) -> Duration {
    jittered_backoff(attempt, rand::random_range(0.0..=MAX_BACKOFF_JITTER))
}

/// [`backoff`] with its random fraction supplied, so the curve is testable.
fn jittered_backoff(attempt: u8, jitter: f64) -> Duration {
    Duration::from_secs(2u64.pow(attempt as u32)).mul_f64(1.0 + jitter)
}

/// The `df` (device filter) query parameter, or none at all. An empty filter is
/// omitted rather than sent blank: a `df=` with no value is not "no filter" to the
/// API. Shared by the device inventory and every patch feed so they cannot drift.
fn df_query(df: Option<&str>) -> Vec<(&'static str, String)> {
    match df {
        Some(f) if !f.is_empty() => vec![("df", f.to_string())],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests;
