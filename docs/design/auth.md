# Auth: PKCE, keyring, scope, refresh

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/auth.rs`, `src-tauri/src/state.rs` (`AppState`),
`src-tauri/src/commands/auth.rs`.

## Secrets discipline — keyring only, never `settings.json`, never logs

The refresh token and optional client secret live in the OS keyring (Keychain / Credential
Manager / Secret Service). The access token is in-memory only. `settings.json` holds
non-sensitive config (instance URL, client id, ports, windows, presets). Never write a
token/secret to disk or a `tracing` event. Types that hold one (`TokenSet`, `TokenResponse`,
`SaveSettingsArgs`) have hand-written `Debug` impls that print `<redacted>`, so a `{:?}` can't leak
it either.

## PKCE, lazy token, Native-or-Web client

`AuthState::access_token()` refreshes lazily before each call. Sign-in is the interactive S256
PKCE flow with a **loopback** redirect on the configured `callback_port` (default `11434`); a hung
sign-in usually means the callback never arrived. **Native** (public) clients have **no** secret;
**Web** (confidential) clients do — the app supports both, so don't hardcode either.

`11434` is also Ollama's default port, so a bind failure names "another program" and the Settings
field rather than blaming a second copy of this app. The default itself stays: it is the redirect
registered in customers' NinjaOne apps.

## A saved sign-in is reused at launch

Because the access token is in-memory only, every launch starts with none. `auth_status` and
`sign_in` therefore call `AuthState::restore_session`, which tries a silent refresh from the
keyring's refresh token for the current tenant; any failure (none saved, `invalid_grant`, a keyring
fault, the network) reads as *not signed in*, never an error. Reporting "signed out" on a
launch that held a working grant meant the lookups never loaded and Sign in ran the browser flow
for a grant the app already had. `reauthorize` must **not** do this — its whole point is to drop
the refresh token and force consent. `auth_status` is `async` for this reason; its IPC shape is
unchanged.

## Sign-out sticks: the session generation

`logout` bumps a session generation (`GrantStamp.session`) and a completed interactive sign-in bumps
it again. Every grant samples a `GrantStamp` (tenant + generation) before its round trip, and
`store_tokens` refuses one whose stamp no longer matches. The tenant stamp alone could not see a
sign-out — a refresh in flight when the operator signed out carried the same tenant, so its
response re-stored the tokens in memory *and* rewrote the keyring entry `logout` had just
deleted. `persist_lock` (a `std` mutex, taken only on blocking threads) pairs each in-memory change
with its keyring write, so a store that already passed its check cannot write after a sign-out.

The same stamp decides what a dead grant may delete: `discard_dead_grant` does nothing once the
generation has moved on, deletes the keyring entry of the tenant the refresh **started** under
(not whichever is configured now), and clears the in-memory tokens only if they are still that
tenant's. A refresh also reads host, client id, secret and refresh token in one lock acquisition
(`refresh_grant`), so the four cannot come from different tenants.

## `expires_in` is clamped

The server's `expires_in` is clamped to 60 s ..= 24 h, and the refresh skew is
`min(5 min, lifetime / 2)` (`refresh_deadline`). Unchecked, a tiny value made every token stale on
arrival — a refresh per API call — and a huge one overflowed `chrono::Duration::seconds`, which
panics.

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
