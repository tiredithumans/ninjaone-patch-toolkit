use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use tracing::{debug, warn};

use crate::error::truncate_body;

/// Read-only scope, and the default. `offline_access` is required to receive a
/// refresh token so the operator does not re-authenticate hourly.
const SCOPE_READ_ONLY: &str = "monitoring offline_access";
/// `management` is what the device *action* endpoints require (patch scan/apply,
/// reboot, script run). Requested only once the operator enables patch actions in
/// Settings, so a reporting-only install never widens its grant.
const SCOPE_WITH_ACTIONS: &str = "monitoring management offline_access";
/// The scope token that distinguishes a write-capable grant.
const MANAGEMENT_SCOPE: &str = "management";

const KEYRING_SERVICE: &str = "NinjaOnePatchToolkit";
const KEYRING_USER_SECRET: &str = "client_secret";
const KEYRING_USER_REFRESH: &str = "refresh_token";

/// The scope to request at sign-in. Split out so the choice is testable without
/// standing up an authorize flow.
fn scope_for(actions_enabled: bool) -> &'static str {
    if actions_enabled {
        SCOPE_WITH_ACTIONS
    } else {
        SCOPE_READ_ONLY
    }
}

/// Whether a granted-scope string carries `management`. Matched on whitespace-
/// separated tokens so a scope like `management_readonly` can't false-positive.
fn scope_grants_management(scope: &str) -> bool {
    scope
        .split_whitespace()
        .any(|s| s.eq_ignore_ascii_case(MANAGEMENT_SCOPE))
}

/// Best-effort read of the `scope`/`scp` claim from a JWT access token.
///
/// The refresh grant does **not** echo `scope` on every tenant, so when the token
/// response omits it this recovers the granted scope from the token itself. The
/// signature is deliberately not verified — this is a UI affordance for deciding
/// whether to prompt for re-consent, never an authorization decision (the server
/// is the authority, and a missing scope surfaces as a 403 regardless). An opaque
/// (non-JWT) token simply yields `None`.
fn scope_claim_from_jwt(access_token: &str) -> Option<String> {
    let payload = access_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    for key in ["scope", "scp"] {
        match claims.get(key) {
            Some(serde_json::Value::String(s)) if !s.is_empty() => return Some(s.clone()),
            // Some issuers emit scopes as an array.
            Some(serde_json::Value::Array(items)) => {
                let joined = items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !joined.is_empty() {
                    return Some(joined);
                }
            }
            _ => {}
        }
    }
    None
}
#[derive(Debug, Clone)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    /// Scope the authorization server actually granted, from the token response's
    /// `scope` field (RFC 6749 §5.1) or, failing that, the access token's own
    /// claim. `None` means "unknowable from this token" — which is not the same as
    /// "read-only", and the UI words the two differently.
    pub granted_scope: Option<String>,
}

impl TokenSet {
    /// True when the access token is expired or within a 5 min skew.
    pub fn is_stale(&self) -> bool {
        Utc::now() + Duration::seconds(300) >= self.expires_at
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
    /// RFC 6749 §5.1. Present on most refresh responses, which is what lets a
    /// stale read-only grant heal itself into a correct `write_enabled` readout
    /// without an interactive round trip.
    #[serde(default)]
    scope: Option<String>,
}

/// Whether a failed refresh response means the *grant itself* is dead, i.e. the
/// stored refresh token can never work again and clearing it is correct.
///
/// Only `invalid_grant` (RFC 6749 §5.2) says that. A 429, a 5xx, a proxy's HTML
/// error page, or a connect failure all mean "ask again later" — clearing on
/// those turned a network blip into a forced interactive sign-in, and combined
/// with a concurrent refresh it let the loser of a race erase the credential the
/// winner had just stored. Anything unparseable is treated as *not* dead: a
/// spurious re-login is a much worse failure than one extra doomed retry.
fn refresh_grant_is_dead(status: reqwest::StatusCode, body: &str) -> bool {
    // RFC 6749 §5.2 returns 400 for `invalid_grant` (401 for the client-auth
    // variants). Deliberately NOT "any 4xx": 429 is a retry-later status, and a
    // rate limit must never cost the operator their credential.
    if !matches!(
        status,
        reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNAUTHORIZED
    ) {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(|v| v.as_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("invalid_grant"))
}

/// Shared auth state used by the API client and the Tauri commands.
#[derive(Clone)]
pub struct AuthState {
    inner: Arc<RwLock<Inner>>,
    http: reqwest::Client,
    /// Serializes the refresh grant. See `access_token`.
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
}

struct Inner {
    base_url: String,
    callback_port: u16,
    client_id: Option<String>,
    /// Optional: a NinjaOne *Native* app registration is a public client with no
    /// secret (pure PKCE). A *Web* app registration is confidential and supplies one.
    client_secret: Option<String>,
    tokens: Option<TokenSet>,
    /// Whether the next interactive sign-in should ask for `management`. Mirrors
    /// `settings.actions.enabled`; it does not describe the *current* grant.
    request_management: bool,
}

impl AuthState {
    pub fn new(
        http: reqwest::Client,
        base_url: String,
        callback_port: u16,
        client_id: Option<String>,
        request_management: bool,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                base_url,
                callback_port,
                client_id,
                client_secret: load_keyring(KEYRING_USER_SECRET).ok(),
                tokens: None,
                request_management,
            })),
            http,
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn base_url(&self) -> String {
        self.inner
            .read()
            .map(|g| g.base_url.clone())
            .unwrap_or_default()
    }

