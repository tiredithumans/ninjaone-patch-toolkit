use chrono::Utc;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::error::UiError;
use crate::export::write_workbook;
use crate::rows::QueryResult;
use crate::state::AppState;

/// Clones the cached query result out of `state`, or errors if no query has run.
/// The lock is taken and released synchronously — never held across the blocking
/// save dialogs below.
fn cached_result(state: &AppState) -> Result<QueryResult, UiError> {
    state
        .last_result
        .lock()
        .map_err(|_| UiError::new("result cache poisoned"))?
        .clone()
        .ok_or_else(|| UiError::new("Run a query before exporting."))
}

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
    // Cheap precondition before the dialog; the full clone waits until the
    // operator has committed to a path (a cancelled dialog costs nothing).
    cached_result(&state)?;

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

    // The clone is owned, so the reboot subset is a move-filter, not a re-clone.
    let QueryResult {
        rows,
        devices,
        compliance,
        compliance_by_os,
        failures,
        ..
    } = cached_result(&state)?;
    let reboot: Vec<_> = devices.into_iter().filter(|d| d.needs_reboot).collect();

    write_workbook(
        &path_str,
        &rows,
        &compliance,
        &compliance_by_os,
        &reboot,
        &failures,
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
    // Same clone-after-dialog flow as the Excel export above.
    cached_result(&state)?;

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

    let result = cached_result(&state)?;
    std::fs::write(&path, crate::report::render_report(&result))
        .map_err(|e| UiError::new(format!("write report: {e}")))?;
    Ok(Some(path_str))
}
