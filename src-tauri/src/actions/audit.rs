//! Append-only audit trail for dispatched actions.
//!
//! Written **before** the request goes out and again once it settles, so a crash
//! mid-batch still leaves evidence of what was attempted. Best-effort throughout:
//! a failed audit write warns but never blocks the operator.
//!
//! Nothing is ever deleted. Past [`ROTATE_AT_BYTES`] the live file is moved aside as
//! a dated generation ([`rotated_generations`]) and a fresh one started, so reading
//! the newest records costs one bounded file rather than the whole history.
//!
//! Secrets discipline applies here as everywhere — a `parameters` string is
//! operator-authored free text, so anything that looks like a credential is
//! redacted before it reaches disk. Tokens are never written at all.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Serialize;
use tracing::warn;

use super::{ActionKind, JobReport, JobState};

/// The log's filename now lives in `paths::audit_path`, which single-sources the
/// whole location; this copy only names temp files in the tests below.
#[cfg(test)]
const AUDIT_FILE: &str = "action-audit.jsonl";

/// A key naming any of these anywhere is a credential. Matched against the key
/// lowercased with every non-alphanumeric dropped, so `--api-key`, `api_key` and
/// `ApiKey` are one name.
///
/// `key`, `pass` and `pw` are deliberately substrings — `-Key1`, `-StorageKeys`,
/// `-EncryptionKeyValue`, `-AdminPass2` are all credentials — and the few ordinary
/// names they catch are exempted one by one in [`BENIGN_KEY_NAMES`].
/// (`pass` covers password/passphrase, `pw` covers pwd, `key` covers apikey, `cred`
/// covers credential.)
const SENSITIVE_KEY_FRAGMENTS: [&str; 9] = [
    "pass",
    "pw",
    "secret",
    "token",
    "key",
    "cred",
    "authorization",
    "bearer",
    // A connection string carries its password as one `;` segment among several,
    // and a quoted one spans tokens; redacting the whole value is the safe reading.
    "connectionstring",
];

/// Credential words only at the *end* of a name: `auth` anywhere would take
/// `-Author` and `-Authentication Kerberos` (a mechanism, not a secret), while
/// `-Auth` and `basicAuth` carry one.
const SENSITIVE_KEY_SUFFIXES: [&str; 2] = ["auth", "sas"];

/// Whole names that contain a credential fragment but are ordinary PowerShell or
/// registry parameters. Their values are evidence of what ran, and redacting them
/// cost the audit trail that evidence.
const BENIGN_KEY_NAMES: [&str; 6] = [
    "passthru",
    "registrykey",
    "regkey",
    "subkey",
    "keypath",
    "bypass",
];

/// Query-string names that carry a credential but are too short to match as a
/// parameter name: `sig` is an Azure SAS signature.
const SENSITIVE_QUERY_NAMES: [&str; 1] = ["sig"];

/// Script parameters as the audit log stores them: through [`redact_parameters`]
/// by construction, since the only way to make one is [`Redacted::of`].
///
/// This was a plain `Option<String>` that the one dispatch site remembered to
/// redact, so a second construction site that forgot would have written a
/// credential to disk with nothing to stop it.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct Redacted(String);

impl Redacted {
    /// `None` for an empty string: a device with nothing to pass records no
    /// parameters rather than an empty one.
    pub fn of(raw: &str) -> Option<Self> {
        (!raw.is_empty()).then(|| Self(redact_parameters(raw)))
    }
}

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
    /// What was sent, redacted — the type sees to that.
    pub parameters: Option<Redacted>,
    pub dry_run: bool,
    /// True when this dispatch went out only because the operator overrode a closed
    /// maintenance window. Omitted otherwise, so the log's shape is unchanged for
    /// every ordinary dispatch and a bypass stands out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub window_override: bool,
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

    /// The record that closes out the one written at dispatch, once `job` has an
    /// outcome — from the poller when the activity feed settles it, or from the
    /// dispatch itself when NinjaOne rejected the request outright. `parameters` and
    /// the confirm-token prefix are already on the opening record.
    pub fn closing(job: &JobReport, instance: String, client_id: Option<String>) -> Self {
        Self {
            timestamp: now_stamp(),
            instance,
            client_id,
            batch_id: job.batch_id,
            job_id: job.id,
            kind: job.kind,
            device_id: job.device_id,
            device_name: job.device_name.clone(),
            organization: job.organization.clone(),
            detail: job.detail.clone(),
            parameters: None,
            dry_run: job.dry_run,
            // Already on the opening record, like the parameters.
            window_override: false,
            confirm_token_prefix: None,
            outcome: Self::outcome_of(&job.state),
            activity_id: job.activity_id,
            series_uid: job.series_uid.clone(),
            exit_code: job.exit_code,
        }
    }

    /// The closing record for a job whose session ended before its outcome was
    /// known — refused at `append_jobs` or wiped by `clear_jobs` (which every tenant
    /// switch runs). Without it the trail held a "dispatching" record with no close,
    /// which reads the same as a crash mid-batch.
    ///
    /// No activity id or exit code: an unresolved close claims no outcome, so it
    /// carries nothing that reads like one. The series uid came from the dispatch
    /// response, so it stays as the correlator.
    pub fn unresolved(job: &JobReport, instance: String, client_id: Option<String>) -> Self {
        Self {
            outcome: UNRESOLVED_SESSION_ENDED.into(),
            activity_id: None,
            exit_code: None,
            ..Self::closing(job, instance, client_id)
        }
    }
}

