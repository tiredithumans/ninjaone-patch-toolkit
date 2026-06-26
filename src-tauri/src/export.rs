use anyhow::{Context, Result};
use rust_xlsxwriter::{Color, Format, Workbook};

use crate::model::PatchRow;
use crate::rows::{ComplianceBucket, DeviceSummary, FailureGroup};

const DETAIL_HEADERS: [&str; 14] = [
    "Organization",
    "Location",
    "Device Role",
    "Device",
    "OS",
    "Node Class",
    "Patch Type",
    "KB",
    "Patch",
    "Severity",
    "Status",
    "Needs Reboot",
    "Release Date",
    "Installed Date",
];

const SUMMARY_HEADERS: [&str; 6] = [
    "Organization",
    "Devices",
    "Compliant",
    "Compliance %",
    "Pending Critical/Important",
    "Aged (past SLA)",
];

const REBOOT_HEADERS: [&str; 6] = [
    "Organization",
    "Location",
    "Device Role",
    "Device",
    "OS",
    "Pending Patches",
];

const FAILURE_HEADERS: [&str; 7] = [
    "Severity",
    "Patch Type",
    "KB",
    "Patch",
    "Affected Devices",
    "Sample Devices",
    "Latest Failure",
];

fn header_format() -> Format {
    Format::new()
        .set_bold()
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x1F2A37))
}

/// Writes a workbook with a Patches detail sheet (one row per device×patch), a
/// Compliance summary sheet, a Needs Reboot sheet for devices flagged for reboot,
/// and a Patch Failures sheet rolling up FAILED installs. Sheets with no data are
/// omitted.
pub fn write_workbook(
    path: &str,
    rows: &[PatchRow],
    compliance: &[ComplianceBucket],
    reboot_devices: &[DeviceSummary],
    failures: &[FailureGroup],
) -> Result<()> {
    let mut workbook = Workbook::new();
    let header = header_format();

    write_detail_sheet(&mut workbook, &header, rows)?;
    if !compliance.is_empty() {
        write_summary_sheet(&mut workbook, &header, compliance)?;
    }
    if !reboot_devices.is_empty() {
        write_reboot_sheet(&mut workbook, &header, reboot_devices)?;
    }
    if !failures.is_empty() {
        write_failures_sheet(&mut workbook, &header, failures)?;
    }

    workbook.save(path).context("save workbook")?;
    Ok(())
}

fn write_detail_sheet(workbook: &mut Workbook, header: &Format, rows: &[PatchRow]) -> Result<()> {
    let sheet = workbook.add_worksheet();
    sheet.set_name("Patches").context("name detail sheet")?;

    for (col, title) in DETAIL_HEADERS.iter().enumerate() {
        sheet
            .write_string_with_format(0, col as u16, *title, header)
            .context("write header")?;
    }

    for (i, r) in rows.iter().enumerate() {
        let row = (i + 1) as u32;
        // Write each cell from a borrow rather than cloning all 14 fields into a
        // temporary array first (this runs once per exported row).
        let cells: [&str; 14] = [
            &r.organization,
            r.location.as_deref().unwrap_or_default(),
            r.device_role.as_deref().unwrap_or_default(),
            &r.device_name,
            r.os_name.as_deref().unwrap_or_default(),
            r.node_class.as_deref().unwrap_or_default(),
            &r.patch_type,
            r.kb.as_deref().unwrap_or_default(),
            &r.name,
            &r.severity,
            &r.status,
            if r.needs_reboot { "Yes" } else { "No" },
            r.release_date.as_deref().unwrap_or_default(),
            r.installed_date.as_deref().unwrap_or_default(),
        ];
        for (col, value) in cells.iter().enumerate() {
            sheet
                .write_string(row, col as u16, *value)
                .context("write cell")?;
        }
    }

    sheet.set_freeze_panes(1, 0).context("freeze header")?;
    let last_row = rows.len() as u32; // header row 0 + data rows
    sheet
        .autofilter(0, 0, last_row.max(1), (DETAIL_HEADERS.len() - 1) as u16)
        .context("autofilter")?;
    apply_widths(
        sheet,
        &[
            24.0, 18.0, 18.0, 22.0, 26.0, 18.0, 11.0, 12.0, 40.0, 11.0, 11.0, 13.0, 20.0, 20.0,
        ],
    )?;
    Ok(())
}

