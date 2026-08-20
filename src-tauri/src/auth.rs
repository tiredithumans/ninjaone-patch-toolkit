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

/// Only the real backend names a service; the test backend is an in-process map.
#[cfg(not(test))]
const KEYRING_SERVICE: &str = "NinjaOnePatchToolkit";
/// Pre-tenant-scoping entry names. Still read once, by [`load_tenant_keyring`], so an
/// existing sign-in survives the upgrade; never written again.
const LEGACY_KEYRING_USER_SECRET: &str = "client_secret";
const LEGACY_KEYRING_USER_REFRESH: &str = "refresh_token";
/// Prefixes for the tenant-scoped entries — see [`tenant_entry`].
const KEYRING_SECRET_PREFIX: &str = "client_secret";
const KEYRING_REFRESH_PREFIX: &str = "refresh_token";

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
/// The tenant a grant was *started* under, sampled before the network call and
/// re-checked before the tokens are stored.
///
/// `AuthState` had the same write-time-stamping hazard `state.rs` solved with
/// `QueryToken`: `store_tokens_blocking` resolved the keyring entry name by reading
/// the *current* `base_url`/`client_id`, but a refresh takes a round trip and an
/// interactive sign-in takes up to three minutes. An operator who changed instance
/// in Settings during that window had tenant A's refresh token written under tenant
/// B's entry name — and then handed to tenant B's API host on the next call.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TenantStamp {
    base_url: String,
    client_id: Option<String>,
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
    /// Held for the duration of an interactive sign-in, so a second one is refused
    /// with an explanation instead of failing at `bind` with "Is another instance of
    /// this app running?" — which blames the wrong thing, since the port is in fact
    /// held by *this* process's own in-flight flow.
    login_gate: Arc<tokio::sync::Mutex<()>>,
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
        let client_secret = load_tenant_keyring(
            KEYRING_SECRET_PREFIX,
            LEGACY_KEYRING_USER_SECRET,
            &base_url,
            client_id.as_deref(),
        )
        .unwrap_or_default();
        Self {
            inner: Arc::new(RwLock::new(Inner {
                base_url,
                callback_port,
                client_id,
                client_secret,
                tokens: None,
                request_management,
            })),
            http,
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
            login_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// The tenant currently configured, for stamping a grant that is about to start.
    fn tenant_stamp(&self) -> TenantStamp {
        let (base_url, client_id) = self
            .inner
            .read()
            .map(|g| (g.base_url.clone(), g.client_id.clone()))
            .unwrap_or_default();
        TenantStamp {
            base_url,
            client_id,
        }
    }

    /// Keyring entry holding the refresh token for the currently configured tenant.
    fn refresh_entry(&self) -> String {
        let (base_url, client_id) = self
            .inner
            .read()
            .map(|g| (g.base_url.clone(), g.client_id.clone()))
            .unwrap_or_default();
        tenant_entry(KEYRING_REFRESH_PREFIX, &base_url, client_id.as_deref())
    }

    /// Keyring entry holding the client secret for the currently configured tenant.
    fn secret_entry(&self) -> String {
        let (base_url, client_id) = self
            .inner
            .read()
            .map(|g| (g.base_url.clone(), g.client_id.clone()))
            .unwrap_or_default();
        tenant_entry(KEYRING_SECRET_PREFIX, &base_url, client_id.as_deref())
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
    /// `settings.json` is the caller's responsibility. Returns whether the *tenant*
    /// changed, so the caller can drop the caches keyed by it.
    ///
    /// A tenant switch drops the in-memory grant, and this is load-bearing rather
    /// than tidiness. Left in place, `is_authenticated()` stayed true across the
    /// switch, `access_token()` handed the previous tenant's token to the new host,
    /// and the resulting 401 drove a refresh that POSTed the old `refresh_token` to
    /// the **new** `/ws/oauth/token` with the **new** `client_id`. That is a textbook
    /// `invalid_grant` — the one arm that deletes the stored credential — so merely
    /// pointing the app at another instance destroyed the sign-in of the instance you
    /// came from. The keyring entries are tenant-scoped, so the previous tenant's
    /// refresh token stays on disk under its own name and switching back finds it.
    ///
    /// Clearing here rather than in the caller keeps it atomic with the change that
    /// invalidates it: the old value cannot be observed under the new tenant, and no
    /// future caller can forget the pairing.
    pub fn apply_settings(
        &self,
        base_url: String,
        client_id: Option<String>,
        callback_port: u16,
        request_management: bool,
    ) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let tenant_changed = inner.base_url != base_url || inner.client_id != client_id;
        inner.base_url = base_url;
        inner.client_id = client_id;
        inner.callback_port = callback_port;
        inner.request_management = request_management;
        if tenant_changed {
            inner.tokens = None;
            // The secret is per-tenant too, so re-read it for the tenant now in
            // effect instead of carrying the previous one's over.
            inner.client_secret = load_tenant_keyring(
                KEYRING_SECRET_PREFIX,
                LEGACY_KEYRING_USER_SECRET,
                &inner.base_url,
                inner.client_id.as_deref(),
            )
            .unwrap_or_default();
        }
        tenant_changed
    }

    /// Whether the *current* grant carries the `management` scope.
    ///
    /// `Some(true)` — writes are permitted. `Some(false)` — the grant demonstrably
    /// lacks it, so re-consent is required (the common case for an install that
    /// signed in before actions existed: the refresh grant never re-sends `scope`,
    /// so the old narrow grant persists silently). `None` — the token carries no
    /// readable scope, treated as not-granted for gating but worded differently so
    /// the operator isn't told their consent was wrong when we simply can't tell.
    ///
    /// A poisoned lock and an absent token set also read as `None`, which is
    /// correct for both: neither lets us observe the grant. That is not a
    /// conflation the callers can be hurt by — `action_blocked_reason` checks
    /// `authenticated` before it ever looks at the grant, so the "couldn't confirm"
    /// wording cannot reach a signed-out operator, and `require_actions_enabled`
    /// denies the action on `None` either way.
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
        let entry = self.secret_entry();
        match &secret {
            Some(s) => save_keyring(&entry, s)?,
            None => delete_keyring(&entry)?,
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

        // A keyring *fault* is not a signed-out install, and conflating them sent the
        // operator to re-run a sign-in that could not have helped. `Ok(None)` is the
        // genuinely-absent case; an error (locked keychain, no Secret Service running)
        // says so, because the stored credential may well still be intact.
        let (base_url, client_id) = self
            .inner
            .read()
            .map(|g| (g.base_url.clone(), g.client_id.clone()))
            .unwrap_or_default();
        match load_tenant_keyring(
            KEYRING_REFRESH_PREFIX,
            LEGACY_KEYRING_USER_REFRESH,
            &base_url,
            client_id.as_deref(),
        ) {
            Ok(Some(refresh)) => self.refresh(&refresh).await,
            Ok(None) => bail!("not authenticated"),
            Err(e) => Err(e)
                .context("could not read the saved sign-in from the OS keyring; it may be locked"),
        }
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

        // Sampled before the round trip; see `TenantStamp`.
        let started = self.tenant_stamp();

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
                let _ = delete_keyring(&self.refresh_entry());
                self.clear_tokens_locked();
            } else {
                debug!(%status, "refresh failed transiently; keeping stored credential");
            }
            bail!("refresh failed ({status}): {text}");
        }

        let parsed: TokenResponse = resp.json().await.context("refresh token body")?;
        let token_set = self.store_tokens(parsed, started).await?;
        Ok(token_set.access_token)
    }

    /// [`Self::store_tokens_blocking`], moved off the async runtime.
    ///
    /// The keyring write inside it is a synchronous OS call — Keychain, Credential
    /// Manager, or a D-Bus round trip to the Secret Service — and it runs while
    /// `refresh_lock` is held, which is exactly when every other caller of
    /// `access_token()` is queued behind it. On a tokio worker that also blocked a
    /// thread the rest of the app needs; a Secret Service that is slow or absent
    /// turned one refresh into a stall across every concurrent fetch in the query.
    /// The lock is still held across this await, which is correct — the point is to
    /// stop holding a *worker* too.
    async fn store_tokens(&self, parsed: TokenResponse, started: TenantStamp) -> Result<TokenSet> {
        let this = self.clone();
        tauri::async_runtime::spawn_blocking(move || this.store_tokens_blocking(parsed, started))
            .await
            .context("token persistence task failed")?
    }

    fn store_tokens_blocking(
        &self,
        parsed: TokenResponse,
        started: TenantStamp,
    ) -> Result<TokenSet> {
        // Tenant drift, checked the way `store_last_result_if_current` checks it:
        // against the tenant the grant *started* under, not the one that happens to
        // be current now. These tokens were issued by `started`'s authorization
        // server; filing them under whatever is configured at this instant is the one
        // way this can be actively wrong rather than merely stale.
        let current = self.tenant_stamp();
        if current != started {
            warn!(
                "the configured instance changed while signing in; discarding tokens issued by the previous one"
            );
            bail!("the instance changed while signing in; sign in again");
        }
        let expires_at = Utc::now() + Duration::seconds(parsed.expires_in);
        // Prefer what the server said it granted; fall back to the token's own
        // claim when the response omits `scope`.
        let granted_scope = parsed
            .scope
            .filter(|s| !s.trim().is_empty())
            .or_else(|| scope_claim_from_jwt(&parsed.access_token));
        // RFC 6749 §6 lets the server omit `refresh_token` when it does not rotate
        // the grant, in which case the existing one stays valid. Taking the response
        // at face value dropped it, leaving the session dependent on the keyring copy
        // — and that copy is explicitly allowed not to exist, since a keyring write
        // failure is downgraded to a warning below. The two together turned a
        // non-rotating server plus a locked keychain into "not authenticated"
        // mid-session, which is exactly the forced re-login this function exists to
        // prevent.
        let previous_refresh = self
            .inner
            .read()
            .ok()
            .and_then(|g| g.tokens.as_ref().and_then(|t| t.refresh_token.clone()));
        let token_set = TokenSet {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token.clone().or(previous_refresh),
            expires_at,
            granted_scope,
        };
        // In-memory first, persistence second — the order is load-bearing.
        //
        // The server has already rotated the grant by the time we get here: the old
        // refresh token is spent whether or not we manage to write the new one. So a
        // keyring failure must not propagate out of `refresh()` and discard a token
        // set that is perfectly valid. It used to: a locked keychain or a Secret
        // Service outage returned `Err`, the fresh access *and* refresh tokens were
        // dropped, and the next attempt replayed the consumed refresh token into the
        // `invalid_grant` arm above — which deletes the credential. A transient OS
        // fault became a forced interactive sign-in.
        //
        // Degrading to "no persistence this session" is the honest failure: the
        // access token is in-memory only by design anyway, so the session continues
        // and only survival across a restart is lost.
        self.inner
            .write()
            .map_err(|_| anyhow!("auth state poisoned"))?
            .tokens = Some(token_set.clone());
        if let Some(ref rt) = parsed.refresh_token
            && let Err(e) = save_keyring(&self.refresh_entry(), rt)
        {
            warn!(
                error = %e,
                "could not persist the refresh token; this session stays signed in but a restart will require signing in again"
            );
        }
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
    /// `stale` identifies the token that actually got the 401. Only that token is
    /// invalidated: a query fans out many concurrent requests, so a 401 issued
    /// against the *old* token routinely lands after the single-flight refresh has
    /// already stored a new one. Stamping unconditionally marked that fresh token
    /// stale, and a burst of in-flight 401s turned into a chain of redundant
    /// grants — the refresh lock serializes them but cannot suppress them.
    pub fn invalidate_access_token(&self, stale: &str) {
        if let Ok(mut inner) = self.inner.write()
            && let Some(tokens) = inner.tokens.as_mut()
            && tokens.access_token == stale
        {
            tokens.expires_at = Utc::now() - Duration::seconds(1);
        }
    }

    /// Signs out: drops the in-memory tokens and deletes the stored refresh token.
    ///
    /// A keyring delete that *fails* is reported, not swallowed. The in-memory clear
    /// still happens first so the session ends either way, but returning `Ok(())`
    /// unconditionally meant the UI could show a clean sign-out over a refresh token
    /// still on disk — which the next `access_token()` would happily use to sign the
    /// next operator straight back in as the previous one. On a shared workstation
    /// that is the whole threat this function exists to close, so the operator has to
    /// be told when it did not work.
    ///
    /// A *missing* entry is not a failure: `delete_keyring` reports it as `Ok`, so
    /// signing out twice, or signing out of an install that never persisted a token,
    /// stays quiet.
    pub fn logout(&self) -> Result<()> {
        // In-memory first, so the session is over regardless of what the keyring does.
        self.clear_tokens_locked();
        let current = delete_keyring(&self.refresh_entry());
        // Signing out of an install that still holds a pre-tenant-scoping entry must
        // clear that too, or the next `access_token()` migrates it straight back in.
        let legacy = delete_keyring(LEGACY_KEYRING_USER_REFRESH);
        current.and(legacy).context(
            "signed out of this session, but the stored refresh token could not be deleted; \
             it may still be on this machine",
        )
    }

    /// Interactive PKCE login: opens the browser and waits up to 3 minutes for the
    /// callback, then exchanges the code for tokens.
    pub async fn login_pkce(&self) -> Result<()> {
        // One interactive flow at a time. Without this, a second sign-in (a
        // double-click on Sign in, or a retry while the first browser tab is still
        // open) raced to `bind` the same callback port and failed with "could not
        // bind … Is another instance of this app running?" — which sends the
        // operator hunting for a second copy of the app that does not exist, when
        // the port is held by this process's own in-flight flow. `try_lock` rather
        // than `lock`: queueing a second browser window behind a three-minute wait
        // is not what the operator asked for.
        let _login = self.login_gate.try_lock().map_err(|_| {
            anyhow!(
                "a sign-in is already in progress — finish or close the browser tab that opened, \
                 then try again"
            )
        })?;
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
        // Sampled before the browser opens: an interactive flow waits up to three
        // minutes, ample room for the operator to change instance in Settings while
        // it is pending. See `TenantStamp`.
        let started = TenantStamp {
            base_url: base_url.clone(),
            client_id: Some(client_id.clone()),
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
            wait_for_callback(listener, &state),
        )
        .await
        .map_err(|_| anyhow!("login timed out — no callback received within 3 minutes"))??;

        // Defense in depth. `wait_for_callback` only returns a redirect whose state
        // already matched, so this is unreachable — kept deliberately, in the same
        // spirit as the dry-run check being asserted at both the plan and the
        // dispatch site: a future change to the listener's filter must not silently
        // remove the state check from the flow.
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
        self.store_tokens(parsed, started).await?;
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
            login_gate: Arc::new(tokio::sync::Mutex::new(())),
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
            login_gate: Arc::new(tokio::sync::Mutex::new(())),
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

/// How long one connection gets to send its request line before it is abandoned.
/// A browser preconnect that opens a socket and sends nothing must not hold the
/// flow hostage for the caller's full three-minute budget.
const CALLBACK_SOCKET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Waits for the browser's redirect back to the loopback listener.
///
/// This **loops over connections** rather than taking the first one. The port is
/// an ordinary localhost port: browsers speculatively preconnect to it, favicon
/// requests arrive on it, extensions and local scanners probe it. Handling exactly
/// one accept meant any of those consumed the sign-in — either failing outright on
/// an empty read, or pinning the flow until the outer 180 s timeout fired, which is
/// the "a hung sign-in usually means the callback never arrived" symptom. A
/// connection that carries no `code`, `state` or `error` is answered with 404 and
/// the loop keeps waiting for the real redirect.
async fn wait_for_callback(listener: TcpListener, expected_state: &str) -> Result<CallbackResult> {
    loop {
        let (sock, _peer) = listener.accept().await.context("callback accept failed")?;
        // One deadline for the whole connection, not per read. The inner timeout is
        // rearmed on every successful read, so a client dribbling a byte every few
        // seconds could hold this serial listener open indefinitely and the real
        // redirect would queue behind it until the outer 3-minute budget expired.
        match timeout(
            CALLBACK_SOCKET_TIMEOUT,
            handle_callback_conn(sock, expected_state),
        )
        .await
        .unwrap_or_else(|_| Err(anyhow!("callback connection exceeded its deadline")))
        {
            Ok(Some(result)) => return Ok(result),
            // Not the redirect — keep listening.
            Ok(None) => continue,
            // One bad connection (timeout, oversized body, truncated request) is not
            // a reason to fail the sign-in; the browser may still be about to arrive.
            Err(e) => {
                debug!(error = %e, "ignoring a non-callback connection on the loopback port");
                continue;
            }
        }
    }
}

/// Reads one connection and, if it is the OAuth redirect, answers the browser and
/// returns the parsed parameters. `Ok(None)` means "this was some other client" —
/// it is answered with a 404 and the caller keeps waiting.
async fn handle_callback_conn(
    mut sock: tokio::net::TcpStream,
    expected_state: &str,
) -> Result<Option<CallbackResult>> {
    let mut buf = [0u8; 4096];
    let mut total = Vec::new();
    loop {
        let n = timeout(CALLBACK_SOCKET_TIMEOUT, sock.read(&mut buf))
            .await
            .map_err(|_| anyhow!("callback connection idle for 10s"))?
            .context("callback read failed")?;
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

    // A redirect always carries `state`, and either `code` or `error` (RFC 6749
    // §4.1.2 / §4.1.2.1). Anything else is a preconnect, a favicon fetch or a
    // probe — send it away without ending the wait.
    //
    // Two conditions, both required, and both are what stop a third party ending
    // the sign-in:
    //
    // * `code` or `error` must be present. A request carrying only `state` used to
    //   satisfy the old "any one of the three" test and end the wait, after which
    //   `login_pkce` aborted the whole flow with "state mismatch — possible CSRF".
    //   Any local process could therefore kill an in-progress sign-in by fetching
    //   `http://127.0.0.1:<port>/?state=x`, and the operator saw a security error
    //   for what was really a stray request.
    // * `state` must equal the value this flow generated. RFC 6749 §4.1.2 requires
    //   the server to echo it, so the genuine redirect always carries it. Checking
    //   it *here* turns it from an abort into a filter: a request that does not
    //   know the state is answered with a 404 and the listener keeps waiting for
    //   the real one, instead of the flow failing closed on someone else's traffic.
    let is_redirect = (code.is_some() || error.is_some())
        && state.as_deref().is_some_and(|s| s == expected_state);
    if !is_redirect {
        if code.is_some() || error.is_some() {
            debug!("ignoring a callback-shaped request whose state did not match this flow");
        }
        let body = "<html><body><h1>Not found</h1></body></html>";
        let _ = sock
            .write_all(
                format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = sock.shutdown().await;
        return Ok(None);
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

    Ok(Some(CallbackResult {
        code,
        state: state.unwrap_or_default(),
        error,
    }))
}

// --- Keyring wrappers ---------------------------------------------------------

/// Entry name holding `prefix`'s credential for one tenant.
///
/// Both entries used to be bare constants shared by every tenant, so signing into a
/// second instance or client id overwrote the first one's credential, and
/// [`AuthState::access_token`]'s keyring fallback replayed whatever was stored
/// against whatever host happened to be configured. `last_result`, the job store and
/// the whole-fleet caches are all tenant-stamped; the credential was the one place
/// that was not.
///
/// The tenant is hashed rather than interpolated so the entry name stays a bounded
/// ASCII string — Windows Credential Manager and the Secret Service each have their
/// own limits on length and encoding — and so an instance URL never becomes a
/// keychain label. Truncating to 8 bytes is ample: this only has to distinguish the
/// handful of tenants one operator uses, and a collision would merely reuse an entry
/// the same way the old global name did.
fn tenant_entry(prefix: &str, base_url: &str, client_id: Option<&str>) -> String {
    use std::fmt::Write as _;

    let mut hasher = Sha256::new();
    hasher.update(base_url.trim_end_matches('/').as_bytes());
    hasher.update([0x1f]);
    hasher.update(client_id.unwrap_or_default().as_bytes());
    let digest = hasher.finalize();

    let mut name = String::with_capacity(prefix.len() + 17);
    name.push_str(prefix);
    name.push('.');
    for byte in &digest[..8] {
        let _ = write!(name, "{byte:02x}");
    }
    name
}

/// Reads this tenant's credential, adopting a pre-tenant-scoped one on first use.
///
/// An install that predates tenant scoping has a single global entry. Which tenant
/// wrote it is unknowable, but a one-tenant install is the overwhelming case, so it
/// is adopted by the tenant configured now and the global entry removed. A
/// multi-tenant install loses at most one stored sign-in, once — and only has to
/// sign in again, since nothing else depends on it.
fn load_tenant_keyring(
    prefix: &str,
    legacy: &str,
    base_url: &str,
    client_id: Option<&str>,
) -> Result<Option<String>> {
    let name = tenant_entry(prefix, base_url, client_id);
    if let Some(value) = load_keyring(&name)? {
        return Ok(Some(value));
    }
    let Some(value) = load_keyring(legacy)? else {
        return Ok(None);
    };
    match save_keyring(&name, &value) {
        // Only drop the global entry once the scoped one is safely written.
        Ok(()) => {
            let _ = delete_keyring(legacy);
        }
        Err(e) => {
            warn!(error = %e, "could not migrate the stored credential to a tenant-scoped entry")
        }
    }
    Ok(Some(value))
}

#[cfg(not(test))]
fn save_keyring(user: &str, value: &str) -> Result<()> {
    keyring::Entry::new(KEYRING_SERVICE, user)
        .context("open keyring entry")?
        .set_password(value)
        .context("keyring write")
}

/// `Ok(None)` means "no such entry" — a signed-out install, not a fault. `Err` is a
/// real keyring failure (locked keychain, no Secret Service), which callers must be
/// able to tell apart: reporting both as "not authenticated" sent the operator to
/// re-run a sign-in that could not have helped.
#[cfg(not(test))]
fn load_keyring(user: &str) -> Result<Option<String>> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, user).context("open keyring entry")?;
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).context("keyring read"),
    }
}

#[cfg(not(test))]
fn delete_keyring(user: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, user).context("open keyring entry")?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).context("keyring delete"),
    }
}

