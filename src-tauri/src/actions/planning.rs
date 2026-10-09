//! Pure planning: `plan()` guardrails, the plan types, the "Apply all" preview.

use super::parameters::{kb_number, uses_kb_encoding};
use super::*;
use crate::model::{Device, Patch, PatchStatus, RebootMode};
use crate::settings::ActionSettings;
use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};

/// A device the action will be dispatched to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedTarget {
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    pub offline: bool,
}

/// A device that was asked for but will not be dispatched to, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedTarget {
    pub device_id: i64,
    pub device_name: String,
    pub reason: String,
}

/// The outcome of planning an action: exactly what would happen, and whether it is
/// allowed to happen at all.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlan {
    pub summary: String,
    pub eligible: Vec<PlannedTarget>,
    pub skipped: Vec<SkippedTarget>,
    pub organizations: Vec<String>,
    /// Soft advisories — the action proceeds.
    pub warnings: Vec<String>,
    /// Hard stops. Non-empty means nothing will be dispatched and no confirmation
    /// token is issued.
    pub blockers: Vec<String>,
    pub reboot_expected: bool,
    pub dry_run: bool,
    /// The exact `parameters` string that will be sent, for scripts. Shown verbatim
    /// in the confirmation dialog — the toolkit never sends a string the operator
    /// has not seen.
    pub parameters_preview: Option<String>,
    /// What a native "Apply all" will install, per device, from the cached current
    /// patches. `None` for every other kind.
    pub apply_preview: Option<ApplyPreview>,
    /// True when this dispatch goes out *because* the operator overrode a closed
    /// maintenance window — recorded on the audit trail, so a bypass is never silent.
    pub window_overridden: bool,
    pub confirm_token: Option<String>,
}

impl ActionPlan {
    pub fn is_blocked(&self) -> bool {
        !self.blockers.is_empty()
    }
}

/// Everything [`plan`] needs. Borrowed rather than owned so the caller can pass
/// slices straight out of the warm fleet cache.
pub struct PlanInput<'a> {
    pub kind: ActionKind,
    pub device_ids: &'a [i64],
    pub devices: &'a [Device],
    pub org_names: &'a HashMap<i64, String>,
    pub settings: &'a ActionSettings,
    pub include_offline: bool,
    pub override_window: bool,
    pub reboot_mode: Option<RebootMode>,
    /// Whether a dispatched *script* will restart the device when it finishes.
    /// Distinct from `reboot_mode`, which addresses the reboot endpoint.
    pub reboot: RebootChoice,
    pub dry_run: bool,
    /// Every target that will be composed into a parameter string, across the
    /// requested devices — empty when nothing is composed (a native endpoint, or
    /// hand-typed parameters). The remediation kinds need at least one; the
    /// KB-encoded kinds need each to be a KB number (see [`kb_number`]).
    pub targets: &'a [&'a str],
    /// Whether the resolved script can honor `dryRun`. Consulted only for a dry run
    /// of a script-running kind; see [`DryRunSupport`].
    pub dry_run_support: DryRunSupport,
    /// The *cached* whole-fleet current patches of the family a native apply
    /// installs, or `None` when that cache is cold (and for every other kind). The
    /// planner never fetches: a plan that paged a six-figure feed to draw a preview
    /// would make the confirm dialog wait on it.
    pub current_patches: Option<CachedFamily<'a>>,
    /// Injected so the maintenance-window check is testable.
    pub now: DateTime<Local>,
}

/// One family of the cached whole-fleet current patches, as [`plan`] reads it.
#[derive(Clone, Copy)]
pub struct CachedFamily<'a> {
    pub patches: &'a [Patch],
    pub fetched_at: DateTime<Utc>,
}

/// Whether the script a request resolves to can be told to preview.
///
/// A dry run appends `dryRun=true` to the composed parameter string, and that is
/// all it does — NinjaOne has no preview mode of its own. A script that never reads
/// `dryRun` simply runs for real while the Jobs tab says "Dry run". So a dry run is
/// allowed only for a library script that *declares* a `dryRun` variable or
/// parameter, the same evidence [`AutomationScript::accepts_kb_allow_list`] uses to
/// gate per-KB targeting.
///
/// [`AutomationScript::accepts_kb_allow_list`]: crate::model::AutomationScript::accepts_kb_allow_list
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryRunSupport {
    /// Not evaluated because the request is not a dry run of a script. If a dry run
    /// ever arrives with this, it is treated as unverified and blocked.
    NotChecked,
    /// The library entry declares `dryRun`.
    Declared,
    /// The library entry declares no `dryRun`; carries its name for the message.
    NotDeclared { script: String },
    /// A NinjaOne built-in action, which takes no `dryRun` at all.
    BuiltInAction,
    /// The operator typed the parameter string, which is sent verbatim — the toolkit
    /// never rewrites it, so it cannot add `dryRun=true` to it.
    TypedParameters,
    /// The library could not be read, or no longer lists the script.
    Unverified(String),
}

