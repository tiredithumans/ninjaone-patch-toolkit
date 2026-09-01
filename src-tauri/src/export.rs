use anyhow::{Context, Result};
use rust_xlsxwriter::{Color, Format, Workbook};

use crate::model::PatchRow;
use crate::rows::{
    ComplianceBucket, DeviceSummary, FailureGroup, OsCompliance, QueryScope, TableCell, TableColumn,
};

/// The Patches detail-sheet columns. Only the workbook renders this table, so it
/// lives here rather than on `PatchRow` — but it is declared the same way as the
/// shared ones so header and value stay a single declaration.
const DETAIL_COLUMNS: [TableColumn<PatchRow>; 15] = [
    ("Organization", |r| TableCell::text(&r.organization)),
    ("Location", |r| TableCell::opt_text(r.location.as_deref())),
    ("Device Role", |r| {
        TableCell::opt_text(r.device_role.as_deref())
    }),
    ("Device", |r| TableCell::text(&r.device_name)),
    ("OS", |r| TableCell::opt_text(r.os_name.as_deref())),
    ("Node Class", |r| {
        TableCell::opt_text(r.node_class.as_deref())
    }),
    ("Patch Type", |r| TableCell::text(r.patch_type)),
    ("KB", |r| TableCell::opt_text(r.kb.as_deref())),
    ("Patch", |r| TableCell::text(&r.name)),
    ("Severity", |r| TableCell::text(r.severity)),
    ("Status", |r| TableCell::text(&r.status)),
    ("Needs Reboot", |r| {
        TableCell::text(if r.needs_reboot { "Yes" } else { "No" })
    }),
    // The flag the compliance sheets' scope note talks about. `PatchRow` has
    // carried it all along and the in-app table draws an "offline" chip from it,
    // but the workbook dropped it — so a sheet asserting "N offline devices
    // excluded" gave the reader no way to tell which rows those were, and no way
    // to reproduce the compliance denominator by hand.
    ("Offline", |r| {
        TableCell::text(if r.offline { "Yes" } else { "No" })
    }),
    ("First Seen", |r| {
        TableCell::opt_text(r.first_seen_date.as_deref())
    }),
    ("Installed Date", |r| {
        TableCell::opt_text(r.installed_date.as_deref())
    }),
];

// Column widths, positionally paired with each sheet's column table. Tying every
// array's length to `COLUMNS.len()` makes a new column without a width a compile
// error — previously the widths were an unchecked `&[f64]` literal passed inline,
// so a column added at one site and not the other silently misaligned the sheet.
const DETAIL_WIDTHS: [f64; DETAIL_COLUMNS.len()] = [
    24.0, 18.0, 18.0, 22.0, 26.0, 18.0, 11.0, 12.0, 40.0, 11.0, 11.0, 13.0, 9.0, 20.0, 20.0,
];
const SUMMARY_WIDTHS: [f64; ComplianceBucket::COLUMNS.len()] = [28.0, 10.0, 11.0, 14.0, 24.0, 16.0];
const OS_SUMMARY_WIDTHS: [f64; OsCompliance::COLUMNS.len()] = [28.0, 10.0, 11.0, 14.0, 24.0, 16.0];
const REBOOT_WIDTHS: [f64; DeviceSummary::COLUMNS.len()] = [24.0, 18.0, 18.0, 22.0, 26.0, 14.0];
const FAILURE_WIDTHS: [f64; FailureGroup::COLUMNS.len()] =
    [11.0, 11.0, 12.0, 40.0, 16.0, 20.0, 60.0];

/// Column widths for the About sheet's label/value pair.
const ABOUT_WIDTHS: [f64; 2] = [24.0, 64.0];

/// What the workbook's numbers describe and when they were taken.
///
/// Until this existed the workbook carried **no** timestamp at all — the only stamp
/// was in the suggested file name, which survives exactly one rename. Both clocks
/// are here because they differ: `generated_at` is when the join and rollups ran,
/// while `data_fetched_at` is when the underlying fleet data last came from
/// NinjaOne, and a re-filter recomputes over a warm cache without a round trip. The
/// in-app UI already says "patch data as of …" for this reason; a shared workbook
/// needs it more, not less.
pub struct WorkbookMeta<'a> {
    pub generated_at: &'a str,
    pub data_fetched_at: &'a str,
    pub devices_total: usize,
    pub devices_offline: usize,
    /// The facets the query ran under. Its `Patch type` entry is where the patch
    /// families are stated — they are not a separate row, because the Type facet and
    /// the rollups' family scope are the same value and two adjacent rows saying it
    /// twice read as two different things.
    pub scope: &'a QueryScope,
    /// The sentence `rows::compliance_scope_note` builds.
    pub scope_note: &'a str,
}