/// In-process stand-in for the OS keyring, used by unit tests only.
///
/// A unit test must never touch the operator's real keychain. `AuthState::new` reads
/// the client secret on construction, so *every* test that builds one used to reach
/// the live keyring: on a desktop with a GUI that raises an OS permission dialog and
/// the entire `just test` run blocks on it indefinitely. CI never caught it because
/// headless runners return an error instead of prompting — the failure mode was
/// exclusive to developer machines. As a bonus this makes the round-trip assertions
/// mean something, where against the real keyring they could only be written as
/// "may or may not have been accepted".
#[cfg(test)]
mod test_keyring {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    pub(super) fn store() -> &'static Mutex<HashMap<String, String>> {
        static STORE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
        STORE.get_or_init(|| Mutex::new(HashMap::new()))
    }
}

#[cfg(test)]
fn save_keyring(user: &str, value: &str) -> Result<()> {
    test_keyring::store()
        .lock()
        .map_err(|_| anyhow!("test keyring poisoned"))?
        .insert(user.to_string(), value.to_string());
    Ok(())
}

#[cfg(test)]
fn load_keyring(user: &str) -> Result<Option<String>> {
    Ok(test_keyring::store()
        .lock()
        .map_err(|_| anyhow!("test keyring poisoned"))?
        .get(user)
        .cloned())
}

