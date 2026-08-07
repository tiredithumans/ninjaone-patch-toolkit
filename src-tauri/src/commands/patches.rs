use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};

use crate::api::{NinjaApiClient, ProgressFn};
use crate::error::UiError;
use crate::filter::FilterParams;
use crate::model::{Device, Location, Organization, Patch, PatchRow, PatchStatus, PatchType, Role};
use crate::rows::{
    GroupBy, GroupPage, LookupMaps, PatchSource, QueryResult, QuerySummary, RowSort,
    build_age_buckets, build_compliance, build_compliance_by_os, build_device_summaries,
    build_failures, build_rows, build_severity_by_org, group_member_page, group_page, page_rows,
    pending_counts,
};
use crate::settings::MAX_WINDOW_DAYS;
use crate::state::{AppState, CurrentPatches, StoreOutcome};

/// The org/location/role lookups a query joins against, each shared behind `Arc`
/// so a cache hit hands out a cheap refcount bump instead of a deep clone.
type Lookups = (Arc<Vec<Organization>>, Arc<Vec<Location>>, Arc<Vec<Role>>);

/// Size of the first page of detail rows returned inline by `query_patches`. Must
/// match the frontend's `PATCHES_PAGE_SIZE` so the seeded page fills the table's
/// first page exactly (later pages come from `get_patch_rows`).
const FIRST_PAGE_ROWS: usize = 100;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchQueryArgs {
    pub filter: FilterParams,
    pub patch_type: PatchType,
    pub statuses: Vec<PatchStatus>,
    /// Overrides the configured install-history lookback window (days).
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

    let result = run_query(
        &state.api,
        state.lookups(),
        devices_fut,
        current_fut,
        settings.install_window_days,
        // Clamped for the same panic-guard reason as the install window: the SLA
        // window reaches `Duration::days` in the compliance rollups, and a
        // settings.json predating the range validation can still hold anything.
        settings.sla_days.clamp(1, MAX_WINDOW_DAYS),
        args,
        Utc::now(),
        &progress,
    )
    .await
    .map_err(UiError::from)?;

    // Hand the frontend a lightweight summary (first page + rollups) and keep the
    // full result in the tenant-stamped cache for paging (`get_patch_rows`) and
    // export — moving it in rather than cloning every row.
    let summary = QuerySummary::from_result(&result, FIRST_PAGE_ROWS);
    summary_for(
        state.store_last_result_if_current(token, result),
        summary,
        qid,
    )
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
        // No such frontend guard exists here: `query_seq` counts runs the frontend
        // *starts*, and switching instance never bumps it, so returning the summary
        // painted the previous tenant's rows and rollups over the new tenant's empty
        // cache. Fail instead — the operator did just change instance, so the
        // message explains itself and re-running is the obvious next step.
        StoreOutcome::TenantChanged => {
            tracing::info!(qid, "instance changed mid-query; discarding the result");
            Err(UiError::new(
                "The instance changed while this query was running, so its results were discarded. Run the query again.",
            ))
        }
        // Same shape as tenant drift: the rows would be on screen while paging and
        // export fail. `with_current_result` reports the poisoning to those callers;
        // report it here too rather than letting the table imply all is well.
        StoreOutcome::Poisoned => Err(UiError::new(
            "The result cache is unusable after an internal error. Restart the app to run queries again.",
        )),
    }
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
/// UI sink, keyed by stage) and `now` (the clock, for the release/install windows, SLA
/// aging, and `generated_at`) are injected so the caller owns both.
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
    /// Lower bound of the install-history lookback, as Unix seconds.
    installed_after: i64,
}

impl QueryPlan {
    fn build(args: PatchQueryArgs, install_window_days: i64, now: DateTime<Utc>) -> Self {
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
        // narrow the current-patch feed for display. A FAILED patch is one whose
        // install was attempted and failed, so it never appears in the current feed
        // ("patches for which there were no installation attempts") — only in the
        // install history.
        let want_installs = args.statuses.iter().any(|s| s.is_install_history());
        let current_status_set: HashSet<&'static str> = args
            .statuses
            .iter()
            .filter(|s| !s.is_install_history())
            .map(|s| s.api_value())
            .collect();
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

        Self {
            filter,
            patch_df,
            want_installs,
            current_status_set,
            install_status_set,
            install_status,
            include_os: args.patch_type.includes_os(),
            include_sw: args.patch_type.includes_software(),
            installed_after: (now - Duration::days(days)).timestamp(),
        }
    }
}

