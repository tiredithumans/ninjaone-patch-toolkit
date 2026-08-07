use chrono::Utc;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::error::UiError;
use crate::export::write_workbook;
use crate::rows::QueryResult;
use crate::state::AppState;

/// Errors unless a query result is cached for the current tenant. The probe returns
/// `()` from inside the lock, so it costs nothing — the previous version cloned the
/// entire `QueryResult` just to discover it existed, which on a 10k-row fleet meant
/// two full deep copies per export (one thrown away immediately).
fn require_cached_result(state: &AppState) -> Result<(), UiError> {
    state
        .with_current_result(|_| ())
        .map_err(|_| UiError::new("result cache poisoned"))?
        .ok_or_else(|| UiError::new("Run a query before exporting."))
}

/// Clones the cached query result for the current tenant, or errors if no query has
/// run for it (a tenant switch invalidates the previous one). The lock is taken and
/// released synchronously inside `with_current_result` — never held across the
/// blocking save dialogs below.
fn cached_result(state: &AppState) -> Result<QueryResult, UiError> {
    state
        .with_current_result(|r| r.clone())
        .map_err(|_| UiError::new("result cache poisoned"))?
        .ok_or_else(|| UiError::new("Run a query before exporting."))
}

/// The default file name for an export, stamped so successive exports don't
/// silently overwrite each other.
fn default_name(stem: &str, ext: &str) -> String {
    format!("{stem}-{}.{ext}", Utc::now().format("%Y%m%dT%H%M%S"))
}

/// Runs the save dialog and returns the chosen path, or `None` if the operator
/// cancelled.
///
/// Both exports open the same dialog with the same filter/name/extract sequence;
/// keeping it in one place means the two cannot drift in how they name files or
/// handle a cancel. `blocking_save_file` needs the main thread free to pump its
/// event loop, which is why both callers are `async`.
fn save_dialog(
    app: &tauri::AppHandle,
    filter_label: &str,
    ext: &str,
    stem: &str,
) -> Result<Option<std::path::PathBuf>, UiError> {
    let Some(file) = app
        .dialog()
        .file()
        .add_filter(filter_label, &[ext])
        .set_file_name(default_name(stem, ext))
        .blocking_save_file()
    else {
        return Ok(None);
    };
    file.into_path()
        .map(Some)
        .map_err(|e| UiError::new(format!("invalid save path: {e}")))
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
    // Fail before opening the dialog rather than after the operator picks a path.
    // This only probes for presence; the one clone happens below, once the save is
    // committed (a cancelled dialog copies nothing).
    require_cached_result(&state)?;

    let Some(path) = save_dialog(&app, "Excel Workbook", "xlsx", "ninjaone-patches")? else {
        return Ok(None);
    };
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
    // Same probe-then-clone-after-dialog flow as the Excel export above.
    require_cached_result(&state)?;

    let Some(path) = save_dialog(&app, "HTML Report", "html", "ninjaone-report")? else {
        return Ok(None);
    };
    let path_str = path.to_string_lossy().to_string();

    let result = cached_result(&state)?;
    std::fs::write(&path, crate::report::render_report(&result))
        .map_err(|e| UiError::new(format!("write report: {e}")))?;
    Ok(Some(path_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both exports stamp their default name, so a second export in the same
    /// session proposes a new file instead of silently overwriting the first.
    #[test]
    fn default_names_carry_the_stem_extension_and_a_timestamp() {
        let xlsx = default_name("ninjaone-patches", "xlsx");
        assert!(xlsx.starts_with("ninjaone-patches-"), "{xlsx}");
        assert!(xlsx.ends_with(".xlsx"), "{xlsx}");
        // stem + '-' + %Y%m%dT%H%M%S (15 chars) + ".xlsx"
        assert_eq!(xlsx.len(), "ninjaone-patches-".len() + 15 + ".xlsx".len());

        let html = default_name("ninjaone-report", "html");
        assert!(
            html.starts_with("ninjaone-report-") && html.ends_with(".html"),
            "{html}"
        );
    }
}
