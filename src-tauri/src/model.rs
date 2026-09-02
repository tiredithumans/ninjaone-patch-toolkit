use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

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
        self.os_name_str().map(str::to_string)
    }

    /// Borrowing form of [`os_name`](Self::os_name). Preferred in per-patch loops:
    /// the owning variant allocates a `String` for every patch examined, including
    /// the majority that the filters immediately discard.
    pub fn os_name_str(&self) -> Option<&str> {
        self.os.as_ref().and_then(|o| o.name.as_deref())
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

    /// Whether NinjaOne patch management can act on this device at all.
    ///
    /// Patch management covers Windows, macOS and Linux agents. Everything else in
    /// the inventory — network gear (`NMS_*`), cloud monitors, agentless VM
    /// hosts/guests, mobile devices — reports no patch records, so a zero pending
    /// count says nothing about it; scoring it compliant inflated the headline
    /// percentage by exactly the share of the fleet that cannot be patched. Written
    /// as an allow list so a class this crate has never seen fails toward
    /// *exclusion* — which every compliance surface states as a count — rather than
    /// toward a silently higher percentage. A device with no `nodeClass` at all is
    /// kept: there is nothing to prove it out on.
    pub fn is_patchable(&self) -> bool {
        match self.node_class.as_deref() {
            Some(class) => PATCHABLE_NODE_CLASSES
                .iter()
                .any(|c| c.eq_ignore_ascii_case(class)),
            None => true,
        }
    }
}

