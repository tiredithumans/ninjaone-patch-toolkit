# NinjaOne Patch Toolkit

[![CI](https://github.com/tiredithumans/ninjaone-patch-toolkit/actions/workflows/ci.yml/badge.svg)](https://github.com/tiredithumans/ninjaone-patch-toolkit/actions/workflows/ci.yml)
[![CodeQL](https://github.com/tiredithumans/ninjaone-patch-toolkit/actions/workflows/codeql.yml/badge.svg)](https://github.com/tiredithumans/ninjaone-patch-toolkit/actions/workflows/codeql.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A native desktop toolkit (Rust / Leptos / Tauri 2) for patching‑operations teams. It
authenticates to the NinjaOne Public API with **OAuth 2.0 + PKCE**, filters the fleet,
lists individual patches per server, and exports to Excel.

## Features

- **PKCE OAuth 2.0** against `/ws/oauth/authorize` + `/ws/oauth/token` (S256, loopback
  redirect). Read‑only scope `monitoring offline_access`. Refresh token stored in the OS
  keyring; the client secret is optional (Native app registrations have none).
- **Advanced filtering** — Organization, Location, Device Role, and OS Type. OS Type is
  both the coarse NinjaOne node‑class facet (pushed into the `df` query) and a granular,
  client‑side OS‑name substring filter.
- **Per‑server patch listing** by **type** (All / OS / Software) and **status**
  (Pending / Approved / Rejected / Installed, plus Failed). Installed patches are pulled
  from the patch‑install history endpoints over a configurable window.
- **Excel export** (`.xlsx`) — Patches detail sheet + Compliance summary + Needs‑Reboot
  sheet, with frozen headers and autofilter.
- **Patching‑ops extras**
  - Install‑history export (what actually installed / failed) over a date window.
  - Reboot & failure views (devices pending reboot; `FAILED` patches).
  - Compliance & SLA aging — per‑org compliance % and aged Critical/Important backlog.
  - Saved filter presets and optional auto‑refresh.

## Architecture

```
src-tauri/   Tauri 2 backend (Rust): auth (PKCE), NinjaOne API client, device↔patch
             join, xlsx export, IPC commands.
web-rs/      Leptos 0.8 (CSR) frontend, bundled by Trunk, talking to the backend over
             the global __TAURI__ invoke bridge.
```

Backend modules of note: `auth.rs` (PKCE + keyring), `api/` (client, pagination, lookups,
devices, patches), `filter.rs` (`df` builder + client‑side facets), `rows.rs` (join →
`PatchRow`, compliance), `export.rs` (`rust_xlsxwriter`).

## NinjaOne setup

In NinjaOne: **Administration → Apps → API → Client App IDs → Add**.

- **Application Platform:** `Native` (public client, no secret) — or `Web` if you prefer a
  confidential client with a secret (the app supports both).
- **Scopes:** `Monitoring` and `offline_access`.
- **Redirect URI:** loopback `http://localhost:11434/` (Native apps use localhost; the
  port matches the app's configurable callback port).

Copy the generated **Client ID** (and Secret, if a Web app).

## Prerequisites

- Rust **1.96** with the `wasm32-unknown-unknown` target (pinned in `rust-toolchain.toml`).
- [`trunk`](https://trunkrs.dev), the Tauri CLI (`cargo install tauri-cli`), and a matching
  `wasm-bindgen-cli` (`cargo install wasm-bindgen-cli --version <lockfile version>`).
- Platform webview deps (WebKitGTK on Linux; bundled on macOS/Windows).

## Run

```sh
just dev          # launches the desktop app (Tauri auto-starts `trunk serve`)
# or, separately:
just web-serve    # frontend dev server on http://localhost:8080
```

On first launch open **Settings**, pick your **Region/Instance** (e.g. `us2`), paste the
**Client ID** (and Secret if applicable), then **Sign in** to complete the PKCE browser
flow.

## Build & verify

```sh
just build        # distributable bundles (.dmg/.app, .msi/.nsis, AppImage)
just verify       # fmt-check + clippy + tests + wasm check + wasm clippy
just test         # backend unit + wiremock integration tests
```

## Security

- Access tokens are kept in memory; the refresh token and optional client secret live in
  the OS keyring (Keychain / Credential Manager / Secret Service). Nothing sensitive is
  written to `settings.json`.
- The app requests read‑only (`monitoring`) scope only.

## Updates

The app checks GitHub for a newer release on launch and offers to install it, showing the new
version's release notes first. Toggle the launch check under **Settings → Automatically check for
updates**, or click **Check for updates** there anytime. Updates are signed (minisign) and verified
before they install, and only apply once a release is published — so a draft release never ships to
users.

## Contributing

Issues and PRs are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) and
[AGENTS.md](AGENTS.md) (the conventions every contributor follows). Run
`just verify` before opening a PR. Report security issues privately via the
[security advisory flow](https://github.com/tiredithumans/ninjaone-patch-toolkit/security/advisories/new),
not a public issue (see [SECURITY.md](.github/SECURITY.md)).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
