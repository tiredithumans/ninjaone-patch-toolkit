use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};
use tracing::warn;

use crate::api::{NinjaApiClient, ProgressFn};
use crate::error::UiError;
use crate::filter::FilterParams;
use crate::model::{Device, Location, Organization, Patch, PatchRow, PatchStatus, PatchType, Role};
use crate::rows::{
    LookupMaps, PatchSource, QueryResult, QuerySummary, build_compliance, build_device_summaries,
    build_rows, pending_counts,
};
use crate::state::AppState;

/// The org/location/role lookups a query joins against, each shared behind `Arc`
/// so a cache hit hands out a cheap refcount bump instead of a deep clone.
type Lookups = (Arc<Vec<Organization>>, Arc<Vec<Location>>, Arc<Vec<Role>>);

/// Size of the first page of detail rows returned inline by `query_patches`. Must
/// match the frontend's `PATCHES_PAGE_SIZE` so the seeded page fills the table's
/// first page exactly (later pages come from `get_patch_rows`).
const FIRST_PAGE_ROWS: usize = 100;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchQueryArgs {
    pub filter: FilterParams,
    pub patch_type: PatchType,
    pub statuses: Vec<PatchStatus>,
    /// Overrides the configured install-history lookback window (days).
    #[serde(default)]
    pub install_after_days: Option<i64>,
}

/// Incremental progress for an in-flight `query_patches`, emitted on the
/// `query:progress` event so the UI can show live record counts. `query_id`
/// echoes the value the frontend passed so it can drop events from a superseded
/// run.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryProgressEvent {
    query_id: u64,
    stage: &'static str,
    loaded: usize,
}

/// Best-effort emit of a progress event (a dropped event just means one fewer UI
/// update, never a failed query).
fn emit_progress(app: &AppHandle, query_id: u64, stage: &'static str, loaded: usize) {
    let _ = app.emit(
        "query:progress",
        QueryProgressEvent {
            query_id,
            stage,
            loaded,
        },
    );
}

/// Fetches devices and patches for the chosen filter/type/status, joins them into
/// per-server detail rows, and computes the reboot/compliance rollups. The result
/// is cached for the Excel exporter.
#[tauri::command]
pub async fn query_patches(
    state: State<'_, AppState>,
    app: AppHandle,
    args: PatchQueryArgs,
    query_id: Option<u64>,
) -> Result<QuerySummary, UiError> {
    let settings = state.settings_snapshot();
    // `qid` (0 when the frontend omits it) lets the frontend drop progress events
    // tagged with a run it has already superseded.
    let qid = query_id.unwrap_or(0);
    let progress =
        move |stage: &'static str, loaded: usize| emit_progress(&app, qid, stage, loaded);

    // The org/location/role lookups are served from `AppState`'s short-TTL cache;
    // the fetch→join→rollup itself lives in `run_query`, which takes them as a
    // future so they still resolve concurrently with the inventory/patch fetches.
    let result = run_query(
        &state.api,
        state.lookups(),
        settings.install_window_days,
        settings.sla_days,
        args,
        Utc::now(),
        &progress,
    )
    .await
    .map_err(UiError::from)?;

    // Hand the frontend a lightweight summary (first page + rollups) and keep the
    // full result in the cache for paging (`get_patch_rows`) and export — moving it
    // in rather than cloning every row.
    let summary = QuerySummary::from_result(&result, FIRST_PAGE_ROWS);
    match state.last_result.lock() {
        Ok(mut slot) => *slot = Some(result),
        // A poisoned cache means export/paging would read the previous run — warn
        // rather than silently dropping the write so the staleness is observable.
        Err(_) => warn!("result cache poisoned; export and paging will use the prior query"),
    }
    Ok(summary)
}