    pub fn client_id(&self) -> Option<String> {
        self.inner.read().ok()?.client_id.clone()
    }

    pub fn has_client_secret(&self) -> bool {
        self.inner
            .read()
            .map(|g| g.client_secret.is_some())
            .unwrap_or(false)
    }

    fn client_secret(&self) -> Option<String> {
        self.inner.read().ok()?.client_secret.clone()
    }

    /// Applies non-secret connection settings (instance URL, client ID, callback
    /// port, and whether to request `management`). Persisting these to
    /// `settings.json` is the caller's responsibility.
    pub fn apply_settings(
        &self,
        base_url: String,
        client_id: Option<String>,
        callback_port: u16,
        request_management: bool,
    ) {
        if let Ok(mut inner) = self.inner.write() {
            inner.base_url = base_url;
            inner.client_id = client_id;
            inner.callback_port = callback_port;
            inner.request_management = request_management;
        }
    }

    /// Whether the *current* grant carries the `management` scope.
    ///
    /// `Some(true)` — writes are permitted. `Some(false)` — the grant demonstrably
    /// lacks it, so re-consent is required (the common case for an install that
    /// signed in before actions existed: the refresh grant never re-sends `scope`,
    /// so the old narrow grant persists silently). `None` — the token carries no
    /// readable scope, treated as not-granted for gating but worded differently so
    /// the operator isn't told their consent was wrong when we simply can't tell.
    pub fn management_grant(&self) -> Option<bool> {
        let scope = self
            .inner
            .read()
            .ok()?
            .tokens
            .as_ref()?
            .granted_scope
            .clone()?;
        Some(scope_grants_management(&scope))
    }

    pub fn set_client_secret(&self, secret: Option<String>) -> Result<()> {
        match &secret {
            Some(s) => save_keyring(KEYRING_USER_SECRET, s)?,
            None => delete_keyring(KEYRING_USER_SECRET)?,
        }
        self.inner
            .write()
            .map_err(|_| anyhow!("auth state poisoned"))?
            .client_secret = secret;
        Ok(())
    }

    pub fn is_authenticated(&self) -> bool {
        self.inner
            .read()
            .map(|g| g.tokens.as_ref().is_some_and(|t| !t.is_stale()))
            .unwrap_or(false)
    }

