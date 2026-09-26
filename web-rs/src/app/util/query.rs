//! The Run guard chain, the queued-run slot and the overlapping-run stamp, lifted
//! out of `AppState::run_query` so their ordering is testable.

use super::super::state::Progress;

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
