use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::UiError;
use crate::settings::{
    ActionSettings, MAX_ACTION_CONCURRENCY, MAX_DEVICES_PER_ACTION_CEILING, MAX_WINDOW_DAYS,
    Preset, Settings, is_loopback_host,
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
    /// Whether this save switched tenant (instance or client id), so the frontend
    /// knows to drop the results still on screen.
    ///
    /// The backend already clears `last_result` on a tenant switch, so without this
    /// the previous tenant's rows stayed rendered while paging, export and the HTML
    /// report all read the miss — the same divergence `query_patches` refuses a
    /// drifted summary for, one layer up. Always `false` from `get_settings`, which
    /// changes nothing.
    pub tenant_changed: bool,
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
        tenant_changed: false,
    }
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsView {
    view(&state)
}

#[derive(Deserialize)]
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

/// Hand-written so the client secret can never reach a log line or a panic
/// message through `{:?}`; only whether one was supplied is shown.
impl std::fmt::Debug for SaveSettingsArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveSettingsArgs")
            .field("instance_base_url", &self.instance_base_url)
            .field("client_id", &self.client_id)
            .field("callback_port", &self.callback_port)
            .field("install_window_days", &self.install_window_days)
            .field("sla_days", &self.sla_days)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("clear_secret", &self.clear_secret)
            .field("auto_check_updates", &self.auto_check_updates)
            .field("actions", &self.actions)
            .finish()
    }
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
    let is_loopback = is_loopback_host(parsed.host_str().unwrap_or_default());
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
    if args.install_window_days < 1 || args.install_window_days > MAX_WINDOW_DAYS {
        return Err(UiError::new(format!(
            "Install window (days) must be between 1 and {MAX_WINDOW_DAYS}."
        )));
    }
    if args.sla_days < 1 || args.sla_days > MAX_WINDOW_DAYS {
        return Err(UiError::new(format!(
            "SLA (days) must be between 1 and {MAX_WINDOW_DAYS}."
        )));
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

/// What a save changed that the caller has to act on.
#[derive(Debug, PartialEq, Eq)]
struct SaveEffects {
    /// Instance or client id moved: every tenant-keyed cache is now another
    /// tenant's.
    tenant_changed: bool,
    /// The scope the next sign-in requests moved.
    actions_changed: bool,
}

/// The settings `args` asks for, applied over `current`. Pure, so what counts as a
/// tenant switch is testable without a Tauri `State`.
fn merge_settings(
    current: &Settings,
    instance_base_url: String,
    args: SaveSettingsArgs,
) -> (Settings, SaveEffects) {
    let client_id = args
        .client_id
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    let mut next = current.clone();
    // Both halves of the cache's tenant key, not just the instance: pointing the
    // same instance at a different client id is as much a tenant switch as
    // changing the host, and the caches key on the pair.
    let effects = SaveEffects {
        tenant_changed: current.instance_base_url != instance_base_url
            || current.client_id != client_id,
        actions_changed: current.actions.enabled != args.actions.enabled,
    };
    next.instance_base_url = instance_base_url;
    next.client_id = client_id;
    // Already range-checked by validate_settings_input.
    next.callback_port = args.callback_port;
    next.install_window_days = args.install_window_days;
    next.sla_days = args.sla_days;
    next.auto_check_updates = args.auto_check_updates;
    next.actions = args.actions;
    next.actions.run_as = next.actions.run_as.trim().to_string();
    (next, effects)
}

/// Runs blocking settings I/O (the file write, keyring reads and writes) off the
/// async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, UiError> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| UiError::new(format!("saving settings failed: {e}")))?
        .map_err(UiError::from)
}

/// Saves the Settings panel.
///
/// Async, with the file and keyring I/O on a blocking thread and no `settings`
/// lock held across it. As a synchronous command it ran on the main thread — the
/// one that pumps the window — and did a keyring round trip and a file write there
/// while holding the settings mutex every query reads, so a slow Secret Service
/// froze the UI and stalled in-flight work at once. Writers are serialized by
/// `settings_write` instead, and the new value is published in memory only once it
/// is on disk: a failed write no longer leaves memory and `settings.json`
/// disagreeing.
#[tauri::command]
pub async fn save_settings(
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

    let secret_change = match args.client_secret.as_deref().map(str::trim) {
        Some(secret) if !secret.is_empty() => Some(Some(secret.to_string())),
        _ if args.clear_secret => Some(None),
        _ => None,
    };

    let _writer = state.settings_write.lock().await;
    let (next, effects) = merge_settings(&state.settings_snapshot(), instance_base_url, args);

    let to_disk = next.clone();
    blocking(move || to_disk.save()).await?;
    state.replace_settings(next.clone());

    // Drops the previous tenant's grant when the tenant actually changed — see
    // `AuthState::apply_settings` for why leaving it in place destroyed the
    // credential of the tenant being switched away from. Its keyring read (the
    // new tenant's secret) and the secret write below are both blocking. The
    // secret is stored after the switch so it lands under the tenant now in effect.
    let auth = state.auth.clone();
    let applied = next.clone();
    // Not `?`-ed until the caches are cleared: the new tenant is already on disk
    // and in memory, so a failed secret write must not skip the clears below.
    let auth_applied = blocking(move || {
        auth.apply_settings(
            applied.instance_base_url,
            applied.client_id,
            applied.callback_port,
            applied.actions.enabled,
        );
        match secret_change {
            Some(secret) => auth.set_client_secret(secret),
            None => Ok(()),
        }
    })
    .await;

    // Only a tenant switch invalidates the caches. The lookups, the whole-fleet
    // devices and the current patches are all per-tenant fleet data that no other
    // setting feeds into — the install window, SLA and presets are applied when a
    // query is assembled, and the port, the secret, the actions block and the
    // update check never reach a fetch. Clearing on every save meant toggling the
    // update check threw away a whole-fleet fetch that can take minutes to redo.
    if effects.tenant_changed {
        state.clear_lookups_cache();
        state.clear_last_result();
        state.clear_jobs();
    }
    auth_applied?;
    // Toggling actions changes the OAuth scope the next sign-in requests, but the
    // *current* grant is unchanged — the frontend reads `write_enabled` from
    // auth_status and prompts for re-authorization when the two disagree.
    if effects.actions_changed {
        tracing::info!(
            enabled = next.actions.enabled,
            "patch actions toggled; re-authorization required for the scope to take effect"
        );
    }

    Ok(SettingsView {
        tenant_changed: effects.tenant_changed,
        ..view(&state)
    })
}

