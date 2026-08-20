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
    // Split on *any* whitespace, not just `' '`. NinjaOne itself splits `parameters`
    // on spaces, but this string is typed — and routinely pasted — by hand in the
    // script picker, and a pasted line carrying a tab or a newline used to leave the
    // whole run as one unsplittable token: `is_sensitive` never matched it and the
    // credential reached disk in cleartext.
    //
    // This deliberately normalizes runs of whitespace to single spaces. The audit
    // record is evidence of what was dispatched, not a byte-exact replay of it, and
    // the alternative is carrying separators through just to reproduce spacing that
    // NinjaOne collapses anyway.
    //
    // Two shapes, because scripts take parameters in both. `-Password hunter2` is
    // the PowerShell/CLI convention and is at least as common here as `key=value`;
    // it used to pass straight through, since neither token contains an `=` and so
    // neither could ever match. The module doc claims a script's service password
    // does not reach disk in cleartext, and for that shape it did.
    let mut out: Vec<String> = Vec::new();
    let mut redact_next = false;
    for token in parameters.split_whitespace() {
        if redact_next {
            redact_next = false;
            // Only a *value* is swallowed. `-Password -Verbose` means the flag was
            // given no value, and blanking the following flag would both lose
            // evidence and misrepresent what ran.
            if !is_flag(token) {
                out.push("<redacted>".into());
                continue;
            }
        }
        match token.split_once('=') {
            Some((key, _)) if is_sensitive(key) => out.push(format!("{key}=<redacted>")),
            _ => {
                redact_next = is_flag(token) && is_sensitive(token);
                out.push(token.to_string());
            }
        }
    }
    out.join(" ")
}

/// Whether `token` is a flag rather than a value: `-Password`, `--api-key`, or the
/// `/Password` form Windows tooling uses.
fn is_flag(token: &str) -> bool {
    token.starts_with('-') || token.starts_with('/')
}

fn is_sensitive(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_KEY_FRAGMENTS.iter().any(|f| lower.contains(f))
}

fn audit_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "ninjaone-patch-toolkit")
        .map(|d| d.config_dir().join(AUDIT_FILE))
}

/// Appends every record in `entries`, opening the log once.
///
/// Never returns an error: auditing must not be able to stop an operator from
/// working, and a warning in the log is the right severity for a disk that won't
/// take the write.
///
/// **Synchronous file I/O — never call this from an async task.** Use
/// [`record_off_runtime`] there. A dispatch fans out one entry per device and the
/// poller closes out a whole settled batch, so this used to do `create_dir_all` +
/// open + write per device directly on tokio workers.
pub fn record_all(entries: &[AuditEntry]) {
    if entries.is_empty() {
        return;
    }
    let Some(path) = audit_path() else {
        warn!("no config directory available; action audit record dropped");
        return;
    };
    write_records(&path, entries);
}

/// The half of [`record_all`] that does not depend on the OS config directory, so it
/// can be tested against a temp path. `record_all` itself was untestable — it
/// resolved its own destination — which left the directory creation, the 0600 mode
/// and the append behaviour with no coverage at all; only the pure `redact_parameters`
/// helper was asserted.
fn write_records(path: &std::path::Path, entries: &[AuditEntry]) {
    // The poller calls this on every tick, so an empty batch must not so much as
    // create the file.
    if entries.is_empty() {
        return;
    }
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        warn!(?err, "could not create the audit directory");
        return;
    }
    // Owner-only. The log names devices, organizations and the operator's own
    // parameters; on a shared or roaming-profile machine the default 0644 made all
    // of that world-readable. Applies at creation, so an existing log keeps whatever
    // mode it already has.
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = match opts.open(path) {
        Ok(f) => f,
        Err(err) => {
            warn!(?err, path = %path.display(), "could not open the action audit log");
            return;
        }
    };
    for entry in entries {
        let line = match serde_json::to_string(entry) {
            Ok(l) => l,
            Err(err) => {
                warn!(?err, "could not serialize an audit record");
                continue;
            }
        };
        if let Err(err) = writeln!(file, "{line}") {
            warn!(?err, path = %path.display(), "could not append to the action audit log");
            return;
        }
    }
}

