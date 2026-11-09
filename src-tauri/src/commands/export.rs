use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::error::UiError;
use crate::export::{BacklogSheets, DETAIL_COLUMNS, WorkbookMeta, write_workbook};
use crate::rows::QueryResult;
use crate::state::AppState;

/// Errors unless a query result is cached for the current tenant. The probe returns
/// `()` from inside the lock, so it costs nothing — the previous version cloned the
/// entire `QueryResult` just to discover it existed, which on a 10k-row fleet meant
/// two full deep copies per export (one thrown away immediately).
fn require_cached_result(state: &AppState) -> Result<(), UiError> {
    state
        .with_current_result(|_| ())
        .map_err(UiError::from)?
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
        .map_err(UiError::from)?
        .ok_or_else(|| UiError::new("Run a query before exporting."))
}

/// Writes an export to `path` through `write`, on a file restricted to its owner
/// before the first byte, and removes the file if the write fails.
///
/// A workbook or report carries the same category of data the action audit log's
/// own comment calls out — device names, organizations, compliance posture for a
/// whole fleet — and is restricted to its owner for the same reason. Exports used to
/// be created at the default umask and narrowed only once written, so for the length
/// of the write — seconds, for a six-figure workbook — the file sat group- and
/// world-readable on a shared or roaming-profile machine; the workbook went through
/// `rust_xlsxwriter`'s own file handle, which left nothing else to do. It is now
/// written through `save_to_writer` into the handle opened here.
///
/// A failed write leaves no half-written file to be mistaken for an export.
fn write_export(
    path: &Path,
    write: impl FnOnce(&mut File) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut file = create_owner_only(path)?;
    let written = write(&mut file);
    if written.is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    written
}

