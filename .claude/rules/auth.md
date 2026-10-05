---
paths:
  - "src-tauri/src/auth*.rs"
  - "src-tauri/src/commands/auth.rs"
---

# Auth (OAuth2 PKCE, keyring, refresh)

- **Secrets live in the keyring only — never `settings.json`, never a `tracing` event.** The
  access token is in-memory only; `restore_session` silently refreshes from the keyring at launch
  and on `sign_in` (never on `reauthorize`). → `docs/design/auth.md#secrets-discipline--keyring-only-never-settingsjson-never-logs`
- **Sign-out sticks.** `logout` and a completed interactive sign-in bump the session generation;
  `store_tokens` checks tenant + session under `persist_lock`; a dead grant deletes only the
  entry of the tenant it started under. → `docs/design/auth.md#sign-out-sticks-the-session-generation`
- **PKCE with a loopback redirect on `callback_port`; Native (no secret) and Web (secret)
  clients are both supported.** The callback listener loops over connections. → `docs/design/auth.md`
- **Scope is conditional on `settings.actions.enabled` and the refresh grant never re-sends it.**
  `management_grant()` detects a read-only grant; `None` means unknowable, not denied.
  `reauthorize` drops the keyring refresh token first.
  → `docs/design/auth.md#scope-is-conditional-and-the-refresh-grant-never-re-sends-it`
- **`store_tokens` assigns in-memory first (session-gated) and downgrades a keyring failure to a
  warning.** `invalidate_access_token(&stale)` no-ops unless the token is still current.
  → `docs/design/auth.md#in-memory-before-keyring-and-only-the-token-that-got-the-401-is-invalidated`
- **The refresh is single-flight under `refresh_lock`, and only `invalid_grant` clears the
  credential** (`refresh_grant_is_dead`). Not "any 4xx": 429 is retry-later.
  → `docs/design/auth.md#the-refresh-is-single-flight-and-only-invalid_grant-clears-the-credential`