/// What a native apply will install on one device, counted from the cached
/// current patches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPreviewDevice {
    pub device_id: i64,
    pub device_name: String,
    /// Records with status `APPROVED` — what the apply endpoint installs.
    pub approved: usize,
    /// Records with status `MANUAL` (NinjaOne's "pending approval"), which the apply
    /// endpoint does *not* install until someone approves them in NinjaOne.
    pub pending_manual: usize,
}

/// The confirm dialog's "what will this install" block for a native apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPreview {
    /// `"OS"` or `"software"`.
    pub family: String,
    /// False when the family's current patches were not cached — the counts are
    /// then unknown rather than zero, and `devices` is empty.
    pub known: bool,
    /// One row per eligible device, in plan order.
    pub devices: Vec<ApplyPreviewDevice>,
    pub approved_total: usize,
    pub pending_manual_total: usize,
    /// When the counted patch data was fetched, so the dialog can say how old the
    /// counts are. `None` when unknown.
    pub data_fetched_at: Option<String>,
}

/// Counts what a native apply of `kind` would install on each eligible device.
///
/// `None` for anything but the two native applies. The status comparison is
/// case-insensitive and uses [`PatchStatus::api_value`], so "pending approval" is
/// NinjaOne's `MANUAL`, not the operator-facing "Pending".
pub fn apply_preview(
    kind: ActionKind,
    eligible: &[PlannedTarget],
    cached: Option<CachedFamily<'_>>,
) -> Option<ApplyPreview> {
    if !kind.exceeds_selection() {
        return None;
    }
    let family = patch_family_label(kind).to_string();
    let Some(cached) = cached else {
        return Some(ApplyPreview {
            family,
            known: false,
            devices: Vec::new(),
            approved_total: 0,
            pending_manual_total: 0,
            data_fetched_at: None,
        });
    };
    // One pass over the whole-fleet feed, counting only the devices in the plan.
    let mut counts: HashMap<i64, (usize, usize)> =
        eligible.iter().map(|t| (t.device_id, (0, 0))).collect();
    let approved = PatchStatus::Approved.api_value();
    let manual = PatchStatus::Pending.api_value();
    for p in cached.patches {
        let (Some(id), Some(status)) = (p.device_id, p.status.as_deref()) else {
            continue;
        };
        let Some(c) = counts.get_mut(&id) else {
            continue;
        };
        let status = status.trim();
        if status.eq_ignore_ascii_case(approved) {
            c.0 += 1;
        } else if status.eq_ignore_ascii_case(manual) {
            c.1 += 1;
        }
    }
    let devices: Vec<ApplyPreviewDevice> = eligible
        .iter()
        .map(|t| {
            let (approved, pending_manual) = counts.get(&t.device_id).copied().unwrap_or_default();
            ApplyPreviewDevice {
                device_id: t.device_id,
                device_name: t.device_name.clone(),
                approved,
                pending_manual,
            }
        })
        .collect();
    Some(ApplyPreview {
        family,
        known: true,
        approved_total: devices.iter().map(|d| d.approved).sum(),
        pending_manual_total: devices.iter().map(|d| d.pending_manual).sum(),
        devices,
        data_fetched_at: Some(fmt_ts(cached.fetched_at)),
    })
}

/// "OS" or "software", for the messages about a patch family.
fn patch_family_label(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::SoftwarePatchScan
        | ActionKind::SoftwarePatchApply
        | ActionKind::SoftwarePatchRemediate => "software",
        _ => "OS",
    }
}

/// A device-name list for a message, capped so a 25-device batch stays one sentence.
pub fn summarize_names(names: &[&str]) -> String {
    const SHOWN: usize = 5;
    if names.len() <= SHOWN {
        return names.join(", ");
    }
    format!(
        "{}, and {} more",
        names[..SHOWN].join(", "),
        names.len() - SHOWN
    )
}

