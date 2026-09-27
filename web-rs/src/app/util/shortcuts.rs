//! Global keyboard shortcuts: which key does what, and when a key is left alone.
//!
//! Only reads and navigation are bound. No key reaches a device action, a
//! dispatch, an export or sign-out — a stray keystroke in an ops console must never
//! be the thing that patched or rebooted a server.

use super::super::Tab;
use super::TAB_ORDER;

/// What a shortcut asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shortcut {
    RunQuery,
    ShowTab(Tab),
    FocusSearch,
    PrevPage,
    NextPage,
    ToggleAutoRefresh,
    ShowHelp,
}

/// Everything about a key press other than the key itself.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct KeyContext {
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    /// Held down: the browser's auto-repeat. Ignored, so holding `r` is one run.
    pub repeat: bool,
    /// Focus is in an input, textarea, select or contenteditable element.
    pub editing: bool,
    /// A modal dialog (`aria-modal="true"`) is on screen.
    pub modal_open: bool,
}

/// The shortcut `key` (a `KeyboardEvent.key` value) triggers, if any.
///
/// Nothing fires while typing, while a dialog owns the keyboard, on auto-repeat,
/// or with Ctrl/Alt/Meta held — those chords belong to the browser and the OS
/// (copy, find, tab switching). Shift is allowed: `?` *is* Shift+/ on most layouts.
pub(crate) fn shortcut_for(key: &str, cx: KeyContext) -> Option<Shortcut> {
    if cx.ctrl || cx.alt || cx.meta || cx.repeat || cx.editing || cx.modal_open {
        return None;
    }
    match key {
        "r" | "R" => Some(Shortcut::RunQuery),
        "/" => Some(Shortcut::FocusSearch),
        "[" => Some(Shortcut::PrevPage),
        "]" => Some(Shortcut::NextPage),
        "p" | "P" => Some(Shortcut::ToggleAutoRefresh),
        "?" => Some(Shortcut::ShowHelp),
        _ => {
            let n: usize = key.parse().ok()?;
            let tab = TAB_ORDER.get(n.checked_sub(1)?)?;
            Some(Shortcut::ShowTab(*tab))
        }
    }
}

/// Whether an element takes typed text, so a letter key there is input, not a
/// shortcut. A checkbox, radio or button does not: after ticking a row the focus
/// stays on its checkbox, and shortcuts must keep working from there.
pub(crate) fn is_text_entry(tag: &str, input_type: Option<&str>, content_editable: bool) -> bool {
    if content_editable {
        return true;
    }
    match tag.to_ascii_uppercase().as_str() {
        "TEXTAREA" | "SELECT" => true,
        "INPUT" => !input_type.is_some_and(|t| {
            matches!(
                t.to_ascii_lowercase().as_str(),
                "checkbox" | "radio" | "button" | "submit" | "reset"
            )
        }),
        _ => false,
    }
}

/// The help dialog's rows and the README's table, as (keys, what it does).
pub(crate) const SHORTCUT_HELP: [(&str, &str); 7] = [
    ("r", "Run the query"),
    (
        "1 – 6",
        "Switch results tab: Patches, Failures, Compliance, Needs Reboot, Trend, Jobs",
    ),
    ("/", "Jump to the Search filter"),
    ("[  ]", "Previous / next page of the Patches table"),
    ("p", "Pause or resume auto-refresh"),
    ("?", "Show this list"),
    ("Esc", "Close a dialog"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> KeyContext {
        KeyContext::default()
    }

    #[test]
    fn maps_the_documented_keys() {
        assert_eq!(shortcut_for("r", plain()), Some(Shortcut::RunQuery));
        assert_eq!(shortcut_for("R", plain()), Some(Shortcut::RunQuery));
        assert_eq!(shortcut_for("/", plain()), Some(Shortcut::FocusSearch));
        assert_eq!(shortcut_for("[", plain()), Some(Shortcut::PrevPage));
        assert_eq!(shortcut_for("]", plain()), Some(Shortcut::NextPage));
        assert_eq!(
            shortcut_for("p", plain()),
            Some(Shortcut::ToggleAutoRefresh)
        );
        assert_eq!(shortcut_for("?", plain()), Some(Shortcut::ShowHelp));
    }

    #[test]
    fn number_keys_follow_the_on_screen_tab_order() {
        for (i, tab) in TAB_ORDER.iter().enumerate() {
            let key = (i + 1).to_string();
            assert_eq!(shortcut_for(&key, plain()), Some(Shortcut::ShowTab(*tab)));
        }
        assert_eq!(shortcut_for("0", plain()), None);
        let past_end = (TAB_ORDER.len() + 1).to_string();
        assert_eq!(shortcut_for(&past_end, plain()), None);
    }

    #[test]
    fn stands_down_while_typing_in_a_dialog_or_under_a_chord() {
        for cx in [
            KeyContext {
                editing: true,
                ..plain()
            },
            KeyContext {
                modal_open: true,
                ..plain()
            },
            KeyContext {
                ctrl: true,
                ..plain()
            },
            KeyContext {
                alt: true,
                ..plain()
            },
            KeyContext {
                meta: true,
                ..plain()
            },
            KeyContext {
                repeat: true,
                ..plain()
            },
        ] {
            assert_eq!(shortcut_for("r", cx), None, "{cx:?}");
            assert_eq!(shortcut_for("1", cx), None, "{cx:?}");
            assert_eq!(shortcut_for("?", cx), None, "{cx:?}");
        }
    }

    #[test]
    fn text_fields_take_keys_but_toggles_do_not() {
        assert!(is_text_entry("INPUT", None, false));
        assert!(is_text_entry("input", Some("search"), false));
        assert!(is_text_entry("INPUT", Some("number"), false));
        assert!(is_text_entry("TEXTAREA", None, false));
        assert!(is_text_entry("SELECT", None, false));
        assert!(is_text_entry("DIV", None, true));
        assert!(!is_text_entry("INPUT", Some("checkbox"), false));
        assert!(!is_text_entry("INPUT", Some("Radio"), false));
        assert!(!is_text_entry("BUTTON", None, false));
        assert!(!is_text_entry("BODY", None, false));
    }

    #[test]
    fn unbound_keys_are_left_to_the_browser() {
        for key in ["a", "Enter", " ", "Tab", "Escape", "ArrowDown", "F5", "-1"] {
            assert_eq!(shortcut_for(key, plain()), None, "{key}");
        }
    }

    /// The help table is the documentation; every row but Esc (handled by each
    /// dialog itself) must name a key that actually does something.
    #[test]
    fn every_documented_key_is_bound() {
        assert_eq!(
            SHORTCUT_HELP[1].0,
            format!("1 – {}", TAB_ORDER.len()),
            "the tab row must name the real tab count"
        );
        for (keys, _) in SHORTCUT_HELP {
            if keys == "Esc" {
                continue;
            }
            for key in keys.split_whitespace().filter(|k| *k != "–") {
                assert!(
                    shortcut_for(key, plain()).is_some(),
                    "documented key {key:?} is not bound"
                );
            }
        }
    }
}
