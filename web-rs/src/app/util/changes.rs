//! "Changes since the previous run" and the Trend tab's run-over-run deltas: the
//! panel's wording, and the per-organization delta table's rows and order.

use std::cmp::Ordering;

use crate::types::{OrgRun, RunChanges, RunRecord};

use super::{group_thousands, trend_delta_label, trend_verdict};

/// Entries per change list the backend ships (`changes::CHANGE_LIST_LIMIT`).
pub(crate) const CHANGE_LIST_LIMIT: usize = 200;

/// The panel's heading. Names the baseline, because "changes" alone does not say
/// since when — and a re-run of the same scope moves it.
pub(crate) fn changes_title(c: &RunChanges) -> String {
    match &c.previous_at {
        Some(at) => format!("Changes since {at}"),
        None => "Changes since the previous run".to_string(),
    }
}

/// The caveats that decide how the three counts may be read. Mirrors the backend's
/// `RunChanges::notes`, which both exports print; the crates share no code.
pub(crate) fn changes_notes(c: &RunChanges, list_limit: usize) -> Vec<String> {
    let mut notes = Vec::new();
    if c.previous_at.is_none() {
        notes.push(
            "No previous comparable run \u{2014} changes are reported from the next run of this \
             scope."
                .to_string(),
        );
    }
    if c.too_large {
        notes.push(
            "This scope is too large to remember, so the next run has no baseline to compare \
             against."
                .to_string(),
        );
    }
    if c.previous_at.is_none() {
        return notes;
    }
    if !c.tracks_pending {
        notes.push(
            "New and resolved are not measured: the status selection includes neither Pending \
             nor Approved."
                .to_string(),
        );
    } else {
        notes.push(
            "Resolved means no longer pending \u{2014} installed, rejected, or no longer in scope."
                .to_string(),
        );
    }
    if !c.tracks_failed {
        notes.push(
            "Failed is not selected, so an install that failed reads as resolved and newly \
             failed is not measured."
                .to_string(),
        );
    }
    if [c.new_pending, c.resolved, c.newly_failed]
        .iter()
        .any(|&n| n > list_limit)
    {
        notes.push(format!(
            "Each list shows at most {list_limit} entries, worst severity first; the counts are \
             exact."
        ));
    }
    notes
}

/// The caption under a capped change list, or `None` when it is complete.
pub(crate) fn change_list_caption(shown: usize, total: usize) -> Option<String> {
    (shown < total).then(|| {
        format!(
            "Showing {} of {}, worst severity first.",
            group_thousands(shown),
            group_thousands(total)
        )
    })
}

/// The change since the run before the last one, over values oldest first. `None`
/// with fewer than two values.
pub(crate) fn delta_vs_previous(values: &[f64]) -> Option<f64> {
    match values {
        [.., prev, last] => Some(last - prev),
        _ => None,
    }
}

/// One organization's movement between the previous comparable run and the newest.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OrgDelta {
    pub organization: String,
    /// `None` when the org is absent from that run.
    pub now: Option<OrgRun>,
    pub previous: Option<OrgRun>,
}

impl OrgDelta {
    /// Percentage points; `None` unless both runs have a percentage.
    pub(crate) fn compliance_delta(&self) -> Option<f64> {
        Some(self.now.as_ref()?.compliance_pct()? - self.previous.as_ref()?.compliance_pct()?)
    }

    fn count_delta(&self, get: fn(&OrgRun) -> usize) -> Option<f64> {
        Some(get(self.now.as_ref()?) as f64 - get(self.previous.as_ref()?) as f64)
    }

    pub(crate) fn pending_delta(&self) -> Option<f64> {
        self.count_delta(|o| o.pending)
    }

    pub(crate) fn pending_critical_delta(&self) -> Option<f64> {
        self.count_delta(|o| o.pending_critical)
    }

    pub(crate) fn aged_delta(&self) -> Option<f64> {
        self.count_delta(|o| o.aged_critical)
    }
}

