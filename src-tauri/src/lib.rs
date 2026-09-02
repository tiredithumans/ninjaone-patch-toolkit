mod actions;
mod api;
mod auth;
mod commands;
mod error;
mod export;
mod filter;
mod history;
mod model;
mod paths;
mod report;
mod rows;
mod settings;
mod state;

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use state::AppState;

/// Default log filter when `RUST_LOG` says nothing.
///
/// Debug for this crate in debug builds, `info` in release. It used to be debug in
/// both, which made every debug event in the crate a *shipped* default — including
/// ones whose arguments are only safe under a filter nobody had opted into. A
/// released ops tool should not be more talkative than asked; `RUST_LOG` still turns
/// everything back on for anyone diagnosing a problem.
#[cfg(debug_assertions)]
const DEFAULT_LOG_FILTER: &str = "info,ninjaone_patch_toolkit_lib=debug";
#[cfg(not(debug_assertions))]
const DEFAULT_LOG_FILTER: &str = "info";

/// Days of rolling log files to keep. A week spans "it started on Monday" without
/// letting an idle install grow a log directory forever.
const LOG_FILES_KEPT: usize = 7;

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    // A bundled `.app` launched from Finder, or an `.msi` from the Start menu, has
    // nowhere for stdout to go — it is discarded. `RUST_LOG` and a terminal recover
    // it, but a patching operator will not relaunch their tool from a shell, so in
    // practice every field bug report arrived with no evidence at all: the one
    // durable artifact was the action audit log, which covers the write path and
    // nothing else. The file layer is what makes a report actionable.
    //
    // Deliberately not `tracing_appender::non_blocking`: that returns a guard whose
    // drop flushes, so it would have to be kept alive for the process lifetime and
    // an early return would silently truncate the log. At `info` this writes a
    // handful of lines per query, and losing the last line of a crash is exactly
    // what must not happen here.
    let file_layer = paths::log_dir()
        .and_then(|dir| {
            std::fs::create_dir_all(&dir)?;
            Ok(tracing_appender::rolling::Builder::new()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix("ninjaone-patch-toolkit")
                .filename_suffix("log")
                .max_log_files(LOG_FILES_KEPT)
                .build(&dir)?)
        })
        .map(|appender| {
            fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(appender)
        })
        .ok();

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false).compact())
        // `Option<Layer>` is itself a `Layer` that does nothing when `None`, so a
        // read-only or missing config directory costs the operator stdout logging
        // and nothing else — it must never stop the app from starting.
        .with(file_layer)
        .try_init();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();

    let app_state = AppState::new().expect("failed to initialize application state");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::auth::sign_in,
            commands::auth::sign_out,
            commands::auth::auth_status,
            commands::auth::reauthorize,
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::settings::save_preset,
            commands::settings::delete_preset,
            commands::lookups::list_orgs,
            commands::lookups::list_locations,
            commands::lookups::list_roles,
            commands::lookups::list_node_classes,
            commands::patches::query_patches,
            commands::patches::get_patch_rows,
            commands::patches::get_patch_groups,
            commands::patches::get_patch_group_members,
            commands::export::export_patches_xlsx,
            commands::export::export_report_html,
            commands::diagnostics::read_action_audit,
            commands::diagnostics::read_run_history,
            commands::diagnostics::open_diagnostics_folder,
            commands::update::check_for_update,
            commands::update::install_update,
            commands::actions::plan_action,
            commands::actions::run_action,
            commands::actions::list_jobs,
            commands::actions::clear_jobs,
            commands::actions::list_scripts,
            commands::actions::list_run_as_options,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
