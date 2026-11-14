use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use tracing::{Instrument, info, info_span, warn};

use super::paging::REPORTING_PAGE_SIZE;
use super::{NinjaApiClient, ProgressFn, df_query};
use crate::model::{Device, Patch};

const SOFTWARE_PATCHES: &str = "/queries/software-patches";
const OS_PATCH_INSTALLS: &str = "/queries/os-patch-installs";
const SOFTWARE_PATCH_INSTALLS: &str = "/queries/software-patch-installs";

/// When a feed is paged as concurrent location slices, and into how many.
///
/// One cursor scan pages strictly in sequence, and every page is a chance to lose
/// one: on 2026-10-26 the 563,255-row third-party feed (113 pages) lost a page every
/// 19–85 pages, and each loss cost the scan all it had paged. Slices cut both the
/// wait and the exposure. The count used to be a flat four whatever the feed held,
/// so a 3,000-row feed paid five sizing requests and four scans for what is one
/// page, and a six-figure one still ran 28 pages a slice.
#[derive(Debug, Clone, Copy)]
pub(super) struct SlicePolicy {
    /// A feed shorter than this is paged whole: a few pages in a row cost less than
    /// sizing the slices does.
    pub(super) min_rows: usize,
    /// Rows one slice is cut to hold.
    pub(super) rows_per_slice: usize,
    /// Most slices paged at once. They share the tenant's rate limit with every
    /// other leg of the query, the job poller and the `/jobs` poll.
    pub(super) max_slices: usize,
}

/// Four pages before a feed is sliced, twelve pages a slice, six slices at most:
/// under 240,000 rows this asks for no more than the four scans it always did.
pub(super) const SLICE_POLICY: SlicePolicy = SlicePolicy {
    min_rows: 4 * REPORTING_PAGE_SIZE as usize,
    rows_per_slice: 12 * REPORTING_PAGE_SIZE as usize,
    max_slices: 6,
};

impl SlicePolicy {
    /// How many slices a feed of `total` rows is paged in; `1` pages it whole.
    pub(super) fn slices_for(&self, total: usize) -> usize {
        if total < self.min_rows {
            1
        } else {
            total
                .div_ceil(self.rows_per_slice.max(1))
                .clamp(2, self.max_slices.max(2))
        }
    }
}

/// The most locations one slice names. Each id adds a few characters to the URL,
/// and a request line past a proxy's limit (8 KB is common) fails outright, so a
/// tenant with more locations than the slices can name pages the feed whole.
const MAX_SLICE_LOCATIONS: usize = 200;

/// The fleet's locations and the weight each is balanced by, from which a feed's
/// `loc` slices are cut ([`SlicePlan::clauses`]).
///
/// Location rather than organization because it is the finer of the two facets the
/// `/queries/*` endpoints honor in `df`: a fleet in one organization still splits.
///
/// A plan is only ever a *proposal*. NinjaOne drops a `df` clause it does not honor
/// without saying so, and a plan can miss rows (a device with no location, a
/// location the list does not name), so the fetch pages the slices only once their
/// sizes — and then their rows — add up to the whole feed's
/// ([`NinjaApiClient::paged_in_slices`]). That check is what lets a plan come from
/// whatever is already in hand, however stale, instead of from a fresh inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicePlan {
    /// `(location id, weight)`, heaviest first, ties by id.
    locations: Vec<(i64, usize)>,
}

impl SlicePlan {
    /// A plan balanced by device count. `None` when the fleet cannot be cut in two:
    /// fewer than two locations, or a device with none (no clause could name it).
    pub fn from_devices(devices: &[Device]) -> Option<Self> {
        let mut per_location: HashMap<i64, usize> = HashMap::new();
        for device in devices {
            *per_location.entry(device.location_id?).or_default() += 1;
        }
        Self::weighted(per_location.into_iter().collect())
    }