    /// Returns a valid access token, refreshing if needed. Does NOT start an
    /// interactive login — the UI layer decides when to prompt.
    ///
    /// The refresh is single-flight. A query fans out many concurrent API calls
    /// (the whole-fleet device and current-patch fetches are deliberately
    /// parallel), and each one calls this before its request. Without the lock
    /// they all observe the same stale token and each POSTs the same
    /// `refresh_token`: last-writer-wins on both the keyring and the in-memory
    /// set, and under refresh-token rotation every loser presents an
    /// already-consumed token and gets `invalid_grant` back — which used to
    /// delete the credential the winner had just stored.
    pub async fn access_token(&self) -> Result<String> {
        if let Some(token) = self.fresh_access_token()? {
            return Ok(token);
        }

        let _guard = self.refresh_lock.lock().await;

        // Re-check under the lock: whoever held it before us may have already
        // refreshed, in which case this call is a cache hit rather than a
        // second grant.
        if let Some(token) = self.fresh_access_token()? {
            return Ok(token);
        }

        let stored_refresh = self
            .inner
            .read()
            .map_err(|_| anyhow!("auth state poisoned"))?
            .tokens
            .as_ref()
            .and_then(|t| t.refresh_token.clone());
        if let Some(refresh) = stored_refresh {
            return self.refresh(&refresh).await;
        }

        if let Ok(refresh) = load_keyring(KEYRING_USER_REFRESH) {
            return self.refresh(&refresh).await;
        }

        bail!("not authenticated");
    }

    /// The cached access token, if one is present and not within the staleness
    /// skew. `None` means a refresh is needed.
    fn fresh_access_token(&self) -> Result<Option<String>> {
        Ok(self
            .inner
            .read()
            .map_err(|_| anyhow!("auth state poisoned"))?
            .tokens
            .as_ref()
            .filter(|t| !t.is_stale())
            .map(|t| t.access_token.clone()))
    }