/// The fetch→join→rollup core of [`query_patches`], split out so it can be driven
/// in tests against a mock NinjaOne server without a Tauri `AppHandle`/`State`.
///
/// `lookups` is taken as a *future* (not a resolved value) so the org/location/role
/// fetch runs concurrently with the inventory and patch fetches — `query_patches`
/// passes `AppState::lookups()` (the cached fetch); a test passes a ready tuple.
/// `progress` (the UI sink, keyed by stage) and `now` (the clock, for the release/
/// install windows, SLA aging, and `generated_at`) are injected so the caller owns
/// both, keeping this function deterministic and side-effect free.
#[allow(clippy::too_many_arguments)]
async fn run_query<L>(
    api: &NinjaApiClient,
    lookups: L,
    install_window_days: i64,
    sla_days: i64,
    args: PatchQueryArgs,
    now: DateTime<Utc>,
    progress: &(dyn Fn(&'static str, usize) + Send + Sync),
) -> anyhow::Result<QueryResult>
where
    L: std::future::Future<Output = anyhow::Result<Lookups>>,
{
    let mut filter = args.filter;
    // Resolve the relative release-date window into an absolute lower bound; the
    // filter is applied client-side in build_rows, which has no clock.
    if let Some(days) = filter.release_within_days {
        filter.release_after = Some((now - Duration::days(days.max(0))).timestamp());
    }
    // The device query honors the node-class facet (`class in (...)`); the patch/
    // install queries don't (NinjaOne's /queries/* ignore `class`), so they use a
    // class-less filter and the node-class facet is reapplied client-side via the
    // device join in build_rows.
    let device_df = filter.device_filter();
    let device_df_ref = device_df.as_deref();
    let patch_df = filter.patch_filter();
    let patch_df_ref = patch_df.as_deref();

    // 1. Classify the requested statuses. Install *results* ("Installed" and
    // "Failed") route to the `*-patch-installs` history endpoints; the rest
    // (MANUAL/APPROVED/REJECTED) narrow the current-patch feed for display. A
    // FAILED patch is one whose install was attempted and failed, so it never
    // appears in the current feed ("patches for which there were no installation
    // attempts") — only in the install history (status FAILED/INSTALLED).
    let want_installs = args.statuses.iter().any(|s| s.is_install_history());
    let current_status_set: HashSet<&'static str> = args
        .statuses
        .iter()
        .filter(|s| !s.is_install_history())
        .map(|s| s.api_value())
        .collect();
    // The install-history statuses the operator asked for (INSTALLED and/or
    // FAILED); the install sources are narrowed to these client-side.
    let install_status_set: HashSet<&'static str> = args
        .statuses
        .iter()
        .filter(|s| s.is_install_history())
        .map(|s| s.api_value())
        .collect();
    let include_os = args.patch_type.includes_os();
    let include_sw = args.patch_type.includes_software();
    // The configured window is validated >= 1 in save_settings; clamp the optional
    // per-query override the same way so a 0/negative lookback can't invert into a
    // future `after` bound that would match no install history.
    let days = args
        .install_after_days
        .unwrap_or(install_window_days)
        .max(1);
    let after = (now - Duration::days(days)).timestamp();

    // 2. Inventory, lookups (cached), and current/installed patches are all
    // independent — fetch them concurrently so the latency is the slowest call,
    // not the sum of all of them. Conditional fetches resolve to empty when the
    // patch type / status doesn't request them.
    // Per-stream progress reporters: each emits a cumulative count tagged with its
    // stage so the UI can show live totals.
    let p_devices = |n: usize| progress("devices", n);
    let p_os = |n: usize| progress("osPatches", n);
    let p_sw = |n: usize| progress("swPatches", n);
    let p_os_inst = |n: usize| progress("osInstalls", n);
    let p_sw_inst = |n: usize| progress("swInstalls", n);

    let (devices, (orgs, locations, roles), os_current, sw_current, os_installs, sw_installs) = tokio::try_join!(
        api.devices(device_df_ref, Some(&p_devices as &ProgressFn)),
        lookups,
        async {
            if include_os {
                api.fleet_os_patches(patch_df_ref, None, Some(&p_os as &ProgressFn))
                    .await
            } else {
                Ok(Vec::new())
            }
        },
        async {
            if include_sw {
                api.fleet_software_patches(patch_df_ref, None, Some(&p_sw as &ProgressFn))
                    .await
            } else {
                Ok(Vec::new())
            }
        },
        async {
            if want_installs && include_os {
                api.fleet_os_patch_installs(
                    patch_df_ref,
                    after,
                    None,
                    Some(&p_os_inst as &ProgressFn),
                )
                .await
            } else {
                Ok(Vec::new())
            }
        },
        async {
            if want_installs && include_sw {
                api.fleet_software_patch_installs(
                    patch_df_ref,
                    after,
                    None,
                    Some(&p_sw_inst as &ProgressFn),
                )
                .await
            } else {
                Ok(Vec::new())
            }
        },
    )?;

    // Fetches done; the rest is the in-memory join/rollup.
    progress("joining", 0);

    let maps = LookupMaps::build(&orgs, &locations, &roles);
    let devices_by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();

    // 5/6. Build detail rows directly from the fetched families. The current-patch
    // sources carry the requested-status filter so build_rows narrows them in
    // place — no need to clone the matched subset out before joining. The borrow
    // ends with the block so the families can then move into `all_current`.
    let mut rows = {
        let mut sources = vec![
            PatchSource {
                patches: &os_current,
                type_label: "OS",
                status_override: None,
                status_filter: Some(&current_status_set),
            },
            PatchSource {
                patches: &sw_current,
                type_label: "SOFTWARE",
                status_override: None,
                status_filter: Some(&current_status_set),
            },
        ];
        if want_installs {
            // The install endpoints return both successful and failed records, so
            // narrow each to the requested install statuses; the override labels a
            // record that omits its own status (defaulting it to INSTALLED).
            sources.push(PatchSource {
                patches: &os_installs,
                type_label: "OS",
                status_override: Some("INSTALLED"),
                status_filter: Some(&install_status_set),
            });
            sources.push(PatchSource {
                patches: &sw_installs,
                type_label: "SOFTWARE",
                status_override: Some("INSTALLED"),
                status_filter: Some(&install_status_set),
            });
        }
        build_rows(&devices_by_id, &maps, &sources, &filter)
    };
    // Highest severity first, then organization, then device — case-insensitive.
    // sort_by_cached_key lowercases each field once instead of on every compare.
    rows.sort_by_cached_key(|r| {
        (
            Reverse(r.severity_rank),
            r.organization.to_lowercase(),
            r.device_name.to_lowercase(),
        )
    });

    // 7. Compliance + reboot rollups from the complete current set.
    let all_current: Vec<Patch> = os_current.into_iter().chain(sw_current).collect();
    let counts = pending_counts(&all_current);
    let summaries = build_device_summaries(&devices, &counts, &maps);
    let compliance = build_compliance(
        &summaries,
        &all_current,
        &devices_by_id,
        &maps,
        sla_days,
        now,
    );

    Ok(QueryResult {
        rows,
        devices: summaries,
        compliance,
        devices_total: devices.len(),
        generated_at: now.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
    })
}

/// Serves one page of detail rows from the cached query result so the frontend can
/// page through a large fleet without receiving every row over IPC. Returns an
/// empty page when there is no cached result or the offset is past the end.
#[tauri::command]
pub async fn get_patch_rows(
    state: State<'_, AppState>,
    offset: usize,
    limit: usize,
) -> Result<Vec<PatchRow>, UiError> {
    let slot = state
        .last_result
        .lock()
        .map_err(|_| UiError::new("result cache poisoned"))?;
    let rows = slot
        .as_ref()
        .map(|r| r.rows.iter().skip(offset).take(limit).cloned().collect())
        .unwrap_or_default();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthState;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A fixed clock so the release/install windows, SLA aging, and `generated_at`
    /// are deterministic regardless of when the test runs.
    fn fixed_now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap() // 2023-11-14T22:13:20Z
    }

    /// Lookups resolved up front — the org/location/role *fetch* is covered by the
    /// `api::mod` tests; here they only need to label rows so the join is assertable.
    fn lookups() -> Lookups {
        (
            Arc::new(vec![Organization {
                id: 1,
                name: "Alpha".into(),
            }]),
            Arc::new(vec![]),
            Arc::new(vec![]),
        )
    }

    fn client(server: &MockServer) -> NinjaApiClient {
        let http = reqwest::Client::new();
        let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
        NinjaApiClient::new(http, auth)
    }

    fn args(patch_type: PatchType, statuses: Vec<PatchStatus>) -> PatchQueryArgs {
        PatchQueryArgs {
            filter: FilterParams::default(),
            patch_type,
            statuses,
            install_after_days: None,
        }
    }

    #[tokio::test]
    async fn pending_query_joins_current_feed_and_maps_manual_to_pending() {
        let server = MockServer::start().await;

        // Two online devices in org Alpha; device 10 needs a reboot.
        Mock::given(method("GET"))
            .and(path("/api/v2/devices-detailed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "id": 10, "systemName": "web-01", "organizationId": 1,
                    "offline": false, "os": { "name": "Windows Server 2022", "needsReboot": true }
                },
                {
                    "id": 20, "systemName": "web-02", "organizationId": 1,
                    "offline": false, "os": { "name": "Windows Server 2019", "needsReboot": false }
                }
            ])))
            .mount(&server)
            .await;

        // Current OS-patch feed: one MANUAL (pending) Critical aged past SLA, one
        // APPROVED Low. With statuses=[Pending] only the MANUAL one becomes a row.
        let aged = fixed_now().timestamp() - 60 * 86_400; // 60 days old
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "deviceId": 10, "kbNumber": "KB1", "status": "MANUAL",
                      "severity": "CRITICAL", "releaseDate": aged },
                    { "deviceId": 20, "kbNumber": "KB2", "status": "APPROVED",
                      "severity": "LOW", "releaseDate": aged }
                ],
                "cursor": ""
            })))
            .mount(&server)
            .await;

        let progress = |_: &'static str, _: usize| {};
        let result = run_query(
            &client(&server),
            async { Ok::<_, anyhow::Error>(lookups()) },
            30,
            30,
            args(PatchType::Os, vec![PatchStatus::Pending]),
            fixed_now(),
            &progress,
        )
        .await
        .expect("query");

        // Only the MANUAL patch survives the Pending filter, displayed as PENDING.
        assert_eq!(result.rows.len(), 1);
        let row = &result.rows[0];
        assert_eq!(row.kb.as_deref(), Some("KB1"));
        assert_eq!(row.status, "PENDING");
        assert_eq!(row.organization, "Alpha");
        assert_eq!(row.severity, "Critical");
        assert!(row.needs_reboot);

        // Both devices counted; only device 10 lands in the reboot subset.
        assert_eq!(result.devices_total, 2);
        let summary = QuerySummary::from_result(&result, FIRST_PAGE_ROWS);
        assert_eq!(summary.reboot_devices.len(), 1);
        assert_eq!(summary.reboot_devices[0].device_id, 10);

        // Compliance: both online, both carry a pending/approved patch → 0% compliant.
        // The MANUAL Critical (aged) lands in pending_critical AND aged_critical; the
        // APPROVED Low is below the Important rank so neither counts it.
        assert_eq!(result.compliance.len(), 1);
        let alpha = &result.compliance[0];
        assert_eq!(alpha.organization, "Alpha");
        assert_eq!(alpha.devices_total, 2);
        assert_eq!(alpha.devices_compliant, 0);
        assert_eq!(alpha.compliance_pct, 0.0);
        assert_eq!(alpha.pending_critical, 1);
        assert_eq!(alpha.aged_critical, 1);
    }

    #[tokio::test]
    async fn installed_query_routes_to_history_endpoint_not_current_feed() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v2/devices-detailed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false }
            ])))
            .mount(&server)
            .await;

        // The current feed is still fetched (include_os) but must contribute no rows
        // for an install-only status — this MANUAL record has to be ignored.
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "deviceId": 10, "kbNumber": "KBCURRENT", "status": "MANUAL",
                      "severity": "CRITICAL" }
                ],
                "cursor": ""
            })))
            .mount(&server)
            .await;

        // The install-history endpoint returns one INSTALLED and one FAILED record;
        // statuses=[Installed] keeps only the INSTALLED one.
        let installed = fixed_now().timestamp() - 5 * 86_400;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patch-installs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "deviceId": 10, "kbNumber": "KBOK", "status": "INSTALLED",
                      "installedAt": installed },
                    { "deviceId": 10, "kbNumber": "KBBAD", "status": "FAILED" }
                ],
                "cursor": ""
            })))
            .mount(&server)
            .await;

        let progress = |_: &'static str, _: usize| {};
        let result = run_query(
            &client(&server),
            async { Ok::<_, anyhow::Error>(lookups()) },
            30,
            30,
            args(PatchType::Os, vec![PatchStatus::Installed]),
            fixed_now(),
            &progress,
        )
        .await
        .expect("query");

        // Exactly the install-history INSTALLED record — the current MANUAL row and
        // the FAILED install are both excluded.
        assert_eq!(result.rows.len(), 1);
        let row = &result.rows[0];
        assert_eq!(row.kb.as_deref(), Some("KBOK"));
        assert_eq!(row.status, "INSTALLED");
        assert!(row.installed_date.is_some());
    }
}
