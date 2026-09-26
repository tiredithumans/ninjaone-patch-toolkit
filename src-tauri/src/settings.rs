use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use crate::filter::FilterParams;

/// Default NinjaOne instance. Operators change this to their region in Settings.
pub const DEFAULT_BASE_URL: &str = "https://us2.ninjarmm.com";
pub const DEFAULT_CALLBACK_PORT: u16 = 11434;
pub const DEFAULT_INSTALL_WINDOW_DAYS: i64 = 30;
pub const DEFAULT_SLA_DAYS: i64 = 30;
/// Upper bound for every operator-supplied day window (install lookback, SLA, the
/// per-query first-seen window), matching the `max="3650"` on the frontend inputs.
///
/// This is a panic guard, not just ergonomics: these values reach
/// `chrono::Duration::days`, which panics when the day count overflows its
/// millisecond representation. A hand-edited `settings.json` or a stale frontend
/// could otherwise hand a command `i64::MAX` and take the process down.
pub const MAX_WINDOW_DAYS: i64 = 3650;

/// Whether `host` (as `url::Url::host_str` spells it) is the local machine — the
/// one place a plaintext `http://` instance is allowed, for a mock server. Shared
/// by the load-time upgrade and the save-time check so the two cannot disagree.
pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

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

pub const DEFAULT_ACTION_CONCURRENCY: usize = 8;
pub const MAX_ACTION_CONCURRENCY: usize = 16;
pub const DEFAULT_MAX_DEVICES_PER_ACTION: usize = 25;
pub const MAX_DEVICES_PER_ACTION_CEILING: usize = 500;
/// NinjaOne's default run-as identity for a script.
pub const DEFAULT_RUN_AS: &str = "system";

/// Write-path configuration.
///
/// Every field defaults off/empty, and `enabled` gates the rest: an install that
/// never opens this panel keeps requesting the read-only OAuth scope and can't
/// dispatch anything. That is what makes adding the write path a non-breaking
/// change for existing deployments — see `auth::scope_for`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ActionSettings {
    /// Master switch. Also decides whether sign-in asks for the `management` scope.
    pub enabled: bool,
    /// Library script id used for KB-targeted OS remediation. NinjaOne has no
    /// script-upload API, so the script is added to the library by hand and its
    /// numeric id copied out of the Admin → Library → Automation URL.
    pub os_patch_script_id: Option<i64>,
    pub software_patch_script_id: Option<i64>,
    /// NinjaOne run-as identity: `system`, `loggedonuser`, or a credential role.
    pub run_as: String,
    pub concurrency: usize,
    /// Blast-radius cap. A *blocker*, not a warning — raising it is a deliberate
    /// edit here rather than a click in a confirmation dialog.
    pub max_devices_per_action: usize,
    /// How many distinct organizations one action may span. Defaults to 1 because a
    /// cross-tenant mistake is the highest-consequence error available here.
    pub max_orgs_per_action: usize,
    /// NinjaOne *queues* work for an offline device, so an action dispatched now can
    /// reboot it hours later when it comes back. Off by default.
    pub allow_offline_targets: bool,
    pub require_maintenance_window: bool,
    /// Days the window is open, `0` = Sunday, matching `chrono::Weekday::num_days_from_sunday`.
    pub window_days: Vec<u8>,
    /// Window bounds as minutes past local midnight. A start later than the end
    /// means the window wraps past midnight.
    pub window_start_minute: u16,
    pub window_end_minute: u16,
    pub allow_window_override: bool,
}