/// Decides what an action would do and whether the guardrails permit it.
///
/// Pure: no I/O, no ambient clock. Every guardrail lives here rather than in the
/// webview, so a stale or modified frontend cannot talk the backend into a bigger
/// blast radius than Settings allows.
pub fn plan(input: PlanInput<'_>) -> ActionPlan {
    let by_id: HashMap<i64, &Device> = input.devices.iter().map(|d| (d.id, d)).collect();
    let s = input.settings;

    let mut eligible: Vec<PlannedTarget> = Vec::new();
    let mut skipped: Vec<SkippedTarget> = Vec::new();

    for id in input.device_ids {
        let Some(device) = by_id.get(id) else {
            skipped.push(SkippedTarget {
                device_id: *id,
                device_name: format!("Device {id}"),
                reason: "not in the current device inventory".into(),
            });
            continue;
        };
        let name = device.label().to_string();
        let offline = device.is_offline();
        // NinjaOne *queues* work for an offline device rather than rejecting it, so
        // an action dispatched now can restart the machine hours later when it
        // reconnects — long after the operator stopped watching.
        if offline && !(input.include_offline && s.allow_offline_targets) {
            skipped.push(SkippedTarget {
                device_id: *id,
                device_name: name,
                reason: "device is offline — NinjaOne would queue this until it reconnects".into(),
            });
            continue;
        }
        eligible.push(PlannedTarget {
            device_id: *id,
            device_name: name,
            organization: org_name(input.org_names, device),
            offline,
        });
    }

    let organizations: Vec<String> = eligible
        .iter()
        .map(|t| t.organization.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut warnings = Vec::new();
    let mut blockers = Vec::new();
    let mut window_overridden = false;

    if eligible.is_empty() {
        blockers.push("No eligible devices — nothing would be dispatched.".into());
    }
    // A repeated id would be dispatched to once per occurrence — the same script run
    // twice on one machine — while the operator's selection names it once. The
    // confirmation hash used to de-duplicate the ids, so an approval for `[5]`
    // validated `[5, 5]`; refusing the request is what keeps the two in agreement.
    let mut seen = HashSet::new();
    let duplicates: BTreeSet<i64> = input
        .device_ids
        .iter()
        .copied()
        .filter(|id| !seen.insert(*id))
        .collect();
    if !duplicates.is_empty() {
        blockers.push(format!(
            "The request lists device(s) {} more than once, so they would be dispatched to \
             repeatedly. Clear the selection and select the devices again.",
            duplicates
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // NinjaOne splits `parameters` on spaces into `key=value` tokens, so a KB target
    // is spliced into the string unquoted: "123 dryRun=false" would add a key of its
    // own. The frontend only sends `kbNumber`s, but the backend is the boundary.
    if uses_kb_encoding(input.kind) {
        let malformed: BTreeSet<&str> = input
            .targets
            .iter()
            .copied()
            .filter(|t| kb_number(t).is_none())
            .collect();
        if !malformed.is_empty() {
            blockers.push(format!(
                "Not a KB number: {}. OS patches are targeted by KB (e.g. KB5040434); \
                 re-select the patch rows.",
                malformed
                    .iter()
                    .map(|t| format!("\"{t}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if input.kind.is_mutating() {
        if eligible.len() > s.max_devices_per_action {
            blockers.push(format!(
                "{} devices exceeds the {}-device limit for one action. Narrow the selection, \
                 or raise the limit in Settings → Patch actions.",
                eligible.len(),
                s.max_devices_per_action
            ));
        }
        if organizations.len() > s.max_orgs_per_action {
            blockers.push(format!(
                "Selection spans {} organizations ({}), above the limit of {}. \
                 Dispatch one organization at a time.",
                organizations.len(),
                organizations.join(", "),
                s.max_orgs_per_action
            ));
        }
        if s.require_maintenance_window && !window_is_open(s, input.now) {
            let window = window_label(s, input.now);
            if input.override_window && s.allow_window_override {
                window_overridden = true;
                warnings.push(format!(
                    "Outside the maintenance window ({window}) — proceeding only because this \
                     dispatch overrides it. The override is recorded in the audit trail."
                ));
            } else if s.allow_window_override {
                blockers.push(format!(
                    "Outside the maintenance window ({window}). Wait for the window to open, or \
                     tick \"Override the maintenance window for this dispatch\" in the action bar \
                     and plan again."
                ));
            } else {
                // This used to say "enable the override in Settings", which named only
                // half of it: the Settings checkbox merely *permits* an override, and
                // the per-dispatch one that requests it had no control at all.
                blockers.push(format!(
                    "Outside the maintenance window ({window}). Wait for the window to open. \
                     Overriding it is switched off — to allow it, tick \"Allow overriding the \
                     maintenance window\" in Settings → Patch actions and save, then tick \
                     \"Override the maintenance window for this dispatch\" in the action bar and \
                     plan again."
                ));
            }
        }
    }

    // A dry run of a native endpoint would be a lie: there is no preview mode, so
    // nothing is sent at all. Say so rather than letting the operator believe they
    // previewed something.
    let dry_run = input.dry_run;
    if dry_run && !input.kind.supports_dry_run() {
        blockers.push(format!(
            "\"{}\" has no preview mode in the NinjaOne API — a dry run would dispatch nothing. \
             Turn off Dry run to send it for real.",
            input.kind.label()
        ));
    } else if dry_run && let Some(why) = dry_run_refusal(&input.dry_run_support) {
        // The opposite failure: a script that ignores `dryRun` *does* dispatch, and
        // runs for real while every surface says "Dry run".
        blockers.push(why);
    }

    // The native apply endpoints (`/device/{id}/patch/{os,software}/apply`) have no
    // per-patch variant — they install everything approved on the device, and the
    // ticked rows only chose *which devices* to dispatch to. Grouping the table
    // "By patch" and ticking one row makes the opposite reading the obvious one, so
    // say it outright rather than leaving it to a code comment.
    if let Some(targeted) = input.kind.targeted_counterpart() {
        let family = patch_family_label(input.kind);
        warnings.push(format!(
            "Installs every approved {family} patch on each device, not just the selected rows — \
             NinjaOne has no per-patch apply endpoint. The selection only chose the {} device(s) \
             below. {}",
            eligible.len(),
            if remediation_script_id(targeted, s).is_some() {
                format!(
                    "To install only the selected patches, use \"{}\".",
                    targeted.label()
                )
            } else {
                format!(
                    "To install only the selected patches, configure a remediation script in \
                     Settings → Patch actions and use \"{}\".",
                    targeted.label()
                )
            }
        ));
    }

    // What that backlog actually is, per device. A device with nothing approved is
    // the surprising case: the apply "succeeds" and installs nothing, while the
    // patches the operator was looking at sit in MANUAL waiting for approval.
    let apply_preview = apply_preview(input.kind, &eligible, input.current_patches);
    if let Some(preview) = apply_preview.as_ref().filter(|p| p.known) {
        let empty: Vec<&str> = preview
            .devices
            .iter()
            .filter(|d| d.approved == 0)
            .map(|d| d.device_name.as_str())
            .collect();
        if !empty.is_empty() {
            warnings.push(format!(
                "{} device(s) have no approved {} patches, so nothing will install on them ({}). \
                 Patches pending approval (NinjaOne status MANUAL) must be approved in NinjaOne \
                 first.",
                empty.len(),
                preview.family,
                summarize_names(&empty)
            ));
        }
    }

    // The targeted half of the pair. Both blockers describe a request that cannot
    // install anything, so they fail closed rather than dispatching a script with an
    // empty allow list — which reads in NinjaOne's activity feed exactly like a
    // successful run that installed nothing.
    if input.kind.is_remediation() {
        let untargeted = input.kind.untargeted_counterpart();
        if remediation_script_id(input.kind, s).is_none() {
            blockers.push(format!(
                "No remediation script configured for this patch family. NinjaOne has no per-patch \
                 apply endpoint, so targeting specific patches needs a library script that accepts \
                 a target list — add one, then paste its id into Settings → Patch actions.{}",
                untargeted
                    .map(|k| format!(
                        " To install the full approved backlog instead, use \"{}\".",
                        k.label()
                    ))
                    .unwrap_or_default()
            ));
        }
        if input.targets.is_empty() {
            blockers.push(format!(
                "No patches selected — this would run the remediation script with an empty target \
                 list and install nothing. Tick the patch rows to install.{}",
                untargeted
                    .map(|k| format!(
                        " To install everything approved instead, use \"{}\".",
                        k.label()
                    ))
                    .unwrap_or_default()
            ));
        }
    }

    // A reboot, an untargeted apply (which installs a whole approved backlog and so
    // routinely reboots), or a script told to reboot. The apply half was spelled as
    // its own `matches!` list naming the same two kinds as `targeted_counterpart`;
    // `exceeds_selection` is now that one definition.
    let reboot_expected = !dry_run
        && (input.kind == ActionKind::Reboot
            || input.kind.exceeds_selection()
            || (input.kind.runs_a_script() && input.reboot == RebootChoice::Auto));
    if reboot_expected {
        warnings.push(match input.reboot_mode {
            Some(RebootMode::Forced) => format!(
                "Forced reboot discards unsaved work on {} device(s).",
                eligible.len()
            ),
            _ => format!("{} device(s) may restart.", eligible.len()),
        });
    }
    let offline_count = eligible.iter().filter(|t| t.offline).count();
    if offline_count > 0 {
        warnings.push(format!(
            "{offline_count} offline device(s) included — NinjaOne will queue the action until \
             they reconnect."
        ));
    }

    ActionPlan {
        summary: format!("{} on {} device(s)", input.kind.label(), eligible.len()),
        organizations,
        eligible,
        skipped,
        warnings,
        blockers,
        reboot_expected,
        dry_run,
        parameters_preview: None,
        apply_preview,
        window_overridden,
        confirm_token: None,
    }
}

/// Why a dry run of a script cannot be honored, or `None` when it can.
fn dry_run_refusal(support: &DryRunSupport) -> Option<String> {
    const OFF: &str = "Turn off Dry run to send it for real.";
    match support {
        DryRunSupport::Declared => None,
        DryRunSupport::NotDeclared { script } => Some(format!(
            "\"{script}\" declares no dryRun script variable or parameter, so it cannot be told \
             to preview — a dry run would run it for real. {OFF} To preview with it, add a dryRun \
             variable to the script in the NinjaOne library."
        )),
        DryRunSupport::BuiltInAction => Some(format!(
            "NinjaOne built-in actions take no dryRun parameter — a dry run would run it for \
             real. {OFF}"
        )),
        DryRunSupport::TypedParameters => Some(format!(
            "Hand-typed parameters are sent verbatim, so the toolkit cannot add dryRun=true to \
             them — a dry run would run the script for real. Clear the Parameters box to compose \
             them from the selection (which adds dryRun=true), or {}",
            OFF.to_lowercase()
        )),
        DryRunSupport::Unverified(why) => Some(format!(
            "Couldn't confirm that the script declares a dryRun variable ({why}), so a dry run \
             could run it for real. Try again, or {}",
            OFF.to_lowercase()
        )),
        DryRunSupport::NotChecked => Some(format!(
            "Couldn't confirm that the script declares a dryRun variable, so a dry run could run \
             it for real. {OFF}"
        )),
    }
}

fn org_name(names: &HashMap<i64, String>, device: &Device) -> String {
    device
        .organization_id
        .and_then(|id| names.get(&id).cloned())
        .unwrap_or_else(|| "(unknown organization)".to_string())
}

/// Whether `now` falls inside the configured window. A start later than the end
/// means the window wraps past midnight (e.g. 22:00–04:00), in which case the day
/// check applies to the day the window *opened*.
pub(super) fn window_is_open(s: &ActionSettings, now: DateTime<Local>) -> bool {
    if s.window_days.is_empty() {
        return false;
    }
    let minute = (now.hour() * 60 + now.minute()) as u16;
    let today = now.weekday().num_days_from_sunday() as u8;
    let yesterday = (today + 6) % 7;

    if s.window_start_minute <= s.window_end_minute {
        s.window_days.contains(&today)
            && minute >= s.window_start_minute
            && minute < s.window_end_minute
    } else {
        // Wrapping window: before midnight belongs to today, after midnight to the
        // day the window opened.
        (s.window_days.contains(&today) && minute >= s.window_start_minute)
            || (s.window_days.contains(&yesterday) && minute < s.window_end_minute)
    }
}

/// The window as the operator configured it, e.g. `Mon/Tue 02:00–05:00`, followed
/// by the clock it is checked against. [`window_is_open`] reads *this computer's*
/// local time — not the devices' time zones, which NinjaOne does not expose here —
/// so the label says so, with the offset, rather than a bare "local".
fn window_label(s: &ActionSettings, now: DateTime<Local>) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let days: Vec<&str> = s
        .window_days
        .iter()
        .filter_map(|d| DAYS.get(*d as usize).copied())
        .collect();
    let hhmm = |m: u16| format!("{:02}:{:02}", m / 60, m % 60);
    format!(
        "{} {}–{}, this computer's time (UTC{})",
        if days.is_empty() {
            "no days".to_string()
        } else {
            days.join("/")
        },
        hhmm(s.window_start_minute),
        hhmm(s.window_end_minute),
        now.format("%:z")
    )
}