fn header_format() -> Format {
    Format::new()
        .set_bold()
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x1F2A37))
}

/// Writes a workbook with a Patches detail sheet (one row per device×patch), a
/// Compliance summary sheet, a Compliance by OS sheet, a Needs Reboot sheet for
/// devices flagged for reboot, a Patch Failures sheet rolling up FAILED installs,
/// and an About sheet carrying the provenance in [`WorkbookMeta`]. Data sheets with
/// no rows are omitted; Patches and About are always written.
pub fn write_workbook(
    path: &str,
    rows: &[PatchRow],
    compliance: &[ComplianceBucket],
    compliance_by_os: &[OsCompliance],
    reboot_devices: &[DeviceSummary],
    failures: &[FailureGroup],
    meta: &WorkbookMeta<'_>,
) -> Result<()> {
    let mut workbook = Workbook::new();
    let header = header_format();

    // The detail sheet is always written (even empty) so the workbook always opens
    // on the table the operator asked for; it is also the only sheet with an
    // autofilter, being the only one meant to be sliced by hand.
    write_sheet(
        &mut workbook,
        &header,
        "Patches",
        &DETAIL_COLUMNS,
        &DETAIL_WIDTHS,
        rows,
        true,
    )?;
    if !compliance.is_empty() {
        write_sheet(
            &mut workbook,
            &header,
            "Compliance",
            &ComplianceBucket::COLUMNS,
            &SUMMARY_WIDTHS,
            compliance,
            false,
        )?;
        // Stated on the sheet itself: a workbook outlives the session it came from,
        // and a bare "Compliance %" column says nothing about which devices and
        // which patch families produced it.
        write_footnote(&mut workbook, compliance.len(), meta.scope_note)?;
    }
    if !compliance_by_os.is_empty() {
        write_sheet(
            &mut workbook,
            &header,
            "Compliance by OS",
            &OsCompliance::COLUMNS,
            &OS_SUMMARY_WIDTHS,
            compliance_by_os,
            false,
        )?;
        write_footnote(&mut workbook, compliance_by_os.len(), meta.scope_note)?;
    }
    if !reboot_devices.is_empty() {
        write_sheet(
            &mut workbook,
            &header,
            "Needs Reboot",
            &DeviceSummary::COLUMNS,
            &REBOOT_WIDTHS,
            reboot_devices,
            false,
        )?;
    }
    if !failures.is_empty() {
        write_sheet(
            &mut workbook,
            &header,
            "Patch Failures",
            &FailureGroup::COLUMNS,
            &FAILURE_WIDTHS,
            failures,
            false,
        )?;
    }

    // Last, so the workbook still opens on the detail table the operator asked for
    // (Excel activates the first sheet), and so the provenance sits outside every
    // data range rather than trailing a sheet someone will sort or filter.
    write_about_sheet(&mut workbook, &header, meta, rows.len())?;

    workbook.save(path).context("save workbook")?;
    Ok(())
}

/// Writes the About sheet: a label/value list recording when the numbers were
/// computed, when the data behind them was fetched, and what population they cover.
fn write_about_sheet(
    workbook: &mut Workbook,
    header: &Format,
    meta: &WorkbookMeta<'_>,
    detail_rows: usize,
) -> Result<()> {
    let devices_total = meta.devices_total.to_string();
    let devices_offline = meta.devices_offline.to_string();
    let detail_rows = detail_rows.to_string();
    let entries: [(&str, &str); 5] = [
        ("Generated", meta.generated_at),
        ("Patch data fetched", meta.data_fetched_at),
        ("Devices in scope", &devices_total),
        ("Offline devices", &devices_offline),
        ("Detail rows", &detail_rows),
    ];

    let sheet = workbook.add_worksheet();
    sheet.set_name("About").context("name sheet")?;
    sheet
        .write_string_with_format(0, 0, "Field", header)
        .context("write header")?;
    sheet
        .write_string_with_format(0, 1, "Value", header)
        .context("write header")?;
    let mut row = 0u32;
    for (label, value) in &entries {
        row += 1;
        sheet.write_string(row, 0, *label)?;
        sheet.write_string(row, 1, *value)?;
    }

    // The facets, under their own banded heading. Without them two workbooks off the
    // same fleet — one scoped to a single org and CRITICAL-only, one unfiltered —
    // are indistinguishable once saved, and every number in both is a different
    // population.
    row += 2;
    sheet
        .write_string_with_format(row, 0, "Filters", header)
        .context("write header")?;
    sheet
        .write_string_with_format(row, 1, "Value", header)
        .context("write header")?;
    for (label, value) in &meta.scope.facets {
        row += 1;
        sheet.write_string(row, 0, *label)?;
        sheet.write_string(row, 1, value)?;
    }

    sheet.write_string(row + 2, 0, meta.scope_note)?;
    apply_widths(sheet, &ABOUT_WIDTHS)?;
    Ok(())
}

