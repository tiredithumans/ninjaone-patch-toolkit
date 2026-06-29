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

    write_workbook(
        &path_str,
        &result.rows,
        &result.compliance,
        &result.compliance_by_os,
        &reboot,
        &result.failures,
    )
    .map_err(UiError::from)?;
    Ok(Some(path_str))
}

/// Opens a save dialog and writes the most recent query result as a self-contained
/// HTML executive report (compliance/severity/age charts + failure & reboot tables)
/// that the operator can print to PDF from a browser. Returns the saved path, or
/// `None` if the operator cancelled the dialog.
///
/// Reads the same cached `QueryResult` the Excel export does — the single source of
/// truth — so it likewise requires a prior successful query. `async` for the same
/// off-main-thread reason as `export_patches_xlsx`.
#[tauri::command]
pub async fn export_report_html(
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
        "ninjaone-report-{}.html",
        Utc::now().format("%Y%m%dT%H%M%S")
    );

    let Some(file) = app
        .dialog()
        .file()
        .add_filter("HTML Report", &["html"])
        .set_file_name(&default_name)
        .blocking_save_file()
    else {
        return Ok(None);
    };

    let path = file
        .into_path()
        .map_err(|e| UiError::new(format!("invalid save path: {e}")))?;
    let path_str = path.to_string_lossy().to_string();

    std::fs::write(&path, crate::report::render_report(&result))
        .map_err(|e| UiError::new(format!("write report: {e}")))?;
    Ok(Some(path_str))
}