/// The outcome [`AuditEntry::unresolved`] records.
pub const UNRESOLVED_SESSION_ENDED: &str = "unresolved: session ended before the outcome was known";

/// Replaces the value of any `key=value` token whose key looks like a credential.
///
/// Operators paste script parameters by hand, and a script that takes a service
/// password would otherwise write it to disk in cleartext.
///
/// Redaction is by *name*: a credential passed positionally (`Set-Thing hunter2`)
/// has none and cannot be told from any other value, so it is written as typed.
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
    //
    // `:` separates a flag from its value as well as `=` does — `-Password:hunter2`
    // is PowerShell's inline form. It used to be read as one bare flag, so the
    // credential went to disk verbatim and the *following* token was redacted in its
    // place. And a quoted value spans tokens once split on whitespace, so a redaction
    // that stopped at the first one left the rest of the passphrase in the log.
    //
    // A credential can also sit inside a quoted run that an *unredacted* token opened
    // (`-Conn "Server=a; Password=hunter 2"`), so the open quote is tracked across
    // tokens and a redaction inside it swallows through its close.
    let mut out: Vec<String> = Vec::new();
    let mut owed = Owed::Nothing;
    let mut open_quote: Option<char> = None;
    for token in parameters.split_whitespace() {
        match owed {
            Owed::ClosingQuote(q) => {
                if token.ends_with(q) {
                    owed = Owed::Nothing;
                }
                continue;
            }
            Owed::Value => {
                owed = Owed::Nothing;
                // Only a *value* is swallowed. `-Password -Verbose` means the flag
                // was given no value, and blanking the following flag would both
                // lose evidence and misrepresent what ran.
                if !is_flag(token) {
                    out.push("<redacted>".into());
                    // `"Server=a; Password= hunter 2"`: the value's tail runs to the
                    // close of a quote an earlier token opened.
                    owed = match open_quote.take() {
                        Some(q) if !closes_quote(token, q) => Owed::ClosingQuote(q),
                        Some(_) => Owed::Nothing,
                        None => quote_owed(token),
                    };
                    continue;
                }
            }
            Owed::Nothing => {}
        }
        let (text, redacted, own_owed) = match split_key_value(token) {
            // `-Password: hunter2` / `password= hunter2`: the value is the next token.
            Some((key, sep, "")) if is_sensitive(key) => {
                out.push(format!("{key}{sep}"));
                owed = Owed::Value;
                // `"Password= hunter 2"`: the quote is on this token, and the value
                // token must know it is inside the run.
                open_quote = quote_after(open_quote, token);
                continue;
            }
            // The quote may sit on the key (`"Pwd=hunter 2;Server=a"`), not the value.
            Some((key, sep, value)) if is_sensitive(key) => (
                format!("{key}{sep}<redacted>"),
                true,
                opens_quote(token).map_or_else(|| quote_owed(value), Owed::ClosingQuote),
            ),
            // Only a bare flag names the next token as its value. A flag that already
            // carried one (`-Mode:password-reset`) must not redact whatever follows
            // it just because its *value* looks sensitive. A quoted flag name
            // (`"-Password" hunter2`) is still a flag.
            kv => {
                let bare = token.trim_matches(['"', '\'']);
                if (kv.is_none() && is_flag(bare) && is_sensitive(bare))
                    || ends_with_empty_sensitive_segment(token)
                {
                    owed = Owed::Value;
                }
                let text = redact_embedded(token);
                let redacted = text != token;
                let own = match opens_quote(token) {
                    Some(q) if redacted => Owed::ClosingQuote(q),
                    _ => Owed::Nothing,
                };
                (text, redacted, own)
            }
        };
        out.push(text);
        if redacted {
            owed = match open_quote.take() {
                // Inside a run this token does not close: the rest of the run is the
                // redacted value's tail.
                Some(q) if !closes_quote(token, q) => Owed::ClosingQuote(q),
                Some(_) => Owed::Nothing,
                None => own_owed,
            };
        } else {
            open_quote = quote_after(open_quote, token);
        }
    }
    out.join(" ")
}

/// The quote `token` opens and leaves open: it starts the token or its value, and
/// appears an odd number of times. An apostrophe inside a word (`O'Brien`) opens
/// nothing.
fn opens_quote(token: &str) -> Option<char> {
    let value = split_key_value(token).map_or(token, |(_, _, v)| v);
    ['"', '\'']
        .into_iter()
        .find(|&q| (token.starts_with(q) || value.starts_with(q)) && closes_quote(token, q))
}

/// Whether `token` holds an odd number of `q`, so it closes a run `q` opened.
fn closes_quote(token: &str, q: char) -> bool {
    token.matches(q).count() % 2 == 1
}

/// What [`redact_parameters`] still owes after the token it just wrote.
#[derive(Clone, Copy)]
enum Owed {
    Nothing,
    /// A sensitive flag with no inline value: the next token is its value.
    Value,
    /// A redacted value opened a quote; swallow tokens through the closing one. An
    /// unterminated quote swallows the rest of the line, which is the safe way to be
    /// wrong here.
    ClosingQuote(char),
}

