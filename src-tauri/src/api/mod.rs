pub mod devices;
pub mod lookups;
pub mod patches;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, warn};

use crate::auth::AuthState;

const DEFAULT_PAGE_SIZE: u32 = 500;
const MAX_RETRIES: u8 = 3;

#[derive(Clone)]
pub struct NinjaApiClient {
    http: reqwest::Client,
    auth: AuthState,
}

impl NinjaApiClient {
    pub fn new(http: reqwest::Client, auth: AuthState) -> Self {
        Self { http, auth }
    }

    async fn request_raw(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
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
                Err(e) if e.is_timeout() && attempt < MAX_RETRIES => {
                    attempt += 1;
                    warn!(?e, attempt, "request timed out, retrying");
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt as u32))).await;
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
                // Access token may have been invalidated server-side — force a
                // refresh on the next attempt.
                attempt += 1;
                continue;
            }
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
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

    /// Cursor-paginated GET. Handles both bare array responses and the
    /// `{ results, cursor }` envelope NinjaOne uses for `/queries/*`. The cursor is
    /// accepted either as a bare string or as a `{ name, offset, ... }` object (the
    /// shape the queries endpoints return), passing the `name` back as `cursor`.
    pub async fn get_paginated<T: DeserializeOwned + Clone>(
        &self,
        path: &str,
        base_query: &[(&str, String)],
    ) -> Result<Vec<T>> {
        let mut all: Vec<T> = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut query: Vec<(&str, String)> = base_query.to_vec();
            query.push(("pageSize", DEFAULT_PAGE_SIZE.to_string()));
            if let Some(c) = &cursor {
                query.push(("cursor", c.clone()));
            }

            let raw: Value = self.request_raw(Method::GET, path, &query, None).await?;

            match raw {
                Value::Array(items) => {
                    for item in items {
                        let v: T = serde_json::from_value(item).context("deserialize page item")?;
                        all.push(v);
                    }
                    return Ok(all);
                }
                Value::Object(mut obj) => {
                    let results = obj
                        .remove("results")
                        .ok_or_else(|| anyhow!("paginated response missing `results`"))?;
                    let page_len = if let Value::Array(items) = results {
                        let len = items.len();
                        for item in items {
                            let v: T =
                                serde_json::from_value(item).context("deserialize page item")?;
                            all.push(v);
                        }
                        len
                    } else {
                        0
                    };

                    let next = next_cursor(obj.get("cursor"));
                    match next {
                        // No rows on this page means the cursor is exhausted even if
                        // the server echoes a stale token — stop to avoid a loop.
                        Some(c) if page_len > 0 => cursor = Some(c),
                        _ => return Ok(all),
                    }
                }
                Value::Null => return Ok(all),
                other => bail!("unexpected paginated body shape: {other}"),
            }
        }
    }
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

        let devices = client.devices(Some("org = 5")).await.expect("devices call");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, 10);
    }
}
