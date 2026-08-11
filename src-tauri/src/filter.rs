use serde::{Deserialize, Serialize};

use crate::model::{Device, Severity};

/// Filter facets chosen by the operator in the UI. The device inventory and current
/// patches are prefetched **whole-fleet** and cached, so every identity facet
/// (org/location/role + the coarse OS-type `node_classes`) is applied **client-side**
/// against the cached devices via [`FilterParams::device_allowed`] — switching scope
/// re-filters the cache with no new round trip. The install-history queries, which
/// are fetched fresh per query, still narrow org/location/role server-side via
/// [`FilterParams::patch_filter`] (the `/queries/*` endpoints ignore `class`, so it
/// is reapplied client-side via the device join). `os_name_contains`, `search`, and
/// `severities` are applied client-side against patch rows after fetch.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FilterParams {
    pub organization_id: Option<i64>,
    pub location_id: Option<i64>,
    pub role_id: Option<i64>,
    /// NinjaOne node classes (e.g. `WINDOWS_SERVER`). The coarse "OS Type" facet.
    pub node_classes: Vec<String>,
    /// Granular OS-name substring, matched client-side against `device.os.name`.
    pub os_name_contains: Option<String>,
    /// Free-text query matched client-side against KB number and patch name.
    pub search: Option<String>,
    /// Patch severities to keep (raw strings like `CRITICAL`/`IMPORTANT`), matched
    /// client-side. NinjaOne's severity is its CVSS-derived bucket, so this doubles
    /// as the CVSS-band filter. Empty = all severities.
    pub severities: Vec<String>,
    /// Relative first-seen window: keep patches NinjaOne first reported within the
    /// last N days. Resolved to `detected_after` (absolute) at query time; stored
    /// relatively so a saved preset stays relative.
    #[serde(default)]
    pub detected_within_days: Option<i64>,
    /// Absolute first-seen bounds (Unix seconds) for a custom range; applied
    /// client-side against each patch's collection timestamp.
    #[serde(default)]
    pub detected_after: Option<i64>,
    #[serde(default)]
    pub detected_before: Option<i64>,
}

impl FilterParams {
    /// Identity clauses (org/location/role) shared by the device and patch filters.
    fn identity_clauses(&self) -> Vec<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(id) = self.organization_id {
            parts.push(format!("org = {id}"));
        }
        if let Some(id) = self.location_id {
            parts.push(format!("location = {id}"));
        }
        if let Some(id) = self.role_id {
            parts.push(format!("role = {id}"));
        }
        parts
    }

    /// Whether any identity facet (org/location/role/class) is active. When none is,
    /// the query spans the whole fleet and [`device_allowed`](Self::device_allowed)
    /// matches every device (so orphan patches whose device isn't in inventory are
    /// kept rather than scoped out).
    pub fn has_identity_scope(&self) -> bool {
        self.organization_id.is_some()
            || self.location_id.is_some()
            || self.role_id.is_some()
            || self.node_classes.iter().any(|c| !c.trim().is_empty())
    }

    /// Client-side identity match against a cached device: keeps it only when it
    /// satisfies every active facet (org / location / role / node-class). This is the
    /// device-query equivalent of the old `df` `class in (...)` + identity clauses,
    /// moved client-side so a scope change re-filters the whole-fleet cache without a
    /// refetch. An inactive facet matches everything; node-class compares
    /// case-insensitively.
    pub fn device_allowed(&self, device: &Device) -> bool {
        if let Some(org) = self.organization_id
            && device.organization_id != Some(org)
        {
            return false;
        }
        if let Some(loc) = self.location_id
            && device.location_id != Some(loc)
        {
            return false;
        }
        if let Some(role) = self.role_id
            && device.node_role_id != Some(role)
        {
            return false;
        }
        // Compared with `eq_ignore_ascii_case` rather than uppercasing both sides:
        // this runs once per device across the whole fleet, and the previous version
        // rebuilt the uppercased class list *and* re-uppercased `node_class` inside
        // the predicate — two allocations per device for a case-insensitive compare
        // that needs none. Same rule `prepare()` documents for the text needles.
        let mut classes = self
            .node_classes
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .peekable();
        if classes.peek().is_some() {
            match device.node_class.as_deref() {
                Some(nc) if classes.any(|c| c.eq_ignore_ascii_case(nc)) => {}
                _ => return false,
            }
        }
        true
    }

    /// Builds the `df` for the **install-history** queries (which are fetched fresh
    /// per query, not cached whole-fleet like the current-patch feed). NinjaOne's
    /// `/queries/*` endpoints don't honor `class` in `df` — passing it returns no
    /// rows even when matching devices exist — so the node-class facet is omitted
    /// here and applied client-side via the device join in `rows::build_rows`. Only
    /// the identity facets (which the query endpoints do honor) are sent server-side.
    pub fn patch_filter(&self) -> Option<String> {
        let parts = self.identity_clauses();
        (!parts.is_empty()).then(|| parts.join(" AND "))
    }

    /// Lowers the query needles and parses the severity strings **once** into a
    /// [`PreparedFilter`], which does the actual per-patch matching for
    /// `rows::build_rows`. Doing the lowering/parsing here rather than in the row
    /// loop avoids re-allocating the needles and re-parsing the severities on every
    /// row.
    pub fn prepare(&self) -> PreparedFilter {
        let os_name_needle = self
            .os_name_contains
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(str::to_ascii_lowercase);

        let search = self
            .search
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| {
                let q_lower = q.to_ascii_lowercase();
                let q_bare = q_lower.trim_start_matches("kb").trim().to_string();
                SearchNeedle { q_lower, q_bare }
            });

        PreparedFilter {
            os_name_needle,
            search,
            severities: self
                .severities
                .iter()
                .map(|s| Severity::from_raw(s))
                .collect(),
            detected_after: self.detected_after,
            detected_before: self.detected_before,
        }
    }
}

