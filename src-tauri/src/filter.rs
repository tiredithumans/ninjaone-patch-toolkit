use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{Device, Severity};

/// Filter facets chosen by the operator in the UI. The device inventory and current
/// patches are prefetched **whole-fleet** and cached, so every identity facet
/// (org/location/role + the coarse OS-type `node_classes`) is applied **client-side**
/// against the cached devices via [`FilterParams::device_allowed`] — switching scope
/// re-filters the cache with no new round trip. The install-history queries, which
/// are fetched fresh per query, additionally narrow org/location/role server-side via
/// [`FilterParams::patch_filter`] (the `df`), purely as a bandwidth optimization: the
/// client-side scope is the authoritative one and is reapplied to every joined row.
/// `os_name_contains`, `search`, and `severities` are applied client-side against
/// patch rows after fetch.
///
/// The three identity facets are **multi-select**: each holds zero or more ids, where
/// empty means "every one of them". They deserialize from either a bare id or a list
/// (see [`ids`]) so a preset saved when they were single-valued still loads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FilterParams {
    /// Selected organizations; empty = every organization.
    #[serde(alias = "organizationId", deserialize_with = "ids")]
    pub organization_ids: Vec<i64>,
    /// Selected locations; empty = every location.
    #[serde(alias = "locationId", deserialize_with = "ids")]
    pub location_ids: Vec<i64>,
    /// Selected device roles; empty = every role.
    #[serde(alias = "roleId", deserialize_with = "ids")]
    pub role_ids: Vec<i64>,
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

/// Deserializes an id facet from `null`, a bare id, or a list of ids, then sorts and
/// dedupes it.
///
/// The scalar form is what makes the multi-select change backward compatible: these
/// three facets were `Option<i64>` (`organizationId`/`locationId`/`roleId`), and both
/// `settings.json` presets and any stale frontend still send that shape. Normalizing
/// here rather than at each use site means the `df` clause, the confirm-token style
/// equality checks and the chip row all see one canonical form — `[2, 1, 2]` and
/// `[1, 2]` are the same scope and must not read as different ones.
fn ids<'de, D>(deserializer: D) -> Result<Vec<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(i64),
        Many(Vec<i64>),
    }

    let mut out = match Option::<OneOrMany>::deserialize(deserializer)? {
        None => Vec::new(),
        Some(OneOrMany::One(id)) => vec![id],
        Some(OneOrMany::Many(v)) => v,
    };
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

impl FilterParams {
    /// Identity clauses (org/location/role) shared by the device and patch filters.
    ///
    /// The field tokens, the spacing and the list form are NinjaOne's, not ours. The
    /// documented device-filter grammar is `org=<id>` / `org in (<id1>, <id2>, …)`,
    /// with its worked example `df=class%3DWINDOWS_SERVER%20AND%20offline` — spaces
    /// around `AND`, none around `=`. And the location facet is spelled **`loc`**;
    /// `location`, which this used to emit, is not a token the grammar defines at
    /// all. See `Ninja RMM Public API Device Filter Syntax`.
    fn identity_clauses(&self) -> Vec<String> {
        [
            ("org", &self.organization_ids),
            ("loc", &self.location_ids),
            ("role", &self.role_ids),
        ]
        .into_iter()
        .filter_map(|(field, ids)| id_clause(field, ids))
        .collect()
    }

    /// Whether any device-scope facet (org/location/role/class/OS name) is active.
    /// When none is, the query spans the whole fleet and
    /// [`PreparedFilter::device_allowed`] matches every device (so orphan patches
    /// whose device isn't in inventory are kept rather than scoped out).
    ///
    /// The OS-name substring counts here because it is a *device* predicate: the
    /// filter panel files it under "Device scope", and it is matched against
    /// `device.os.name`, not against anything on the patch.
    pub fn has_identity_scope(&self) -> bool {
        !self.organization_ids.is_empty()
            || !self.location_ids.is_empty()
            || !self.role_ids.is_empty()
            || self.node_classes.iter().any(|c| !c.trim().is_empty())
            || self
                .os_name_contains
                .as_deref()
                .is_some_and(|n| !n.trim().is_empty())
    }

