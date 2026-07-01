//! Small, standalone view helpers shared across the app components: option/date
//! parsing, number formatting, and CSS-class pickers. They touch no `AppState`, so
//! they live here rather than bloating `app.rs`. Most are JS-free and unit-test on
//! the host target; the two `js_sys::Date` helpers are the exception (browser only).

use std::cmp::Ordering;

use super::{AppliedFilters, Tab};
use crate::types::{PatchRow, RowSort, RowSortKey};

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

/// Next state when a sortable column header is clicked: none → asc → desc → none
/// on the same key; clicking a different key starts it ascending.
pub(crate) fn next_sort(current: Option<RowSort>, key: RowSortKey) -> Option<RowSort> {
    match current {
        Some(s) if s.key == key && !s.desc => Some(RowSort { key, desc: true }),
        Some(s) if s.key == key => None,
        _ => Some(RowSort { key, desc: false }),
    }
}

/// `aria-sort` value for a column header under the current sort.
pub(crate) fn aria_sort(sort: Option<RowSort>, key: RowSortKey) -> &'static str {
    match sort {
        Some(s) if s.key == key => {
            if s.desc {
                "descending"
            } else {
                "ascending"
            }
        }
        _ => "none",
    }
}

/// Direction glyph suffix for a sorted column header ("" when not sorted by it).
pub(crate) fn sort_glyph(sort: Option<RowSort>, key: RowSortKey) -> &'static str {
    match sort {
        Some(s) if s.key == key => {
            if s.desc {
                " ▼"
            } else {
                " ▲"
            }
        }
        _ => "",
    }
}

/// Client-side counterpart of the backend row sort, for demo mode only (the demo
/// holds its full sample locally; there is no backend cache to re-page from).
/// Deliberately mirrors `src-tauri/src/rows.rs::compare_rows` — duplicating small
/// logic across the crates is the sanctioned pattern (no shared crate over wasm).
pub(crate) fn sort_patch_rows(rows: &mut [PatchRow], sort: RowSort) {
    rows.sort_by(|a, b| compare_rows(a, b, sort));
}

fn compare_rows(a: &PatchRow, b: &PatchRow, sort: RowSort) -> Ordering {
    use RowSortKey::*;
    let dir = |o: Ordering| if sort.desc { o.reverse() } else { o };
    match sort.key {
        Organization => dir(cmp_ci(&a.organization, &b.organization)),
        Location => cmp_opt_last(a.location.as_deref(), b.location.as_deref(), sort.desc),
        Role => cmp_opt_last(
            a.device_role.as_deref(),
            b.device_role.as_deref(),
            sort.desc,
        ),
        Device => dir(cmp_ci(&a.device_name, &b.device_name)),
        Os => cmp_opt_last(a.os_name.as_deref(), b.os_name.as_deref(), sort.desc),
        PatchType => dir(a.patch_type.cmp(&b.patch_type)),
        Kb => cmp_opt_last(a.kb.as_deref(), b.kb.as_deref(), sort.desc),
        Name => dir(cmp_ci(&a.name, &b.name)),
        // Most urgent first on ascending, like the backend's presentation ordinal.
        Severity => dir(sev_ordinal(&a.severity).cmp(&sev_ordinal(&b.severity))),
        Status => dir(a.status.cmp(&b.status)),
        // The mirror carries dates as ISO `yyyy-mm-dd` strings — lexicographic
        // order is chronological.
        ReleaseDate => cmp_opt_last(
            a.release_date.as_deref(),
            b.release_date.as_deref(),
            sort.desc,
        ),
        InstalledDate => cmp_opt_last(
            a.installed_date.as_deref(),
            b.installed_date.as_deref(),
            sort.desc,
        ),
    }
}

/// Severity ordinal (0 = most urgent), matching the backend's rank order.
fn sev_ordinal(sev: &str) -> u8 {
    match sev {
        "Critical" => 0,
        "Important" => 1,
        "Moderate" => 2,
        "Low" => 3,
        "Optional" => 4,
        _ => 5,
    }
}