    /// A plan over a bare list of location ids, every one weighted alike — for when
    /// no inventory is in hand to count devices by. Less even than
    /// [`Self::from_devices`], and just as safe: the sizes decide.
    pub fn from_locations(ids: impl IntoIterator<Item = i64>) -> Option<Self> {
        let unique: HashSet<i64> = ids.into_iter().collect();
        Self::weighted(unique.into_iter().map(|id| (id, 1)).collect())
    }

    fn weighted(mut locations: Vec<(i64, usize)>) -> Option<Self> {
        if locations.len() < 2 {
            return None;
        }
        locations.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Some(Self { locations })
    }

    /// Up to `wanted` `loc` clauses that together name every location, balanced by
    /// weight: largest first, each onto the lightest slice so far, so the split is
    /// close to even unless one location dominates, and that one then gets a slice
    /// to itself. More slices than `wanted` when that is what keeps each under
    /// [`MAX_SLICE_LOCATIONS`]; `None` when even `max_slices` cannot, or when the
    /// result would be a single slice.
    fn clauses(&self, wanted: usize, max_slices: usize) -> Option<Vec<String>> {
        let to_fit = self.locations.len().div_ceil(MAX_SLICE_LOCATIONS);
        let count = wanted.max(to_fit).min(self.locations.len());
        if count < 2 || count > max_slices {
            return None;
        }
        let mut slices: Vec<(usize, Vec<i64>)> = vec![(0, Vec::new()); count];
        for &(location, weight) in &self.locations {
            let lightest = (0..slices.len())
                .min_by_key(|&slice| slices[slice].0)
                .unwrap_or(0);
            slices[lightest].0 += weight;
            slices[lightest].1.push(location);
        }
        if slices
            .iter()
            .any(|(_, ids)| ids.len() > MAX_SLICE_LOCATIONS)
        {
            return None;
        }
        Some(
            slices
                .iter()
                .filter_map(|(_, ids)| crate::filter::id_clause("loc", ids))
                .collect(),
        )
    }
}

impl NinjaApiClient {
    /// Current OS patches across the filtered fleet — those "for which there were
    /// no installation attempts" (status MANUAL/APPROVED/REJECTED). FAILED/INSTALLED
    /// are install *results* and live on `fleet_os_patch_installs`, not here.
    /// `status` narrows to a single NinjaOne status when supplied.
    pub async fn fleet_os_patches(
        &self,
        df: Option<&str>,
        status: Option<&str>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        self.get_paginated_reporting(
            "/queries/os-patches",
            &patch_query(df, status),
            REPORTING_PAGE_SIZE,
            on_progress,
        )
        .await
    }

    /// Current third-party patches under `df` (the whole fleet when `None`), paged
    /// as concurrent location slices of `plan` when the feed is long enough to be
    /// worth it — see [`Self::paged_in_slices`].
    pub async fn fleet_software_patches_sliced(
        &self,
        df: Option<&str>,
        plan: Option<&SlicePlan>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        self.paged_in_slices(SLICE_POLICY, SOFTWARE_PATCHES, df, &[], plan, on_progress)
            .await
    }

