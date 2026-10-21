//! The "Run as" choices: friendly labels for NinjaOne's `runAs` values and the
//! option lists the action bar and Settings render from them.
//!
//! The vendor spec describes `runAs` only as "Credential role/identifier". The role
//! strings below, and a stored credential's id as a string (`"26"`), are the ones
//! the community NinjaOne PowerShell module documents
//! (<https://docs.homotechsual.dev/modules/ninjaone/commandlets/invoke/script-or-action/>).
//! Values are sent verbatim and are case-sensitive, so a label only ever matches
//! the exact spelling.

use crate::types::RunAsCredential;

/// The Windows roles Settings offers as the default. Settings has no device to ask,
/// so it offers the roles every Windows device has; a specific stored credential is
/// picked per dispatch in the action bar, from what the device reports.
pub(crate) const SETTINGS_RUN_AS_ROLES: [&str; 4] = [
    "system",
    "loggedonuser",
    "SR_LOCAL_ADMINISTRATOR",
    "SR_DOMAIN_ADMINISTRATOR",
];

/// One `<option>`: the `runAs` value sent and the text shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunAsChoice {
    pub(crate) value: String,
    pub(crate) label: String,
}

impl RunAsChoice {
    fn of(value: &str) -> Self {
        Self {
            value: value.to_string(),
            label: run_as_label(value),
        }
    }
}

/// What a `runAs` value means, in the words NinjaOne's own Run-as menu uses. An
/// unknown role shows as sent, so nothing is renamed into something it isn't.
pub(crate) fn run_as_label(value: &str) -> String {
    match value {
        "system" => "System".into(),
        "loggedonuser" => "Logged-on user".into(),
        "SR_LOCAL_ADMINISTRATOR" => "Preferred Windows local admin".into(),
        "SR_DOMAIN_ADMINISTRATOR" => "Preferred Windows domain admin".into(),
        "SR_MAC_SCRIPT" => "Preferred Mac script credential".into(),
        "SR_LINUX_SCRIPT" => "Preferred Linux script credential".into(),
        v if is_credential_id(v) => format!("Credential #{v}"),
        v => v.to_string(),
    }
}

/// A stored credential is chosen by its id, so an all-digit value is one.
fn is_credential_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

/// The Settings default's choices: the Windows roles, plus `current` when it is
/// anything else (a credential id or a role typed before this was a menu), so
/// opening Settings never silently swaps the saved value for the first option.
pub(crate) fn settings_run_as_choices(current: &str) -> Vec<RunAsChoice> {
    let mut out: Vec<RunAsChoice> = SETTINGS_RUN_AS_ROLES
        .iter()
        .map(|r| RunAsChoice::of(r))
        .collect();
    let current = current.trim();
    if !current.is_empty() && !SETTINGS_RUN_AS_ROLES.contains(&current) {
        out.push(RunAsChoice::of(current));
    }
    out
}

/// The action bar's choices besides "Settings default": the sampled device's roles,
/// and its stored credentials by name (value = the id as a string).
///
/// `current` (the per-dispatch value, which a retry can set) is kept as an extra
/// role entry when neither list has it — a closed menu without it would show the
/// first option while sending the old value.
pub(crate) fn action_run_as_choices(
    roles: &[String],
    credentials: &[RunAsCredential],
    current: &str,
) -> (Vec<RunAsChoice>, Vec<RunAsChoice>) {
    let mut role_choices: Vec<RunAsChoice> = Vec::with_capacity(roles.len() + 1);
    for role in roles {
        if !role.is_empty() && !role_choices.iter().any(|c| &c.value == role) {
            role_choices.push(RunAsChoice::of(role));
        }
    }
    let credential_choices: Vec<RunAsChoice> = credentials
        .iter()
        .map(|c| RunAsChoice {
            value: c.id.to_string(),
            label: c.name.clone(),
        })
        .collect();
    let listed = |v: &str| {
        role_choices
            .iter()
            .chain(&credential_choices)
            .any(|c| c.value == v)
    };
    if !current.is_empty() && !listed(current) {
        role_choices.push(RunAsChoice::of(current));
    }
    (role_choices, credential_choices)
}