    async fn refresh(&self, refresh_token: &str) -> Result<String> {
        let client_id = self
            .client_id()
            .ok_or_else(|| anyhow!("no client ID configured"))?;
        let base_url = self.base_url();

        let mut body = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token.to_string()),
            ("client_id", client_id),
        ];
        if let Some(secret) = self.client_secret() {
            body.push(("client_secret", secret));
        }

        let resp = self
            .http
            .post(format!("{base_url}/ws/oauth/token"))
            .form(&body)
            .send()
            .await
            .context("refresh token request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let raw = resp.text().await.unwrap_or_default();
            let text = truncate_body(&raw);
            // Only drop the stored credential when the server says the grant is
            // dead. A transient failure (429, 5xx, proxy error page) leaves it in
            // place so the next attempt can succeed without an interactive login.
            if refresh_grant_is_dead(status, &raw) {
                let _ = delete_keyring(KEYRING_USER_REFRESH);
                self.clear_tokens_locked();
            } else {
                debug!(%status, "refresh failed transiently; keeping stored credential");
            }
            bail!("refresh failed ({status}): {text}");
        }

        let parsed: TokenResponse = resp.json().await.context("refresh token body")?;
        let token_set = self.store_tokens(parsed)?;
        Ok(token_set.access_token)
    }

    fn store_tokens(&self, parsed: TokenResponse) -> Result<TokenSet> {
        let expires_at = Utc::now() + Duration::seconds(parsed.expires_in);
        // Prefer what the server said it granted; fall back to the token's own
        // claim when the response omits `scope`.
        let granted_scope = parsed
            .scope
            .filter(|s| !s.trim().is_empty())
            .or_else(|| scope_claim_from_jwt(&parsed.access_token));
        let token_set = TokenSet {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token.clone(),
            expires_at,
            granted_scope,
        };
        if let Some(ref rt) = parsed.refresh_token {
            save_keyring(KEYRING_USER_REFRESH, rt)?;
        }
        self.inner
            .write()
            .map_err(|_| anyhow!("auth state poisoned"))?
            .tokens = Some(token_set.clone());
        Ok(token_set)
    }

    fn clear_tokens_locked(&self) {
        if let Ok(mut inner) = self.inner.write() {
            inner.tokens = None;
        }
    }

    /// Marks the cached access token stale so the next `access_token()` call
    /// refreshes it. The API client calls this when a request returns 401 with an
    /// otherwise-unexpired token (revoked/invalidated server-side): staleness is
    /// purely time-based, so without this the same dead token would be resent on
    /// every retry until the budget is exhausted.
    pub fn invalidate_access_token(&self) {
        if let Ok(mut inner) = self.inner.write()
            && let Some(tokens) = inner.tokens.as_mut()
        {
            tokens.expires_at = Utc::now() - Duration::seconds(1);
        }
    }

    pub fn logout(&self) -> Result<()> {
        let _ = delete_keyring(KEYRING_USER_REFRESH);
        self.clear_tokens_locked();
        Ok(())
    }

    /// Interactive PKCE login: opens the browser and waits up to 3 minutes for the
    /// callback, then exchanges the code for tokens.
    pub async fn login_pkce(&self) -> Result<()> {
        let (client_id, base_url, port, request_management) = {
            let inner = self
                .inner
                .read()
                .map_err(|_| anyhow!("auth state poisoned"))?;
            (
                inner
                    .client_id
                    .clone()
                    .ok_or_else(|| anyhow!("client ID not configured"))?,
                inner.base_url.clone(),
                inner.callback_port,
                inner.request_management,
            )
        };
        let scope = scope_for(request_management);
        let client_secret = self.client_secret();

        let pkce = PkceChallenge::new();
        let state = random_url_token(32);
        // NinjaOne Native API clients register the loopback redirect as
        // `http://127.0.0.1` (host only) and accept any port per RFC 8252, so the
        // redirect_uri MUST use `127.0.0.1` (not `localhost`, which NinjaOne treats
        // as a different host) with no trailing path. The callback listener binds
        // 127.0.0.1 below, so the browser reaches it either way.
        let redirect_uri = format!("http://127.0.0.1:{port}");

        let auth_url = build_auth_url(
            &base_url,
            &client_id,
            &redirect_uri,
            &pkce.challenge,
            &state,
            scope,
        );

        // Pre-flight: NinjaOne's /ws/oauth/authorize returns 404 when it doesn't
        // recognize the client_id at this host (confirmed across every region). A
        // recognized client — even with no browser session — instead redirects to
        // the login page, so only a 404 is fatal. Catch it here with an actionable
        // message rather than opening the browser to a bare 404 and then waiting
        // out the 3-minute callback timeout. Best-effort: a probe error (offline,
        // proxy, …) falls through to the normal flow.
        let probe = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .build();
        if let Ok(probe) = probe
            && let Ok(resp) = probe.get(&auth_url).send().await
            && resp.status() == reqwest::StatusCode::NOT_FOUND
        {
            bail!(
                "NinjaOne did not recognize this Client ID at {base_url} (HTTP 404). \
                 Check that Region/Instance matches the host you sign in to NinjaOne at, \
                 that the Client ID is copied correctly, and that the API app is a Native \
                 app with the Authorization Code grant and the Monitoring scope \
                 (plus Management, if you have enabled patch actions)."
            );
        }

        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .with_context(|| {
                format!(
                    "could not bind OAuth callback listener on 127.0.0.1:{port}. \
                     Is another instance of this app running?"
                )
            })?;

        debug!(%auth_url, "opening browser for PKCE login");
        if let Err(err) = open::that(&auth_url) {
            warn!(?err, "failed to open browser; user must navigate manually");
        }

        let callback = timeout(
            std::time::Duration::from_secs(180),
            wait_for_callback(listener),
        )
        .await
        .map_err(|_| anyhow!("login timed out — no callback received within 3 minutes"))??;

        if callback.state != state {
            bail!("state mismatch — possible CSRF");
        }
        if let Some(err) = callback.error {
            // The one authorization error with a specific, actionable fix: the app
            // registration doesn't carry a scope we asked for.
            if err.contains("invalid_scope") && request_management {
                bail!(
                    "NinjaOne rejected the requested scope ({scope}). Enable the \
                     Management scope on this API app in NinjaOne → Administration → \
                     Apps → API, then sign in again. To keep the app read-only instead, \
                     turn off Patch actions in Settings."
                );
            }
            bail!("authorization error: {err}");
        }
        let code = callback
            .code
            .ok_or_else(|| anyhow!("no authorization code in callback"))?;

        let mut body = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", pkce.verifier),
        ];
        if let Some(secret) = client_secret {
            body.push(("client_secret", secret));
        }

        let resp = self
            .http
            .post(format!("{base_url}/ws/oauth/token"))
            .form(&body)
            .send()
            .await
            .context("token exchange request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = truncate_body(&resp.text().await.unwrap_or_default());
            bail!("token exchange failed ({status}): {text}");
        }

        let parsed: TokenResponse = resp.json().await.context("token exchange body")?;
        self.store_tokens(parsed)?;
        Ok(())
    }
}

