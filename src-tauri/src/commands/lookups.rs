use serde::Serialize;
use tauri::State;

use crate::error::UiError;
use crate::model::{Location, Organization, Role};
use crate::state::AppState;

#[tauri::command]
pub async fn list_orgs(state: State<'_, AppState>) -> Result<Vec<Organization>, UiError> {
    state.api.organizations().await.map_err(UiError::from)
}

#[tauri::command]
pub async fn list_locations(
    state: State<'_, AppState>,
    org_id: i64,
) -> Result<Vec<Location>, UiError> {
    state.api.locations(org_id).await.map_err(UiError::from)
}

#[tauri::command]
pub async fn list_roles(state: State<'_, AppState>) -> Result<Vec<Role>, UiError> {
    state.api.roles().await.map_err(UiError::from)
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