/// The blank option's text: blank sends no override, so the backend uses the
/// Settings default — say which one.
pub(crate) fn run_as_default_label(settings_value: &str) -> String {
    let v = settings_value.trim();
    let v = if v.is_empty() { "system" } else { v };
    format!("Settings default ({})", run_as_label(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(id: i64, name: &str) -> RunAsCredential {
        RunAsCredential {
            id,
            name: name.into(),
        }
    }

    #[test]
    fn known_roles_get_menu_labels_and_unknown_ones_show_as_sent() {
        assert_eq!(run_as_label("system"), "System");
        assert_eq!(run_as_label("loggedonuser"), "Logged-on user");
        assert_eq!(
            run_as_label("SR_LOCAL_ADMINISTRATOR"),
            "Preferred Windows local admin"
        );
        assert_eq!(
            run_as_label("SR_DOMAIN_ADMINISTRATOR"),
            "Preferred Windows domain admin"
        );
        assert_eq!(
            run_as_label("SR_MAC_SCRIPT"),
            "Preferred Mac script credential"
        );
        assert_eq!(
            run_as_label("SR_LINUX_SCRIPT"),
            "Preferred Linux script credential"
        );
        assert_eq!(run_as_label("26"), "Credential #26");
        assert_eq!(run_as_label("SR_SOMETHING_NEW"), "SR_SOMETHING_NEW");
    }

    /// `runAs` is case-sensitive: a label must not make a wrong spelling look like
    /// the role NinjaOne accepts.
    #[test]
    fn labels_match_only_the_exact_spelling() {
        assert_eq!(run_as_label("SYSTEM"), "SYSTEM");
        assert_eq!(run_as_label("LoggedOnUser"), "LoggedOnUser");
    }

    #[test]
    fn settings_offers_the_windows_roles_and_keeps_any_other_saved_value() {
        let values = |cur: &str| -> Vec<String> {
            settings_run_as_choices(cur)
                .into_iter()
                .map(|c| c.value)
                .collect()
        };
        assert_eq!(values("system"), SETTINGS_RUN_AS_ROLES);
        assert_eq!(values(""), SETTINGS_RUN_AS_ROLES);
        let kept = settings_run_as_choices("26");
        assert_eq!(kept.len(), 5);
        assert_eq!(
            kept.last(),
            Some(&RunAsChoice {
                value: "26".into(),
                label: "Credential #26".into()
            })
        );
    }

    #[test]
    fn action_bar_lists_roles_then_credentials_by_name() {
        let roles = vec!["system".to_string(), "loggedonuser".to_string()];
        let (r, c) = action_run_as_choices(&roles, &[cred(26, "Patching admin")], "");
        assert_eq!(
            r.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
            ["System", "Logged-on user"]
        );
        assert_eq!(
            c,
            [RunAsChoice {
                value: "26".into(),
                label: "Patching admin".into()
            }]
        );
    }

    /// A retry restores the run-as it was sent with; a menu without that value would
    /// display the first option while still sending the old one.
    #[test]
    fn action_bar_keeps_a_current_value_the_device_did_not_list() {
        let roles = vec!["system".to_string()];
        let (r, c) = action_run_as_choices(&roles, &[cred(26, "Patching admin")], "31");
        assert_eq!(r.last().map(|c| c.value.as_str()), Some("31"));
        assert_eq!(r.last().map(|c| c.label.as_str()), Some("Credential #31"));
        assert_eq!(c.len(), 1);

        // Already listed (as a role or as a credential): not repeated.
        let (r, _) = action_run_as_choices(&roles, &[cred(26, "Patching admin")], "26");
        assert_eq!(r.len(), 1);
        let (r, _) = action_run_as_choices(&roles, &[], "system");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn action_bar_drops_blank_and_repeated_roles() {
        let roles = vec!["system".to_string(), String::new(), "system".to_string()];
        let (r, _) = action_run_as_choices(&roles, &[], "");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn the_blank_option_names_the_settings_default() {
        assert_eq!(run_as_default_label("system"), "Settings default (System)");
        assert_eq!(run_as_default_label(""), "Settings default (System)");
        assert_eq!(
            run_as_default_label("26"),
            "Settings default (Credential #26)"
        );
    }
}
