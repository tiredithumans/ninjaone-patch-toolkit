//! CSV export of a column table — today the Patches detail rows — written by hand
//! rather than through a CSV crate: the format needed here is three rules (RFC 4180
//! quoting, CRLF, a UTF-8 BOM) plus a formula-injection guard, which is less code
//! than the dependency's configuration and is tested below.
//!
//! CSV has no comment or metadata syntax, so unlike the workbook's About sheet a
//! file carries no provenance of its own; the scope and both clocks go in the
//! default file name (`commands::export::csv_file_name`) and nowhere else. The
//! workbook is the artifact to share when the facets matter.

use std::io::{self, Write};

use crate::rows::{TableCell, TableColumn, utc_text};

/// UTF-8 byte-order mark. Without it Excel opens a CSV in the system ANSI code
/// page, and every non-ASCII organization or device name ("Zürich", an em dash in
/// a location) arrives as mojibake.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Characters that make a spreadsheet read a cell as a formula (or, for tab and
/// CR, that some importers strip to expose one). OWASP's CSV-injection guidance:
/// a cell beginning with any of them gets a leading `'`, so a patch or device name
/// such as `=HYPERLINK("http://evil/", "Click")` is displayed, not evaluated.
///
/// `-` is on the list, which is why only [`TableCell::Text`] is guarded: a
/// negative number written from `Count`/`Number` must stay a number, while a text
/// cell that happens to start with `-` costs one visible apostrophe.
const FORMULA_TRIGGERS: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// Writes `rows` as CSV: a UTF-8 BOM, one header record from the column titles,
/// then one record per row through the same accessors, every record ending CRLF.
///
/// Text fields are always quoted (RFC 4180: embedded `"` doubled, so commas and
/// line breaks inside a field are safe) and guarded against formula injection;
/// numbers are written bare so they stay numeric; date-times use the app's one UTC
/// spelling ([`utc_text`]), which sorts chronologically even as text.
pub fn write_csv<T, W: Write>(
    out: &mut W,
    columns: &[TableColumn<T>],
    rows: &[T],
) -> io::Result<()> {
    out.write_all(UTF8_BOM)?;
    let mut line = String::new();
    for (i, (title, _)) in columns.iter().enumerate() {
        if i > 0 {
            line.push(',');
        }
        push_text(&mut line, title);
    }
    line.push_str("\r\n");
    out.write_all(line.as_bytes())?;

    for row in rows {
        line.clear();
        for (i, (_, value)) in columns.iter().enumerate() {
            if i > 0 {
                line.push(',');
            }
            push_cell(&mut line, value(row));
        }
        line.push_str("\r\n");
        out.write_all(line.as_bytes())?;
    }
    Ok(())
}

fn push_cell(line: &mut String, cell: TableCell) {
    match cell {
        TableCell::Text(s) => push_text(line, &s),
        TableCell::Count(n) => line.push_str(&n.to_string()),
        TableCell::Number(n) => line.push_str(&n.to_string()),
        TableCell::DateTime(ts) => {
            if let Some(when) = ts.and_then(utc_text) {
                push_text(line, &when);
            }
        }
    }
}

/// One quoted text field, formula-guarded. An empty value is written as an empty
/// field rather than `""`, the way every other CSV writer spells "no value".
fn push_text(line: &mut String, s: &str) {
    if s.is_empty() {
        return;
    }
    line.push('"');
    if s.starts_with(FORMULA_TRIGGERS) {
        line.push('\'');
    }
    for ch in s.chars() {
        if ch == '"' {
            line.push('"');
        }
        line.push(ch);
    }
    line.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> String {
        let mut line = String::new();
        push_text(&mut line, s);
        line
    }

    fn cell(c: TableCell) -> String {
        let mut line = String::new();
        push_cell(&mut line, c);
        line
    }

    /// OWASP's list, every entry: a cell a spreadsheet would evaluate is written
    /// with a leading apostrophe so it is displayed instead.
    #[test]
    fn every_formula_trigger_is_neutralised() {
        assert_eq!(text("=1+1"), "\"'=1+1\"");
        assert_eq!(text("+cmd"), "\"'+cmd\"");
        assert_eq!(text("-2+3"), "\"'-2+3\"");
        assert_eq!(text("@SUM(A1)"), "\"'@SUM(A1)\"");
        assert_eq!(text("\t=1"), "\"'\t=1\"");
        assert_eq!(text("\r=1"), "\"'\r=1\"");
        // A real-world payload keeps its quotes doubled after the guard.
        assert_eq!(
            text("=HYPERLINK(\"http://x\",\"y\")"),
            "\"'=HYPERLINK(\"\"http://x\"\",\"\"y\"\")\""
        );
    }

    /// Only a *leading* trigger is a formula; the same characters later in a value
    /// are ordinary text and must not grow an apostrophe.
    #[test]
    fn ordinary_text_is_left_alone() {
        assert_eq!(text("KB5040434"), "\"KB5040434\"");
        assert_eq!(text("a=b"), "\"a=b\"");
        assert_eq!(
            text("Server 2022 - Datacenter"),
            "\"Server 2022 - Datacenter\""
        );
        assert_eq!(text(""), "");
    }

    /// `-` is a trigger, so the guard applies to text only — a number column must
    /// stay numeric, negative values included.
    #[test]
    fn numbers_are_written_bare_and_unguarded() {
        assert_eq!(cell(TableCell::Count(42)), "42");
        assert_eq!(cell(TableCell::Number(-1.5)), "-1.5");
        assert_eq!(cell(TableCell::Number(99.9)), "99.9");
    }

    #[test]
    fn date_times_use_the_shared_utc_spelling() {
        assert_eq!(
            cell(TableCell::DateTime(Some(1_777_000_000))),
            "\"2026-04-24 03:06 UTC\""
        );
        assert_eq!(cell(TableCell::DateTime(None)), "");
    }

    /// RFC 4180: a field holding a comma, a quote or a line break survives a round
    /// trip because it is quoted and its quotes are doubled.
    #[test]
    fn separators_inside_a_field_are_quoted() {
        assert_eq!(text("Contoso, Ltd"), "\"Contoso, Ltd\"");
        assert_eq!(text("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(text("two\nlines"), "\"two\nlines\"");
    }

    struct Row {
        name: &'static str,
        count: usize,
        seen: Option<i64>,
    }

    const COLUMNS: [TableColumn<Row>; 3] = [
        ("Name", |r| TableCell::text(r.name)),
        ("Count", |r| TableCell::Count(r.count)),
        ("First Seen", |r| TableCell::DateTime(r.seen)),
    ];

    /// The whole file: BOM, header from the column titles, CRLF after every record
    /// including the last.
    #[test]
    fn writes_bom_header_and_crlf_records() {
        let rows = [
            Row {
                name: "srv01",
                count: 3,
                seen: Some(1_777_000_000),
            },
            Row {
                name: "=cmd|' /C calc'!A0",
                count: 0,
                seen: None,
            },
        ];
        let mut out = Vec::new();
        write_csv(&mut out, &COLUMNS, &rows).unwrap();
        assert!(
            out.starts_with(UTF8_BOM),
            "Excel needs the BOM to read UTF-8"
        );
        let body = std::str::from_utf8(&out[UTF8_BOM.len()..]).unwrap();
        assert_eq!(
            body,
            "\"Name\",\"Count\",\"First Seen\"\r\n\
             \"srv01\",3,\"2026-04-24 03:06 UTC\"\r\n\
             \"'=cmd|' /C calc'!A0\",0,\r\n"
        );
    }
}
