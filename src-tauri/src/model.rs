use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub id: i64,
    pub name: String,
    #[serde(default, rename = "organizationId")]
    pub organization_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    pub id: i64,
    pub name: String,
    #[serde(default, rename = "nodeClass")]
    pub node_class: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "needsReboot")]
    pub needs_reboot: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: i64,
    #[serde(default)]
    pub system_name: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub organization_id: Option<i64>,
    #[serde(default)]
    pub location_id: Option<i64>,
    #[serde(default, alias = "roleId", alias = "role")]
    pub node_role_id: Option<i64>,
    #[serde(default)]
    pub node_class: Option<String>,
    #[serde(default)]
    pub offline: Option<bool>,
    #[serde(default)]
    pub os: Option<OsInfo>,
}

impl Device {
    pub fn label(&self) -> &str {
        self.display_name
            .as_deref()
            .or(self.system_name.as_deref())
            .unwrap_or("(unnamed)")
    }

    pub fn os_name(&self) -> Option<String> {
        self.os.as_ref().and_then(|o| o.name.clone())
    }

    pub fn needs_reboot(&self) -> bool {
        self.os
            .as_ref()
            .and_then(|o| o.needs_reboot)
            .unwrap_or(false)
    }

    pub fn is_offline(&self) -> bool {
        self.offline.unwrap_or(false)
    }
}

/// Severity buckets returned by NinjaOne's patch feeds.
///
/// Mostly MSRC-aligned, but NinjaOne mixes in two of its own classifications that
/// are **not** MSRC severities and needed their own variants rather than being
/// forced into one: `security` (the largest bucket on a real OS feed — it says the
/// patch is a security update but not how urgent) and `recommended` (the
/// non-critical tier of third-party patch approval). Both previously fell through
/// to [`Unknown`](Self::Unknown), which sank them to the bottom of the severity
/// sort and made them vanish whenever the severity facet was active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Severity {
    Critical,
    Important,
    Security,
    Moderate,
    Recommended,
    Low,
    Optional,
    Unknown,
}

impl Severity {
    /// Case-insensitive, because NinjaOne returns two vocabularies on the same
    /// field: uppercase MSRC values (`CRITICAL`, `IMPORTANT`, `OPTIONAL`, `NONE`)
    /// alongside lowercase engine values (`critical`, `security`, `optional`,
    /// `recommended`, `unknown`).
    pub fn from_raw(raw: &str) -> Self {
        match raw.to_ascii_uppercase().as_str() {
            "CRITICAL" => Self::Critical,
            "IMPORTANT" | "HIGH" => Self::Important,
            "SECURITY" => Self::Security,
            "MODERATE" | "MEDIUM" => Self::Moderate,
            "RECOMMENDED" => Self::Recommended,
            "LOW" => Self::Low,
            "OPTIONAL" | "NONE" => Self::Optional,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "Critical",
            Self::Important => "Important",
            Self::Security => "Security",
            Self::Moderate => "Moderate",
            Self::Recommended => "Recommended",
            Self::Low => "Low",
            Self::Optional => "Optional",
            Self::Unknown => "Unknown",
        }
    }

    /// Higher = more urgent. Drives the severity sort and the "Important or above"
    /// threshold the compliance/SLA rollups use (`rank() < Important.rank()`).
    ///
    /// `Security` and `Recommended` sit **below** `Important` deliberately: both are
    /// classifications rather than urgency grades, so an ungraded security update
    /// shouldn't silently enter the critical-backlog and SLA-aging figures. They
    /// still rank above `Unknown`, so they sort and filter as real severities.
    pub fn rank(self) -> u8 {
        match self {
            Self::Critical => 7,
            Self::Important => 6,
            Self::Security => 5,
            Self::Moderate => 4,
            Self::Recommended => 3,
            Self::Low => 2,
            Self::Optional => 1,
            Self::Unknown => 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    #[serde(default)]
    pub device_id: Option<i64>,
    #[serde(default)]
    pub kb_number: Option<String>,
    #[serde(
        default,
        alias = "productName",
        alias = "title",
        alias = "product",
        alias = "displayName"
    )]
    pub name: Option<String>,
    #[serde(default, alias = "productVersion", alias = "ver")]
    pub version: Option<String>,
    #[serde(default, alias = "vendor", alias = "publisher")]
    pub product_vendor: Option<String>,
    #[serde(default, alias = "impact", alias = "severityLevel", alias = "priority")]
    pub severity: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "type")]
    pub patch_type: Option<String>,
    #[serde(default, alias = "releaseDate", alias = "timestamp")]
    pub release_timestamp: Option<f64>,
    #[serde(default, alias = "installedAt")]
    pub installed_timestamp: Option<f64>,
}

