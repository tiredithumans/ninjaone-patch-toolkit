//! The Run guard chain, the queued-run slot, the overlapping-run stamp and what a
//! landed run does to the view, lifted out of `AppState::run_query` so their
//! ordering is testable.

use std::collections::BTreeSet;

use crate::types::{PatchGroup, PatchQueryArgs, RowSort};

use super::super::state::{Progress, Stage};
use super::{clamp_page, filters_changed, group_thousands, page_count, paged_total};

/// What [`AppState::run_query_inner`] should do, decided from the flags alone.
///
/// Lifted out of the component-adjacent method so the guard chain is reachable by a
/// test: `state.rs` has no test module, and this ordering is load-bearing — a demo
/// run must be checked *before* the auth guard (the demo has no session and would
/// otherwise be told to sign in), and the busy guard before both (an auto-refresh
/// tick firing during a manual Run must not start a second one).
///
/// A *manual* request that finds a run in flight is queued, never dropped. The Run
/// button only disables on `busy`, so while a silent auto-refresh was paging the
/// fleet a click on Run — or on a drill-down, which has already rewritten the
/// filters — did nothing at all, and the table went on describing the old scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunDecision {
    /// An auto-refresh tick found a run in flight; skip it (the next tick, or the
    /// run in flight, supplies fresh data).
    AlreadyRunning,
    /// A manual run found one in flight; run it as soon as that one settles.
    Queue,
    /// No backend — filter the sample locally.
    Demo,
    NotSignedIn,
    NoStatusSelected,
    Run,
}

pub(crate) fn run_decision(
    busy: bool,
    refreshing: bool,
    silent: bool,
    demo: bool,
    authed: bool,
    statuses_empty: bool,
) -> RunDecision {
    if busy || refreshing {
        if silent {
            RunDecision::AlreadyRunning
        } else {
            RunDecision::Queue
        }
    } else if demo {
        RunDecision::Demo
    } else if !authed {
        RunDecision::NotSignedIn
    } else if statuses_empty {
        RunDecision::NoStatusSelected
    } else {
        RunDecision::Run
    }
}

/// The stamp identifying one run. Wrapping on purpose: only equality is ever asked
/// of it, so an overflow after 2^64 runs is harmless, whereas a plain `+ 1` would
/// panic in debug.
pub(crate) fn next_query_seq(current: u64) -> u64 {
    current.wrapping_add(1)
}

/// Whether a completed run has been overtaken by a newer one and must not paint.
///
/// Queries overlap routinely — an auto-refresh tick fires while a manual Run is
/// still paging the fleet — and they do not resolve in start order, so without this
/// a superseded response could overwrite a newer one on screen while the backend,
/// which drops the superseded *cache* write, kept the newer rows.
pub(crate) fn is_superseded(current_seq: u64, my_seq: u64) -> bool {
    current_seq != my_seq
}

/// Whether a lookup-like reply (the org/role/group/location lists, the script
/// library, the job list) was asked for in a session that has since ended — a
/// sign-out, sign-in, re-authorization or tenant switch moved `session.epoch`
/// while it was in flight. Such a reply describes the previous session (often the
/// previous tenant) and must not repaint the pickers: an old organization id would
/// then scope the new tenant's query. Same shape as [`is_superseded`].
/// Whether a failed settings save still switched tenants — the backend cleared the
/// previous tenant's result, jobs and grant before the step that failed.
pub(crate) fn save_switched_tenant(code: Option<&str>) -> bool {
    code == Some(crate::types::ERR_TENANT_CHANGED)
}

pub(crate) fn outlived_session(current_epoch: u64, my_epoch: u64) -> bool {
    current_epoch != my_epoch
}

/// Folds one more manual request into the queued-run slot. The slot holds the
/// `force` flag the queued run will use: several clicks while one run is in flight
/// collapse into a single re-run (it reads the filters as they are when it starts,
/// so every click's intent is in it), and a queued ↻ Refresh wins over a plain Run.
pub(crate) fn queue_run(queued: Option<bool>, force: bool) -> Option<bool> {
    Some(queued.unwrap_or(false) || force)
}