    /// Builds the `df` for the **install-history** queries (which are fetched fresh
    /// per query, not cached whole-fleet like the current-patch feed). NinjaOne's
    /// `/queries/*` endpoints don't honor `class` in `df` — passing it returns no
    /// rows even when matching devices exist — so the node-class facet is omitted
    /// here and applied client-side via the device join in `rows::build_rows`. Only
    /// the identity facets (which the query endpoints do honor) are sent server-side.
    ///
    /// This clause is a **bandwidth optimization, not the scope boundary**:
    /// `rows::build_rows` re-checks every joined row against the same client-side
    /// scope `device_allowed` applies to the cached feeds. A `df` the server declines
    /// to honor therefore costs a larger download, never a wider result.
    pub fn patch_filter(&self) -> Option<String> {
        let parts = self.identity_clauses();
        (!parts.is_empty()).then(|| parts.join(" AND "))
    }

    /// Lowers the query needles and parses the severity strings **once** into a
    /// [`PreparedFilter`], which does the actual per-patch matching for
    /// `rows::build_rows`. Doing the lowering/parsing here rather than in the row
    /// loop avoids re-allocating the needles and re-parsing the severities on every
    /// row.
    pub fn prepare(&self) -> PreparedFilter<'_> {
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
            organization_ids: &self.organization_ids,
            location_ids: &self.location_ids,
            role_ids: &self.role_ids,
            node_classes: &self.node_classes,
            has_scope: self.has_identity_scope(),
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

/// Renders one identity facet as a `df` clause, in the form NinjaOne documents for
/// the number of ids selected. `None` when the facet is inactive.
///
/// A single id keeps the `field = id` form rather than a one-element `in (…)`: it is
/// the form that has always been sent, and the list form is only needed once there
/// is more than one id to send.
fn id_clause(field: &str, ids: &[i64]) -> Option<String> {
    match ids {
        [] => None,
        [one] => Some(format!("{field}={one}")),
        many => {
            // Normalized here as well as in [`ids`], so the clause is canonical for
            // any `FilterParams`, however it was built — not only for one that came
            // through serde. Three short sorts per query is not a cost worth trading
            // for two different requests that mean the same scope.
            let mut sorted = many.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            let list = sorted
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("{field} in ({list})"))
        }
    }
}

/// Whether a device's id for one facet satisfies the selection: an empty selection
/// matches everything, and a device that reports no id for an *active* facet is
/// excluded (it cannot be shown to be in scope).
fn id_allowed(selected: &[i64], value: Option<i64>) -> bool {
    selected.is_empty() || value.is_some_and(|v| selected.contains(&v))
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
pub struct PreparedFilter<'a> {
    organization_ids: &'a [i64],
    location_ids: &'a [i64],
    role_ids: &'a [i64],
    node_classes: &'a [String],
    /// Cached [`FilterParams::has_identity_scope`] — read once per patch in the row
    /// join, which is the largest loop in the app.
    has_scope: bool,
    /// Trimmed, lowercased OS-name needle. `None` = facet inactive (match all).
    os_name_needle: Option<String>,
    /// Lowercased free-text needle. `None` = facet inactive (match all).
    search: Option<SearchNeedle>,
    /// Parsed severities to keep. Empty = all severities allowed.
    severities: Vec<Severity>,
    detected_after: Option<i64>,
    detected_before: Option<i64>,
}

