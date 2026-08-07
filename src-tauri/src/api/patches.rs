use anyhow::Result;

use super::{NinjaApiClient, ProgressFn, REPORTING_PAGE_SIZE};
use crate::model::Patch;

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
            "/queries/software-patches",
            &patch_query(df, status),
            REPORTING_PAGE_SIZE,
            on_progress,
        )
        .await
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

fn df_query(df: Option<&str>) -> Vec<(&'static str, String)> {
    match df {
        Some(f) if !f.is_empty() => vec![("df", f.to_string())],
        _ => Vec::new(),
    }
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