fn write_summary_sheet(
    workbook: &mut Workbook,
    header: &Format,
    compliance: &[ComplianceBucket],
) -> Result<()> {
    let sheet = workbook.add_worksheet();
    sheet.set_name("Compliance").context("name summary sheet")?;

    for (col, title) in SUMMARY_HEADERS.iter().enumerate() {
        sheet
            .write_string_with_format(0, col as u16, *title, header)
            .context("write header")?;
    }

    for (i, b) in compliance.iter().enumerate() {
        let row = (i + 1) as u32;
        sheet.write_string(row, 0, &b.organization)?;
        sheet.write_number(row, 1, b.devices_total as f64)?;
        sheet.write_number(row, 2, b.devices_compliant as f64)?;
        sheet.write_number(row, 3, (b.compliance_pct * 10.0).round() / 10.0)?;
        sheet.write_number(row, 4, b.pending_critical as f64)?;
        sheet.write_number(row, 5, b.aged_critical as f64)?;
    }

    sheet.set_freeze_panes(1, 0).context("freeze header")?;
    apply_widths(sheet, &[28.0, 10.0, 11.0, 14.0, 24.0, 16.0])?;
    Ok(())
}

fn write_reboot_sheet(
    workbook: &mut Workbook,
    header: &Format,
    devices: &[DeviceSummary],
) -> Result<()> {
    let sheet = workbook.add_worksheet();
    sheet
        .set_name("Needs Reboot")
        .context("name reboot sheet")?;

    for (col, title) in REBOOT_HEADERS.iter().enumerate() {
        sheet
            .write_string_with_format(0, col as u16, *title, header)
            .context("write header")?;
    }

    for (i, d) in devices.iter().enumerate() {
        let row = (i + 1) as u32;
        sheet.write_string(row, 0, &d.organization)?;
        sheet.write_string(row, 1, d.location.as_deref().unwrap_or_default())?;
        sheet.write_string(row, 2, d.device_role.as_deref().unwrap_or_default())?;
        sheet.write_string(row, 3, &d.device_name)?;
        sheet.write_string(row, 4, d.os_name.as_deref().unwrap_or_default())?;
        sheet.write_number(row, 5, d.pending_count as f64)?;
    }

    sheet.set_freeze_panes(1, 0).context("freeze header")?;
    apply_widths(sheet, &[24.0, 18.0, 18.0, 22.0, 26.0, 14.0])?;
    Ok(())
}

fn write_failures_sheet(
    workbook: &mut Workbook,
    header: &Format,
    failures: &[FailureGroup],
) -> Result<()> {
    let sheet = workbook.add_worksheet();
    sheet
        .set_name("Patch Failures")
        .context("name failures sheet")?;

    for (col, title) in FAILURE_HEADERS.iter().enumerate() {
        sheet
            .write_string_with_format(0, col as u16, *title, header)
            .context("write header")?;
    }

    for (i, f) in failures.iter().enumerate() {
        let row = (i + 1) as u32;
        sheet.write_string(row, 0, &f.severity)?;
        sheet.write_string(row, 1, &f.patch_type)?;
        sheet.write_string(row, 2, f.kb.as_deref().unwrap_or_default())?;
        sheet.write_string(row, 3, &f.name)?;
        sheet.write_number(row, 4, f.affected_devices as f64)?;
        sheet.write_string(row, 5, f.sample_devices.join(", "))?;
        sheet.write_string(row, 6, f.latest_failure.as_deref().unwrap_or_default())?;
    }

    sheet.set_freeze_panes(1, 0).context("freeze header")?;
    apply_widths(sheet, &[11.0, 11.0, 12.0, 40.0, 16.0, 40.0, 20.0])?;
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
            patch_type: "OS".into(),
            kb: Some("KB5040434".into()),
            name: "Cumulative Update".into(),
            severity: "Critical".into(),
            severity_rank: 5,
            status: "PENDING".into(),
            release_date: Some("2026-05-01 00:00 UTC".into()),
            installed_date: None,
            release_ts: Some(1_777_000_000),
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
        write_workbook(&path_str, &rows, &compliance, &[], &[]).unwrap();

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
        write_workbook(&path.to_string_lossy(), &[], &[], &reboot, &[]).unwrap();

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let sheets = wb.sheet_names().to_owned();
        assert!(sheets.contains(&"Patches".to_string()));
        assert!(sheets.contains(&"Needs Reboot".to_string()));
        assert!(
            !sheets.contains(&"Compliance".to_string()),
            "an empty compliance set omits the Compliance sheet"
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
            sample_devices: vec!["srv01".into(), "srv02".into()],
            latest_failure: Some("2026-05-01 00:00 UTC".into()),
            latest_failure_ts: Some(1_777_000_000),
        }];
        write_workbook(&path.to_string_lossy(), &[], &[], &[], &failures).unwrap();

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let range = wb.worksheet_range("Patch Failures").unwrap();
        assert_eq!(range.get_value((0, 0)).unwrap().to_string(), "Severity");
        assert_eq!(range.get_value((1, 2)).unwrap().to_string(), "KB5040434");
        assert_eq!(range.get_value((1, 4)).unwrap().to_string(), "3");
        assert_eq!(
            range.get_value((1, 5)).unwrap().to_string(),
            "srv01, srv02",
            "sample devices are comma-joined"
        );

        let _ = std::fs::remove_file(&path);
    }
}
