//! The auto-refresh countdown and the rule that holds it.
//!
//! The cadence used to be a bare `setInterval`: nothing on screen said when the
//! next refetch would land, nothing could stop it short of switching it Off (and
//! losing the chosen cadence), and a tick fired underneath an open confirmation
//! dialog. The countdown is now driven by a one-second ticker that asks
//! [`advance_refresh`] whether to fire, so the arithmetic — and the order of the
//! hold reasons — is host-tested rather than living in a timer closure.

/// Why the countdown is not running. At most one is reported, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefreshHold {
    /// The operator pressed Pause. Wins over everything: it is the one hold they
    /// chose, so it is the one the label must name.
    Paused,
    /// A dialog is open — the action confirmation, the update splash, the
    /// shortcuts help — or the Settings panel. A refresh landing underneath a
    /// confirmation would re-scope the rows the operator is reading about.
    Dialog,
    /// The window is hidden or minimized: nobody is reading the result.
    Hidden,
    /// A query (manual or automatic) is in flight. The countdown restarts from
    /// the full cadence once it lands, so a manual run also pushes the next
    /// automatic one a whole cadence out.
    Running,
}

/// The single hold that applies, if any. Selection is deliberately *not* a hold:
/// a refresh already prunes the selection to the rows still listed (see
/// `prune_device_selection`), and after a dispatch the selection is still there
/// while the operator watches the patches land — pausing on it would switch the
/// cadence off exactly when it is for.
pub(crate) fn refresh_hold(
    paused: bool,
    dialog_open: bool,
    hidden: bool,
    running: bool,
) -> Option<RefreshHold> {
    if paused {
        Some(RefreshHold::Paused)
    } else if dialog_open {
        Some(RefreshHold::Dialog)
    } else if hidden {
        Some(RefreshHold::Hidden)
    } else if running {
        Some(RefreshHold::Running)
    } else {
        None
    }
}

/// Time left until the next automatic refresh, measured in wall-clock
/// milliseconds between ticks — not by counting ticks, which a webview throttles
/// when it is in the background.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RefreshClock {
    pub remaining_ms: f64,
    pub last_ms: f64,
}

impl RefreshClock {
    /// A full cadence from `now_ms`.
    pub(crate) fn start(cadence_secs: u32, now_ms: f64) -> Self {
        Self {
            remaining_ms: f64::from(cadence_secs) * 1000.0,
            last_ms: now_ms,
        }
    }
}

/// Advances the countdown to `now_ms`. Returns the new clock and whether to fire a
/// refresh now.
///
/// - `Running` resets to a full cadence: the result that lands is the fresh one.
/// - Any other hold freezes the countdown where it was.
/// - A clock that went backwards counts as no time passing, never as a refill.
/// - A remaining time above the cadence (the operator shortened it) is clamped.
/// - Cadence 0 (Off) never fires.
pub(crate) fn advance_refresh(
    clock: RefreshClock,
    cadence_secs: u32,
    now_ms: f64,
    hold: Option<RefreshHold>,
) -> (RefreshClock, bool) {
    let full = f64::from(cadence_secs) * 1000.0;
    if cadence_secs == 0 {
        return (RefreshClock::start(0, now_ms), false);
    }
    match hold {
        Some(RefreshHold::Running) => (RefreshClock::start(cadence_secs, now_ms), false),
        Some(_) => (
            RefreshClock {
                remaining_ms: clock.remaining_ms.min(full),
                last_ms: now_ms,
            },
            false,
        ),
        None => {
            let elapsed = (now_ms - clock.last_ms).max(0.0);
            let remaining = clock.remaining_ms.min(full) - elapsed;
            if remaining <= 0.0 {
                (RefreshClock::start(cadence_secs, now_ms), true)
            } else {
                (
                    RefreshClock {
                        remaining_ms: remaining,
                        last_ms: now_ms,
                    },
                    false,
                )
            }
        }
    }
}