/// Splits `key=value` / `key:value` at whichever separator comes first, so a path in
/// a value (`logPath=C:/temp`) still splits at the `=`.
fn split_key_value(token: &str) -> Option<(&str, char, &str)> {
    let at = token.find(['=', ':'])?;
    let sep = token[at..].chars().next()?;
    Some((&token[..at], sep, &token[at + sep.len_utf8()..]))
}

/// Whether a redacted `value` opened a quote it did not close in the same token.
fn quote_owed(value: &str) -> Owed {
    match value.chars().next() {
        Some(q @ ('"' | '\'')) if value.len() == 1 || !value.ends_with(q) => Owed::ClosingQuote(q),
        _ => Owed::Nothing,
    }
}

/// Whether `token` is a flag rather than a value: `-Password`, `--api-key`, or the
/// `/Password` form Windows tooling uses.
fn is_flag(token: &str) -> bool {
    token.starts_with('-') || token.starts_with('/')
}

/// The form every name is judged in: lowercase ASCII alphanumerics only.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn is_sensitive(key: &str) -> bool {
    let name = normalize(key);
    if BENIGN_KEY_NAMES.contains(&name.as_str()) {
        return false;
    }
    SENSITIVE_KEY_FRAGMENTS.iter().any(|f| name.contains(f))
        || SENSITIVE_KEY_SUFFIXES.iter().any(|f| name.ends_with(f))
}

/// The quoted run still open after an unredacted `token`, given the one open before it.
fn quote_after(open: Option<char>, token: &str) -> Option<char> {
    match open {
        Some(q) if closes_quote(token, q) => None,
        Some(q) => Some(q),
        None => opens_quote(token),
    }
}

/// `conn=a;Password= hunter2` or `user:svc,password: hunter2`: a sensitive segment
/// with an empty value, whose value is the next token, as with `-Password hunter2`.
/// Only a segment's *own* name counts — a name chained after an `=` is how base64
/// padding (`…Pw=`) looks.
fn ends_with_empty_sensitive_segment(token: &str) -> bool {
    let Some(at) = token.rfind(SEGMENT_SEPARATORS) else {
        return false;
    };
    token[at + 1..]
        .strip_suffix(['=', ':'])
        .is_some_and(|name| !name.contains(['=', ':']) && is_sensitive(name))
}

/// Where [`redact_embedded`] cuts a token into segments, each judged on its own.
///
/// `;`, `&` and `?` separate connection-string and query-string segments. `,`, `{`
/// and `[` separate the entries of a list or a JSON object written without spaces,
/// whose names end at `:` (`user:svc,password:x`, `{"user":"svc","password":"x"}`).
/// Only a segment's *first* name may end at `:` (see [`embedded_value_at`]), so
/// without these cuts every entry after the first went to disk verbatim.
const SEGMENT_SEPARATORS: [char; 6] = [';', '&', '?', ',', '{', '['];

/// The separators after which a bare `sig` is an Azure SAS signature.
const QUERY_SEPARATORS: [char; 3] = [';', '&', '?'];

/// Redacts credentials carried *inside* a token whose own key is not sensitive.
///
/// The key/value split stops at the first `=` or `:`, so `conn=Server=a;Password=x`
/// is keyed on `conn`, `/p:Password=x` on `/p` and `https://user:pass@host` on
/// `https`, and all of them went to disk verbatim. Two embedded shapes are common
/// enough to scan for: URL userinfo, and segments cut at [`SEGMENT_SEPARATORS`]
/// (connection strings, query strings, compact lists and JSON), each scanned by
/// [`embedded_value_at`].
fn redact_embedded(token: &str) -> String {
    let token = redact_userinfo(token);
    let mut out = String::with_capacity(token.len());
    let mut after_query_separator = false;
    for piece in token.split_inclusive(SEGMENT_SEPARATORS) {
        let body = piece.trim_end_matches(SEGMENT_SEPARATORS);
        match embedded_value_at(body, after_query_separator) {
            Some(at) => {
                out.push_str(&body[..at]);
                out.push_str("<redacted>");
                out.push_str(&piece[body.len()..]);
            }
            None => out.push_str(piece),
        }
        after_query_separator = piece.ends_with(QUERY_SEPARATORS);
    }
    out
}

/// Where a credential value starts in one segment, if a name in it is sensitive.
///
/// Names chain: `conn=Pwd=x` is `conn` then `Pwd`, and `-Conn:Pwd=x` is `-Conn`
/// then `Pwd`, so every name up to an `=` is judged, not only the first. The first
/// name may end at `:` as well, like the token-level split; later ones only at `=`,
/// so a URL's `host:port` is not read as a name. A value that is empty or only `=`
/// is base64 padding (`productAllowListB64=…Pw==`), not a credential.
fn embedded_value_at(segment: &str, after_separator: bool) -> Option<usize> {
    let mut start = 0;
    loop {
        let seps: &[char] = if start == 0 { &['=', ':'] } else { &['='] };
        let end = start + segment[start..].find(seps)?;
        let name = &segment[start..end];
        // Both separators are one byte.
        let value = end + 1;
        let query_secret = after_separator
            && start == 0
            && SENSITIVE_QUERY_NAMES.contains(&normalize(name).as_str());
        if (is_sensitive(name) || query_secret) && !segment[value..].trim_matches('=').is_empty() {
            return Some(value);
        }
        start = value;
    }
}

