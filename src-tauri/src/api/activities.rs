//! Reading back what a dispatched action did.
//!
//! NinjaOne v2 has **no script-output endpoint**, so a job is resolved from the
//! activity feed, which carries the terminal state plus an exit code. (`/jobs`
//! would only report what is still in flight — the *absence* of a terminal
//! activity already tells us that, so polling it too would buy nothing.)

use anyhow::{Result, bail};
use serde_json::Value;

use super::NinjaApiClient;
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
    /// The response is a bare array on most tenants but an `{ "activities": [...] }`
    /// envelope on others, so both are accepted.
    ///
    /// Scoped to one device, so the [`ACTIVITY_PAGE_SIZE`] window is ample in
    /// practice. A device noisy enough to overflow it within a job's timeout would
    /// push the terminal activity out of reach, so that case is logged rather than
    /// left to look like a job that simply never finished — the job itself still
    /// resolves via its timeout.
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
        since_ts: Option<i64>,
    ) -> Result<Vec<Activity>> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(id) = device_id {
            // No spaces around `=`: the documented grammar is `id=<DeviceID>`, and
            // its worked example is `df=class%3DWINDOWS_SERVER%20AND%20offline`.
            query.push(("df", format!("id={id}")));
        }
        query.push(("pageSize", ACTIVITY_PAGE_SIZE.to_string()));

        let raw: Value = self.get_json("/activities", &query).await?;
        let items = match raw {
            Value::Array(items) => items,
            Value::Object(mut obj) => match obj.remove("activities") {
                Some(Value::Array(items)) => items,
                Some(other) => bail!("activities field must be an array, got: {other}"),
                None => bail!("activities response missing an `activities` field"),
            },
            other => bail!("unexpected activities response shape: {other}"),
        };

        if items.len() >= ACTIVITY_PAGE_SIZE as usize {
            // A full page means the window may be truncated and a terminal activity
            // could be out of view. Surfaced so a job that then times out has a
            // logged cause instead of looking like the device never answered.
            tracing::warn!(
                device_id,
                returned = items.len(),
                "activity page came back full; a terminal activity may be beyond the window"
            );
        }

        // A single malformed entry shouldn't blind the poller to every other job on
        // the device, so unparseable rows are dropped rather than failing the call.
        let floor = since_ts.map(|t| t as f64);
        Ok(items
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Activity>(v).ok())
            // An activity with no timestamp is kept: it cannot be shown to predate
            // the dispatch, and dropping it would lose a terminal state.
            .filter(|a| match (floor, a.activity_time) {
                (Some(floor), Some(ts)) => ts >= floor,
                _ => true,
            })
            .collect())
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
            .activities(Some(42), Some(1700))
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
            .activities(Some(42), Some(1_500))
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

        let list = client(&server).activities(None, None).await.expect("list");
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

        let list = client(&server).activities(None, None).await.expect("list");
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

        let list = client(&server).activities(None, None).await.expect("list");
        assert_eq!(list[0].exit_code(), Some(2));
    }
}
