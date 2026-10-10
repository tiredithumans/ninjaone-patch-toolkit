use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context as _;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};

use crate::api::{NinjaApiClient, ProgressFn};
use crate::changes::{self, RunSnapshot};
use crate::error::UiError;
use crate::filter::FilterParams;
use crate::model::{Device, Patch, PatchRow, PatchStatus, PatchType};
use crate::rows::{
    DeviceDetail, GroupBy, GroupPage, InstallWindow, LookupMaps, PatchFamilies, PatchSource,
    QueryResult, QuerySummary, RowSort, SlaCutoffs, apply_device_health, build_age_buckets,
    build_approval_backlog, build_compliance, build_compliance_by_os, build_device_backlogs,
    build_device_summaries, build_failures, build_groups, build_query_scope, build_rows,
    build_severity_by_org, build_time_to_install, group_member_page, page_rows, pending_counts,
    slice_groups, sort_device_summaries, sort_order,
};
use crate::settings::{MAX_WINDOW_DAYS, SlaPolicy};
use crate::state::{AppState, CurrentPatches, LookupSet, Memo, StoreOutcome};

/// The org/location/role lookups a query joins against, shared behind the cache's
/// `Arc` so a cache hit hands out a cheap refcount bump instead of a deep clone.
type Lookups = Arc<LookupSet>;

/// Size of the first page of detail rows returned inline by `query_patches`. Must
/// match the frontend's `PATCHES_PAGE_SIZE` so the seeded page fills the table's
/// first page exactly (later pages come from `get_patch_rows`).
const FIRST_PAGE_ROWS: usize = 100;

/// Hard cap on how many rows one paging call may return.
///
/// The write path re-checks every guardrail backend-side precisely because a stale
/// or modified frontend must not be able to widen what it asks for; the read path
/// took `offset`/`limit` verbatim, so the one cap that existed (`GROUP_MEMBER_LIMIT`)
/// lived frontend-side and a `limit` of `usize::MAX` cloned the entire cache into a
/// single IPC response — the exact whole-fleet serialization the paging design
/// exists to avoid.
const MAX_PAGE_LIMIT: usize = 1_000;

/// Clamps a requested page window to something the backend is willing to serve.
fn clamp_page(limit: usize) -> usize {
    limit.min(MAX_PAGE_LIMIT)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchQueryArgs {
    pub filter: FilterParams,
    pub patch_type: PatchType,
    pub statuses: Vec<PatchStatus>,
    /// Overrides the configured install-history lookback window (days). Ignored
    /// when `filter` carries an absolute install range, which replaces it.
    #[serde(default)]
    pub install_after_days: Option<i64>,
}

/// Incremental progress for an in-flight `query_patches`, emitted on the
/// `query:progress` event so the UI can show live record counts. `query_id`
/// echoes the value the frontend passed so it can drop events from a superseded
/// run.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryProgressEvent {
    query_id: u64,
    stage: &'static str,
    loaded: usize,
}

/// Best-effort emit of a progress event (a dropped event just means one fewer UI
/// update, never a failed query).
fn emit_progress(app: &AppHandle, query_id: u64, stage: &'static str, loaded: usize) {
    let _ = app.emit(
        "query:progress",
        QueryProgressEvent {
            query_id,
            stage,
            loaded,
        },
    );
}

