//! Reading back what a dispatched action did.
//!
//! NinjaOne v2 has **no script-output endpoint**, so a job is resolved from the
//! activity feed, which carries the terminal state plus an exit code. (`/jobs`
//! only reports what is still in flight — the *absence* of a terminal activity
//! already tells us that, so it is never a resolution source. It is read for the
//! live job status display alone; see `api::jobs`.)

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::value::RawValue;

use super::{NinjaApiClient, lenient_rows};
use crate::error::truncate_body;
use crate::model::Activity;

/// Rows requested per activity poll.
///
/// This endpoint is deliberately **not** routed through `get_paginated`: that helper
/// pages a newest-*last* feed by advancing an ascending `after` id over a bare array
/// or a `{ results, cursor }` envelope, whereas `/activities` is newest-first and
/// answers with either a bare array or an `{ activities: [...] }` envelope. Driving
/// it with the wrong cursor semantics would page in the wrong direction, which is a
/// worse failure than a bounded window — so the request is a single shot with an
/// explicit size, and a full page is reported rather than silently accepted.
const ACTIVITY_PAGE_SIZE: u32 = 500;

impl NinjaApiClient {
    /// Activity log, newest first. `since_ts` is a Unix timestamp in **seconds** and
    /// is applied **client-side** — see below.
    ///
    /// `series_uid` narrows the read to one activity series via the documented
    /// `seriesUid` parameter, for a job whose dispatch returned that correlator. It
    /// is sent *alongside* the device `df`, never instead of it: the device scope is
    /// the boundary the third-tier correlation heuristic relies on, and a tenant that
    /// ignored an unrecognized parameter must not widen the read to the whole fleet.
    ///
    /// The response is a bare array on most tenants but an `{ "activities": [...] }`
    /// envelope on others, so both are accepted.
    ///
    /// Scoped to one device, so the [`ACTIVITY_PAGE_SIZE`] window is ample in
    /// practice. A device noisy enough to overflow it within a job's timeout would
    /// push the terminal activity out of reach, so that case is logged rather than
    /// left to look like a job that simply never finished — the job itself still
    /// resolves via its timeout. Only a full page that may have cut the window short
    /// is logged ([`window_may_be_cut_short`]).
    ///
    /// **The time bound is not sent to the API.** This used to pass the dispatch
    /// timestamp as `newerThan`, which the spec defines as *"activities … than
    /// specified activity **ID**"* (`type: integer, format: int64`) — so a value near
    /// 1.7e9 asked for activities newer than an id far beyond any real one, and the
    /// feed came back empty for every poll. An empty feed reads as "the feed lags"
    /// (which is a real and expected condition), so every dispatched scan, apply,
    /// reboot and script resolved by timeout instead of by its activity, with no
    /// error anywhere. The endpoint's date-valued parameters are `after`/`before`,
    /// whose format the spec does not state; rather than guess between epoch and
    /// ISO-8601 on the path that reports what a *write* did, the request is left
    /// unbounded and `activityTime` — which is typed, and already parsed — does the
    /// filtering here.
    pub async fn activities(
        &self,
        device_id: Option<i64>,
        series_uid: Option<&str>,
        since_ts: Option<i64>,
    ) -> Result<Vec<Activity>> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(id) = device_id {
            // No spaces around `=`: the documented grammar is `id=<DeviceID>`, and
            // its worked example is `df=class%3DWINDOWS_SERVER%20AND%20offline`.
            query.push(("df", format!("id={id}")));
        }
        if let Some(uid) = series_uid {
            query.push(("seriesUid", uid.to_string()));
        }
        query.push(("pageSize", ACTIVITY_PAGE_SIZE.to_string()));

        let page = self
            .get_decoded("/activities", &query, parse_activities)
            .await?;

        let floor = since_ts.map(|t| t as f64);
        let oldest = page
            .rows
            .iter()
            .filter_map(|a| a.activity_time)
            .reduce(f64::min);
        if window_may_be_cut_short(page.returned, oldest, floor) {
            // A terminal activity could be out of view. Surfaced so a job that then
            // times out has a logged cause instead of looking like the device never
            // answered.
            tracing::warn!(
                device_id,
                returned = page.returned,
                "activity page came back full; a terminal activity may be beyond the window"
            );
        }

        Ok(page
            .rows
            .into_iter()
            // An activity with no timestamp is kept: it cannot be shown to predate
            // the dispatch, and dropping it would lose a terminal state.
            .filter(|a| match (floor, a.activity_time) {
                (Some(floor), Some(ts)) => ts >= floor,
                _ => true,
            })
            .collect())
    }
}

