use anyhow::Result;

use super::NinjaApiClient;
use crate::model::{DeviceGroup, Location, Organization, Role};

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

    /// Device groups (saved searches). Like `/roles`, the spec declares no
    /// parameters, so it is one unpaginated GET.
    pub async fn device_groups(&self) -> Result<Vec<DeviceGroup>> {
        self.get_json("/groups", &[]).await
    }

    /// The ids of the devices currently matching group `id`'s criteria.
    pub async fn device_group_members(&self, id: i64) -> Result<Vec<i64>> {
        self.get_json(&format!("/group/{id}/device-ids"), &[]).await
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

    /// Both group endpoints answer with a bare array in one response: the groups
    /// as records, a group's members as bare device ids.
    #[tokio::test]
    async fn device_groups_and_their_members_are_single_requests() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "id": 7,
                    "name": "Patch ring 1",
                    "description": "Pilot servers",
                    "shared": true,
                    "deviceCount": 2,
                },
                { "id": 8 },
            ])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/group/7/device-ids"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([101, 102])))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let auth = AuthState::seeded(http.clone(), server.uri(), "test-token");
        let api = NinjaApiClient::new(http, auth);
        let groups = api.device_groups().await.expect("groups");
        let names: Vec<(i64, &str)> = groups.iter().map(|g| (g.id, g.name.as_str())).collect();
        assert_eq!(names, [(7, "Patch ring 1"), (8, "")]);
        assert_eq!(
            api.device_group_members(7).await.expect("members"),
            [101, 102]
        );
    }
}