/// The `nodeClass` values NinjaOne patch management covers, from the spec's
/// `NodeClass` enum. See [`Device::is_patchable`].
pub const PATCHABLE_NODE_CLASSES: [&str; 6] = [
    "WINDOWS_SERVER",
    "WINDOWS_WORKSTATION",
    "LINUX_SERVER",
    "LINUX_WORKSTATION",
    "MAC",
    "MAC_SERVER",
];

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
    /// Whether `raw` is a value this build has no mapping for.
    ///
    /// Distinct from "maps to [`Unknown`](Self::Unknown)": NinjaOne really does send
    /// `unknown`, and an unrated patch is a legitimate state the severity facet can
    /// select. This is the *other* case — a value the vendor added that this build
    /// has never seen, which silently sinks to the bottom of the severity sort and
    /// disappears whenever the facet is active.
    ///
    /// It matters because the spec declares `DeviceOSPatch.severity` as a free-form
    /// string with no enum (see `docs/api/ninjaone-surface.md`), so the vendor
    /// promises nothing about this vocabulary and the mapping cannot be exhaustive
    /// by construction. `build_rows` reports these once per distinct value.
    pub fn is_unmapped(raw: &str) -> bool {
        !matches!(raw.to_ascii_uppercase().as_str(), "UNKNOWN" | "")
            && Self::from_raw(raw) == Self::Unknown
    }

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
    /// When NinjaOne last collected/updated this patch record — **not** a patch
    /// release date.
    ///
    /// The API exposes no release date at all: per the official spec, `DeviceOSPatch`
    /// and `DeviceSoftwarePatch` carry only `installedAt` ("Installation attempt
    /// timestamp") and `timestamp` ("Date/Time when data was collected/updated"),
    /// and `releaseDate` appears nowhere in it. This field used to alias
    /// `releaseDate` too and was named `release_timestamp`, so every consumer read a
    /// collection time as though it were a publication date — which made the SLA
    /// rollup compare *now* against a timestamp that is always recent and therefore
    /// report ~0 breaches on any fleet.
    ///
    /// For a *pending* patch this is the closest thing available to "how long have we
    /// known about this", which is what the SLA and age rollups now say they measure.
    #[serde(default, alias = "timestamp")]
    pub collected_timestamp: Option<f64>,
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

    /// When NinjaOne first reported this patch record. Named for what it is — see
    /// [`Patch::collected_timestamp`]; this is not a release date.
    pub fn first_seen_at(&self) -> Option<DateTime<Utc>> {
        self.collected_timestamp.and_then(unix_to_datetime)
    }

    pub fn installed_at(&self) -> Option<DateTime<Utc>> {
        self.installed_timestamp.and_then(unix_to_datetime)
    }

    /// Human-friendly patch label combining KB, vendor, name and version.
    /// The operator-facing patch title — KB, vendor, name and version, joined — into
    /// a caller-owned buffer, which it clears first.
    ///
    /// Buffered rather than returning a `String` because the join calls it once per
    /// patch across the whole fleet purely to produce a key for the row interner, and
    /// the vast majority of those keys already exist. One reused buffer turns "a
    /// `String` allocated and dropped per row" into one allocation for the whole join.
    pub fn write_display_name(&self, buf: &mut String) {
        buf.clear();
        for field in [
            self.kb_number.as_deref(),
            self.product_vendor.as_deref(),
            self.name.as_deref(),
            self.version.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if field.is_empty() {
                continue;
            }
            if !buf.is_empty() {
                buf.push_str(" · ");
            }
            buf.push_str(field);
        }
        if buf.is_empty() {
            buf.push_str("(unnamed patch)");
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

    /// The operator-facing name of this status — what the UI's Status facet calls
    /// it, and what an export's provenance block prints. Deliberately not
    /// [`api_value`](Self::api_value): `Pending` is NinjaOne's `MANUAL`, and an
    /// export that named the wire value would describe a filter the operator never
    /// recognizes selecting.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Approved => "Approved",
            Self::Rejected => "Rejected",
            Self::Installed => "Installed",
            Self::Failed => "Failed",
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
    pub device_name: Arc<str>,
    pub organization: Arc<str>,
    pub location: Option<Arc<str>>,
    pub device_role: Option<Arc<str>>,
    pub os_name: Option<Arc<str>>,
    pub node_class: Option<Arc<str>>,
    pub needs_reboot: bool,
    /// Mirrors `Device::is_offline()`. Carried on the row so the frontend can show
    /// which selected devices an action would be *queued* for rather than run now.
    pub offline: bool,
    /// One of a two-value vocabulary supplied by the join itself, so it is borrowed
    /// rather than allocated per row.
    pub patch_type: &'static str,
    pub kb: Option<Arc<str>>,
    pub name: Arc<str>,
    /// [`Severity::label`] is already `&'static str`; a row does not need its own copy.
    pub severity: &'static str,
    pub severity_rank: u8,
    pub status: Arc<str>,
    /// Formatted [`Patch::collected_timestamp`] — when NinjaOne first reported the
    /// patch, not when it was published. See that field for why.
    pub first_seen_date: Option<String>,
    pub installed_date: Option<String>,
    pub first_seen_ts: Option<i64>,
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
///
/// The spec's `Activity` schema names three fields the terminal-state decision
/// depends on, and they are not interchangeable:
/// * `statusCode` — the enumerated lifecycle code (`STARTED`, `IN_PROCESS`,
///   `COMPLETED`, `CANCELLED`, `BLOCKED`, …).
/// * `status` — "Status description", free text, with no enum at all.
/// * `activityResult` — `SUCCESS` / `FAILURE` / `UNSUPPORTED` / `UNCOMPLETED` /
///   `AGENT_OFFLINE`.
///
/// This modelled only `status` and matched the *code* vocabulary against it, so on a
/// tenant where `status` really is a description, no job ever reached a terminal
/// state and every dispatch resolved by timeout instead. Both are read now, code
/// first, and the outcome comes from `activityResult` rather than from a `FAILED`
/// spelling that the code vocabulary does not contain.
#[derive(Debug, Clone, Deserialize)]
pub struct Activity {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default, rename = "activityType")]
    pub activity_type: Option<String>,
    /// The enumerated lifecycle code. Preferred over [`status`](Self::status).
    #[serde(default, rename = "statusCode")]
    pub status_code: Option<String>,
    /// The human-readable status description. Kept as a fallback because tenants
    /// have been observed returning the code here, which is what the previous
    /// version relied on exclusively.
    #[serde(default)]
    pub status: Option<String>,
    /// `SUCCESS` / `FAILURE` / `UNSUPPORTED` / `UNCOMPLETED` / `AGENT_OFFLINE`.
    #[serde(default, rename = "activityResult")]
    pub activity_result: Option<String>,
    #[serde(default, rename = "activityTime")]
    pub activity_time: Option<f64>,
    /// Activity series uid — the correlator shared with `Job.uid` and, on some
    /// tenants, the `script/run` response.
    #[serde(default, rename = "seriesUid")]
    pub series_uid: Option<String>,
    /// The free-form payload. `data` is the spec's name for it; `result` is kept as
    /// an alias because that is the key this code originally read — and reading only
    /// a key the schema does not define meant `exit_code()` silently returned `None`
    /// for every job, which surfaced as "Completed, no exit code" rather than as an
    /// error.
    #[serde(default, alias = "result")]
    pub data: Option<Value>,
}

/// How a terminal [`Activity`] turned out, decoupled from the `JobState` it maps to
/// so `model` stays free of the action domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityOutcome {
    Succeeded,
    TimedOut,
    /// Carries the code that explains it, for the job report.
    Failed(String),
}

impl Activity {
    /// The lifecycle code, preferring the enumerated field over the description.
    fn lifecycle(&self) -> Option<&str> {
        self.status_code.as_deref().or(self.status.as_deref())
    }