/// Fetches devices and patches for the chosen filter/type/status, joins them into
/// per-server detail rows, and computes the reboot/compliance rollups. The result
/// is cached for the Excel exporter.
#[tauri::command]
pub async fn query_patches(
    state: State<'_, AppState>,
    app: AppHandle,
    args: PatchQueryArgs,
    query_id: Option<u64>,
    force_refresh: Option<bool>,
) -> Result<QuerySummary, UiError> {
    // Re-checked backend-side even though the UI cannot produce it: an empty status
    // list can only ever return an empty table, and reaching the fetch anyway starts
    // the most expensive thing this app does — a whole-fleet inventory sweep plus, on
    // a cold cache, a six-figure third-party patch feed. A stale or hand-built
    // frontend payload must not be able to ask for that.
    if args.statuses.is_empty() {
        return Err(UiError::new(
            "Select at least one patch status before running a query.",
        ));
    }
    // Validated before the token is claimed: a malformed range must not supersede a
    // good query that is still in flight. `QueryPlan::build` re-checks it — and, like
    // it, only when the range can matter at all.
    if args.statuses.iter().any(|s| s.is_install_history()) {
        args.filter
            .install_range(Utc::now().timestamp())
            .map_err(UiError::new)?;
    }
    let settings = state.settings_snapshot();
    // Claimed before any fetch so overlapping queries are ordered by *start*, and so
    // the result is stamped with the tenant it was actually fetched under. Redeemed
    // at the store below.
    let token = state.begin_query();
    // `qid` (0 when the frontend omits it) lets the frontend drop progress events
    // tagged with a run it has already superseded. Display only — the authoritative
    // ordering is `token`, which the frontend cannot influence.
    let qid = query_id.unwrap_or(0);
    // A re-filter (Run query) reuses the cached whole-fleet data; an auto-refresh
    // tick / manual refresh passes `force_refresh` to pull fresh patch state.
    let force = force_refresh.unwrap_or(false);
    let progress =
        move |stage: &'static str, loaded: usize| emit_progress(&app, qid, stage, loaded);

    // Whole-fleet devices + current patches come from `AppState`'s caches, so a scope
    // change re-filters them client-side with no refetch. Both are taken as futures
    // (each carrying its own progress reporter) so a *cold* fetch still resolves
    // concurrently with the lookups and install-history fetches inside `run_query`;
    // a cache hit resolves instantly. `force` bypasses the current-patch TTL.
    let p_devices = |n: usize| progress("devices", n);
    let p_os = |n: usize| progress("osPatches", n);
    let p_sw = |n: usize| progress("swPatches", n);
    let devices_fut = state.fleet_devices(Some(&p_devices as &ProgressFn));
    // The requested `PatchType` decides which families are fetched at all — a
    // family this query can't display is never worth a whole-fleet page-through.
    let current_fut = state.fleet_current_patches(
        force,
        args.patch_type.includes_os(),
        args.patch_type.includes_software(),
        Some(&p_os as &ProgressFn),
        Some(&p_sw as &ProgressFn),
    );

    // What the changes diff needs from the request; `args` moves into the query.
    let statuses = args.statuses.clone();
    let install_days = args
        .statuses
        .iter()
        .any(|s| s.is_install_history())
        .then(|| {
            args.install_after_days
                .unwrap_or(settings.install_window_days)
                .clamp(1, MAX_WINDOW_DAYS)
        });

    let result = run_query(
        &state.api,
        state.lookups(),
        devices_fut,
        current_fut,
        settings.install_window_days,
        // Clamped (inside `sla_policy`) for the same panic-guard reason as the
        // install window: every SLA window reaches `Duration::days` in the rollups,
        // and a settings.json predating the range validation can hold anything.
        // Read per query, so an SLA change in Settings reaches the next Run query
        // — a re-filter over the warm cache — with no refetch.
        settings.sla_policy(),
        settings.instance_base_url.clone(),
        args,
        Utc::now(),
        &progress,
    )
    .await
    .map_err(UiError::from)?;

    let (result, snapshot) =
        diff_against_previous_run(result, token.tenant_label(), statuses, install_days).await?;

    // Hand the frontend a lightweight summary (first page + rollups) and keep the
    // full result in the tenant-stamped cache for paging (`get_patch_rows`) and
    // export — moving it in rather than cloning every row.
    let summary = QuerySummary::from_result(&result, FIRST_PAGE_ROWS);

    // One rollup line per completed query, so the app has a time dimension at all.
    // Written before the store: this records what this query *measured*, which is
    // true whether or not the result went on to win the cache — a superseded run
    // still observed the fleet. Off the runtime, per the concurrency rule.
    let entry = crate::history::RunRecord::from_result(&result, &settings.instance_base_url);
    tokio::task::spawn_blocking(move || crate::history::record(&entry));

    let outcome = state.store_last_result_if_current(token, result);
    save_baseline_if_stored(&outcome, move || changes::save(snapshot)).await;
    summary_for(outcome, summary, qid)
}

/// Writes this run's snapshot as the next run's baseline, and returns only once it
/// is on disk.
///
/// Only a stored result becomes the baseline. A superseded one is by definition
/// older than the run that won the cache; writing it would roll the baseline
/// backwards, and the next diff would report changes that already happened. A
/// dropped (tenant-drifted, session-cleared) one belongs to nobody.
///
/// Awaited, not detached. The save used to be spawned and left to finish whenever,
/// so a query started straight after this one returned (an auto-refresh tick, a
/// quick re-run) could load the baseline before the save landed and diff against
/// the run *before* this one. The wait is one serialize and an atomic write of a
/// snapshot capped at a million items, small next to the fetch it follows. A
/// failed save is logged inside `save`; it never fails the query.
async fn save_baseline_if_stored(outcome: &StoreOutcome, save: impl FnOnce() + Send + 'static) {
    if !matches!(outcome, StoreOutcome::Stored) {
        return;
    }
    if let Err(err) = tokio::task::spawn_blocking(save).await {
        tracing::warn!(
            ?err,
            "saving the run snapshot panicked; baseline not advanced"
        );
    }
}

/// Fills `result.changes` from the previous comparable run's snapshot and returns
/// this run's snapshot for the caller to persist once the result is stored.
///
/// Off the runtime: it walks every row, and reads a file unless the baseline is
/// the snapshot this process wrote last (`changes::load`). The run is compared
/// against whatever baseline is current *now*, so an auto-refresh tick diffs
/// against the run immediately before it in the same scope.
async fn diff_against_previous_run(
    mut result: QueryResult,
    tenant: String,
    statuses: Vec<PatchStatus>,
    install_days: Option<i64>,
) -> Result<(QueryResult, RunSnapshot), UiError> {
    tokio::task::spawn_blocking(move || {
        let scope = changes::scope_key(
            &result.scope.fingerprint,
            result.patch_families,
            install_days,
        );
        let previous = changes::load(&tenant, &scope);
        let snapshot = RunSnapshot::build(
            &result.rows,
            &tenant,
            scope,
            &result.generated_at,
            &statuses,
        );
        result.changes = changes::diff(previous.as_deref(), &snapshot);
        (result, snapshot)
    })
    .await
    .map_err(|e| UiError::new(format!("comparing with the previous run panicked: {e}")))
}

