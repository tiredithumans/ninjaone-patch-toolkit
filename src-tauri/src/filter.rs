use serde::{Deserialize, Serialize};

use crate::model::Severity;

/// Filter facets chosen by the operator in the UI. Identity facets (org/location/
/// role) and the coarse OS-type facet (`node_classes`) are pushed into the NinjaOne
/// `df` device-filter DSL; `os_name_contains`, `search`, and `severities` are
/// applied client-side against patch rows after fetch.
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
}

impl FilterParams {
    /// Builds the NinjaOne `df` DSL string from the identity + node-class facets.
    /// Returns `None` when no server-side facet is selected (query the whole fleet).
    pub fn device_filter(&self) -> Option<String> {
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
        let classes: Vec<String> = self
            .node_classes
            .iter()
            .map(|c| c.trim().to_ascii_uppercase())
            .filter(|c| !c.is_empty())
            .collect();
        if !classes.is_empty() {
            parts.push(format!("class in ({})", classes.join(", ")));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" AND "))
        }
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