/// `scheme://user:secret@host` → `scheme://user:<redacted>@host`. Userinfo with no
/// `:` is usually a token standing in for the user name (`https://ghp_x@github.com`),
/// so all of it is redacted.
fn redact_userinfo(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    let mut rest = token;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let authority_end = tail.find(['/', '?', '#']).unwrap_or(tail.len());
        match tail[..authority_end].rfind('@') {
            Some(userinfo_end) => {
                match tail[..userinfo_end].split_once(':') {
                    Some((user, _)) => {
                        out.push_str(user);
                        out.push_str(":<redacted>");
                    }
                    None => out.push_str("<redacted>"),
                }
                rest = &tail[userinfo_end..];
            }
            None => rest = tail,
        }
    }
    out.push_str(rest);
    out
}

fn audit_path() -> Option<PathBuf> {
    crate::paths::audit_path().ok()
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

/// One record as the exact bytes appended: the JSON and its newline in one buffer,
/// so it reaches the file in a single `write_all`.
///
/// This was `writeln!(file, "{line}")` on the unbuffered `File`, which issues the
/// JSON and the newline as two separate writes. Dispatch audits from a task per
/// device and the poller closes records concurrently, so two appenders could land
/// between each other's halves — two records fused on one line followed by an empty
/// one — and a crash between the halves left a record with no terminator for the
/// next append to run into. Either way a JSONL reader loses both records.
fn encode_line(entry: &AuditEntry) -> serde_json::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    Ok(line)
}

/// Past this size the live log is moved aside as a dated generation and a fresh one
/// started. Nothing is ever deleted: the cap bounds what reading the newest records
/// costs (the Jobs tab does it on every visit), not how much history is kept.
const ROTATE_AT_BYTES: u64 = 8 * 1024 * 1024;

/// How many names [`rotate_if_full`] tries for one second's generation before it
/// gives up and lets the live file grow past the cap.
const ROTATE_NAME_ATTEMPTS: u32 = 100;

/// Serializes this process's appenders. Rotation moves the live file, so two writers
/// deciding to rotate at once would race over it. Another process (a second app
/// instance) is covered by the no-clobber link in [`rotate_if_full`].
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// The half of [`record_all`] that does not depend on the OS config directory, so it
/// can be tested against a temp path. `record_all` itself was untestable — it
/// resolved its own destination — which left the directory creation, the 0600 mode
/// and the append behaviour with no coverage at all; only the pure `redact_parameters`
/// helper was asserted.
fn write_records(path: &Path, entries: &[AuditEntry]) {
    write_records_rotating_at(path, entries, ROTATE_AT_BYTES, Utc::now());
}

/// [`write_records`] with its rotation threshold and clock injected for the tests.
fn write_records_rotating_at(
    path: &Path,
    entries: &[AuditEntry],
    rotate_at: u64,
    now: DateTime<Utc>,
) {
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
    let _serialized = WRITE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    rotate_if_full(path, rotate_at, now);
    // A record torn by a crash or a full disk has no newline, and the next append
    // would run on from it: the reader then loses the torn record *and* the whole
    // one after it. Starting on a fresh line costs only the torn one.
    let mut torn = ends_mid_line(path);
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
        let mut line = match encode_line(entry) {
            Ok(l) => l,
            Err(err) => {
                warn!(?err, "could not serialize an audit record");
                continue;
            }
        };
        if torn {
            line.insert(0, b'\n');
            torn = false;
        }
        if let Err(err) = file.write_all(&line) {
            warn!(?err, path = %path.display(), "could not append to the action audit log");
            return;
        }
    }
}

/// Whether the file's last byte is something other than a newline: a record was cut
/// off mid-write. A missing or empty file is not.
fn ends_mid_line(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1)).is_ok() && file.read_exact(&mut last).is_ok() && last[0] != b'\n'
}

/// Moves a live log of `rotate_at` bytes or more aside as a dated generation (see
/// [`generation_path`]), so the next append starts a fresh file.
///
/// Linked under the new name and then unlinked, never renamed onto it. `hard_link`
/// refuses a name that exists, so a generation another app instance made in the same
/// second is never overwritten. A record appended through a handle opened before the
/// move still lands in the generation, because both names are one file. A file system
/// without hard links falls back to a rename, after checking the name is free.
fn rotate_if_full(path: &Path, rotate_at: u64, now: DateTime<Utc>) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() < rotate_at {
        return;
    }
    for n in 0..ROTATE_NAME_ATTEMPTS {
        let target = generation_path(path, now, n);
        match std::fs::hard_link(path, &target) {
            Ok(()) => {
                if let Err(err) = std::fs::remove_file(path) {
                    // Both names still point at the live file. Drop the new one, or
                    // the reader would read every record in it twice.
                    warn!(
                        ?err,
                        "could not move the full audit log aside; it keeps growing"
                    );
                    let _ = std::fs::remove_file(&target);
                }
                return;
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            // Another writer moved it first; the append starts the fresh file.
            Err(err) if err.kind() == ErrorKind::NotFound => return,
            Err(_) => {
                if target.exists() {
                    continue;
                }
                if let Err(err) = std::fs::rename(path, &target) {
                    warn!(
                        ?err,
                        "could not move the full audit log aside; it keeps growing"
                    );
                }
                return;
            }
        }
    }
    warn!("no free name for the rotated audit log; it keeps growing past the cap");
}

