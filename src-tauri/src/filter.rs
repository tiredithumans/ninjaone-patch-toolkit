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
    /// Relative release-date window: keep patches released within the last N days.
    /// Resolved to `release_after` (absolute) at query time; stored relatively so a
    /// saved preset stays relative.
    #[serde(default)]
    pub release_within_days: Option<i64>,
    /// Absolute release-date bounds (Unix seconds) for a custom range; applied
    /// client-side against each patch's release timestamp.
    #[serde(default)]
    pub release_after: Option<i64>,
    #[serde(default)]
    pub release_before: Option<i64>,
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
        let classes: Vec<String> = self
            .node_classes
            .iter()
            .map(|c| c.trim().to_ascii_uppercase())
            .filter(|c| !c.is_empty())
            .collect();
        if !classes.is_empty() {
            match device.node_class.as_deref() {
                Some(nc) if classes.iter().any(|c| c == &nc.to_ascii_uppercase()) => {}
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
            release_after: self.release_after,
            release_before: self.release_before,
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
    release_after: Option<i64>,
    release_before: Option<i64>,
}

impl PreparedFilter {
    /// Case-insensitive substring match of the OS-name needle against a device's
    /// reported OS name. An inactive facet matches everything; an active one
    /// excludes a device that reports no OS name.
    pub fn os_name_allowed(&self, os_name: Option<&str>) -> bool {
        match &self.os_name_needle {
            None => true,
            Some(needle) => os_name
                .map(|n| n.to_ascii_lowercase().contains(needle.as_str()))
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
        let kb_lower = kb.map(|k| k.to_ascii_lowercase()).unwrap_or_default();
        let kb_bare = kb_lower.trim_start_matches("kb").trim();
        let name_lower = name.map(|n| n.to_ascii_lowercase()).unwrap_or_default();
        kb_lower.contains(needle.q_lower.as_str())
            || kb_bare.contains(needle.q_bare.as_str())
            || name_lower.contains(needle.q_lower.as_str())
    }

    /// True when the patch severity is among the selected set. An empty selection
    /// matches every severity.
    pub fn severity_allowed(&self, severity: Severity) -> bool {
        self.severities.is_empty() || self.severities.contains(&severity)
    }

    /// True when the patch's release timestamp (Unix seconds) falls within the
    /// configured `release_after`/`release_before` bounds. With no bounds set,
    /// everything matches; once a bound is set, an undated patch is excluded (its
    /// age can't be confirmed).
    pub fn release_date_allowed(&self, released_ts: Option<i64>) -> bool {
        if self.release_after.is_none() && self.release_before.is_none() {
            return true;
        }
        let Some(ts) = released_ts else {
            return false;
        };
        self.release_after.is_none_or(|a| ts >= a) && self.release_before.is_none_or(|b| ts <= b)
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
    fn release_date_bounds_filter_and_exclude_undated() {
        // No bounds → everything matches, including undated.
        let any = FilterParams::default().prepare();
        assert!(any.release_date_allowed(Some(1_700_000_000)));
        assert!(any.release_date_allowed(None));

        // after + before define an inclusive window; undated is excluded.
        let f = FilterParams {
            release_after: Some(1_000),
            release_before: Some(2_000),
            ..Default::default()
        }
        .prepare();
        assert!(f.release_date_allowed(Some(1_500)));
        assert!(f.release_date_allowed(Some(1_000)));
        assert!(f.release_date_allowed(Some(2_000)));
        assert!(!f.release_date_allowed(Some(999)));
        assert!(!f.release_date_allowed(Some(2_001)));
        assert!(!f.release_date_allowed(None));

        // after-only bound.
        let after = FilterParams {
            release_after: Some(1_000),
            ..Default::default()
        }
        .prepare();
        assert!(after.release_date_allowed(Some(5_000)));
        assert!(!after.release_date_allowed(Some(500)));
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