impl PreparedFilter<'_> {
    /// Whether any device-scope facet is active; see
    /// [`FilterParams::has_identity_scope`].
    pub fn has_scope(&self) -> bool {
        self.has_scope
    }

    /// Client-side device-scope match against a cached device: keeps it only when it
    /// satisfies every active facet (org / location / role / node-class / OS name).
    ///
    /// This is the device-query equivalent of the old `df` `class in (...)` +
    /// identity clauses, moved client-side so a scope change re-filters the
    /// whole-fleet cache without a refetch. An inactive facet matches everything;
    /// within one facet the selected ids are OR'd (matching the `in (…)` clause it
    /// mirrors) while the facets themselves are AND'd; node-class compares
    /// case-insensitively.
    ///
    /// The OS-name substring is matched **here**, not only per patch row. It reads as
    /// device scope everywhere it is presented — the filter panel files it under
    /// "Device scope" and the applied-filter chip stays undimmed on the fleet-health
    /// tabs — but it used to be applied only inside the row join, so the device
    /// count, the compliance rollups and the Needs-Reboot list all covered the whole
    /// scoped fleet while the chip on screen said otherwise.
    pub fn device_allowed(&self, device: &Device) -> bool {
        if !id_allowed(self.organization_ids, device.organization_id) {
            return false;
        }
        if !id_allowed(self.location_ids, device.location_id) {
            return false;
        }
        if !id_allowed(self.role_ids, device.node_role_id) {
            return false;
        }
        // Compared with `eq_ignore_ascii_case` rather than uppercasing both sides:
        // this runs once per device across the whole fleet, and the previous version
        // rebuilt the uppercased class list *and* re-uppercased `node_class` inside
        // the predicate — two allocations per device for a case-insensitive compare
        // that needs none. Same rule the text needles follow.
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
        self.os_name_allowed(device.os_name_str())
    }

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

    /// `device_allowed` lives on the prepared filter now (it needs the lowered
    /// OS-name needle), so the tests go through `prepare()` the way a query does.
    fn allows(f: &FilterParams, d: &Device) -> bool {
        f.prepare().device_allowed(d)
    }

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
        assert!(allows(&f, &device(7, 2, 3, "WINDOWS_SERVER")));
    }

    #[test]
    fn device_allowed_matches_each_identity_facet() {
        let f = FilterParams {
            organization_ids: vec![7],
            ..Default::default()
        };
        assert!(f.has_identity_scope());
        assert!(allows(&f, &device(7, 2, 3, "WINDOWS_SERVER")));
        assert!(!allows(&f, &device(8, 2, 3, "WINDOWS_SERVER")));

        // Every active facet must match (AND semantics, like the old `df` clauses).
        let all = FilterParams {
            organization_ids: vec![1],
            location_ids: vec![2],
            role_ids: vec![3],
            node_classes: vec!["windows_server".into(), "LINUX_SERVER".into()],
            ..Default::default()
        };
        assert!(allows(&all, &device(1, 2, 3, "WINDOWS_SERVER")));
        assert!(allows(&all, &device(1, 2, 3, "linux_server"))); // class case-insensitive
        assert!(!allows(&all, &device(1, 2, 3, "MAC"))); // class not in set
        assert!(!allows(&all, &device(1, 99, 3, "WINDOWS_SERVER"))); // wrong location
    }

    /// Multi-select semantics: ids *within* a facet are OR'd, facets are AND'd. This
    /// is the whole point of the plural facets — picking two orgs must widen the
    /// scope to their union, not narrow it to devices belonging to both (which no
    /// device can be).
    #[test]
    fn selected_ids_are_ored_within_a_facet_and_anded_across_them() {
        let f = FilterParams {
            organization_ids: vec![1, 2],
            role_ids: vec![3, 4],
            ..Default::default()
        };
        assert!(allows(&f, &device(1, 9, 3, "MAC")));
        assert!(allows(&f, &device(2, 9, 4, "MAC")));
        // In one facet but not the other.
        assert!(!allows(&f, &device(3, 9, 3, "MAC")));
        assert!(!allows(&f, &device(1, 9, 5, "MAC")));
    }

    /// A device that reports no id for an *active* facet can't be shown to be in
    /// scope, so it is excluded rather than admitted by default.
    #[test]
    fn a_device_missing_the_facets_id_is_out_of_scope() {
        let mut d = device(1, 2, 3, "MAC");
        d.location_id = None;
        let f = FilterParams {
            location_ids: vec![2],
            ..Default::default()
        };
        assert!(!allows(&f, &d));
        // …but only when that facet is active.
        assert!(allows(&FilterParams::default(), &d));
    }

    /// The OS-name substring is device scope, not a patch facet: it decides which
    /// *devices* the query covers, so it must reach `device_allowed` (and therefore
    /// the device count and every fleet-health rollup), not only the row join.
    #[test]
    fn the_os_name_substring_scopes_devices() {
        let f = FilterParams {
            os_name_contains: Some("server 2022".into()),
            ..Default::default()
        };
        assert!(
            f.has_identity_scope(),
            "an OS-name needle is an active device scope"
        );

        let mut win22 = device(1, 2, 3, "WINDOWS_SERVER");
        win22.os = Some(crate::model::OsInfo {
            name: Some("Windows Server 2022".into()),
            needs_reboot: None,
        });
        let mut win19 = device(1, 2, 3, "WINDOWS_SERVER");
        win19.os = Some(crate::model::OsInfo {
            name: Some("Windows Server 2019".into()),
            needs_reboot: None,
        });
        assert!(allows(&f, &win22));
        assert!(!allows(&f, &win19));
        // A device reporting no OS name can't be shown to match.
        assert!(!allows(&f, &device(1, 2, 3, "WINDOWS_SERVER")));
        // Blank/whitespace-only needles stay inactive rather than excluding everything.
        let blank = FilterParams {
            os_name_contains: Some("   ".into()),
            ..Default::default()
        };
        assert!(!blank.has_identity_scope());
        assert!(allows(&blank, &device(1, 2, 3, "WINDOWS_SERVER")));
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
        assert!(allows(&f, &device(1, 2, 3, "LINUX_SERVER")));
        assert!(!allows(&f, &device(1, 2, 3, "WINDOWS_SERVER")));
    }

    /// The `df` tokens are NinjaOne's, and a wrong one is invisible: the server
    /// either rejects the whole filter or ignores the clause and hands back a wider
    /// fleet than the operator asked for. The location facet in particular was
    /// spelled `location`, which the documented grammar does not define — it is
    /// `loc`.
    #[test]
    fn patch_filter_uses_the_documented_field_tokens_and_omits_node_class() {
        let f = FilterParams {
            organization_ids: vec![1],
            location_ids: vec![2],
            role_ids: vec![3],
            node_classes: vec!["LINUX_SERVER".into()],
            ..Default::default()
        };
        assert_eq!(
            f.patch_filter().as_deref(),
            Some("org=1 AND loc=2 AND role=3")
        );
    }

    /// Two or more ids use NinjaOne's documented list form. Sending three separate
    /// `org = n` clauses joined by AND would match nothing at all.
    #[test]
    fn several_ids_render_as_the_documented_in_list() {
        let f = FilterParams {
            organization_ids: vec![3, 1, 2],
            location_ids: vec![9],
            ..Default::default()
        };
        assert_eq!(
            f.patch_filter().as_deref(),
            // Ids are normalized (sorted, deduped) on the way out as well as on the
            // way in, so the clause is stable regardless of the order the operator
            // ticked them.
            Some("org in (1, 2, 3) AND loc=9")
        );
    }

    /// Presets are persisted as `FilterParams` in `settings.json`, and the three
    /// identity facets used to be scalars. A preset saved then must still load — and
    /// load as the same scope, not as an empty one (which would silently widen it to
    /// the whole fleet).
    #[test]
    fn scalar_id_facets_still_deserialize_from_older_presets() {
        let legacy: FilterParams = serde_json::from_str(
            r#"{"organizationId": 7, "locationId": null, "roleId": 3, "nodeClasses": []}"#,
        )
        .expect("legacy preset shape must still parse");
        assert_eq!(legacy.organization_ids, vec![7]);
        assert!(legacy.location_ids.is_empty());
        assert_eq!(legacy.role_ids, vec![3]);

        // The current shape parses too, and normalizes duplicates/ordering.
        let current: FilterParams =
            serde_json::from_str(r#"{"organizationIds": [3, 1, 3], "roleIds": []}"#).unwrap();
        assert_eq!(current.organization_ids, vec![1, 3]);
        assert!(current.role_ids.is_empty());

        // A missing facet is simply inactive.
        let empty: FilterParams = serde_json::from_str("{}").unwrap();
        assert!(!empty.has_identity_scope());
    }

    #[test]
    fn os_name_substring_is_case_insensitive() {
        let f = FilterParams {
            os_name_contains: Some("server 2022".into()),
            ..Default::default()
        };
        let p = f.prepare();
        assert!(p.os_name_allowed(Some("Windows Server 2022")));
        assert!(!p.os_name_allowed(Some("Windows Server 2019")));
        assert!(!p.os_name_allowed(None));
    }

    #[test]
    fn search_matches_kb_with_or_without_prefix() {
        let f = FilterParams {
            search: Some("KB5040434".into()),
            ..Default::default()
        };
        let p = f.prepare();
        assert!(p.search_allowed(Some("5040434"), None));
        assert!(p.search_allowed(Some("KB5040434"), None));
        assert!(!p.search_allowed(Some("5036893"), None));
    }

    #[test]
    fn empty_search_allows_all() {
        let empty = FilterParams::default();
        assert!(empty.prepare().search_allowed(None, None));
    }

    #[test]
    fn first_seen_bounds_filter_and_exclude_undated() {
        // No bounds → everything matches, including undated.
        let default_params = FilterParams::default();
        let any = default_params.prepare();
        assert!(any.detected_within_allowed(Some(1_700_000_000)));
        assert!(any.detected_within_allowed(None));

        // after + before define an inclusive window; undated is excluded.
        let bounded = FilterParams {
            detected_after: Some(1_000),
            detected_before: Some(2_000),
            ..Default::default()
        };
        let f = bounded.prepare();
        assert!(f.detected_within_allowed(Some(1_500)));
        assert!(f.detected_within_allowed(Some(1_000)));
        assert!(f.detected_within_allowed(Some(2_000)));
        assert!(!f.detected_within_allowed(Some(999)));
        assert!(!f.detected_within_allowed(Some(2_001)));
        assert!(!f.detected_within_allowed(None));

        // after-only bound.
        let after_params = FilterParams {
            detected_after: Some(1_000),
            ..Default::default()
        };
        let after = after_params.prepare();
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
        let sel = FilterParams {
            severities: vec!["SECURITY".into(), "RECOMMENDED".into()],
            ..Default::default()
        };
        let p = sel.prepare();
        assert!(p.severity_allowed(Severity::from_raw("security")));
        assert!(p.severity_allowed(Severity::from_raw("recommended")));
        assert!(!p.severity_allowed(Severity::from_raw("critical")));
        // An unrated patch is now reachable too, instead of being excluded by every
        // possible selection.
        let unrated_params = FilterParams {
            severities: vec!["UNKNOWN".into()],
            ..Default::default()
        };
        let unrated = unrated_params.prepare();
        assert!(unrated.severity_allowed(Severity::from_raw("unknown")));
        assert!(unrated.severity_allowed(Severity::from_raw("no-such-grade")));
        assert!(!unrated.severity_allowed(Severity::from_raw("security")));
    }

    #[test]
    fn severity_filter_keeps_only_selected() {
        use crate::model::Severity;
        let sel = FilterParams {
            severities: vec!["CRITICAL".into(), "IMPORTANT".into()],
            ..Default::default()
        };
        let p = sel.prepare();
        assert!(p.severity_allowed(Severity::Critical));
        assert!(p.severity_allowed(Severity::Important));
        assert!(!p.severity_allowed(Severity::Low));
        assert!(!p.severity_allowed(Severity::Unknown));
        // Empty selection matches everything.
        let all = FilterParams::default();
        assert!(all.prepare().severity_allowed(Severity::Low));
    }

    #[test]
    fn prepare_trims_needles_and_kb_prefix_is_bidirectional() {
        // Whitespace around a needle is trimmed before matching.
        let os_params = FilterParams {
            os_name_contains: Some("  server 2022 ".into()),
            ..Default::default()
        };
        let os = os_params.prepare();
        assert!(os.os_name_allowed(Some("Windows Server 2022")));

        // A bare query matches a `KB`-prefixed stored value (and the free-text
        // needle also matches against the patch name, not just the KB).
        let bare_params = FilterParams {
            search: Some("5040434".into()),
            ..Default::default()
        };
        let bare = bare_params.prepare();
        assert!(bare.search_allowed(Some("KB5040434"), None));
        let by_name_params = FilterParams {
            search: Some("cumulative".into()),
            ..Default::default()
        };
        let by_name = by_name_params.prepare();
        assert!(by_name.search_allowed(None, Some("Cumulative Update")));
    }
}
