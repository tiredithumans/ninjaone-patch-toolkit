//! Auto-update commands. Thin wrappers over `tauri-plugin-updater` so the frontend
//! can show a changelog splash and trigger the install. The updater fetches the
//! signed `latest.json` from the GitHub releases endpoint configured in
//! `tauri.conf.json` (backend egress — not subject to the webview CSP).

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

use crate::error::UiError;

/// Update metadata surfaced to the frontend splash. `notes` is the published
/// release body (the changelog).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
}

/// Checks the configured endpoint for a newer release. Returns `None` when the
/// installed version is current.
#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<Option<UpdateInfo>, UiError> {
    let updater = app
        .updater()
        .map_err(|e| UiError::new(format!("updater unavailable: {e}")))?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(UpdateInfo {
            version: update.version.clone(),
            current_version: update.current_version.clone(),
            notes: update.body.clone(),
        })),
        Ok(None) => Ok(None),
        Err(e) => Err(UiError::new(format!("update check failed: {e}"))),
    }
}

/// Downloads and installs the available update, then relaunches the app. Re-checks
/// rather than holding the `Update` across IPC calls. On success the process
/// restarts, so this never returns `Ok`; an error means the install failed.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), UiError> {
    let updater = app
        .updater()
        .map_err(|e| UiError::new(format!("updater unavailable: {e}")))?;
    let update = updater
        .check()
        .await
        .map_err(|e| UiError::new(format!("update check failed: {e}")))?
        .ok_or_else(|| UiError::new("no update available"))?;
    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| UiError::new(format!("update install failed: {e}")))?;
    app.restart();
}
