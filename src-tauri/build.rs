fn main() {
    // Windows gives the main thread a 1 MiB stack (macOS/Linux: 8 MiB). v0.15.0's
    // release build overflowed it (0xc00000fd, `thread 'main' has overflowed its
    // stack`) on every Run query; 8 MiB fixed it. See
    // docs/design/concurrency.md#the-windows-main-thread-has-a-1-mib-stack.
    //
    // Here rather than in `src-tauri/.cargo/config.toml`, which cargo reads only when
    // run from inside `src-tauri/` — not from the repo root with `--manifest-path`, as
    // the justfile does. Not `RUSTFLAGS`, which also reaches the Trunk WASM build and
    // build scripts. `-bins` keeps it to the app binary; `/STACK` is an MSVC linker
    // option, so other Windows toolchains are left alone.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
    tauri_build::build();
}
