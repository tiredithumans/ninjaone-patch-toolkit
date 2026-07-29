use serde::Serialize;
use tauri::State;

use crate::error::UiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub authenticated: bool,
    pub client_id: Option<String>,
    pub has_client_secret: bool,
    pub instance_base_url: String,
    /// Whether patch actions are switched on in Settings. Independent of whether
    /// the *current* grant can actually perform them.
    pub actions_enabled: bool,
    /// Whether the current OAuth grant carries the `management` scope, i.e. the
    /// write endpoints will accept us. False with `actions_enabled` true means the
    /// operator needs to re-authorize.
    pub write_enabled: bool,
    /// Whether the grant's scope could be determined at all. An opaque access token
    /// leaves this false, in which case `write_enabled` being false means "unknown",
    /// not "denied" — the UI words the two differently rather than telling the
    /// operator their consent was wrong when we simply cannot tell.
    pub scope_known: bool,
}

/// Launches the interactive PKCE browser flow and waits for the callback.
#[tauri::command]
pub async fn sign_in(state: State<'_, AppState>) -> Result<(), UiError> {
    state.auth.login_pkce().await.map_err(UiError::from)
}

/// Forces a fresh consent round trip.
///
/// The refresh grant never re-sends `scope`, so an install that signed in before
/// patch actions were enabled keeps its read-only grant indefinitely. Dropping the
/// stored refresh token first is what makes the browser flow issue a *new* grant
/// rather than silently reusing the narrow one.
#[tauri::command]
pub async fn reauthorize(state: State<'_, AppState>) -> Result<(), UiError> {
    state.auth.logout().map_err(UiError::from)?;
    state.auth.login_pkce().await.map_err(UiError::from)
}

#[tauri::command]
pub async fn sign_out(state: State<'_, AppState>) -> Result<(), UiError> {
    state.clear_lookups_cache();
    state.clear_last_result();
    state.clear_jobs();
    state.auth.logout().map_err(UiError::from)
}

#[tauri::command]
pub fn auth_status(state: State<'_, AppState>) -> AuthStatus {
    let grant = state.auth.management_grant();
    AuthStatus {
        authenticated: state.auth.is_authenticated(),
        client_id: state.auth.client_id(),
        has_client_secret: state.auth.has_client_secret(),
        instance_base_url: state.auth.base_url(),
        actions_enabled: state.settings_snapshot().actions.enabled,
        write_enabled: grant.unwrap_or(false),
        scope_known: grant.is_some(),
    }
}