/// Decides what a query hands back once its cache write has been adjudicated.
///
/// Free and pure so the branch can be tested — the `query_patches` wrapper around
/// it needs a Tauri `State`/`AppHandle` and so had no test at all, which is
/// precisely where the tenant-drift bug lived.
///
/// The rule: return the summary only when the rows behind it are readable. Every
/// other path that shows those rows (`get_patch_rows`, the group pages, the Excel
/// export, the HTML report) reads the cache, so returning a summary whose write was
/// dropped puts rows on screen that none of them can reach.
fn summary_for(
    outcome: StoreOutcome,
    summary: QuerySummary,
    qid: u64,
) -> Result<QuerySummary, UiError> {
    match outcome {
        StoreOutcome::Stored => Ok(summary),
        // The frontend already drops this itself — it applies only its own newest
        // `query_seq` — so surfacing an error would blame the operator for a race
        // they neither caused nor can see.
        StoreOutcome::Superseded => {
            tracing::debug!(qid, "query superseded before its result could be cached");
            Ok(summary)
        }
        // The frontend guard is not enough here: `clear_session` bumps `query_seq`
        // only once the settings save replies, and this reply can land first, so
        // returning the summary painted the previous tenant's rows and rollups over
        // the new tenant's empty cache. Fail instead — the operator did just change
        // instance, so the message explains itself and re-running is the obvious
        // next step.
        StoreOutcome::TenantChanged => {
            tracing::info!(qid, "instance changed mid-query; discarding the result");
            Err(UiError::new(
                "The instance changed while this query was running, so its results were discarded. Run the query again.",
            ))
        }
        // Same shape as tenant drift, and for a sharper reason: the operator signed
        // out (or re-authorized as someone else) while this query was running, so
        // these rows belong to the session that just ended. Returning the summary
        // would paint them over a cache that deliberately refused to keep them.
        StoreOutcome::SessionCleared => {
            tracing::info!(qid, "session cleared mid-query; discarding the result");
            Err(UiError::new(
                "The session ended while this query was running, so its results were discarded. Sign in and run the query again.",
            ))
        }
        // Same shape again: the rows would be on screen while paging and
        // export fail. `with_current_result` reports the poisoning to those callers;
        // report it here too rather than letting the table imply all is well.
        StoreOutcome::Poisoned => Err(UiError::new(
            "The result cache is unusable after an internal error. Restart the app to run queries again.",
        )),
    }
}

/// The routing decisions a query makes *before* it fetches anything: which feeds
/// to hit, which statuses narrow which source, and the absolute time bounds.
///
/// Pure, so it can be tested directly. These decisions were inline in `run_query`
/// and reachable only through a wiremock round trip, which asserts on rows rather
/// than on what was actually requested — and getting one wrong is silent: the
/// query simply returns the wrong set. The `Installed`/`Failed` routing in
/// particular was a real bug (a FAILED query returned nothing, because FAILED
/// never appears in the current feed).
struct QueryPlan {
    filter: FilterParams,
    /// The operator's status selection, kept verbatim for the export provenance
    /// block. The two derived sets below are equal to it by construction, but they
    /// are `HashSet`s — unordered, and spelled in NinjaOne's wire vocabulary — so
    /// reconstructing the operator's own list from them would be both lossy and a
    /// second place to get `MANUAL` ⇄ "Pending" wrong.
    statuses: Vec<PatchStatus>,
    /// Server-side `df` for the install-history endpoints, which are fetched fresh
    /// per query. The whole-fleet caches are scoped client-side instead.
    patch_df: Option<String>,
    /// Whether any requested status is an install *result*, i.e. needs the history
    /// endpoints at all.
    want_installs: bool,
    /// Statuses that narrow the current feed (MANUAL/APPROVED/REJECTED).
    current_status_set: HashSet<&'static str>,
    /// Install statuses the operator asked for (INSTALLED and/or FAILED).
    install_status_set: HashSet<&'static str>,
    /// Pushed to the history endpoints only when exactly one install status is
    /// requested — see [`QueryPlan::build`].
    install_status: Option<&'static str>,
    include_os: bool,
    include_sw: bool,
    /// The install-history window, pushed down as `installedAfter` /
    /// `installedBefore` and re-applied client-side in [`assemble_result`].
    install_window: InstallWindow,
}