/// Writes a scope sentence one blank row under the last data row of the sheet just
/// added. Takes the row count rather than the sheet so it can run after
/// [`write_sheet`] has handed the worksheet back to the workbook.
fn write_footnote(workbook: &mut Workbook, data_rows: usize, note: &str) -> Result<()> {
    let last = workbook.worksheets().len() - 1;
    let sheet = workbook.worksheet_from_index(last)?;
    sheet
        .write_string((data_rows + 2) as u32, 0, note)
        .context("write scope note")?;
    Ok(())
}

/// Writes one sheet from a column table: headers, then every row's cells through
/// the same accessors that produced those headers.
///
/// One function for all five data sheets. They were five near-identical bodies, each
/// re-deriving the header loop, the per-cell writes and the width application by
/// hand — which is exactly how the failures table came to be headed one way in the
/// workbook and another in the report.
fn write_sheet<T>(
    workbook: &mut Workbook,
    header: &Format,
    name: &str,
    columns: &[TableColumn<T>],
    widths: &[f64],
    rows: &[T],
    autofilter: bool,
) -> Result<()> {
    let sheet = workbook.add_worksheet();
    sheet.set_name(name).context("name sheet")?;

    for (col, (title, _)) in columns.iter().enumerate() {
        sheet
            .write_string_with_format(0, col as u16, *title, header)
            .context("write header")?;
    }

    for (i, item) in rows.iter().enumerate() {
        let row = (i + 1) as u32;
        for (col, (_, value)) in columns.iter().enumerate() {
            let col = col as u16;
            match value(item) {
                TableCell::Text(s) => sheet.write_string(row, col, &s)?,
                TableCell::Count(n) => sheet.write_number(row, col, n as f64)?,
                TableCell::Number(n) => sheet.write_number(row, col, n)?,
            };
        }
    }

    sheet.set_freeze_panes(1, 0).context("freeze header")?;
    if autofilter {
        let last_row = rows.len() as u32; // header row 0 + data rows
        sheet
            .autofilter(0, 0, last_row.max(1), (columns.len() - 1) as u16)
            .context("autofilter")?;
    }
    apply_widths(sheet, widths)?;
    Ok(())
}

