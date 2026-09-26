use anyhow::Result;

use super::paging::DEFAULT_PAGE_SIZE;
use super::{NinjaApiClient, ProgressFn, df_query};
use crate::model::Device;

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
}
