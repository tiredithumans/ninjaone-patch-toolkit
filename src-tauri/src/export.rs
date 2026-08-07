use anyhow::{Context, Result};
use rust_xlsxwriter::{Color, Format, Workbook};

use crate::model::PatchRow;
use crate::rows::{
    ComplianceBucket, DeviceSummary, FailureGroup, OsCompliance, TableCell, TableColumn,
};

/// The Patches detail-sheet columns. Only the workbook renders this table, so it
/// lives here rather than on `PatchRow` — but it is declared the same way as the
/// shared ones so header and value stay a single declaration.
const DETAIL_COLUMNS: [TableColumn<PatchRow>; 14] = [
    ("Organization", |r| TableCell::Text(r.organization.clone())),
    ("Location", |r| {
        TableCell::Text(r.location.clone().unwrap_or_default())
    }),
    ("Device Role", |r| {
        TableCell::Text(r.device_role.clone().unwrap_or_default())
    }),
    ("Device", |r| TableCell::Text(r.device_name.clone())),
    ("OS", |r| {
        TableCell::Text(r.os_name.clone().unwrap_or_default())
    }),
    ("Node Class", |r| {
        TableCell::Text(r.node_class.clone().unwrap_or_default())
    }),
    ("Patch Type", |r| TableCell::Text(r.patch_type.clone())),
    ("KB", |r| TableCell::Text(r.kb.clone().unwrap_or_default())),
    ("Patch", |r| TableCell::Text(r.name.clone())),
    ("Severity", |r| TableCell::Text(r.severity.clone())),
    ("Status", |r| TableCell::Text(r.status.clone())),
    ("Needs Reboot", |r| {
        TableCell::Text(if r.needs_reboot { "Yes" } else { "No" }.to_string())
    }),
    ("First Seen", |r| {
        TableCell::Text(r.first_seen_date.clone().unwrap_or_default())
    }),
    ("Installed Date", |r| {
        TableCell::Text(r.installed_date.clone().unwrap_or_default())
    }),
];

// Column widths, positionally paired with each sheet's column table. Tying every
// array's length to `COLUMNS.len()` makes a new column without a width a compile
// error — previously the widths were an unchecked `&[f64]` literal passed inline,
// so a column added at one site and not the other silently misaligned the sheet.
const DETAIL_WIDTHS: [f64; DETAIL_COLUMNS.len()] = [
    24.0, 18.0, 18.0, 22.0, 26.0, 18.0, 11.0, 12.0, 40.0, 11.0, 11.0, 13.0, 20.0, 20.0,
];
const SUMMARY_WIDTHS: [f64; ComplianceBucket::COLUMNS.len()] = [28.0, 10.0, 11.0, 14.0, 24.0, 16.0];
const OS_SUMMARY_WIDTHS: [f64; OsCompliance::COLUMNS.len()] = [28.0, 10.0, 11.0, 14.0, 24.0, 16.0];
const REBOOT_WIDTHS: [f64; DeviceSummary::COLUMNS.len()] = [24.0, 18.0, 18.0, 22.0, 26.0, 14.0];
const FAILURE_WIDTHS: [f64; FailureGroup::COLUMNS.len()] =
    [11.0, 11.0, 12.0, 40.0, 16.0, 20.0, 60.0];

fn header_format() -> Format {
    Format::new()
        .set_bold()
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x1F2A37))
}

/// Writes a workbook with a Patches detail sheet (one row per device×patch), a
/// Compliance summary sheet, a Compliance by OS sheet, a Needs Reboot sheet for
/// devices flagged for reboot, and a Patch Failures sheet rolling up FAILED installs.
/// Sheets with no data are omitted.
pub fn write_workbook(
    path: &str,
    rows: &[PatchRow],
    compliance: &[ComplianceBucket],
    compliance_by_os: &[OsCompliance],
    reboot_devices: &[DeviceSummary],
    failures: &[FailureGroup],
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

    workbook.save(path).context("save workbook")?;
    Ok(())
}

/// Writes one sheet from a column table: headers, then every row's cells through
/// the same accessors that produced those headers.
///
/// One function for all five sheets. They were five near-identical bodies, each
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
    use super::*;

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
            patch_type: "OS".into(),
            kb: Some("KB5040434".into()),
            name: "Cumulative Update".into(),
            severity: "Critical".into(),
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
        write_workbook(&path_str, &rows, &compliance, &compliance_by_os, &[], &[]).unwrap();

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
        write_workbook(&path.to_string_lossy(), &[], &[], &[], &reboot, &[]).unwrap();

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
            patch_type: "OS".into(),
            kb: Some("KB5040434".into()),
            name: "Cumulative Update".into(),
            severity: "Critical".into(),
            severity_rank: 5,
            affected_devices: 3,
            device_names: vec!["srv01".into(), "srv02".into(), "srv03".into()],
            latest_failure: Some("2026-05-01 00:00 UTC".into()),
            latest_failure_ts: Some(1_777_000_000),
        }];
        write_workbook(&path.to_string_lossy(), &[], &[], &[], &[], &failures).unwrap();

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