impl Patch {
    pub fn severity_enum(&self) -> Severity {
        self.severity
            .as_deref()
            .map(Severity::from_raw)
            .unwrap_or(Severity::Unknown)
    }

    pub fn released_at(&self) -> Option<DateTime<Utc>> {
        self.release_timestamp.and_then(unix_to_datetime)
    }

    pub fn installed_at(&self) -> Option<DateTime<Utc>> {
        self.installed_timestamp.and_then(unix_to_datetime)
    }

    /// Human-friendly patch label combining KB, vendor, name and version.
    pub fn display_name(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for field in [
            self.kb_number.as_deref(),
            self.product_vendor.as_deref(),
            self.name.as_deref(),
            self.version.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !field.is_empty() {
                parts.push(field);
            }
        }
        if parts.is_empty() {
            "(unnamed patch)".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// NinjaOne returns release/install times as Unix **seconds**, but some endpoints
/// have historically returned **milliseconds** for `*At` fields. A seconds value
/// for any realistic date is below 1e11 (year 5138), so treat anything larger as
/// milliseconds — otherwise an `from_timestamp(ms, 0)` yields a ~50,000-year date
/// that silently breaks SLA aging.
fn unix_to_datetime(ts: f64) -> Option<DateTime<Utc>> {
    let secs = if ts >= 1e11 { ts / 1000.0 } else { ts };
    DateTime::<Utc>::from_timestamp(secs as i64, 0)
}

/// Patch family the operator wants to list. Selects which API endpoints to query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PatchType {
    All,
    Os,
    Software,
}

impl PatchType {
    pub fn includes_os(self) -> bool {
        matches!(self, Self::All | Self::Os)
    }

    pub fn includes_software(self) -> bool {
        matches!(self, Self::All | Self::Software)
    }
}

/// Operator-facing patch status. `Installed` and `Failed` are install *results*,
/// sourced from the `*-patch-installs` history endpoints; `Pending`/`Approved`/
/// `Rejected` come from the current-patches feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PatchStatus {
    Pending,
    Approved,
    Rejected,
    Installed,
    Failed,
}

impl PatchStatus {
    /// The status string NinjaOne returns/accepts for this state. NinjaOne's
    /// `/queries/{os,software}-patches` use `MANUAL` for patches pending approval
    /// (its UI labels them "Pending"), so the operator-facing "Pending" maps to
    /// `MANUAL` — not the literal `PENDING`, which the API never returns.
    pub fn api_value(self) -> &'static str {
        match self {
            Self::Pending => "MANUAL",
            Self::Approved => "APPROVED",
            Self::Rejected => "REJECTED",
            Self::Installed => "INSTALLED",
            Self::Failed => "FAILED",
        }
    }

    /// Whether this status is sourced from the `*-patch-installs` history
    /// endpoints rather than the current-patches feed. Both `Installed` and
    /// `Failed` are install *results*: per the NinjaOne API, the current
    /// `/queries/{os,software}-patches` feed returns only patches "for which there
    /// were no installation attempts" (MANUAL/APPROVED/REJECTED), while the
    /// `*-patch-installs` history endpoints return the "successful and failed"
    /// records (status `INSTALLED`/`FAILED`). Routing `Failed` to the current feed
    /// is why a FAILED query returns nothing — it is never present there.
    pub fn is_install_history(self) -> bool {
        matches!(self, Self::Installed | Self::Failed)
    }
}

