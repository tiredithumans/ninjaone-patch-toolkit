use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

/// MSRC-aligned severity buckets returned by NinjaOne's patch feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Severity {
    Critical,
    Important,
    Moderate,
    Low,
    Optional,
    Unknown,
}

impl Severity {
    pub fn from_raw(raw: &str) -> Self {
        match raw.to_ascii_uppercase().as_str() {
            "CRITICAL" => Self::Critical,
            "IMPORTANT" | "HIGH" => Self::Important,
            "MODERATE" | "MEDIUM" => Self::Moderate,
            "LOW" => Self::Low,
            "OPTIONAL" | "NONE" => Self::Optional,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "Critical",
            Self::Important => "Important",
            Self::Moderate => "Moderate",
            Self::Low => "Low",
            Self::Optional => "Optional",
            Self::Unknown => "Unknown",
        }
    }

    /// Higher = more urgent. Used for SLA aging on Critical/Important.
    pub fn rank(self) -> u8 {
        match self {
            Self::Critical => 5,
            Self::Important => 4,
            Self::Moderate => 3,
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
