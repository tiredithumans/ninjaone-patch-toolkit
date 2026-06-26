# Troubleshooting

Common issues and how to resolve them. If none of these fit, open an issue with the
app version and (for sign-in problems) your **Region/Instance** and whether the API
client is **Native** or **Web**.

## Sign-in

### Sign-in hangs / the browser tab never returns to the app

The PKCE flow finishes by redirecting your browser to a **loopback** URL the app is
listening on — `http://127.0.0.1:<callback port>` (default **11434**). A hang almost
always means that callback never arrived. Check:

- **Port conflict.** Something else is already bound to the callback port, so the app
  couldn't start its listener. Change **Settings → Callback port** to a free port. For
  a **Web** client this must also match the registered redirect URI exactly (see below).
- **A firewall / security tool blocking loopback.** The redirect is local-only
  (`127.0.0.1`), never outbound — allow the app to listen on the callback port.
- **You closed the browser tab before it redirected.** Start **Sign in** again and let
  the "Login successful" page load fully before closing it.

### Sign-in reports a 404

NinjaOne didn't recognize the Client ID **at that host**. A Client ID is only valid on
the NinjaOne instance it was created on. Fix:

- Set **Settings → Region/Instance** to the exact host you sign in to NinjaOne at — the
  host in your browser's address bar, e.g. `https://us2.ninjarmm.com`. `us2` ≠ `eu` ≠
  `app`; the wrong region 404s.
- Confirm the API client is an **Authorization Code** app (Native or Web), **not** a
  client-credentials / machine-to-machine app — those have no authorization-code flow,
  so the sign-in page 404s.

### "instance URL must use https://"

The instance URL must be `https://` (cleartext would carry OAuth tokens, codes, and the
client secret in the open). `http://` is accepted **only** for `localhost`/`127.0.0.1`
when testing against a mock server.

### Native vs Web client / "redirect URI mismatch"

- **Native** (public) clients have **no** secret. NinjaOne registers the redirect as
  `http://127.0.0.1` and accepts any port, so the **Callback port** can be anything free.
  Leave the **Client Secret** blank.
- **Web** (confidential) clients **do** have a secret, and the redirect URI must be
  registered **exactly** as `http://127.0.0.1:11434` — `127.0.0.1` (not `localhost`), no
  trailing slash, and the port must match **Settings → Callback port**. Paste the secret
  into **Settings → Client Secret**.

### Sign-in succeeds but later calls fail with "possible CSRF" / state mismatch

The browser returned a different `state` value than the one the app generated for this
sign-in. Start sign-in again from the app (don't reuse an old/bookmarked authorize URL).

## Data & export

### "Run a query before exporting."

Export reads the **last successful query's** cached result. Run a query first; if you
just signed out or changed the instance, the cache is intentionally dropped (so an export
can't write a previous tenant's rows) — run a fresh query.

### A FAILED-status query returns nothing

`Failed` and `Installed` are install **results**, sourced from the patch-install history
endpoints over the **install window** (Settings → *Install window (days)*), not the
current-patches feed. If a `Failed` query is empty, widen the install window or confirm
there were install attempts in that window.

### Location names are blank in the rows

Locations are optional labels. If the locations fetch failed, rows still render but omit
the location name (the failure is logged as a warning, not surfaced as an error). Re-run
the query; a persistent blank suggests the locations endpoint is unreachable for your
instance/permissions.

### Compliance % looks too high or excludes devices

Offline devices are **excluded from the compliance denominator** — they can't apply
patches and report no current-patch records, so scoring them would distort the metric.
A device counts as compliant when it has **zero** pending/approved patches.

## Credentials & storage

- The **refresh token** and optional **client secret** live in the OS keyring (Keychain /
  Credential Manager / Secret Service) — never in `settings.json`. If the OS keyring is
  locked or unavailable, sign-in/refresh can fail; unlock it and retry.
- `settings.json` holds only **non-secret** config (instance URL, client id, ports,
  windows, presets). Deleting it resets those to defaults; it never holds a token.

## Build & run (contributors)

- Needs Rust **1.96** with the `wasm32-unknown-unknown` target (pinned in
  `rust-toolchain.toml`), `trunk`, the Tauri CLI, and a matching `wasm-bindgen-cli`.
- On Linux, install the webview deps (`libwebkit2gtk-4.1-dev`, …) — see the CI workflow
  for the exact list.
- `just dev` builds the backend and auto-starts `trunk serve`; a backend-only compile
  needs a frontend `dist` to exist (the `tauri::generate_context!` macro reads it at
  compile time), which `trunk` produces.

See also the [README](../README.md) (setup) and [SECURITY.md](../.github/SECURITY.md)
(security model).