/// One joined detail row: a single patch on a single device, enriched with the
/// device's organization/location/role/OS names. This is the export unit and the
/// table row shown in the UI.
///
/// Serialized to the frontend over IPC: field names MUST be camelCase to match
/// `web-rs/src/types.rs` (which deserializes with `rename_all = "camelCase"`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchRow {
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    pub location: Option<String>,
    pub device_role: Option<String>,
    pub os_name: Option<String>,
    pub node_class: Option<String>,
    pub needs_reboot: bool,
    /// Mirrors `Device::is_offline()`. Carried on the row so the frontend can show
    /// which selected devices an action would be *queued* for rather than run now.
    pub offline: bool,
    pub patch_type: String,
    pub kb: Option<String>,
    pub name: String,
    pub severity: String,
    pub severity_rank: u8,
    pub status: String,
    pub release_date: Option<String>,
    pub installed_date: Option<String>,
    pub release_ts: Option<i64>,
    pub installed_ts: Option<i64>,
}

/// How `POST /v2/device/{id}/reboot/{mode}` is addressed. `Forced` skips the
/// graceful-shutdown path, so it discards unsaved work — the UI gates it behind an
/// extra confirmation tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RebootMode {
    Normal,
    Forced,
}

impl RebootMode {
    /// The path segment NinjaOne expects.
    pub fn api_value(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Forced => "FORCED",
        }
    }
}

/// One entry from `GET /v2/activities`. Only the fields needed to resolve a
/// dispatched job are modelled; the feed carries far more.
#[derive(Debug, Clone, Deserialize)]
pub struct Activity {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default, rename = "activityType")]
    pub activity_type: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "activityTime")]
    pub activity_time: Option<f64>,
    /// Activity series uid — the correlator shared with `Job.uid` and, on some
    /// tenants, the `script/run` response.
    #[serde(default, rename = "seriesUid")]
    pub series_uid: Option<String>,
    /// Exit-code fields vary by activity kind, so this stays untyped.
    #[serde(default)]
    pub result: Option<Value>,
}

impl Activity {
    /// Whether the activity has reached a state that will not change again.
    /// NinjaOne spells cancellation both ways across tenants.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_deref(),
            Some("COMPLETED" | "FAILED" | "TIMED_OUT" | "CANCELED" | "CANCELLED")
        )
    }

    pub fn exit_code(&self) -> Option<i32> {
        let v = self.result.as_ref()?;
        for key in ["exitCode", "resultCode"] {
            if let Some(n) = v.get(key).and_then(Value::as_i64) {
                return Some(n as i32);
            }
        }
        None
    }
}

/// A script variable declared on a library entry. The `name` is what NinjaOne
/// injects as an environment variable on the device.
#[derive(Debug, Clone, Deserialize)]
pub struct ScriptVariable {
    #[serde(default)]
    pub name: Option<String>,
}

/// One entry from `GET /v2/automation/scripts`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationScript {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub operating_systems: Vec<String>,
    #[serde(default)]
    pub script_parameters: Vec<String>,
    #[serde(default)]
    pub script_variables: Vec<ScriptVariable>,
}

/// The script-variable / parameter name that lets a library script be told *which*
/// KBs to install. NinjaOne has no per-KB apply endpoint, so a script declaring
/// this is the only way to target specific patches.
const KB_ALLOW_LIST_VAR: &str = "kballowlist";

impl AutomationScript {
    /// Whether this script can be told which KBs to install — i.e. it declares a
    /// `kbAllowList` script variable or parameter. Only such a script may be
    /// offered per-KB targeting in the UI; anything else installs whatever the
    /// device needs, and pretending otherwise would misrepresent what runs.
    pub fn accepts_kb_allow_list(&self) -> bool {
        let matches = |s: &str| s.trim().to_ascii_lowercase().contains(KB_ALLOW_LIST_VAR);
        self.script_variables
            .iter()
            .filter_map(|v| v.name.as_deref())
            .any(matches)
            || self.script_parameters.iter().any(|p| matches(p))
    }
}

/// Credential choices available for `runAs` on a given device.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeviceCredentialOptions {
    #[serde(default)]
    pub roles: Vec<String>,
}