#[cfg(test)]
impl AuthState {
    /// Builds an already-authenticated client with a fixed access token, for tests
    /// that exercise the API client against a mock server without a real login.
    pub(crate) fn seeded(http: reqwest::Client, base_url: String, access_token: &str) -> Self {
        let inner = Inner {
            base_url,
            callback_port: 0,
            client_id: None,
            client_secret: None,
            tokens: Some(TokenSet {
                access_token: access_token.to_string(),
                refresh_token: None,
                expires_at: Utc::now() + Duration::seconds(3600),
                // Tests that drive the action endpoints need a write-capable grant.
                granted_scope: Some(SCOPE_WITH_ACTIONS.to_string()),
            }),
            request_management: false,
        };
        Self {
            inner: Arc::new(RwLock::new(inner)),
            http,
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Like `seeded`, but with a refresh token + client id so a 401 can drive a
    /// real `refresh()` round-trip against a mock token endpoint.
    pub(crate) fn seeded_refreshable(
        http: reqwest::Client,
        base_url: String,
        access_token: &str,
        refresh_token: &str,
        client_id: &str,
    ) -> Self {
        let inner = Inner {
            base_url,
            callback_port: 0,
            client_id: Some(client_id.to_string()),
            client_secret: None,
            tokens: Some(TokenSet {
                access_token: access_token.to_string(),
                refresh_token: Some(refresh_token.to_string()),
                expires_at: Utc::now() + Duration::seconds(3600),
                granted_scope: Some(SCOPE_WITH_ACTIONS.to_string()),
            }),
            request_management: false,
        };
        Self {
            inner: Arc::new(RwLock::new(inner)),
            http,
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

fn build_auth_url(
    base_url: &str,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    scope: &str,
) -> String {
    let q = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", scope),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ]
    .iter()
    .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
    .collect::<Vec<_>>()
    .join("&");
    format!("{base_url}/ws/oauth/authorize?{q}")
}

struct PkceChallenge {
    verifier: String,
    challenge: String,
}

impl PkceChallenge {
    fn new() -> Self {
        let verifier = random_url_token(64);
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }
}

fn random_url_token(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

struct CallbackResult {
    code: Option<String>,
    state: String,
    error: Option<String>,
}

async fn wait_for_callback(listener: TcpListener) -> Result<CallbackResult> {
    let (mut sock, _peer) = listener.accept().await.context("callback accept failed")?;

    let mut buf = [0u8; 4096];
    let mut total = Vec::new();
    loop {
        let n = sock.read(&mut buf).await.context("callback read failed")?;
        if n == 0 {
            break;
        }
        total.extend_from_slice(&buf[..n]);
        if total.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if total.len() > 16 * 1024 {
            bail!("callback request exceeded 16 KB");
        }
    }

    let request = String::from_utf8_lossy(&total);
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| anyhow!("empty callback request"))?;
    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("malformed callback request"))?;

    let query_start = path.find('?').map(|i| i + 1).unwrap_or(path.len());
    let query = &path[query_start..];

    let mut code = None;
    let mut state = None;
    let mut error = None;
    // Spec-conformant application/x-www-form-urlencoded parsing (RFC 6749 §4.1.2
    // redirect encoding): keys are percent-decoded too, and `+` means space.
    // Last-wins on duplicate keys.
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }

    let (status, body) = if error.is_some() {
        (
            400,
            "<html><body><h1>Authentication failed</h1><p>You can close this tab and return to the app.</p></body></html>",
        )
    } else if code.is_some() {
        (
            200,
            "<html><body style=\"background:#0f1117;color:#e2e4e9;font-family:sans-serif;text-align:center;padding:80px\"><h1>Login successful</h1><p>You can close this tab.</p></body></html>",
        )
    } else {
        (400, "<html><body><h1>Missing code</h1></body></html>")
    };

    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(response.as_bytes()).await;
    let _ = sock.shutdown().await;

    Ok(CallbackResult {
        code,
        state: state.unwrap_or_default(),
        error,
    })
}

// --- Keyring wrappers ---------------------------------------------------------

fn save_keyring(user: &str, value: &str) -> Result<()> {
    keyring::Entry::new(KEYRING_SERVICE, user)
        .context("open keyring entry")?
        .set_password(value)
        .context("keyring write")
}

fn load_keyring(user: &str) -> Result<String> {
    keyring::Entry::new(KEYRING_SERVICE, user)
        .context("open keyring entry")?
        .get_password()
        .context("keyring read")
}

fn delete_keyring(user: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, user).context("open keyring entry")?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).context("keyring delete"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dead grant is the *only* thing that may clear the stored refresh token.
    /// Everything else has to leave it alone: clearing on a transient failure is
    /// what turned a network blip into a forced interactive sign-in.
    #[test]
    fn only_invalid_grant_clears_the_stored_credential() {
        use reqwest::StatusCode;

        assert!(refresh_grant_is_dead(
            StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant","error_description":"expired"}"#
        ));
        // Case-insensitive per RFC 6749's ABNF being a bare string.
        assert!(refresh_grant_is_dead(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"INVALID_GRANT"}"#
        ));

        // Transient / not-about-this-token: keep the credential.
        assert!(!refresh_grant_is_dead(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":"invalid_grant"}"#,
        ));
        assert!(!refresh_grant_is_dead(
            StatusCode::BAD_GATEWAY,
            "<html>upstream timeout</html>"
        ));
        assert!(!refresh_grant_is_dead(
            StatusCode::INTERNAL_SERVER_ERROR,
            ""
        ));
        // A captive portal answering 400 with HTML must not look like a dead grant.
        assert!(!refresh_grant_is_dead(
            StatusCode::BAD_REQUEST,
            "<html>sign in to the wifi</html>"
        ));
        // A different OAuth error code is not our token's problem.
        assert!(!refresh_grant_is_dead(
            StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_client"}"#
        ));
    }

    /// RFC 7636 Appendix B test vector.
    #[test]
    fn pkce_matches_rfc7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn token_set_staleness() {
        let fresh = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Utc::now() + Duration::seconds(3600),
            granted_scope: None,
        };
        assert!(!fresh.is_stale());

        let expiring = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Utc::now() + Duration::seconds(60),
            granted_scope: None,
        };
        assert!(expiring.is_stale());
    }

    fn auth_url_with(scope: &str) -> String {
        build_auth_url(
            "https://us2.ninjarmm.com",
            "client123",
            "http://127.0.0.1:11434",
            "challengeABC",
            "stateXYZ",
            scope,
        )
    }

    #[test]
    fn auth_url_requests_read_only_scope_by_default() {
        let url = auth_url_with(scope_for(false));
        assert!(url.starts_with("https://us2.ninjarmm.com/ws/oauth/authorize?"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("code_challenge=challengeABC"));
        assert!(url.contains("response_type=code"));
        // monitoring + offline_access, URL-encoded space.
        assert!(url.contains("scope=monitoring%20offline_access"));
        assert!(
            !url.contains("management"),
            "an install with actions off must never ask for write access"
        );
    }

    #[test]
    fn auth_url_requests_management_when_actions_enabled() {
        let url = auth_url_with(scope_for(true));
        assert!(url.contains("scope=monitoring%20management%20offline_access"));
    }

    #[test]
    fn management_is_matched_as_a_whole_scope_token() {
        assert!(scope_grants_management(
            "monitoring management offline_access"
        ));
        assert!(scope_grants_management("MANAGEMENT"));
        assert!(!scope_grants_management("monitoring offline_access"));
        // A different scope that merely contains the word must not count.
        assert!(!scope_grants_management("management_readonly"));
        assert!(!scope_grants_management(""));
    }

    /// Builds an unsigned JWT-shaped token whose payload carries `claims`. The
    /// signature is irrelevant — `scope_claim_from_jwt` never verifies it.
    fn jwt_with(claims: serde_json::Value) -> String {
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("header.{payload}.signature")
    }

    #[test]
    fn granted_scope_falls_back_to_the_jwt_claim() {
        let token = jwt_with(serde_json::json!({ "scope": "monitoring management" }));
        assert_eq!(
            scope_claim_from_jwt(&token).as_deref(),
            Some("monitoring management")
        );

        // `scp`, and the array spelling, are both in the wild.
        let scp = jwt_with(serde_json::json!({ "scp": ["monitoring", "management"] }));
        assert_eq!(
            scope_claim_from_jwt(&scp).as_deref(),
            Some("monitoring management")
        );
    }

    #[test]
    fn opaque_token_yields_unknown_not_false() {
        // An opaque token carries no readable scope. The distinction matters: the
        // UI must not tell the operator their consent was wrong when it simply
        // cannot tell.
        assert_eq!(scope_claim_from_jwt("opaque-token-value"), None);
        assert_eq!(scope_claim_from_jwt(""), None);
        // Well-formed shape, but no scope claim at all.
        assert_eq!(scope_claim_from_jwt(&jwt_with(serde_json::json!({}))), None);
    }

    #[test]
    fn granted_scope_prefers_the_token_response_over_the_claim() {
        let http = reqwest::Client::new();
        let auth = AuthState::new(http, "https://x".into(), 0, None, false);
        // Response says read-only; the token claims management. RFC 6749 §5.1 makes
        // the response authoritative for what was actually granted.
        auth.store_tokens(TokenResponse {
            access_token: jwt_with(serde_json::json!({ "scope": "monitoring management" })),
            refresh_token: None,
            expires_in: 3600,
            scope: Some("monitoring offline_access".into()),
        })
        .expect("store");
        assert_eq!(auth.management_grant(), Some(false));
    }

    #[test]
    fn granted_scope_from_token_response_marks_write_enabled() {
        let http = reqwest::Client::new();
        let auth = AuthState::new(http, "https://x".into(), 0, None, true);
        assert_eq!(
            auth.management_grant(),
            None,
            "no token yet means the grant is unknown, not denied"
        );

        auth.store_tokens(TokenResponse {
            access_token: "opaque".into(),
            refresh_token: None,
            expires_in: 3600,
            scope: Some("monitoring management offline_access".into()),
        })
        .expect("store");
        assert_eq!(auth.management_grant(), Some(true));
    }

    /// Drives `wait_for_callback` end-to-end: binds a loopback listener, connects a
    /// client, sends a single raw HTTP request line, and returns the parsed result.
    async fn drive_callback(request_target: &str) -> CallbackResult {
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(wait_for_callback(listener));

        let mut client = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET {request_target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        );
        client.write_all(req.as_bytes()).await.unwrap();
        // Drain the browser-facing response so the server task finishes cleanly.
        let mut resp = Vec::new();
        let _ = client.read_to_end(&mut resp).await;

        server.await.unwrap().expect("callback parsed")
    }

    #[tokio::test]
    async fn callback_extracts_code_and_state() {
        let r = drive_callback("/?code=abc123&state=xyz").await;
        assert_eq!(r.code.as_deref(), Some("abc123"));
        assert_eq!(r.state, "xyz");
        assert!(r.error.is_none());
    }

    #[tokio::test]
    async fn callback_surfaces_provider_error() {
        let r = drive_callback("/?error=access_denied&state=xyz").await;
        assert_eq!(r.error.as_deref(), Some("access_denied"));
        assert!(r.code.is_none());
    }

    #[tokio::test]
    async fn callback_url_decodes_percent_encoded_values() {
        let r = drive_callback("/?code=a%20b&state=s%2Fx").await;
        assert_eq!(r.code.as_deref(), Some("a b"));
        assert_eq!(r.state, "s/x");
    }

    #[tokio::test]
    async fn callback_without_code_or_error_yields_neither() {
        // The "missing code" 400 path — the caller treats this as a failed sign-in.
        let r = drive_callback("/?state=xyz").await;
        assert!(r.code.is_none());
        assert!(r.error.is_none());
        assert_eq!(r.state, "xyz");
    }

    #[tokio::test]
    async fn callback_decodes_percent_encoded_keys() {
        // A percent-encoded key must still match — form_urlencoded decodes both sides.
        let r = drive_callback("/?%63ode=abc&state=xyz").await;
        assert_eq!(r.code.as_deref(), Some("abc"));
    }

    #[tokio::test]
    async fn callback_decodes_plus_as_space() {
        // x-www-form-urlencoded semantics: `+` is a space (providers send `%2B`
        // for a literal plus, so real flows are unaffected).
        let r = drive_callback("/?code=a+b&state=xyz").await;
        assert_eq!(r.code.as_deref(), Some("a b"));
    }

    #[tokio::test]
    async fn callback_duplicate_keys_last_wins() {
        let r = drive_callback("/?code=first&code=second&state=xyz").await;
        assert_eq!(r.code.as_deref(), Some("second"));
    }
}
