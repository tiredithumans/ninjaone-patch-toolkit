//! Small, standalone view helpers shared across the app components: option/date
//! parsing, number formatting, and CSS-class pickers. They touch no `AppState`, so
//! they live here rather than bloating `app.rs`. Most are JS-free and unit-test on
//! the host target; the two `js_sys::Date` helpers are the exception (browser only).

use super::{AppliedFilters, Tab};

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

/// The counts the tier-aware results summary needs — all already on `QueryResult`,
/// so the summary can describe whichever tab is active without extra backend data.
pub(crate) struct SummaryCounts {
    pub rows_total: usize,
    pub devices_total: usize,
    pub failures: usize,
    pub orgs: usize,
    pub reboot: usize,
}

/// Builds the results-summary line for the active tab. The old shared line read
/// "{rows} patch rows …" across every tab, which is the Patches detail-row count —
/// misleading on Compliance/Reboot, whose scope is devices, not patch rows.
pub(crate) fn summary_line(tab: Tab, c: &SummaryCounts, generated_at: &str) -> String {
    let head = match tab {
        Tab::Patches => format!(
            "{} patch rows across {} devices",
            group_thousands(c.rows_total),
            group_thousands(c.devices_total),
        ),
        Tab::Failures => format!(
            "{} failing patches across {} devices",
            group_thousands(c.failures),
            group_thousands(c.devices_total),
        ),
        Tab::Compliance => format!(
            "{} organizations \u{00b7} {} devices",
            group_thousands(c.orgs),
            group_thousands(c.devices_total),
        ),
        Tab::Reboot => format!(
            "{} of {} devices need reboot",
            group_thousands(c.reboot),
            group_thousands(c.devices_total),
        ),
    };
    format!("{head} \u{00b7} generated {generated_at}")
}

/// Fleet-health tabs (Compliance, Needs Reboot) reflect the device scope only and
/// ignore the patch filters; Filtered-results tabs (Patches, Failures) honor them.
pub(crate) fn is_fleet_tab(tab: Tab) -> bool {
    matches!(tab, Tab::Compliance | Tab::Reboot)
}

/// One applied-filter chip. `patch` marks a patch-tier facet so the view can grey it
/// out on Fleet-health tabs, where patch filters don't apply.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FilterChip {
    pub label: String,
    pub patch: bool,
}

/// Humanizes the release-date filter into a chip label, or `None` when no window is
/// set. Pure (no `js_sys`), unlike `date_to_epoch`/`epoch_to_date`, so it host-tests.
pub(crate) fn release_label(window: &str, after: &str, before: &str) -> Option<String> {
    match window {
        "1" => Some("last 24 hours".to_string()),
        "7" => Some("last 7 days".to_string()),
        "30" => Some("last 30 days".to_string()),
        "90" => Some("last 90 days".to_string()),
        "custom" => {
            let (a, b) = (after.trim(), before.trim());
            match (a.is_empty(), b.is_empty()) {
                (false, false) => Some(format!("{a} \u{2192} {b}")),
                (false, true) => Some(format!("after {a}")),
                (true, false) => Some(format!("before {b}")),
                (true, true) => None,
            }
        }
        _ => None,
    }
}

