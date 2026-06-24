use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::UiError;
use crate::settings::Preset;
use crate::state::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub instance_base_url: String,
    pub client_id: Option<String>,
    pub callback_port: u16,
    pub install_window_days: i64,
    pub sla_days: i64,
    pub has_client_secret: bool,
    pub presets: Vec<Preset>,
    pub auto_check_updates: bool,
}

fn view(state: &AppState) -> SettingsView {
    let s = state.settings_snapshot();
    SettingsView {
        instance_base_url: s.instance_base_url,
        client_id: s.client_id,
        callback_port: s.callback_port,
        install_window_days: s.install_window_days,
        sla_days: s.sla_days,
        has_client_secret: state.auth.has_client_secret(),
        presets: s.presets,
        auto_check_updates: s.auto_check_updates,
    }
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsView {
    view(&state)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSettingsArgs {
    pub instance_base_url: String,
    pub client_id: Option<String>,
    pub callback_port: u16,
    pub install_window_days: i64,
    pub sla_days: i64,
    /// New secret to store; ignored when empty/None unless `clear_secret` is set.
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub clear_secret: bool,
    #[serde(default = "default_auto_check")]
    pub auto_check_updates: bool,
}

fn default_auto_check() -> bool {
    true
}

#[tauri::command]
pub fn save_settings(
    state: State<'_, AppState>,
    args: SaveSettingsArgs,
) -> Result<SettingsView, UiError> {
    let snapshot = {
        let mut guard = state
            .settings
            .lock()
            .map_err(|_| UiError::new("settings state poisoned"))?;
        guard.instance_base_url = args
            .instance_base_url
            .trim()
            .trim_end_matches('/')
            .to_string();
        guard.client_id = args
            .client_id
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
        guard.callback_port = args.callback_port;
        guard.install_window_days = args.install_window_days.max(1);
        guard.sla_days = args.sla_days.max(1);
        guard.auto_check_updates = args.auto_check_updates;
        guard.save().map_err(UiError::from)?;
        guard.clone()
    };

    state.auth.apply_settings(
        snapshot.instance_base_url.clone(),
        snapshot.client_id.clone(),
        snapshot.callback_port,
    );
    // The instance may have changed — drop cached lookups so a different tenant
    // doesn't inherit stale org/location/role names.
    state.clear_lookups_cache();

    match args.client_secret.map(|s| s.trim().to_string()) {
        Some(secret) if !secret.is_empty() => {
            state
                .auth
                .set_client_secret(Some(secret))
                .map_err(UiError::from)?;
        }
        _ if args.clear_secret => {
            state.auth.set_client_secret(None).map_err(UiError::from)?;
        }
        _ => {}
    }

    Ok(view(&state))
}

#[tauri::command]
pub fn list_presets(state: State<'_, AppState>) -> Vec<Preset> {
    state.settings_snapshot().presets
}

/// Upserts a preset by name.
#[tauri::command]
pub fn save_preset(state: State<'_, AppState>, preset: Preset) -> Result<Vec<Preset>, UiError> {
    let mut guard = state
        .settings
        .lock()
        .map_err(|_| UiError::new("settings state poisoned"))?;
    if let Some(existing) = guard.presets.iter_mut().find(|p| p.name == preset.name) {
        existing.filter = preset.filter;
    } else {
        guard.presets.push(preset);
    }
    guard.save().map_err(UiError::from)?;
    Ok(guard.presets.clone())
}

#[tauri::command]
pub fn delete_preset(state: State<'_, AppState>, name: String) -> Result<Vec<Preset>, UiError> {
    let mut guard = state
        .settings
        .lock()
        .map_err(|_| UiError::new("settings state poisoned"))?;
    guard.presets.retain(|p| p.name != name);
    guard.save().map_err(UiError::from)?;
    Ok(guard.presets.clone())
}