/// The name the live log takes when it is rotated at `at`: `action-audit.jsonl`
/// becomes `action-audit-20261030T150512.jsonl`, and the `n`-th name tried in the
/// same second gets `-n` before the extension.
pub(crate) fn generation_path(live: &Path, at: DateTime<Utc>, n: u32) -> PathBuf {
    let stem = live
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("action-audit");
    let ext = live.extension().and_then(|e| e.to_str()).unwrap_or("jsonl");
    let stamp = at.format(GENERATION_STAMP);
    let name = match n {
        0 => format!("{stem}-{stamp}.{ext}"),
        n => format!("{stem}-{stamp}-{n}.{ext}"),
    };
    live.with_file_name(name)
}

/// How a generation's name dates it, in UTC.
const GENERATION_STAMP: &str = "%Y%m%dT%H%M%S";

/// Every rotated generation of the live log at `live`, newest first. Names that do
/// not parse as one are ignored.
pub(crate) fn rotated_generations(live: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(stem), Some(ext)) = (
        live.parent(),
        live.file_stem().and_then(|s| s.to_str()),
        live.extension().and_then(|e| e.to_str()),
    ) else {
        return Vec::new();
    };
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let prefix = format!("{stem}-");
    let suffix = format!(".{ext}");
    let mut found: Vec<((NaiveDateTime, u32), PathBuf)> = listing
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let middle = name.strip_prefix(&prefix)?.strip_suffix(&suffix)?;
            let (stamp, n) = match middle.split_once('-') {
                Some((stamp, n)) => (stamp, n.parse::<u32>().ok()?),
                None => (middle, 0),
            };
            let at = NaiveDateTime::parse_from_str(stamp, GENERATION_STAMP).ok()?;
            Some(((at, n), entry.path()))
        })
        .collect();
    found.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
    found.into_iter().map(|(_, path)| path).collect()
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

    /// Credential names the fragment list used to miss, and credentials embedded in a
    /// value whose own key is not sensitive (the key split stops at the first `=`/`:`).
    #[test]
    fn credential_names_and_embedded_credentials_the_matcher_used_to_miss() {
        for (input, expected) in [
            ("-Pwd hunter2 -Force", "-Pwd <redacted> -Force"),
            ("-Credential hunter2", "-Credential <redacted>"),
            ("-Cred hunter2", "-Cred <redacted>"),
            ("-Auth hunter2", "-Auth <redacted>"),
            ("basicAuth=hunter2", "basicAuth=<redacted>"),
            ("-StorageSas hunter2", "-StorageSas <redacted>"),
            ("adminPass=hunter2", "adminPass=<redacted>"),
            (
                "connectionString=Server=a;Password=hunter2",
                "connectionString=<redacted>",
            ),
            (
                r#"-ConnectionString "Server=a;Password=hunter2" -Force"#,
                "-ConnectionString <redacted> -Force",
            ),
            // The key is not sensitive; the `;` segment is.
            (
                "conn=Server=a;Pwd=hunter2;Encrypt=true",
                "conn=Server=a;Pwd=<redacted>;Encrypt=true",
            ),
            (
                r#"-Conn "Server=a;Password=hunter2""#,
                r#"-Conn "Server=a;Password=<redacted>"#,
            ),
            (
                "https://user:hunter2@host/path",
                "https://user:<redacted>@host/path",
            ),
            (
                "-Uri:https://user:hunter2@host",
                "-Uri:https://user:<redacted>@host",
            ),
            (
                "repo=https://hunter2@github.com/o/r",
                "repo=https://<redacted>@github.com/o/r",
            ),
            (
                "url=https://host/x?a=1&token=hunter2",
                "url=https://host/x?a=1&token=<redacted>",
            ),
        ] {
            let redacted = redact_parameters(input);
            assert_eq!(redacted, expected, "{input}");
            assert!(!redacted.contains("hunter2"), "{redacted}");
        }
    }

    /// Narrowing `key`/`pass` to suffixes un-redacted names the old substring match
    /// caught; nested names after `:`/`=` and quoted runs opened by an unredacted
    /// token leaked through the embedded scan. Never redact less than before.
    #[test]
    fn names_the_substring_match_caught_and_nested_or_quoted_embedded_values() {
        for (input, expected) in [
            ("-Key1 hunter2", "-Key1 <redacted>"),
            ("-AdminPass2 hunter2", "-AdminPass2 <redacted>"),
            ("-PassString hunter2", "-PassString <redacted>"),
            (
                "-EncryptionKeyValue hunter2",
                "-EncryptionKeyValue <redacted>",
            ),
            ("-KeyData hunter2", "-KeyData <redacted>"),
            ("-StorageKeys hunter2", "-StorageKeys <redacted>"),
            ("-Keys hunter2", "-Keys <redacted>"),
            // The first segment's value is itself a name nobody judged.
            (
                "-Conn:Pwd=hunter2;Server=a",
                "-Conn:Pwd=<redacted>;Server=a",
            ),
            ("conn=Pwd=hunter2;Server=a", "conn=Pwd=<redacted>;Server=a"),
            ("/p:Password=hunter2", "/p:Password=<redacted>"),
            // The embedded redaction opens no quote of its own, but the token did.
            (
                r#"-Conn "Server=a;Password=hunter 2" -Force"#,
                r#"-Conn "Server=a;Password=<redacted> -Force"#,
            ),
            // The quote was opened by an earlier, unredacted token.
            (
                r#"-Conn "Server=a; Password=hunter 2" -Force"#,
                r#"-Conn "Server=a; Password=<redacted> -Force"#,
            ),
            (
                r#"-Authorization "Bearer hunter2" -Force"#,
                "-Authorization <redacted> -Force",
            ),
            (
                "-Headers Authorization=Bearer_hunter2",
                "-Headers Authorization=<redacted>",
            ),
            ("-BearerValue hunter2", "-BearerValue <redacted>"),
            (
                "-Uri https://h/x?sv=1&sig=hunter2",
                "-Uri https://h/x?sv=1&sig=<redacted>",
            ),
        ] {
            let redacted = redact_parameters(input);
            assert_eq!(redacted, expected, "{input}");
            assert!(!redacted.contains("hunter"), "{redacted}");
        }
    }

    /// The quote that opens a sensitive value can sit on its *key* token, on an
    /// earlier token, or around a flag name; and a nested segment's value can be the
    /// next token. Each left the tail of the credential in the log.
    #[test]
    fn quotes_on_the_key_and_values_in_the_next_token_are_redacted() {
        for (input, expected) in [
            (
                r#"-Conn "Pwd=hunter 2;Server=a" -Force"#,
                r#"-Conn "Pwd=<redacted> -Force"#,
            ),
            (
                "-Conn 'Password=hunter 2' -Force",
                "-Conn 'Password=<redacted> -Force",
            ),
            (
                r#"x "token=hunter b c" -Force"#,
                r#"x "token=<redacted> -Force"#,
            ),
            (
                "conn=a;Password= hunter2 -Force",
                "conn=a;Password= <redacted> -Force",
            ),
            (
                r#"-Conn "Server=a; Password= hunter 2" -Force"#,
                r#"-Conn "Server=a; Password= <redacted> -Force"#,
            ),
            // The quote is on the key token and the value is the next one.
            (
                r#"-Conn "Password= hunter 2" -Force"#,
                r#"-Conn "Password= <redacted> -Force"#,
            ),
            (
                r#"-Conn "Pwd: hunter 2" -Force"#,
                r#"-Conn "Pwd: <redacted> -Force"#,
            ),
            (
                r#""-Password" hunter2 -Force"#,
                r#""-Password" <redacted> -Force"#,
            ),
        ] {
            let redacted = redact_parameters(input);
            assert_eq!(redacted, expected, "{input}");
            assert!(
                !redacted.contains("hunter") && !redacted.contains(" 2"),
                "{redacted}"
            );
        }
    }

    /// Standard base64 pads with `=`, and a chunk before the padding can spell a
    /// credential fragment (`Pw`, `Key`). Padding is not a value.
    #[test]
    fn base64_padding_is_not_read_as_an_embedded_value() {
        let params = "productAllowListB64=Q2hyb21lPw== rebootBehavior=Never dryRun=true";
        assert_eq!(redact_parameters(params), params);
        // `sig` is only a credential as a query name, not as the token's own key.
        assert_eq!(redact_parameters("sig=abc"), "sig=abc");
    }

    /// `pass` and `key` match anywhere, which redacted ordinary PowerShell and registry
    /// flags and cost the audit trail its evidence of what ran; those are exempted by
    /// whole name. `-Author` and `-Authentication` must not trip the `auth` suffix.
    #[test]
    fn names_that_only_contain_a_short_credential_word_pass_through() {
        for params in [
            "-PassThru C:/out -Force",
            r"-RegistryKey HKLM\SOFTWARE\Contoso -Force",
            r"-KeyPath HKLM\SOFTWARE\Contoso",
            "-Author ops -Authentication Kerberos",
            "-ExecutionPolicy Bypass -Bypass yes",
            "https://example.com/path?a=1&b=2 ssh://host:22/x",
            "kbAllowList=5040434 productAllowListB64=Q2hyb21l dryRun=true",
        ] {
            assert_eq!(redact_parameters(params), params);
        }
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
            window_override: false,
            confirm_token_prefix: None,
            outcome: "dispatching".into(),
            activity_id: None,
            series_uid: None,
            exit_code: None,
        }
    }

    /// An override of a closed maintenance window is on the record; an ordinary
    /// dispatch's record keeps the shape it always had.
    #[test]
    fn a_window_override_is_written_only_when_it_happened() {
        let ordinary = serde_json::to_value(sample_entry(1)).expect("json");
        assert!(ordinary.get("windowOverride").is_none(), "{ordinary}");
        let overridden = serde_json::to_value(AuditEntry {
            window_override: true,
            ..sample_entry(2)
        })
        .expect("json");
        assert_eq!(overridden["windowOverride"], serde_json::json!(true));
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

    /// Each record is one newline-terminated buffer with no interior newline, and
    /// concurrent appenders — dispatch audits per device, from parallel tasks —
    /// never split or fuse one. Every line must read back as a whole record.
    #[test]
    fn concurrent_appends_never_split_or_fuse_a_record() {
        let one = encode_line(&sample_entry(1)).expect("serializes");
        assert_eq!(one.last(), Some(&b'\n'));
        assert_eq!(one.iter().filter(|b| **b == b'\n').count(), 1);

        let dir = std::env::temp_dir().join(format!("njp-audit-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(AUDIT_FILE);

        const WRITERS: u64 = 8;
        const PER_WRITER: u64 = 50;
        std::thread::scope(|s| {
            for w in 0..WRITERS {
                let path = &path;
                s.spawn(move || {
                    for i in 0..PER_WRITER {
                        let mut entry = sample_entry(w * PER_WRITER + i);
                        // Large enough that a split write would have room to interleave.
                        entry.detail = "x".repeat(2048);
                        write_records(path, &[entry]);
                    }
                });
            }
        });

        let body = std::fs::read_to_string(&path).expect("the log exists");
        let mut ids: Vec<u64> = body
            .lines()
            .map(|line| {
                let v: serde_json::Value =
                    serde_json::from_str(line).expect("every line is exactly one record");
                v["jobId"].as_u64().expect("job id")
            })
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, (0..WRITERS * PER_WRITER).collect::<Vec<_>>());
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
        // `:` is a separator now, so a non-sensitive inline value must survive it.
        let inline = "-Path:C:/temp logPath=C:/logs/run.txt https://example.com";
        assert_eq!(redact_parameters(inline), inline);
    }

    /// `-Password:hunter2` is PowerShell's inline form. It read as one bare flag, so
    /// the credential was written verbatim and the *next* token redacted instead.
    #[test]
    fn a_colon_separated_credential_is_redacted_in_place() {
        assert_eq!(
            redact_parameters("-Password:hunter2 -Verbose"),
            "-Password:<redacted> -Verbose"
        );
        assert_eq!(
            redact_parameters("-Password:hunter2 C:/temp"),
            "-Password:<redacted> C:/temp",
            "the token after an inline value is not the credential"
        );
        // An empty inline value means the credential is the next token.
        assert_eq!(
            redact_parameters("-Password: hunter2 -Force"),
            "-Password: <redacted> -Force"
        );
        assert_eq!(
            redact_parameters("servicePassword= hunter2"),
            "servicePassword= <redacted>"
        );
    }

    /// A flag that already carried its value must not redact what follows just
    /// because the value mentions something sensitive.
    #[test]
    fn a_flag_with_an_inline_value_does_not_redact_the_next_token() {
        assert_eq!(
            redact_parameters("-Mode:password-reset -Force"),
            "-Mode:password-reset -Force"
        );
    }

    /// Split on whitespace, a quoted passphrase is several tokens; stopping at the
    /// first left the rest of it on disk.
    #[test]
    fn a_quoted_multi_word_value_is_redacted_through_its_closing_quote() {
        for (input, expected) in [
            (
                r#"-Password "correct horse battery" -Force"#,
                "-Password <redacted> -Force",
            ),
            (
                r#"-Password:"correct horse battery" -Force"#,
                "-Password:<redacted> -Force",
            ),
            (
                "servicePassword='correct horse' dryRun=true",
                "servicePassword=<redacted> dryRun=true",
            ),
            // A quoted single word closes in its own token.
            (
                r#"-Password "hunter2" -Force"#,
                "-Password <redacted> -Force",
            ),
            // A lone quote opens the value; its partner closes it.
            (
                r#"-Password " hunter 2 " -Force"#,
                "-Password <redacted> -Force",
            ),
        ] {
            let redacted = redact_parameters(input);
            assert_eq!(redacted, expected, "{input}");
            assert!(
                !redacted.contains("horse") && !redacted.contains("hunter"),
                "{redacted}"
            );
        }
        // An unterminated quote swallows the rest rather than leak it.
        assert_eq!(
            redact_parameters(r#"-Password "correct horse -Force"#),
            "-Password <redacted>"
        );
    }

    /// A send-time rejection is closed out at dispatch, since the poller never sees
    /// it. The record carries the outcome but not the parameters, which the opening
    /// record already holds.
    #[test]
    fn a_closing_record_carries_the_outcome_and_correlators() {
        let job = JobReport {
            id: 9,
            batch_id: 3,
            device_id: 7,
            device_name: "srv-1".into(),
            organization: "Contoso".into(),
            kind: ActionKind::Reboot,
            detail: "Reboot (NORMAL)".into(),
            dry_run: false,
            state: JobState::Failed("400 not applicable".into()),
            dispatched_at: String::new(),
            dispatched_ts: 0,
            finished_at: None,
            duration_seconds: None,
            activity_id: Some(11),
            series_uid: None,
            exit_code: Some(1),
            request: None,
        };
        let entry = AuditEntry::closing(&job, "https://x".into(), None);
        assert_eq!(entry.outcome, "Failed: 400 not applicable");
        assert_eq!((entry.batch_id, entry.job_id), (3, 9));
        assert_eq!(entry.activity_id, Some(11));
        assert_eq!(entry.exit_code, Some(1));
        assert!(entry.parameters.is_none() && entry.confirm_token_prefix.is_none());
    }

    /// What the log stores for `raw`, through the one constructor it has.
    fn logged(raw: &str) -> String {
        serde_json::to_value(Redacted::of(raw).expect("non-empty"))
            .expect("json")
            .as_str()
            .expect("a string")
            .to_string()
    }

    /// Only a segment's first name may end at `:`, and segments were cut only at
    /// `;`, `&` and `?`. A compact JSON object or `key:value` list whose first entry
    /// was not a credential therefore reached disk with every later one verbatim.
    #[test]
    fn colon_separated_credentials_after_the_first_entry_are_redacted() {
        for (input, expected) in [
            (
                r#"-Config {"user":"svc","password":"hunter2"}"#,
                r#"-Config {"user":"svc","password":<redacted>"#,
            ),
            ("user:svc,password:hunter2", "user:svc,password:<redacted>"),
            (
                r#"-Body {"cfg":{"apiKey":"hunter2"}}"#,
                r#"-Body {"cfg":{"apiKey":<redacted>"#,
            ),
            (
                r#"-Items [{"name":"a","token":"hunter2"}]"#,
                r#"-Items [{"name":"a","token":<redacted>"#,
            ),
            // The value sits in the next token, as `-Password hunter2` does.
            (
                "user:svc,password: hunter2",
                "user:svc,password: <redacted>",
            ),
        ] {
            let stored = logged(input);
            assert_eq!(stored, expected, "{input}");
            assert!(!stored.contains("hunter2"), "{stored}");
        }
    }

    /// The shapes that were already redacted stay redacted, and what is not a
    /// credential in a list or JSON keeps its evidence.
    #[test]
    fn redaction_of_the_shapes_already_handled_is_unchanged() {
        for (input, expected) in [
            ("password=hunter2", "password=<redacted>"),
            ("-Password hunter2", "-Password <redacted>"),
            (r#"{"password":"hunter2"}"#, r#"{"password":<redacted>"#),
            ("apiKey:abc123 mode=fast", "apiKey:<redacted> mode=fast"),
        ] {
            assert_eq!(logged(input), expected, "{input}");
        }
        for evidence in [
            "kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=true",
            r#"-Config {"user":"svc","mode":"fast"}"#,
            "hosts:web01,db01:1433",
        ] {
            assert_eq!(logged(evidence), evidence);
        }
    }

    fn temp_log(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("njp-audit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(AUDIT_FILE);
        (dir, path)
    }

    fn job_ids(path: &Path) -> Vec<u64> {
        std::fs::read_to_string(path)
            .expect("readable")
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v["jobId"].as_u64())
            .collect()
    }

    /// A record cut off mid-write has no newline, and the next append ran on from
    /// it: the reader then lost the next, whole record as well. The next append
    /// starts on a line of its own.
    #[test]
    fn an_append_after_a_torn_record_starts_a_new_line() {
        let (dir, path) = temp_log("torn");
        std::fs::write(&path, br#"{"jobId":1,"timestamp":"2026-01"#).expect("torn record");

        write_records(&path, &[sample_entry(2)]);

        assert_eq!(job_ids(&path), vec![2], "only the torn record is lost");
        let body = std::fs::read_to_string(&path).expect("readable");
        assert_eq!(body.lines().count(), 2);
        // An intact log gets no extra blank line.
        write_records(&path, &[sample_entry(3)]);
        assert_eq!(
            std::fs::read_to_string(&path)
                .expect("readable")
                .lines()
                .count(),
            3
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Past the cap the live log becomes a dated generation and a fresh one starts.
    /// No record is dropped, and a generation already holding that second's name is
    /// never overwritten.
    #[test]
    fn a_full_log_rotates_to_a_dated_generation_without_losing_a_record() {
        use chrono::TimeZone as _;
        let (dir, path) = temp_log("rotate");
        let at = Utc
            .with_ymd_and_hms(2026, 10, 30, 15, 5, 12)
            .single()
            .expect("valid instant");
        let cap = 64; // any one record is longer

        write_records_rotating_at(&path, &[sample_entry(1)], cap, at);
        write_records_rotating_at(&path, &[sample_entry(2)], cap, at);

        let first = generation_path(&path, at, 0);
        assert!(first.ends_with("action-audit-20261030T150512.jsonl"));
        assert_eq!(job_ids(&first), vec![1]);
        assert_eq!(job_ids(&path), vec![2]);

        // Same second again: the next free name, the first generation untouched.
        write_records_rotating_at(&path, &[sample_entry(3)], cap, at);
        assert_eq!(job_ids(&first), vec![1]);
        assert_eq!(job_ids(&generation_path(&path, at, 1)), vec![2]);
        assert_eq!(job_ids(&path), vec![3]);

        // Below the cap nothing moves.
        write_records_rotating_at(&path, &[sample_entry(4)], u64::MAX, at);
        assert_eq!(job_ids(&path), vec![3, 4]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reader walks generations newest first, so their order must follow the
    /// stamp and the same-second counter, and nothing else in the folder counts.
    #[test]
    fn rotated_generations_are_listed_newest_first() {
        let (dir, path) = temp_log("generations");
        for name in [
            "action-audit-20260101T000000.jsonl",
            "action-audit-20261030T150512.jsonl",
            "action-audit-20261030T150512-1.jsonl",
            "action-audit-20260615T120000.jsonl",
            // Not generations.
            "action-audit.jsonl",
            "action-audit-notes.jsonl",
            "action-audit-20261030T150512.txt",
            "run-history.jsonl",
        ] {
            std::fs::write(dir.join(name), b"").expect("file");
        }
        let names: Vec<String> = rotated_generations(&path)
            .iter()
            .filter_map(|p| p.file_name()?.to_str().map(str::to_string))
            .collect();
        assert_eq!(
            names,
            [
                "action-audit-20261030T150512-1.jsonl",
                "action-audit-20261030T150512.jsonl",
                "action-audit-20260615T120000.jsonl",
                "action-audit-20260101T000000.jsonl",
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