/// One `/activities` response, decoded.
#[derive(Debug)]
struct ActivityPage {
    /// Every entry the response held, malformed ones included, for the full-page
    /// check.
    returned: usize,
    /// The entries that decoded, newest first as NinjaOne sent them.
    rows: Vec<Activity>,
}

/// Reads one `/activities` response: a bare array on most tenants, an
/// `{ "activities": [...] }` envelope on others.
///
/// Each entry is decoded on its own, straight into [`Activity`] ([`lenient_rows`]):
/// a single malformed entry must not blind the poller to every other job on the
/// device, so it is dropped rather than failing the read. This used to build the
/// whole response as a `Value` tree and then convert it entry by entry. Shape errors
/// quote at most [`truncate_body`] of what arrived.
fn parse_activities(body: &[u8]) -> Result<ActivityPage> {
    #[derive(Deserialize)]
    struct Envelope<'a> {
        #[serde(borrow, default)]
        activities: Option<&'a RawValue>,
    }

    let body = body.trim_ascii();
    let entries: Vec<&RawValue> = match body.first() {
        Some(b'[') => serde_json::from_slice(body).context("decode the activities list")?,
        Some(b'{') => {
            let envelope: Envelope =
                serde_json::from_slice(body).context("decode the activities envelope")?;
            let Some(list) = envelope.activities else {
                bail!("activities response has no `activities` list");
            };
            if !list.get().trim_start().starts_with('[') {
                bail!(
                    "activities field must be an array, got: {}",
                    truncate_body(list.get())
                );
            }
            serde_json::from_str(list.get()).context("decode the activities list")?
        }
        _ => bail!(
            "unexpected activities response shape: {}",
            truncate_body(&String::from_utf8_lossy(body))
        ),
    };
    Ok(ActivityPage {
        returned: entries.len(),
        rows: lenient_rows(entries),
    })
}