/// Pre-lowered free-text needle: the full lowercased query plus its `KB`-stripped
/// form, both computed once in [`FilterParams::prepare`].
struct SearchNeedle {
    q_lower: String,
    q_bare: String,
}

/// The client-side patch facets with their query needles lowercased and their
/// severities parsed up front, so matching a row costs no query-side allocation.
/// Built by [`FilterParams::prepare`]; consumed per row by `rows::build_rows`.
pub struct PreparedFilter {
    /// Trimmed, lowercased OS-name needle. `None` = facet inactive (match all).
    os_name_needle: Option<String>,
    /// Lowercased free-text needle. `None` = facet inactive (match all).
    search: Option<SearchNeedle>,
    /// Parsed severities to keep. Empty = all severities allowed.
    severities: Vec<Severity>,
    detected_after: Option<i64>,
    detected_before: Option<i64>,
}

impl PreparedFilter {
    /// Case-insensitive substring match of the OS-name needle against a device's
    /// reported OS name. An inactive facet matches everything; an active one
    /// excludes a device that reports no OS name.
    pub fn os_name_allowed(&self, os_name: Option<&str>) -> bool {
        match &self.os_name_needle {
            None => true,
            // Matched without lowercasing the haystack. This runs once per patch
            // across the whole fleet — a far larger N than `device_allowed`, where
            // the same per-call `String` was already removed for the same reason —
            // and the needle is pre-lowered by `prepare`, so the copy bought nothing.
            Some(needle) => os_name
                .map(|n| contains_ascii_ci(n, needle))
                .unwrap_or(false),
        }
    }

    /// Case-insensitive substring match of the free-text needle against the KB
    /// number and patch name. Accepts a `KB` prefix on either side (`KB5040434`
    /// matches a stored `5040434`). An inactive facet matches everything.
    pub fn search_allowed(&self, kb: Option<&str>, name: Option<&str>) -> bool {
        let Some(needle) = &self.search else {
            return true;
        };
        // Two more per-row `String`s removed: the needles are already lowered by
        // `prepare`, so the haystacks never needed a lowercased copy of their own.
        let kb = kb.unwrap_or_default();
        contains_ascii_ci(kb, &needle.q_lower)
            || contains_ascii_ci(strip_kb_prefix(kb), &needle.q_bare)
            || contains_ascii_ci(name.unwrap_or_default(), &needle.q_lower)
    }

    /// True when the patch severity is among the selected set. An empty selection
    /// matches every severity.
    pub fn severity_allowed(&self, severity: Severity) -> bool {
        self.severities.is_empty() || self.severities.contains(&severity)
    }

    /// True when the patch's first-seen timestamp (Unix seconds) falls within the
    /// configured `detected_after`/`detected_before` bounds. With no bounds set,
    /// everything matches; once a bound is set, an undated patch is excluded (its
    /// age can't be confirmed).
    pub fn detected_within_allowed(&self, first_seen_ts: Option<i64>) -> bool {
        if self.detected_after.is_none() && self.detected_before.is_none() {
            return true;
        }
        let Some(ts) = first_seen_ts else {
            return false;
        };
        self.detected_after.is_none_or(|a| ts >= a) && self.detected_before.is_none_or(|b| ts <= b)
    }
}