    /// Pages one `/queries/*` patch feed — `rest` is its query without the `df` —
    /// as concurrent location slices of `plan` rather than one long scan, when
    /// `policy` says the feed is long enough and the slices provably add up to it.
    ///
    /// The whole feed is sized first, off a one-row page; a short feed is then
    /// simply paged, at the cost of that one request. Otherwise a one-row page per
    /// slice reads their sizes, which must sum to exactly the whole's. That catches
    /// a `df` NinjaOne ignores (an unhonored clause is dropped silently, so every
    /// slice would be the whole feed) and rows no slice names (a device with no
    /// location, a location the plan does not list) before either costs a download.
    /// The paged rows must then add up to the same total, which catches a device
    /// that moved between slices mid-fetch. Anything else — no plan, a size that
    /// cannot be read, a sum that does not match — pages the feed whole.
    ///
    /// A slice's `df` is the caller's own narrowed by a `loc` clause, so a scoped
    /// feed slices like the whole fleet's.
    pub(super) async fn paged_in_slices(
        &self,
        policy: SlicePolicy,
        path: &'static str,
        df: Option<&str>,
        rest: &[(&'static str, String)],
        plan: Option<&SlicePlan>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        let query_for = |df: Option<&str>| {
            let mut query = df_query(df);
            query.extend_from_slice(rest);
            query
        };
        let whole_query = query_for(df);
        let whole =
            || self.get_paginated_reporting(path, &whole_query, REPORTING_PAGE_SIZE, on_progress);
        let Some(plan) = plan else {
            return whole().await;
        };
        let started = std::time::Instant::now();

        let total = match self.feed_size(path, &whole_query).await {
            Ok(Some(total)) => total,
            Ok(None) => return whole().await,
            Err(err) => {
                warn!(
                    path,
                    error = format!("{err:#}").as_str(),
                    "could not size the feed; paging it whole"
                );
                return whole().await;
            }
        };
        let wanted = policy.slices_for(total);
        if wanted < 2 {
            return whole().await;
        }
        let Some(clauses) = plan.clauses(wanted, policy.max_slices) else {
            return whole().await;
        };
        let slices: Vec<String> = clauses
            .into_iter()
            .map(|loc| match df {
                Some(scope) if !scope.is_empty() => format!("{scope} AND {loc}"),
                _ => loc,
            })
            .collect();

        let queries: Vec<_> = slices.iter().map(|df| query_for(Some(df))).collect();
        let sizes = try_join_all(
            queries
                .iter()
                .map(|query| self.feed_size(path, query))
                .collect(),
        )
        .await;
        match sizes {
            Ok(parts) => {
                let sliced = parts.iter().copied().sum::<Option<usize>>();
                if sliced != Some(total) {
                    warn!(
                        path,
                        whole = total,
                        ?parts,
                        "the location slices do not add up to the whole feed; paging it whole"
                    );
                    return whole().await;
                }
            }
            Err(err) => {
                warn!(
                    path,
                    error = format!("{err:#}").as_str(),
                    "could not size the location slices; paging the feed whole"
                );
                return whole().await;
            }
        }

        // Each slice restarts its own progress from zero, so the sum is what the
        // operator sees, against the whole feed's size.
        let loaded: Vec<AtomicUsize> = slices.iter().map(|_| AtomicUsize::new(0)).collect();
        let report = |slice: usize, rows: usize| {
            loaded[slice].store(rows, Ordering::Relaxed);
            if let Some(report) = on_progress {
                report(
                    loaded.iter().map(|n| n.load(Ordering::Relaxed)).sum(),
                    Some(total),
                );
            }
        };
        let report = &report;
        let reporters: Vec<_> = (0..slices.len())
            .map(|slice| move |rows: usize, _: Option<usize>| report(slice, rows))
            .collect();
        let pages = try_join_all(
            queries
                .iter()
                .zip(&reporters)
                .enumerate()
                .map(|(n, (query, reporter))| {
                    // Every log line a slice's scan writes carries which slice it is.
                    self.get_paginated_reporting(
                        path,
                        query,
                        REPORTING_PAGE_SIZE,
                        Some(reporter as &ProgressFn),
                    )
                    .instrument(info_span!(
                        "slice",
                        n = n + 1,
                        of = slices.len()
                    ))
                })
                .collect(),
        )
        .await?;

        // Counted before anything is allocated, and the joined vector sized from the
        // rows in hand: `total` is the count the server announced, and sizing an
        // allocation by it let a bogus `count` abort the whole app.
        let paged: usize = pages.iter().map(Vec::len).sum();
        if paged != total {
            warn!(
                path,
                rows = paged,
                total,
                "the location slices paged a different total than the whole feed holds; paging it whole"
            );
            return whole().await;
        }
        let mut rows = Vec::with_capacity(paged);
        for page in pages {
            rows.extend(page);
        }
        info!(
            path,
            slices = slices.len(),
            rows = rows.len(),
            elapsed_s = started.elapsed().as_secs(),
            "paged in slices"
        );
        Ok(rows)
    }

    /// OS-patch install history within a time window (Unix seconds). The history
    /// endpoint returns both successful and failed records; `status` narrows it to a
    /// single NinjaOne install status (`FAILED`/`INSTALLED`) server-side, so a query
    /// for just one of them isn't forced to download the other and discard it.
    ///
    /// A long pull is paged as location slices of `plan` under the same checks as
    /// the third-party feed ([`Self::paged_in_slices`]): it is fetched afresh by
    /// every query that asks for it, auto-refresh ticks included, and a month of
    /// successful installs on a large fleet is as long a scan as any.
    pub async fn fleet_os_patch_installs(
        &self,
        df: Option<&str>,
        status: Option<&str>,
        installed_after: i64,
        installed_before: Option<i64>,
        plan: Option<&SlicePlan>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        let rest = install_query(status, installed_after, installed_before);
        self.paged_in_slices(
            SLICE_POLICY,
            OS_PATCH_INSTALLS,
            df,
            &rest,
            plan,
            on_progress,
        )
        .await
    }

    /// Software-patch install history within a time window (Unix seconds). `status`
    /// narrows to a single install status server-side, and a long pull is sliced —
    /// see `fleet_os_patch_installs`.
    pub async fn fleet_software_patch_installs(
        &self,
        df: Option<&str>,
        status: Option<&str>,
        installed_after: i64,
        installed_before: Option<i64>,
        plan: Option<&SlicePlan>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        let rest = install_query(status, installed_after, installed_before);
        self.paged_in_slices(
            SLICE_POLICY,
            SOFTWARE_PATCH_INSTALLS,
            df,
            &rest,
            plan,
            on_progress,
        )
        .await
    }
}

fn patch_query(df: Option<&str>, status: Option<&str>) -> Vec<(&'static str, String)> {
    let mut query = df_query(df);
    if let Some(s) = status {
        query.push(("status", s.to_string()));
    }
    query
}

/// Polls every future in `futures` on the current task, as `tokio::try_join!` does
/// for a fixed few, and returns their outputs in order — or the first error,
/// dropping (and so cancelling) the rest. Spawned tasks could not borrow the client
/// and the progress sink, and `futures::future::try_join_all` is a dependency for
/// these few lines.
async fn try_join_all<T, F: Future<Output = Result<T>>>(futures: Vec<F>) -> Result<Vec<T>> {
    let mut futures: Vec<Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut outputs: Vec<Option<T>> = futures.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        for (future, output) in futures.iter_mut().zip(outputs.iter_mut()) {
            if output.is_some() {
                continue;
            }
            match future.as_mut().poll(cx) {
                Poll::Ready(Ok(value)) => *output = Some(value),
                Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                Poll::Pending => {}
            }
        }
        if outputs.iter().all(Option::is_some) {
            Poll::Ready(Ok(outputs.iter_mut().filter_map(Option::take).collect()))
        } else {
            Poll::Pending
        }
    })
    .await
}

