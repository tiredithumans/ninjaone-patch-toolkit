---
name: debug
description: Debug issues in the NinjaOne Patch Toolkit (Tauri + Leptos WASM app). Use when the user says "debug X" where X is a symptom (e.g., "sign-in hangs", "patches not loading", "export is empty", "WASM build error"), or asks for help diagnosing a problem.
argument-hint: "[symptom] — e.g., 'sign-in hangs', 'export is empty'"
---

# Debug — diagnose Tauri + Leptos WASM issues

Walk through the app's architecture layers to pinpoint the root cause. Start with a hypothesis from
the symptom, then check each layer systematically.

## 0. Clarify the symptom

Confirm:
- Which surface is affected? (sign-in/auth, fleet filters, patch listing, compliance/reboot views,
  Excel export, settings/presets.)
- Is it happening in `just dev` (native) or only after a WASM rebuild?
- Any error toast (the backend's `UiError.message`) or terminal/console output?

## 1. Check the Rust backend (`src-tauri/`)

If Rust source changed:
- `just clippy` — fastest check for compile-time issues.
- `just test` — backend unit + wiremock integration tests (`cargo test --manifest-path src-tauri/Cargo.toml`).

If backend logic is responsible:
- `src-tauri/src/state.rs` — `AppState` singleton (auth handle, API client, `last_result` cache).
- `src-tauri/src/commands/<domain>.rs` — the failing handler.
- `src-tauri/src/api/` — NinjaOne client (`devices.rs`, `patches.rs`, `lookups.rs`); pagination and
  the `df` query string.
- `src-tauri/src/filter.rs` — `df` device-filter builder + client-side OS-name facet.
- `src-tauri/src/rows.rs` — device↔patch join, compliance/SLA rollups, pending counts.

## 2. Check the WASM frontend (`web-rs/`)

- `just web-build` (Trunk) or `just web-check` — rebuild to see if the issue is in WASM.
- `web-rs/src/api.rs` — do the `invoke(...)` wrappers match backend handler names + arg shapes?
- `web-rs/src/types.rs` — do the frontend types mirror `src-tauri/src/model.rs` / command structs?
- `web-rs/src/app.rs` — the view/handler that renders the feature.

## 3. Check auth (common source of bugs)

For sign-in / token / keyring issues (`src-tauri/src/auth.rs`):
- **PKCE flow:** loopback redirect on the configured callback port (default `11434`); the browser
  must reach `http://localhost:<port>/`. A hung sign-in is usually the callback never arriving.
- **Scopes:** read-only `monitoring offline_access`. A 403 on a NinjaOne call ≠ an auth bug.
- **Keyring:** refresh token + optional client secret live in the OS keyring (Keychain / Credential
  Manager / Secret Service). A "not signed in after restart" symptom → keyring read failing or empty.
- **Native vs Web app:** Native (public) clients have **no** secret; a "secret required" error means
  the client-type/secret config is mismatched in Settings.

## 4. Check the IPC boundary

- Is the command in `generate_handler![]` (`src-tauri/src/lib.rs`)?
- Does the `invoke("<name>", …)` string in `web-rs/src/api.rs` match the registered command, and do
  the arg-object keys match the Rust fn parameter names (camelCase)?
- `command-parity-check.sh` flags missing registry/wrapper halves — re-read its last warning.

## 5. Output format

```
# Debug Report — Export Is Empty

## Symptom
Excel export downloads a workbook with only headers, no rows.

## Hypothesis
`export_patches_xlsx` reads `state.last_result`, which is only populated by `query_patches`.
Export was triggered before any query ran (or after a failed query that left the cache empty).

## Evidence
- `src-tauri/src/commands/export.rs` reads `state.last_result`.
- `src-tauri/src/commands/patches.rs:180` only writes `last_result` on a successful query.

## Next steps
1. `just dev`, run a query first, then export.
2. If still empty → log row count in `query_patches` before caching.

## Files changed
- src-tauri/src/commands/export.rs (guard: error if no cached result)
```

## Common symptoms and quick checks

| Symptom | Quick check | Likely culprit |
|---------|-------------|----------------|
| Sign-in hangs | callback port reachable? firewall? | PKCE loopback callback never arrives (`auth.rs`) |
| Not signed in after restart | keyring entry present? | refresh token not stored/read (`auth.rs`) |
| Patches list empty | `df` string built right? statuses requested? | `filter.rs` device-filter or status mapping |
| "Installed" patches missing | install-window days? | history endpoints not queried (`commands/patches.rs`) |
| Export empty | did a query run first? | `state.last_result` not populated |
| WASM build error | `just web-build` (Trunk) | server dep (tokio/reqwest/keyring) pulled into `web-rs` |
| Command not found in frontend | check `api.rs` + handler list | parity gap (`command-parity-check.sh`) |
| Wrong camelCase in UI | check `api.rs` args vs backend params | arg-object key ≠ Rust fn param name |

## Failure handling

- If `just verify` passes but the issue persists → likely a runtime/logic bug, not a build error.
- If `just verify` fails → fix the failing gate first, then re-check whether the original issue remains.
- If WASM-only → check Trunk output (`web-rs/dist/`), browser console, and WASM-specific panics
  (`console_error_panic_hook` is wired in).