/// Case-insensitive (ASCII) substring test that allocates nothing.
///
/// `needle` must already be lowercase — every caller takes it from
/// [`FilterParams::prepare`], which lowers it once per query rather than once per
/// row. An empty needle matches, mirroring `str::contains("")`.
fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    h.len() >= n.len() && h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// A KB value with any leading `KB` removed and trimmed, borrowed from the input.
///
/// Mirrors the previous `to_ascii_lowercase().trim_start_matches("kb").trim()`
/// exactly, including stripping a repeated prefix and only trimming afterwards.
fn strip_kb_prefix(kb: &str) -> &str {
    let mut rest = kb;
    loop {
        match rest.get(..2) {
            Some(p) if p.eq_ignore_ascii_case("kb") => rest = &rest[2..],
            _ => return rest.trim(),
        }
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    /// The allocation-free matchers must behave exactly like the
    /// `to_ascii_lowercase().contains(..)` pair they replaced, including the
    /// boundary cases that only show up on odd data.
    #[test]
    fn case_insensitive_contains_matches_the_allocating_form() {
        for (haystack, needle, want) in [
            ("Windows Server 2022", "server", true),
            // The comparison is symmetric, so an un-lowered needle matches too. The
            // callers still pre-lower theirs in `prepare`, which is what keeps that
            // work out of the per-row path.
            ("Windows Server 2022", "SERVER", true),
            ("Windows", "windows", true),
            ("Windows Server", "linux", false),
            ("Win", "windows", false), // needle longer than haystack
            ("anything", "", true),    // mirrors `str::contains("")`
            ("", "x", false),
        ] {
            assert_eq!(
                contains_ascii_ci(haystack, needle),
                want,
                "{haystack:?} contains {needle:?}"
            );
        }
    }

    #[test]
    fn the_kb_prefix_is_stripped_case_insensitively_and_repeatedly() {
        assert_eq!(strip_kb_prefix("KB5040434"), "5040434");
        assert_eq!(strip_kb_prefix("kb5040434"), "5040434");
        assert_eq!(strip_kb_prefix("5040434"), "5040434");
        // `trim_start_matches` stripped a repeated prefix; this keeps that.
        assert_eq!(strip_kb_prefix("KBkb123"), "123");
        // Trimming happens after stripping, as it did before.
        assert_eq!(strip_kb_prefix(" KB123 "), "KB123");
        // A multi-byte leading character must not panic on the 2-byte slice probe.
        assert_eq!(strip_kb_prefix("é123"), "é123");
        assert_eq!(strip_kb_prefix(""), "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(org: i64, location: i64, role: i64, class: &str) -> Device {
        Device {
            id: 1,
            system_name: None,
            display_name: None,
            organization_id: Some(org),
            location_id: Some(location),
            node_role_id: Some(role),
            node_class: Some(class.into()),
            offline: None,
            os: None,
        }
    }

    #[test]
    fn empty_state_has_no_identity_scope_and_allows_every_device() {
        let f = FilterParams::default();
        assert!(!f.has_identity_scope());
        assert!(f.device_allowed(&device(7, 2, 3, "WINDOWS_SERVER")));
    }

    #[test]
    fn device_allowed_matches_each_identity_facet() {
        let f = FilterParams {
            organization_id: Some(7),
            ..Default::default()
        };
        assert!(f.has_identity_scope());
        assert!(f.device_allowed(&device(7, 2, 3, "WINDOWS_SERVER")));
        assert!(!f.device_allowed(&device(8, 2, 3, "WINDOWS_SERVER")));

        // Every active facet must match (AND semantics, like the old `df` clauses).
        let all = FilterParams {
            organization_id: Some(1),
            location_id: Some(2),
            role_id: Some(3),
            node_classes: vec!["windows_server".into(), "LINUX_SERVER".into()],
            ..Default::default()
        };
        assert!(all.device_allowed(&device(1, 2, 3, "WINDOWS_SERVER")));
        assert!(all.device_allowed(&device(1, 2, 3, "linux_server"))); // class case-insensitive
        assert!(!all.device_allowed(&device(1, 2, 3, "MAC"))); // class not in set
        assert!(!all.device_allowed(&device(1, 99, 3, "WINDOWS_SERVER"))); // wrong location
    }

    #[test]
    fn class_only_scope_drops_class_from_the_install_df() {
        // A class-only selection is an active scope, but `class` can't go in the
        // install-history `df` (the /queries/* endpoints ignore it), so patch_filter
        // is whole-fleet (None) and the class is reapplied via device_allowed.
        let f = FilterParams {
            node_classes: vec!["LINUX_SERVER".into()],
            ..Default::default()
        };
        assert!(f.has_identity_scope());
        assert!(f.patch_filter().is_none());
        assert!(f.device_allowed(&device(1, 2, 3, "LINUX_SERVER")));
        assert!(!f.device_allowed(&device(1, 2, 3, "WINDOWS_SERVER")));
    }

    #[test]
    fn patch_filter_keeps_identity_but_omits_node_class() {
        // The install-history query keeps identity facets but drops `class`.
        let f = FilterParams {
            organization_id: Some(1),
            location_id: Some(2),
            role_id: Some(3),
            node_classes: vec!["LINUX_SERVER".into()],
            ..Default::default()
        };
        assert_eq!(
            f.patch_filter().as_deref(),
            Some("org = 1 AND location = 2 AND role = 3")
        );
    }

    #[test]
    fn os_name_substring_is_case_insensitive() {
        let p = FilterParams {
            os_name_contains: Some("server 2022".into()),
            ..Default::default()
        }
        .prepare();
        assert!(p.os_name_allowed(Some("Windows Server 2022")));
        assert!(!p.os_name_allowed(Some("Windows Server 2019")));
        assert!(!p.os_name_allowed(None));
    }

    #[test]
    fn search_matches_kb_with_or_without_prefix() {
        let p = FilterParams {
            search: Some("KB5040434".into()),
            ..Default::default()
        }
        .prepare();
        assert!(p.search_allowed(Some("5040434"), None));
        assert!(p.search_allowed(Some("KB5040434"), None));
        assert!(!p.search_allowed(Some("5036893"), None));
    }

    #[test]
    fn empty_search_allows_all() {
        assert!(FilterParams::default().prepare().search_allowed(None, None));
    }

    #[test]
    fn first_seen_bounds_filter_and_exclude_undated() {
        // No bounds → everything matches, including undated.
        let any = FilterParams::default().prepare();
        assert!(any.detected_within_allowed(Some(1_700_000_000)));
        assert!(any.detected_within_allowed(None));

        // after + before define an inclusive window; undated is excluded.
        let f = FilterParams {
            detected_after: Some(1_000),
            detected_before: Some(2_000),
            ..Default::default()
        }
        .prepare();
        assert!(f.detected_within_allowed(Some(1_500)));
        assert!(f.detected_within_allowed(Some(1_000)));
        assert!(f.detected_within_allowed(Some(2_000)));
        assert!(!f.detected_within_allowed(Some(999)));
        assert!(!f.detected_within_allowed(Some(2_001)));
        assert!(!f.detected_within_allowed(None));

        // after-only bound.
        let after = FilterParams {
            detected_after: Some(1_000),
            ..Default::default()
        }
        .prepare();
        assert!(after.detected_within_allowed(Some(5_000)));
        assert!(!after.detected_within_allowed(Some(500)));
    }

    #[test]
    fn severity_facet_reaches_ninjaones_own_classifications() {
        use crate::model::Severity;
        // The reported bug: selecting any severity made every third-party patch
        // disappear. NinjaOne grades them `security` / `recommended` / `unknown`,
        // none of which `from_raw` mapped, so all three collapsed onto Unknown and
        // no facet selection could ever match them.
        let p = FilterParams {
            severities: vec!["SECURITY".into(), "RECOMMENDED".into()],
            ..Default::default()
        }
        .prepare();
        assert!(p.severity_allowed(Severity::from_raw("security")));
        assert!(p.severity_allowed(Severity::from_raw("recommended")));
        assert!(!p.severity_allowed(Severity::from_raw("critical")));
        // An unrated patch is now reachable too, instead of being excluded by every
        // possible selection.
        let unrated = FilterParams {
            severities: vec!["UNKNOWN".into()],
            ..Default::default()
        }
        .prepare();
        assert!(unrated.severity_allowed(Severity::from_raw("unknown")));
        assert!(unrated.severity_allowed(Severity::from_raw("no-such-grade")));
        assert!(!unrated.severity_allowed(Severity::from_raw("security")));
    }

    #[test]
    fn severity_filter_keeps_only_selected() {
        use crate::model::Severity;
        let p = FilterParams {
            severities: vec!["CRITICAL".into(), "IMPORTANT".into()],
            ..Default::default()
        }
        .prepare();
        assert!(p.severity_allowed(Severity::Critical));
        assert!(p.severity_allowed(Severity::Important));
        assert!(!p.severity_allowed(Severity::Low));
        assert!(!p.severity_allowed(Severity::Unknown));
        // Empty selection matches everything.
        assert!(
            FilterParams::default()
                .prepare()
                .severity_allowed(Severity::Low)
        );
    }

    #[test]
    fn prepare_trims_needles_and_kb_prefix_is_bidirectional() {
        // Whitespace around a needle is trimmed before matching.
        let os = FilterParams {
            os_name_contains: Some("  server 2022 ".into()),
            ..Default::default()
        }
        .prepare();
        assert!(os.os_name_allowed(Some("Windows Server 2022")));

        // A bare query matches a `KB`-prefixed stored value (and the free-text
        // needle also matches against the patch name, not just the KB).
        let bare = FilterParams {
            search: Some("5040434".into()),
            ..Default::default()
        }
        .prepare();
        assert!(bare.search_allowed(Some("KB5040434"), None));
        let by_name = FilterParams {
            search: Some("cumulative".into()),
            ..Default::default()
        }
        .prepare();
        assert!(by_name.search_allowed(None, Some("Cumulative Update")));
    }
}