/// Whether a page of `returned` entries may have cut the time window short: it is
/// full, and its oldest activity is still at or after the time floor, so entries
/// inside the window can lie past the page's end.
///
/// Any full page used to be logged, but on a busy device a full page usually reaches
/// back well before the dispatch, and then it holds the whole window: everything it
/// left out is older than the floor and would be filtered anyway. Without a floor, or
/// without a timestamp to compare, a full page cannot be shown to cover the window.
fn window_may_be_cut_short(returned: usize, oldest: Option<f64>, floor: Option<f64>) -> bool {
    returned >= ACTIVITY_PAGE_SIZE as usize
        && match (oldest, floor) {
            (Some(oldest), Some(floor)) => oldest >= floor,
            _ => true,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthState;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> NinjaApiClient {
        let http = reqwest::Client::new();
        let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
        NinjaApiClient::new(http, auth)
    }

    #[tokio::test]
    async fn activities_accept_a_bare_array_and_scope_by_device() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/activities"))
            .and(query_param("df", "id=42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 1, "activityType": "SCRIPTING", "statusCode": "COMPLETED",
                  "activityResult": "SUCCESS", "activityTime": 1800.0,
                  "data": { "exitCode": 0 } },
            ])))
            .mount(&server)
            .await;

        let list = client(&server)
            .activities(Some(42), None, Some(1700))
            .await
            .expect("activities");
        assert_eq!(list.len(), 1);
        assert!(list[0].is_terminal());
        assert_eq!(list[0].exit_code(), Some(0));
    }

    /// The dispatch-time floor is ours to apply, because the API parameter that
    /// looks like it (`newerThan`) takes an activity id, not a timestamp.
    #[tokio::test]
    async fn the_time_floor_is_applied_here_and_never_sent_as_newer_than() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/activities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 1, "activityTime": 1_000.0, "statusCode": "COMPLETED" },
                { "id": 2, "activityTime": 2_000.0, "statusCode": "COMPLETED" },
                // No timestamp: kept, since it can't be shown to predate the floor.
                { "id": 3, "statusCode": "COMPLETED" },
            ])))
            .mount(&server)
            .await;

        let list = client(&server)
            .activities(Some(42), None, Some(1_500))
            .await
            .expect("activities");
        assert_eq!(
            list.iter().filter_map(|a| a.id).collect::<Vec<_>>(),
            vec![2, 3]
        );

        let sent = &server.received_requests().await.expect("requests")[0];
        let url = sent.url.as_str();
        assert!(
            !url.contains("newerThan"),
            "newerThan takes an activity id, not a timestamp: {url}"
        );
    }

    /// A series-scoped read keeps the device `df`: the device is the boundary the
    /// correlation heuristic relies on, so `seriesUid` only ever narrows it.
    #[tokio::test]
    async fn a_series_scoped_read_sends_series_uid_and_keeps_the_device_scope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/activities"))
            .and(query_param("df", "id=42"))
            .and(query_param("seriesUid", "uid-7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 9, "seriesUid": "uid-7", "statusCode": "COMPLETED" },
            ])))
            .mount(&server)
            .await;

        let list = client(&server)
            .activities(Some(42), Some("uid-7"), None)
            .await
            .expect("activities");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].series_uid.as_deref(), Some("uid-7"));
    }

    #[tokio::test]
    async fn activities_accept_the_enveloped_shape() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/activities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "activities": [{ "id": 2, "status": "RUNNING" }],
            })))
            .mount(&server)
            .await;

        let list = client(&server)
            .activities(None, None, None)
            .await
            .expect("list");
        assert_eq!(list.len(), 1);
        assert!(!list[0].is_terminal());
    }

    #[tokio::test]
    async fn a_malformed_row_does_not_fail_the_whole_poll() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/activities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": "not-a-number" },
                { "id": 3, "statusCode": "COMPLETED", "activityResult": "FAILURE" },
            ])))
            .mount(&server)
            .await;

        let list = client(&server)
            .activities(None, None, None)
            .await
            .expect("list");
        assert_eq!(list.len(), 1, "the good row must still come through");
        assert_eq!(list[0].id, Some(3));
    }

    #[tokio::test]
    async fn exit_code_falls_back_to_result_code() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/activities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 4, "statusCode": "COMPLETED", "activityResult": "FAILURE",
                  "data": { "resultCode": 2 } },
            ])))
            .mount(&server)
            .await;

        let list = client(&server)
            .activities(None, None, None)
            .await
            .expect("list");
        assert_eq!(list[0].exit_code(), Some(2));
    }

    /// Entries are decoded one by one: an entry that is not an activity at all —
    /// `null`, a number, a string — is dropped like a malformed one, in either
    /// shape, and every entry still counts toward the full-page check.
    #[test]
    fn each_entry_is_decoded_on_its_own_in_either_shape() {
        let entries = json!([
            null,
            5,
            "x",
            { "id": "not-a-number" },
            { "id": 7, "statusCode": "COMPLETED", "activityTime": 10.0 },
        ]);
        for body in [
            entries.clone(),
            json!({ "activities": entries, "more": true }),
        ] {
            let page = parse_activities(&serde_json::to_vec(&body).unwrap()).expect("a page");
            assert_eq!(page.returned, 5, "{body}");
            assert_eq!(
                page.rows.iter().map(|a| a.id).collect::<Vec<_>>(),
                [Some(7)],
                "{body}"
            );
        }
    }

    /// A body that is not the expected shape is an error naming a capped excerpt of
    /// it, never the whole thing: a proxy's sign-in page reached the poller's log in
    /// full through the old `{other}`.
    #[test]
    fn a_misshapen_body_is_an_error_quoting_a_capped_excerpt() {
        let page = format!("<html>{}</html>", "sign in to continue ".repeat(5_000));
        let not_a_list = serde_json::to_vec(&json!({ "activities": page.clone() })).unwrap();
        let bodies: [&[u8]; 5] = [page.as_bytes(), &not_a_list, b"{}", b"", b"42"];
        for body in bodies {
            let err = format!(
                "{:#}",
                parse_activities(body).expect_err("not an activities list")
            );
            assert!(err.chars().count() < 700, "{} chars", err.chars().count());
        }
    }

    /// A non-JSON 2xx — the proxy page again, served as `text/html` — fails the read
    /// with a capped message rather than arriving as rows or as a whole-page string.
    #[tokio::test]
    async fn a_non_json_page_fails_the_read_with_a_capped_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/activities"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<html>sign in</html>".repeat(2_000), "text/html"),
            )
            .mount(&server)
            .await;
        let err = client(&server)
            .activities(Some(1), None, None)
            .await
            .expect_err("a sign-in page is not an activity list");
        assert!(format!("{err:#}").chars().count() < 700, "{err:#}");
    }

    /// Only a full page whose oldest activity is still inside the window may have
    /// cut it short; one that reaches back past the floor holds the whole window.
    #[test]
    fn only_a_full_page_still_inside_the_window_is_reported() {
        let full = ACTIVITY_PAGE_SIZE as usize;
        assert!(!window_may_be_cut_short(
            full - 1,
            Some(2_000.0),
            Some(1_000.0)
        ));
        assert!(!window_may_be_cut_short(full, Some(999.0), Some(1_000.0)));
        assert!(window_may_be_cut_short(full, Some(1_000.0), Some(1_000.0)));
        assert!(window_may_be_cut_short(full, Some(2_000.0), Some(1_000.0)));
        assert!(
            window_may_be_cut_short(full, None, Some(1_000.0)),
            "no timestamp to compare"
        );
        assert!(
            window_may_be_cut_short(full, Some(2_000.0), None),
            "no floor: any full page may be cut short"
        );
    }
}
