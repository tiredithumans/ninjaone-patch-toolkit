//! What the results area says before there is anything to show: the desktop app
//! has no fleet until it is connected and signed in, and an empty tab bar gave no
//! hint of either step.

/// The step a desktop operator is missing before a query can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Onboarding {
    /// No client id saved: Settings is the first stop.
    Connect,
    /// Configured but signed out.
    SignIn,
}

/// Which onboarding card to show, if any.
///
/// Nothing until the saved settings have loaded: the form starts on defaults (no
/// client id), so deciding earlier flashed "Connect to NinjaOne" on every launch.
/// Nothing until the first sign-in check has answered, for the same reason: "not
/// signed in" was assumed while it ran, which flashed "Sign in to load your fleet"
/// at every launch of an app that was signed in all along.
/// Nothing in the web demo either, which has no settings or sign-in, and nothing
/// once signed in — the tabs' own empty states take over from there.
pub(crate) fn onboarding(
    web_mode: bool,
    settings_loaded: bool,
    auth_checked: bool,
    has_client_id: bool,
    authed: bool,
) -> Option<Onboarding> {
    if web_mode || !settings_loaded || !auth_checked || authed {
        return None;
    }
    Some(if has_client_id {
        Onboarding::SignIn
    } else {
        Onboarding::Connect
    })
}
