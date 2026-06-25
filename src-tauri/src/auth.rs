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

/// Read-only scope for a patching-operations toolkit. `offline_access` is required
/// to receive a refresh token so the operator does not re-authenticate hourly.
const SCOPE: &str = "monitoring offline_access";
const KEYRING_SERVICE: &str = "NinjaOnePatchToolkit";
const KEYRING_USER_SECRET: &str = "client_secret";
const KEYRING_USER_REFRESH: &str = "refresh_token";

#[derive(Debug, Clone)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
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
}

/// Shared auth state used by the API client and the Tauri commands.
#[derive(Clone)]
pub struct AuthState {
    inner: Arc<RwLock<Inner>>,
    http: reqwest::Client,
}

struct Inner {
    base_url: String,
    callback_port: u16,
    client_id: Option<String>,
    /// Optional: a NinjaOne *Native* app registration is a public client with no
    /// secret (pure PKCE). A *Web* app registration is confidential and supplies one.
    client_secret: Option<String>,
    tokens: Option<TokenSet>,
}

impl AuthState {
    pub fn new(
        http: reqwest::Client,
        base_url: String,
        callback_port: u16,
        client_id: Option<String>,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                base_url,
                callback_port,
                client_id,
                client_secret: load_keyring(KEYRING_USER_SECRET).ok(),
                tokens: None,
            })),
            http,
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
    /// port). Persisting these to `settings.json` is the caller's responsibility.
    pub fn apply_settings(&self, base_url: String, client_id: Option<String>, callback_port: u16) {
        if let Ok(mut inner) = self.inner.write() {
            inner.base_url = base_url;
            inner.client_id = client_id;
            inner.callback_port = callback_port;
        }
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
    pub async fn access_token(&self) -> Result<String> {
        let snapshot = self
            .inner
            .read()
            .map_err(|_| anyhow!("auth state poisoned"))?
            .tokens
            .clone();
        if let Some(tokens) = snapshot {
            if !tokens.is_stale() {
                return Ok(tokens.access_token);
            }
            if let Some(refresh) = tokens.refresh_token {
                return self.refresh(&refresh).await;
            }
        }

        if let Ok(refresh) = load_keyring(KEYRING_USER_REFRESH) {
            return self.refresh(&refresh).await;
        }

        bail!("not authenticated");
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
            let text = truncate_body(&resp.text().await.unwrap_or_default());
            // Clear invalid refresh token so the next attempt forces interactive login.
            let _ = delete_keyring(KEYRING_USER_REFRESH);
            self.clear_tokens_locked();
            bail!("refresh failed ({status}): {text}");
        }

        let parsed: TokenResponse = resp.json().await.context("refresh token body")?;
        let token_set = self.store_tokens(parsed)?;
        Ok(token_set.access_token)
    }

    fn store_tokens(&self, parsed: TokenResponse) -> Result<TokenSet> {
        let expires_at = Utc::now() + Duration::seconds(parsed.expires_in);
        let token_set = TokenSet {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token.clone(),
            expires_at,
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
        let (client_id, base_url, port) = {
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
            )
        };
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
                 app with the Authorization Code grant and the Monitoring scope."
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
            }),
        };
        Self {
            inner: Arc::new(RwLock::new(inner)),
            http,
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
            }),
        };
        Self {
            inner: Arc::new(RwLock::new(inner)),
            http,
        }
    }
}

fn build_auth_url(
    base_url: &str,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> String {
    let q = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPE),
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
    for pair in query.split('&') {
        let Some(eq) = pair.find('=') else { continue };
        let (k, v) = pair.split_at(eq);
        let v = &v[1..];
        let decoded = urlencoding::decode(v).unwrap_or_default().into_owned();
        match k {
            "code" => code = Some(decoded),
            "state" => state = Some(decoded),
            "error" => error = Some(decoded),
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
        };
        assert!(!fresh.is_stale());

        let expiring = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Utc::now() + Duration::seconds(60),
        };
        assert!(expiring.is_stale());
    }

    #[test]
    fn auth_url_includes_pkce_and_scope() {
        let url = build_auth_url(
            "https://us2.ninjarmm.com",
            "client123",
            "http://127.0.0.1:11434",
            "challengeABC",
            "stateXYZ",
        );
        assert!(url.starts_with("https://us2.ninjarmm.com/ws/oauth/authorize?"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("code_challenge=challengeABC"));
        assert!(url.contains("response_type=code"));
        // monitoring + offline_access, URL-encoded space.
        assert!(url.contains("scope=monitoring%20offline_access"));
    }
}
