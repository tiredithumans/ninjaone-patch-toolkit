# Contributing to NinjaOne Patch Toolkit

Thanks for your interest in improving the NinjaOne Patch Toolkit. Issues and pull
requests are welcome — **please open an issue first to discuss non-trivial
changes** so we can agree on the approach before you invest time.

By participating you agree to abide by our [Code of Conduct](./CODE_OF_CONDUCT.md).

## Getting set up

Prerequisites and the run/build commands live in the [README](./README.md). The
short version:

```bash
cargo install just      # one-time: the task runner (or brew/winget)
just dev                # daily dev loop (= cargo tauri dev; auto-starts trunk serve)
```

You also need Rust 1.96 with the `wasm32-unknown-unknown` target (pinned in
`rust-toolchain.toml`), the Tauri CLI, and `trunk`. No secrets or env vars are
needed to build — the NinjaOne **Region/Instance**, **Client ID**, and optional
**Secret** are entered at runtime in the app's **Settings**.

## Working agreements

The single source of truth for how the code is structured and the conventions
every contributor (human or AI assistant) follows is **[AGENTS.md](./AGENTS.md)**.
Read it before your first change — it covers the Tauri command pattern, the IPC
arg-shape rule, the keyring/secrets boundary, the `df` filter split, WASM gating,
and the project's other load-bearing gotchas.

## Before you open a pull request

Run the same gates CI runs, and make sure they pass:

```bash
just verify            # fmt-check → clippy → test → web-check → web-clippy
```

- **Keep changes focused.** Solve one logical thing per PR; smaller diffs review
  faster and break less.
- **Test what you change.** Add or update tests alongside behavior changes; keep
  `just test` green.
- **Don't log or persist secrets.** Preserve the keyring boundary described in
  AGENTS.md — tokens and the client secret never reach disk or logs.

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org/) are required (a
local hook validates them): `<type>[(scope)][!]: <description>`.

- **types:** `feat fix docs chore refactor test build ci perf style revert deps`
- **scopes seen in this repo:** `desktop`, `web`, `api`, `auth`, `export`,
  `filter`, `settings`, `ci`, `docs`

## Reporting security issues

Please report vulnerabilities privately — see [SECURITY.md](./.github/SECURITY.md).
Do not open a public issue for them.
