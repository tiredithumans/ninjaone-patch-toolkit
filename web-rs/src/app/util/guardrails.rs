//! The dispatch guardrails the operator configures or trips over: the maintenance
//! window (its editor and the per-dispatch override), whether a dry run is honest
//! for the script it would reach, and what a native "Apply all" will install.
//!
//! The backend's `actions::plan` enforces every one of these; this only decides
//! what the UI offers and how it explains itself.

use crate::types::{ActionKind, ActionSettings, ApplyPreview, ApplyPreviewDevice};

use super::group_thousands;

/// Day names indexed like `ActionSettings::window_days` (`0` = Sunday), matching
/// the backend's `window_label`.
pub(crate) const WINDOW_DAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Minutes past midnight as the `HH:MM` an `<input type="time">` takes.
pub(crate) fn minutes_to_hhmm(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// An `<input type="time">` value (`HH:MM`, or `HH:MM:SS` when the browser adds
/// seconds) as minutes past midnight. `None` for an empty or malformed value, so
/// clearing the field keeps the stored time instead of snapping it to midnight.
pub(crate) fn parse_hhmm(value: &str) -> Option<u16> {
    let mut parts = value.trim().split(':');
    let hours: u16 = parts.next()?.parse().ok()?;
    let minutes: u16 = parts.next()?.parse().ok()?;
    (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

/// `days` with `day` switched on or off, sorted and de-duplicated — the canonical
/// form the backend stores, so the editor never shows an order the saved file won't.
pub(crate) fn toggle_window_day(days: &[u8], day: u8, on: bool) -> Vec<u8> {
    let mut out: Vec<u8> = days.iter().copied().filter(|d| *d != day).collect();
    if on {
        out.push(day);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The window in the backend blocker's own words (`Mon/Tue 02:00–05:00`), so the
/// Settings panel, the override checkbox and the blocker describe it identically.
pub(crate) fn window_summary(a: &ActionSettings) -> String {
    let mut days: Vec<u8> = a.window_days.clone();
    days.sort_unstable();
    days.dedup();
    let names: Vec<&str> = days
        .iter()
        .filter_map(|d| WINDOW_DAY_NAMES.get(*d as usize).copied())
        .collect();
    let wraps = a.window_end_minute < a.window_start_minute;
    format!(
        "{} {}–{}{}",
        if names.is_empty() {
            "no days".to_string()
        } else {
            names.join("/")
        },
        minutes_to_hhmm(a.window_start_minute),
        minutes_to_hhmm(a.window_end_minute),
        if wraps { " (wraps past midnight)" } else { "" }
    )
}

/// Why the window as edited would be refused on save, mirroring the backend's
/// `validate_action_settings`, so the panel says it before the Save round trip.
pub(crate) fn window_settings_problem(a: &ActionSettings) -> Option<&'static str> {
    if a.window_start_minute == a.window_end_minute {
        return Some("The window opens and closes at the same time, so it would never be open.");
    }
    if a.require_maintenance_window && a.window_days.is_empty() {
        return Some("Pick at least one day — a window with no days blocks every change.");
    }
    None
}

/// Whether the action bar offers the per-dispatch override: the window is enforced
/// and Settings permits overriding it.
///
/// Deliberately not "…and the window is closed right now": that would mean
/// re-implementing `window_is_open` against the webview's clock, and two copies of
/// a boundary check disagree exactly at the boundary. The backend decides; an
/// override requested while the window is open is inert and is not audited as one.
pub(crate) fn window_override_offered(a: &ActionSettings) -> bool {
    a.enabled && a.require_maintenance_window && a.allow_window_override
}

/// Why `kind` cannot be dispatched as a dry run, if the UI can already tell.
///
/// A dry run only appends `dryRun=true` to the composed parameters; a script that
/// never reads it runs for real, and a hand-typed string is sent verbatim with no
/// flag added. `script_declares` is `None` while the library is unknown (not loaded,
/// or the id is not in it) — the backend decides those, so the button stays live.
pub(crate) fn dry_run_disabled_reason(
    kind: ActionKind,
    dry_run: bool,
    script_declares: Option<bool>,
    typed_parameters: bool,
) -> Option<String> {
    if !dry_run || !kind.runs_a_script() {
        return None;
    }
    if kind == ActionKind::Script && typed_parameters {
        return Some(
            "Dry run is on, but typed parameters are sent verbatim — the toolkit cannot add \
             dryRun=true to them, so the script would run for real. Clear the Parameters box or \
             turn off Dry run."
                .to_string(),
        );
    }
    (script_declares == Some(false)).then(|| {
        "Dry run is on, but this script declares no dryRun variable, so it would run for real. \
         Turn off Dry run to send it for real."
            .to_string()
    })
}

/// The Dry run checkbox's caveat: which of the scripts it reaches cannot preview.
///
/// `scripts` is (what the script is, whether it declares `dryRun`) for each one the
/// checkbox currently reaches; unknown entries are left out rather than guessed.
pub(crate) fn dry_run_caveat(scripts: &[(String, Option<bool>)]) -> Option<String> {
    let cannot: Vec<&str> = scripts
        .iter()
        .filter(|(_, declares)| *declares == Some(false))
        .map(|(name, _)| name.as_str())
        .collect();
    (!cannot.is_empty()).then(|| {
        format!(
            "Can't preview (no dryRun variable): {}. Their actions are disabled while Dry run is on.",
            cannot.join(", ")
        )
    })
}

/// The one-line summary of what "Apply all" will install, shown on the confirm
/// dialog's collapsible preview.
pub(crate) fn apply_preview_summary(p: &ApplyPreview) -> String {
    if !p.known {
        return format!(
            "Approved {} patches per device: unknown (patch data not loaded). Run a query that \
             includes {} patches to see what will install.",
            p.family, p.family
        );
    }
    let mut out = format!(
        "NinjaOne will install {} approved {} patch(es) across {} device(s)",
        group_thousands(p.approved_total),
        p.family,
        p.devices.len()
    );
    if p.pending_manual_total > 0 {
        out.push_str(&format!(
            "; {} more pending approval will not install",
            group_thousands(p.pending_manual_total)
        ));
    }
    out.push('.');
    if let Some(at) = &p.data_fetched_at {
        out.push_str(&format!(" Per patch data fetched {at}."));
    }
    out
}

/// One device's line in the preview.
pub(crate) fn apply_preview_line(d: &ApplyPreviewDevice) -> String {
    let will = if d.approved == 0 {
        "nothing approved — nothing will install".to_string()
    } else {
        format!("{} approved will install", group_thousands(d.approved))
    };
    if d.pending_manual == 0 {
        format!("{} — {will}", d.device_name)
    } else {
        format!(
            "{} — {will}; {} pending approval",
            d.device_name,
            group_thousands(d.pending_manual)
        )
    }
}

/// The audit trail's Mode cell. An override of a closed maintenance window is the
/// one live dispatch the trail must make stand out.
pub(crate) fn audit_mode_label(dry_run: bool, window_override: bool) -> &'static str {
    match (dry_run, window_override) {
        (true, _) => "Dry run",
        (false, true) => "Live (window override)",
        (false, false) => "Live",
    }
}
