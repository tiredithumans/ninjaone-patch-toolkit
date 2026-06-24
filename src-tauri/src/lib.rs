mod api;
mod auth;
mod commands;
mod error;
mod export;
mod filter;
mod model;
mod rows;
mod settings;
mod state;

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use state::AppState;

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ninjaone_patch_toolkit_lib=debug"));
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
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::settings::list_presets,
            commands::settings::save_preset,
            commands::settings::delete_preset,
            commands::lookups::list_orgs,
            commands::lookups::list_locations,
            commands::lookups::list_roles,
            commands::lookups::list_node_classes,
            commands::patches::query_patches,
            commands::export::export_patches_xlsx,
            commands::update::check_for_update,
            commands::update::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