/// Takes the queued run once nothing is in flight any more, returning its `force`
/// flag. Leaves the slot alone while a run is still going: that run's own
/// completion is what drains it.
pub(crate) fn take_queued_run(
    queued: &mut Option<bool>,
    busy: bool,
    refreshing: bool,
) -> Option<bool> {
    if busy || refreshing {
        None
    } else {
        queued.take()
    }
}

/// Applies one backend `query:progress` event to the live counters. Unknown stages
/// are ignored rather than guessed at, so a newer backend cannot corrupt the bar.
///
/// `total` is taken as sent, absent included: a scan that starts over reports zero
/// rows and no size until its new cursor announces one.
pub(crate) fn apply_progress_stage(
    p: &mut Progress,
    stage: &str,
    loaded: usize,
    total: Option<usize>,
) {
    let slot = match stage {
        "devices" => &mut p.devices,
        "osPatches" => &mut p.os_patches,
        "swPatches" => &mut p.sw_patches,
        "osInstalls" => &mut p.os_installs,
        "swInstalls" => &mut p.sw_installs,
        "joining" => {
            p.joining = true;
            return;
        }
        _ => return,
    };
    *slot = Stage {
        loaded,
        total,
        started: true,
    };
}

/// Every feed a query can read, in the order they are listed, with its label.
fn stages(p: &Progress) -> [(&'static str, Stage); 5] {
    [
        ("Devices", p.devices),
        ("OS patches", p.os_patches),
        ("Third-party patches", p.sw_patches),
        ("OS install history", p.os_installs),
        ("Third-party install history", p.sw_installs),
    ]
}

/// Records loaded so far across every feed.
pub(crate) fn progress_records(p: &Progress) -> usize {
    stages(p).iter().map(|(_, s)| s.loaded).sum()
}

/// One feed's line in the loading panel.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProgressLine {
    pub label: &'static str,
    /// "5,000 of 6,721" when the feed announced its size, else "4,210".
    pub count: String,
    /// How much of the feed is in, when its size is known.
    pub fraction: Option<f64>,
}

/// The feeds that have reported, in a fixed order — a feed this query did not ask
/// for never appears, and one that has not answered yet is not shown at zero.
pub(crate) fn progress_lines(p: &Progress) -> Vec<ProgressLine> {
    stages(p)
        .into_iter()
        .filter(|(_, s)| s.started)
        .map(|(label, s)| ProgressLine {
            label,
            count: match s.total {
                Some(total) => format!(
                    "{} of {}",
                    group_thousands(s.loaded),
                    group_thousands(total)
                ),
                None => group_thousands(s.loaded),
            },
            fraction: s.total.map(|total| stage_fraction(s.loaded, total)),
        })
        .collect()
}

/// `loaded / total`, within 0–1. A feed that announced nothing to read is done.
fn stage_fraction(loaded: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        (loaded as f64 / total as f64).clamp(0.0, 1.0)
    }
}

/// How far along a running query is, 0–0.95, or `None` for an indeterminate bar.
///
/// Measured where it can be: the records in against the sizes NinjaOne announced,
/// over the feeds that announced one. That used to be a guess from how long the
/// previous run took — wrong whenever the scope, the statuses or the cache state
/// differed from last time, and unavailable on the first run of a session, the
/// longest one. The guess remains the fallback while no feed has announced a size
/// (the device inventory never does). Held under 1.0: the rollups still follow the
/// last record, and an over-running estimate must not claim to be finished.
pub(crate) fn progress_fraction(
    p: &Progress,
    elapsed_ms: f64,
    last_duration_ms: Option<f64>,
) -> Option<f64> {
    const CEILING: f64 = 0.95;
    let (loaded, total) = stages(p)
        .iter()
        .filter_map(|(_, s)| s.total.map(|total| (s.loaded.min(total), total)))
        .fold((0usize, 0usize), |(l, t), (loaded, total)| {
            (l.saturating_add(loaded), t.saturating_add(total))
        });
    if total > 0 {
        return Some((loaded as f64 / total as f64).clamp(0.0, CEILING));
    }
    let last = last_duration_ms.filter(|ms| *ms > 0.0)?;
    Some((elapsed_ms / last).clamp(0.0, CEILING))
}

