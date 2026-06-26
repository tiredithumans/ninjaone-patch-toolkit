# NinjaOne Patch Toolkit — task runner
# Backend (Tauri 2) lives in src-tauri/; the Leptos CSR frontend in web-rs/.

set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

wasm := "wasm32-unknown-unknown"

# List available recipes.
default:
    @just --list

# --- Daily development -------------------------------------------------------
# Frontend recipes pass `--config web-rs/Trunk.toml` so trunk resolves paths from
# web-rs/ without a shell `cd`, keeping them portable across sh and PowerShell.

# Run the desktop app (Tauri auto-starts `trunk serve` via beforeDevCommand).
dev:
    cargo tauri dev

# Serve the frontend on http://localhost:8080 with hot reload.
web-serve:
    trunk serve --config web-rs/Trunk.toml

# Debug frontend build into web-rs/dist.
web-build:
    trunk build --config web-rs/Trunk.toml

# Release frontend build into web-rs/dist.
web-build-release:
    trunk build --release --locked --config web-rs/Trunk.toml

# Release frontend build for GitHub Pages. Sets the base href to the repo subpath
# so assets resolve under https://<user>.github.io/<repo>/. The desktop build keeps
# the default "/" base — never set public_url in Trunk.toml or Tauri's webview (which
# loads from a relative dist) breaks.
web-build-pages base="/ninjaone-patch-toolkit/":
    trunk build --release --locked --public-url {{base}} --config web-rs/Trunk.toml

# --- Verification (CI gates) -------------------------------------------------

# Format both crates.
fmt:
    cargo fmt --manifest-path src-tauri/Cargo.toml
    cargo fmt --manifest-path web-rs/Cargo.toml

# Check formatting in both crates.
fmt-check:
    cargo fmt --manifest-path src-tauri/Cargo.toml --check
    cargo fmt --manifest-path web-rs/Cargo.toml --check

# Lint the backend with warnings denied.
clippy:
    cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings

# Lint the frontend (wasm target) with warnings denied.
web-clippy:
    cargo clippy --manifest-path web-rs/Cargo.toml --target {{wasm}} -- -D warnings

# Backend unit + integration tests.
test:
    cargo test --manifest-path src-tauri/Cargo.toml

# Backend test coverage (requires `cargo install cargo-llvm-cov`). Runs the
# instrumented suite once, prints a per-file summary, then writes an lcov report
# to src-tauri/target/lcov.info. The wasm frontend has no test suite, so coverage
# is backend-only (the `web-check`/`web-clippy` gates cover that crate).
coverage:
    cargo llvm-cov --manifest-path src-tauri/Cargo.toml --no-report
    cargo llvm-cov --manifest-path src-tauri/Cargo.toml report --summary-only
    cargo llvm-cov --manifest-path src-tauri/Cargo.toml report --lcov --output-path src-tauri/target/lcov.info

# Type-check the frontend for the wasm target.
web-check:
    cargo check --manifest-path web-rs/Cargo.toml --target {{wasm}}

# Frontend pure-helper unit tests on the host target. The wasm build excludes the
# #[cfg(test)] module, and the helpers under test are JS-free so they run natively
# (no wasm/browser runner). Components and js_sys-backed helpers aren't covered.
web-test:
    cargo test --manifest-path web-rs/Cargo.toml

# Run every CI gate in sequence.
verify: fmt-check clippy test web-check web-clippy web-test

# --- Dependency policy -------------------------------------------------------

# RustSec advisory scan (requires `cargo install cargo-audit`).
audit:
    cargo audit --file src-tauri/Cargo.lock
    cargo audit --file web-rs/Cargo.lock

# License + supply-chain + bans policy (deny.toml; requires `cargo install cargo-deny`).
deny:
    cargo deny --manifest-path src-tauri/Cargo.toml check --config deny.toml licenses bans sources

# Same license/supply-chain policy for the frontend tree.
web-deny:
    cargo deny --manifest-path web-rs/Cargo.toml check --config deny.toml licenses bans sources

# --- Release / packaging -----------------------------------------------------

# Build distributable bundles (.dmg/.app, .msi/.nsis, AppImage).
build:
    cargo tauri build

# Regenerate bundled icon formats from the source PNG.
icon:
    cargo tauri icon src-tauri/icons/icon.png

# Regenerate the README demo screenshot (docs/images/screenshot.png) by driving the
# built web demo in headless Chromium. Needs Node; first run installs Playwright +
# its Chromium under scripts/ (both gitignored). CI runs the same via screenshot.yml.
screenshot:
    just web-build
    npm --prefix scripts install --no-audit --no-fund
    cd scripts && npx playwright install chromium
    node scripts/screenshot.mjs

# --- Housekeeping ------------------------------------------------------------

# Remove build artifacts from both crates and the frontend dist.
clean:
    cargo clean --manifest-path src-tauri/Cargo.toml
    cargo clean --manifest-path web-rs/Cargo.toml
    trunk clean --config web-rs/Trunk.toml
