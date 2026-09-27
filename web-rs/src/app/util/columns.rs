//! The Patches table's column chooser: which columns the operator hid, how that is
//! remembered, and the CSS that hides them.
//!
//! Remembered as the set of *hidden* column ids, never as the visible ones, so a
//! column added by a later build shows up by default instead of being hidden by a
//! list that predates it. Ids the current build does not know are kept (a newer
//! build may know them) and simply match nothing.
//!
//! Hiding is CSS (`display: none` by column position) rather than a filtered cell
//! list: the header and every row cell stay rendered from one place, so a column
//! added to the table needs no second edit here, and the row markup cannot drift
//! from the chooser. Exports never consult any of this — they write every column.

use std::collections::BTreeSet;

/// A stable id for a column, derived from its header label: `"First seen"` →
/// `"first-seen"`.
pub(crate) fn column_id(label: &str) -> String {
    let mut id = String::with_capacity(label.len());
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    while id.ends_with('-') {
        id.pop();
    }
    id
}

/// Reads the stored hidden set. Anything unreadable — no value, not JSON, not a
/// list of strings — is "nothing hidden": the worst a bad value can do is show
/// every column.
pub(crate) fn parse_hidden_columns(stored: Option<&str>) -> BTreeSet<String> {
    stored
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .map(|ids| ids.into_iter().filter(|id| !id.is_empty()).collect())
        .unwrap_or_default()
}

pub(crate) fn serialize_hidden_columns(hidden: &BTreeSet<String>) -> String {
    serde_json::to_string(hidden).unwrap_or_else(|_| "[]".to_string())
}

/// Whether a column is shown. A required column always is, whatever was stored.
pub(crate) fn column_visible(id: &str, required: &[&str], hidden: &BTreeSet<String>) -> bool {
    required.contains(&id) || !hidden.contains(id)
}

/// Flips one column's visibility. A no-op for a required column.
pub(crate) fn toggle_column(hidden: &mut BTreeSet<String>, id: &str, required: &[&str]) {
    if required.contains(&id) {
        return;
    }
    if !hidden.remove(id) {
        hidden.insert(id.to_string());
    }
}

/// The stylesheet hiding the chosen columns of `table_class`. `labels` are the
/// header labels in display order; `leading` counts the unlabelled columns before
/// them (the selection checkbox), which are never hidden.
pub(crate) fn hidden_columns_css(
    table_class: &str,
    labels: &[&str],
    leading: usize,
    required: &[&str],
    hidden: &BTreeSet<String>,
) -> String {
    labels
        .iter()
        .enumerate()
        .filter(|(_, label)| !column_visible(&column_id(label), required, hidden))
        .map(|(i, _)| {
            let n = leading + i + 1;
            format!(
                "table.{table_class} th:nth-child({n}),table.{table_class} td:nth-child({n}){{display:none}}"
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ids_are_slugs_of_the_label() {
        assert_eq!(column_id("First seen"), "first-seen");
        assert_eq!(column_id("KB"), "kb");
        assert_eq!(column_id("  Needs  reboot? "), "needs-reboot");
        assert_eq!(column_id("OS / version"), "os-version");
    }

    #[test]
    fn a_bad_stored_value_hides_nothing() {
        assert!(parse_hidden_columns(None).is_empty());
        assert!(parse_hidden_columns(Some("")).is_empty());
        assert!(parse_hidden_columns(Some("not json")).is_empty());
        assert!(parse_hidden_columns(Some("{\"kb\":false}")).is_empty());
        assert!(parse_hidden_columns(Some("[1,2]")).is_empty());
    }

    #[test]
    fn round_trips_and_keeps_ids_this_build_does_not_know() {
        let hidden = set(&["kb", "a-column-from-a-newer-build"]);
        let back = parse_hidden_columns(Some(&serialize_hidden_columns(&hidden)));
        assert_eq!(back, hidden);
    }

    #[test]
    fn a_new_column_is_visible_by_default() {
        let hidden = set(&["kb"]);
        assert!(column_visible("product", &[], &hidden));
        assert!(!column_visible("kb", &[], &hidden));
    }

    #[test]
    fn required_columns_cannot_be_hidden() {
        let required = ["device", "patch"];
        // Not by the toggle...
        let mut hidden = BTreeSet::new();
        toggle_column(&mut hidden, "device", &required);
        assert!(hidden.is_empty());
        // ...and not by a hand-edited stored value either.
        let hidden = set(&["device"]);
        assert!(column_visible("device", &required, &hidden));
    }

    #[test]
    fn toggle_flips() {
        let mut hidden = BTreeSet::new();
        toggle_column(&mut hidden, "kb", &[]);
        assert_eq!(hidden, set(&["kb"]));
        toggle_column(&mut hidden, "kb", &[]);
        assert!(hidden.is_empty());
    }

    #[test]
    fn css_targets_positions_after_the_leading_columns() {
        let labels = ["Organization", "Device", "KB"];
        let css = hidden_columns_css("t", &labels, 1, &["device"], &set(&["kb", "device"]));
        // KB is the 3rd label after one leading column → 4th cell. Device is
        // required, so it is not hidden despite the stored value.
        assert_eq!(
            css,
            "table.t th:nth-child(4),table.t td:nth-child(4){display:none}"
        );
        assert_eq!(
            hidden_columns_css("t", &labels, 1, &[], &BTreeSet::new()),
            ""
        );
    }
}
