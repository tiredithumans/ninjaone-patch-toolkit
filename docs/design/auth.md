# Auth: PKCE, keyring, scope, refresh

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/auth.rs`, `src-tauri/src/state.rs` (`AppState`),
`src-tauri/src/commands/auth.rs`.

## Secrets discipline — keyring only, never `settings.json`, never logs

The refresh token and optional client secret live in the OS keyring (Keychain / Credential
Manager / Secret Service). The access token is in-memory only. `settings.json` holds
non-sensitive config (instance URL, client id, ports, windows, presets). Never write a
token/secret to disk or a `tracing` event.

## PKCE, lazy token, Native-or-Web client

`AuthState::access_token()` refreshes lazily before each call. Sign-in is the interactive S256
PKCE flow with a **loopback** redirect on the configured `callback_port` (default `11434`); a hung
sign-in usually means the callback never arrived. **Native** (public) clients have **no** secret;
**Web** (confidential) clients do — the app supports both, so don't hardcode either.

## Scope is conditional, and the refresh grant never re-sends it

`scope_for(actions_enabled)` picks `monitoring offline_access` or
`monitoring management offline_access`; `settings.actions.enabled` (default **false**) is what
flips it, which is why adding the write path didn't break existing installs. The refresh grant
does **not** send `scope`, so an install that signed in before actions were enabled keeps its
read-only grant silently and every write 403s. `AuthState::management_grant()` detects this from
the token response's `scope` (RFC 6749 §5.1, self-healing on each refresh) with a JWT-claim
fallback; `None` means *unknowable*, not *denied*, and the UI words the two differently.
`commands::auth::reauthorize` drops the keyring refresh token **first** so the browser flow must
issue a fresh grant.

## In-memory before keyring, and only the token that got the 401 is invalidated

`store_tokens` assigns `inner.tokens` **first** and downgrades a keyring write failure to a
warning. The server has already rotated the grant by then, so propagating the error would discard
a valid token set and the next attempt would replay the consumed refresh token into
`invalid_grant`, which clears the credential — a transient locked keychain became a forced
interactive sign-in. Degrading to "no persistence this session" is correct: the access token is
in-memory only anyway.

Relatedly, `invalidate_access_token(&stale)` takes the token that actually got the 401 and no-ops
unless it is still the current one; a query fans out many concurrent requests, so a lagging 401
answering a *replaced* token would otherwise mark the fresh one stale and chain into redundant
grants.

## The callback listener loops over connections

`wait_for_callback` accepts repeatedly and answers anything without `code`/`state`/`error` with a
404, with a per-socket read timeout. Handling exactly one accept meant a browser preconnect,
favicon fetch or port probe consumed the sign-in — the documented "a hung sign-in usually means
the callback never arrived" symptom.

## The refresh is single-flight, and only `invalid_grant` clears the credential

A query deliberately fans out many concurrent API calls and each one calls `access_token()`
first, so without a guard they all observe the same stale token and each POSTs the same
`refresh_token` — last-writer-wins on both the keyring and the in-memory set. `access_token()`
therefore takes `refresh_lock` (a `tokio::Mutex`) and re-checks under it, so concurrent callers
await one grant.

That composes with the error arm: `refresh_grant_is_dead` clears the stored refresh token **only**
on a 400/401 whose OAuth `error` is `invalid_grant`. Clearing on any non-2xx meant a 429, a 5xx or
a captive-portal page forced an interactive re-login — and under refresh-token rotation the loser
of a refresh race erased the credential the winner had just stored. Deliberately **not** "any
4xx": 429 is a retry-later status.
