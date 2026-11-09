# Auth: PKCE, keyring, scope, refresh

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/auth.rs`, `src-tauri/src/state.rs` (`AppState`),
`src-tauri/src/commands/auth.rs`, `src-tauri/src/settings.rs` (`instance_url_problem`).

## Secrets discipline — keyring only, never `settings.json`, never logs

The refresh token and optional client secret live in the OS keyring (Keychain / Credential
Manager / Secret Service). The access token is in-memory only. `settings.json` holds
non-sensitive config (instance URL, client id, ports, windows, presets). Never write a
token/secret to disk or a `tracing` event. Types that hold one (`TokenSet`, `TokenResponse`,
`SaveSettingsArgs`) have hand-written `Debug` impls that print `<redacted>`, so a `{:?}` can't leak
it either.

On Windows every entry is created with `Local` persistence (`keyring_entry`). Credential Manager's
store defaults to `Enterprise`, which roams with the user's profile, so the refresh token — a
fleet-wide `management` grant once actions are on — and the client secret were copied to the
profile server and to every machine the operator logged on to. Persistence is applied when a secret
is written, so an entry saved before this moves to `Local` at its next write. `keyring`'s own
`Entry` cannot take the modifier, which is why `keyring-core` is a direct dependency.

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

A request is stamped too. The API client builds its URL from `tenant_stamp()` once and asks
`access_token_for(&stamp)` on every attempt, which fails with `TenantMoved` (`is_tenant_moved`)
instead of handing out a token once `apply_settings` has moved the tenant: the fresh path compares
the tenant under the same read lock that reads the token, and the refresh path compares the grant's
stamp before it POSTs. Plain `access_token()` refreshed for whichever tenant was current, so a read
waiting out a 429 across a switch sent tenant B's token to host A
([api-client.md](./api-client.md#a-429-pauses-the-whole-client-and-each-attempts-token-is-bound-to-the-urls-tenant)).

Reading the saved refresh token (`load_saved_refresh`) is under `persist_lock` and checks the
generation too. Adopting the pre-tenant-scoping entry *writes* the tenant's, and a sign-out landing
between that read and that write had its deletion undone; and a caller whose session ended while it
queued read the *next* session's refresh token and POSTed it for a grant `store_tokens` then refused
— under rotation, spending the new session's credential.

## Sign-out removes every tenant's saved sign-in: the index

A tenant switch keeps the previous tenant's refresh token so that switching back finds it
(`apply_settings`). That made sign-out partial: `logout` deleted only the configured tenant's entry,
so after a switch the other tenant's grant stayed on disk behind a clean-looking sign-out, and the
next operator to point Settings at that tenant was silently signed in as the previous one — with
their scope, and with NinjaOne attributing every action to them. Saving Settings was enough: the
frontend's `auth_status` call restores it.

So every refresh token the app writes is first named in an index (`save_refresh_entry` →
`index_refresh_entry`, under `persist_lock`), and `logout` — and `reauthorize`, which signs out
first — deletes every named entry as well as the configured tenant's and the legacy one, then the
index. A failed delete is reported, as before, and leaves the index for the next sign-out to retry.
Index first, token second: a named entry with nothing behind it costs one no-op delete, while an
unnamed token is the bug, so a token whose entry cannot be indexed is not written at all — the same
"no persistence this session" a failed token write degrades to. Every name read back is held to
`tenant_entry`'s spelling (`is_refresh_entry_name`), so whatever the index holds, sign-out deletes
only refresh tokens.

The index is itself a keyring entry (`refresh_token_index`, one entry name — a non-secret hash — per
line), not a file in the config directory. It lives and dies with the tokens it names: same
machine, same profile, same "reset the keychain". A file could be deleted, roam with a Windows
profile, or come back from a backup independently of them, and a name missing from the index is
exactly the failure it exists to prevent. Tests give each `AuthState` its own index name
(`refresh_index_entry`): they share one in-process keyring and run in parallel.

**Not covered:** a refresh token saved before the index existed, for a tenant other than the one
configured at sign-out. None of the keyring stores this app links can be listed portably, so such an
entry is found only by name — once its tenant is visited (`load_saved_refresh` indexes a token it
reads), or when it is the configured tenant at sign-out.

## The instance is an origin, and one that is not is never contacted

`settings::instance_url_problem` is the one rule: `https` (or `http` to loopback, for a mock
server), no user name or password, no query, no fragment, no path beyond `/`. A user name moves the
host — `https://us2.ninjarmm.com@evil.example` is `evil.example` — and every endpoint is appended to
the instance as written, so a query or fragment swallows `/ws/oauth/token` and a path misdirects
it. `save_settings` refuses such a value. One that reaches `settings.json` another way (an older
version, a hand edit) loads exactly as written and reads as *not configured*: `refresh_grant` and
`login_pkce` refuse it before the keyring read, the probe or the browser, and the load logs the
problem, never the value. It is not repaired, because the instance string — with the client id —
keys the saved sign-ins, the first-seen ledger and the run snapshots, so rewriting even a valid
spelling of it would orphan them. For the same reason the load-time `http`→`https` upgrade applies
only to a value that is otherwise valid, which also keeps a user name's password out of its log
line.

## The browser launch neither holds a worker nor logs the URL

`open_browser` runs `open::that_detached` on a blocking thread. `open::that` waited for its
launcher on a tokio worker: a whole `powershell.exe` run on Windows, and on a generic desktop an
`xdg-open` that runs a browser that was not already open in the foreground, until it closes —
before the callback's three-minute clock had started. A failure is logged by `io::ErrorKind` and
`url_without_query` only, because `open` words its error from the launcher's command line, which is
the authorize URL with `state` and `code_challenge`. The log directory is created (and tightened
to) 0700 on Unix (`prepare_log_dir` in `lib.rs`): `tracing_appender` creates its files 0644.

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
fallback; a refresh that omits both keeps the session's previous scope (§5.1 allows omitting an
unchanged scope), while an interactive sign-in never inherits one. `None` means *unknowable*, not
*denied*, and the UI words the two differently.
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
await one grant. A failure is shared the same way: the lock holds the last one (stamped with its
`GrantStamp`) and a counter readable outside it, so a caller that queued while an attempt for the
same grant failed returns that error instead of POSTing again. Without it a 429 or a 5xx became
one serial retry per queued caller. The same record covers a keyring read that could not produce
the grant (a locked keychain, no Secret Service, nothing saved), which was otherwise one blocking
keyring round trip per queued caller, and a 2xx whose body is not a token response, which under
rotation would have had every queued caller re-POST a spent token into `invalid_grant`. A caller
arriving after the failure still retries; there is deliberately no cooldown, which would stretch
a blip into an outage.

That composes with the error arm: `refresh_grant_is_dead` clears the stored refresh token **only**
on a 400/401 whose OAuth `error` is `invalid_grant`. Clearing on any non-2xx meant a 429, a 5xx or
a captive-portal page forced an interactive re-login — and under refresh-token rotation the loser
of a refresh race erased the credential the winner had just stored. Deliberately **not** "any
4xx": 429 is a retry-later status.