/// Opens `path` for writing, truncated, readable by its owner only.
///
/// `mode` applies only to a file this call creates. One the operator chose to
/// overwrite keeps its old mode, so it is narrowed through the handle, still before
/// anything is written — best-effort, like the narrowing this replaced: a file
/// another user owns cannot be, and refusing the export over its mode would be the
/// wrong trade. Non-unix targets have no equivalent; Windows inherits the parent
/// directory's ACL, which is already per-user under the profile.
fn create_owner_only(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        options.mode(0o600);
        let file = options.open(path)?;
        if let Err(err) = file.set_permissions(std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(
                ?err,
                path = %path.display(),
                "could not restrict the exported file to its owner"
            );
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    options.open(path)
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
    file_name: String,
) -> Result<Option<std::path::PathBuf>, UiError> {
    let app = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter(filter_label, &[ext])
            .set_file_name(file_name)
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

    let name = default_name("ninjaone-patches", "xlsx");
    let Some(path) = save_dialog(&app, "Excel Workbook", "xlsx", name).await? else {
        return Ok(None);
    };
    let path_str = path.to_string_lossy().to_string();

    // A handle on the cached result, not a copy of it. The whole thing moves into the
    // blocking task and the sheets borrow out of it there.
    let result = cached_result(&state)?;
    let scope_note = crate::rows::compliance_scope_note(
        result.devices_offline,
        result.devices_unpatchable,
        result.patch_families,
    );

    // Serializing a six-figure row set into a zipped workbook is seconds of pure CPU
    // plus the file write — both of which would hold a tokio worker for the duration.
    tauri::async_runtime::spawn_blocking(move || {
        write_export(&path, |file| {
            write_workbook(
                file,
                &result.rows,
                &result.compliance,
                &result.compliance_by_os,
                &result.devices,
                &result.failures,
                &BacklogSheets {
                    worst_devices: &result.worst_devices,
                    offline_backlog: &result.offline_backlog,
                    time_to_install: &result.time_to_install,
                },
                &result.approvals,
                &WorkbookMeta {
                    generated_at: &result.generated_at,
                    data_fetched_at: &result.data_fetched_at,
                    devices_total: result.devices_total,
                    devices_offline: result.devices_offline,
                    devices_unpatchable: result.devices_unpatchable,
                    scope: &result.scope,
                    scope_note: &scope_note,
                    changes: &result.changes,
                    instance: &result.instance,
                    sla_policy: &result.sla_policy,
                    compliance_total: &result.compliance_total,
                    uncounted_orgs: &result.uncounted_orgs,
                    first_seen: &result.first_seen,
                    patch_families: result.patch_families,
                    exploit_data: &result.exploit_data.sentence,
                },
            )
        })
    })
    .await
    .map_err(|e| UiError::new(format!("export task failed: {e}")))?
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

    let name = default_name("ninjaone-report", "html");
    let Some(path) = save_dialog(&app, "HTML Report", "html", name).await? else {
        return Ok(None);
    };
    let path_str = path.to_string_lossy().to_string();

    let result = cached_result(&state)?;
    // Same reason as the workbook: rendering the report walks every rollup and
    // builds one large string, then writes it — CPU and file I/O, not async work.
    tauri::async_runtime::spawn_blocking(move || {
        write_export(&path, |file| {
            file.write_all(crate::report::render_report(&result).as_bytes())?;
            Ok(())
        })
    })
    .await
    .map_err(|e| UiError::new(format!("report task failed: {e}")))?
    .map_err(|e| UiError::new(format!("write report: {e:#}")))?;
    Ok(Some(path_str))
}

/// Opens a save dialog and writes the cached detail rows — the Patches sheet's
/// columns, every row, no row limit — as CSV (see [`crate::csv_export`]). Returns
/// the saved path, or `None` if the operator cancelled.
///
/// Same probe → dialog → handle → blocking-write shape as the other two exports.
/// A CSV cannot carry the About sheet's provenance, so the proposed file name
/// states the device scope, the status selection and both clocks
/// ([`csv_file_name`]).
#[tauri::command]
pub async fn export_csv(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, UiError> {
    require_cached_result(&state)?;
    // The name is read off the result the probe just found. A re-query landing
    // between here and the write below is the same race the other exports accept:
    // the file then holds the newer rows under a name stamped with the older clocks.
    let name = state
        .with_current_result(csv_file_name)
        .map_err(UiError::from)?
        .ok_or_else(|| UiError::new("Run a query before exporting."))?;

    let Some(path) = save_dialog(&app, "CSV (comma-separated)", "csv", name).await? else {
        return Ok(None);
    };
    let path_str = path.to_string_lossy().to_string();

    let result = cached_result(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        write_export(&path, |file| {
            let mut out = std::io::BufWriter::new(file);
            crate::csv_export::write_csv(&mut out, &DETAIL_COLUMNS, &result.rows)?;
            // Explicitly: a flush that fails on drop is silently lost.
            out.flush()?;
            Ok(())
        })
    })
    .await
    .map_err(|e| UiError::new(format!("CSV export task failed: {e}")))?
    .map_err(|e| UiError::new(format!("write CSV: {e:#}")))?;
    Ok(Some(path_str))
}

/// The CSV's proposed file name, which is the only place a CSV can say what it
/// holds: `ninjaone-patches_<device scope>_<statuses>_data-<fetched>_generated-<generated>.csv`,
/// e.g. `ninjaone-patches_whole-fleet_pending_data-20260502T0840Z_generated-20260502T0915Z.csv`.
///
/// The device scope is `whole-fleet` or the selected device facets' values; the
/// patch filters other than Status (severity, search, date windows) are not in it —
/// a file name has no room for them, and the workbook's About sheet has. Both clocks
/// are UTC, compacted to `YYYYMMDDTHHMMZ`.
fn csv_file_name(result: &QueryResult) -> String {
    let scope = if result.scope.device_scoped {
        let values: Vec<&str> = result
            .scope
            .facets
            .iter()
            .filter(|(label, _)| !matches!(*label, "Scope" | "Patch type"))
            .map(|(_, value)| value.as_str())
            .collect();
        Some(slug(&values.join(" "))).filter(|s| !s.is_empty())
    } else {
        Some("whole-fleet".to_string())
    };
    let statuses = result
        .scope
        .patch_facets
        .iter()
        .find(|(label, _)| *label == "Status")
        .map(|(_, value)| slug(value))
        .filter(|s| !s.is_empty());
    let mut parts = vec!["ninjaone-patches".to_string()];
    parts.push(scope.unwrap_or_else(|| "scoped".to_string()));
    parts.extend(statuses);
    parts.push(format!("data-{}", compact_clock(&result.data_fetched_at)));
    parts.push(format!("generated-{}", compact_clock(&result.generated_at)));
    format!("{}.csv", parts.join("_"))
}

/// Lowercase ASCII letters and digits, every other run of characters collapsed to
/// one `-`, capped at [`SLUG_MAX`] — safe on every filesystem the app ships to. A
/// name made only of characters outside ASCII slugs to empty; the caller falls back.
fn slug(s: &str) -> String {
    const SLUG_MAX: usize = 40;
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(SLUG_MAX);
    out.trim_end_matches('-').to_string()
}

/// `2026-05-02 08:40:00 UTC` → `20260502T0840Z`. A clock that does not parse keeps
/// only its digits, so the name still sorts and never carries a path separator.
fn compact_clock(clock: &str) -> String {
    match crate::export::clock_timestamp(clock)
        .and_then(|ts| chrono::DateTime::<Utc>::from_timestamp(ts, 0))
    {
        Some(when) => when.format("%Y%m%dT%H%MZ").to_string(),
        None => clock.chars().filter(char::is_ascii_digit).collect(),
    }
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
    /// A CSV has nowhere to state its provenance but its name: the device scope,
    /// the status selection and both clocks, all filesystem-safe.
    #[test]
    fn the_csv_name_states_scope_statuses_and_both_clocks() {
        let mut result = sample_result();
        result.generated_at = "2026-05-02 09:15:00 UTC".into();
        result.data_fetched_at = "2026-05-02 08:40:00 UTC".into();
        result.scope = crate::rows::QueryScope {
            facets: vec![
                ("Scope", "Whole fleet — no device filters applied".into()),
                ("Patch type", "OS patches only".into()),
            ],
            patch_facets: vec![("Status", "Pending, Failed".into())],
            ..Default::default()
        };
        assert_eq!(
            csv_file_name(&result),
            "ninjaone-patches_whole-fleet_pending-failed_data-20260502T0840Z_generated-20260502T0915Z.csv"
        );

        result.scope = crate::rows::QueryScope {
            facets: vec![
                ("Organizations", "Contoso Ltd, Zürich AG".into()),
                ("OS type", "WINDOWS_SERVER".into()),
                ("Patch type", "OS patches only".into()),
            ],
            patch_facets: vec![("Status", "Pending".into())],
            device_scoped: true,
            ..Default::default()
        };
        assert_eq!(
            csv_file_name(&result),
            "ninjaone-patches_contoso-ltd-z-rich-ag-windows-server_pending_data-20260502T0840Z_generated-20260502T0915Z.csv"
        );
    }

    #[test]
    fn slugs_are_short_ascii_and_never_empty_by_accident() {
        assert_eq!(slug("  Contoso / HQ — Seattle  "), "contoso-hq-seattle");
        assert_eq!(slug("../../etc"), "etc");
        assert_eq!(slug("日本"), "", "the caller falls back to a fixed word");
        let long = slug(&"a b ".repeat(40));
        assert!(long.len() <= 40 && !long.ends_with('-'), "{long}");
        // A clock that does not parse keeps its digits and loses any separator.
        assert_eq!(compact_clock("2026/05/02 08:40"), "202605020840");
    }

    fn export_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("njp-export-{}-{tag}", std::process::id()))
    }

    /// The mode of the file behind `file`, as the write sees it.
    #[cfg(unix)]
    fn mode_of(file: &File) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        file.metadata().expect("metadata").permissions().mode() & 0o777
    }

    /// An export carries a whole fleet's device names, organizations and compliance
    /// posture — the same category the audit log sets 0600 for, with a comment about
    /// roaming profiles. It used to be created at the default umask and narrowed only
    /// once written; it is owner-only before its first byte now.
    #[cfg(unix)]
    #[test]
    fn an_export_is_owner_only_before_its_first_byte() {
        let path = export_path("new.csv");
        let _ = std::fs::remove_file(&path);
        write_export(&path, |file| {
            assert_eq!(mode_of(file), 0o600, "narrowed before anything is written");
            file.write_all(b"device,kb\r\n")?;
            Ok(())
        })
        .expect("written");
        assert_eq!(std::fs::read(&path).unwrap(), b"device,kb\r\n");
        let _ = std::fs::remove_file(&path);
    }

    /// `mode` covers only a file the open creates. One the operator chose to
    /// overwrite is narrowed through the handle before the write, and its old
    /// contents are gone.
    #[cfg(unix)]
    #[test]
    fn an_overwritten_export_is_narrowed_before_it_is_written() {
        use std::os::unix::fs::PermissionsExt as _;
        let path = export_path("old.html");
        std::fs::write(&path, b"last week's much longer report").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_export(&path, |file| {
            assert_eq!(mode_of(file), 0o600, "narrowed before anything is written");
            file.write_all(b"<html>")?;
            Ok(())
        })
        .expect("written");
        assert_eq!(std::fs::read(&path).unwrap(), b"<html>");
        let _ = std::fs::remove_file(&path);
    }

    /// A write that fails leaves nothing behind to be mistaken for an export, and
    /// the failure is what the operator is told.
    #[test]
    fn a_failed_export_leaves_no_partial_file() {
        let path = export_path("partial.xlsx");
        let err = write_export(&path, |file| {
            file.write_all(b"PK half a zip")?;
            anyhow::bail!("disk full")
        })
        .expect_err("the write failed");
        assert_eq!(err.to_string(), "disk full");
        assert!(!path.exists(), "the partial file is removed");
    }

    /// The workbook goes through the same handle as the CSV and the report, so it is
    /// owner-only too, and still a workbook.
    #[cfg(unix)]
    #[test]
    fn the_workbook_is_written_through_the_owner_only_handle() {
        use calamine::{Reader, Xlsx, open_workbook};
        use std::os::unix::fs::PermissionsExt as _;
        let path = export_path("book.xlsx");
        let result = sample_result();
        write_export(&path, |file| {
            write_workbook(
                file,
                &result.rows,
                &result.compliance,
                &result.compliance_by_os,
                &result.devices,
                &result.failures,
                &BacklogSheets {
                    worst_devices: &result.worst_devices,
                    offline_backlog: &result.offline_backlog,
                    time_to_install: &result.time_to_install,
                },
                &result.approvals,
                &WorkbookMeta {
                    generated_at: &result.generated_at,
                    data_fetched_at: &result.data_fetched_at,
                    devices_total: result.devices_total,
                    devices_offline: result.devices_offline,
                    devices_unpatchable: result.devices_unpatchable,
                    scope: &result.scope,
                    scope_note: "scope",
                    changes: &result.changes,
                    instance: &result.instance,
                    sla_policy: &result.sla_policy,
                    compliance_total: &result.compliance_total,
                    uncounted_orgs: &result.uncounted_orgs,
                    first_seen: &result.first_seen,
                    patch_families: result.patch_families,
                    exploit_data: &result.exploit_data.sentence,
                },
            )
        })
        .expect("written");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let mut wb: Xlsx<_> = open_workbook(&path).expect("a workbook");
        assert!(wb.worksheet_range("About").is_ok());
        let _ = std::fs::remove_file(&path);
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
            compliance_total: Default::default(),
            uncounted_orgs: Vec::new(),
            compliance_by_os: Vec::new(),
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            worst_devices: Default::default(),
            offline_backlog: Default::default(),
            time_to_install: Default::default(),
            first_seen: Default::default(),
            sla_policy: Default::default(),
            instance: "https://app.ninjarmm.com".into(),
            approvals: Default::default(),
            devices_total: 0,
            devices_offline: 0,
            devices_unpatchable: 0,
            patch_families: crate::rows::PatchFamilies {
                os: true,
                software: true,
            },
            scope: Default::default(),
            changes: Default::default(),
            generated_at: "2026-01-01 00:00:00 UTC".into(),
            data_fetched_at: "2026-01-01 00:00:00 UTC".into(),
            exploit_data: Default::default(),
        }
    }
}
