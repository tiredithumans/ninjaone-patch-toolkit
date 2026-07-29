use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::UiError;
use crate::settings::{
    ActionSettings, MAX_ACTION_CONCURRENCY, MAX_DEVICES_PER_ACTION_CEILING, Preset,
};
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
    pub actions: ActionSettings,
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
        actions: s.actions,
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
    /// Omitted by a frontend that predates the actions panel, which then leaves the
    /// write path disabled rather than silently enabling it.
    #[serde(default)]
    pub actions: ActionSettings,
}

fn default_auto_check() -> bool {
    true
}

/// Rejects an instance URL that would carry OAuth tokens, codes, and the client
/// secret in cleartext. `https` is required everywhere except a loopback host,
/// where `http` is allowed for local testing against a mock server.
fn require_https_instance(url: &str) -> Result<(), UiError> {
    let parsed = url::Url::parse(url)
        .map_err(|_| UiError::new(format!("instance URL is not a valid URL: {url}")))?;
    let host = parsed.host_str().unwrap_or_default();
    let is_loopback = matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1");
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if is_loopback => Ok(()),
        _ => Err(UiError::new(
            "instance URL must use https:// (http is allowed only for localhost)",
        )),
    }
}

/// Rejects numeric settings that would break a query or the OAuth redirect, rather
/// than silently clamping an operator typo (e.g. a `0` window) into a value they
/// didn't choose. The callback port must be a real port (`0` means "any" to the OS,
/// so it can't match a registered redirect URI), and the install/SLA windows must
/// be at least one day.
fn validate_settings_input(args: &SaveSettingsArgs) -> Result<(), UiError> {
    if args.callback_port == 0 {
        return Err(UiError::new("Callback port must be between 1 and 65535."));
    }
    if args.install_window_days < 1 {
        return Err(UiError::new("Install window (days) must be at least 1."));
    }
    if args.sla_days < 1 {
        return Err(UiError::new("SLA (days) must be at least 1."));
    }
    validate_action_settings(&args.actions)?;
    Ok(())
}

/// Same reject-don't-clamp policy for the write-path knobs. These are guardrails,
/// so a typo that silently became "500 devices" or "concurrency 0" (which would
/// deadlock the dispatch semaphore) is exactly the failure mode to avoid.
fn validate_action_settings(a: &ActionSettings) -> Result<(), UiError> {
    if a.concurrency < 1 || a.concurrency > MAX_ACTION_CONCURRENCY {
        return Err(UiError::new(format!(
            "Dispatch concurrency must be between 1 and {MAX_ACTION_CONCURRENCY}."
        )));
    }
    if a.max_devices_per_action < 1 || a.max_devices_per_action > MAX_DEVICES_PER_ACTION_CEILING {
        return Err(UiError::new(format!(
            "Max devices per action must be between 1 and {MAX_DEVICES_PER_ACTION_CEILING}."
        )));
    }
    if a.max_orgs_per_action < 1 {
        return Err(UiError::new(
            "Max organizations per action must be at least 1.",
        ));
    }
    if a.run_as.trim().is_empty() {
        return Err(UiError::new("Run-as identity cannot be empty."));
    }
    if a.window_start_minute >= 1440 || a.window_end_minute >= 1440 {
        return Err(UiError::new(
            "Maintenance-window times must be within a 24-hour day.",
        ));
    }
    if a.window_start_minute == a.window_end_minute {
        return Err(UiError::new(
            "Maintenance-window start and end must differ — an empty window blocks every action.",
        ));
    }
    if a.window_days.iter().any(|d| *d > 6) {
        return Err(UiError::new(
            "Maintenance-window days must be 0 (Sunday) through 6 (Saturday).",
        ));
    }
    if a.require_maintenance_window && a.window_days.is_empty() {
        return Err(UiError::new(
            "A maintenance window with no days selected blocks every action.",
        ));
    }
    Ok(())
}

