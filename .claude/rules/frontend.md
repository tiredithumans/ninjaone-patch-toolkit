---
paths:
  - "web-rs/**"
---

# Frontend (Leptos 0.8 CSR, wasm32)

- **Server deps never enter `web-rs`**; shared logic is duplicated as plain types.
  → `docs/design/frontend.md#wasm-gating`
- **CSP governs the webview only; NinjaOne hosts need no `connect-src` change.** The updater is
  backend egress too. → `docs/design/frontend.md#csp-governs-the-webview-not-backend-egress`
- **Non-trivial logic does not belong in a `#[component]` body or in `state.rs`** — put it in
  the `util` module as a free function and test it there.
  → `docs/design/frontend.md#non-trivial-logic-does-not-belong-in-a-component-body`
- **A dialog calls `modal::focus_trap()` in the closure that creates it**, per instance.
  → `docs/design/frontend.md#frontend-reactivity-is-closure-based-leptos-csr`
- **A `query.members` entry is replaced with a fresh `Arc`, never mutated in place** — open
  groups detect change by `Arc::ptr_eq` (`util::member_entry_changed`).
  → `docs/design/frontend.md#frontend-reactivity-is-closure-based-leptos-csr`
- **Every Jobs column that can change after dispatch is part of `util::job_row_key`** — the
  keyed `<For>` builds a row's cells once per key. Test: `job_row_key_moves_with_every_mutable_column`.
  → `docs/design/frontend.md#frontend-reactivity-is-closure-based-leptos-csr`
- **View prefs live in `localStorage` (`api::ui_pref_str`; may throw); no shortcut reaches a
  mutating action; a view link carries no selection or credential.**
  → `docs/design/frontend.md#operator-ux`
- **`api::is_tauri()` gates every backend touch; `demo.rs` is the only sample-data source and
  demo mode is web-only.** Never set `public_url` in `Trunk.toml`.
  → `docs/design/frontend.md#demo-mode--browserpages-guard`
- **IPC arg keys equal the Rust handler's parameter names, camelCase** (mirrored here in
  `api.rs`/`types.rs`); see `.claude/rules/ipc-contract.md`.