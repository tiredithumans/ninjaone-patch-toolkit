//! The Run guard chain, the queued-run slot, the overlapping-run stamp and what a
//! landed run does to the view, lifted out of `AppState::run_query` so their
//! ordering is testable.

use std::collections::BTreeSet;

use crate::types::{PatchGroup, RowSort};

use super::super::state::Progress;
use super::{clamp_page, page_count, paged_total};

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
pub(crate) fn apply_progress_stage(p: &mut Progress, stage: &str, loaded: usize) {
    match stage {
        "devices" => p.devices = loaded,
        "osPatches" => p.os_patches = loaded,
        "swPatches" => p.sw_patches = loaded,
        "osInstalls" => p.os_installs = loaded,
        "swInstalls" => p.sw_installs = loaded,
        "joining" => p.joining = true,
        _ => {}
    }
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
    pub grouped: bool,
    pub rows_total: usize,
    pub groups_total: usize,
    pub page_size: usize,
    pub current_page: usize,
    pub current_sort: Option<RowSort>,
    pub sort_on_next_run: Option<RowSort>,
    /// The operator has toggled the filter panel at least once (the choice is
    /// remembered and always wins over the first-run collapse).
    pub filters_pref_set: bool,
}

/// What a run that succeeded does to the view.
///
/// Lifted out of the `Ok` arm of `AppState::run_query_inner` so the decisions are
/// reachable by a test; that arm is compile-checked only. A manual run is a new
/// scope: page 1, the sort a view link queued (or the canonical order), the
/// selection dropped, the groups collapsed. A silent refresh is the same scope with
/// fresher data: the page is kept (clamped in case the result shrank), the sort and
/// the ticked rows are kept, and the groups the operator had open are reopened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RunPlan {
    pub page: usize,
    /// The sort the view lands on. Applied only by a manual run — a silent one
    /// leaves `patches_sort` as it is rather than rewriting it with its own value.
    pub sort: Option<RowSort>,
    pub fetch: ViewFetch,
    /// Reclaim the fold on a manual run when the operator has not said otherwise.
    pub collapse_filters: bool,
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
        collapse_filters: !ctx.silent && !ctx.filters_pref_set,
        keep_selection: ctx.silent,
        reopen_groups: ctx.silent && ctx.grouped,
    }
}

/// The groups to reopen after a silent refresh: those that were `open` and are
/// still on the new header page, in header order. One that moved to another page
/// or left the result stays closed — its key would otherwise sit in `expanded`
/// with no header to show it under. One in `already_open` is skipped: the operator
/// reopened it while the headers were loading, and the reopen is a toggle that
/// would close it again.
pub(crate) fn groups_to_reopen(
    open: &BTreeSet<String>,
    already_open: &BTreeSet<String>,
    groups: &[PatchGroup],
) -> Vec<String> {
    groups
        .iter()
        .filter(|g| open.contains(&g.key) && !already_open.contains(&g.key))
        .map(|g| g.key.clone())
        .collect()
}
