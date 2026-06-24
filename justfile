# NinjaOne Patch Toolkit — task runner
# Backend (Tauri 2) lives in src-tauri/; the Leptos CSR frontend in web-rs/.

set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

wasm := "wasm32-unknown-unknown"

# List available recipes.
default:
    @just --list

# --- Daily development -------------------------------------------------------

# Run the desktop app (Tauri auto-starts `trunk serve` via beforeDevCommand).
dev:
    cargo tauri dev

# Serve the frontend on http://localhost:8080 with hot reload.
web-serve:
    cd web-rs && trunk serve

# Debug frontend build into web-rs/dist.
web-build:
    cd web-rs && trunk build

# Release frontend build into web-rs/dist.
web-build-release:
    cd web-rs && trunk build --release --locked

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

# Type-check the frontend for the wasm target.
web-check:
    cargo check --manifest-path web-rs/Cargo.toml --target {{wasm}}

# Run every CI gate in sequence.
verify: fmt-check clippy test web-check web-clippy

# --- Dependency policy -------------------------------------------------------

# RustSec advisory scan (requires `cargo install cargo-audit`).
audit:
    cargo audit --file src-tauri/Cargo.lock
    cargo audit --file web-rs/Cargo.lock

# --- Release / packaging -----------------------------------------------------

# Build distributable bundles (.dmg/.app, .msi/.nsis, AppImage).
build:
    cargo tauri build

# Regenerate bundled icon formats from the source PNG.
icon:
    cargo tauri icon src-tauri/icons/icon.png

# --- Housekeeping ------------------------------------------------------------

# Remove build artifacts from both crates and the frontend dist.
clean:
    cargo clean --manifest-path src-tauri/Cargo.toml
    cargo clean --manifest-path web-rs/Cargo.toml
    rm -rf web-rs/dist
