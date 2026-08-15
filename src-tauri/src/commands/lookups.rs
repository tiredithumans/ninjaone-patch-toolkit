use serde::Serialize;
use tauri::State;

use crate::error::UiError;
use crate::model::{Location, Organization, Role};
use crate::state::AppState;

/// The three reference lists all come from the same tenant-scoped cache
/// (`AppState::lookups`), which the device→row join already populates. Fetching them
/// per command instead meant startup pulled organizations and roles twice — once for
/// the dropdowns and again for the join — and a scope change re-fetched locations
/// every time.
#[tauri::command]
pub async fn list_orgs(state: State<'_, AppState>) -> Result<Vec<Organization>, UiError> {
    let (orgs, _, _) = state.lookups().await.map_err(UiError::from)?;
    Ok(orgs.as_ref().clone())
}

/// Locations belonging to any of `org_ids`; an empty list means every organization.
///
/// Served from the same cache, rather than from `/organization/{id}/locations` per
/// selected org. With the organization facet multi-select, the per-org endpoint would
/// have meant one round trip per tick of a checkbox — and the rows it returns are
/// already in memory, `organizationId` and all.
#[tauri::command]
pub async fn list_locations(
    state: State<'_, AppState>,
    org_ids: Vec<i64>,
) -> Result<Vec<Location>, UiError> {
    let (_, locations, _) = state.lookups().await.map_err(UiError::from)?;
    Ok(locations
        .iter()
        .filter(|l| org_ids.is_empty() || l.organization_id.is_some_and(|id| org_ids.contains(&id)))
        .cloned()
        .collect())
}

#[tauri::command]
pub async fn list_roles(state: State<'_, AppState>) -> Result<Vec<Role>, UiError> {
    let (_, _, roles) = state.lookups().await.map_err(UiError::from)?;
    Ok(roles.as_ref().clone())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeClass {
    pub value: &'static str,
    pub label: &'static str,
}

/// The patch-relevant NinjaOne node classes offered as the coarse "OS Type" facet.
#[tauri::command]
pub fn list_node_classes() -> Vec<NodeClass> {
    [
        ("WINDOWS_SERVER", "Windows Server"),
        ("WINDOWS_WORKSTATION", "Windows Workstation"),
        ("MAC_SERVER", "macOS Server"),
        ("MAC", "macOS"),
        ("LINUX_SERVER", "Linux Server"),
        ("LINUX_WORKSTATION", "Linux Workstation"),
    ]
    .into_iter()
    .map(|(value, label)| NodeClass { value, label })
    .collect()
}
