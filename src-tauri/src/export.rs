use std::borrow::Borrow;

use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use rust_xlsxwriter::{Color, ExcelDateTime, Format, Workbook, Worksheet};

use crate::model::PatchRow;
use crate::rows::{
    ComplianceBucket, DeviceSummary, FailureGroup, OsCompliance, QueryScope, TableCell,
    TableColumn, clamp_cell, utc_text,
};

/// Data rows one worksheet can hold: Excel's 1,048,576-row limit less the header.
const MAX_SHEET_DATA_ROWS: usize = 1_048_575;

/// The Patches detail-sheet columns. Only the workbook renders this table, so it
/// lives here rather than on `PatchRow` — but it is declared the same way as the
/// shared ones so header and value stay a single declaration.
pub(crate) const DETAIL_COLUMNS: [TableColumn<PatchRow>; 15] = [
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
    // The instants, not the row's formatted text, so the workbook writes real
    // date-times — sortable and filterable as dates in Excel.
    ("First Seen", |r| TableCell::DateTime(r.first_seen_ts)),
    ("Installed Date", |r| TableCell::DateTime(r.installed_ts)),
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
const DEVICE_WIDTHS: [f64; DeviceSummary::DEVICE_COLUMNS.len()] = [
    24.0, 18.0, 18.0, 22.0, 26.0, 8.0, 24.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 14.0,
    13.0, 13.0, 18.0,
];

/// The one number format every date-time cell is written with. The values are UTC
/// (NinjaOne's timestamps are epoch seconds, and the app's clocks are formatted in
/// UTC); the About sheet says so, since a date cell cannot carry a zone.
const DATE_TIME_FORMAT: &str = "yyyy-mm-dd hh:mm";

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
    pub devices_unpatchable: usize,
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
/// Compliance summary sheet, a Compliance by OS sheet, a Devices sheet (one row per
/// in-scope device), a Needs Reboot sheet for the devices flagged for reboot, a
/// Patch Failures sheet rolling up FAILED installs, and an About sheet carrying the
/// provenance in [`WorkbookMeta`]. Data sheets with no rows are omitted; Patches and
/// About are always written. Detail rows past one sheet's capacity continue on
/// `Patches (2)`, `Patches (3)`, … Every date-time is a real Excel date-time in UTC.
pub fn write_workbook(
    path: &str,
    rows: &[PatchRow],
    compliance: &[ComplianceBucket],
    compliance_by_os: &[OsCompliance],
    devices: &[DeviceSummary],
    failures: &[FailureGroup],
    meta: &WorkbookMeta<'_>,
) -> Result<()> {
    write_workbook_split(
        path,
        rows,
        compliance,
        compliance_by_os,
        devices,
        failures,
        meta,
        MAX_SHEET_DATA_ROWS,
    )
}

/// [`write_workbook`] with the per-sheet detail-row limit injected, so the split
/// is testable without writing a million rows.
#[allow(clippy::too_many_arguments)]
fn write_workbook_split(
    path: &str,
    rows: &[PatchRow],
    compliance: &[ComplianceBucket],
    compliance_by_os: &[OsCompliance],
    devices: &[DeviceSummary],
    failures: &[FailureGroup],
    meta: &WorkbookMeta<'_>,
    rows_per_sheet: usize,
) -> Result<()> {
    let mut workbook = Workbook::new();
    let header = header_format();

    // The detail sheet is always written (even empty) so the workbook always opens
    // on the table the operator asked for. It and the Devices sheet carry an
    // autofilter, being the two meant to be sliced by hand.
    //
    // Past Excel's row limit the rows continue on "Patches (2)", "Patches (3)", …
    // rather than failing the export: a whole-fleet third-party feed can run to
    // seven figures, and the rows past the limit are as real as the ones before it.
    let mut chunks = rows.chunks(rows_per_sheet.max(1));
    write_sheet(
        &mut workbook,
        &header,
        "Patches",
        &DETAIL_COLUMNS,
        &DETAIL_WIDTHS,
        chunks.next().unwrap_or_default(),
        true,
    )?;
    for (i, chunk) in chunks.enumerate() {
        write_sheet(
            &mut workbook,
            &header,
            &format!("Patches ({})", i + 2),
            &DETAIL_COLUMNS,
            &DETAIL_WIDTHS,
            chunk,
            true,
        )?;
    }
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
        write_footnote(&mut workbook, compliance.len(), &[meta.scope_note])?;
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
        write_footnote(&mut workbook, compliance_by_os.len(), &[meta.scope_note])?;
    }
    if !devices.is_empty() {
        // Every in-scope device, the excluded ones included and labelled: the scope
        // note says "N offline and M non-patchable devices excluded", and this is
        // the sheet that lets a reader find them by name. Their count cells are
        // blank rather than zero (see `DeviceSummary::DEVICE_COLUMNS`).
        write_sheet(
            &mut workbook,
            &header,
            "Devices",
            &DeviceSummary::DEVICE_COLUMNS,
            &DEVICE_WIDTHS,
            devices,
            true,
        )?;
        write_footnote(
            &mut workbook,
            devices.len(),
            &[meta.scope_note, FAILED_INSTALLS_NOTE],
        )?;
    }
    let reboot: Vec<&DeviceSummary> = devices.iter().filter(|d| d.needs_reboot).collect();
    if !reboot.is_empty() {
        write_sheet(
            &mut workbook,
            &header,
            "Needs Reboot",
            &DeviceSummary::COLUMNS,
            &REBOOT_WIDTHS,
            &reboot,
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

/// The Devices sheet's second footnote. The column is the one per-device number
/// that depends on the Status selection, so it says when it is blank and why.
const FAILED_INSTALLS_NOTE: &str = "Failed Installs counts FAILED install-history records in \
     the lookback window, and is blank when the query did not include the Failed status.";

/// The About sheet's statement of the zone every date cell is in. A date-time cell
/// has no zone of its own, and a reader in UTC−8 would otherwise take a 09:15
/// first-seen stamp as local morning.
const TIME_ZONE_NOTE: &str = "UTC — every date and time in this workbook";

/// Writes the About sheet: a label/value list recording when the numbers were
/// computed, when the data behind them was fetched, and what population they cover.
fn write_about_sheet(
    workbook: &mut Workbook,
    header: &Format,
    meta: &WorkbookMeta<'_>,
    detail_rows: usize,
) -> Result<()> {
    let date = date_time_format();
    let sheet = workbook.add_worksheet();
    sheet.set_name("About").context("name sheet")?;
    sheet
        .write_string_with_format(0, 0, "Field", header)
        .context("write header")?;
    sheet
        .write_string_with_format(0, 1, "Value", header)
        .context("write header")?;

    // The two clocks as date cells like every other date in the workbook. They
    // arrive as the app's formatted UTC text, so they are parsed back; a value that
    // does not parse is written as the text it was rather than dropped.
    let mut row = 0u32;
    for (label, clock) in [
        ("Generated", meta.generated_at),
        ("Patch data fetched", meta.data_fetched_at),
    ] {
        row += 1;
        sheet.write_string(row, 0, label)?;
        match clock_timestamp(clock).and_then(excel_date_time) {
            Some(when) => sheet.write_datetime_with_format(row, 1, &when, &date)?,
            None => sheet.write_string(row, 1, clock)?,
        };
    }
    row += 1;
    sheet.write_string(row, 0, "Time zone")?;
    sheet.write_string(row, 1, TIME_ZONE_NOTE)?;
    for (label, value) in [
        ("Devices in scope", meta.devices_total),
        ("Offline devices", meta.devices_offline),
        ("Non-patchable devices", meta.devices_unpatchable),
        ("Detail rows", detail_rows),
    ] {
        row += 1;
        sheet.write_string(row, 0, label)?;
        sheet.write_number(row, 1, value as f64)?;
    }

    // The facets, under their own banded headings. Without them two workbooks off
    // the same fleet — one scoped to a single org and CRITICAL-only, one unfiltered —
    // are indistinguishable once saved, and every number in both is a different
    // population. Two headings, because the two tiers reach different sheets: the
    // device scope and patch type narrow every sheet, but Status/Severity/Search and
    // the date windows narrow only the detail rows. The in-app Compliance tab says
    // so; a workbook that listed "Severity: CRITICAL" beside the Compliance sheet
    // without it read as a critical-only backlog.
    for (heading, facets) in [
        ("Filters (every sheet)", &meta.scope.facets),
        (
            "Patch filters (Patches and Patch Failures sheets only)",
            &meta.scope.patch_facets,
        ),
    ] {
        row += 2;
        sheet
            .write_string_with_format(row, 0, heading, header)
            .context("write header")?;
        sheet
            .write_string_with_format(row, 1, "Value", header)
            .context("write header")?;
        for (label, value) in facets {
            row += 1;
            sheet.write_string(row, 0, *label)?;
            // Operator-chosen names and free text (a long org list, a pasted
            // search) — clamped like every other free-text cell.
            sheet.write_string(row, 1, clamp_cell(value.clone()))?;
        }
    }

    sheet.write_string(row + 2, 0, meta.scope_note)?;
    apply_widths(sheet, &ABOUT_WIDTHS)?;
    Ok(())
}

/// Writes sentences under the last data row of the sheet just added — one blank
/// row, then one line each. Takes the row count rather than the sheet so it can run
/// after [`write_sheet`] has handed the worksheet back to the workbook.
fn write_footnote(workbook: &mut Workbook, data_rows: usize, lines: &[&str]) -> Result<()> {
    let last = workbook.worksheets().len() - 1;
    let sheet = workbook.worksheet_from_index(last)?;
    for (i, line) in lines.iter().enumerate() {
        sheet
            .write_string((data_rows + 2 + i) as u32, 0, *line)
            .context("write scope note")?;
    }
    Ok(())
}

fn date_time_format() -> Format {
    Format::new().set_num_format(DATE_TIME_FORMAT)
}

/// A Unix-seconds instant as an Excel date-time, or `None` outside the years Excel
/// can represent (1900–9999). A NinjaOne timestamp never is, but a corrupt one must
/// degrade to a text cell rather than fail the whole export.
fn excel_date_time(ts: i64) -> Option<ExcelDateTime> {
    ExcelDateTime::from_timestamp(ts).ok()
}

/// Reads back one of the app's clock stamps (`2026-05-02 09:15:00 UTC`, the format
/// `assemble_result` writes) as Unix seconds.
pub(crate) fn clock_timestamp(clock: &str) -> Option<i64> {
    NaiveDateTime::parse_from_str(clock, "%Y-%m-%d %H:%M:%S UTC")
        .ok()
        .map(|t| t.and_utc().timestamp())
}

/// Writes one cell. A date-time goes in as a real Excel date-time with
/// [`DATE_TIME_FORMAT`], so it sorts and filters as a date rather than as text;
/// `None` leaves the cell empty; an instant Excel cannot hold falls back to its
/// text spelling.
fn write_cell(
    sheet: &mut Worksheet,
    row: u32,
    col: u16,
    cell: TableCell,
    date: &Format,
) -> Result<()> {
    match cell {
        // By value: the accessor already allocated this String, and a reference
        // only made the writer clone it again.
        TableCell::Text(s) => sheet.write_string(row, col, clamp_cell(s))?,
        TableCell::Count(n) => sheet.write_number(row, col, n as f64)?,
        TableCell::Number(n) => sheet.write_number(row, col, n)?,
        TableCell::DateTime(None) => sheet,
        TableCell::DateTime(Some(ts)) => match excel_date_time(ts) {
            Some(when) => sheet.write_datetime_with_format(row, col, &when, date)?,
            None => sheet.write_string(row, col, utc_text(ts).unwrap_or_default())?,
        },
    };
    Ok(())
}

/// Writes one sheet from a column table: headers, then every row's cells through
/// the same accessors that produced those headers.
///
/// One function for every data sheet. They were five near-identical bodies, each
/// re-deriving the header loop, the per-cell writes and the width application by
/// hand — which is exactly how the failures table came to be headed one way in the
/// workbook and another in the report. `R: Borrow<T>` so a filtered subset (the
/// reboot list) is written from references rather than cloned out.
fn write_sheet<T, R: Borrow<T>>(
    workbook: &mut Workbook,
    header: &Format,
    name: &str,
    columns: &[TableColumn<T>],
    widths: &[f64],
    rows: &[R],
    autofilter: bool,
) -> Result<()> {
    let date = date_time_format();
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
            write_cell(sheet, row, col as u16, value(item.borrow()), &date)?;
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

fn apply_widths(sheet: &mut Worksheet, widths: &[f64]) -> Result<()> {
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
    use calamine::DataType as _;

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
            devices_unpatchable: 1,
            scope: SCOPE.get_or_init(|| QueryScope {
                facets: vec![
                    ("Organizations", "Contoso".to_string()),
                    ("Patch type", "OS and third-party patches".to_string()),
                ],
                patch_facets: vec![
                    ("Status", "Pending, Failed".to_string()),
                    ("Severity", "CRITICAL".to_string()),
                ],
                ..Default::default()
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

    fn device_summary(id: i64, name: &str, needs_reboot: bool) -> DeviceSummary {
        DeviceSummary {
            device_id: id,
            device_name: name.into(),
            organization: "Contoso".into(),
            location: Some("HQ".into()),
            device_role: None,
            os_name: Some("Windows Server 2022".into()),
            node_class: None,
            needs_reboot,
            pending_count: 4,
            offline: false,
            rollup_scope: crate::rows::RollupScope::Included,
            pending_by_severity: crate::rows::SeverityCounts {
                critical: 3,
                low: 1,
                ..Default::default()
            },
            aged_critical: 2,
            failed_installs: None,
            last_contact: Some("2026-04-24 03:06 UTC".into()),
            last_contact_ts: Some(1_777_000_000),
        }
    }

    /// A cell as the reader sees it: a date-time as `YYYY-MM-DD HH:MM:SS` (what the
    /// cell *holds*, independent of its display format), anything else as its text.
    fn cell_text(c: &calamine::Data) -> String {
        match c {
            calamine::Data::DateTime(d) => {
                let (y, mo, day, h, mi, s, _) = d.to_ymd_hms_milli();
                format!("{y:04}-{mo:02}-{day:02} {h:02}:{mi:02}:{s:02}")
            }
            other => other.to_string(),
        }
    }

    fn column(columns: &[&str], title: &str) -> u32 {
        columns
            .iter()
            .position(|t| *t == title)
            .unwrap_or_else(|| panic!("no {title:?} column")) as u32
    }

    /// Dates are written as real Excel date-times, not as text: a text date sorts
    /// and filters as a string, so "first seen before May" was a string compare.
    /// Every date column — detail, failures, devices — goes through the same cell
    /// writer, and a missing date is an empty cell rather than a blank string.
    #[test]
    fn dates_are_real_excel_date_times_in_utc() {
        use calamine::{Reader, Xlsx, open_workbook};
        let path = std::env::temp_dir().join("npt-export-dates.xlsx");
        let failures = vec![FailureGroup {
            patch_type: "OS",
            kb: Some("KB5040434".into()),
            name: "Cumulative Update".into(),
            severity: "Critical",
            severity_rank: 7,
            affected_devices: 1,
            device_names: vec!["srv01".into()],
            latest_failure: Some("2026-04-24 03:06 UTC".into()),
            latest_failure_ts: Some(1_777_000_000),
        }];
        write_workbook(
            &path.to_string_lossy(),
            &[sample_row()],
            &[],
            &[],
            &[device_summary(1, "srv01", false)],
            &failures,
            &meta(),
        )
        .unwrap();

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let titles = |cols: &[TableColumn<PatchRow>]| cols.iter().map(|c| c.0).collect::<Vec<_>>();
        let detail = titles(&DETAIL_COLUMNS);
        let patches = wb.worksheet_range("Patches").unwrap();
        let seen = patches
            .get_value((1, column(&detail, "First Seen")))
            .unwrap();
        assert!(
            seen.is_datetime(),
            "First Seen is a date cell, got {seen:?}"
        );
        assert_eq!(cell_text(seen), "2026-04-24 03:06:40");
        assert!(
            patches
                .get_value((1, column(&detail, "Installed Date")))
                .is_none_or(|c| c.is_empty()),
            "an absent date is an empty cell"
        );

        let failures = wb.worksheet_range("Patch Failures").unwrap();
        let latest = failures.get_value((1, 5)).unwrap();
        assert_eq!(
            failures.get_value((0, 5)).unwrap().to_string(),
            "Latest Failure"
        );
        assert!(latest.is_datetime());
        assert_eq!(cell_text(latest), "2026-04-24 03:06:40");

        let devices = wb.worksheet_range("Devices").unwrap();
        let device_titles: Vec<&str> = DeviceSummary::DEVICE_COLUMNS.iter().map(|c| c.0).collect();
        let contact = devices
            .get_value((1, column(&device_titles, "Last Contact")))
            .unwrap();
        assert!(contact.is_datetime());
        assert_eq!(cell_text(contact), "2026-04-24 03:06:40");

        let _ = std::fs::remove_file(&path);
    }

    /// A timestamp Excel cannot hold (outside 1900–9999) degrades to text instead
    /// of failing the whole export on one corrupt record.
    #[test]
    fn an_unrepresentable_date_falls_back_to_text() {
        assert!(excel_date_time(1_777_000_000).is_some());
        assert!(excel_date_time(-3_000_000_000).is_none(), "before 1900");
        assert!(excel_date_time(300_000_000_000).is_none(), "after 9999");
        assert_eq!(
            clock_timestamp("2026-05-02 09:15:00 UTC"),
            Some(1_777_713_300)
        );
        assert_eq!(clock_timestamp("not a clock"), None);
    }

    /// The Devices sheet: one row per in-scope device, every severity band as its
    /// own column, and an excluded device labelled with its reason and left blank —
    /// not zero — in every rollup count, because an offline device's zero means
    /// "unknown", not "clean".
    #[test]
    fn the_devices_sheet_lists_every_device_and_blanks_the_excluded() {
        use calamine::{Reader, Xlsx, open_workbook};
        let path = std::env::temp_dir().join("npt-export-devices.xlsx");
        let devices = vec![
            DeviceSummary {
                failed_installs: Some(2),
                ..device_summary(1, "srv01", false)
            },
            DeviceSummary {
                offline: true,
                rollup_scope: crate::rows::RollupScope::Offline,
                last_contact_ts: None,
                ..device_summary(2, "srv02", false)
            },
        ];
        write_workbook(
            &path.to_string_lossy(),
            &[],
            &[],
            &[],
            &devices,
            &[],
            &meta(),
        )
        .unwrap();

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let sheet = wb.worksheet_range("Devices").unwrap();
        let titles: Vec<&str> = DeviceSummary::DEVICE_COLUMNS.iter().map(|c| c.0).collect();
        let at = |row: u32, title: &str| {
            sheet
                .get_value((row, column(&titles, title)))
                .map(cell_text)
                .unwrap_or_default()
        };
        assert_eq!(at(0, "Pending Critical"), "Pending Critical");
        assert_eq!(at(1, "Device"), "srv01");
        assert_eq!(at(1, "Online"), "Yes");
        assert_eq!(at(1, "Compliance Scope"), "Included");
        assert_eq!(at(1, "Pending Critical"), "3");
        assert_eq!(at(1, "Pending Low"), "1");
        assert_eq!(at(1, "Pending Important"), "0");
        assert_eq!(at(1, "Aged (past SLA)"), "2");
        assert_eq!(at(1, "Failed Installs"), "2");

        assert_eq!(at(2, "Device"), "srv02");
        assert_eq!(at(2, "Online"), "No");
        assert_eq!(at(2, "Compliance Scope"), "Excluded (offline)");
        for title in ["Pending Critical", "Pending Low", "Aged (past SLA)"] {
            assert_eq!(at(2, title), "", "{title} is blank for an excluded device");
        }
        assert_eq!(
            at(2, "Failed Installs"),
            "",
            "unknown when Failed was not queried"
        );
        assert_eq!(at(2, "Last Contact"), "");

        // The scope note and the Failed Installs caveat sit under the data.
        assert_eq!(at(4, "Organization"), NOTE);
        assert_eq!(at(5, "Organization"), FAILED_INSTALLS_NOTE);

        let _ = std::fs::remove_file(&path);
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
            .map(|r| r.iter().map(cell_text).collect::<Vec<_>>().join("|"))
            .collect();
        let joined = text.join("\n");
        // The clocks are date cells like every other date in the workbook, and the
        // sheet says which zone they are in, since a date cell cannot.
        assert!(about.get_value((1, 1)).unwrap().is_datetime());
        assert!(about.get_value((2, 1)).unwrap().is_datetime());
        for expected in [
            "Generated|2026-05-02 09:15:00",
            "Patch data fetched|2026-05-02 08:40:00",
            "Time zone|UTC — every date and time in this workbook",
            "Devices in scope|2",
            "Offline devices|1",
            "Non-patchable devices|1",
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
        // And under headings that say which sheets each tier narrows — the row-only
        // facets listed beside the Compliance sheet with no such note read as a
        // critical-only backlog.
        let every = joined.find("Filters (every sheet)").expect("fleet heading");
        let patch = joined
            .find("Patch filters (Patches and Patch Failures sheets only)")
            .expect("patch heading");
        let status = joined.find("Status|Pending, Failed").unwrap();
        let orgs = joined.find("Organizations|Contoso").unwrap();
        assert!(every < orgs && orgs < patch && patch < status);

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
        let reboot = vec![device_summary(7, "srv07", true)];
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

    /// A patch failing on a few thousand devices joined its names past Excel's
    /// 32,767-character cell limit, and the one rejected cell failed the whole
    /// export. The list now ends in "… and N more" inside the limit.
    #[test]
    fn a_fleet_wide_failure_list_fits_in_one_cell() {
        use crate::rows::CELL_MAX_CHARS;
        use calamine::{Reader, Xlsx, open_workbook};
        let path = std::env::temp_dir().join("npt-export-huge-failure.xlsx");

        let names: Vec<std::sync::Arc<str>> = (0..5_000)
            .map(|i| format!("workstation-{i:05}.corp.example").into())
            .collect();
        let failures = vec![FailureGroup {
            patch_type: "OS",
            kb: Some("KB5040434".into()),
            name: "Cumulative Update".into(),
            severity: "Critical",
            severity_rank: 7,
            affected_devices: names.len(),
            device_names: names,
            latest_failure: None,
            latest_failure_ts: None,
        }];
        // A free-text facet past the cap is clamped rather than failing too.
        let scope = QueryScope {
            facets: vec![("Organizations", "x".repeat(CELL_MAX_CHARS + 10))],
            ..Default::default()
        };
        let meta = WorkbookMeta {
            scope: &scope,
            ..meta()
        };
        write_workbook(
            &path.to_string_lossy(),
            &[],
            &[],
            &[],
            &[],
            &failures,
            &meta,
        )
        .expect("the export no longer fails on a long cell");

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let range = wb.worksheet_range("Patch Failures").unwrap();
        let cell = range.get_value((1, 6)).unwrap().to_string();
        assert!(cell.chars().count() <= CELL_MAX_CHARS);
        assert!(cell.starts_with("workstation-00000.corp.example, "));
        let more = cell.rsplit("… and ").next().unwrap();
        let dropped: usize = more.trim_end_matches(" more").parse().unwrap();
        let shown = cell.matches("workstation-").count();
        assert_eq!(
            shown + dropped,
            5_000,
            "the count accounts for every device"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Past Excel's row limit the detail rows continue on numbered sheets instead
    /// of failing the export; nothing is dropped and About still goes last.
    #[test]
    fn detail_rows_past_one_sheet_continue_on_numbered_sheets() {
        use calamine::{Reader, Xlsx, open_workbook};
        let path = std::env::temp_dir().join("npt-export-split.xlsx");
        let rows: Vec<PatchRow> = (0..5)
            .map(|i| PatchRow {
                device_id: i + 1,
                device_name: format!("srv{i}").into(),
                ..sample_row()
            })
            .collect();
        write_workbook_split(
            &path.to_string_lossy(),
            &rows,
            &[],
            &[],
            &[],
            &[],
            &meta(),
            2,
        )
        .unwrap();

        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        assert_eq!(
            wb.sheet_names(),
            ["Patches", "Patches (2)", "Patches (3)", "About"]
        );
        let mut seen = Vec::new();
        for sheet in ["Patches", "Patches (2)", "Patches (3)"] {
            let range = wb.worksheet_range(sheet).unwrap();
            assert_eq!(
                range.get_value((0, 0)).unwrap().to_string(),
                "Organization",
                "{sheet} repeats the header"
            );
            for r in 1..range.height() as u32 {
                seen.push(range.get_value((r, 3)).unwrap().to_string());
            }
        }
        assert_eq!(seen, ["srv0", "srv1", "srv2", "srv3", "srv4"]);

        let _ = std::fs::remove_file(&path);
    }
}
