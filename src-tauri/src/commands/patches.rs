use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use chrono::{Duration, Utc};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};

use crate::api::ProgressFn;
use crate::error::UiError;
use crate::filter::FilterParams;
use crate::model::{Device, Patch, PatchStatus, PatchType};
use crate::rows::{
    LookupMaps, PatchSource, QueryResult, build_compliance, build_device_summaries, build_rows,
    pending_counts,
};
use crate::state::AppState;

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
) -> Result<QueryResult, UiError> {
    let settings = state.settings_snapshot();
    let api = state.api.clone();
    let filter = args.filter;
    // The device query honors the node-class facet (`class in (...)`); the patch/
    // install queries don't (NinjaOne's /queries/* ignore `class`), so they use a
    // class-less filter and the node-class facet is reapplied client-side via the
    // device join in build_rows.
    let device_df = filter.device_filter();
    let device_df_ref = device_df.as_deref();
    let patch_df = filter.patch_filter();
    let patch_df_ref = patch_df.as_deref();

    // 1. Classify the requested statuses. "Installed" routes to the history
    // endpoints; the rest narrow the current-patch set for display.
    let want_installed = args.statuses.iter().any(|s| s.is_installed());
    let current_status_set: HashSet<&'static str> = args
        .statuses
        .iter()
        .filter(|s| !s.is_installed())
        .map(|s| s.api_value())
        .collect();
    let include_os = args.patch_type.includes_os();
    let include_sw = args.patch_type.includes_software();
    let days = args
        .install_after_days
        .unwrap_or(settings.install_window_days)
        .max(1);
    let after = (Utc::now() - Duration::days(days)).timestamp();

    // 2. Inventory, lookups (cached), and current/installed patches are all
    // independent — fetch them concurrently so the latency is the slowest call,
    // not the sum of all of them. Conditional fetches resolve to empty when the
    // patch type / status doesn't request them.
    // Per-stream progress reporters: each emits a cumulative count tagged with its
    // stage so the UI can show live totals. `qid` (0 when the frontend omits it)
    // lets the frontend drop events from a run it has already superseded.
    let qid = query_id.unwrap_or(0);
    let app_d = app.clone();
    let p_devices = move |n: usize| emit_progress(&app_d, qid, "devices", n);
    let app_o = app.clone();
    let p_os = move |n: usize| emit_progress(&app_o, qid, "osPatches", n);
    let app_s = app.clone();
    let p_sw = move |n: usize| emit_progress(&app_s, qid, "swPatches", n);
    let app_oi = app.clone();
    let p_os_inst = move |n: usize| emit_progress(&app_oi, qid, "osInstalls", n);
    let app_si = app.clone();
    let p_sw_inst = move |n: usize| emit_progress(&app_si, qid, "swInstalls", n);

    let (devices, (orgs, locations, roles), os_current, sw_current, os_installs, sw_installs) =
        tokio::try_join!(
            api.devices(device_df_ref, Some(&p_devices as &ProgressFn)),
            state.lookups(),
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
                if want_installed && include_os {
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
                if want_installed && include_sw {
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
        )
        .map_err(UiError::from)?;

    // Fetches done; the rest is the in-memory join/rollup.
    emit_progress(&app, qid, "joining", 0);

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
        if want_installed {
            sources.push(PatchSource {
                patches: &os_installs,
                type_label: "OS",
                status_override: Some("INSTALLED"),
                status_filter: None,
            });
            sources.push(PatchSource {
                patches: &sw_installs,
                type_label: "SOFTWARE",
                status_override: Some("INSTALLED"),
                status_filter: None,
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
        settings.sla_days,
        Utc::now(),
    );

    let result = QueryResult {
        rows,
        devices: summaries,
        compliance,
        devices_total: devices.len(),
        generated_at: Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
    };

    if let Ok(mut slot) = state.last_result.lock() {
        *slot = Some(result.clone());
    }
    Ok(result)
}
