use serde::Serialize;

/// Error shape returned across the Tauri IPC boundary. The frontend renders
/// `message` in a toast.
#[derive(Debug, Serialize)]
pub struct UiError {
    pub message: String,
}

impl UiError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for UiError {
    fn from(err: anyhow::Error) -> Self {
        Self {
            message: format!("{err:#}"),
        }
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