/// Builds the glanceable chip row from a Run-time snapshot — one chip per non-default
/// facet (an empty vec ⇒ the caller shows a "whole fleet" placeholder). Device-scope
/// facets come first (`patch: false`), then the patch-tier facets (`patch: true`).
pub(crate) fn filter_chips(f: &AppliedFilters) -> Vec<FilterChip> {
    let mut out = Vec::new();
    if let Some(o) = &f.organization {
        out.push(FilterChip {
            label: format!("Org: {o}"),
            patch: false,
        });
    }
    if let Some(l) = &f.location {
        out.push(FilterChip {
            label: format!("Location: {l}"),
            patch: false,
        });
    }
    if let Some(r) = &f.role {
        out.push(FilterChip {
            label: format!("Role: {r}"),
            patch: false,
        });
    }
    if !f.os_types.is_empty() {
        out.push(FilterChip {
            label: format!("OS Type: {}", f.os_types.join(", ")),
            patch: false,
        });
    }
    if let Some(n) = &f.os_name {
        out.push(FilterChip {
            label: format!("OS name: {n}"),
            patch: false,
        });
    }
    if matches!(f.patch_type.as_str(), "OS" | "SOFTWARE") {
        out.push(FilterChip {
            label: format!("Type: {}", f.patch_type),
            patch: true,
        });
    }
    if !f.statuses.is_empty() {
        out.push(FilterChip {
            label: format!("Status: {}", f.statuses.join(", ")),
            patch: true,
        });
    }
    if !f.severities.is_empty() {
        out.push(FilterChip {
            label: format!("Severity: {}", f.severities.join(", ")),
            patch: true,
        });
    }
    if let Some(s) = &f.search {
        out.push(FilterChip {
            label: format!("Search: {s}"),
            patch: true,
        });
    }
    if let Some(rl) = release_label(&f.release_window, &f.release_after, &f.release_before) {
        out.push(FilterChip {
            label: format!("Released: {rl}"),
            patch: true,
        });
    }
    if let Some(d) = f.install_days {
        out.push(FilterChip {
            label: format!("Installed within {d}d"),
            patch: true,
        });
    }
    out
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

    #[test]
    fn summary_line_is_tab_aware() {
        let c = SummaryCounts {
            rows_total: 12_300,
            devices_total: 540,
            failures: 7,
            orgs: 3,
            reboot: 12,
        };
        assert_eq!(
            summary_line(Tab::Patches, &c, "2026-06-28"),
            "12,300 patch rows across 540 devices \u{00b7} generated 2026-06-28"
        );
        assert_eq!(
            summary_line(Tab::Failures, &c, "2026-06-28"),
            "7 failing patches across 540 devices \u{00b7} generated 2026-06-28"
        );
        assert_eq!(
            summary_line(Tab::Compliance, &c, "2026-06-28"),
            "3 organizations \u{00b7} 540 devices \u{00b7} generated 2026-06-28"
        );
        assert_eq!(
            summary_line(Tab::Reboot, &c, "2026-06-28"),
            "12 of 540 devices need reboot \u{00b7} generated 2026-06-28"
        );
    }

    #[test]
    fn is_fleet_tab_flags_compliance_and_reboot() {
        assert!(is_fleet_tab(Tab::Compliance));
        assert!(is_fleet_tab(Tab::Reboot));
        assert!(!is_fleet_tab(Tab::Patches));
        assert!(!is_fleet_tab(Tab::Failures));
    }

    #[test]
    fn release_label_humanizes_each_window() {
        assert_eq!(
            release_label("1", "", ""),
            Some("last 24 hours".to_string())
        );
        assert_eq!(release_label("7", "", ""), Some("last 7 days".to_string()));
        assert_eq!(
            release_label("30", "", ""),
            Some("last 30 days".to_string())
        );
        assert_eq!(
            release_label("90", "", ""),
            Some("last 90 days".to_string())
        );
        assert_eq!(release_label("", "", ""), None);
        assert_eq!(
            release_label("custom", "2026-01-01", "2026-02-01"),
            Some("2026-01-01 \u{2192} 2026-02-01".to_string())
        );
        assert_eq!(
            release_label("custom", "2026-01-01", ""),
            Some("after 2026-01-01".to_string())
        );
        assert_eq!(
            release_label("custom", "", "2026-02-01"),
            Some("before 2026-02-01".to_string())
        );
        assert_eq!(release_label("custom", "", ""), None);
    }

    #[test]
    fn filter_chips_emits_only_non_default_facets() {
        // A default snapshot (no facets, ALL/empty) yields no chips.
        assert!(filter_chips(&AppliedFilters::default()).is_empty());

        let scope_only = AppliedFilters {
            organization: Some("Acme".to_string()),
            patch_type: "ALL".to_string(),
            ..Default::default()
        };
        let chips = filter_chips(&scope_only);
        assert_eq!(chips.len(), 1);
        assert_eq!(chips[0].label, "Org: Acme");
        assert!(!chips[0].patch);

        let full = AppliedFilters {
            organization: Some("Acme".to_string()),
            location: Some("HQ".to_string()),
            role: Some("Server".to_string()),
            os_types: vec!["Windows Server".to_string()],
            os_name: Some("2022".to_string()),
            patch_type: "OS".to_string(),
            statuses: vec!["INSTALLED".to_string()],
            severities: vec!["Critical".to_string()],
            search: Some("KB5040434".to_string()),
            release_window: "7".to_string(),
            release_after: String::new(),
            release_before: String::new(),
            install_days: Some(30),
        };
        let chips = filter_chips(&full);
        let labels: Vec<&str> = chips.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Org: Acme",
                "Location: HQ",
                "Role: Server",
                "OS Type: Windows Server",
                "OS name: 2022",
                "Type: OS",
                "Status: INSTALLED",
                "Severity: Critical",
                "Search: KB5040434",
                "Released: last 7 days",
                "Installed within 30d",
            ]
        );
        // The first five facets are device-scope; the rest are patch-tier.
        assert!(chips.iter().take(5).all(|c| !c.patch));
        assert!(chips.iter().skip(5).all(|c| c.patch));
    }
}
