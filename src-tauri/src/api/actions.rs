//! Device *write* actions: patch scan/apply, reboot, and script dispatch.
//!
//! Everything here POSTs, so every call goes out with [`ReplaySafety::ActOnce`]
//! (via [`NinjaApiClient::post_action`] / [`NinjaApiClient::post_json`]) — a
//! timed-out dispatch is never replayed, because NinjaOne offers no idempotency
//! key and the device may already be running the job.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tracing::{debug, info};

use super::NinjaApiClient;
use crate::error::truncate_body;
use crate::model::{AutomationScript, DeviceScriptingOptions, PatchType, RebootMode};

/// What `POST /v2/device/{id}/script/run` should run. NinjaOne's
/// `RunScriptRequest` names the library-script field `id`; `scriptId` was an
/// earlier guess that returns 400 "Unrecognized field 'scriptId'".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptRef {
    /// A custom script from the automation library.
    Script { id: i64 },
    /// A NinjaOne built-in action.
    Action { uid: String },
}

impl ScriptRef {
    fn apply_to(&self, body: &mut serde_json::Map<String, Value>) {
        match self {
            Self::Script { id } => {
                body.insert("type".into(), json!("SCRIPT"));
                body.insert("id".into(), json!(id));
            }
            Self::Action { uid } => {
                body.insert("type".into(), json!("ACTION"));
                body.insert("uid".into(), json!(uid));
            }
        }
    }
}

/// What `script/run` handed back.
///
/// NinjaOne's OpenAPI documents the *request* body but not the response, and the
/// observed shape varies by tenant: a bare integer, `{ "id": … }`,
/// `{ "activityId": … }`, `{ "jobUid": … }`, a bare uuid string, or an empty 204.
/// All of them mean the dispatch succeeded — only the correlator differs, so a
/// response with no usable id is still a success and simply falls back to
/// time-window activity matching.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptDispatch {
    pub id: Option<i64>,
    pub activity_id: Option<i64>,
    pub series_uid: Option<String>,
}

impl ScriptDispatch {
    /// The best numeric correlator available, preferring the activity id.
    pub fn any_id(&self) -> Option<i64> {
        self.activity_id.or(self.id)
    }
}

impl NinjaApiClient {
    /// Queues a patch *scan*. Read-only on the device: it refreshes NinjaOne's view
    /// of what the device needs without installing anything.
    pub async fn device_patch_scan(&self, device_id: i64, family: PatchType) -> Result<()> {
        let segment = patch_family_segment(family)?;
        self.post_action(&format!("/device/{device_id}/patch/{segment}/scan"), None)
            .await
    }

    /// Queues a patch *apply*, installing every approved/applicable patch of this
    /// family on the device. NinjaOne v2 has no per-KB variant — targeting specific
    /// KBs requires a library script that accepts a KB allow list.
    pub async fn device_patch_apply(&self, device_id: i64, family: PatchType) -> Result<()> {
        let segment = patch_family_segment(family)?;
        self.post_action(&format!("/device/{device_id}/patch/{segment}/apply"), None)
            .await
    }

    /// Restarts the device. `reason` is surfaced in NinjaOne's own activity feed,
    /// which makes it a free server-side audit record — so it is required.
    pub async fn device_reboot(
        &self,
        device_id: i64,
        mode: RebootMode,
        reason: &str,
    ) -> Result<()> {
        let path = format!("/device/{device_id}/reboot/{}", mode.api_value());
        self.post_action(&path, Some(json!({ "reason": reason })))
            .await
    }

