//! `QueryScope`: the provenance block both exports print, built from the
//! `QueryPlan` the fetch actually ran under rather than from the request.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::filter::FilterParams;
use crate::model::PatchStatus;

use super::*;

/// The facets that produced a result, resolved to the names the operator chose them
/// by — the provenance block both exports print.
///
/// Derived backend-side from the `QueryPlan` the fetch actually ran under, **not**
/// taken from the request or from the frontend's `AppliedFilters` chip row. Those
/// describe what was *selected*; this describes what the query *did*, which is the
/// distinction that matters once a workbook has been emailed to someone who never
/// saw the app. It is the same rule the write path follows for the same reason: the
/// backend re-derives rather than trusting a caller's description of its own request.
///
/// Carried on [`QueryResult`] **only**, deliberately not on [`QuerySummary`]: the
/// frontend already has `AppliedFilters` (a Run-time snapshot with its own chip
/// rendering), so shipping a second copy over IPC would add a wire field with no
/// reader and a second thing to keep in step.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryScope {
    /// `(facet, value)` in display order for the facets that reach **every** sheet
    /// and section: the device scope and the patch type. Never empty — the patch
    /// type always applies.
    pub facets: Vec<(&'static str, String)>,
    /// The facets that narrow only the detail rows — status, severity, search, the
    /// first-seen window and the install lookback — and so reach the Patches and
    /// Patch Failures sheets but **not** the compliance, severity, age or reboot
    /// sections, which are computed from the unnarrowed current feed. Kept apart
    /// so the exports can say which is which: the in-app Compliance tab dims these
    /// chips with "Ignored on this tab", and a workbook that listed them beside the
    /// Compliance sheet with no such note read as "45 pending critical KB5040434
    /// installs". Never empty — the status selection always applies.
    pub patch_facets: Vec<(&'static str, String)>,
}

/// Renders one id facet as a comma-separated name list.
///
/// An id the lookups can't resolve prints as `id 42` rather than the rows'
/// `(unknown)`: two unresolved ids would otherwise render as `(unknown), (unknown)`,
/// which says neither how many were selected nor which.
fn id_facet(ids: &[i64], names: &HashMap<i64, String>) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let mut out: Vec<String> = ids
        .iter()
        .map(|id| names.get(id).cloned().unwrap_or_else(|| format!("id {id}")))
        .collect();
    out.sort_by(|a, b| cmp_ci(a, b));
    Some(out.join(", "))
}

/// Builds the provenance block from the plan a query ran under.
///
/// `install_history_after` is `Some` only when the status selection actually reached
/// the `*-patch-installs` endpoints; printing a lookback window on a Pending-only
/// query would describe a fetch that never happened.
pub fn build_query_scope(
    filter: &FilterParams,
    maps: &LookupMaps,
    families: PatchFamilies,
    statuses: &[PatchStatus],
    install_history_after: Option<i64>,
) -> QueryScope {
    let mut facets: Vec<(&'static str, String)> = Vec::new();

    let organizations = id_facet(&filter.organization_ids, &maps.orgs);
    let locations = id_facet(&filter.location_ids, &maps.locations);
    let roles = id_facet(&filter.role_ids, &maps.roles);
    let os_types = (!filter.node_classes.is_empty()).then(|| filter.node_classes.join(", "));
    let severities = (!filter.severities.is_empty()).then(|| filter.severities.join(", "));
    // Absolute bounds, because a report is read long after the day it was run:
    // "the last 30 days" silently re-anchors to whenever the reader happens to
    // look, while a timestamp does not. The relative window rides along in
    // parentheses when that is what the operator picked, so the line still matches
    // the control they used.
    // `from_timestamp` rather than `model::unix_to_datetime`: these three bounds are
    // composed backend-side as Unix *seconds* (`QueryPlan::build`), never read off a
    // NinjaOne record, so the millisecond normalization that helper exists for
    // cannot apply here.
    let stamp = |ts: i64| {
        DateTime::<Utc>::from_timestamp(ts, 0).map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
    };
    let detected_after =
        filter
            .detected_after
            .and_then(stamp)
            .map(|when| match filter.detected_within_days {
                Some(days) => format!("{when} (last {days} days)"),
                None => when,
            });
    let detected_before = filter.detected_before.and_then(stamp);

    // Stated rather than left to inference: on a printed artifact the absence of
    // narrowing lines is indistinguishable from a renderer that dropped them.
    let narrowed = [
        &organizations,
        &locations,
        &roles,
        &os_types,
        &severities,
        &detected_after,
        &detected_before,
        &filter.os_name_contains,
        &filter.search,
    ]
    .iter()
    .any(|f| f.is_some());
    if !narrowed {
        facets.push((
            "Scope",
            "Whole fleet \u{2014} no device or patch filters applied".to_string(),
        ));
    }

    for (label, value) in [
        ("Organizations", organizations),
        ("Locations", locations),
        ("Device roles", roles),
        ("OS type", os_types),
        ("OS name contains", filter.os_name_contains.clone()),
    ] {
        if let Some(value) = value {
            facets.push((label, value));
        }
    }

    facets.push(("Patch type", families.label().to_string()));

    let mut patch_facets = vec![(
        "Status",
        statuses
            .iter()
            .map(|s| s.label())
            .collect::<Vec<_>>()
            .join(", "),
    )];
    for (label, value) in [
        ("Severity", severities),
        ("Search", filter.search.clone()),
        ("First seen after", detected_after),
        ("First seen before", detected_before),
        (
            "Install history since",
            install_history_after.and_then(stamp),
        ),
    ] {
        if let Some(value) = value {
            patch_facets.push((label, value));
        }
    }

    QueryScope {
        facets,
        patch_facets,
    }
}
