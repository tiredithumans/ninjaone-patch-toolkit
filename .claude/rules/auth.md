---
paths:
  - "src-tauri/src/auth*.rs"
  - "src-tauri/src/commands/auth.rs"
  - "src-tauri/src/settings.rs"
  - "src-tauri/src/commands/settings.rs"
---

# Auth (OAuth2 PKCE, keyring, refresh)

- **Secrets live in the keyring only — never `settings.json`, never a `tracing` event.** The
  access token is in-memory only; `restore_session` silently refreshes from the keyring at launch
  and on `sign_in` (never on `reauthorize`). Every entry opens through `keyring_entry`, which
  makes a Windows credential `Local`, never roaming.
  → `docs/design/auth.md#secrets-discipline--keyring-only-never-settingsjson-never-logs`
- **Sign-out sticks.** `logout` and a completed interactive sign-in bump the session generation;
  `store_tokens` checks tenant + session under `persist_lock`, and so does reading a saved token
  (`load_saved_refresh`); a dead grant deletes only the entry of the tenant it started under.
  → `docs/design/auth.md#sign-out-sticks-the-session-generation`
- **Sign-out removes every tenant's saved sign-in.** A refresh token is written only through
  `save_refresh_entry`, which names it in the keyring index first (and writes nothing if it
  cannot); `logout` deletes every indexed entry plus the configured and legacy ones. A tenant
  switch keeps the other tenant's. Test: `signing_out_deletes_every_tenants_saved_sign_in`.
  → `docs/design/auth.md#sign-out-removes-every-tenants-saved-sign-in-the-index`
- **An instance is an origin** (`settings::instance_url_problem`: https or loopback http; no user
  name, query, fragment or path). `save_settings` refuses anything else; a stored value that fails
  it loads as written and is never contacted (`refresh_grant`, `login_pkce`) — never rewrite it.
  → `docs/design/auth.md#the-instance-is-an-origin-and-one-that-is-not-is-never-contacted`
- **PKCE with a loopback redirect on `callback_port`; Native (no secret) and Web (secret)
  clients are both supported.** The callback listener loops over connections. The browser opens
  through `open_browser` (`that_detached` on a blocking thread); its error is logged by kind and
  `url_without_query` only — `open`'s message carries the whole authorize URL.
  → `docs/design/auth.md#the-browser-launch-neither-holds-a-worker-nor-logs-the-url`
- **Scope is conditional on `settings.actions.enabled` and the refresh grant never re-sends it.**
  `management_grant()` detects a read-only grant; `None` means unknowable, not denied.
  `reauthorize` drops the keyring refresh token first.
  → `docs/design/auth.md#scope-is-conditional-and-the-refresh-grant-never-re-sends-it`
- **`store_tokens` assigns in-memory first (session-gated) and downgrades a keyring failure to a
  warning.** `invalidate_access_token(&stale)` no-ops unless the token is still current.
  → `docs/design/auth.md#in-memory-before-keyring-and-only-the-token-that-got-the-401-is-invalidated`
- **The refresh is single-flight under `refresh_lock`, and only `invalid_grant` clears the
  credential** (`refresh_grant_is_dead`). Not "any 4xx": 429 is retry-later. Callers queued
  behind a failure for the same `GrantStamp` — transient from the endpoint, an unreadable token
  body, or a keyring read fault — return it rather than re-POSTing or re-reading.
  → `docs/design/auth.md#the-refresh-is-single-flight-and-only-invalid_grant-clears-the-credential`
