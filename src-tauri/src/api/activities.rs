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

impl NinjaApiClient {
    /// Activity log, newest first. `newer_than` is a Unix timestamp in **seconds**.
    ///
    /// The response is a bare array on most tenants but an `{ "activities": [...] }`
    /// envelope on others, so both are accepted.
    pub async fn activities(
        &self,
        device_id: Option<i64>,
        newer_than: Option<i64>,
    ) -> Result<Vec<Activity>> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(id) = device_id {
            query.push(("df", format!("id = {id}")));
        }
        if let Some(after) = newer_than {
            query.push(("newerThan", after.to_string()));
        }

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

        // A single malformed entry shouldn't blind the poller to every other job on
        // the device, so unparseable rows are dropped rather than failing the call.
        Ok(items
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Activity>(v).ok())
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
            .and(query_param("df", "id = 42"))
            .and(query_param("newerThan", "1700"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 1, "activityType": "SCRIPT", "status": "COMPLETED",
                  "result": { "exitCode": 0 } },
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
                { "id": 3, "status": "FAILED" },
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
                { "id": 4, "status": "FAILED", "result": { "resultCode": 2 } },
            ])))
            .mount(&server)
            .await;

        let list = client(&server).activities(None, None).await.expect("list");
        assert_eq!(list[0].exit_code(), Some(2));
    }
}