impl QueryPlan {
    fn build(
        args: PatchQueryArgs,
        install_window_days: i64,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Self> {
        let mut filter = args.filter;
        // Resolve the relative first-seen window into an absolute lower bound; the
        // filter is applied client-side in build_rows, which has no clock.
        if let Some(days) = filter.detected_within_days {
            filter.detected_after =
                Some((now - Duration::days(days.clamp(0, MAX_WINDOW_DAYS))).timestamp());
        }
        let patch_df = filter.patch_filter();

        // Install *results* ("Installed" and "Failed") route to the
        // `*-patch-installs` history endpoints; the rest (MANUAL/APPROVED/REJECTED)
        // narrow the current-patch feed for display. The set carries **every**
        // selected status, install results included: the spec's description says
        // the current feed holds only "patches for which there were no installation
        // attempts", but its own titles for the same endpoints are "Pending, Failed
        // and Rejected … report", and `status` there has no enum. A FAILED record
        // that does arrive in the current feed is counted against compliance by
        // `rows::is_pending`, so it must also be *visible* under the Failed
        // selection — filtering it out of the rows here left a device the rollups
        // called non-compliant with nothing on the Patches tab to show why. The set
        // only reaches `build_rows`; the rollups take the unnarrowed feed.
        let want_installs = args.statuses.iter().any(|s| s.is_install_history());
        let current_status_set: HashSet<&'static str> =
            args.statuses.iter().map(|s| s.api_value()).collect();
        let install_status_set: HashSet<&'static str> = args
            .statuses
            .iter()
            .filter(|s| s.is_install_history())
            .map(|s| s.api_value())
            .collect();
        // When exactly one install status is requested, push it to the history
        // endpoints server-side so a FAILED-only query (the failure dashboard)
        // doesn't download every successful install just to drop it. With both
        // requested we need both records, so leave it unset; the client-side
        // `install_status_set` filter in build_rows stays as a backstop either way.
        let install_status: Option<&'static str> = match install_status_set.len() {
            1 => install_status_set.iter().copied().next(),
            _ => None,
        };

        // The configured window is validated 1..=MAX_WINDOW_DAYS in save_settings;
        // clamp the optional per-query override the same way so a 0/negative
        // lookback can't invert into a future `after` bound that would match no
        // install history. The upper bound is load-bearing too: `Duration::days`
        // panics on an out-of-range day count, and a settings.json written before
        // that validation existed (or edited by hand) can still carry one.
        let days = args
            .install_after_days
            .unwrap_or(install_window_days)
            .clamp(1, MAX_WINDOW_DAYS);
        // An absolute range replaces the relative lookback rather than intersecting
        // it — see `FilterParams::installed_after`. Validated only when it can
        // matter: with no install status the window is never used, and the Filters
        // panel hides the control, so a stale range left in it must not fail a
        // Pending query with an error whose cause is off screen.
        let install_range = if want_installs {
            filter
                .install_range(now.timestamp())
                .map_err(anyhow::Error::msg)?
        } else {
            None
        };
        let install_window = match install_range {
            Some(range) => InstallWindow {
                after: range.after,
                before: range.before,
                relative_days: None,
            },
            None => InstallWindow {
                after: (now - Duration::days(days)).timestamp(),
                before: None,
                relative_days: Some(days),
            },
        };

        Ok(Self {
            filter,
            statuses: args.statuses,
            patch_df,
            want_installs,
            current_status_set,
            install_status_set,
            install_status,
            include_os: args.patch_type.includes_os(),
            include_sw: args.patch_type.includes_software(),
            install_window,
        })
    }
}

/// Everything a query fetched, before scoping and joining.
struct FetchedSources {
    devices: Arc<Vec<Device>>,
    current: CurrentPatches,
    lookups: Lookups,
    os_installs: Vec<Patch>,
    sw_installs: Vec<Patch>,
}

