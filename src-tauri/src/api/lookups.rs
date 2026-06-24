use anyhow::Result;

use super::NinjaApiClient;
use crate::model::{Location, Organization, Role};

impl NinjaApiClient {
    pub async fn organizations(&self) -> Result<Vec<Organization>> {
        self.get_paginated("/organizations", &[]).await
    }

    pub async fn locations(&self, org_id: i64) -> Result<Vec<Location>> {
        let path = format!("/organization/{org_id}/locations");
        self.get_paginated(&path, &[]).await
    }

    /// All locations across every organization (each location carries its
    /// `organizationId`), used to resolve names without N per-org round trips.
    pub async fn all_locations(&self) -> Result<Vec<Location>> {
        self.get_paginated("/locations", &[]).await
    }

    pub async fn roles(&self) -> Result<Vec<Role>> {
        self.get_paginated("/roles", &[]).await
    }
}