/// The install-history query without its `df`, which [`NinjaApiClient::paged_in_slices`]
/// adds — whole, or narrowed per slice.
fn install_query(
    status: Option<&str>,
    installed_after: i64,
    installed_before: Option<i64>,
) -> Vec<(&'static str, String)> {
    let mut query = Vec::new();
    if let Some(s) = status {
        query.push(("status", s.to_string()));
    }
    query.push(("installedAfter", installed_after.to_string()));
    if let Some(before) = installed_before {
        query.push(("installedBefore", before.to_string()));
    }
    query
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fleet(locations: &[(i64, usize)]) -> Vec<Device> {
        let mut id = 0;
        let mut devices = Vec::new();
        for &(location, count) in locations {
            for _ in 0..count {
                id += 1;
                devices.push(
                    serde_json::from_value(serde_json::json!({ "id": id, "locationId": location }))
                        .expect("device"),
                );
            }
        }
        devices
    }

    /// The clauses a plan over `devices` cuts for `wanted` slices, under the
    /// shipped ceiling.
    fn clauses(devices: &[Device], wanted: usize) -> Option<Vec<String>> {
        SlicePlan::from_devices(devices)?.clauses(wanted, SLICE_POLICY.max_slices)
    }

    /// Largest location first, each onto the lightest slice: the six locations land
    /// as 5 / 4 / 4 / 3 devices rather than in the order they were listed.
    #[test]
    fn the_feed_is_sliced_by_location_and_balanced_by_device_count() {
        let devices = fleet(&[(6, 1), (4, 2), (1, 5), (5, 1), (3, 3), (2, 4)]);
        assert_eq!(
            clauses(&devices, 4).expect("a plan"),
            ["loc=1", "loc=2", "loc in (3, 6)", "loc in (4, 5)"]
        );
        assert_eq!(
            clauses(&devices, 2).expect("a plan"),
            ["loc in (1, 4, 5)", "loc in (2, 3, 6)"],
            "fewer slices, still balanced: 8 and 8 devices"
        );
    }

    #[test]
    fn fewer_locations_than_slices_get_one_slice_each() {
        let devices = fleet(&[(7, 3), (8, 1)]);
        assert_eq!(clauses(&devices, 4).expect("a plan"), ["loc=7", "loc=8"]);
    }

    /// With no inventory to count devices by, the cached location list still cuts
    /// the feed: every location weighs the same, so they are dealt out in id order,
    /// and a repeated id names its location once.
    #[test]
    fn a_bare_location_list_is_dealt_out_evenly() {
        let plan = SlicePlan::from_locations([30, 10, 20, 40, 10]).expect("a plan");
        assert_eq!(
            plan.clauses(2, SLICE_POLICY.max_slices).expect("clauses"),
            ["loc in (10, 30)", "loc in (20, 40)"]
        );
        assert_eq!(SlicePlan::from_locations([7]), None);
        assert_eq!(SlicePlan::from_locations([]), None);
    }

    /// A short feed is paged whole, a long one in more slices the longer it is, and
    /// never past the ceiling. Under 240,000 rows that is at most the four scans the
    /// feed was always cut into.
    #[test]
    fn the_slice_count_follows_the_feed_size() {
        let page = REPORTING_PAGE_SIZE as usize;
        let slices = |rows: usize| SLICE_POLICY.slices_for(rows);
        assert_eq!(slices(0), 1);
        assert_eq!(
            slices(4 * page - 1),
            1,
            "three pages and a bit: paged whole"
        );
        assert_eq!(slices(4 * page), 2);
        assert_eq!(slices(24 * page), 2);
        assert_eq!(slices(24 * page + 1), 3);
        assert_eq!(slices(48 * page), 4);
        assert_eq!(
            slices(563_255),
            6,
            "the 113-page feed: six slices of 19 pages"
        );
        assert_eq!(slices(usize::MAX), 6);
    }

    /// The slices finish in whatever order NinjaOne answers them, but their rows are
    /// joined in plan order, and one failure ends the fetch rather than waiting on
    /// the rest.
    #[tokio::test]
    async fn try_join_all_keeps_the_input_order_and_stops_at_the_first_error() {
        let after = |ms: u64, value: Result<u64>| async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            value
        };
        let joined = try_join_all(vec![after(30, Ok(1)), after(0, Ok(2)), after(10, Ok(3))])
            .await
            .expect("all succeed");
        assert_eq!(joined, [1, 2, 3]);

        let started = std::time::Instant::now();
        let failed = try_join_all(vec![
            after(5_000, Ok(1)),
            after(0, Err(anyhow::anyhow!("slice 2 failed"))),
        ])
        .await;
        assert_eq!(failed.expect_err("fails").to_string(), "slice 2 failed");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "the slow slice is dropped, not waited on"
        );
    }

    /// Every way the fleet cannot be cut into location slices that cover it pages the
    /// feed whole: a slice of one is the whole feed, a device with no location is in
    /// no slice, and a slice naming too many ids is a URL a proxy may refuse.
    #[test]
    fn a_fleet_that_cannot_be_sliced_is_paged_whole() {
        assert_eq!(SlicePlan::from_devices(&fleet(&[(1, 9)])), None);
        assert_eq!(SlicePlan::from_devices(&[]), None);

        let mut unplaced = fleet(&[(1, 2), (2, 2)]);
        unplaced[0].location_id = None;
        assert_eq!(SlicePlan::from_devices(&unplaced), None);

        let locations = |n: usize| (1..=n as i64).map(|l| (l, 1)).collect::<Vec<_>>();
        let most = SLICE_POLICY.max_slices * MAX_SLICE_LOCATIONS;
        assert!(clauses(&fleet(&locations(most)), 2).is_some());
        assert_eq!(clauses(&fleet(&locations(most + 1)), 2), None);
    }

    /// A fleet with more locations than the wanted slices can name gets as many more
    /// slices as it takes to keep each URL short, rather than one long scan.
    #[test]
    fn extra_slices_keep_every_slice_under_the_location_cap() {
        let locations: Vec<_> = (1..=(2 * MAX_SLICE_LOCATIONS as i64 + 1))
            .map(|l| (l, 1))
            .collect();
        let cut = clauses(&fleet(&locations), 2).expect("a plan");
        assert_eq!(cut.len(), 3, "two slices would name 201 locations each");
    }

    /// The query builders decide what the *server* filters, so a mistake here is
    /// invisible in the result — it just silently returns the wrong fleet. They had
    /// no tests: the four fleet-scale endpoints were only ever exercised
    /// indirectly through `run_query`, which asserts on rows rather than on the
    /// request. This is the same surface that produced the `releaseDate` incident,
    /// where the fixtures fed the field the code expected and so proved only that
    /// the (wrong) aliasing worked.
    fn keys(q: &[(&'static str, String)]) -> Vec<&'static str> {
        q.iter().map(|(k, _)| *k).collect()
    }

    fn value<'a>(q: &'a [(&'static str, String)], key: &str) -> Option<&'a str> {
        q.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn an_absent_or_empty_df_is_omitted_rather_than_sent_blank() {
        // A `df=` with no value is not "no filter" to the API — omit the parameter.
        assert!(df_query(None).is_empty());
        assert!(df_query(Some("")).is_empty());
        assert_eq!(value(&df_query(Some("org = 1")), "df"), Some("org = 1"));
    }

    #[test]
    fn the_current_feed_sends_status_only_when_one_is_requested() {
        assert_eq!(keys(&patch_query(Some("org = 1"), None)), vec!["df"]);
        assert_eq!(
            keys(&patch_query(Some("org = 1"), Some("APPROVED"))),
            vec!["df", "status"]
        );
        assert_eq!(
            value(&patch_query(None, Some("APPROVED")), "status"),
            Some("APPROVED")
        );
    }

    /// The install window is mandatory and the upper bound optional, so an
    /// open-ended "everything since" query must not send an empty
    /// `installedBefore`.
    #[test]
    fn the_install_window_always_sends_after_and_before_only_when_bounded() {
        let open = install_query(None, 1_700_000_000, None);
        assert_eq!(keys(&open), vec!["installedAfter"]);
        assert_eq!(value(&open, "installedAfter"), Some("1700000000"));

        let bounded = install_query(None, 1_700_000_000, Some(1_800_000_000));
        assert_eq!(keys(&bounded), vec!["installedAfter", "installedBefore"]);
        assert_eq!(value(&bounded, "installedBefore"), Some("1800000000"));
    }

    /// Status pushdown is what keeps a failure-dashboard query from downloading
    /// the window's successful installs just to drop them. `run_query` passes a
    /// status only when exactly one install status is requested; with both, it must
    /// be absent or the other kind of record never arrives. The `df` is not this
    /// builder's: the slicer adds it, whole or narrowed per slice.
    #[test]
    fn the_install_history_pushes_a_single_status_and_omits_it_otherwise() {
        let failed = install_query(Some("FAILED"), 1, None);
        assert_eq!(keys(&failed), vec!["status", "installedAfter"]);
        assert_eq!(value(&failed, "status"), Some("FAILED"));

        let both = install_query(None, 1, None);
        assert!(
            !keys(&both).contains(&"status"),
            "requesting INSTALLED *and* FAILED must not narrow the server-side status"
        );
    }
}
