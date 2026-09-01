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
///
/// Clears the session first. Signing in is not necessarily a *new* session
/// continuing an old one — on a shared workstation it is routinely a different
/// operator on the same instance, which every tenant stamp in the app reads as
/// identical. Anything left in the caches at this point belongs to whoever was here
/// before.
#[tauri::command]
pub async fn sign_in(state: State<'_, AppState>) -> Result<(), UiError> {
    clear_session_state(&state);
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
    // Same reason as `sign_in`: re-consent runs the full browser flow, so the
    // operator who comes back may not be the one who left.
    clear_session_state(&state);
    state.auth.logout().map_err(UiError::from)?;
    state.auth.login_pkce().await.map_err(UiError::from)
}

#[tauri::command]
pub async fn sign_out(state: State<'_, AppState>) -> Result<(), UiError> {
    clear_session_state(&state);
    state.auth.logout().map_err(UiError::from)
}

/// Drops every piece of tenant-scoped state a sign-out must not leave behind.
///
/// Split out from the command so it can be tested — `sign_out` needs a Tauri
/// `State`, so nothing asserted that all of these actually ran, and each is a
/// separate call that a future edit could drop silently. On a shared operator
/// workstation the consequence is the next person seeing the previous one's rows:
/// the caches are tenant-stamped, but signing out and back in as a *different
/// operator on the same instance* is the same tenant, so the stamp does not help
/// here. These clears are the only defense — which is why every path that starts or
/// ends a session calls this, not just `sign_out`: `sign_in` and `reauthorize` both
/// run the interactive flow and can hand the process to a different operator.
///
/// `clear_last_result` also bumps the result-cache epoch, so a whole-fleet query
/// still in flight cannot store the departing operator's rows after this returns.
fn clear_session_state(state: &AppState) {
    // Also drops the whole-fleet device and current-patch caches.
    state.clear_lookups_cache();
    state.clear_last_result();
    // Also clears any pending confirmation token.
    state.clear_jobs();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ActionKind, JobReport, JobState};
    use crate::rows::{PatchFamilies, QueryResult};

    fn sample_result() -> QueryResult {
        QueryResult {
            rows: Vec::new(),
            devices: Vec::new(),
            compliance: Vec::new(),
            compliance_by_os: Vec::new(),
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            devices_total: 0,
            devices_offline: 0,
            devices_unpatchable: 0,
            patch_families: PatchFamilies {
                os: true,
                software: true,
            },
            scope: Default::default(),
            generated_at: "2026-01-01 00:00:00 UTC".into(),
            data_fetched_at: "2026-01-01 00:00:00 UTC".into(),
        }
    }

    fn sample_job() -> JobReport {
        JobReport {
            id: 1,
            batch_id: 1,
            device_id: 7,
            device_name: "srv-1".into(),
            organization: "Contoso".into(),
            kind: ActionKind::OsPatchApply,
            detail: "Apply OS patches".into(),
            dry_run: false,
            state: JobState::Completed,
            dispatched_at: "2026-01-01 00:00:00 UTC".into(),
            dispatched_ts: 0,
            finished_at: None,
            duration_seconds: None,
            activity_id: None,
            series_uid: None,
            exit_code: None,
        }
    }

    /// Signing out has to leave nothing readable behind. The tenant stamp does not
    /// cover this case — a second operator signing in on the same instance is the
    /// same tenant — so a dropped clear here means their predecessor's patch rows
    /// and dispatch history stay on screen.
    #[test]
    fn signing_out_drops_the_cached_result_and_the_job_history() {
        let state = AppState::new().expect("build state");
        state.store_last_result_if_current(state.begin_query(), sample_result());
        state.append_jobs(vec![sample_job()]);

        assert!(state.with_current_result(|_| ()).unwrap().is_some());
        assert_eq!(state.jobs_snapshot().len(), 1);

        clear_session_state(&state);

        assert!(
            state.with_current_result(|_| ()).unwrap().is_none(),
            "the cached rows behind paging and export must be gone"
        );
        assert!(
            state.jobs_snapshot().is_empty(),
            "dispatch history must not survive a sign-out"
        );
    }
}