/// Case-insensitive (ASCII) ordering without a per-comparison allocation.
fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// Missing values sort last regardless of direction (blanks never lead).
fn cmp_opt_last(a: Option<&str>, b: Option<&str>, desc: bool) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => {
            let o = cmp_ci(x, y);
            if desc { o.reverse() } else { o }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Presentation for an "Aged (past SLA)" table cell as (CSS class, label, title).
/// An aged backlog gets a ⚠ prefix so it reads without relying on color.
pub(crate) fn aged_badge(aged: usize) -> (&'static str, String, &'static str) {
    if aged > 0 {
        (
            "sev-critical",
            format!("⚠ {aged}"),
            "Past SLA — needs attention",
        )
    } else {
        ("", aged.to_string(), "")
    }
}

/// One inline run within a changelog line: plain text or a `**bold**` span.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MdSpan {
    Text(String),
    Strong(String),
}

/// One rendered block of the update changelog (a `CHANGELOG.md` version section).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MdBlock {
    /// A `#`/`##`/`###` section heading (e.g. "Added", "Fixed").
    Heading(String),
    /// A bullet list; each item is its sequence of inline spans.
    List(Vec<Vec<MdSpan>>),
    /// A free-text paragraph (the GitHub fallback note, or any non-list text).
    Paragraph(Vec<MdSpan>),
}

/// Splits one line into `**bold**` and plain-text runs. An unterminated `**` is
/// left as literal text so we never drop content.
pub(crate) fn parse_inline(text: &str) -> Vec<MdSpan> {
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("**") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("**") else {
            break; // no closing marker — the rest is plain text
        };
        if open > 0 {
            spans.push(MdSpan::Text(rest[..open].to_string()));
        }
        let bold = &after[..close];
        if !bold.is_empty() {
            spans.push(MdSpan::Strong(bold.to_string()));
        }
        rest = &after[close + 2..];
    }
    if !rest.is_empty() {
        spans.push(MdSpan::Text(rest.to_string()));
    }
    spans
}

/// Parses the changelog subset the updater notes use — `#` headings, `-`/`*` bullet
/// lists (wrapped continuation lines fold into the bullet), `**bold**`, and plain
/// paragraphs — into renderable blocks. Anything unrecognized falls through as text,
/// so the GitHub fallback note ("See the release notes …") renders as a paragraph.
pub(crate) fn parse_changelog(src: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let mut items: Vec<String> = Vec::new(); // raw text of the bullets in the open list
    let mut para: Vec<String> = Vec::new(); // raw lines of the open paragraph

    for raw in src.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        if trimmed.is_empty() {
            flush_para(&mut blocks, &mut para);
            flush_list(&mut blocks, &mut items);
        } else if trimmed.starts_with('#') {
            flush_para(&mut blocks, &mut para);
            flush_list(&mut blocks, &mut items);
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() {
                blocks.push(MdBlock::Heading(heading.to_string()));
            }
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            flush_para(&mut blocks, &mut para); // a list and paragraph never overlap
            items.push(item.trim().to_string());
        } else if let Some(last) = items.last_mut() {
            // A non-blank, non-marker line under a bullet is a wrapped continuation.
            last.push(' ');
            last.push_str(trimmed);
        } else {
            para.push(trimmed.to_string());
        }
    }
    flush_para(&mut blocks, &mut para);
    flush_list(&mut blocks, &mut items);
    blocks
}

fn flush_list(blocks: &mut Vec<MdBlock>, items: &mut Vec<String>) {
    if !items.is_empty() {
        let spans = items.drain(..).map(|i| parse_inline(&i)).collect();
        blocks.push(MdBlock::List(spans));
    }
}