/// The fetch→scope→join→rollup core of [`query_patches`], split out so it can be
/// driven in tests against a mock NinjaOne server without a Tauri `AppHandle`/`State`.
///
/// `lookups`, `devices_fut`, and `current_fut` are taken as *futures* (not resolved
/// values) so the cached-or-fetched org/location/role lookups, the whole-fleet device
/// inventory, and the whole-fleet current patches all resolve concurrently with the
/// per-query install-history fetch — a cache hit resolves its future instantly.
/// `query_patches` passes the `AppState` cache accessors; a test passes ready values.
/// The whole-fleet devices and current patches are then scoped to the requested
/// identity facets **client-side** (so a re-filter needs no refetch). `progress` (the
/// UI sink, keyed by stage) and `now` (the clock, for the first-seen and install
/// windows, SLA aging since first seen, and `generated_at`) are injected so the
/// caller owns both.
#[allow(clippy::too_many_arguments)]
async fn run_query<L, D, C>(
    api: &NinjaApiClient,
    lookups: L,
    devices_fut: D,
    current_fut: C,
    install_window_days: i64,
    sla: SlaPolicy,
    instance: String,
    args: PatchQueryArgs,
    now: DateTime<Utc>,
    progress: &(dyn Fn(&'static str, usize) + Send + Sync),
) -> anyhow::Result<QueryResult>
where
    L: std::future::Future<Output = anyhow::Result<Lookups>>,
    D: std::future::Future<Output = anyhow::Result<Arc<Vec<Device>>>>,
    C: std::future::Future<Output = anyhow::Result<CurrentPatches>>,
{
    let plan = QueryPlan::build(args, install_window_days, now)?;

    // The cached whole-fleet devices/current-patches (futures), the lookups, and the
    // per-query install history are all independent — resolve them concurrently so
    // latency is the slowest call, not the sum. The install fetch resolves to empty
    // when no install status / matching family is requested.
    let p_os_inst = |n: usize| progress("osInstalls", n);
    let p_sw_inst = |n: usize| progress("swInstalls", n);
    let patch_df_ref = plan.patch_df.as_deref();

    // `join!`, not `try_join!`. `try_join!` drops the other futures the moment one
    // returns `Err`, and the two most expensive legs here — the whole-fleet device
    // inventory and the whole-fleet current-patch feeds — are the ones that populate
    // the long-lived caches. A transient failure on the *per-query* install-history
    // pull therefore cancelled minutes of sequential cursor paging before it could
    // reach its cache, so the retry started cold and hit exactly the same odds
    // again. Letting every leg finish means the query still fails (the `?`s below
    // are unchanged), but the fleet caches are warm for the retry.
    let (devices, current, lookup_sets, os_installs, sw_installs) = tokio::join!(
        devices_fut,
        current_fut,
        lookups,
        async {
            if plan.want_installs && plan.include_os {
                api.fleet_os_patch_installs(
                    patch_df_ref,
                    plan.install_status,
                    plan.install_window.after,
                    plan.install_window.before,
                    Some(&p_os_inst as &ProgressFn),
                )
                .await
            } else {
                Ok(Vec::new())
            }
        },
        async {
            if plan.want_installs && plan.include_sw {
                api.fleet_software_patch_installs(
                    patch_df_ref,
                    plan.install_status,
                    plan.install_window.after,
                    plan.install_window.before,
                    Some(&p_sw_inst as &ProgressFn),
                )
                .await
            } else {
                Ok(Vec::new())
            }
        },
    );
    // Surfaced in the same order `try_join!` would have, so the operator sees the
    // same first error; the difference is only that the siblings were allowed to
    // finish and cache.
    let devices = devices?;
    let current = current?;
    let lookups = lookup_sets?;
    let os_installs = os_installs?;
    let sw_installs = sw_installs?;

    // Fetches done; the rest is the in-memory scope + join/rollup.
    progress("joining", 0);
    let src = FetchedSources {
        devices,
        current,
        lookups,
        os_installs,
        sw_installs,
    };
    // Off the async runtime. This is the one genuinely CPU-bound stretch in the
    // command — scoping the whole-fleet caches, joining every patch to its device,
    // sorting the row set and computing six rollups over it — and on a large fleet it
    // runs for seconds with no `.await` in it. Left inline it held a tokio worker for
    // that whole time, stalling unrelated IPC commands and the job poller. Everything
    // it needs is owned and `Send`, so moving it is just a `spawn_blocking`.
    tauri::async_runtime::spawn_blocking(move || assemble_result(&plan, src, sla, instance, now))
        .await
        .context("join/rollup task failed")
}

/// Scopes the whole-fleet caches client-side, joins devices to patches, and
/// computes every rollup. No I/O — everything it needs has already been fetched.
fn assemble_result(
    plan: &QueryPlan,
    src: FetchedSources,
    sla: SlaPolicy,
    instance: String,
    now: DateTime<Utc>,
) -> QueryResult {
    let maps = LookupMaps::build(
        &src.lookups.orgs,
        &src.lookups.locations,
        &src.lookups.roles,
    );

    // Scope the whole-fleet caches to the selected identity facets (org/location/
    // role/class) client-side — this is what makes a re-filter a no-refetch
    // operation, replacing the old per-query device/patch `df`. `devices_by_id` then
    // holds only in-scope devices, so every downstream rollup is scoped through it.
    // One prepared filter for the whole assembly: it lowers the text needles and
    // parses the severities once, and it is what both the device scoping below and
    // the row join use — so a device the scope excludes cannot reappear as a row.
    let prepared = plan.filter.prepare();
    let has_scope = prepared.has_scope();
    let scoped_devices: Vec<&Device> = src
        .devices
        .iter()
        .filter(|d| prepared.device_allowed(d))
        .collect();
    let devices_by_id: HashMap<i64, &Device> = scoped_devices.iter().map(|d| (d.id, *d)).collect();

    // Narrow the cached current patches to the same scope and the requested families.
    // With no identity scope every patch is kept (orphans included, as before); with
    // a scope, only patches whose device is in the scoped set survive.
    //
    // These **borrow** from the `Arc` cache rather than cloning out of it. A whole-fleet
    // third-party feed runs to six figures, and each `Patch` owns six `Option<String>`s,
    // so cloning the scoped subset — and then cloning it again into `all_current` — cost
    // millions of allocations per query for data the cache already owns and outlives.
    // The rollups take `&[&Patch]`, so scoping is now a pointer copy.
    let in_scope = |p: &Patch| {
        !has_scope
            || p.device_id
                .is_some_and(|id| devices_by_id.contains_key(&id))
    };
    let scoped_os_current: Vec<&Patch> = if plan.include_os {
        src.current.os.iter().filter(|p| in_scope(p)).collect()
    } else {
        Vec::new()
    };
    let scoped_sw_current: Vec<&Patch> = if plan.include_sw {
        src.current.sw.iter().filter(|p| in_scope(p)).collect()
    } else {
        Vec::new()
    };

    // Build detail rows from the scoped current families plus the install history.
    // The install sets are owned by this call, so they're borrowed into the same
    // `&[&Patch]` shape the scoped current families use.
    //
    // The lookback window is re-applied here rather than trusted to the server. The
    // `installedAfter` parameter is typed only as `string` in the spec with no stated
    // format; Unix seconds is what the widely used community client sends and what
    // this app has always sent, but nothing in the response says whether the bound
    // was honored, and the exports print "Install history since <date>" on the
    // strength of it. Undated records are kept: the window cannot prove them out.
    // Both bounds, for the same reason: `installedBefore` is as unspecified as
    // `installedAfter`, and a custom range's export prints its end date too.
    let window = plan.install_window;
    let within_window = |p: &&Patch| {
        p.installed_at().is_none_or(|t| {
            let t = t.timestamp();
            t >= window.after && window.before.is_none_or(|b| t <= b)
        })
    };
    let os_install_refs: Vec<&Patch> = src.os_installs.iter().filter(within_window).collect();
    let sw_install_refs: Vec<&Patch> = src.sw_installs.iter().filter(within_window).collect();
    let mut rows = {
        let mut sources = vec![
            // A current-feed record that omits its status is pending by construction
            // (see `rows::is_pending`), and every rollup counts it that way. The
            // override labels it MANUAL so the row join agrees: it matches the
            // Pending selection and renders as PENDING. With no override it fell
            // through the status filter and never became a row, so the Compliance
            // sheet counted patches the Patches sheet could not show.
            PatchSource {
                patches: &scoped_os_current,
                type_label: "OS",
                status_override: Some(PatchStatus::Pending.api_value()),
                status_filter: Some(&plan.current_status_set),
            },
            PatchSource {
                patches: &scoped_sw_current,
                type_label: "SOFTWARE",
                status_override: Some(PatchStatus::Pending.api_value()),
                status_filter: Some(&plan.current_status_set),
            },
        ];
        if plan.want_installs {
            // The install endpoints return both successful and failed records, so
            // narrow each to the requested install statuses; the override labels a
            // record that omits its own status.
            //
            // That label is the pushed-down status when there is one. `status` is not
            // required on an install record, and hardcoding INSTALLED meant that on a
            // FAILED-only query — where the server has already filtered to failures —
            // an untyped record was labelled INSTALLED and then dropped by the
            // client-side FAILED backstop. The failure dashboard silently lost rows,
            // and the wiremock fixtures always set an explicit status, so nothing
            // caught it. With both statuses requested nothing has been narrowed, so
            // INSTALLED stays the default.
            let install_label = plan
                .install_status
                .unwrap_or(PatchStatus::Installed.api_value());
            sources.push(PatchSource {
                patches: &os_install_refs,
                type_label: "OS",
                status_override: Some(install_label),
                status_filter: Some(&plan.install_status_set),
            });
            sources.push(PatchSource {
                patches: &sw_install_refs,
                type_label: "SOFTWARE",
                status_override: Some(install_label),
                status_filter: Some(&plan.install_status_set),
            });
        }
        build_rows(&devices_by_id, &maps, &sources, &prepared)
    };
    // Highest severity first, then organization, then device — case-insensitive.
    //
    // `cmp_ci` rather than `sort_by_cached_key`: the cached key allocated two owned
    // lowercase `String`s *per row*, which on a six-figure fleet is a few hundred
    // thousand allocations to sort data whose repeated strings the row builder went
    // to lengths to intern into shared `Arc<str>`. `cmp_ci` is the allocation-free
    // comparison the memoized paging sort already uses on the same fields.
    rows.sort_by(|a, b| {
        b.severity_rank
            .cmp(&a.severity_rank)
            .then_with(|| crate::rows::cmp_ci(&a.organization, &b.organization))
            .then_with(|| crate::rows::cmp_ci(&a.device_name, &b.device_name))
    });

    // Compliance + reboot rollups from the scoped current set. Concatenating the two
    // families copies pointers, not patches.
    let all_current: Vec<&Patch> = scoped_os_current
        .iter()
        .chain(&scoped_sw_current)
        .copied()
        .collect();
    let counts = pending_counts(&all_current);
    let mut summaries = build_device_summaries(&scoped_devices, &counts, &maps);
    let cutoffs = SlaCutoffs::new(&sla, now);
    // Failed installs per device exist only when the Failed status reached the
    // history endpoints; otherwise the per-device count stays unknown (`None`), not
    // zero. Labelled the way the row join labels them — an untyped record takes the
    // pushed-down status — and over the same lookback-filtered records.
    let failed_installs: Option<Vec<&Patch>> = plan
        .install_status_set
        .contains(PatchStatus::Failed.api_value())
        .then(|| {
            let label = plan
                .install_status
                .unwrap_or(PatchStatus::Installed.api_value());
            os_install_refs
                .iter()
                .chain(&sw_install_refs)
                .copied()
                .filter(|p| p.status.as_deref().unwrap_or(label) == PatchStatus::Failed.api_value())
                .collect()
        });
    apply_device_health(
        &mut summaries,
        &all_current,
        failed_installs.as_deref(),
        &cutoffs,
    );
    sort_device_summaries(&mut summaries);
    let compliance = build_compliance(&summaries, &all_current, &devices_by_id, &maps, &cutoffs);
    let compliance_by_os =
        build_compliance_by_os(&summaries, &all_current, &devices_by_id, &cutoffs);
    let (worst_devices, offline_backlog) =
        build_device_backlogs(&all_current, &devices_by_id, &maps, &cutoffs);

    // Dashboard/failure rollups. Failures are derived from the FAILED rows already
    // joined (present only when the FAILED status was requested — no extra fetch);
    // the severity/age distributions come from the current pending backlog.
    let failures = build_failures(&rows);
    let severity_by_org = build_severity_by_org(&all_current, &devices_by_id, &maps);
    let age_buckets = build_age_buckets(&all_current, &devices_by_id, now);
    let time_to_install =
        build_time_to_install(&rows, plan.statuses.contains(&PatchStatus::Installed));
    // "Stuck" reuses the default SLA window: an approved patch still not installed
    // past the point the SLA calls overdue is the agent-trouble signal, and a second
    // knob for the same idea of "too long" would only let the two disagree. The
    // default rather than the per-band policy, because a stuck approval is an agent
    // problem whatever the patch's severity.
    let approvals =
        build_approval_backlog(&all_current, &devices_by_id, &maps, sla.default_days, now);

    let families = PatchFamilies {
        os: plan.include_os,
        software: plan.include_sw,
    };

    QueryResult {
        rows,
        devices: summaries,
        compliance,
        compliance_by_os,
        failures,
        severity_by_org,
        age_buckets,
        worst_devices,
        offline_backlog,
        time_to_install,
        sla_policy: sla,
        instance,
        approvals,
        devices_total: scoped_devices.len(),
        // Counted over the same scoped set the compliance rollups draw from, so the
        // two device numbers on screen are reconcilable: `devices_total` is every
        // in-scope device, and `devices_total - devices_offline` is the compliance
        // denominator.
        devices_offline: scoped_devices.iter().filter(|d| d.is_offline()).count(),
        // Online only, so the three counts reconcile: an offline switch is already
        // in `devices_offline`.
        devices_unpatchable: scoped_devices
            .iter()
            .filter(|d| !d.is_offline() && !d.is_patchable())
            .count(),
        patch_families: families,
        // Built from the plan the fetch ran under, so the block describes the query
        // rather than the request — including the install lookback, which is named
        // only when the status selection actually reached the history endpoints.
        scope: build_query_scope(
            &plan.filter,
            &maps,
            families,
            &plan.statuses,
            plan.want_installs.then_some(plan.install_window),
        ),
        // Needs the stored snapshot, so `query_patches` fills it after the join.
        changes: Default::default(),
        generated_at: now.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        data_fetched_at: src
            .current
            .fetched_at
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string(),
    }
}

/// Serves one page of detail rows from the cached query result so the frontend can
/// page through a large fleet without receiving every row over IPC. `sort` re-orders
/// the view per request (the cached rows keep their canonical order). Returns an
/// empty page when there is no cached result or the offset is past the end.
///
/// All three paging commands treat a cache miss the same way — an empty page, not an
/// error. A miss is a normal transient: a tenant switch, a sign-out, or a superseded
/// query can all retire the cache while a page request is in flight, and none of them
/// is something the operator did wrong. `get_patch_groups` used to error here, which
/// toasted "Run a query before grouping" at someone who had just switched instance.
/// The frontend renders its own "Run a query" empty state from the absence of a
/// result, so the error carried no information the UI lacked.
#[tauri::command]
pub async fn get_patch_rows(
    state: State<'_, AppState>,
    offset: usize,
    limit: usize,
    sort: Option<RowSort>,
) -> Result<Vec<PatchRow>, UiError> {
    // Only a result belonging to the current tenant is read (a tenant switch reads
    // as empty). The sort order is memoized, so paging through a sorted view costs
    // one sweep rather than one per page.
    let limit = clamp_page(limit);
    let Some(sort) = sort else {
        // Cache order: a handle, then a slice of at most `limit` rows with the lock
        // already released.
        return Ok(state
            .current_result_handle()?
            .map(|result| page_rows(&result.rows, None, offset, limit))
            .unwrap_or_default());
    };
    let Some(Memo { result, memo }) = state.sort_memo(sort)? else {
        return Ok(Vec::new());
    };
    let order = match memo {
        Some(order) => order,
        None => {
            // The first request for a sort pays one full sweep over every cached
            // row — CPU-bound, so off the runtime, and with the result mutex
            // released (the memo read above held it for two `Arc` bumps). It used
            // to run inside that mutex on an async worker, stalling every other
            // paging command, the export and unrelated IPC for its duration.
            let rows = Arc::clone(&result);
            let order = Arc::new(
                tokio::task::spawn_blocking(move || sort_order(&rows.rows, sort))
                    .await
                    .map_err(|e| UiError::new(format!("sorting the rows panicked: {e}")))?,
            );
            // Kept only if this is still the cached result; either way this page is
            // served from the rows the order was built over.
            state.store_sort_memo(&result, sort, Arc::clone(&order));
            order
        }
    };
    Ok(page_rows(&result.rows, Some(&order), offset, limit))
}

/// Serves one page of **group headers** over the same cached rows `get_patch_rows`
/// pages. Grouping happens backend-side for the same reason sorting does: the
/// frontend only ever holds one page, so it cannot group a fleet it hasn't seen.
///
/// Returns headers only. A patch group can span the whole fleet (one Chrome update
/// covers every device), so its members stay off the wire until expanded.
#[tauri::command]
pub async fn get_patch_groups(
    state: State<'_, AppState>,
    group_by: GroupBy,
    offset: usize,
    limit: usize,
) -> Result<GroupPage, UiError> {
    // Empty on a miss, matching `get_patch_rows` / `get_patch_group_members` — see
    // the note on `get_patch_rows`.
    // Through the memo, so the whole grouping is built once per `group_by` rather
    // than on every page request.
    let limit = clamp_page(limit);
    let Some(Memo { result, memo }) = state.group_memo(group_by)? else {
        return Ok(GroupPage::default());
    };
    let groups = match memo {
        Some(groups) => groups,
        None => {
            // Same two-phase shape as `get_patch_rows`: build off the runtime with
            // the lock released, keep it only if the result is still current.
            let rows = Arc::clone(&result);
            let groups = Arc::new(
                tokio::task::spawn_blocking(move || build_groups(&rows.rows, group_by))
                    .await
                    .map_err(|e| UiError::new(format!("grouping the rows panicked: {e}")))?,
            );
            state.store_group_memo(&result, group_by, Arc::clone(&groups));
            groups
        }
    };
    Ok(slice_groups(&groups, offset, limit))
}

/// Serves one page of a single group's member rows. `key` is the opaque
/// `PatchGroup.key` the frontend was handed, so no per-request state is kept
/// backend-side and a stale key simply matches nothing.
#[tauri::command]
pub async fn get_patch_group_members(
    state: State<'_, AppState>,
    group_by: GroupBy,
    key: String,
    offset: usize,
    limit: usize,
) -> Result<Vec<PatchRow>, UiError> {
    let Some(result) = state.current_result_handle()? else {
        return Ok(Vec::new());
    };
    // A member lookup scans every cached row for the key, so it runs off the runtime
    // on a handle rather than under the result mutex.
    let limit = clamp_page(limit);
    tokio::task::spawn_blocking(move || {
        group_member_page(&result.rows, group_by, &key, offset, limit)
    })
    .await
    .map_err(|e| UiError::new(format!("reading the group's rows panicked: {e}")))
}

/// Serves several devices' rows in one pass over the cached result — the
/// post-refresh selection prune's read, which was one `get_patch_group_members`
/// call per selected device. Empty on a cache miss, like the paging commands.
#[tauri::command]
pub async fn get_device_rows(
    state: State<'_, AppState>,
    device_ids: Vec<i64>,
    limit: usize,
) -> Result<Vec<crate::rows::DeviceRows>, UiError> {
    let Some(result) = state.current_result_handle()? else {
        return Ok(Vec::new());
    };
    let limit = clamp_page(limit);
    tokio::task::spawn_blocking(move || {
        crate::rows::rows_by_device(&result.rows, &device_ids, limit)
    })
    .await
    .map_err(|e| UiError::new(format!("reading the devices' rows panicked: {e}")))
}

/// Serves the device drill-down — one device's facts, its per-device rollup and up
/// to [`MAX_PAGE_LIMIT`] of its detail rows — from the cached result. Read-only.
///
/// `None` on a cache miss, like the paging commands: the drill-down is a view over
/// the rows already on screen, and a tenant switch or superseded query retiring
/// them is not something to toast about. The scan over every cached row runs on the
/// blocking pool against a handle, not under the result mutex.
#[tauri::command]
pub async fn device_detail(
    state: State<'_, AppState>,
    device_id: i64,
) -> Result<Option<DeviceDetail>, UiError> {
    let Some(result) = state.current_result_handle()? else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || {
        crate::rows::device_detail(&result, device_id, MAX_PAGE_LIMIT)
    })
    .await
    .map_err(|e| UiError::new(format!("reading the device's rows panicked: {e}")))
}

#[cfg(test)]
mod tests;
