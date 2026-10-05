use serde::Serialize;

/// Error shape returned across the Tauri IPC boundary. The frontend renders
/// `message` in a toast.
#[derive(Debug, Serialize)]
pub struct UiError {
    pub message: String,
    /// Set only on an error the frontend must handle differently from "show the
    /// message", so it never has to match on message text. Omitted otherwise, so the
    /// shape of every other error is unchanged. Mirrored in `web-rs/src/types.rs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
}

/// `run_action` refused to record a batch whose session ended mid-dispatch. Some
/// devices may already have acted, so the frontend must not offer to re-plan it —
/// a re-plan would send to them again.
pub const ERR_PARTIAL_DISPATCH: &str = "partialDispatch";

impl UiError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }

    /// An error carrying one of the `ERR_*` codes above.
    pub fn coded(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: Some(code),
        }
    }
}

impl From<anyhow::Error> for UiError {
    fn from(err: anyhow::Error) -> Self {
        Self::new(format!("{err:#}"))
    }
}

/// The result cache's poisoning error, which every read site had been translating
/// with the same copy-pasted `.map_err(|_| UiError::new("result cache poisoned"))` —
/// five verbatim copies of one message, so a reword had to find all five.
impl From<crate::state::CachePoisoned> for UiError {
    fn from(_: crate::state::CachePoisoned) -> Self {
        Self::new(
            "The result cache is unusable after an internal error. Restart the app to run queries again.",
        )
    }
}

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Caps a server-returned body before it goes into a log line or a user-facing
/// error message. Responses can be large and may echo back request parameters,
/// so neither the toast nor the trace log should carry the whole thing.
pub(crate) fn truncate_body(s: &str) -> String {
    const MAX_CHARS: usize = 500;
    let mut out: String = s.chars().take(MAX_CHARS).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// The operator-facing reason for a 3xx, or `None` for any other status.
///
/// The shared client does not follow redirects (see `state::build_http_client`), so
/// an Instance that redirects — `http`→`https`, an old regional host — would
/// otherwise fail as a bare "301 Moved Permanently" with an empty body. Only the
/// target's scheme and host are shown: the path and query can carry anything the
/// server chose to put there, and the host is all the operator needs to fix it.
pub(crate) fn redirect_hint(resp: &reqwest::Response) -> Option<String> {
    if !resp.status().is_redirection() {
        return None;
    }
    let target = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|loc| resp.url().join(loc).ok());
    Some(match target {
        Some(t) => format!(
            "NinjaOne redirected to {}; set Instance in Settings to that address",
            t.origin().ascii_serialization()
        ),
        None => "NinjaOne answered with a redirect; check Instance in Settings".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frontend's `ErrShape` reads `message` and an optional `code`. A plain
    /// error keeps the `{ message }` shape every handler has always returned; a coded
    /// one adds the key the frontend branches on instead of matching message text.
    #[test]
    fn only_a_coded_error_carries_a_code_over_ipc() {
        assert_eq!(
            serde_json::to_value(UiError::new("nope")).unwrap(),
            serde_json::json!({ "message": "nope" })
        );
        assert_eq!(
            serde_json::to_value(UiError::coded(ERR_PARTIAL_DISPATCH, "sent some")).unwrap(),
            serde_json::json!({ "message": "sent some", "code": "partialDispatch" })
        );
    }
}
