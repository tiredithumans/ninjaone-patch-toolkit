use std::sync::Arc;

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

/// Takes a handle on the cached query result for the current tenant, or errors if no
/// query has run for it (a tenant switch invalidates the previous one).
///
/// An `Arc` bump, not a copy. This used to `clone()` the entire result — every row,
/// with each of its shared `Arc<str>` fields refcounted — *while holding* the result
/// mutex, which is the same mutex the three paging commands take. A six-figure fleet
/// therefore froze the table for the length of a full deep copy on every export. The
/// lock is still taken and released synchronously here, never held across the
/// blocking save dialogs below.
fn cached_result(state: &AppState) -> Result<Arc<QueryResult>, UiError> {
    state
        .current_result_handle()
        .map_err(|_| UiError::new("result cache poisoned"))?
        .ok_or_else(|| UiError::new("Run a query before exporting."))
}

/// Restricts an exported file to its owner, matching what the action audit log
/// already does and for the same reason.
///
/// A workbook or report carries the same category of data the audit log's own
/// comment calls out — device names, organizations, compliance posture for a whole
/// fleet — but they were written at the default umask, so on a shared or
/// roaming-profile machine they landed group/world-readable. Applied after the write
/// rather than through `OpenOptions`, because `rust_xlsxwriter` owns its own file
/// handle.
///
/// Best-effort: a failure here means the export still succeeded, and refusing to
/// hand the operator the file they asked for because its mode could not be narrowed
/// would be the wrong trade. Non-unix targets have no equivalent, so this is a no-op
/// there — Windows inherits the parent directory's ACL, which is already per-user
/// under the profile.
fn restrict_to_owner(path: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(
                ?err,
                path,
                "could not restrict the exported file to its owner"
            );
        }
    }
    #[cfg(not(unix))]
    let _ = path;
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
/// handle a cancel.
///
/// Run on the blocking pool rather than inline. A Tauri `async` command runs on the
/// tokio runtime, so `blocking_save_file` — which parks until the operator picks a
/// file, potentially for minutes — was occupying a tokio *worker* thread the whole
/// time, not just the calling task. `async` moves the work off the UI thread (which
/// the dialog needs free to pump its event loop); `spawn_blocking` is what keeps it
/// off the async runtime's workers, where it would otherwise stall unrelated IPC
/// commands and the job poller.
async fn save_dialog(
    app: &tauri::AppHandle,
    filter_label: &'static str,
    ext: &'static str,
    stem: &'static str,
) -> Result<Option<std::path::PathBuf>, UiError> {
    let app = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter(filter_label, &[ext])
            .set_file_name(default_name(stem, ext))
            .blocking_save_file()
    })
    .await
    .map_err(|e| UiError::new(format!("save dialog failed: {e}")))?;

    let Some(file) = picked else {
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

    let Some(path) = save_dialog(&app, "Excel Workbook", "xlsx", "ninjaone-patches").await? else {
        return Ok(None);
    };
    let path_str = path.to_string_lossy().to_string();

    // A handle on the cached result, not a copy of it. The whole thing moves into the
    // blocking task and the sheets borrow out of it there, so the only allocation on
    // this path is the reboot subset — which is a filtered projection either way.
    let result = cached_result(&state)?;
    let scope_note =
        crate::rows::compliance_scope_note(result.devices_offline, result.patch_families);

    // Serializing a six-figure row set into a zipped workbook is seconds of pure CPU
    // plus the file write — both of which would hold a tokio worker for the duration.
    let written = path_str.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let reboot: Vec<_> = result
            .devices
            .iter()
            .filter(|d| d.needs_reboot)
            .cloned()
            .collect();
        write_workbook(
            &written,
            &result.rows,
            &result.compliance,
            &result.compliance_by_os,
            &reboot,
            &result.failures,
            &scope_note,
        )
    })
    .await
    .map_err(|e| UiError::new(format!("export task failed: {e}")))?
    .map_err(UiError::from)?;
    restrict_to_owner(&path_str);
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

    let Some(path) = save_dialog(&app, "HTML Report", "html", "ninjaone-report").await? else {
        return Ok(None);
    };
    let path_str = path.to_string_lossy().to_string();

    let result = cached_result(&state)?;
    // Same reason as the workbook: rendering the report walks every rollup and
    // builds one large string, then writes it — CPU and file I/O, not async work.
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::write(&path, crate::report::render_report(&result))
    })
    .await
    .map_err(|e| UiError::new(format!("report task failed: {e}")))?
    .map_err(|e| UiError::new(format!("write report: {e}")))?;
    restrict_to_owner(&path_str);
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
    /// An export carries a whole fleet's device names, organizations and compliance
    /// posture — the same category the audit log sets 0600 for, with a comment about
    /// roaming profiles. These were written at the default umask.
    #[cfg(unix)]
    #[test]
    fn an_exported_file_is_restricted_to_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::env::temp_dir().join(format!("njp-export-{}.xlsx", std::process::id()));
        let path_str = path.to_string_lossy().to_string();
        std::fs::write(&path, b"not really a workbook").expect("seed the file");

        restrict_to_owner(&path_str);

        let mode = std::fs::metadata(&path)
            .expect("exists")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "exports must not be group/world readable"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Best-effort by design: an export that succeeded must not be reported as failed
    /// because its mode could not be narrowed.
    #[test]
    fn restricting_a_missing_file_is_not_fatal() {
        restrict_to_owner("/definitely/not/a/real/path/export.xlsx");
    }

    /// Both exports read the same cache, and both must refuse rather than write an
    /// empty file when no query has run. `require_cached_result` is the probe that
    /// runs *before* the save dialog, so the operator is not asked to pick a
    /// destination for a file that cannot be produced.
    #[test]
    fn exporting_before_any_query_is_refused() {
        let state = AppState::new().expect("build state");

        let err = require_cached_result(&state).expect_err("nothing has been queried yet");
        assert!(
            err.message.contains("Run a query"),
            "the message must say what to do: {}",
            err.message
        );
        assert!(
            cached_result(&state).is_err(),
            "and the handle path must refuse too, not hand back an empty result"
        );
    }

    /// With a result cached for this tenant, both paths succeed — the refusal above
    /// must be about the empty cache and nothing else.
    #[test]
    fn exporting_after_a_query_finds_the_cached_result() {
        let state = AppState::new().expect("build state");
        state.store_last_result_if_current(state.begin_query(), sample_result());

        require_cached_result(&state).expect("a cached result satisfies the probe");
        let handle = cached_result(&state).expect("and the handle resolves");
        assert_eq!(handle.rows.len(), 0);
    }

    fn sample_result() -> QueryResult {
        QueryResult {
            rows: Vec::new(),
            devices: Vec::new(),
            compliance: Vec::new(),
            compliance_by_os: Vec::new(),
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            devices_total: 0,
            devices_offline: 0,
            patch_families: crate::rows::PatchFamilies {
                os: true,
                software: true,
            },
            generated_at: "2026-01-01 00:00:00 UTC".into(),
            data_fetched_at: "2026-01-01 00:00:00 UTC".into(),
        }
    }
}