    /// Dispatches a script or built-in action. `parameters` is forwarded to the
    /// device verbatim as the combined argument line.
    pub async fn run_script(
        &self,
        device_id: i64,
        script: &ScriptRef,
        parameters: &str,
        run_as: &str,
    ) -> Result<ScriptDispatch> {
        let mut body = serde_json::Map::new();
        script.apply_to(&mut body);
        body.insert("parameters".into(), json!(parameters));
        body.insert("runAs".into(), json!(run_as));

        let raw = self
            .post_json(
                &format!("/device/{device_id}/script/run"),
                Value::Object(body),
            )
            .await?;
        let parsed = parse_dispatch_response(&raw).ok_or_else(|| {
            anyhow!(
                "script/run reported success but returned an unrecognized body: {}",
                truncate_body(&raw.to_string())
            )
        })?;
        // The response shape is undocumented and tenant-specific; log the raw body
        // so an unseen shape from the field is one grep away from a new test case.
        debug!(device_id, raw = %truncate_body(&raw.to_string()), "script/run response");
        info!(
            device_id,
            dispatch_id = ?parsed.any_id(),
            series_uid = ?parsed.series_uid,
            "script/run dispatched"
        );
        Ok(parsed)
    }

    /// The tenant's automation-script library.
    pub async fn automation_scripts(&self) -> Result<Vec<AutomationScript>> {
        self.get_json("/automation/scripts", &[]).await
    }

    /// Scripts and credential roles applicable to one device.
    pub async fn device_scripting_options(&self, device_id: i64) -> Result<DeviceScriptingOptions> {
        self.get_json(&format!("/device/{device_id}/scripting/options"), &[])
            .await
    }
}

/// The `os` / `software` path segment. [`PatchType::All`] has no endpoint of its
/// own — the caller must dispatch each family separately, so asking for it here is
/// a programming error rather than an operator-facing one.
fn patch_family_segment(family: PatchType) -> Result<&'static str> {
    match family {
        PatchType::Os => Ok("os"),
        PatchType::Software => Ok("software"),
        PatchType::All => Err(anyhow!(
            "patch actions target one family at a time; dispatch OS and software separately"
        )),
    }
}

