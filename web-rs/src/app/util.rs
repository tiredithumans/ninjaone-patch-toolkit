//! Small, standalone view helpers shared across the app components: option/date
//! parsing, number formatting, and CSS-class pickers. They touch no `AppState`, so
//! they live here rather than bloating `app.rs`. Most are JS-free and unit-test on
//! the host target; the two `js_sys::Date` helpers are the exception (browser only).

use super::Tab;

pub(crate) fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

pub(crate) fn parse_opt(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}

/// Parses a `yyyy-mm-dd` date string to Unix seconds (UTC midnight), or `None`.
pub(crate) fn date_to_epoch(date: &str) -> Option<i64> {
    let trimmed = date.trim();
    if trimmed.is_empty() {
        return None;
    }
    let ms = js_sys::Date::parse(trimmed);
    if ms.is_nan() {
        None
    } else {
        Some((ms / 1000.0) as i64)
    }
}

/// Formats Unix seconds back to a `yyyy-mm-dd` date string (UTC), or "" for `None`.
pub(crate) fn epoch_to_date(epoch: Option<i64>) -> String {
    let Some(e) = epoch else {
        return String::new();
    };
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64((e * 1000) as f64));
    d.to_iso_string()
        .as_string()
        .map(|s| s.chars().take(10).collect())
        .unwrap_or_default()
}

/// Formats a count with thousands separators (e.g. `12300` → `12,300`).
pub(crate) fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub(crate) fn tab_class(active: Tab, this: Tab) -> &'static str {
    if active == this { "tab tab-on" } else { "tab" }
}

pub(crate) fn sev_class(sev: &str) -> &'static str {
    match sev {
        "Critical" => "sev sev-critical",
        "Important" => "sev sev-important",
        "Moderate" => "sev sev-moderate",
        "Low" => "sev sev-low",
        _ => "sev sev-none",
    }
}

pub(crate) fn status_class(status: &str) -> &'static str {
    match status {
        "INSTALLED" => "stat stat-installed",
        "APPROVED" => "stat stat-approved",
        "PENDING" => "stat stat-pending",
        "REJECTED" => "stat stat-rejected",
        "FAILED" => "stat stat-failed",
        _ => "stat",
    }
}

// Host-target unit tests for the JS-free pure helpers. The wasm build excludes this
// module (`cfg(test)` is never set there); the date helpers call `js_sys::Date`,
// which only runs in the browser, so they're deliberately not covered here.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_thousands_inserts_separators() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(42), "42");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(12_300), "12,300");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn parse_opt_trims_and_rejects_non_numbers() {
        assert_eq!(parse_opt("  42 "), Some(42));
        assert_eq!(parse_opt("-7"), Some(-7));
        assert_eq!(parse_opt(""), None);
        assert_eq!(parse_opt("abc"), None);
    }

    #[test]
    fn non_empty_collapses_blank_to_none() {
        assert_eq!(non_empty("   ".to_string()), None);
        assert_eq!(non_empty(" hi ".to_string()), Some("hi".to_string()));
    }

    #[test]
    fn severity_and_status_classes_map_known_and_unknown_values() {
        assert_eq!(sev_class("Critical"), "sev sev-critical");
        assert_eq!(sev_class("nonsense"), "sev sev-none");
        assert_eq!(status_class("PENDING"), "stat stat-pending");
        assert_eq!(status_class("FAILED"), "stat stat-failed");
        assert_eq!(status_class("???"), "stat");
    }

    #[test]
    fn tab_class_marks_only_the_active_tab() {
        assert_eq!(tab_class(Tab::Patches, Tab::Patches), "tab tab-on");
        assert_eq!(tab_class(Tab::Patches, Tab::Reboot), "tab");
    }
}
