use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

use crate::filter::FilterParams;

/// Default NinjaOne instance. Operators change this to their region in Settings.
pub const DEFAULT_BASE_URL: &str = "https://us2.ninjarmm.com";
pub const DEFAULT_CALLBACK_PORT: u16 = 11434;
pub const DEFAULT_INSTALL_WINDOW_DAYS: i64 = 30;
pub const DEFAULT_SLA_DAYS: i64 = 30;

/// A named, reusable filter combination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub filter: FilterParams,
}

fn default_true() -> bool {
    true
}

/// Non-secret app configuration persisted to `settings.json`. The client secret and
/// refresh token live in the OS keyring, never here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub instance_base_url: String,
    #[serde(default)]
    pub client_id: Option<String>,
    pub callback_port: u16,
    pub install_window_days: i64,
    pub sla_days: i64,
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// Whether to check GitHub for a newer release on launch. Defaults on; older
    /// settings files without the field are treated as enabled.
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            instance_base_url: DEFAULT_BASE_URL.to_string(),
            client_id: None,
            callback_port: DEFAULT_CALLBACK_PORT,
            install_window_days: DEFAULT_INSTALL_WINDOW_DAYS,
            sla_days: DEFAULT_SLA_DAYS,
            presets: Vec::new(),
            auto_check_updates: true,
        }
    }
}

impl Settings {
    pub fn load() -> Result<Self> {
        let path = settings_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path).context("read settings")?;
        let cfg: Settings = serde_json::from_str(&text).context("parse settings")?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create settings dir")?;
        }
        let text = serde_json::to_string_pretty(self).context("serialize settings")?;
        fs::write(&path, text).context("write settings")?;
        Ok(())
    }
}

fn settings_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("io.github", "tiredithumans", "NinjaOnePatchToolkit")
        .context("locate project config dir")?;
    Ok(dirs.config_dir().join("settings.json"))
}