fn apply_widths(sheet: &mut rust_xlsxwriter::Worksheet, widths: &[f64]) -> Result<()> {
    for (col, w) in widths.iter().enumerate() {
        sheet
            .set_column_width(col as u16, *w)
            .context("set column width")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Stand-in for the scope sentence the command builds from the cached result.
    const NOTE: &str = "Compliance covers online devices only.";
    use super::*;

    /// The provenance block the command fills from the cached `QueryResult`. The two
    /// clocks differ on purpose here — a re-filter recomputes over a warm cache, so
    /// the About sheet has to show both rather than implying one.
    static SCOPE: std::sync::OnceLock<QueryScope> = std::sync::OnceLock::new();

    fn meta() -> WorkbookMeta<'static> {
        WorkbookMeta {
            generated_at: "2026-05-02 09:15:00 UTC",
            data_fetched_at: "2026-05-02 08:40:00 UTC",
            devices_total: 2,
            devices_offline: 1,
            scope: SCOPE.get_or_init(|| QueryScope {
                facets: vec![
                    ("Organizations", "Contoso".to_string()),
                    ("Patch type", "OS and third-party patches".to_string()),
                    ("Status", "Pending, Failed".to_string()),
                    ("Severity", "CRITICAL".to_string()),
                ],
            }),
            scope_note: NOTE,
        }
    }

    fn sample_row() -> PatchRow {
        PatchRow {
            device_id: 1,
            device_name: "srv01".into(),
            organization: "Contoso".into(),
            location: Some("HQ".into()),
            device_role: Some("DC".into()),
            os_name: Some("Windows Server 2022".into()),
            node_class: Some("WINDOWS_SERVER".into()),
            needs_reboot: true,
            offline: false,
            patch_type: "OS",
            kb: Some("KB5040434".into()),
            name: "Cumulative Update".into(),
            severity: "Critical",
            severity_rank: 5,
            status: "PENDING".into(),
            first_seen_date: Some("2026-05-01 00:00 UTC".into()),
            installed_date: None,
            first_seen_ts: Some(1_777_000_000),
            installed_ts: None,
        }
    }

    #[test]
    fn writes_readable_workbook_with_headers_and_rows() {
        let dir = std::env::temp_dir();
        let path = dir.join("npt-export-test.xlsx");
        let path_str = path.to_string_lossy().to_string();
        let rows = vec![sample_row()];
        let compliance = vec![ComplianceBucket {
            organization: "Contoso".into(),
            devices_total: 2,
            devices_compliant: 1,
            compliance_pct: 50.0,
            pending_critical: 3,
            aged_critical: 1,
        }];
        let compliance_by_os = vec![OsCompliance {
            os: "Windows Server 2022".into(),
            devices_total: 2,
            devices_compliant: 1,
            compliance_pct: 50.0,
            pending_critical: 3,
            aged_critical: 1,
        }];
        write_workbook(
            &path_str,
            &rows,
            &compliance,
            &compliance_by_os,
            &[],
            &[],
            &meta(),
        )
        .unwrap();

        // Read it back to prove it is a valid, populated workbook.
        use calamine::{Reader, Xlsx, open_workbook};
        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let range = wb.worksheet_range("Patches").unwrap();
        assert_eq!(range.get_value((0, 0)).unwrap().to_string(), "Organization");
        assert_eq!(range.get_value((1, 0)).unwrap().to_string(), "Contoso");
        assert_eq!(range.get_value((1, 7)).unwrap().to_string(), "KB5040434");
        let summary = wb.worksheet_range("Compliance").unwrap();
        assert_eq!(
            summary.get_value((0, 0)).unwrap().to_string(),
            "Organization"
        );
        assert_eq!(summary.get_value((1, 0)).unwrap().to_string(), "Contoso");
        let os = wb.worksheet_range("Compliance by OS").unwrap();
        assert_eq!(os.get_value((0, 0)).unwrap().to_string(), "OS");
        assert_eq!(
            os.get_value((1, 0)).unwrap().to_string(),
            "Windows Server 2022"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A workbook outlives the session that produced it, and until the About sheet
    /// existed it carried no timestamp at all — the only stamp was in the suggested
    /// file name, which survives exactly one rename. Both clocks must be on it: a
    /// re-filter recomputes over a warm cache, so `generated_at` alone would date a
    /// report to now over data fetched much earlier.
    #[test]
    fn the_about_sheet_records_both_clocks_the_filters_and_the_scope() {
        use calamine::{Reader, Xlsx, open_workbook};
        let path = std::env::temp_dir().join("npt-export-about.xlsx");
        write_workbook(
            &path.to_string_lossy(),
            &[sample_row()],
            &[],
            &[],
            &[],
            &[],
            &meta(),
        )
        .unwrap();

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let about = wb.worksheet_range("About").unwrap();
        let text: Vec<String> = about
            .rows()
            .map(|r| {
                r.iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect();
        let joined = text.join("\n");
        for expected in [
            "Generated|2026-05-02 09:15:00 UTC",
            "Patch data fetched|2026-05-02 08:40:00 UTC",
            "Devices in scope|2",
            "Offline devices|1",
            "Detail rows|1",
        ] {
            assert!(
                joined.contains(expected),
                "About sheet is missing {expected:?}:\n{joined}"
            );
        }
        assert!(joined.contains(NOTE), "the scope sentence rides along too");

        // And the facets, without which two workbooks off the same fleet under
        // different filters are indistinguishable once saved.
        for expected in [
            "Organizations|Contoso",
            "Patch type|OS and third-party patches",
            "Status|Pending, Failed",
            "Severity|CRITICAL",
        ] {
            assert!(
                joined.contains(expected),
                "About sheet is missing the {expected:?} facet:\n{joined}"
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    /// The compliance sheets assert "N offline devices excluded"; the detail sheet
    /// has to let a reader act on that. `PatchRow` carried the flag all along and
    /// the in-app table draws an "offline" chip from it — only the workbook dropped
    /// it, leaving the compliance denominator impossible to reproduce by hand.
    #[test]
    fn the_detail_sheet_reports_whether_a_device_was_offline() {
        use calamine::{Reader, Xlsx, open_workbook};
        let path = std::env::temp_dir().join("npt-export-offline.xlsx");
        let rows = vec![
            sample_row(),
            PatchRow {
                offline: true,
                device_id: 2,
                device_name: "srv02".into(),
                ..sample_row()
            },
        ];
        write_workbook(&path.to_string_lossy(), &rows, &[], &[], &[], &[], &meta()).unwrap();

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let range = wb.worksheet_range("Patches").unwrap();
        let col = DETAIL_COLUMNS
            .iter()
            .position(|(title, _)| *title == "Offline")
            .expect("the detail sheet declares an Offline column") as u32;
        assert_eq!(range.get_value((0, col)).unwrap().to_string(), "Offline");
        assert_eq!(range.get_value((1, col)).unwrap().to_string(), "No");
        assert_eq!(range.get_value((2, col)).unwrap().to_string(), "Yes");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn omits_empty_sheets_and_writes_the_reboot_sheet() {
        use calamine::{Reader, Xlsx, open_workbook};
        let path = std::env::temp_dir().join("npt-export-conditional.xlsx");

        // Reboot devices present but no compliance rows: the Compliance sheet is
        // omitted while Needs Reboot is written.
        let reboot = vec![DeviceSummary {
            device_id: 7,
            device_name: "srv07".into(),
            organization: "Contoso".into(),
            location: Some("HQ".into()),
            device_role: None,
            os_name: Some("Windows Server 2022".into()),
            node_class: None,
            needs_reboot: true,
            pending_count: 4,
        }];
        write_workbook(
            &path.to_string_lossy(),
            &[],
            &[],
            &[],
            &reboot,
            &[],
            &meta(),
        )
        .unwrap();

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let sheets = wb.sheet_names().to_owned();
        assert!(sheets.contains(&"Patches".to_string()));
        assert!(sheets.contains(&"Needs Reboot".to_string()));
        assert!(
            !sheets.contains(&"Compliance".to_string()),
            "an empty compliance set omits the Compliance sheet"
        );
        assert!(
            !sheets.contains(&"Compliance by OS".to_string()),
            "an empty OS-compliance set omits the Compliance by OS sheet"
        );
        assert!(
            !sheets.contains(&"Patch Failures".to_string()),
            "an empty failure set omits the Patch Failures sheet"
        );
        assert!(
            sheets.contains(&"About".to_string()),
            "the provenance sheet is written whatever the data sheets contain"
        );
        assert_eq!(
            sheets.first().map(String::as_str),
            Some("Patches"),
            "About goes last so the workbook still opens on the detail table"
        );
        let reboot_range = wb.worksheet_range("Needs Reboot").unwrap();
        assert_eq!(
            reboot_range.get_value((0, 0)).unwrap().to_string(),
            "Organization"
        );
        assert_eq!(reboot_range.get_value((1, 3)).unwrap().to_string(), "srv07");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn writes_the_patch_failures_sheet_when_present() {
        use calamine::{Reader, Xlsx, open_workbook};
        let path = std::env::temp_dir().join("npt-export-failures.xlsx");

        let failures = vec![FailureGroup {
            patch_type: "OS",
            kb: Some("KB5040434".into()),
            name: "Cumulative Update".into(),
            severity: "Critical",
            severity_rank: 5,
            affected_devices: 3,
            device_names: vec!["srv01".into(), "srv02".into(), "srv03".into()],
            latest_failure: Some("2026-05-01 00:00 UTC".into()),
            latest_failure_ts: Some(1_777_000_000),
        }];
        write_workbook(
            &path.to_string_lossy(),
            &[],
            &[],
            &[],
            &[],
            &failures,
            &meta(),
        )
        .unwrap();

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let range = wb.worksheet_range("Patch Failures").unwrap();
        assert_eq!(range.get_value((0, 0)).unwrap().to_string(), "Severity");
        assert_eq!(range.get_value((1, 2)).unwrap().to_string(), "KB5040434");
        assert_eq!(range.get_value((1, 4)).unwrap().to_string(), "3");
        assert_eq!(
            range.get_value((0, 6)).unwrap().to_string(),
            "Devices",
            "the device list is the last column"
        );
        assert_eq!(
            range.get_value((1, 6)).unwrap().to_string(),
            "srv01, srv02, srv03",
            "every affected device name is comma-joined"
        );

        let _ = std::fs::remove_file(&path);
    }
}
