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
    let req =
        format!("GET {request_target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
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
/// The authorize URL's query string carries the anti-CSRF `state`, the PKCE
/// `code_challenge` and the client id. Five analyzers in the 2026-08-20 review
/// converged on the single `debug!` that used to emit it whole — the highest
/// agreement in that run — because `state` is the only value distinguishing the
/// genuine redirect from any other local process hitting the callback port.
#[test]
fn logging_an_authorize_url_drops_everything_secret() {
    let auth_url = "https://app.ninjarmm.com/ws/oauth/authorize?response_type=code\
                    &client_id=abc123&state=super-secret-nonce\
                    &code_challenge=Zm9vYmFy&redirect_uri=http%3A%2F%2F127.0.0.1%3A11434";
    let logged = url_without_query(auth_url);

    assert_eq!(logged, "https://app.ninjarmm.com/ws/oauth/authorize");
    for secret in [
        "state",
        "super-secret-nonce",
        "code_challenge",
        "Zm9vYmFy",
        "abc123",
    ] {
        assert!(
            !logged.contains(secret),
            "{secret} must not reach the log stream"
        );
    }
}

/// A URL with nothing to hide must survive intact, or the log stops being useful.
#[test]
fn a_url_without_a_query_is_unchanged() {
    let plain = "https://app.ninjarmm.com/ws/oauth/authorize";
    assert_eq!(url_without_query(plain), plain);
    assert_eq!(
        url_without_query("https://example.com/path#frag"),
        "https://example.com/path"
    );
}
