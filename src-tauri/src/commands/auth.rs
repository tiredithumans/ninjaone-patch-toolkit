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
}

/// Launches the interactive PKCE browser flow and waits for the callback.
#[tauri::command]
pub async fn sign_in(state: State<'_, AppState>) -> Result<(), UiError> {
    state.auth.login_pkce().await.map_err(UiError::from)
}

#[tauri::command]
pub async fn sign_out(state: State<'_, AppState>) -> Result<(), UiError> {
    state.auth.logout().map_err(UiError::from)
}

#[tauri::command]
pub fn auth_status(state: State<'_, AppState>) -> AuthStatus {
    AuthStatus {
        authenticated: state.auth.is_authenticated(),
        client_id: state.auth.client_id(),
        has_client_secret: state.auth.has_client_secret(),
        instance_base_url: state.auth.base_url(),
    }
}