/// The per-organization delta table: every org in either run, the biggest
/// regression first.
///
/// "Regression" is ranked by compliance lost, then aged criticals gained, then
/// pending criticals gained, then pending gained — the order an operator would
/// triage in. An org present in only one run has no deltas and sorts after every
/// org that does; an org's name breaks every tie so the table does not reshuffle
/// between renders.
pub(crate) fn org_deltas(newest: &RunRecord, previous: &RunRecord) -> Vec<OrgDelta> {
    let mut out: Vec<OrgDelta> = newest
        .orgs
        .iter()
        .map(|now| OrgDelta {
            organization: now.organization.clone(),
            now: Some(now.clone()),
            previous: previous
                .orgs
                .iter()
                .find(|p| p.organization == now.organization)
                .cloned(),
        })
        .collect();
    out.extend(
        previous
            .orgs
            .iter()
            .filter(|p| !newest.orgs.iter().any(|n| n.organization == p.organization))
            .map(|p| OrgDelta {
                organization: p.organization.clone(),
                now: None,
                previous: Some(p.clone()),
            }),
    );
    // Worse first: a *lower* compliance delta and *higher* count deltas.
    let key = |d: &OrgDelta| {
        [
            d.compliance_delta().map(|v| -v),
            d.aged_delta(),
            d.pending_critical_delta(),
            d.pending_delta(),
        ]
    };
    out.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        let both = a.now.is_some() && a.previous.is_some();
        let both_b = b.now.is_some() && b.previous.is_some();
        both_b
            .cmp(&both)
            .then_with(|| {
                ka.iter()
                    .zip(kb.iter())
                    .map(|(x, y)| desc_none_last(*x, *y))
                    .find(|o| o.is_ne())
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| {
                a.organization
                    .to_lowercase()
                    .cmp(&b.organization.to_lowercase())
            })
    });
    out
}

