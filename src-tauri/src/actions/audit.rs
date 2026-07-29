//! Append-only audit trail for dispatched actions.
//!
//! Written **before** the request goes out and again once it settles, so a crash
//! mid-batch still leaves evidence of what was attempted. Best-effort throughout:
//! a failed audit write warns but never blocks the operator.
//!
//! Secrets discipline applies here as everywhere — a `parameters` string is
//! operator-authored free text, so anything that looks like a credential is
//! redacted before it reaches disk. Tokens are never written at all.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use chrono::Utc;
use serde::Serialize;
use tracing::warn;

use super::{ActionKind, JobState};

const AUDIT_FILE: &str = "action-audit.jsonl";

/// Key fragments whose `key=value` token gets its value replaced. Matched
/// case-insensitively against the token's key half.
const SENSITIVE_KEY_FRAGMENTS: [&str; 5] = ["pass", "secret", "token", "apikey", "key"];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub timestamp: String,
    pub instance: String,
    pub client_id: Option<String>,
    pub batch_id: u64,
    pub job_id: u64,
    pub kind: ActionKind,
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    pub detail: String,
    /// Redacted copy of what was sent — see [`redact_parameters`].
    pub parameters: Option<String>,
    pub dry_run: bool,
    /// First 8 characters of the confirmation token, enough to tie a dispatch back
    /// to the plan the operator approved without storing the token itself.
    pub confirm_token_prefix: Option<String>,
    pub outcome: String,
    pub activity_id: Option<i64>,
    pub series_uid: Option<String>,
    pub exit_code: Option<i32>,
}

impl AuditEntry {
    pub fn outcome_of(state: &JobState) -> String {
        state.label()
    }
}

/// Replaces the value of any `key=value` token whose key looks like a credential.
///
/// Operators paste script parameters by hand, and a script that takes a service
/// password would otherwise write it to disk in cleartext.
pub fn redact_parameters(parameters: &str) -> String {
    parameters
        .split(' ')
        .map(|token| match token.split_once('=') {
            Some((key, _)) if is_sensitive(key) => format!("{key}=<redacted>"),
            _ => token.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_sensitive(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_KEY_FRAGMENTS.iter().any(|f| lower.contains(f))
}

fn audit_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "ninjaone-patch-toolkit")
        .map(|d| d.config_dir().join(AUDIT_FILE))
}

/// Appends one JSON-lines record. Never returns an error: auditing must not be
/// able to stop an operator from working, and a warning in the log is the right
/// severity for a disk that won't take the write.
pub fn record(entry: &AuditEntry) {
    let Some(path) = audit_path() else {
        warn!("no config directory available; action audit record dropped");
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        warn!(?err, "could not create the audit directory");
        return;
    }
    let line = match serde_json::to_string(entry) {
        Ok(l) => l,
        Err(err) => {
            warn!(?err, "could not serialize an audit record");
            return;
        }
    };
    let written = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(err) = written {
        warn!(?err, path = %path.display(), "could not append to the action audit log");
    }
}

pub fn now_stamp() -> String {
    super::fmt_ts(Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_shaped_parameters_are_redacted() {
        let redacted = redact_parameters(
            "kbAllowList=5040434 servicePassword=hunter2 apiKey=abc dryRun=false",
        );
        assert!(redacted.contains("kbAllowList=5040434"), "{redacted}");
        assert!(redacted.contains("dryRun=false"), "{redacted}");
        assert!(
            redacted.contains("servicePassword=<redacted>"),
            "{redacted}"
        );
        assert!(redacted.contains("apiKey=<redacted>"), "{redacted}");
        assert!(
            !redacted.contains("hunter2") && !redacted.contains("abc"),
            "no credential value may survive: {redacted}"
        );
    }

    #[test]
    fn redaction_is_case_insensitive_and_keeps_bare_tokens() {
        let redacted = redact_parameters("-Verbose CLIENTSECRET=zzz Token=qqq");
        assert!(redacted.starts_with("-Verbose "));
        assert!(redacted.contains("CLIENTSECRET=<redacted>"));
        assert!(redacted.contains("Token=<redacted>"));
        assert!(!redacted.contains("zzz") && !redacted.contains("qqq"));
    }

    #[test]
    fn ordinary_parameters_pass_through_unchanged() {
        let params = "kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=true";
        assert_eq!(redact_parameters(params), params);
    }
}