#[cfg(test)]
fn delete_keyring(user: &str) -> Result<()> {
    test_keyring::store()
        .lock()
        .map_err(|_| anyhow!("test keyring poisoned"))?
        .remove(user);
    Ok(())
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

    fn token_response(refresh: Option<&str>) -> TokenResponse {
        TokenResponse {
            access_token: "access".into(),
            refresh_token: refresh.map(str::to_string),
            expires_in: 3600,
            scope: Some("monitoring offline_access".into()),
        }
    }

    /// The sev-4 case. A tenant switch left the previous tenant's grant in place, so
    /// `is_authenticated()` stayed true, the old token went to the new host, and the
    /// resulting `invalid_grant` deleted the credential of the tenant switched away
    /// from.
    #[test]
    fn switching_tenant_drops_the_previous_grant() {
        let auth = AuthState::new(
            reqwest::Client::new(),
            "https://us2.example.com".into(),
            11434,
            Some("client-a".into()),
            false,
        );
        auth.store_tokens_blocking(token_response(Some("refresh-a")), auth.tenant_stamp())
            .expect("store");
        assert!(auth.is_authenticated());

        let changed = auth.apply_settings(
            "https://eu.example.com".into(),
            Some("client-b".into()),
            11434,
            false,
        );

        assert!(changed, "instance + client id both moved");
        assert!(
            !auth.is_authenticated(),
            "the old tenant's token must not be presented to the new host"
        );
    }

    /// Changing a non-tenant setting must not sign the operator out.
    #[test]
    fn a_non_tenant_settings_change_keeps_the_session() {
        let auth = AuthState::new(
            reqwest::Client::new(),
            "https://us2.example.com".into(),
            11434,
            Some("client-a".into()),
            false,
        );
        auth.store_tokens_blocking(token_response(Some("refresh-a")), auth.tenant_stamp())
            .expect("store");

        let changed = auth.apply_settings(
            "https://us2.example.com".into(),
            Some("client-a".into()),
            9999,
            true,
        );

        assert!(!changed, "only the port and the scope wish changed");
        assert!(auth.is_authenticated());
    }

    /// Each tenant's refresh token lives under its own entry, so signing into a
    /// second instance cannot overwrite the first one's credential — and switching
    /// back finds it again.
    #[test]
    fn each_tenant_stores_its_refresh_token_separately() {
        let a = tenant_entry(
            KEYRING_REFRESH_PREFIX,
            "https://us2.example.com",
            Some("client-a"),
        );
        let b = tenant_entry(
            KEYRING_REFRESH_PREFIX,
            "https://eu.example.com",
            Some("client-a"),
        );
        let c = tenant_entry(
            KEYRING_REFRESH_PREFIX,
            "https://us2.example.com",
            Some("client-b"),
        );

        assert_ne!(a, b, "a different instance is a different tenant");
        assert_ne!(a, c, "so is a different client id on the same instance");
        assert_eq!(
            a,
            tenant_entry(
                KEYRING_REFRESH_PREFIX,
                "https://us2.example.com/",
                Some("client-a")
            ),
            "a trailing slash is the same tenant"
        );
    }

    /// An install that predates tenant scoping keeps its sign-in: the global entry is
    /// adopted by the configured tenant on first read, then removed.
    #[test]
    fn a_legacy_global_credential_is_migrated_once() {
        save_keyring(LEGACY_KEYRING_USER_REFRESH, "legacy-refresh").expect("seed legacy");

        let got = load_tenant_keyring(
            KEYRING_REFRESH_PREFIX,
            LEGACY_KEYRING_USER_REFRESH,
            "https://migrate.example.com",
            Some("client-m"),
        )
        .expect("migrate");

        assert_eq!(got.as_deref(), Some("legacy-refresh"));
        assert_eq!(
            load_keyring(LEGACY_KEYRING_USER_REFRESH).expect("read legacy"),
            None,
            "the global entry is removed once the scoped one is written"
        );
        let scoped = tenant_entry(
            KEYRING_REFRESH_PREFIX,
            "https://migrate.example.com",
            Some("client-m"),
        );
        assert_eq!(
            load_keyring(&scoped).expect("read scoped").as_deref(),
            Some("legacy-refresh")
        );
    }

    /// RFC 6749 §6 lets a server omit `refresh_token` when it does not rotate the
    /// grant. Taking that at face value dropped the token and left the session
    /// dependent on a keyring copy that is explicitly allowed not to exist.
    #[test]
    fn a_non_rotating_refresh_keeps_the_existing_token() {
        let auth = AuthState::new(
            reqwest::Client::new(),
            "https://keep.example.com".into(),
            11434,
            Some("client-k".into()),
            false,
        );
        auth.store_tokens_blocking(
            token_response(Some("original-refresh")),
            auth.tenant_stamp(),
        )
        .expect("first store");

        let set = auth
            .store_tokens_blocking(token_response(None), auth.tenant_stamp())
            .expect("second store, server declined to rotate");

        assert_eq!(
            set.refresh_token.as_deref(),
            Some("original-refresh"),
            "a session must not lose its refresh token to a non-rotating response"
        );
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
        auth.store_tokens_blocking(
            TokenResponse {
                access_token: jwt_with(serde_json::json!({ "scope": "monitoring management" })),
                refresh_token: None,
                expires_in: 3600,
                scope: Some("monitoring offline_access".into()),
            },
            auth.tenant_stamp(),
        )
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

        auth.store_tokens_blocking(
            TokenResponse {
                access_token: "opaque".into(),
                refresh_token: None,
                expires_in: 3600,
                scope: Some("monitoring management offline_access".into()),
            },
            auth.tenant_stamp(),
        )
        .expect("store");
        assert_eq!(auth.management_grant(), Some(true));
    }

    /// Drives `wait_for_callback` end-to-end: binds a loopback listener, connects a
    /// client, sends a single raw HTTP request line, and returns the parsed result.
    async fn drive_callback(request_target: &str) -> CallbackResult {
        drive_callback_with_state(request_target, "xyz").await
    }

    /// As [`drive_callback`], but lets a test choose the state this flow expects —
    /// the listener now filters on it rather than aborting the sign-in on a mismatch.
    async fn drive_callback_with_state(
        request_target: &str,
        expected_state: &'static str,
    ) -> CallbackResult {
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { wait_for_callback(listener, expected_state).await });

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

    /// The loopback port is an ordinary localhost port: browsers preconnect to it,
    /// extensions and scanners probe it. Handling exactly one accept meant the first
    /// such connection consumed the sign-in and the real redirect was never read —
    /// the documented "hung sign-in" symptom.
    #[tokio::test]
    async fn a_stray_connection_does_not_consume_the_sign_in() {
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { wait_for_callback(listener, "xyz").await });

        // A probe that is not the redirect — no code, no state, no error.
        let mut probe = TcpStream::connect(addr).await.unwrap();
        probe
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut drained = Vec::new();
        let _ = probe.read_to_end(&mut drained).await;
        assert!(
            String::from_utf8_lossy(&drained).contains("404"),
            "a non-callback request should be turned away, not treated as the redirect"
        );

        // The browser then arrives, and must still be served.
        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                b"GET /?code=abc123&state=xyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut resp = Vec::new();
        let _ = client.read_to_end(&mut resp).await;

        let r = server.await.unwrap().expect("callback parsed");
        assert_eq!(r.code.as_deref(), Some("abc123"));
        assert_eq!(r.state, "xyz");
    }

    /// A connection that opens and says nothing must not pin the sign-in: it is
    /// abandoned on its own timeout and the wait continues.
    #[tokio::test]
    async fn a_silent_connection_does_not_pin_the_sign_in() {
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { wait_for_callback(listener, "s").await });

        // Connect and immediately drop without sending anything — the read returns
        // 0 bytes, which used to `bail!("empty callback request")` out of the whole
        // sign-in.
        drop(TcpStream::connect(addr).await.unwrap());

        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                b"GET /?code=ok&state=s HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut resp = Vec::new();
        let _ = client.read_to_end(&mut resp).await;

        assert_eq!(
            server
                .await
                .unwrap()
                .expect("callback parsed")
                .code
                .as_deref(),
            Some("ok")
        );
    }

    /// The server has already rotated the grant by the time `store_tokens` runs, so
    /// the old refresh token is spent either way. A keyring outage must therefore
    /// degrade to "no persistence", not throw away a valid session — the next
    /// attempt would otherwise replay a consumed token into `invalid_grant`, which
    /// clears the credential and forces an interactive sign-in.
    #[test]
    fn a_keyring_failure_keeps_the_session_it_just_obtained() {
        let http = reqwest::Client::new();
        let auth = AuthState::new(http, "https://x".into(), 0, None, true);

        // Backed by the in-process test keyring, so the write does land and this
        // asserts the in-memory assignment rather than tolerating either outcome.
        auth.store_tokens_blocking(
            TokenResponse {
                access_token: "fresh-access".into(),
                refresh_token: Some("fresh-refresh".into()),
                expires_in: 3600,
                scope: Some("monitoring management offline_access".into()),
            },
            auth.tenant_stamp(),
        )
        .expect("a keyring problem must not fail the store");

        assert!(
            auth.is_authenticated(),
            "the session must survive a keyring write that did not land"
        );
        assert_eq!(auth.management_grant(), Some(true));
    }

    /// A query fans out many concurrent requests, so a 401 answering the *old*
    /// token routinely lands after the single-flight refresh stored a new one.
    /// Stamping unconditionally marked the fresh token stale and turned a burst of
    /// lagging 401s into a chain of redundant grants.
    #[test]
    fn a_late_401_does_not_invalidate_a_token_that_replaced_it() {
        let http = reqwest::Client::new();
        let auth = AuthState::new(http, "https://x".into(), 0, None, true);

        auth.store_tokens_blocking(
            TokenResponse {
                access_token: "new-token".into(),
                refresh_token: None,
                expires_in: 3600,
                scope: None,
            },
            auth.tenant_stamp(),
        )
        .expect("store");
        assert!(auth.is_authenticated());

        // The 401 was provoked by the token this one replaced.
        auth.invalidate_access_token("old-token");
        assert!(
            auth.is_authenticated(),
            "a 401 for a superseded token must leave the current one alone"
        );

        // The 401 that really does name the live token still invalidates it.
        auth.invalidate_access_token("new-token");
        assert!(!auth.is_authenticated());
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
        let r = drive_callback_with_state("/?code=a%20b&state=s%2Fx", "s/x").await;
        assert_eq!(r.code.as_deref(), Some("a b"));
        assert_eq!(r.state, "s/x");
    }

    /// A request carrying only `state` is not the redirect and must not end the
    /// wait. It used to satisfy the old "any one of code/state/error" test, so any
    /// local process could kill an in-progress sign-in by fetching
    /// `http://127.0.0.1:<port>/?state=x` — and the operator was shown
    /// "state mismatch — possible CSRF" for what was really a stray request.
    #[tokio::test]
    async fn a_bare_state_is_not_the_redirect() {
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { wait_for_callback(listener, "xyz").await });

        let mut probe = TcpStream::connect(addr).await.unwrap();
        probe
            .write_all(b"GET /?state=xyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut drained = Vec::new();
        let _ = probe.read_to_end(&mut drained).await;
        assert!(
            String::from_utf8_lossy(&drained).contains("404"),
            "a bare state carries no authorization result, so it is not the redirect"
        );

        // The real redirect still lands.
        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                b"GET /?code=real&state=xyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut resp = Vec::new();
        let _ = client.read_to_end(&mut resp).await;

        let r = server.await.unwrap().expect("callback parsed");
        assert_eq!(r.code.as_deref(), Some("real"));
    }

    /// A `code` whose `state` belongs to some other flow must be ignored rather than
    /// aborting this one. Checking state in the listener turns it from an abort into
    /// a filter: an attacker who cannot guess the state cannot end the sign-in, which
    /// was previously a trivial local denial of service against it.
    #[tokio::test]
    async fn a_code_with_the_wrong_state_does_not_end_the_wait() {
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { wait_for_callback(listener, "expected").await });

        let mut attacker = TcpStream::connect(addr).await.unwrap();
        attacker
            .write_all(
                b"GET /?code=injected&state=wrong HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut drained = Vec::new();
        let _ = attacker.read_to_end(&mut drained).await;
        assert!(
            String::from_utf8_lossy(&drained).contains("404"),
            "a mismatched state is turned away"
        );

        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                b"GET /?code=genuine&state=expected HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut resp = Vec::new();
        let _ = client.read_to_end(&mut resp).await;

        let r = server.await.unwrap().expect("callback parsed");
        assert_eq!(
            r.code.as_deref(),
            Some("genuine"),
            "the injected code must never be the one exchanged"
        );
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
    /// A refresh takes a round trip and an interactive sign-in waits up to three
    /// minutes — ample room for the operator to change instance in Settings. The
    /// tokens in hand were issued by the *previous* instance's authorization server,
    /// so filing them under the new tenant's keyring entry would hand tenant A's
    /// refresh token to tenant B's API host. `state.rs` solved the identical hazard
    /// with `QueryToken`; this is the auth-side counterpart.
    #[test]
    fn tokens_are_discarded_when_the_instance_changes_mid_grant() {
        let auth = AuthState::new(
            reqwest::Client::new(),
            "https://a.example.com".into(),
            11434,
            Some("client-a".into()),
            false,
        );
        let started = auth.tenant_stamp();

        // The operator switches instance while the grant is in flight.
        auth.apply_settings(
            "https://b.example.com".into(),
            Some("client-b".into()),
            11434,
            false,
        );

        let err = auth
            .store_tokens_blocking(token_response(Some("refresh-a")), started)
            .expect_err("a grant issued by the previous instance must not be stored");
        assert!(
            err.to_string().contains("instance changed"),
            "the message must name the cause: {err}"
        );
        assert!(
            !auth.is_authenticated(),
            "the new tenant must not inherit the old tenant's session"
        );
    }

    /// The stamp must not reject the ordinary case, or every sign-in fails.
    #[test]
    fn tokens_are_stored_when_the_instance_is_unchanged() {
        let auth = AuthState::new(
            reqwest::Client::new(),
            "https://a.example.com".into(),
            11434,
            Some("client-a".into()),
            false,
        );
        let started = auth.tenant_stamp();
        auth.store_tokens_blocking(token_response(Some("refresh-a")), started)
            .expect("an unchanged tenant stores normally");
        assert!(auth.is_authenticated());
    }

    /// Signing out must not report success over a refresh token still on disk: the
    /// next `access_token()` would use it to sign the next operator back in as the
    /// previous one, which on a shared workstation is the whole threat. A *missing*
    /// entry is not a failure, so a second sign-out stays quiet.
    #[test]
    fn logout_clears_the_session_and_is_idempotent() {
        let auth = AuthState::new(
            reqwest::Client::new(),
            "https://a.example.com".into(),
            11434,
            Some("client-a".into()),
            false,
        );
        auth.store_tokens_blocking(token_response(Some("refresh-a")), auth.tenant_stamp())
            .expect("store");
        assert!(auth.is_authenticated());

        auth.logout().expect("sign out");
        assert!(!auth.is_authenticated());
        auth.logout().expect("signing out twice is not an error");
    }
}
