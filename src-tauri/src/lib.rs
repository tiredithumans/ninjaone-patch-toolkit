mod actions;
mod api;
mod auth;
mod changes;
mod commands;
mod csv_export;
mod error;
mod exploits;
mod export;
mod filter;
mod first_seen;
#[cfg(test)]
mod fixtures;
mod history;
mod model;
mod paths;
mod report;
mod rows;
mod settings;
mod state;
mod window_state;

use std::sync::Arc;

use tauri::Manager;
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
            prepare_log_dir(&dir)?;
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

/// Creates the log directory owner-only (0700 on Unix), and tightens one an earlier
/// version created at the default 0755.
///
/// `tracing_appender` opens its files with the process umask — 0644 in practice —
/// and cannot be asked for less, so the directory is what keeps other local users
/// out of them. Everything else this app writes is 0600 for the same reason: the
/// logs carry the instance, request paths and NinjaOne's error bodies. Elsewhere the
/// directory inherits the user profile's ACL, which already is owner-only.
fn prepare_log_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        // `mode` only reaches directories this call creates.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// The log files are 0644 whatever we do, so the directory must not let other
    /// users reach them — whether this run creates it or an earlier one left it 0755.
    #[test]
    fn the_log_directory_is_owner_only() {
        let root = std::env::temp_dir().join(format!("npt-log-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let fresh = root.join("app").join("logs");
        super::prepare_log_dir(&fresh).expect("create");
        assert_eq!(mode(&fresh), 0o700);

        let earlier = root.join("earlier-logs");
        std::fs::create_dir_all(&earlier).unwrap();
        std::fs::set_permissions(&earlier, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::prepare_log_dir(&earlier).expect("tighten");
        assert_eq!(mode(&earlier), 0o700);

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();

    let app_state = AppState::new().expect("failed to initialize application state");
    let window_saver = Arc::new(window_state::WindowStateSaver::default());

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(app_state)
        // The main window is created hidden (`tauri.conf.json`) so the remembered
        // size and position apply before its first frame instead of visibly
        // jumping there. `show` runs whatever `restore` managed — a window that
        // stays hidden would be a far worse failure than one at the default size.
        .setup(|app| {
            // Off the critical path: reads the disk copy, then refreshes in the
            // background if it is due, so the first query rarely waits on nothing.
            let state = app.state::<AppState>();
            state
                .exploits
                .ensure_fresh(state.settings_snapshot().exploit_data);
            if let Some(window) = app.get_webview_window(window_state::MAIN_WINDOW) {
                window_state::restore(&window);
                window.show()?;
            }
            Ok(())
        })
        .on_window_event(move |window, event| window_saver.on_event(window, event))
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
            commands::lookups::list_device_groups,
            commands::live_jobs::list_running_jobs,
            commands::lookups::list_node_classes,
            commands::patches::query_patches,
            commands::patches::get_patch_rows,
            commands::patches::get_patch_groups,
            commands::patches::get_patch_group_members,
            commands::patches::selection_status,
            commands::patches::get_reboot_devices,
            commands::patches::device_detail,
            commands::export::export_patches_xlsx,
            commands::export::export_report_html,
            commands::export::export_csv,
            commands::exploits::find_cve,
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