/// Descending, with `None` after every value.
fn desc_none_last(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.partial_cmp(&a).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// A delta cell in the per-org table as (CSS class, label). `present_now` /
/// `present_before` say which side is missing when there is no delta, because "—"
/// alone would not tell a new org from one that left the scope.
pub(crate) fn org_delta_cell(
    delta: Option<f64>,
    percent: bool,
    higher_is_better: bool,
    present_now: bool,
    present_before: bool,
) -> (&'static str, String) {
    match delta {
        Some(d) => (
            trend_verdict(d, percent, higher_is_better).css_class(),
            trend_delta_label(d, percent),
        ),
        None if !present_before => ("trend-delta", "new".to_string()),
        None if !present_now => ("trend-delta", "not in this run".to_string()),
        None => ("trend-delta", "\u{2014}".to_string()),
    }
}

/// Why the per-org table is empty or partial, or `None` when it is neither.
pub(crate) fn org_delta_note(newest: &RunRecord, previous: &RunRecord) -> Option<String> {
    if newest.orgs.is_empty() || previous.orgs.is_empty() {
        return Some(
            "Per-organization changes appear once two runs of this scope have recorded them."
                .to_string(),
        );
    }
    let total = newest.orgs_total.max(previous.orgs_total);
    (total > newest.orgs.len()).then(|| {
        format!(
            "Covers the {} largest of {} organizations.",
            group_thousands(newest.orgs.len()),
            group_thousands(total)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org(name: &str, n: usize, c: usize, pending: usize, pc: usize, ac: usize) -> OrgRun {
        OrgRun {
            organization: name.into(),
            devices_in_scope: n,
            devices_compliant: c,
            pending,
            pending_critical: pc,
            aged_critical: ac,
        }
    }

    fn run(orgs: Vec<OrgRun>) -> RunRecord {
        RunRecord {
            orgs_total: orgs.len(),
            orgs,
            ..RunRecord::default()
        }
    }

    #[test]
    fn the_biggest_regression_sorts_first() {
        let previous = run(vec![
            org("Steady", 10, 9, 3, 0, 0),
            org("Slipping", 10, 9, 3, 0, 0),
            org("Collapsing", 10, 9, 3, 0, 0),
            org("Improving", 10, 5, 9, 2, 1),
            org("Closed", 4, 4, 0, 0, 0),
        ]);
        let newest = run(vec![
            org("Steady", 10, 9, 3, 0, 0),
            org("Slipping", 10, 8, 5, 1, 0),
            org("Collapsing", 10, 4, 12, 3, 2),
            org("Improving", 10, 8, 4, 0, 0),
            org("Onboarded", 6, 1, 20, 4, 0),
        ]);
        let names: Vec<String> = org_deltas(&newest, &previous)
            .into_iter()
            .map(|d| d.organization)
            .collect();
        assert_eq!(
            names,
            [
                "Collapsing",
                "Slipping",
                "Steady",
                "Improving",
                "Closed",
                "Onboarded"
            ],
            "regressions first, improvements last, one-sided orgs after all of them"
        );
    }

    #[test]
    fn a_tie_on_compliance_is_broken_by_the_aged_backlog() {
        let previous = run(vec![org("A", 10, 5, 3, 0, 0), org("B", 10, 5, 3, 0, 0)]);
        let newest = run(vec![org("A", 10, 5, 3, 0, 0), org("B", 10, 5, 3, 0, 2)]);
        let first = &org_deltas(&newest, &previous)[0];
        assert_eq!(first.organization, "B");
        assert_eq!(first.aged_delta(), Some(2.0));
        assert_eq!(first.compliance_delta(), Some(0.0));
    }

    #[test]
    fn an_empty_org_has_no_compliance_delta_rather_than_a_fake_one() {
        let d = OrgDelta {
            organization: "Empty".into(),
            now: Some(org("Empty", 0, 0, 0, 0, 0)),
            previous: Some(org("Empty", 5, 5, 0, 0, 0)),
        };
        assert_eq!(d.compliance_delta(), None);
        assert_eq!(d.pending_delta(), Some(0.0));
    }

    #[test]
    fn the_org_note_explains_an_empty_or_capped_table() {
        let one = run(vec![org("A", 1, 1, 0, 0, 0)]);
        assert!(org_delta_note(&one, &run(Vec::new())).is_some());
        assert_eq!(org_delta_note(&one, &one), None);
        let capped = RunRecord {
            orgs_total: 240,
            ..one.clone()
        };
        assert_eq!(
            org_delta_note(&capped, &one).as_deref(),
            Some("Covers the 1 largest of 240 organizations.")
        );
    }

    #[test]
    fn a_delta_cell_is_signed_once_and_names_a_missing_side() {
        assert_eq!(
            org_delta_cell(Some(-2.5), true, true, true, true),
            ("trend-delta trend-worse", "-2.5%".to_string())
        );
        assert_eq!(
            org_delta_cell(Some(12.0), false, false, true, true),
            ("trend-delta trend-worse", "+12".to_string()),
            "more pending is worse, and signed exactly once"
        );
        assert_eq!(org_delta_cell(None, false, false, true, false).1, "new");
        assert_eq!(
            org_delta_cell(None, false, false, false, true).1,
            "not in this run"
        );
        assert_eq!(org_delta_cell(None, true, true, true, true).1, "\u{2014}");
    }

    #[test]
    fn the_previous_delta_reads_the_last_two_values() {
        assert_eq!(delta_vs_previous(&[]), None);
        assert_eq!(delta_vs_previous(&[4.0]), None);
        assert_eq!(delta_vs_previous(&[1.0, 10.0, 7.0]), Some(-3.0));
    }

    #[test]
    fn the_panel_says_when_there_is_no_baseline() {
        let none = RunChanges {
            tracks_pending: true,
            tracks_failed: true,
            ..RunChanges::default()
        };
        assert_eq!(changes_title(&none), "Changes since the previous run");
        let notes = changes_notes(&none, 200);
        assert_eq!(
            notes.len(),
            1,
            "no caveats about counts that were not measured"
        );
        assert!(notes[0].starts_with("No previous comparable run"));
    }

    #[test]
    fn the_caveats_follow_what_the_status_selection_measured() {
        let c = RunChanges {
            previous_at: Some("2026-09-01 10:00:00 UTC".into()),
            tracks_pending: true,
            tracks_failed: false,
            resolved: 450,
            ..RunChanges::default()
        };
        assert_eq!(changes_title(&c), "Changes since 2026-09-01 10:00:00 UTC");
        let notes = changes_notes(&c, 200).join(" ");
        assert!(notes.contains("Resolved means no longer pending"));
        assert!(notes.contains("Failed is not selected"));
        assert!(notes.contains("at most 200 entries"));

        let failed_only = RunChanges {
            tracks_pending: false,
            tracks_failed: true,
            ..c
        };
        let notes = changes_notes(&failed_only, 200).join(" ");
        assert!(notes.contains("New and resolved are not measured"));
        assert!(!notes.contains("Failed is not selected"));
    }

    #[test]
    fn a_capped_list_says_so() {
        assert_eq!(change_list_caption(12, 12), None);
        assert_eq!(
            change_list_caption(200, 1_204).as_deref(),
            Some("Showing 200 of 1,204, worst severity first.")
        );
    }
}
