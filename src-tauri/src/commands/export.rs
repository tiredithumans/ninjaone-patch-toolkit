use chrono::Utc;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::error::UiError;
use crate::export::write_workbook;
use crate::state::AppState;

/// Opens a save dialog and writes the most recent query result to an `.xlsx`
/// workbook (Patches + Compliance + Needs Reboot sheets). Returns the saved path,
/// or `None` if the operator cancelled the dialog.
///
/// Declared `async` so it runs off the main thread, which `blocking_save_file`
/// requires (the dialog needs the main thread free to pump its event loop).
#[tauri::command]
pub async fn export_patches_xlsx(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, UiError> {
    let result = state
        .last_result
        .lock()
        .map_err(|_| UiError::new("result cache poisoned"))?
        .clone();
    let Some(result) = result else {
        return Err(UiError::new("Run a query before exporting."));
    };

    let default_name = format!(
        "ninjaone-patches-{}.xlsx",
        Utc::now().format("%Y%m%dT%H%M%S")
    );

    let Some(file) = app
        .dialog()
        .file()
        .add_filter("Excel Workbook", &["xlsx"])
        .set_file_name(&default_name)
        .blocking_save_file()
    else {
        return Ok(None);
    };

    let path = file
        .into_path()
        .map_err(|e| UiError::new(format!("invalid save path: {e}")))?;
    let path_str = path.to_string_lossy().to_string();

    let reboot: Vec<_> = result
        .devices
        .iter()
        .filter(|d| d.needs_reboot)
        .cloned()
        .collect();

    write_workbook(&path_str, &result.rows, &result.compliance, &reboot).map_err(UiError::from)?;
    Ok(Some(path_str))
}
