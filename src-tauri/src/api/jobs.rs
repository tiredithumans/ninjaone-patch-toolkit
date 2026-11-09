//! Jobs NinjaOne reports as running, for the live job status the app displays.
//!
//! Display only. A dispatched action still resolves from `/activities` alone
//! (`api::activities`): `/jobs` lists what is in flight, and a job leaving it says
//! nothing about how it ended.

use anyhow::{Context, Result, bail};
use serde_json::value::RawValue;

use super::{NinjaApiClient, lenient_rows};
use crate::model::RunningJob;

impl NinjaApiClient {
    /// Every running job in the tenant. One request: the spec declares no paging,
    /// only `df`, `jobType`, `lang` and `tz`. Decoded off the runtime, job by job, so
    /// one malformed job cannot hide the rest ([`parse_jobs`]).
    pub async fn running_jobs(&self) -> Result<Vec<RunningJob>> {
        self.get_decoded("/jobs", &[], parse_jobs).await
    }
}

/// Reads a `/jobs` response: a bare array, each job decoded on its own straight into
/// [`RunningJob`]. The node record and `data` each job carries are skipped as the
/// array is scanned rather than built, which is most of every job's bytes.
fn parse_jobs(body: &[u8]) -> Result<Vec<RunningJob>> {
    let body = body.trim_ascii();
    if body.first() != Some(&b'[') {
        bail!("the jobs response was not a list");
    }
    let jobs: Vec<&RawValue> = serde_json::from_slice(body).context("decode the jobs list")?;
    Ok(lenient_rows(jobs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthState;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn a_malformed_job_is_dropped_not_the_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "uid": "0d1e3c8a-6f0b-4b7e-9c51-2a7b8d9e0f11",
                    "deviceId": 10,
                    "message": "Installing updates",
                    "createTime": 1_782_480_000.0,
                    "updateTime": 1_782_480_060.0,
                    "sourceType": "SCHEDULED_TASK",
                    "sourceName": "Patch window",
                    "subject": "OS patch installation",
                    "jobStatus": "IN_PROCESS",
                    "jobType": "PATCH_MANAGEMENT",
                    "data": { "anything": true },
                    "device": { "id": 10, "systemName": "web-01" }
                },
                { "deviceId": "not a number" }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
        let jobs = NinjaApiClient::new(http, auth)
            .running_jobs()
            .await
            .expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].device_id, Some(10));
        assert_eq!(jobs[0].job_type.as_deref(), Some("PATCH_MANAGEMENT"));
        assert_eq!(jobs[0].job_status.as_deref(), Some("IN_PROCESS"));
        assert!(jobs[0].started_at().is_some());
    }

    /// Anything but a list fails the read without echoing the body — a proxy's
    /// sign-in page included — and an entry that is not a job is dropped like a
    /// malformed one.
    #[test]
    fn only_a_list_of_jobs_is_read() {
        let page = "<html>sign in</html>".repeat(2_000);
        let bodies: [&[u8]; 4] = [page.as_bytes(), b"{}", b"", b"null"];
        for body in bodies {
            let err = parse_jobs(body).expect_err("not a list");
            assert_eq!(format!("{err:#}"), "the jobs response was not a list");
        }
        let jobs =
            parse_jobs(br#" [null, 3, {"deviceId": 4, "jobType": "SCRIPTING"}] "#).expect("a list");
        assert_eq!(
            jobs.iter().map(|j| j.device_id).collect::<Vec<_>>(),
            [Some(4)]
        );
    }
}
