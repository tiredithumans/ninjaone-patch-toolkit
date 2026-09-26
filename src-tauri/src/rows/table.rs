//! The shared table-column definition: each rendered table pairs a header
//! with the accessor that fills it, so the workbook and the HTML report cannot
//! disagree about a column.

/// One rendered cell of a table row. `Count`/`Number` are written as numbers by the
/// Excel exporter and right-aligned by the HTML report; `Text` is written as-is
/// (and HTML-escaped by the report).
pub enum TableCell {
    Text(String),
    Count(usize),
    Number(f64),
}

impl TableCell {
    /// A text cell from anything string-like. The row fields are a mix of `String`,
    /// `Arc<str>` and `&'static str` now that [`PatchRow`] shares its repeated
    /// values, and a renderer should not have to care which.
    pub fn text(value: impl AsRef<str>) -> Self {
        Self::Text(value.as_ref().to_string())
    }

    /// A text cell for an optional field, blank when absent — which every renderer
    /// already spelled out as `.clone().unwrap_or_default()`.
    pub fn opt_text(value: Option<impl AsRef<str>>) -> Self {
        Self::Text(value.map(|v| v.as_ref().to_string()).unwrap_or_default())
    }
}

/// One table column: its header and how to read that cell off a row.
///
/// Every table rendered from a cached [`QueryResult`] is defined as an array of
/// these, so a column's header and its value are one declaration rather than two
/// lists that agree by convention. That convention had already failed twice — the
/// HTML report dropped `Patch Type` from the failures table the workbook wrote,
/// and the reboot table's headers drifted to "Role" against the workbook's "Device
/// Role" — which is why the definitions now live here, next to the data, instead
/// of once per renderer.
pub type TableColumn<T> = (&'static str, fn(&T) -> TableCell);

/// Rounds a percentage to one decimal for display, so the workbook and the report
/// cannot disagree about precision — and never *up* to a full 100. See
/// [`format_pct`], which applies the same rule at zero decimals.
pub(super) fn pct_cell(pct: f64) -> TableCell {
    let rounded = (pct * 10.0).round() / 10.0;
    TableCell::Number(if pct >= 100.0 {
        100.0
    } else {
        rounded.min(99.9)
    })
}

/// Formats a compliance percentage at zero decimals **without ever rounding up to
/// 100%**.
///
/// A compliance report that prints "100%" for a fleet that is not fully compliant is
/// the one rounding error in this app that can send someone home: plain `{:.0}%`
/// renders anything from 99.5% up as `100%`, so 199 of 200 devices patched read as
/// "done". Values below 100 are capped at 99%, which understates by less than a
/// point and never claims a clean fleet that isn't. Exactly 100.0 still prints
/// `100%`.
///
/// Mirrored in `web-rs/src/app/util/format.rs::format_pct` for the in-app tables and charts;
/// the two crates share no code, so the rule is written twice on purpose.
pub fn format_pct(pct: f64) -> String {
    let shown = if pct >= 100.0 {
        100.0
    } else {
        pct.round().min(99.0)
    };
    format!("{shown:.0}%")
}

/// Excel's hard limit on the characters in one cell. `rust_xlsxwriter` rejects a
/// longer string with an error, and one rejected cell fails the whole workbook — so
/// an export of a large fleet failed outright on a single long value.
pub const CELL_MAX_CHARS: usize = 32_767;

/// Cuts `s` to at most [`CELL_MAX_CHARS`] characters, marking the cut with `…`.
///
/// The backstop for any free-text cell. A list that can say what it dropped should
/// use [`join_capped`] instead.
pub fn clamp_cell(s: String) -> String {
    if s.len() <= CELL_MAX_CHARS || s.chars().count() <= CELL_MAX_CHARS {
        return s;
    }
    let mut out: String = s.chars().take(CELL_MAX_CHARS - 1).collect();
    out.push('…');
    out
}

/// Joins `items` with `, ` in at most `max_chars` characters. When the whole list
/// does not fit it keeps as many leading items as leave room for a closing
/// "… and N more", so the reader knows the list is incomplete and by how much.
pub fn join_capped<S: AsRef<str>>(items: &[S], max_chars: usize) -> String {
    let full = items
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(", ");
    if full.chars().count() <= max_chars {
        return full;
    }
    // Room for the longest suffix this list could need, so the kept items can
    // never push it over the cap.
    let budget = max_chars.saturating_sub(format!(", … and {} more", items.len()).len());
    let mut out = String::new();
    let mut len = 0;
    let mut kept = 0;
    for item in items {
        let item = item.as_ref();
        let add = item.chars().count() + if kept == 0 { 0 } else { 2 };
        if len + add > budget {
            break;
        }
        if kept > 0 {
            out.push_str(", ");
        }
        out.push_str(item);
        len += add;
        kept += 1;
    }
    let rest = items.len() - kept;
    if kept > 0 {
        out.push_str(", ");
    }
    out.push_str(&format!("… and {rest} more"));
    out
}