    /// Whether the activity has reached a state that will not change again.
    ///
    /// `CANCELED`/`CANCELLED` are both accepted (tenants spell it both ways), and
    /// `FAILED`/`TIMED_OUT` are kept even though the documented `statusCode` enum
    /// contains neither — dropping a spelling that a tenant might still emit would
    /// turn a finished job back into one that hangs to timeout.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.lifecycle(),
            Some(
                "COMPLETED"
                    | "FAILED"
                    | "TIMED_OUT"
                    | "CANCELED"
                    | "CANCELLED"
                    | "BLOCKED"
                    | "EVALUATION_FAILURE"
            )
        )
    }

    /// How a terminal activity turned out, as a short code for the job report.
    ///
    /// `activityResult` is the field the spec gives an outcome enum, so it decides;
    /// the lifecycle code answers only when the result is absent. A `COMPLETED`
    /// activity carrying `FAILURE` is a failure, which the previous version — which
    /// read the lifecycle alone — reported as success.
    pub fn outcome(&self) -> ActivityOutcome {
        match self.activity_result.as_deref() {
            Some("SUCCESS") => return ActivityOutcome::Succeeded,
            Some("UNCOMPLETED") => return ActivityOutcome::TimedOut,
            Some(other) if !other.is_empty() => {
                return ActivityOutcome::Failed(other.to_string());
            }
            _ => {}
        }
        match self.lifecycle() {
            Some("COMPLETED") => ActivityOutcome::Succeeded,
            Some("TIMED_OUT") => ActivityOutcome::TimedOut,
            Some(other) => ActivityOutcome::Failed(other.to_string()),
            None => ActivityOutcome::Failed("unknown terminal state".into()),
        }
    }

    pub fn exit_code(&self) -> Option<i32> {
        let v = self.data.as_ref()?;
        // Checked at the top level and one nesting down: `data` is an untyped bag
        // (`additionalProperties: true`), and tenants have been seen putting the
        // script result under `data.result` as well as directly on `data`.
        for bag in [Some(v), v.get("result")].into_iter().flatten() {
            for key in ["exitCode", "resultCode"] {
                if let Some(n) = bag.get(key).and_then(Value::as_i64) {
                    return Some(n as i32);
                }
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

    /// A device with every optional field absent — the shape the feed actually
    /// sends for a sparsely-populated record.
    fn device() -> Device {
        Device {
            id: 1,
            system_name: None,
            display_name: None,
            organization_id: None,
            location_id: None,
            node_role_id: None,
            node_class: None,
            offline: None,
            os: None,
        }
    }

    fn script() -> AutomationScript {
        AutomationScript {
            id: 1,
            name: None,
            description: None,
            active: None,
            language: None,
            operating_systems: Vec::new(),
            script_parameters: Vec::new(),
            script_variables: Vec::new(),
        }
    }

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

    /// `docs/api/ninjaone-surface.md` records that `DeviceOSPatch.severity` is a
    /// free-form string with no enum, so the vendor promises nothing about this
    /// vocabulary. `is_unmapped` separates "NinjaOne told us it is unrated" — a real,
    /// selectable state — from "NinjaOne sent something this build has never seen",
    /// which is the case worth a log line.
    #[test]
    fn an_unrated_patch_is_not_the_same_as_an_unrecognised_severity() {
        // Genuinely unrated: NinjaOne's own word for it, and the empty case.
        assert!(!Severity::is_unmapped("unknown"));
        assert!(!Severity::is_unmapped("UNKNOWN"));
        assert!(!Severity::is_unmapped(""));
        // Every alias the build handles.
        for raw in [
            "CRITICAL",
            "IMPORTANT",
            "HIGH",
            "SECURITY",
            "MODERATE",
            "MEDIUM",
            "RECOMMENDED",
            "LOW",
            "OPTIONAL",
            "NONE",
            "critical",
            "security",
        ] {
            assert!(!Severity::is_unmapped(raw), "{raw} is mapped");
        }
        // A value the vendor could add tomorrow. It maps to Unknown either way; the
        // point is that this build can tell it apart from a real "unknown" and say so.
        for raw in ["ELEVATED", "SEV1", "major"] {
            assert!(
                Severity::is_unmapped(raw),
                "{raw} should report as unmapped"
            );
            assert_eq!(Severity::from_raw(raw), Severity::Unknown);
        }
    }

    /// `HIGH`, `MEDIUM` and `NONE` are aliases the doc comment above `from_raw`
    /// never mentions, so nothing recorded that they were deliberate. They matter:
    /// an alias that stops mapping falls to `Unknown` (rank 0), which both sinks
    /// those patches below every other row and makes them unreachable from the
    /// severity facet — the failure mode reads as "those patches don't exist".
    #[test]
    fn severity_from_raw_maps_every_documented_alias() {
        for (raw, expected) in [
            ("HIGH", Severity::Important),
            ("high", Severity::Important),
            ("MEDIUM", Severity::Moderate),
            ("medium", Severity::Moderate),
            ("NONE", Severity::Optional),
            ("none", Severity::Optional),
        ] {
            assert_eq!(Severity::from_raw(raw), expected, "alias {raw}");
        }
    }

    /// Every value the frontend can offer in its severity facet must map to a
    /// distinct variant, or ticking it filters to nothing.
    #[test]
    fn every_facet_value_maps_to_its_own_severity() {
        for raw in [
            "CRITICAL",
            "IMPORTANT",
            "SECURITY",
            "MODERATE",
            "RECOMMENDED",
            "LOW",
            "OPTIONAL",
        ] {
            let sev = Severity::from_raw(raw);
            assert_ne!(
                sev,
                Severity::Unknown,
                "{raw} is offered as a facet value but maps to Unknown"
            );
            assert_eq!(
                sev.label().to_ascii_uppercase(),
                raw,
                "{raw} must round-trip through its label"
            );
        }
    }

    #[test]
    fn device_labels_prefer_display_name_and_degrade_to_a_placeholder() {
        let with_both = Device {
            display_name: Some("web-01.corp".into()),
            system_name: Some("WEB01".into()),
            ..device()
        };
        assert_eq!(with_both.label(), "web-01.corp");

        let system_only = Device {
            display_name: None,
            system_name: Some("WEB01".into()),
            ..device()
        };
        assert_eq!(system_only.label(), "WEB01");

        // Never blank: a nameless row is still actionable by id, and an empty cell
        // in the export reads as a broken join.
        assert_eq!(device().label(), "(unnamed)");
    }

    /// Both default to "no" on absent data. A missing `os.needsReboot` must not
    /// sweep a device into the reboot rollup, and a missing `offline` must not
    /// mark a reachable device as queued-only.
    #[test]
    fn device_flags_default_to_false_when_the_feed_omits_them() {
        assert!(!device().needs_reboot());
        assert!(!device().is_offline());
        assert_eq!(device().os_name(), None);
        assert_eq!(device().os_name_str(), None);

        let d = Device {
            offline: Some(true),
            os: Some(OsInfo {
                name: Some("Windows Server 2022".into()),
                needs_reboot: Some(true),
            }),
            ..device()
        };
        assert!(d.needs_reboot());
        assert!(d.is_offline());
        assert_eq!(d.os_name_str(), Some("Windows Server 2022"));
    }

    /// Per-KB targeting is possible *only* through a script declaring a
    /// `kbAllowList`; offering it for a script that doesn't would misrepresent what
    /// the run actually installs. The match is case- and whitespace-insensitive
    /// because the variable name is typed by hand in the NinjaOne library.
    #[test]
    fn kb_allow_list_is_detected_from_variables_or_parameters() {
        let via_variable = AutomationScript {
            script_variables: vec![ScriptVariable {
                name: Some("  KBAllowList ".into()),
            }],
            ..script()
        };
        assert!(via_variable.accepts_kb_allow_list());

        let via_parameter = AutomationScript {
            script_parameters: vec!["-kbAllowList $kbs".into()],
            ..script()
        };
        assert!(via_parameter.accepts_kb_allow_list());

        assert!(
            !script().accepts_kb_allow_list(),
            "a script declaring nothing must not be offered per-KB targeting"
        );
        let unrelated = AutomationScript {
            script_parameters: vec!["-Force".into()],
            script_variables: vec![ScriptVariable {
                name: Some("RebootAfter".into()),
            }],
            ..script()
        };
        assert!(!unrelated.accepts_kb_allow_list());
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

    fn patch_collected_at(ts: f64) -> Patch {
        Patch {
            device_id: None,
            kb_number: None,
            name: None,
            version: None,
            product_vendor: None,
            severity: None,
            status: None,
            patch_type: None,
            collected_timestamp: Some(ts),
            installed_timestamp: None,
        }
    }

    #[test]
    fn millisecond_collected_timestamp_normalizes_to_seconds() {
        let secs = 1_700_000_000.0; // 2023-11-14, comfortably in Unix-seconds range
        let from_secs = patch_collected_at(secs).first_seen_at();
        let from_millis = patch_collected_at(secs * 1000.0).first_seen_at();
        assert!(from_secs.is_some());
        assert_eq!(
            from_secs, from_millis,
            "a millisecond value must map to the same instant as seconds"
        );
    }
}