impl Default for ActionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            os_patch_script_id: None,
            software_patch_script_id: None,
            run_as: DEFAULT_RUN_AS.to_string(),
            concurrency: DEFAULT_ACTION_CONCURRENCY,
            max_devices_per_action: DEFAULT_MAX_DEVICES_PER_ACTION,
            max_orgs_per_action: 1,
            allow_offline_targets: false,
            require_maintenance_window: false,
            // Mon–Fri 02:00–05:00 local, used only once the window is switched on.
            window_days: vec![1, 2, 3, 4, 5],
            window_start_minute: 2 * 60,
            window_end_minute: 5 * 60,
            allow_window_override: false,
        }
    }
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
    /// Write-path configuration. Absent from every settings file written before the
    /// actions feature existed, so it defaults to fully disabled.
    #[serde(default)]
    pub actions: ActionSettings,
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
            actions: ActionSettings::default(),
        }
    }
}

impl Settings {
    /// Loads `settings.json`, falling back to the defaults — loudly — when it cannot.
    ///
    /// This used to be `load().unwrap_or_default()` with nothing logged, so a corrupt
    /// file silently became the defaults and the next save of *any* setting wrote
    /// them over the operator's real configuration, destroying the one copy that
    /// could have been repaired. An unparseable file is now moved aside to
    /// `settings.json.corrupt-<UTC timestamp>` first, so it survives for inspection
    /// and the fresh defaults never overwrite it.
    pub fn load_or_recover() -> Self {
        match settings_path() {
            Ok(path) => Self::load_or_quarantine(&path, chrono::Utc::now()),
            Err(e) => {
                tracing::warn!(error = %e, "could not locate settings.json; using default settings");
                Self::default()
            }
        }
    }

    fn load_or_quarantine(path: &Path, now: chrono::DateTime<chrono::Utc>) -> Self {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            // Unreadable is not corrupt: the file may be fine and merely locked or
            // permission-denied, so it is left exactly where it is.
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "could not read settings.json; using default settings for this session"
                );
                return Self::default();
            }
        };
        match Self::parse(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                let aside = quarantine_path(path, now);
                match fs::rename(path, &aside) {
                    Ok(()) => tracing::warn!(
                        error = %e,
                        moved_to = %aside.display(),
                        "settings.json could not be parsed; it was moved aside and default settings are in use"
                    ),
                    Err(re) => tracing::warn!(
                        error = %e,
                        rename_error = %re,
                        "settings.json could not be parsed and could not be moved aside; default settings are in use"
                    ),
                }
                Self::default()
            }
        }
    }

    /// Reads settings from an explicit path — the seam the tests use.
    /// A missing file yields the defaults (first run); a present-but-unparseable
    /// file is an error so a corrupted config surfaces loudly rather than silently
    /// resetting the operator's instance/client configuration.
    #[cfg(test)]
    fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path).context("read settings")?;
        Self::parse(&text)
    }

    fn parse(text: &str) -> Result<Self> {
        let mut cfg: Settings = serde_json::from_str(text).context("parse settings")?;
        cfg.enforce_https_instance();
        Ok(cfg)
    }

    /// Upgrades a non-loopback `http://` instance URL to `https://`.
    ///
    /// `save_settings` refuses plaintext at the IPC boundary, but nothing enforced it
    /// on the *load* path — and `settings.json` is a plain file in the config
    /// directory. A hand-edited or downgrade-written `http://` host therefore
    /// survived a restart and every token request, refresh grant and API call for
    /// that session went out in cleartext, carrying the bearer token and, on the
    /// token endpoint, the client secret.
    ///
    /// Upgrading rather than rejecting keeps the operator's configured host: the
    /// alternative is resetting them to the default instance, which loses the very
    /// setting they are most likely to have deliberately changed. Loopback is left
    /// alone — it is what a local mock server uses, and `require_https_instance`
    /// permits it for the same reason.
    fn enforce_https_instance(&mut self) {
        let Ok(parsed) = url::Url::parse(&self.instance_base_url) else {
            return;
        };
        if parsed.scheme() != "http" {
            return;
        }
        if is_loopback_host(parsed.host_str().unwrap_or_default()) {
            return;
        }
        let upgraded = self.instance_base_url.replacen("http://", "https://", 1);
        tracing::warn!(
            from = %self.instance_base_url,
            to = %upgraded,
            "settings.json specified a plaintext instance URL; upgrading to https so credentials are not sent in the clear"
        );
        self.instance_base_url = upgraded;
    }

    /// Writes `settings.json`. **Blocking** file I/O — async callers run it on a
    /// blocking thread.
    pub fn save(&self) -> Result<()> {
        self.save_to(&settings_path()?)
    }

    /// Atomic: the new contents go to a temporary file beside the real one and are
    /// renamed over it. A plain `fs::write` truncates first, so a crash, a full disk
    /// or a power cut mid-write left a half-written file — which the next launch
    /// could not parse.
    fn save_to(&self, path: &Path) -> Result<()> {
        let dir = path
            .parent()
            .context("settings path has no parent directory")?;
        fs::create_dir_all(dir).context("create settings dir")?;
        let text = serde_json::to_string_pretty(self).context("serialize settings")?;
        let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
        let written = write_owner_only(&tmp, text.as_bytes())
            .and_then(|()| fs::rename(&tmp, path).context("replace settings"));
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
    }
}

