use anyhow::Result;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use tracing::{Instrument, info, info_span, warn};

use super::paging::REPORTING_PAGE_SIZE;
use super::{NinjaApiClient, ProgressFn, df_query};
use crate::model::{Device, Patch};

const SOFTWARE_PATCHES: &str = "/queries/software-patches";

/// How many location slices the whole-fleet third-party feed is split into, all
/// paged at once.
///
/// One cursor scan of a 563,255-row feed is 113 pages in a row, and every page is a
/// chance to lose one: on 2026-10-26 a page went missing every 19–85 pages, and each
/// loss cost the scan all it had paged. Four slices cut a scan to about 28 pages and
/// the wait to about the largest slice's, at four requests in flight — fewer than
/// the seven legs a cold query already sends at once.
const SOFTWARE_SLICES: usize = 4;

/// The most locations one slice names. Each id adds a few characters to the URL,
/// and a request line past a proxy's limit (8 KB is common) fails outright, so a
/// tenant with more locations than the slices can name pages the feed whole.
const MAX_SLICE_LOCATIONS: usize = 200;

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

    /// Current third-party software patches across the filtered fleet.
    pub async fn fleet_software_patches(
        &self,
        df: Option<&str>,
        status: Option<&str>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        self.get_paginated_reporting(
            SOFTWARE_PATCHES,
            &patch_query(df, status),
            REPORTING_PAGE_SIZE,
            on_progress,
        )
        .await
    }

    /// Every current third-party patch in the fleet, paged as concurrent location
    /// slices of `devices` (see [`plan_location_slices`]) rather than one long scan.
    ///
    /// The slices replace the whole feed only if they provably add up to it. Before
    /// any paging, a one-row page per slice and one for the whole feed read their
    /// sizes, and the slices' must sum to exactly the whole's. That catches a `df`
    /// NinjaOne ignores (an unhonored clause is dropped silently, so every slice would
    /// be the whole feed) and rows on devices no slice names (the inventory's
    /// locations do not cover them) before either costs a download. The paged rows
    /// must then add up to the same total, which catches a device that moved between
    /// slices mid-fetch. Anything else — no plan, a size that cannot be read, a sum
    /// that does not match — pages the feed whole, as before.
    pub async fn fleet_software_patches_sliced(
        &self,
        devices: &[Device],
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        let Some(slices) = plan_location_slices(devices) else {
            return self.fleet_software_patches(None, None, on_progress).await;
        };
        let started = std::time::Instant::now();

        // The whole feed first, then each slice.
        let queries: Vec<_> = std::iter::once(df_query(None))
            .chain(slices.iter().map(|df| df_query(Some(df))))
            .collect();
        let sizes = try_join_all(
            queries
                .iter()
                .map(|query| self.feed_size(SOFTWARE_PATCHES, query))
                .collect(),
        )
        .await;
        let total = match sizes {
            Ok(sizes) => {
                let (whole, parts) = (sizes[0], &sizes[1..]);
                let sliced = parts.iter().copied().sum::<Option<usize>>();
                match whole {
                    Some(whole) if sliced == Some(whole) => Some(whole),
                    _ => {
                        warn!(
                            path = SOFTWARE_PATCHES,
                            ?whole,
                            ?parts,
                            "the location slices do not add up to the whole feed; paging it whole"
                        );
                        None
                    }
                }
            }
            Err(err) => {
                warn!(
                    path = SOFTWARE_PATCHES,
                    error = format!("{err:#}").as_str(),
                    "could not size the location slices; paging the feed whole"
                );
                None
            }
        };
        let Some(total) = total else {
            return self.fleet_software_patches(None, None, on_progress).await;
        };

        // Each slice restarts its own progress from zero, so the sum is what the
        // operator sees.
        let loaded: Vec<AtomicUsize> = slices.iter().map(|_| AtomicUsize::new(0)).collect();
        let report = |slice: usize, rows: usize| {
            loaded[slice].store(rows, Ordering::Relaxed);
            if let Some(report) = on_progress {
                report(loaded.iter().map(|n| n.load(Ordering::Relaxed)).sum());
            }
        };
        let report = &report;
        let reporters: Vec<_> = (0..slices.len())
            .map(|slice| move |rows: usize| report(slice, rows))
            .collect();
        let pages = try_join_all(
            slices
                .iter()
                .zip(&reporters)
                .enumerate()
                .map(|(n, (df, reporter))| {
                    // Every log line a slice's scan writes carries which slice it is.
                    self.fleet_software_patches(Some(df), None, Some(reporter as &ProgressFn))
                        .instrument(info_span!("slice", n = n + 1, of = slices.len()))
                })
                .collect(),
        )
        .await?;

        let mut rows = Vec::with_capacity(total);
        for page in pages {
            rows.extend(page);
        }
        if rows.len() != total {
            warn!(
                path = SOFTWARE_PATCHES,
                rows = rows.len(),
                total,
                "the location slices paged a different total than the whole feed holds; paging it whole"
            );
            return self.fleet_software_patches(None, None, on_progress).await;
        }
        info!(
            path = SOFTWARE_PATCHES,
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
    pub async fn fleet_os_patch_installs(
        &self,
        df: Option<&str>,
        status: Option<&str>,
        installed_after: i64,
        installed_before: Option<i64>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        let query = install_query(df, status, installed_after, installed_before);
        self.get_paginated_reporting(
            "/queries/os-patch-installs",
            &query,
            REPORTING_PAGE_SIZE,
            on_progress,
        )
        .await
    }

    /// Software-patch install history within a time window (Unix seconds). `status`
    /// narrows to a single install status server-side — see `fleet_os_patch_installs`.
    pub async fn fleet_software_patch_installs(
        &self,
        df: Option<&str>,
        status: Option<&str>,
        installed_after: i64,
        installed_before: Option<i64>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        let query = install_query(df, status, installed_after, installed_before);
        self.get_paginated_reporting(
            "/queries/software-patch-installs",
            &query,
            REPORTING_PAGE_SIZE,
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

/// Splits the fleet into up to [`SOFTWARE_SLICES`] `df` clauses by location,
/// balanced by device count, or `None` when it cannot be split in two: fewer than
/// two locations, a device with none (no clause could name it), or a slice that
/// would name more than [`MAX_SLICE_LOCATIONS`].
///
/// Location rather than organization because it is the finer of the two facets the
/// `/queries/*` endpoints honor in `df`: a fleet in one organization still splits.
pub fn plan_location_slices(devices: &[Device]) -> Option<Vec<String>> {
    let mut per_location: HashMap<i64, usize> = HashMap::new();
    for device in devices {
        *per_location.entry(device.location_id?).or_default() += 1;
    }
    if per_location.len() < 2 {
        return None;
    }
    let mut locations: Vec<(i64, usize)> = per_location.into_iter().collect();
    // Largest first, each onto the lightest slice so far: close to even unless one
    // location dominates, and that one then gets a slice to itself.
    locations.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut slices: Vec<(usize, Vec<i64>)> =
        vec![(0, Vec::new()); SOFTWARE_SLICES.min(locations.len())];
    for (location, count) in locations {
        let lightest = (0..slices.len())
            .min_by_key(|&slice| slices[slice].0)
            .unwrap_or(0);
        slices[lightest].0 += count;
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

fn install_query(
    df: Option<&str>,
    status: Option<&str>,
    installed_after: i64,
    installed_before: Option<i64>,
) -> Vec<(&'static str, String)> {
    let mut query = df_query(df);
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

    /// Largest location first, each onto the lightest slice: the six locations land
    /// as 5 / 4 / 4 / 3 devices rather than in the order they were listed.
    #[test]
    fn the_feed_is_sliced_by_location_and_balanced_by_device_count() {
        let devices = fleet(&[(6, 1), (4, 2), (1, 5), (5, 1), (3, 3), (2, 4)]);
        assert_eq!(
            plan_location_slices(&devices).expect("a plan"),
            ["loc=1", "loc=2", "loc in (3, 6)", "loc in (4, 5)"]
        );
    }

    #[test]
    fn fewer_locations_than_slices_get_one_slice_each() {
        let devices = fleet(&[(7, 3), (8, 1)]);
        assert_eq!(
            plan_location_slices(&devices).expect("a plan"),
            ["loc=7", "loc=8"]
        );
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
        assert_eq!(plan_location_slices(&fleet(&[(1, 9)])), None);
        assert_eq!(plan_location_slices(&[]), None);

        let mut unplaced = fleet(&[(1, 2), (2, 2)]);
        unplaced[0].location_id = None;
        assert_eq!(plan_location_slices(&unplaced), None);

        let most = SOFTWARE_SLICES * MAX_SLICE_LOCATIONS;
        let locations = |n: usize| (1..=n as i64).map(|l| (l, 1)).collect::<Vec<_>>();
        assert!(plan_location_slices(&fleet(&locations(most))).is_some());
        assert_eq!(plan_location_slices(&fleet(&locations(most + 1))), None);
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
        let open = install_query(None, None, 1_700_000_000, None);
        assert_eq!(keys(&open), vec!["installedAfter"]);
        assert_eq!(value(&open, "installedAfter"), Some("1700000000"));

        let bounded = install_query(None, None, 1_700_000_000, Some(1_800_000_000));
        assert_eq!(keys(&bounded), vec!["installedAfter", "installedBefore"]);
        assert_eq!(value(&bounded, "installedBefore"), Some("1800000000"));
    }

    /// Status pushdown is what keeps a failure-dashboard query from downloading
    /// the window's successful installs just to drop them. `run_query` passes a
    /// status only when exactly one install status is requested; with both, it must
    /// be absent or the other kind of record never arrives.
    #[test]
    fn the_install_history_pushes_a_single_status_and_omits_it_otherwise() {
        let failed = install_query(Some("org = 1"), Some("FAILED"), 1, None);
        assert_eq!(keys(&failed), vec!["df", "status", "installedAfter"]);
        assert_eq!(value(&failed, "status"), Some("FAILED"));

        let both = install_query(Some("org = 1"), None, 1, None);
        assert!(
            !keys(&both).contains(&"status"),
            "requesting INSTALLED *and* FAILED must not narrow the server-side status"
        );
    }
}
