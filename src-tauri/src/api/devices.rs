use anyhow::Result;

use super::paging::{DEFAULT_PAGE_SIZE, REPORTING_PAGE_SIZE};
use super::{NinjaApiClient, ProgressFn, df_query};
use crate::model::{Device, DeviceHealth};

impl NinjaApiClient {
    /// Fetches devices that match the optional `df` (device filter) DSL string,
    /// reporting cumulative progress to `on_progress` for the UI.
    pub async fn devices(
        &self,
        df: Option<&str>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Device>> {
        self.get_paginated_reporting(
            "/devices-detailed",
            &df_query(df),
            DEFAULT_PAGE_SIZE,
            on_progress,
        )
        .await
    }

    /// Whole-fleet `/queries/device-health`, read only for each device's
    /// `pendingRebootReason`. Cursor-paged like the other `/queries/*` feeds.
    pub async fn device_health(&self) -> Result<Vec<DeviceHealth>> {
        self.get_paginated_reporting("/queries/device-health", &[], REPORTING_PAGE_SIZE, None)
            .await
    }
}
