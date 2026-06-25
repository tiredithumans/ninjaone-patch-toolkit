use anyhow::Result;

use super::{NinjaApiClient, ProgressFn};
use crate::model::Patch;

impl NinjaApiClient {
    /// Current OS patches (PENDING/APPROVED/REJECTED/FAILED) across the filtered
    /// fleet. `status` narrows to a single NinjaOne status when supplied.
    pub async fn fleet_os_patches(
        &self,
        df: Option<&str>,
        status: Option<&str>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        self.get_paginated_reporting("/queries/os-patches", &patch_query(df, status), on_progress)
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
            on_progress,
        )
        .await
    }

    /// Installed-OS-patch history within a time window (Unix seconds).
    pub async fn fleet_os_patch_installs(
        &self,
        df: Option<&str>,
        installed_after: i64,
        installed_before: Option<i64>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        let query = install_query(df, installed_after, installed_before);
        self.get_paginated_reporting("/queries/os-patch-installs", &query, on_progress)
            .await
    }

    /// Installed-software-patch history within a time window (Unix seconds).
    pub async fn fleet_software_patch_installs(
        &self,
        df: Option<&str>,
        installed_after: i64,
        installed_before: Option<i64>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Patch>> {
        let query = install_query(df, installed_after, installed_before);
        self.get_paginated_reporting("/queries/software-patch-installs", &query, on_progress)
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
    installed_after: i64,
    installed_before: Option<i64>,
) -> Vec<(&'static str, String)> {
    let mut query = df_query(df);
    query.push(("installedAfter", installed_after.to_string()));
    if let Some(before) = installed_before {
        query.push(("installedBefore", before.to_string()));
    }
    query
}