fn flush_para(blocks: &mut Vec<MdBlock>, para: &mut Vec<String>) {
    if !para.is_empty() {
        let text = para.join(" ");
        para.clear();
        blocks.push(MdBlock::Paragraph(parse_inline(&text)));
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
    fn next_sort_cycles_none_asc_desc_none() {
        let key = RowSortKey::Device;
        let asc = next_sort(None, key);
        assert_eq!(asc, Some(RowSort { key, desc: false }));
        let desc = next_sort(asc, key);
        assert_eq!(desc, Some(RowSort { key, desc: true }));
        assert_eq!(next_sort(desc, key), None);
        // A different key restarts ascending.
        assert_eq!(
            next_sort(desc, RowSortKey::Kb),
            Some(RowSort {
                key: RowSortKey::Kb,
                desc: false
            })
        );
    }

    #[test]
    fn aria_sort_and_glyph_follow_the_active_key() {
        let key = RowSortKey::Severity;
        assert_eq!(aria_sort(None, key), "none");
        let asc = Some(RowSort { key, desc: false });
        assert_eq!(aria_sort(asc, key), "ascending");
        assert_eq!(aria_sort(asc, RowSortKey::Kb), "none");
        assert_eq!(sort_glyph(asc, key), " ▲");
        assert_eq!(sort_glyph(Some(RowSort { key, desc: true }), key), " ▼");
        assert_eq!(sort_glyph(asc, RowSortKey::Kb), "");
    }

    fn sortable(device: &str, sev: &str, installed: Option<&str>) -> PatchRow {
        PatchRow {
            device_name: device.into(),
            organization: "Org".into(),
            location: None,
            device_role: None,
            os_name: None,
            patch_type: "OS".into(),
            kb: None,
            name: "Patch".into(),
            severity: sev.into(),
            status: "PENDING".into(),
            release_date: None,
            installed_date: installed.map(Into::into),
        }
    }

    #[test]
    fn sort_patch_rows_matches_backend_semantics() {
        // Severity ascending surfaces the most urgent first.
        let mut rows = vec![
            sortable("low", "Low", None),
            sortable("crit", "Critical", None),
            sortable("mod", "Moderate", None),
        ];
        sort_patch_rows(
            &mut rows,
            RowSort {
                key: RowSortKey::Severity,
                desc: false,
            },
        );
        let names: Vec<_> = rows.iter().map(|r| r.device_name.as_str()).collect();
        assert_eq!(names, ["crit", "mod", "low"]);

        // Missing dates sort last even on a descending sort.
        let mut rows = vec![
            sortable("a", "Low", Some("2026-01-05")),
            sortable("b", "Low", None),
            sortable("c", "Low", Some("2026-03-01")),
        ];
        sort_patch_rows(
            &mut rows,
            RowSort {
                key: RowSortKey::InstalledDate,
                desc: true,
            },
        );
        let names: Vec<_> = rows.iter().map(|r| r.device_name.as_str()).collect();
        assert_eq!(names, ["c", "a", "b"]);
    }

    #[test]
    fn aged_badge_flags_only_nonzero_backlogs() {
        assert_eq!(aged_badge(0), ("", "0".to_string(), ""));
        assert_eq!(
            aged_badge(3),
            (
                "sev-critical",
                "⚠ 3".to_string(),
                "Past SLA — needs attention"
            )
        );
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
    fn parse_inline_splits_bold_runs() {
        assert_eq!(parse_inline("plain"), vec![MdSpan::Text("plain".into())]);
        assert_eq!(
            parse_inline("**Lead.** then text"),
            vec![
                MdSpan::Strong("Lead.".into()),
                MdSpan::Text(" then text".into()),
            ]
        );
        assert_eq!(
            parse_inline("a **b** c **d**"),
            vec![
                MdSpan::Text("a ".into()),
                MdSpan::Strong("b".into()),
                MdSpan::Text(" c ".into()),
                MdSpan::Strong("d".into()),
            ]
        );
        // An unterminated marker stays literal so no content is dropped.
        assert_eq!(
            parse_inline("trailing **oops"),
            vec![MdSpan::Text("trailing **oops".into())]
        );
    }

    #[test]
    fn parse_changelog_handles_headings_lists_and_wrapped_bullets() {
        let src = "### Added\n\n- **Compliance by OS.** A per-OS\n  bar chart and table.\n- Second item.\n\n### Fixed\n\n- A fix.";
        assert_eq!(
            parse_changelog(src),
            vec![
                MdBlock::Heading("Added".into()),
                MdBlock::List(vec![
                    vec![
                        MdSpan::Strong("Compliance by OS.".into()),
                        // The wrapped continuation line folds into the bullet.
                        MdSpan::Text(" A per-OS bar chart and table.".into()),
                    ],
                    vec![MdSpan::Text("Second item.".into())],
                ]),
                MdBlock::Heading("Fixed".into()),
                MdBlock::List(vec![vec![MdSpan::Text("A fix.".into())]]),
            ]
        );
    }

    #[test]
    fn parse_changelog_treats_plain_text_as_a_paragraph() {
        // The GitHub fallback note has no markdown markers.
        assert_eq!(
            parse_changelog("See the release notes on GitHub for what's new in v1.2.3."),
            vec![MdBlock::Paragraph(vec![MdSpan::Text(
                "See the release notes on GitHub for what's new in v1.2.3.".into()
            )])]
        );
        assert!(parse_changelog("   \n\n  ").is_empty());
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
