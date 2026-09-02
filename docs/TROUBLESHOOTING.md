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

## Patch actions

### The action buttons are greyed out

The bar under the tabs says which of these it is:

- *"Patch actions are disabled"* — switch them on in **Settings → Patch actions**.
- *"Your NinjaOne sign-in is read-only"* — choose **Re-authorize**. Enabling the feature
  changes only what the **next** sign-in asks for; the OAuth refresh grant never re-sends
  the scope, so your existing grant stays read-only until you consent again.
- *"Couldn't confirm your sign-in grants the Management scope"* — your tenant issues an
  opaque access token, so the app can't read the granted scope. Re-authorize to be sure.
- *"Patch actions run only in the desktop app"* — you're in the hosted web demo.

### Everything 403s after re-authorizing

The **`Management`** scope isn't enabled on the API app itself. Add it in NinjaOne under
**Administration → Apps → API**, then re-authorize again. If NinjaOne rejects the sign-in
with `invalid_scope`, that is the same cause.

### "Device is not applicable for os apply"

NinjaOne returned 400 for that device: it has no approved OS patches waiting, its patch
policy doesn't cover OS patching, or the agent doesn't support it (common on non-Windows
node classes). Run **Scan OS** first, re-query, and check the device actually has pending
patches.

### The script ran but nothing was installed

Check the exit code in the **Jobs** tab. `Install-CriticalSecurityUpdates.ps1` returns **1**
for a *configuration* error — meaning it never received its parameters. Either the library
entry is missing the **String / Overridable** `kbAllowList`, `rebootBehavior` and `dryRun`
script variables, or the script has no `param()` block to catch positional arguments.

Also confirm you configured the right script: two files in `ninjaone-scripts` share the name
`Install-CriticalSecurityUpdates.ps1`, and only the one under `Windows/Install/` accepts
command-line arguments. Binding by numeric **ID** rather than name avoids picking the wrong
one.

### A job is stuck on "Running"

The poller reads `/activities` every 15 seconds and gives up after 45 minutes. A device that
is offline, or a long install, will sit in *Running* until then — that is expected, not a
hang. Hover the status to get the NinjaOne activity/job ID and look the run up in the
console; the v2 API exposes no script output, so that is where the detail lives.

### A job says "Unknown"

The dispatch request timed out **after** the body was sent, so NinjaOne may or may not have
queued it. The toolkit deliberately does **not** retry these — a replay could install or
reboot twice. Check the device's activity feed in NinjaOne before dispatching again.

### An action was refused as "not confirmed"

Confirmation tokens are single-use, expire after five minutes, and are bound to the exact
device set and parameter string that was planned. Changing the selection (or leaving the
dialog open too long) invalidates the approval. Re-open the action and confirm again.

## Credentials & storage

- The **refresh token** and optional **client secret** live in the OS keyring (Keychain /
  Credential Manager / Secret Service) — never in `settings.json`. If the OS keyring is
  locked or unavailable, sign-in/refresh can fail; unlock it and retry.
- `settings.json` holds only **non-secret** config (instance URL, client id, ports,
  windows, presets, patch-action guardrails). Deleting it resets those to defaults — including
  turning patch actions back off — and it never holds a token.
- `action-audit.jsonl` sits beside it and records one line per dispatched action, written
  before the request goes out and again when it settles. Script parameters that look like
  credentials (`*pass*`, `*secret*`, `*token*`, `*key*`) are redacted; tokens are never
  written there. The **Jobs** tab renders it under *Audit trail*, so it can be read without
  leaving the app; deleting the file discards that history.
- `logs/` holds the rolling daily log files (seven days kept), and **Settings → Open
  diagnostics folder** reveals it. Attach the relevant day to a bug report. `RUST_LOG` still
  raises the level for anyone running the binary from a terminal.

> **Upgrading from 0.13.5 or earlier?** `action-audit.jsonl` used to be written to a
> *different* config directory than `settings.json` (`ninjaone-patch-toolkit` rather than
> `io.github.tiredithumans.NinjaOnePatchToolkit`), despite the docs saying otherwise. New
> records go beside `settings.json`; the old file is still read, so the **Audit trail** view
> shows history from both.

## Build & run (contributors)

- Needs Rust **1.98** with the `wasm32-unknown-unknown` target (pinned in
  `rust-toolchain.toml`), `trunk`, the Tauri CLI, and a matching `wasm-bindgen-cli`.
- On Linux, install the webview deps (`libwebkit2gtk-4.1-dev`, …) — see the CI workflow
  for the exact list.
- `just dev` builds the backend and auto-starts `trunk serve`; a backend-only compile
  needs a frontend `dist` to exist (the `tauri::generate_context!` macro reads it at
  compile time), which `trunk` produces.

See also the [README](../README.md) (setup) and [SECURITY.md](../.github/SECURITY.md)
(security model).