/// Applies `edit` to the presets and persists the result, the same way
/// `save_settings` persists: serialized by `settings_write`, written on a blocking
/// thread, published in memory once on disk.
async fn update_presets(
    state: &AppState,
    edit: impl FnOnce(&mut Vec<Preset>),
) -> Result<Vec<Preset>, UiError> {
    let _writer = state.settings_write.lock().await;
    let mut next = state.settings_snapshot();
    edit(&mut next.presets);
    let to_disk = next.clone();
    blocking(move || to_disk.save()).await?;
    let presets = next.presets.clone();
    state.replace_settings(next);
    Ok(presets)
}

/// Upserts a preset by name.
#[tauri::command]
pub async fn save_preset(
    state: State<'_, AppState>,
    preset: Preset,
) -> Result<Vec<Preset>, UiError> {
    update_presets(&state, |presets| {
        if let Some(existing) = presets.iter_mut().find(|p| p.name == preset.name) {
            // Replace the whole record so re-saving a name also updates the
            // patch-query selectors, not just `filter`.
            *existing = preset;
        } else {
            presets.push(preset);
        }
    })
    .await
}

#[tauri::command]
pub async fn delete_preset(
    state: State<'_, AppState>,
    name: String,
) -> Result<Vec<Preset>, UiError> {
    update_presets(&state, |presets| presets.retain(|p| p.name != name)).await
}

#[cfg(test)]
mod tests {
    use super::{
        ActionSettings, MAX_WINDOW_DAYS, SaveEffects, SaveSettingsArgs, Settings, merge_settings,
        require_https_instance, validate_action_settings, validate_settings_input,
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

    /// The upper bound is a panic guard, not a preference: both windows reach
    /// `chrono::Duration::days`, which panics on an out-of-range day count.
    #[test]
    fn day_windows_reject_values_that_would_overflow_duration_days() {
        assert!(validate_settings_input(&args(11434, MAX_WINDOW_DAYS, MAX_WINDOW_DAYS)).is_ok());
        assert!(validate_settings_input(&args(11434, MAX_WINDOW_DAYS + 1, 30)).is_err());
        assert!(validate_settings_input(&args(11434, 30, MAX_WINDOW_DAYS + 1)).is_err());
        assert!(validate_settings_input(&args(11434, i64::MAX, 30)).is_err());
        assert!(validate_settings_input(&args(11434, 30, i64::MAX)).is_err());
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

    /// Saving used to drop every whole-fleet cache no matter what changed, so
    /// flipping the update check cost the next query a full refetch. Only the
    /// instance or the client id may count as a tenant switch.
    #[test]
    fn only_the_instance_or_client_id_is_a_tenant_switch() {
        let current = Settings {
            client_id: Some("client-a".into()),
            ..Settings::default()
        };
        let same_tenant = |edit: fn(&mut SaveSettingsArgs)| {
            let mut a = SaveSettingsArgs {
                client_id: Some("client-a".into()),
                ..args(11434, 30, 30)
            };
            edit(&mut a);
            merge_settings(&current, current.instance_base_url.clone(), a).1
        };
        let unchanged = SaveEffects {
            tenant_changed: false,
            actions_changed: false,
        };

        assert_eq!(same_tenant(|a| a.auto_check_updates = false), unchanged);
        assert_eq!(same_tenant(|a| a.callback_port = 12000), unchanged);
        assert_eq!(same_tenant(|a| a.sla_days = 7), unchanged);
        assert_eq!(
            same_tenant(|a| a.client_secret = Some("s".into())),
            unchanged
        );
        // Whitespace around the client id is trimmed, not a new tenant.
        assert_eq!(
            same_tenant(|a| a.client_id = Some("  client-a ".into())),
            unchanged
        );
        assert_eq!(
            same_tenant(|a| a.actions.enabled = true),
            SaveEffects {
                tenant_changed: false,
                actions_changed: true,
            }
        );

        assert!(same_tenant(|a| a.client_id = Some("client-b".into())).tenant_changed);
        assert!(same_tenant(|a| a.client_id = None).tenant_changed);
        let moved = merge_settings(
            &current,
            "https://eu.ninjarmm.com".into(),
            SaveSettingsArgs {
                client_id: Some("client-a".into()),
                ..args(11434, 30, 30)
            },
        );
        assert!(moved.1.tenant_changed);
        assert_eq!(moved.0.instance_base_url, "https://eu.ninjarmm.com");
    }

    /// The client secret must never be printable through `{:?}`.
    #[test]
    fn debug_output_redacts_the_client_secret() {
        let a = SaveSettingsArgs {
            client_secret: Some("hunter2-secret-value".into()),
            ..args(11434, 30, 30)
        };
        let printed = format!("{a:?}");
        assert!(!printed.contains("hunter2-secret-value"));
        assert!(printed.contains("<redacted>"));
    }
}
