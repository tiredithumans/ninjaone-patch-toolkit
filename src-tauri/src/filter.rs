use serde::{Deserialize, Serialize};

use crate::model::Severity;

/// Filter facets chosen by the operator in the UI. Identity facets (org/location/
/// role) go into the `df` for both the device and patch queries; the coarse OS-type
/// facet (`node_classes`) goes into the device `df` only (the patch `/queries/*`
/// endpoints ignore `class`) and is reapplied client-side via the device join.
/// `os_name_contains`, `search`, and `severities` are applied client-side against
/// patch rows after fetch.
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

    /// Builds the NinjaOne `df` DSL for the **device** query from the identity
    /// facets plus the coarse OS-type (node-class) facet. Returns `None` when no
    /// server-side facet is selected (query the whole fleet).
    pub fn device_filter(&self) -> Option<String> {
        let mut parts = self.identity_clauses();
        let classes: Vec<String> = self
            .node_classes
            .iter()
            .map(|c| c.trim().to_ascii_uppercase())
            .filter(|c| !c.is_empty())
            .collect();
        if !classes.is_empty() {
            parts.push(format!("class in ({})", classes.join(", ")));
        }
        (!parts.is_empty()).then(|| parts.join(" AND "))
    }

    /// Builds the `df` for the **patch / install** queries. NinjaOne's `/queries/*`
    /// endpoints don't honor `class` in `df` — passing it returns no rows even when
    /// matching devices exist — so the node-class facet is omitted here and applied
    /// client-side via the device join in `rows::build_rows`. Only the identity
    /// facets (which the query endpoints do honor) are sent server-side.
    pub fn patch_filter(&self) -> Option<String> {
        let parts = self.identity_clauses();
        (!parts.is_empty()).then(|| parts.join(" AND "))
    }

    /// Case-insensitive substring match of the OS-name sub-filter against a device's
    /// reported OS name. Empty/unset filter matches everything.
    pub fn os_name_allowed(&self, os_name: Option<&str>) -> bool {
        match self.os_name_contains.as_deref().map(str::trim) {
            None | Some("") => true,
            Some(q) => os_name
                .map(|n| n.to_ascii_lowercase().contains(&q.to_ascii_lowercase()))
                .unwrap_or(false),
        }
    }

    /// Case-insensitive substring match against KB number and patch name. Accepts a
    /// `KB` prefix on either side (`KB5040434` matches a stored `5040434`).
    pub fn search_allowed(&self, kb: Option<&str>, name: Option<&str>) -> bool {
        let Some(q) = self
            .search
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
        else {
            return true;
        };
        let q_lower = q.to_ascii_lowercase();
        let q_bare = q_lower.trim_start_matches("kb").trim();
        let kb_lower = kb.map(|k| k.to_ascii_lowercase()).unwrap_or_default();
        let kb_bare = kb_lower.trim_start_matches("kb").trim();
        let name_lower = name.map(|n| n.to_ascii_lowercase()).unwrap_or_default();
        kb_lower.contains(&q_lower) || kb_bare.contains(q_bare) || name_lower.contains(&q_lower)
    }

    /// True when a patch's severity is among the selected severities. An empty
    /// selection matches everything.
    pub fn severity_allowed(&self, severity: Severity) -> bool {
        if self.severities.is_empty() {
            return true;
        }
        self.severities
            .iter()
            .any(|s| Severity::from_raw(s) == severity)
    }

    /// True when a patch's release timestamp (Unix seconds) falls within the
    /// `release_after`/`release_before` bounds. With no bounds set, everything
    /// matches; with a bound set, a patch with no release date is excluded (its age
    /// can't be confirmed). The relative `release_within_days` is resolved into
    /// `release_after` before this is called.
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

    #[test]
    fn empty_state_yields_none() {
        assert!(FilterParams::default().device_filter().is_none());
    }

    #[test]
    fn single_org_clause() {
        let f = FilterParams {
            organization_id: Some(7),
            ..Default::default()
        };
        assert_eq!(f.device_filter().as_deref(), Some("org = 7"));
    }

    #[test]
    fn combined_identity_and_class_clauses() {
        let f = FilterParams {
            organization_id: Some(1),
            location_id: Some(2),
            role_id: Some(3),
            node_classes: vec!["windows_server".into(), "LINUX_SERVER".into()],
            ..Default::default()
        };
        assert_eq!(
            f.device_filter().as_deref(),
            Some(
                "org = 1 AND location = 2 AND role = 3 AND class in (WINDOWS_SERVER, LINUX_SERVER)"
            )
        );
    }

    #[test]
    fn patch_filter_omits_node_class() {
        // The patch query keeps identity facets but drops `class` (the /queries/*
        // endpoints ignore it), so the node-class facet is applied client-side.
        let f = FilterParams {
            organization_id: Some(1),
            node_classes: vec!["LINUX_SERVER".into()],
            ..Default::default()
        };
        assert_eq!(
            f.device_filter().as_deref(),
            Some("org = 1 AND class in (LINUX_SERVER)")
        );
        assert_eq!(f.patch_filter().as_deref(), Some("org = 1"));

        // Class-only selection → patch filter is whole-fleet (None); class is
        // reconstructed via the device join.
        let g = FilterParams {
            node_classes: vec!["LINUX_SERVER".into()],
            ..Default::default()
        };
        assert_eq!(
            g.device_filter().as_deref(),
            Some("class in (LINUX_SERVER)")
        );
        assert!(g.patch_filter().is_none());
    }

    #[test]
    fn class_only_filter() {
        let f = FilterParams {
            node_classes: vec!["WINDOWS_WORKSTATION".into()],
            ..Default::default()
        };
        assert_eq!(
            f.device_filter().as_deref(),
            Some("class in (WINDOWS_WORKSTATION)")
        );
    }

    #[test]
    fn os_name_substring_is_case_insensitive() {
        let f = FilterParams {
            os_name_contains: Some("server 2022".into()),
            ..Default::default()
        };
        assert!(f.os_name_allowed(Some("Windows Server 2022")));
        assert!(!f.os_name_allowed(Some("Windows Server 2019")));
        assert!(!f.os_name_allowed(None));
    }

    #[test]
    fn search_matches_kb_with_or_without_prefix() {
        let f = FilterParams {
            search: Some("KB5040434".into()),
            ..Default::default()
        };
        assert!(f.search_allowed(Some("5040434"), None));
        assert!(f.search_allowed(Some("KB5040434"), None));
        assert!(!f.search_allowed(Some("5036893"), None));
    }

    #[test]
    fn empty_search_allows_all() {
        let f = FilterParams::default();
        assert!(f.search_allowed(None, None));
    }

    #[test]
    fn release_date_bounds_filter_and_exclude_undated() {
        // No bounds → everything matches, including undated.
        let any = FilterParams::default();
        assert!(any.release_date_allowed(Some(1_700_000_000)));
        assert!(any.release_date_allowed(None));

        // after + before define an inclusive window; undated is excluded.
        let f = FilterParams {
            release_after: Some(1_000),
            release_before: Some(2_000),
            ..Default::default()
        };
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
        };
        assert!(after.release_date_allowed(Some(5_000)));
        assert!(!after.release_date_allowed(Some(500)));
    }

    #[test]
    fn severity_filter_keeps_only_selected() {
        use crate::model::Severity;
        let f = FilterParams {
            severities: vec!["CRITICAL".into(), "IMPORTANT".into()],
            ..Default::default()
        };
        assert!(f.severity_allowed(Severity::Critical));
        assert!(f.severity_allowed(Severity::Important));
        assert!(!f.severity_allowed(Severity::Low));
        assert!(!f.severity_allowed(Severity::Unknown));
        // Empty selection matches everything.
        assert!(FilterParams::default().severity_allowed(Severity::Low));
    }
}