/// Response of `GET /v2/device/{id}/scripting/options`.
///
/// Only the credential roles are modelled. The `scripts` array duplicates
/// `/automation/scripts` (which the picker already reads, and which is the only
/// source carrying `scriptVariables`), so deserializing it twice would buy nothing.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeviceScriptingOptions {
    #[serde(default)]
    pub credentials: DeviceCredentialOptions,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_type_includes() {
        assert!(PatchType::All.includes_os() && PatchType::All.includes_software());
        assert!(PatchType::Os.includes_os() && !PatchType::Os.includes_software());
        assert!(!PatchType::Software.includes_os() && PatchType::Software.includes_software());
    }

    #[test]
    fn status_api_value_and_install_history_routing() {
        assert_eq!(PatchStatus::Pending.api_value(), "MANUAL");
        assert_eq!(PatchStatus::Installed.api_value(), "INSTALLED");
        assert_eq!(PatchStatus::Failed.api_value(), "FAILED");
        // Installed AND Failed are install results → history endpoints; the rest
        // come from the current-patches feed.
        assert!(PatchStatus::Installed.is_install_history());
        assert!(PatchStatus::Failed.is_install_history());
        assert!(!PatchStatus::Approved.is_install_history());
        assert!(!PatchStatus::Pending.is_install_history());
        assert!(!PatchStatus::Rejected.is_install_history());
    }

    #[test]
    fn severity_from_raw_maps_msrc_strings() {
        assert_eq!(Severity::from_raw("Critical"), Severity::Critical);
        assert_eq!(Severity::from_raw("important"), Severity::Important);
        assert_eq!(Severity::from_raw("garbage"), Severity::Unknown);
    }

    #[test]
    fn severity_from_raw_maps_ninjaones_own_classifications() {
        // Both vocabularies arrive on the same field, in both cases. `security` is
        // the largest bucket on a real OS feed and `recommended` is the third-party
        // non-critical tier; before they were mapped, both fell to Unknown — rank 0,
        // so they sorted below every other patch and the severity facet dropped them.
        assert_eq!(Severity::from_raw("security"), Severity::Security);
        assert_eq!(Severity::from_raw("SECURITY"), Severity::Security);
        assert_eq!(Severity::from_raw("recommended"), Severity::Recommended);
        assert_eq!(Severity::from_raw("RECOMMENDED"), Severity::Recommended);
        // NinjaOne's literal "unknown" still means unknown.
        assert_eq!(Severity::from_raw("unknown"), Severity::Unknown);
    }

    #[test]
    fn ninjaone_classifications_rank_below_important_but_above_unknown() {
        // Load-bearing: the compliance and SLA-aging rollups keep a patch only when
        // `rank() >= Important.rank()`. Ranking either of these at or above Important
        // would sweep a whole classification into the critical backlog and move the
        // operator's numbers without them asking for it.
        for sev in [Severity::Security, Severity::Recommended] {
            assert!(sev.rank() < Severity::Important.rank());
            assert!(sev.rank() > Severity::Unknown.rank());
        }
        // The ordering the table sorts by stays strictly descending.
        let ranks: Vec<u8> = [
            Severity::Critical,
            Severity::Important,
            Severity::Security,
            Severity::Moderate,
            Severity::Recommended,
            Severity::Low,
            Severity::Optional,
            Severity::Unknown,
        ]
        .iter()
        .map(|s| s.rank())
        .collect();
        assert!(
            ranks.windows(2).all(|w| w[0] > w[1]),
            "severity ranks must be strictly descending: {ranks:?}"
        );
    }

    fn patch_with_release(ts: f64) -> Patch {
        Patch {
            device_id: None,
            kb_number: None,
            name: None,
            version: None,
            product_vendor: None,
            severity: None,
            status: None,
            patch_type: None,
            release_timestamp: Some(ts),
            installed_timestamp: None,
        }
    }

    #[test]
    fn millisecond_release_timestamp_normalizes_to_seconds() {
        let secs = 1_700_000_000.0; // 2023-11-14, comfortably in Unix-seconds range
        let from_secs = patch_with_release(secs).released_at();
        let from_millis = patch_with_release(secs * 1000.0).released_at();
        assert!(from_secs.is_some());
        assert_eq!(
            from_secs, from_millis,
            "a millisecond value must map to the same instant as seconds"
        );
    }
}
