use anyhow::Result;

use super::NinjaApiClient;
use crate::model::{Location, Organization, Role};

impl NinjaApiClient {
    pub async fn organizations(&self) -> Result<Vec<Organization>> {
        self.get_paginated("/organizations", &[]).await
    }

    /// All locations across every organization (each location carries its
    /// `organizationId`), used to resolve names without N per-org round trips.
    pub async fn all_locations(&self) -> Result<Vec<Location>> {
        self.get_paginated("/locations", &[]).await
    }

    /// Device roles. One unpaginated GET: the spec declares no parameters for
    /// `/roles` (see `docs/api/ninjaone-surface.md`), so it answers with the whole
    /// list. Driving it through `get_paginated` sent an `after`/`pageSize` the server
    /// ignores, and a tenant with at least a page's worth of roles would get the same
    /// page back for the `after` request — a stall, which is an error.
    pub async fn roles(&self) -> Result<Vec<Role>> {
        self.get_json("/roles", &[]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthState;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// `/roles` takes no paging parameters, so it answers with every role at once.
    /// Paged with `after`, a list of at least one page came back unchanged for the
    /// second request and read as a stalled scan.
    #[tokio::test]
    async fn roles_are_one_unpaginated_request() {
        let server = MockServer::start().await;
        let roles: Vec<_> = (1..=600)
            .map(|id| json!({ "id": id, "name": format!("role-{id}") }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/v2/roles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(roles))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
        let roles = NinjaApiClient::new(http, auth)
            .roles()
            .await
            .expect("a whole list in one response is complete");
        assert_eq!(roles.len(), 600);

        let requests = server.received_requests().await.unwrap_or_default();
        assert!(
            requests.iter().all(|r| r.url.query().is_none()),
            "no paging parameters are sent to /roles"
        );
    }
}