/// Everything a query fetched, before scoping and joining.
struct FetchedSources {
    devices: Arc<Vec<Device>>,
    current: CurrentPatches,
    orgs: Arc<Vec<Organization>>,
    locations: Arc<Vec<Location>>,
    roles: Arc<Vec<Role>>,
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
#[allow(clippy::too_many_arguments)]
async fn run_query<L, D, C>(
    api: &NinjaApiClient,
    lookups: L,
    devices_fut: D,
    current_fut: C,
    install_window_days: i64,
    sla_days: i64,
    args: PatchQueryArgs,
    now: DateTime<Utc>,
    progress: &(dyn Fn(&'static str, usize) + Send + Sync),
) -> anyhow::Result<QueryResult>
where
    L: std::future::Future<Output = anyhow::Result<Lookups>>,
    D: std::future::Future<Output = anyhow::Result<Arc<Vec<Device>>>>,
    C: std::future::Future<Output = anyhow::Result<CurrentPatches>>,
{
    let plan = QueryPlan::build(args, install_window_days, now);

    // The cached whole-fleet devices/current-patches (futures), the lookups, and the
    // per-query install history are all independent — resolve them concurrently so
    // latency is the slowest call, not the sum. The install fetch resolves to empty
    // when no install status / matching family is requested.
    let p_os_inst = |n: usize| progress("osInstalls", n);
    let p_sw_inst = |n: usize| progress("swInstalls", n);
    let patch_df_ref = plan.patch_df.as_deref();

    let (devices, current, (orgs, locations, roles), os_installs, sw_installs) = tokio::try_join!(
        devices_fut,
        current_fut,
        lookups,
        async {
            if plan.want_installs && plan.include_os {
                api.fleet_os_patch_installs(
                    patch_df_ref,
                    plan.install_status,
                    plan.installed_after,
                    None,
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
                    plan.installed_after,
                    None,
                    Some(&p_sw_inst as &ProgressFn),
                )
                .await
            } else {
                Ok(Vec::new())
            }
        },
    )?;

    // Fetches done; the rest is the in-memory scope + join/rollup.
    progress("joining", 0);
    Ok(assemble_result(
        &plan,
        FetchedSources {
            devices,
            current,
            orgs,
            locations,
            roles,
            os_installs,
            sw_installs,
        },
        sla_days,
        now,
    ))
}

/// Scopes the whole-fleet caches client-side, joins devices to patches, and
/// computes every rollup. No I/O — everything it needs has already been fetched.
fn assemble_result(
    plan: &QueryPlan,
    src: FetchedSources,
    sla_days: i64,
    now: DateTime<Utc>,
) -> QueryResult {
    let maps = LookupMaps::build(&src.orgs, &src.locations, &src.roles);

    // Scope the whole-fleet caches to the selected identity facets (org/location/
    // role/class) client-side — this is what makes a re-filter a no-refetch
    // operation, replacing the old per-query device/patch `df`. `devices_by_id` then
    // holds only in-scope devices, so every downstream rollup is scoped through it.
    let has_scope = plan.filter.has_identity_scope();
    let scoped_devices: Vec<&Device> = src
        .devices
        .iter()
        .filter(|d| plan.filter.device_allowed(d))
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
    let os_install_refs: Vec<&Patch> = src.os_installs.iter().collect();
    let sw_install_refs: Vec<&Patch> = src.sw_installs.iter().collect();
    let mut rows = {
        let mut sources = vec![
            PatchSource {
                patches: &scoped_os_current,
                type_label: "OS",
                status_override: None,
                status_filter: Some(&plan.current_status_set),
            },
            PatchSource {
                patches: &scoped_sw_current,
                type_label: "SOFTWARE",
                status_override: None,
                status_filter: Some(&plan.current_status_set),
            },
        ];
        if plan.want_installs {
            // The install endpoints return both successful and failed records, so
            // narrow each to the requested install statuses; the override labels a
            // record that omits its own status (defaulting it to INSTALLED).
            sources.push(PatchSource {
                patches: &os_install_refs,
                type_label: "OS",
                status_override: Some("INSTALLED"),
                status_filter: Some(&plan.install_status_set),
            });
            sources.push(PatchSource {
                patches: &sw_install_refs,
                type_label: "SOFTWARE",
                status_override: Some("INSTALLED"),
                status_filter: Some(&plan.install_status_set),
            });
        }
        build_rows(&devices_by_id, &maps, &sources, &plan.filter)
    };
    // Highest severity first, then organization, then device — case-insensitive.
    // sort_by_cached_key lowercases each field once instead of on every compare.
    rows.sort_by_cached_key(|r| {
        (
            Reverse(r.severity_rank),
            r.organization.to_lowercase(),
            r.device_name.to_lowercase(),
        )
    });

    // Compliance + reboot rollups from the scoped current set. Concatenating the two
    // families copies pointers, not patches.
    let all_current: Vec<&Patch> = scoped_os_current
        .iter()
        .chain(&scoped_sw_current)
        .copied()
        .collect();
    let counts = pending_counts(&all_current);
    let summaries = build_device_summaries(&scoped_devices, &counts, &maps);
    let compliance = build_compliance(
        &summaries,
        &all_current,
        &devices_by_id,
        &maps,
        sla_days,
        now,
    );
    let compliance_by_os =
        build_compliance_by_os(&summaries, &all_current, &devices_by_id, sla_days, now);

    // Dashboard/failure rollups. Failures are derived from the FAILED rows already
    // joined (present only when the FAILED status was requested — no extra fetch);
    // the severity/age distributions come from the current pending backlog.
    let failures = build_failures(&rows);
    let severity_by_org = build_severity_by_org(&all_current, &devices_by_id, &maps);
    let age_buckets = build_age_buckets(&all_current, now);

    QueryResult {
        rows,
        devices: summaries,
        compliance,
        compliance_by_os,
        failures,
        severity_by_org,
        age_buckets,
        devices_total: scoped_devices.len(),
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
    // `with_current_result` runs the page-slice under the lock and only against a
    // result belonging to the current tenant (a tenant switch reads as empty). The
    // sort is an in-memory, bounded ref-sort — still "locks are brief, never across
    // await".
    let rows = state
        .with_current_result(|r| page_rows(&r.rows, offset, limit, sort))
        .map_err(|_| UiError::new("result cache poisoned"))?
        .unwrap_or_default();
    Ok(rows)
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
    let page = state
        .with_current_result(|r| group_page(&r.rows, group_by, offset, limit))
        .map_err(|_| UiError::new("result cache poisoned"))?
        .unwrap_or_default();
    Ok(page)
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
    let rows = state
        .with_current_result(|r| group_member_page(&r.rows, group_by, &key, offset, limit))
        .map_err(|_| UiError::new("result cache poisoned"))?
        .unwrap_or_default();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthState;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn empty_summary() -> QuerySummary {
        QuerySummary::from_result(
            &QueryResult {
                rows: Vec::new(),
                devices: Vec::new(),
                compliance: Vec::new(),
                compliance_by_os: Vec::new(),
                failures: Vec::new(),
                severity_by_org: Vec::new(),
                age_buckets: Vec::new(),
                devices_total: 0,
                generated_at: "2026-01-01 00:00:00 UTC".into(),
                data_fetched_at: "2026-01-01 00:00:00 UTC".into(),
            },
            FIRST_PAGE_ROWS,
        )
    }

    /// The frontend discards a superseded response itself (it applies only its own
    /// newest `query_seq`), so this must not become an error the operator never
    /// caused — a manual Run overtaken by an auto-refresh tick is routine.
    #[test]
    fn a_superseded_query_still_returns_its_summary() {
        assert!(summary_for(StoreOutcome::Superseded, empty_summary(), 7).is_ok());
        assert!(summary_for(StoreOutcome::Stored, empty_summary(), 7).is_ok());
    }

    /// The frontend has no guard for this: `query_seq` counts runs it *starts*, and
    /// switching instance never bumps it, so a returned summary was rendered — the
    /// previous tenant's rows and rollups over the new tenant's empty cache, while
    /// paging and export read the miss. Returning the summary here is the bug.
    #[test]
    fn a_tenant_switch_mid_query_refuses_to_hand_back_a_renderable_summary() {
        let err = summary_for(StoreOutcome::TenantChanged, empty_summary(), 7)
            .expect_err("a result the cache refused must not be rendered");
        assert!(
            err.message.contains("instance changed"),
            "the operator just switched instance; say so instead of a generic failure: {}",
            err.message
        );
    }

    /// Same shape: the rows would be on screen while every path that re-reads them
    /// (paging, export, the HTML report) fails.
    #[test]
    fn a_poisoned_cache_refuses_to_hand_back_a_renderable_summary() {
        assert!(summary_for(StoreOutcome::Poisoned, empty_summary(), 7).is_err());
    }

    /// `FIRST_PAGE_ROWS` and the frontend's `PATCHES_PAGE_SIZE` must agree: the
    /// summary seeds page 0 inline, and the frontend renders that seed directly
    /// rather than fetching it. If the backend sends more rows than a page holds,
    /// the surplus is invisible until the operator pages away and back; if it sends
    /// fewer, page 0 is short while `rows_total` promises more.
    ///
    /// The two crates share no code (native vs wasm32 targets), so the only thing
    /// linking them was a doc comment saying "must match". This reads the frontend's
    /// declaration and fails on drift.
    #[test]
    fn the_seeded_first_page_matches_the_frontend_page_size() {
        const FRONTEND_APP_RS: &str = include_str!("../../../web-rs/src/app.rs");

        let declared = FRONTEND_APP_RS
            .lines()
            .find_map(|l| l.trim().strip_prefix("const PATCHES_PAGE_SIZE: usize = "))
            .and_then(|v| v.trim_end_matches(';').parse::<usize>().ok())
            .expect("web-rs/src/app.rs must declare `const PATCHES_PAGE_SIZE: usize = N;`");

        assert_eq!(
            FIRST_PAGE_ROWS, declared,
            "FIRST_PAGE_ROWS ({FIRST_PAGE_ROWS}) and the frontend's PATCHES_PAGE_SIZE \
             ({declared}) must be the same number"
        );
    }

    fn args_with(statuses: Vec<PatchStatus>, patch_type: PatchType) -> PatchQueryArgs {
        PatchQueryArgs {
            filter: FilterParams::default(),
            patch_type,
            statuses,
            install_after_days: None,
        }
    }

    /// Both `Installed` *and* `Failed` are install results and must route to the
    /// history endpoints. Routing `Failed` to the current feed was a real bug — it
    /// never appears there ("patches for which there were no installation
    /// attempts"), so a FAILED query returned nothing at all.
    #[test]
    fn install_results_route_to_history_and_the_rest_narrow_the_current_feed() {
        let history = QueryPlan::build(
            args_with(vec![PatchStatus::Failed], PatchType::All),
            30,
            fixed_now(),
        );
        assert!(history.want_installs);
        assert!(
            history.current_status_set.is_empty(),
            "a FAILED-only query must not narrow the current feed to FAILED, which \
             would starve the compliance and severity rollups"
        );

        let pending = QueryPlan::build(
            args_with(vec![PatchStatus::Pending], PatchType::All),
            30,
            fixed_now(),
        );
        assert!(
            !pending.want_installs,
            "no history fetch for a pending query"
        );
        assert!(!pending.current_status_set.is_empty());
    }

    /// The pushdown is what keeps the failure dashboard from downloading the
    /// window's successful installs just to drop them — but with both statuses
    /// requested it must be absent, or the other kind of record never arrives.
    #[test]
    fn a_single_install_status_is_pushed_down_and_two_are_not() {
        let one = QueryPlan::build(
            args_with(vec![PatchStatus::Failed], PatchType::All),
            30,
            fixed_now(),
        );
        assert_eq!(one.install_status, Some("FAILED"));

        let both = QueryPlan::build(
            args_with(
                vec![PatchStatus::Failed, PatchStatus::Installed],
                PatchType::All,
            ),
            30,
            fixed_now(),
        );
        assert_eq!(both.install_status, None);
        assert_eq!(both.install_status_set.len(), 2);
    }

    /// `Duration::days` panics on an out-of-range count, and a settings.json
    /// predating the range validation (or edited by hand) can still carry one. A
    /// zero or negative lookback is worse than useless: it inverts into a *future*
    /// lower bound that matches no install history at all.
    #[test]
    fn the_install_window_is_clamped_against_hand_edited_settings() {
        let now = fixed_now();
        for (requested, expect_days) in [(Some(0), 1), (Some(-5), 1), (Some(7), 7)] {
            let plan = QueryPlan::build(
                PatchQueryArgs {
                    install_after_days: requested,
                    ..args_with(vec![PatchStatus::Installed], PatchType::All)
                },
                30,
                now,
            );
            assert_eq!(
                plan.installed_after,
                (now - Duration::days(expect_days)).timestamp(),
                "requested {requested:?} should clamp to {expect_days} day(s)"
            );
        }
        // An absurd stored window must not reach `Duration::days` unclamped.
        let huge = QueryPlan::build(
            PatchQueryArgs {
                install_after_days: Some(i64::MAX),
                ..args_with(vec![PatchStatus::Installed], PatchType::All)
            },
            30,
            now,
        );
        assert_eq!(
            huge.installed_after,
            (now - Duration::days(MAX_WINDOW_DAYS)).timestamp()
        );
    }

    /// The relative first-seen window is resolved to an absolute bound here because
    /// `build_rows`, which applies it, has no clock.
    #[test]
    fn a_relative_detection_window_becomes_an_absolute_lower_bound() {
        let now = fixed_now();
        let plan = QueryPlan::build(
            PatchQueryArgs {
                filter: FilterParams {
                    detected_within_days: Some(7),
                    ..FilterParams::default()
                },
                ..args_with(vec![PatchStatus::Pending], PatchType::All)
            },
            30,
            now,
        );
        assert_eq!(
            plan.filter.detected_after,
            Some((now - Duration::days(7)).timestamp())
        );
    }

    /// A family the query cannot display is never worth a whole-fleet page-through.
    #[test]
    fn the_requested_patch_type_decides_which_families_are_fetched() {
        let os = QueryPlan::build(
            args_with(vec![PatchStatus::Pending], PatchType::Os),
            30,
            fixed_now(),
        );
        assert!(os.include_os && !os.include_sw);

        let all = QueryPlan::build(
            args_with(vec![PatchStatus::Pending], PatchType::All),
            30,
            fixed_now(),
        );
        assert!(all.include_os && all.include_sw);
    }

    /// A fixed clock so the release/install windows, SLA aging, and `generated_at`
    /// are deterministic regardless of when the test runs.
    fn fixed_now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap() // 2023-11-14T22:13:20Z
    }

    /// Lookups resolved up front — the org/location/role *fetch* is covered by the
    /// `api::mod` tests; here they only need to label rows so the join is assertable.
    fn lookups() -> Lookups {
        (
            Arc::new(vec![Organization {
                id: 1,
                name: "Alpha".into(),
            }]),
            Arc::new(vec![]),
            Arc::new(vec![]),
        )
    }

    fn client(server: &MockServer) -> NinjaApiClient {
        let http = reqwest::Client::new();
        let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
        NinjaApiClient::new(http, auth)
    }

    /// The whole-fleet device future `run_query` now expects, backed by the test's
    /// `/devices-detailed` mock (the caching itself lives in `AppState` and is
    /// exercised separately). Keeps the existing per-test device mocks in play.
    async fn fleet_devices_via(c: &NinjaApiClient) -> anyhow::Result<Arc<Vec<Device>>> {
        Ok(Arc::new(c.devices(None, None).await?))
    }

    /// The whole-fleet current-patches future, backed by the test's
    /// `/queries/os-patches` mock (software-patches is left empty — the OS feed is
    /// what these joins assert). `fetched_at` is fixed for determinism.
    async fn fleet_current_via(c: &NinjaApiClient) -> anyhow::Result<CurrentPatches> {
        Ok(CurrentPatches {
            os: Arc::new(c.fleet_os_patches(None, None, None).await?),
            sw: Arc::new(Vec::new()),
            fetched_at: fixed_now(),
        })
    }

    fn args(patch_type: PatchType, statuses: Vec<PatchStatus>) -> PatchQueryArgs {
        PatchQueryArgs {
            filter: FilterParams::default(),
            patch_type,
            statuses,
            install_after_days: None,
        }
    }

    fn dev(id: i64, org: i64) -> Device {
        Device {
            id,
            system_name: Some(format!("srv{id}")),
            display_name: None,
            organization_id: Some(org),
            location_id: None,
            node_role_id: None,
            node_class: Some("WINDOWS_SERVER".into()),
            offline: Some(false),
            os: None,
        }
    }

    fn cur(device_id: i64, kb: &str, status: &str, severity: &str) -> Patch {
        Patch {
            device_id: Some(device_id),
            kb_number: Some(kb.into()),
            name: None,
            version: None,
            product_vendor: None,
            severity: Some(severity.into()),
            status: Some(status.into()),
            patch_type: None,
            collected_timestamp: Some(fixed_now().timestamp() as f64),
            installed_timestamp: None,
        }
    }

    #[tokio::test]
    async fn pending_query_joins_current_feed_and_maps_manual_to_pending() {
        let server = MockServer::start().await;

        // Two online devices in org Alpha; device 10 needs a reboot.
        Mock::given(method("GET"))
            .and(path("/api/v2/devices-detailed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "id": 10, "systemName": "web-01", "organizationId": 1,
                    "offline": false, "os": { "name": "Windows Server 2022", "needsReboot": true }
                },
                {
                    "id": 20, "systemName": "web-02", "organizationId": 1,
                    "offline": false, "os": { "name": "Windows Server 2019", "needsReboot": false }
                }
            ])))
            .mount(&server)
            .await;

        // Current OS-patch feed: one MANUAL (pending) Critical aged past SLA, one
        // APPROVED Low. With statuses=[Pending] only the MANUAL one becomes a row.
        let aged = fixed_now().timestamp() - 60 * 86_400; // 60 days old
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "deviceId": 10, "kbNumber": "KB1", "status": "MANUAL",
                      "severity": "CRITICAL", "timestamp": aged },
                    { "deviceId": 20, "kbNumber": "KB2", "status": "APPROVED",
                      "severity": "LOW", "timestamp": aged }
                ],
                "cursor": ""
            })))
            .mount(&server)
            .await;

        let progress = |_: &'static str, _: usize| {};
        let result = run_query(
            &client(&server),
            async { Ok::<_, anyhow::Error>(lookups()) },
            fleet_devices_via(&client(&server)),
            fleet_current_via(&client(&server)),
            30,
            30,
            args(PatchType::Os, vec![PatchStatus::Pending]),
            fixed_now(),
            &progress,
        )
        .await
        .expect("query");

        // Only the MANUAL patch survives the Pending filter, displayed as PENDING.
        assert_eq!(result.rows.len(), 1);
        let row = &result.rows[0];
        assert_eq!(row.kb.as_deref(), Some("KB1"));
        assert_eq!(row.status, "PENDING");
        assert_eq!(row.organization, "Alpha");
        assert_eq!(row.severity, "Critical");
        assert!(row.needs_reboot);

        // Both devices counted; only device 10 lands in the reboot subset.
        assert_eq!(result.devices_total, 2);
        let summary = QuerySummary::from_result(&result, FIRST_PAGE_ROWS);
        assert_eq!(summary.reboot_devices.len(), 1);
        assert_eq!(summary.reboot_devices[0].device_id, 10);

        // Compliance: both online, both carry a pending/approved patch → 0% compliant.
        // The MANUAL Critical (aged) lands in pending_critical AND aged_critical; the
        // APPROVED Low is below the Important rank so neither counts it.
        assert_eq!(result.compliance.len(), 1);
        let alpha = &result.compliance[0];
        assert_eq!(alpha.organization, "Alpha");
        assert_eq!(alpha.devices_total, 2);
        assert_eq!(alpha.devices_compliant, 0);
        assert_eq!(alpha.compliance_pct, 0.0);
        assert_eq!(alpha.pending_critical, 1);
        assert_eq!(alpha.aged_critical, 1);
    }

    #[tokio::test]
    async fn installed_query_routes_to_history_endpoint_not_current_feed() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v2/devices-detailed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false }
            ])))
            .mount(&server)
            .await;

        // The current feed is still fetched (include_os) but must contribute no rows
        // for an install-only status — this MANUAL record has to be ignored.
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "deviceId": 10, "kbNumber": "KBCURRENT", "status": "MANUAL",
                      "severity": "CRITICAL" }
                ],
                "cursor": ""
            })))
            .mount(&server)
            .await;

        // The install-history endpoint returns one INSTALLED and one FAILED record;
        // statuses=[Installed] keeps only the INSTALLED one.
        let installed = fixed_now().timestamp() - 5 * 86_400;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patch-installs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "deviceId": 10, "kbNumber": "KBOK", "status": "INSTALLED",
                      "installedAt": installed },
                    { "deviceId": 10, "kbNumber": "KBBAD", "status": "FAILED" }
                ],
                "cursor": ""
            })))
            .mount(&server)
            .await;

        let progress = |_: &'static str, _: usize| {};
        let result = run_query(
            &client(&server),
            async { Ok::<_, anyhow::Error>(lookups()) },
            fleet_devices_via(&client(&server)),
            fleet_current_via(&client(&server)),
            30,
            30,
            args(PatchType::Os, vec![PatchStatus::Installed]),
            fixed_now(),
            &progress,
        )
        .await
        .expect("query");

        // Exactly the install-history INSTALLED record — the current MANUAL row and
        // the FAILED install are both excluded.
        assert_eq!(result.rows.len(), 1);
        let row = &result.rows[0];
        assert_eq!(row.kb.as_deref(), Some("KBOK"));
        assert_eq!(row.status, "INSTALLED");
        assert!(row.installed_date.is_some());

        // No FAILED status was requested, so the failure rollup is empty.
        assert!(result.failures.is_empty());
    }

    #[tokio::test]
    async fn failed_query_populates_the_failure_rollup_grouped_by_patch() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v2/devices-detailed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false },
                { "id": 20, "systemName": "web-02", "organizationId": 1, "offline": false }
            ])))
            .mount(&server)
            .await;

        // Current feed contributes nothing for a FAILED-only query.
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [], "cursor": ""
            })))
            .mount(&server)
            .await;

        // The same KB fails on two devices; a different KB fails on one.
        let failed_at = fixed_now().timestamp() - 2 * 86_400;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patch-installs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "deviceId": 10, "kbNumber": "KBFAIL", "status": "FAILED",
                      "severity": "CRITICAL", "installedAt": failed_at },
                    { "deviceId": 20, "kbNumber": "KBFAIL", "status": "FAILED",
                      "severity": "CRITICAL", "installedAt": failed_at },
                    { "deviceId": 10, "kbNumber": "KBOTHER", "status": "FAILED",
                      "severity": "IMPORTANT", "installedAt": failed_at }
                ],
                "cursor": ""
            })))
            .mount(&server)
            .await;

        let progress = |_: &'static str, _: usize| {};
        let result = run_query(
            &client(&server),
            async { Ok::<_, anyhow::Error>(lookups()) },
            fleet_devices_via(&client(&server)),
            fleet_current_via(&client(&server)),
            30,
            30,
            args(PatchType::Os, vec![PatchStatus::Failed]),
            fixed_now(),
            &progress,
        )
        .await
        .expect("query");

        assert_eq!(result.failures.len(), 2, "one group per failing patch");
        let top = &result.failures[0];
        assert_eq!(top.kb.as_deref(), Some("KBFAIL"));
        assert_eq!(top.affected_devices, 2, "KBFAIL failed on two devices");
    }

    #[tokio::test]
    async fn failed_only_query_pushes_status_filter_to_the_install_endpoint() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v2/devices-detailed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false }
            ])))
            .mount(&server)
            .await;

        // The current feed is still fetched (it drives compliance) but contributes
        // no rows for a FAILED-only query.
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [], "cursor": ""
            })))
            .mount(&server)
            .await;

        // This install mock matches ONLY when status=FAILED is present. If the
        // server-side pushdown regressed (no status param sent), nothing would match
        // the install request and run_query would error on the 404 instead of
        // returning the FAILED row — so the assertion below is the pushdown proof.
        let failed_at = fixed_now().timestamp() - 2 * 86_400;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patch-installs"))
            .and(query_param("status", "FAILED"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "deviceId": 10, "kbNumber": "KBFAIL", "status": "FAILED",
                      "severity": "CRITICAL", "installedAt": failed_at }
                ],
                "cursor": ""
            })))
            .mount(&server)
            .await;

        let progress = |_: &'static str, _: usize| {};
        let result = run_query(
            &client(&server),
            async { Ok::<_, anyhow::Error>(lookups()) },
            fleet_devices_via(&client(&server)),
            fleet_current_via(&client(&server)),
            30,
            30,
            args(PatchType::Os, vec![PatchStatus::Failed]),
            fixed_now(),
            &progress,
        )
        .await
        .expect("a FAILED-only query must send status=FAILED to the install endpoint");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].status, "FAILED");
        assert_eq!(result.failures.len(), 1);
    }

    #[tokio::test]
    async fn installed_and_failed_query_omits_the_server_side_status_filter() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v2/devices-detailed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 10, "systemName": "web-01", "organizationId": 1, "offline": false }
            ])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [], "cursor": ""
            })))
            .mount(&server)
            .await;

        // Both INSTALLED and FAILED are requested, so neither can be dropped
        // server-side — the call must omit `status`, and this mock matches only then.
        let ts = fixed_now().timestamp() - 86_400;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/os-patch-installs"))
            .and(query_param_is_missing("status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "deviceId": 10, "kbNumber": "KBOK", "status": "INSTALLED", "installedAt": ts },
                    { "deviceId": 10, "kbNumber": "KBBAD", "status": "FAILED", "installedAt": ts }
                ],
                "cursor": ""
            })))
            .mount(&server)
            .await;

        let progress = |_: &'static str, _: usize| {};
        let result = run_query(
            &client(&server),
            async { Ok::<_, anyhow::Error>(lookups()) },
            fleet_devices_via(&client(&server)),
            fleet_current_via(&client(&server)),
            30,
            30,
            args(
                PatchType::Os,
                vec![PatchStatus::Installed, PatchStatus::Failed],
            ),
            fixed_now(),
            &progress,
        )
        .await
        .expect("an INSTALLED+FAILED query must omit the server-side status filter");

        // Both records survive: one INSTALLED, one FAILED.
        assert_eq!(result.rows.len(), 2);
        assert!(result.rows.iter().any(|r| r.status == "INSTALLED"));
        assert!(result.rows.iter().any(|r| r.status == "FAILED"));
    }

    #[tokio::test]
    async fn org_scope_filters_cached_fleet_client_side_without_a_df() {
        // The whole-fleet devices + current patches are supplied directly (as the
        // caches would hand them over), spanning two orgs. An org=1 scope must narrow
        // them client-side — no `df`, no API call (no install status is requested, so
        // the api client is never touched) — leaving only Alpha's device and patch in
        // the rows AND the compliance rollup.
        let devices = Arc::new(vec![dev(10, 1), dev(20, 2)]);
        let os_current = Arc::new(vec![
            cur(10, "KB1", "MANUAL", "CRITICAL"), // org 1 (Alpha) — in scope
            cur(20, "KB2", "MANUAL", "CRITICAL"), // org 2 (Beta) — out of scope
        ]);
        let lookups = (
            Arc::new(vec![
                Organization {
                    id: 1,
                    name: "Alpha".into(),
                },
                Organization {
                    id: 2,
                    name: "Beta".into(),
                },
            ]),
            Arc::new(vec![]),
            Arc::new(vec![]),
        );

        let mut a = args(PatchType::Os, vec![PatchStatus::Pending]);
        a.filter.organization_id = Some(1);

        let http = reqwest::Client::new();
        let api = NinjaApiClient::new(
            http.clone(),
            AuthState::seeded(http, "http://127.0.0.1:0".into(), "t"),
        );
        let progress = |_: &'static str, _: usize| {};
        let result = run_query(
            &api,
            async { Ok::<_, anyhow::Error>(lookups) },
            async { Ok::<_, anyhow::Error>(devices) },
            async {
                Ok::<_, anyhow::Error>(CurrentPatches {
                    os: os_current,
                    sw: Arc::new(Vec::new()),
                    fetched_at: fixed_now(),
                })
            },
            30,
            30,
            a,
            fixed_now(),
            &progress,
        )
        .await
        .expect("query");

        assert_eq!(
            result.devices_total, 1,
            "only the in-scope org's device counts"
        );
        assert_eq!(result.rows.len(), 1, "only Alpha's patch becomes a row");
        assert_eq!(result.rows[0].kb.as_deref(), Some("KB1"));
        assert_eq!(result.rows[0].organization, "Alpha");
        assert_eq!(
            result.compliance.len(),
            1,
            "only Alpha in the compliance roll"
        );
        assert_eq!(result.compliance[0].organization, "Alpha");
        assert_eq!(result.compliance[0].pending_critical, 1);
    }
}