/// [`record_all`], moved off the async runtime.
///
/// The write is a synchronous `create_dir_all` + open + append. On a tokio worker
/// that blocks a thread the rest of the app needs — and both callers are on the
/// hottest paths there are for it: `dispatch_one` runs inside a `JoinSet` fanning
/// out across every targeted device, and the job poller closes out every settled
/// batch. A slow or full disk stalled unrelated IPC and the poller itself.
pub async fn record_off_runtime(entries: Vec<AuditEntry>) {
    if entries.is_empty() {
        return;
    }
    if let Err(err) = tauri::async_runtime::spawn_blocking(move || record_all(&entries)).await {
        warn!(?err, "the action audit write task failed");
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

    /// A pasted parameter line is not guaranteed to be space-separated, and the
    /// splitter used to be `' '` only — so a tab or newline left the whole run as one
    /// token that `is_sensitive` could not match, and the credential was written to
    /// disk verbatim.
    #[test]
    fn credentials_are_redacted_across_any_whitespace_separator() {
        for sep in ["\t", "\n", "\r\n", "  "] {
            let redacted =
                redact_parameters(&format!("kbAllowList=5040434{sep}servicePassword=hunter2"));
            assert!(
                redacted.contains("servicePassword=<redacted>") && !redacted.contains("hunter2"),
                "separator {sep:?} left the credential in: {redacted}"
            );
            assert!(redacted.contains("kbAllowList=5040434"), "{redacted}");
        }
    }

    #[test]
    fn ordinary_parameters_pass_through_unchanged() {
        let params = "kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=true";
        assert_eq!(redact_parameters(params), params);
    }
    fn sample_entry(job_id: u64) -> AuditEntry {
        AuditEntry {
            timestamp: "2026-01-01 00:00:00 UTC".into(),
            instance: "https://app.ninjarmm.com".into(),
            client_id: Some("client-a".into()),
            batch_id: 1,
            job_id,
            kind: ActionKind::OsPatchApply,
            device_id: 7,
            device_name: "srv-1".into(),
            organization: "Contoso".into(),
            detail: "Apply OS patches".into(),
            parameters: None,
            dry_run: false,
            confirm_token_prefix: None,
            outcome: "dispatching".into(),
            activity_id: None,
            series_uid: None,
            exit_code: None,
        }
    }

    /// The log is append-only JSON lines and creates its own directory. None of that
    /// was covered: `record` resolved its own destination from the OS config dir, so
    /// only the pure `redact_parameters` helper could be asserted.
    #[test]
    fn records_append_as_one_json_line_each_and_create_the_directory() {
        let dir = std::env::temp_dir().join(format!("njp-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join(AUDIT_FILE);

        write_records(&path, &[sample_entry(1), sample_entry(2)]);
        write_records(&path, &[sample_entry(3)]);

        let body = std::fs::read_to_string(&path).expect("the log must have been created");
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(
            lines.len(),
            3,
            "one line per record, appended not overwritten"
        );
        for (line, expected) in lines.iter().zip([1u64, 2, 3]) {
            let v: serde_json::Value = serde_json::from_str(line).expect("each line is JSON");
            assert_eq!(v["jobId"], expected);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The mode is set at creation and the comment on it explains why: the log names
    /// devices, organizations and the operator's own parameters, and a roaming profile
    /// makes 0644 world-readable.
    #[cfg(unix)]
    #[test]
    fn the_log_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("njp-audit-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(AUDIT_FILE);

        write_records(&path, &[sample_entry(1)]);

        let mode = std::fs::metadata(&path)
            .expect("log exists")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the audit log must not be group/world readable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An empty batch must not create the file — the poller calls this on every tick.
    #[test]
    fn an_empty_batch_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("njp-audit-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(AUDIT_FILE);

        record_all(&[]);
        write_records(&path, &[]);

        assert!(!path.exists(), "no entries means no file");
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// `-Password hunter2` — the PowerShell/CLI convention — carries the credential
    /// in the *next* token, so neither half contains an `=` and neither could ever
    /// match. It went to disk verbatim, contradicting this module's own doc comment.
    #[test]
    fn a_credential_passed_as_a_separate_flag_value_is_redacted() {
        for flag in ["-Password", "--api-key", "/Secret", "-Token"] {
            let redacted = redact_parameters(&format!("-Verbose {flag} hunter2 -Force"));
            assert!(
                !redacted.contains("hunter2"),
                "{flag} left the credential in: {redacted}"
            );
            assert!(
                redacted.contains("<redacted>"),
                "{flag} should mark the value: {redacted}"
            );
            // Everything that is not the credential survives.
            assert!(redacted.starts_with("-Verbose "), "{redacted}");
            assert!(redacted.ends_with("-Force"), "{redacted}");
        }
    }

    /// A sensitive flag given no value must not swallow the next flag: that loses
    /// evidence and misrepresents what actually ran.
    #[test]
    fn a_valueless_sensitive_flag_does_not_swallow_the_next_flag() {
        assert_eq!(
            redact_parameters("-Password -Verbose"),
            "-Password -Verbose"
        );
    }

    /// Ordinary flags must not trigger it, or the audit trail redacts itself away.
    #[test]
    fn ordinary_flag_values_pass_through() {
        let params = "-Path C:/temp -Retries 3 -Force";
        assert_eq!(redact_parameters(params), params);
    }
}
