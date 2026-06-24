use anyhow::Result;

use super::NinjaApiClient;
use crate::model::Device;

impl NinjaApiClient {
    /// Fetches devices that match the optional `df` (device filter) DSL string.
    pub async fn devices(&self, df: Option<&str>) -> Result<Vec<Device>> {
        let query: Vec<(&str, String)> = match df {
            Some(f) if !f.is_empty() => vec![("df", f.to_string())],
            _ => Vec::new(),
        };
        self.get_paginated("/devices-detailed", &query).await
    }
}
