//! What can be dispatched: `ActionKind`, its guardrail predicates, `RebootChoice`,
//! and which library script a remediation kind resolves to (`remediation_script_id`).

use crate::settings::ActionSettings;
use serde::{Deserialize, Serialize};

/// What the operator asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionKind {
    OsPatchScan,
    SoftwarePatchScan,
    OsPatchApply,
    SoftwarePatchApply,
    /// Install *only* the selected patches, via the configured OS remediation
    /// script. See [`ActionKind::is_remediation`] for why this is a kind of its own
    /// rather than a [`Self::Script`] with a preselected library entry.
    OsPatchRemediate,
    SoftwarePatchRemediate,
    Reboot,
    Script,
}

impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OsPatchScan => "Scan for OS patches",
            Self::SoftwarePatchScan => "Scan for software patches",
            // "all" vs "selected" is the whole distinction between these two pairs,
            // and it is the one the operator cannot recover from afterwards, so it
            // is in the label rather than in help text next to it.
            Self::OsPatchApply => "Apply all OS patches",
            Self::SoftwarePatchApply => "Apply all software patches",
            Self::OsPatchRemediate => "Apply selected OS patches",
            Self::SoftwarePatchRemediate => "Apply selected software patches",
            Self::Reboot => "Reboot",
            Self::Script => "Run script",
        }
    }

    /// Whether this action's reach is wider than the operator's selection.
    ///
    /// True only for the two native apply endpoints. NinjaOne has no per-patch apply,
    /// so `/patch/{os,software}/apply` takes no target list and installs the device's
    /// entire approved backlog — ticking one row and pressing it installs everything
    /// approved on that device. `plan()` already warns when this is paired with a
    /// partial selection; this is the same fact in a form the *UI* can render, so the
    /// button and the confirm dialog can state the reach instead of leaving it to an
    /// 11px group heading and the README. Mirrored in `web-rs/src/types.rs`.
    pub fn exceeds_selection(self) -> bool {
        // Defined as "has a targeted counterpart" rather than by its own `matches!`
        // list: the two are the same set by construction, and a third apply
        // mechanism should not be able to appear in one list and not the other.
        self.targeted_counterpart().is_some()
    }

    /// Whether this changes the device. Drives the confirmation gate, the
    /// blast-radius cap, and the maintenance-window check — a scan only refreshes
    /// NinjaOne's view of what the device needs, so it is exempt from all three.
    pub fn is_mutating(self) -> bool {
        !matches!(self, Self::OsPatchScan | Self::SoftwarePatchScan)
    }

    /// Whether this can restart the device as a side effect.
    ///
    /// Every mutating kind can: a reboot obviously, and any install because patches
    /// routinely set the pending-reboot flag. Only a scan cannot. This exists so the
    /// post-action cache invalidation is one decision instead of two — the dispatch
    /// site invalidated the device inventory only for `Reboot` while the job poller
    /// invalidated it for *every* settled batch, so the two copies of the same rule
    /// disagreed in both directions at once. Mirrors `can_reboot` in
    /// `web-rs/src/types.rs`.
    pub fn can_reboot(self) -> bool {
        self.is_mutating()
    }

    /// Whether this kind *can* preview at all. Only a library script can (via its
    /// own `dryRun` parameter); the native endpoints have no preview mode, so a "dry
    /// run" of them dispatches nothing.
    ///
    /// Necessary, not sufficient: a script previews only if it actually reads
    /// `dryRun`, which is a property of the library entry, not of the kind — see
    /// [`DryRunSupport`](super::DryRunSupport), which `plan()` checks on top of this.
    pub fn supports_dry_run(self) -> bool {
        self.runs_a_script()
    }

    /// Whether this dispatches a library script rather than a native endpoint.
    pub fn runs_a_script(self) -> bool {
        matches!(
            self,
            Self::Script | Self::OsPatchRemediate | Self::SoftwarePatchRemediate
        )
    }

    /// Whether this is a *targeted* apply — a remediation script that receives the
    /// specific patches the operator ticked.
    ///
    /// This is a kind rather than a preselected [`Self::Script`] for three reasons,
    /// each of which was a real defect in the script-only design: the kind is what
    /// selects the parameter encoding (`kbAllowList` vs `productAllowListB64`, which
    /// the software path never reached), it is what the Jobs tab and the audit log
    /// record (they said "Run script", not what was installed), and it is what lets
    /// the guardrails demand a target list at all.
    pub fn is_remediation(self) -> bool {
        matches!(self, Self::OsPatchRemediate | Self::SoftwarePatchRemediate)
    }

    /// The native apply that installs the device's *whole* approved backlog for the
    /// same patch family — the other half of the pair, named for the warning and
    /// tooltip that point between them.
    pub fn untargeted_counterpart(self) -> Option<Self> {
        match self {
            Self::OsPatchRemediate => Some(Self::OsPatchApply),
            Self::SoftwarePatchRemediate => Some(Self::SoftwarePatchApply),
            _ => None,
        }
    }

    /// The targeted counterpart of a native apply, if this is one.
    pub fn targeted_counterpart(self) -> Option<Self> {
        match self {
            Self::OsPatchApply => Some(Self::OsPatchRemediate),
            Self::SoftwarePatchApply => Some(Self::SoftwarePatchRemediate),
            _ => None,
        }
    }
}

/// The library script id configured for a remediation kind, if any.
///
/// NinjaOne has no script-upload API, so these are added to the library by hand and
/// their numeric ids pasted into Settings → Patch actions. An unset id is what makes
/// the corresponding action unavailable rather than silently no-op.
pub fn remediation_script_id(kind: ActionKind, s: &ActionSettings) -> Option<i64> {
    match kind {
        ActionKind::OsPatchRemediate => s.os_patch_script_id,
        ActionKind::SoftwarePatchRemediate => s.software_patch_script_id,
        _ => None,
    }
}

/// Whether the dispatched *script* should restart the device when it finishes.
/// Distinct from [`RebootMode`](crate::model::RebootMode), which addresses the reboot endpoint directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RebootChoice {
    #[default]
    Never,
    Auto,
}

impl RebootChoice {
    /// The token the PowerShell side expects for `rebootBehavior` — the vocabulary
    /// `remediation/*.ps1` accept (`ConvertTo-ToolkitRebootBehavior`).
    pub fn script_value(self) -> &'static str {
        match self {
            Self::Never => "Never",
            Self::Auto => "Auto",
        }
    }
}