/// Creates (or truncates) `path` readable by the owner only, like the audit log and
/// run history, writes `bytes` and syncs them to disk before the caller renames it
/// into place.
fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).context("create temporary settings file")?;
    file.write_all(bytes).context("write settings")?;
    file.sync_all().context("flush settings")?;
    Ok(())
}

/// Where an unparseable `settings.json` is moved: beside it, stamped so a second
/// corruption never overwrites the first one's evidence.
fn quarantine_path(path: &Path, now: chrono::DateTime<chrono::Utc>) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".corrupt-{}", now.format("%Y%m%dT%H%M%SZ")));
    path.with_file_name(name)
}

fn settings_path() -> Result<PathBuf> {
    Ok(crate::paths::app_dir()
        .context("locate project config dir")?
        .join("settings.json"))
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
            actions: ActionSettings {
                enabled: true,
                os_patch_script_id: Some(123),
                max_devices_per_action: 10,
                ..ActionSettings::default()
            },
        };

        original.save_to(&path).expect("save");
        let loaded = Settings::load_from(&path).expect("load");

        assert_eq!(loaded.instance_base_url, "https://eu.ninjarmm.com");
        assert_eq!(loaded.client_id.as_deref(), Some("client-abc"));
        assert_eq!(loaded.callback_port, 12000);
        assert_eq!(loaded.install_window_days, 14);
        assert_eq!(loaded.sla_days, 7);
        assert!(!loaded.auto_check_updates);
        assert!(loaded.actions.enabled);
        assert_eq!(loaded.actions.os_patch_script_id, Some(123));
        assert_eq!(loaded.actions.max_devices_per_action, 10);
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
        // The load-bearing migration guarantee: an install that predates patch
        // actions stays read-only. If this ever flips, every existing deployment
        // would start requesting the `management` scope without being asked.
        assert!(
            !loaded.actions.enabled,
            "a settings file with no `actions` block must not enable the write path"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_partial_actions_block_keeps_the_remaining_guardrails() {
        // A file that enables actions but predates the newer guardrail fields must
        // still get their safe defaults rather than zero/empty.
        let path = temp_path("partial-actions");
        fs::write(
            &path,
            r#"{
                "instanceBaseUrl": "https://us2.ninjarmm.com",
                "callbackPort": 11434,
                "installWindowDays": 30,
                "slaDays": 30,
                "actions": { "enabled": true }
            }"#,
        )
        .expect("write");
        let loaded = Settings::load_from(&path).expect("load partial");

        assert!(loaded.actions.enabled);
        assert_eq!(
            loaded.actions.max_devices_per_action,
            DEFAULT_MAX_DEVICES_PER_ACTION
        );
        assert_eq!(loaded.actions.concurrency, DEFAULT_ACTION_CONCURRENCY);
        assert_eq!(loaded.actions.max_orgs_per_action, 1);
        assert_eq!(loaded.actions.run_as, DEFAULT_RUN_AS);
        assert!(!loaded.actions.allow_offline_targets);
        let _ = fs::remove_file(&path);
    }
    /// `save_settings` refuses plaintext at the IPC boundary, but settings.json is a
    /// plain file in the config directory: a hand-edited `http://` host used to
    /// survive a restart, and then every token request, refresh grant and API call
    /// for that session went out in the clear carrying the bearer token — and, on the
    /// token endpoint, the client secret.
    #[test]
    fn a_plaintext_instance_url_is_upgraded_on_load() {
        let mut cfg = Settings {
            instance_base_url: "http://app.ninjarmm.com".into(),
            ..Settings::default()
        };
        cfg.enforce_https_instance();
        assert_eq!(cfg.instance_base_url, "https://app.ninjarmm.com");
    }

    /// Loopback is what a local mock server uses, and `require_https_instance`
    /// permits it for the same reason — upgrading it would break that setup.
    #[test]
    fn a_loopback_instance_url_is_left_alone() {
        for url in [
            "http://127.0.0.1:8080",
            "http://localhost:3000",
            "https://app.ninjarmm.com",
        ] {
            let mut cfg = Settings {
                instance_base_url: url.into(),
                ..Settings::default()
            };
            cfg.enforce_https_instance();
            assert_eq!(cfg.instance_base_url, url, "{url} must be preserved");
        }
    }

    /// A corrupt file used to become the defaults silently, and the next save of
    /// any setting overwrote the operator's real configuration with them. It is
    /// now moved aside first, so the evidence survives and the save cannot touch it.
    #[test]
    fn a_corrupt_file_is_moved_aside_before_defaults_are_used() {
        let dir = std::env::temp_dir().join(format!("npt-settings-corrupt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("settings.json");
        fs::write(&path, "{ \"instanceBaseUrl\": ").expect("write");
        let now = chrono::DateTime::from_timestamp(1_790_000_000, 0).expect("ts");

        let loaded = Settings::load_or_quarantine(&path, now);

        assert_eq!(loaded.instance_base_url, DEFAULT_BASE_URL);
        assert!(
            !path.exists(),
            "the corrupt file must not stay where a save would overwrite it"
        );
        let aside = quarantine_path(&path, now);
        assert_eq!(
            aside.file_name().unwrap().to_string_lossy(),
            "settings.json.corrupt-20260921T141320Z"
        );
        assert_eq!(
            fs::read_to_string(&aside).expect("kept"),
            "{ \"instanceBaseUrl\": "
        );

        // A later save writes a fresh file and leaves the evidence alone.
        loaded.save_to(&path).expect("save");
        assert!(aside.exists());
        assert!(Settings::load_from(&path).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A missing file is a first run, not a corruption: nothing is moved.
    #[test]
    fn a_missing_file_is_not_quarantined() {
        let path = temp_path("quarantine-missing");
        let _ = fs::remove_file(&path);
        let loaded = Settings::load_or_quarantine(&path, chrono::Utc::now());
        assert_eq!(loaded.callback_port, DEFAULT_CALLBACK_PORT);
    }

    /// The save goes through a temporary file and a rename, leaves no temporary
    /// behind, and the result is owner-only like the audit log.
    #[test]
    fn saving_replaces_the_file_atomically_and_owner_only() {
        let dir = std::env::temp_dir().join(format!("npt-settings-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");

        Settings::default().save_to(&path).expect("first save");
        let edited = Settings {
            sla_days: 9,
            ..Settings::default()
        };
        edited.save_to(&path).expect("overwrite");

        assert_eq!(Settings::load_from(&path).expect("load").sla_days, 9);
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .expect("list")
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n != "settings.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "temporary files left behind: {leftovers:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