/// Which collection the view fetches once a run has landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewFetch {
    /// Grouped view: this page of group headers.
    Groups(usize),
    /// Flat view, page 0, canonical order: the rows that shipped with the summary.
    SeedFirstPage,
    /// Flat view, a later page or an active sort: fetched.
    Rows(usize),
}

/// What [`run_plan`] decides from. `groups_total` is the *previous* result's (the
/// new one arrives with the headers, and `fetch_groups` re-clamps against it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RunContext {
    pub silent: bool,
    /// The run re-sent the query already on screen ([`same_query`]): a manual
    /// ↻ Refresh, or Run with the controls untouched.
    pub same_query: bool,
    pub grouped: bool,
    pub rows_total: usize,
    pub groups_total: usize,
    pub page_size: usize,
    pub current_page: usize,
    pub current_sort: Option<RowSort>,
    pub sort_on_next_run: Option<RowSort>,
}

/// What a run that succeeded does to the view.
///
/// Lifted out of the `Ok` arm of `AppState::run_query_inner` so the decisions are
/// reachable by a test; that arm is compile-checked only. A manual run lands on
/// page 1, the sort a view link queued (or the canonical order), the groups
/// collapsed. A silent refresh is the same scope with fresher data: the page is
/// kept (clamped in case the result shrank), the sort is kept, and the groups the
/// operator had open are reopened. The selection is dropped only for a different
/// query: a manual ↻ Refresh of the same one prunes it like a silent refresh, so
/// "Software after OS from the same selection" survives refreshing in between.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RunPlan {
    pub page: usize,
    /// The sort the view lands on. Applied only by a manual run — a silent one
    /// leaves `patches_sort` as it is rather than rewriting it with its own value.
    pub sort: Option<RowSort>,
    pub fetch: ViewFetch,
    /// Prune the selection against the fresh rows instead of dropping it.
    pub keep_selection: bool,
    /// Reopen the groups that were open and are still listed.
    pub reopen_groups: bool,
}

pub(crate) fn run_plan(ctx: RunContext) -> RunPlan {
    let (page, sort) = if ctx.silent {
        let total = paged_total(ctx.grouped, ctx.rows_total, ctx.groups_total);
        (
            clamp_page(ctx.current_page, page_count(total, ctx.page_size)),
            ctx.current_sort,
        )
    } else {
        (0, ctx.sort_on_next_run)
    };
    let fetch = if ctx.grouped {
        ViewFetch::Groups(page)
    } else if page == 0 && sort.is_none() {
        ViewFetch::SeedFirstPage
    } else {
        ViewFetch::Rows(page)
    };
    RunPlan {
        page,
        sort,
        fetch,
        keep_selection: ctx.silent || ctx.same_query,
        reopen_groups: ctx.silent && ctx.grouped,
    }
}

/// Whether a run re-sends the query already on screen. No query on screen is not
/// "the same": there is nothing for a selection to have been made against.
pub(crate) fn same_query(applied: Option<&PatchQueryArgs>, sent: &PatchQueryArgs) -> bool {
    applied.is_some() && !filters_changed(applied, sent)
}

/// What a silent refresh's new header page does to the open groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenGroups {
    /// The groups left open: those open now that the new page still lists. One
    /// that moved to another page or left the result closes — its key would
    /// otherwise sit in `expanded` with no header to show it under.
    pub open: BTreeSet<String>,
    /// The open groups whose members are from the previous result, in header
    /// order: each is refetched, its old rows shown until the new ones land.
    pub refetch: Vec<String>,
}

/// Settles the open groups once a silent refresh's headers land. `stale` holds
/// the groups whose cached members predate the refresh (those open when it
/// landed); `open_now` is `expanded` as it is now, so a group the operator
/// closed while the headers loaded stays closed, and one opened meanwhile —
/// loaded from the new result — is kept without a second fetch.
pub(crate) fn settle_open_groups(
    stale: &BTreeSet<String>,
    open_now: &BTreeSet<String>,
    groups: &[PatchGroup],
) -> OpenGroups {
    let listed: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
    let open: BTreeSet<String> = open_now
        .iter()
        .filter(|k| listed.contains(&k.as_str()))
        .cloned()
        .collect();
    let refetch = listed
        .iter()
        .filter(|k| open.contains(**k) && stale.contains(**k))
        .map(|k| k.to_string())
        .collect();
    OpenGroups { open, refetch }
}

