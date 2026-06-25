use anyhow::Result;

use super::{DEFAULT_PAGE_SIZE, NinjaApiClient, ProgressFn};
use crate::model::Device;

impl NinjaApiClient {
    /// Fetches devices that match the optional `df` (device filter) DSL string,
    /// reporting cumulative progress to `on_progress` for the UI.
    pub async fn devices(
        &self,
        df: Option<&str>,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<Device>> {
        let query: Vec<(&str, String)> = match df {
            Some(f) if !f.is_empty() => vec![("df", f.to_string())],
            _ => Vec::new(),
        };
        self.get_paginated_reporting("/devices-detailed", &query, DEFAULT_PAGE_SIZE, on_progress)
            .await
    }
}