/// The countdown line beside the Auto-refresh control.
pub(crate) fn countdown_label(remaining_ms: f64, hold: Option<RefreshHold>) -> String {
    match hold {
        Some(RefreshHold::Paused) => "Auto-refresh paused".to_string(),
        Some(RefreshHold::Dialog) => "Auto-refresh waits while a dialog is open".to_string(),
        Some(RefreshHold::Hidden) => "Auto-refresh waits while the window is hidden".to_string(),
        Some(RefreshHold::Running) => "Refreshing…".to_string(),
        None => {
            // Rounded up: "in 0s" would be a lie for the whole last second.
            let secs = (remaining_ms.max(0.0) / 1000.0).ceil() as u64;
            if secs >= 60 {
                format!("Next refresh in {}:{:02}", secs / 60, secs % 60)
            } else {
                format!("Next refresh in {secs}s")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_outranks_every_other_hold() {
        assert_eq!(
            refresh_hold(true, true, true, true),
            Some(RefreshHold::Paused)
        );
        assert_eq!(
            refresh_hold(false, true, true, true),
            Some(RefreshHold::Dialog)
        );
        assert_eq!(
            refresh_hold(false, false, true, true),
            Some(RefreshHold::Hidden)
        );
        assert_eq!(
            refresh_hold(false, false, false, true),
            Some(RefreshHold::Running)
        );
        assert_eq!(refresh_hold(false, false, false, false), None);
    }

    #[test]
    fn counts_down_by_wall_time_and_fires_at_zero() {
        let c = RefreshClock::start(60, 1_000.0);
        let (c, fire) = advance_refresh(c, 60, 21_000.0, None);
        assert!(!fire);
        assert_eq!(c.remaining_ms, 40_000.0);
        // A throttled ticker that skipped 40 s still lands exactly on time.
        let (c, fire) = advance_refresh(c, 60, 61_000.0, None);
        assert!(fire);
        assert_eq!(c, RefreshClock::start(60, 61_000.0));
    }

    #[test]
    fn a_hold_freezes_the_countdown_and_does_not_bank_the_held_time() {
        let c = RefreshClock::start(60, 0.0);
        let (c, _) = advance_refresh(c, 60, 10_000.0, None);
        assert_eq!(c.remaining_ms, 50_000.0);
        // Five minutes behind a dialog: nothing elapses, nothing fires.
        let (c, fire) = advance_refresh(c, 60, 310_000.0, Some(RefreshHold::Dialog));
        assert!(!fire);
        assert_eq!(c.remaining_ms, 50_000.0);
        // Released: counting resumes from the release, not from before the hold.
        let (c, fire) = advance_refresh(c, 60, 311_000.0, None);
        assert!(!fire);
        assert_eq!(c.remaining_ms, 49_000.0);
    }

    #[test]
    fn a_run_in_flight_restarts_the_full_cadence() {
        let c = RefreshClock {
            remaining_ms: 3_000.0,
            last_ms: 0.0,
        };
        let (c, fire) = advance_refresh(c, 300, 1_000.0, Some(RefreshHold::Running));
        assert!(!fire);
        assert_eq!(c.remaining_ms, 300_000.0);
    }

    #[test]
    fn a_clock_going_backwards_is_no_time_not_a_refill() {
        let c = RefreshClock {
            remaining_ms: 5_000.0,
            last_ms: 10_000.0,
        };
        let (c, fire) = advance_refresh(c, 60, 4_000.0, None);
        assert!(!fire);
        assert_eq!(c.remaining_ms, 5_000.0);
    }

    #[test]
    fn a_shortened_cadence_clamps_what_is_left() {
        let c = RefreshClock::start(900, 0.0);
        let (c, fire) = advance_refresh(c, 60, 1_000.0, None);
        assert!(!fire);
        assert_eq!(c.remaining_ms, 59_000.0);
    }

    #[test]
    fn off_never_fires() {
        let c = RefreshClock {
            remaining_ms: 0.0,
            last_ms: 0.0,
        };
        let (_, fire) = advance_refresh(c, 0, 1_000_000.0, None);
        assert!(!fire);
    }

    #[test]
    fn countdown_rounds_up_and_switches_to_minutes() {
        assert_eq!(countdown_label(59_001.0, None), "Next refresh in 1:00");
        assert_eq!(countdown_label(59_000.0, None), "Next refresh in 59s");
        assert_eq!(countdown_label(200.0, None), "Next refresh in 1s");
        assert_eq!(countdown_label(-5.0, None), "Next refresh in 0s");
        assert_eq!(countdown_label(245_000.0, None), "Next refresh in 4:05");
        assert_eq!(
            countdown_label(1.0, Some(RefreshHold::Paused)),
            "Auto-refresh paused"
        );
    }
}
