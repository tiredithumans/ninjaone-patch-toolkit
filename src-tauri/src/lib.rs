mod actions;
mod api;
mod auth;
mod commands;
mod error;
mod export;
mod filter;
mod model;
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

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false).compact())
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
