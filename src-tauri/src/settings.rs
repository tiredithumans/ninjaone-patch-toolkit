use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::filter::FilterParams;

/// Default NinjaOne instance. Operators change this to their region in Settings.
pub const DEFAULT_BASE_URL: &str = "https://us2.ninjarmm.com";
pub const DEFAULT_CALLBACK_PORT: u16 = 11434;
pub const DEFAULT_INSTALL_WINDOW_DAYS: i64 = 30;
pub const DEFAULT_SLA_DAYS: i64 = 30;

/// A named, reusable filter combination. The device/OS/search/severity facets live
/// in `filter`; the patch-query selectors (type/status/install window) are stored
/// alongside so a preset restores the whole query. The selectors are optional for
/// backward compatibility — a preset saved before this field existed leaves the
/// current Type/Status/install-window untouched when applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub filter: FilterParams,
    #[serde(default)]
    pub patch_type: Option<String>,
    #[serde(default)]
    pub statuses: Option<Vec<String>>,
    #[serde(default)]
    pub install_days: Option<i64>,
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
        Self::load_from(&settings_path()?)
    }

    /// Reads settings from an explicit path — the seam `load` and the tests share.
    /// A missing file yields the defaults (first run); a present-but-unparseable
    /// file is an error so a corrupted config surfaces loudly rather than silently
    /// resetting the operator's instance/client configuration.
    fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path).context("read settings")?;
        let cfg: Settings = serde_json::from_str(&text).context("parse settings")?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&settings_path()?)
    }

    fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create settings dir")?;
        }
        let text = serde_json::to_string_pretty(self).context("serialize settings")?;
        fs::write(path, text).context("write settings")?;
        Ok(())
    }
}

fn settings_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("io.github", "tiredithumans", "NinjaOnePatchToolkit")
        .context("locate project config dir")?;
    Ok(dirs.config_dir().join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A distinct temp path per test (tests run in parallel) that doesn't exist yet.
    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("npt-settings-{}-{tag}.json", std::process::id()))
    }

    #[test]
    fn round_trips_non_default_settings_through_disk() {
        let path = temp_path("roundtrip");
        let _ = fs::remove_file(&path);
        let original = Settings {
            instance_base_url: "https://eu.ninjarmm.com".into(),
            client_id: Some("client-abc".into()),
            callback_port: 12000,
            install_window_days: 14,
            sla_days: 7,
            presets: vec![Preset {
                name: "Servers".into(),
                filter: FilterParams::default(),
                patch_type: Some("OS".into()),
                statuses: Some(vec!["PENDING".into()]),
                install_days: Some(45),
            }],
            auto_check_updates: false,
        };

        original.save_to(&path).expect("save");
        let loaded = Settings::load_from(&path).expect("load");

        assert_eq!(loaded.instance_base_url, "https://eu.ninjarmm.com");
        assert_eq!(loaded.client_id.as_deref(), Some("client-abc"));
        assert_eq!(loaded.callback_port, 12000);
        assert_eq!(loaded.install_window_days, 14);
        assert_eq!(loaded.sla_days, 7);
        assert!(!loaded.auto_check_updates);
        assert_eq!(loaded.presets.len(), 1);
        assert_eq!(loaded.presets[0].name, "Servers");
        assert_eq!(loaded.presets[0].install_days, Some(45));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let path = temp_path("missing");
        let _ = fs::remove_file(&path);
        let loaded = Settings::load_from(&path).expect("load missing");
        assert_eq!(loaded.instance_base_url, DEFAULT_BASE_URL);
        assert_eq!(loaded.callback_port, DEFAULT_CALLBACK_PORT);
        assert!(loaded.auto_check_updates);
        assert!(loaded.presets.is_empty());
    }

    #[test]
    fn malformed_json_is_an_error_not_a_silent_reset() {
        let path = temp_path("malformed");
        fs::write(&path, "{ this is not valid json ").expect("write");
        let result = Settings::load_from(&path);
        assert!(
            result.is_err(),
            "a corrupted config must surface as an error"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn older_files_without_new_fields_fall_back_to_defaults() {
        // A settings file written before `presets`/`autoCheckUpdates` existed.
        let path = temp_path("legacy");
        fs::write(
            &path,
            r#"{
                "instanceBaseUrl": "https://us2.ninjarmm.com",
                "callbackPort": 11434,
                "installWindowDays": 30,
                "slaDays": 30
            }"#,
        )
        .expect("write");
        let loaded = Settings::load_from(&path).expect("load legacy");
        assert!(loaded.presets.is_empty());
        assert!(
            loaded.auto_check_updates,
            "a missing autoCheckUpdates defaults to enabled"
        );
        let _ = fs::remove_file(&path);
    }
}
