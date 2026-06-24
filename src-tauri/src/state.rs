use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::api::NinjaApiClient;
use crate::auth::AuthState;
use crate::model::{Location, Organization, Role};
use crate::rows::QueryResult;
use crate::settings::Settings;

/// How long cached org/location/role lookups stay fresh before a query refetches
/// them. They change rarely, so this spares repeat queries and every auto-refresh
/// tick from three extra round trips.
const LOOKUP_TTL: Duration = Duration::from_secs(300);

struct LookupCache {
    at: Instant,
    orgs: Vec<Organization>,
    locations: Vec<Location>,
    roles: Vec<Role>,
}

/// Process-wide application state injected into every Tauri command.
pub struct AppState {
    pub auth: AuthState,
    pub api: NinjaApiClient,
    /// Locked only for brief read/clone/replace — never held across `.await`.
    pub settings: Mutex<Settings>,
    /// Last query result, cached so the export command can write it without the
    /// frontend round-tripping all rows back over IPC.
    pub last_result: Mutex<Option<QueryResult>>,
    /// Near-static lookups (orgs/locations/roles) cached with a short TTL.
    lookups_cache: Mutex<Option<LookupCache>>,
}

impl AppState {
    pub fn new() -> Result<Self> {
        let settings = Settings::load().unwrap_or_default();

        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "ninjaone-patch-toolkit/",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(Duration::from_secs(45))
            .build()
            .context("build http client")?;

        let auth = AuthState::new(
            http.clone(),
            settings.instance_base_url.clone(),
            settings.callback_port,
            settings.client_id.clone(),
        );
        let api = NinjaApiClient::new(http, auth.clone());

        Ok(Self {
            auth,
            api,
            settings: Mutex::new(settings),
            last_result: Mutex::new(None),
            lookups_cache: Mutex::new(None),
        })
    }

    /// Snapshot of settings for use across `.await` points without holding the lock.
    pub fn settings_snapshot(&self) -> Settings {
        self.settings.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Orgs/locations/roles used to label patch rows, served from a short-TTL
    /// cache. Fetches the three concurrently on a miss. The lock is never held
    /// across the `.await`.
    pub async fn lookups(&self) -> Result<(Vec<Organization>, Vec<Location>, Vec<Role>)> {
        if let Ok(guard) = self.lookups_cache.lock()
            && let Some(c) = guard.as_ref()
            && c.at.elapsed() < LOOKUP_TTL
        {
            return Ok((c.orgs.clone(), c.locations.clone(), c.roles.clone()));
        }
        let (orgs, locations, roles) = tokio::try_join!(
            self.api.organizations(),
            async { Ok::<_, anyhow::Error>(self.api.all_locations().await.unwrap_or_default()) },
            self.api.roles(),
        )?;
        if let Ok(mut guard) = self.lookups_cache.lock() {
            *guard = Some(LookupCache {
                at: Instant::now(),
                orgs: orgs.clone(),
                locations: locations.clone(),
                roles: roles.clone(),
            });
        }
        Ok((orgs, locations, roles))
    }

    /// Drops cached lookups so a different tenant (after sign-out or an instance
    /// change) doesn't see stale org/location/role names.
    pub fn clear_lookups_cache(&self) {
        if let Ok(mut guard) = self.lookups_cache.lock() {
            *guard = None;
        }
    }
}