fn parse_dispatch_response(raw: &Value) -> Option<ScriptDispatch> {
    match raw {
        Value::Object(obj) => {
            let num = |k: &str| obj.get(k).and_then(Value::as_i64);
            let text = |k: &str| {
                obj.get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            Some(ScriptDispatch {
                id: num("id"),
                activity_id: num("activityId").or_else(|| num("activity_id")),
                series_uid: text("jobUid").or_else(|| text("seriesUid")).or_else(|| {
                    // Only treat `uid` as a series uid when `id` didn't already
                    // identify the dispatch, so an echoed script uid isn't mistaken
                    // for a job correlator.
                    text("uid")
                }),
            })
        }
        Value::Number(n) => Some(ScriptDispatch {
            activity_id: n.as_i64(),
            ..Default::default()
        }),
        // Some tenants answer with the bare activity-series uid.
        Value::String(s) if !s.is_empty() => Some(ScriptDispatch {
            series_uid: Some(s.clone()),
            ..Default::default()
        }),
        // A 204 is a successful dispatch with no correlator; fall back to matching
        // the device's activity feed by time window.
        Value::Null => Some(ScriptDispatch::default()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthState;
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> NinjaApiClient {
        let http = reqwest::Client::new();
        let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
        NinjaApiClient::new(http, auth)
    }

    #[test]
    fn parses_object_with_activity_id() {
        let resp = parse_dispatch_response(&json!({ "activityId": 42 })).expect("parse");
        assert_eq!(resp.any_id(), Some(42));
    }

    #[test]
    fn parses_object_with_id_only() {
        let resp = parse_dispatch_response(&json!({ "id": 7 })).expect("parse");
        assert_eq!(resp.any_id(), Some(7));
    }

    #[test]
    fn activity_id_takes_precedence_over_id() {
        let resp = parse_dispatch_response(&json!({ "id": 7, "activityId": 42 })).expect("parse");
        assert_eq!(resp.any_id(), Some(42));
    }

    #[test]
    fn parses_bare_integer_as_activity_id() {
        let resp = parse_dispatch_response(&json!(123)).expect("parse");
        assert_eq!(resp.any_id(), Some(123));
    }

    #[test]
    fn parses_job_uid_as_series_uid() {
        let resp = parse_dispatch_response(&json!({ "jobUid": "abc-123" })).expect("parse");
        assert_eq!(resp.series_uid.as_deref(), Some("abc-123"));
        assert_eq!(resp.any_id(), None, "a uid is not a numeric correlator");
    }

    /// Replaces the reference implementation's `unknown_shape_is_rejected`: a bare
    /// uuid string is a real dispatch response, not an unparseable one.
    #[test]
    fn bare_uuid_string_is_a_series_uid() {
        let resp = parse_dispatch_response(&json!("6f1c-uuid")).expect("parse");
        assert_eq!(resp.series_uid.as_deref(), Some("6f1c-uuid"));
    }

    #[test]
    fn null_response_is_a_successful_dispatch_without_a_correlator() {
        let resp = parse_dispatch_response(&Value::Null).expect("parse");
        assert_eq!(resp.any_id(), None);
        assert_eq!(resp.series_uid, None);
    }

    #[test]
    fn unparseable_shape_is_rejected() {
        assert!(parse_dispatch_response(&json!([1, 2, 3])).is_none());
    }

    #[test]
    fn patch_family_segment_rejects_all() {
        assert_eq!(patch_family_segment(PatchType::Os).unwrap(), "os");
        assert_eq!(
            patch_family_segment(PatchType::Software).unwrap(),
            "software"
        );
        assert!(patch_family_segment(PatchType::All).is_err());
    }

    #[tokio::test]
    async fn patch_apply_posts_to_the_family_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/device/7/patch/os/apply"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        client(&server)
            .device_patch_apply(7, PatchType::Os)
            .await
            .expect("apply");
    }

    #[tokio::test]
    async fn patch_apply_surfaces_the_not_applicable_rejection() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/device/7/patch/software/apply"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string("Device is not applicable for apply."),
            )
            .mount(&server)
            .await;

        let err = client(&server)
            .device_patch_apply(7, PatchType::Software)
            .await
            .expect_err("400 must surface");
        assert!(
            err.to_string().contains("not applicable"),
            "operator needs the server's reason, got: {err}"
        );
    }

    #[tokio::test]
    async fn reboot_sends_the_mode_segment_and_reason() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/device/9/reboot/FORCED"))
            .and(body_json(json!({ "reason": "monthly patching" })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        client(&server)
            .device_reboot(9, RebootMode::Forced, "monthly patching")
            .await
            .expect("reboot");
    }

    #[tokio::test]
    async fn run_script_sends_id_not_script_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/device/3/script/run"))
            .and(body_json(json!({
                "type": "SCRIPT",
                "id": 55,
                "parameters": "kbAllowList=5040434 dryRun=true",
                "runAs": "system",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "activityId": 900 })))
            .expect(1)
            .mount(&server)
            .await;

        let out = client(&server)
            .run_script(
                3,
                &ScriptRef::Script { id: 55 },
                "kbAllowList=5040434 dryRun=true",
                "system",
            )
            .await
            .expect("dispatch");
        assert_eq!(out.any_id(), Some(900));
    }

    #[tokio::test]
    async fn run_script_accepts_a_204_with_no_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/device/3/script/run"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let out = client(&server)
            .run_script(3, &ScriptRef::Script { id: 1 }, "", "system")
            .await
            .expect("a bodiless 204 is still a successful dispatch");
        assert_eq!(out.any_id(), None);
    }

    #[tokio::test]
    async fn automation_scripts_flags_kb_allow_list_support() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/automation/scripts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "id": 1,
                    "name": "Install-CriticalSecurityUpdates",
                    "scriptVariables": [{ "name": "kbAllowList" }, { "name": "dryRun" }],
                },
                { "id": 2, "name": "Repair-WindowsUpdate" },
                { "id": 3, "name": "Positional", "scriptParameters": ["kbAllowList"] },
            ])))
            .mount(&server)
            .await;

        let scripts = client(&server).automation_scripts().await.expect("scripts");
        assert_eq!(scripts.len(), 3);
        assert!(scripts[0].accepts_kb_allow_list());
        assert!(
            !scripts[1].accepts_kb_allow_list(),
            "a script with no kbAllowList must not be offered per-KB targeting"
        );
        assert!(
            scripts[2].accepts_kb_allow_list(),
            "a positional script parameter counts too"
        );
    }
}