#[tauri::command]
pub fn save_settings(
    state: State<'_, AppState>,
    args: SaveSettingsArgs,
) -> Result<SettingsView, UiError> {
    let instance_base_url = args
        .instance_base_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    require_https_instance(&instance_base_url)?;
    validate_settings_input(&args)?;

    let instance_changed;
    let actions_changed;
    let snapshot = {
        let mut guard = state
            .settings
            .lock()
            .map_err(|_| UiError::new("settings state poisoned"))?;
        instance_changed = guard.instance_base_url != instance_base_url;
        guard.instance_base_url = instance_base_url;
        guard.client_id = args
            .client_id
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
        // Already range-checked by validate_settings_input above.
        guard.callback_port = args.callback_port;
        guard.install_window_days = args.install_window_days;
        guard.sla_days = args.sla_days;
        guard.auto_check_updates = args.auto_check_updates;
        actions_changed = guard.actions.enabled != args.actions.enabled;
        guard.actions = args.actions;
        guard.actions.run_as = guard.actions.run_as.trim().to_string();
        guard.save().map_err(UiError::from)?;
        guard.clone()
    };

    state.auth.apply_settings(
        snapshot.instance_base_url.clone(),
        snapshot.client_id.clone(),
        snapshot.callback_port,
        snapshot.actions.enabled,
    );
    // The instance may have changed — drop cached lookups so a different tenant
    // doesn't inherit stale org/location/role names, and drop the cached query
    // result so an export can't write the previous tenant's rows.
    state.clear_lookups_cache();
    if instance_changed {
        state.clear_last_result();
        state.clear_jobs();
    }
    // Toggling actions changes the OAuth scope the next sign-in requests, but the
    // *current* grant is unchanged — the frontend reads `write_enabled` from
    // auth_status and prompts for re-authorization when the two disagree.
    if actions_changed {
        tracing::info!(
            enabled = snapshot.actions.enabled,
            "patch actions toggled; re-authorization required for the scope to take effect"
        );
    }

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
        // Replace the whole record so re-saving a name also updates the patch-query
        // selectors, not just `filter`.
        *existing = preset;
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

#[cfg(test)]
mod tests {
    use super::{
        ActionSettings, SaveSettingsArgs, require_https_instance, validate_action_settings,
        validate_settings_input,
    };

    #[test]
    fn https_instance_is_required() {
        assert!(require_https_instance("https://us2.ninjarmm.com").is_ok());
        // Loopback may use http for local testing.
        assert!(require_https_instance("http://127.0.0.1:8080").is_ok());
        assert!(require_https_instance("http://localhost").is_ok());
        // Cleartext to a real host, a non-http scheme, and a non-URL are rejected.
        assert!(require_https_instance("http://eu.ninjarmm.com").is_err());
        assert!(require_https_instance("ftp://us2.ninjarmm.com").is_err());
        assert!(require_https_instance("not a url").is_err());
    }

    fn args(callback_port: u16, install_window_days: i64, sla_days: i64) -> SaveSettingsArgs {
        SaveSettingsArgs {
            instance_base_url: "https://us2.ninjarmm.com".into(),
            client_id: None,
            callback_port,
            install_window_days,
            sla_days,
            client_secret: None,
            clear_secret: false,
            auto_check_updates: true,
            actions: ActionSettings::default(),
        }
    }

    #[test]
    fn numeric_settings_reject_invalid_values_instead_of_clamping() {
        assert!(validate_settings_input(&args(11434, 30, 30)).is_ok());
        // Port 0 ("any") can't match a registered redirect URI.
        assert!(validate_settings_input(&args(0, 30, 30)).is_err());
        // Sub-day windows are operator typos, surfaced rather than clamped to 1.
        assert!(validate_settings_input(&args(11434, 0, 30)).is_err());
        assert!(validate_settings_input(&args(11434, -5, 30)).is_err());
        assert!(validate_settings_input(&args(11434, 30, 0)).is_err());
    }

    #[test]
    fn action_settings_default_to_fully_disabled() {
        let a = ActionSettings::default();
        assert!(!a.enabled, "the write path must be opt-in");
        assert!(!a.allow_offline_targets);
        assert!(!a.allow_window_override);
        assert_eq!(a.max_orgs_per_action, 1);
        assert!(validate_action_settings(&a).is_ok());
    }

    #[test]
    fn action_guardrail_bounds_reject_invalid_values() {
        let base = ActionSettings::default();

        // Concurrency 0 would deadlock the dispatch semaphore.
        assert!(
            validate_action_settings(&ActionSettings {
                concurrency: 0,
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            validate_action_settings(&ActionSettings {
                concurrency: 17,
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            validate_action_settings(&ActionSettings {
                max_devices_per_action: 0,
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            validate_action_settings(&ActionSettings {
                max_devices_per_action: 501,
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            validate_action_settings(&ActionSettings {
                max_orgs_per_action: 0,
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            validate_action_settings(&ActionSettings {
                run_as: "  ".into(),
                ..base.clone()
            })
            .is_err()
        );
    }

    #[test]
    fn a_window_that_can_never_open_is_rejected() {
        let base = ActionSettings::default();

        // Identical bounds are a zero-length window, which would block everything.
        assert!(
            validate_action_settings(&ActionSettings {
                window_start_minute: 120,
                window_end_minute: 120,
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            validate_action_settings(&ActionSettings {
                window_start_minute: 1440,
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            validate_action_settings(&ActionSettings {
                window_days: vec![7],
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            validate_action_settings(&ActionSettings {
                require_maintenance_window: true,
                window_days: vec![],
                ..base.clone()
            })
            .is_err()
        );
        // Days may be empty as long as the window isn't being enforced.
        assert!(
            validate_action_settings(&ActionSettings {
                require_maintenance_window: false,
                window_days: vec![],
                ..base
            })
            .is_ok()
        );
    }
}
