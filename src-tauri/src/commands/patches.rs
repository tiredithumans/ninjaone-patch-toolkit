use std::collections::{HashMap, HashSet};

use chrono::{Duration, Utc};
use serde::Deserialize;
use tauri::State;

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

/// Fetches devices and patches for the chosen filter/type/status, joins them into
/// per-server detail rows, and computes the reboot/compliance rollups. The result
/// is cached for the Excel exporter.
#[tauri::command]
pub async fn query_patches(
    state: State<'_, AppState>,
    args: PatchQueryArgs,
) -> Result<QueryResult, UiError> {
    let settings = state.settings_snapshot();
    let api = state.api.clone();
    let filter = args.filter;
    let df = filter.device_filter();
    let df_ref = df.as_deref();

    // 1. Inventory + lookups.
    let devices = api.devices(df_ref).await.map_err(UiError::from)?;
    let orgs = api.organizations().await.map_err(UiError::from)?;
    let locations = api.all_locations().await.unwrap_or_default();
    let roles = api.roles().await.map_err(UiError::from)?;
    let maps = LookupMaps::build(&orgs, &locations, &roles);
    let devices_by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();

    // 2. Split requested statuses: Installed routes to the history endpoints.
    let want_installed = args.statuses.iter().any(|s| s.is_installed());
    let current_status_set: HashSet<&'static str> = args
        .statuses
        .iter()
        .filter(|s| !s.is_installed())
        .map(|s| s.api_value())
        .collect();

    // 3. Current patches — always fetched (drives compliance + pending counts).
    let os_current = if args.patch_type.includes_os() {
        api.fleet_os_patches(df_ref, None)
            .await
            .map_err(UiError::from)?
    } else {
        Vec::new()
    };
    let sw_current = if args.patch_type.includes_software() {
        api.fleet_software_patches(df_ref, None)
            .await
            .map_err(UiError::from)?
    } else {
        Vec::new()
    };

    // 4. Install history — only when the operator asked for Installed.
    let (os_installs, sw_installs) = if want_installed {
        let days = args
            .install_after_days
            .unwrap_or(settings.install_window_days)
            .max(1);
        let after = (Utc::now() - Duration::days(days)).timestamp();
        let osi = if args.patch_type.includes_os() {
            api.fleet_os_patch_installs(df_ref, after, None)
                .await
                .map_err(UiError::from)?
        } else {
            Vec::new()
        };
        let swi = if args.patch_type.includes_software() {
            api.fleet_software_patch_installs(df_ref, after, None)
                .await
                .map_err(UiError::from)?
        } else {
            Vec::new()
        };
        (osi, swi)
    } else {
        (Vec::new(), Vec::new())
    };

    // 5. Narrow current patches to the requested non-installed statuses for display.
    let status_match = |p: &Patch| {
        p.status
            .as_deref()
            .map(|s| current_status_set.contains(s))
            .unwrap_or(false)
    };
    let os_display: Vec<Patch> = os_current
        .iter()
        .filter(|p| status_match(p))
        .cloned()
        .collect();
    let sw_display: Vec<Patch> = sw_current
        .iter()
        .filter(|p| status_match(p))
        .cloned()
        .collect();

    // 6. Build detail rows from every source.
    let mut sources = vec![
        PatchSource {
            patches: &os_display,
            type_label: "OS",
            status_override: None,
        },
        PatchSource {
            patches: &sw_display,
            type_label: "SOFTWARE",
            status_override: None,
        },
    ];
    if want_installed {
        sources.push(PatchSource {
            patches: &os_installs,
            type_label: "OS",
            status_override: Some("INSTALLED"),
        });
        sources.push(PatchSource {
            patches: &sw_installs,
            type_label: "SOFTWARE",
            status_override: Some("INSTALLED"),
        });
    }
    let mut rows = build_rows(&devices_by_id, &maps, &sources, &filter);
    rows.sort_by(|a, b| {
        b.severity_rank
            .cmp(&a.severity_rank)
            .then_with(|| {
                a.organization
                    .to_lowercase()
                    .cmp(&b.organization.to_lowercase())
            })
            .then_with(|| {
                a.device_name
                    .to_lowercase()
                    .cmp(&b.device_name.to_lowercase())
            })
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