/// What a failed query is put down to, in the operator's terms, for the causes
/// the backend names (`types::ERR_NETWORK` …). `None` for any other failure, whose
/// own message is the explanation — a filter NinjaOne rejected, an expired sign-in.
///
/// The raw message is a method, a URL and a status. It used to be all the banner
/// and the toast said, so the operator had to know that "502 Bad Gateway" on
/// `/queries/software-patches` means "wait a minute and press Run again".
pub(crate) fn failure_cause(code: Option<&str>) -> Option<&'static str> {
    use crate::types::{ERR_ACCESS, ERR_NETWORK, ERR_RATE_LIMITED, ERR_SERVER};
    match code? {
        ERR_NETWORK => {
            Some("Couldn't reach NinjaOne — check this computer's connection, VPN or proxy.")
        }
        ERR_RATE_LIMITED => {
            Some("NinjaOne is rate-limiting this app — wait a minute before trying again.")
        }
        ERR_SERVER => Some("NinjaOne had trouble answering — this is usually temporary."),
        ERR_ACCESS => Some(
            "NinjaOne refused access — sign in again; if it keeps happening, check the API app's scopes.",
        ),
        _ => None,
    }
}

/// Whether a failed `query_patches` was the operator's own Cancel
/// (`types::ERR_CANCELLED`): nothing went wrong, so there is no error to show.
pub(crate) fn query_was_cancelled(code: Option<&str>) -> bool {
    code == Some(crate::types::ERR_CANCELLED)
}

/// The results area's error banner: what it says, the raw message when that is not
/// already in the text, and whether running the query again is the remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ErrorBanner {
    pub text: String,
    /// The backend's message, shown under "Details" when `text` words the cause
    /// instead of quoting it.
    pub details: Option<String>,
    pub retry: bool,
}

/// A failed page, sort or group-header load is not a failed query: the result is
/// still good, only that view of it did not arrive, and calling it "Last query
/// failed" sent operators to re-run a query that had worked — so it offers no Retry
/// either. A failed query leads with its cause when the backend named one
/// ([`failure_cause`]) and keeps the raw message for the details.
pub(crate) fn query_error_banner(
    error: &str,
    code: Option<&str>,
    has_result: bool,
    paging: bool,
) -> ErrorBanner {
    if paging {
        return ErrorBanner {
            text: format!(
                "Couldn't load those rows — what is below is the last view that loaded: {error}"
            ),
            details: None,
            retry: false,
        };
    }
    let previous = if has_result {
        " The results below are from the previous run."
    } else {
        ""
    };
    match failure_cause(code) {
        Some(cause) => ErrorBanner {
            text: format!("{cause}{previous}"),
            details: Some(error.to_string()),
            retry: true,
        },
        None if has_result => ErrorBanner {
            text: format!(
                "Last query failed — the results below are from the previous run: {error}"
            ),
            details: None,
            retry: true,
        },
        None => ErrorBanner {
            text: format!("Last query failed: {error}"),
            details: None,
            retry: true,
        },
    }
}

/// How old the data on screen is, from the backend's `YYYY-MM-DD HH:MM UTC` stamp
/// and the wall clock in whole minutes: "just now", "12 min ago", "3 h ago",
/// "2 days ago". `None` for a stamp in another shape or one in the future (a
/// skewed clock), where the absolute time alone is the honest answer.
pub(crate) fn data_age_label(fetched_at: &str, now_min: i64) -> Option<String> {
    let (date, rest) = fetched_at.split_once(' ')?;
    let (hh, mm) = rest.strip_suffix(" UTC")?.split_once(':')?;
    let (hh, mm): (i64, i64) = (hh.parse().ok()?, mm.parse().ok()?);
    let at_min = super::date_to_epoch(date)? / 60 + hh * 60 + mm;
    let age = now_min - at_min;
    Some(match age {
        i64::MIN..=-1 => return None,
        0 => "just now".to_string(),
        1..=59 => format!("{age} min ago"),
        60..=2879 => format!("{} h ago", age / 60),
        _ => format!("{} days ago", age / 1440),
    })
}
