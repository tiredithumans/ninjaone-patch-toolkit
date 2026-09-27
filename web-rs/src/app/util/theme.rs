//! The colour theme choice. `System` follows `prefers-color-scheme`; the other two
//! pin a palette through `<html data-theme="…">`, which `styles.css` reads.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub(crate) const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    /// The stored value. Anything unrecognised is `System` — the one choice that
    /// can never leave the app unreadable against the OS.
    pub(crate) fn from_pref(stored: Option<&str>) -> Self {
        match stored {
            Some("light") => Self::Light,
            Some("dark") => Self::Dark,
            _ => Self::System,
        }
    }

    pub(crate) fn pref_value(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// The `data-theme` attribute to set, or `None` to remove it so the media
    /// query decides.
    pub(crate) fn attr(self) -> Option<&'static str> {
        match self {
            Self::System => None,
            Self::Light => Some("light"),
            Self::Dark => Some("dark"),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSS: &str = include_str!("../../../styles.css");

    #[test]
    fn round_trips_every_choice_and_defaults_to_system() {
        for t in Theme::ALL {
            assert_eq!(Theme::from_pref(Some(t.pref_value())), t);
        }
        assert_eq!(Theme::from_pref(None), Theme::System);
        assert_eq!(Theme::from_pref(Some("DARK")), Theme::System);
        assert_eq!(Theme::System.attr(), None);
    }

    /// The custom properties declared in the first block that starts with
    /// `selector`.
    fn tokens_in(selector: &str) -> Vec<String> {
        let (_, rest) = CSS
            .split_once(selector)
            .unwrap_or_else(|| panic!("no `{selector}` block in styles.css"));
        let (body, _) = rest.split_once('}').expect("unterminated block");
        body.lines()
            .filter_map(|l| l.trim().strip_prefix("--"))
            .filter_map(|l| l.split_once(':').map(|(name, _)| format!("--{name}")))
            .collect()
    }

    /// Each light block must restate every token the dark `:root` defines. A token
    /// left out keeps its dark value — a pale severity tint on a white page is the
    /// unreadable badge this guards against.
    #[test]
    fn the_light_palette_overrides_every_root_token() {
        let root = tokens_in(":root {");
        assert!(root.iter().any(|t| t == "--sev-critical"));
        for selector in [
            ":root:not([data-theme=\"dark\"]) {",
            ":root[data-theme=\"light\"] {",
        ] {
            let light = tokens_in(selector);
            for token in &root {
                assert!(
                    light.contains(token),
                    "`{selector}` does not override {token}"
                );
            }
        }
    }

    /// The system-light block must sit inside the media query, or it would apply
    /// to every System user regardless of their OS setting.
    #[test]
    fn the_system_light_block_is_gated_on_the_media_query() {
        let (before, _) = CSS
            .split_once(":root:not([data-theme=\"dark\"]) {")
            .expect("system-light block");
        let media = before
            .rfind("@media (prefers-color-scheme: light)")
            .expect("media query");
        assert!(!before[media..].contains('}'), "block escaped its @media");
    }

    #[test]
    fn reduced_motion_is_honoured() {
        assert!(CSS.contains("@media (prefers-reduced-motion: reduce)"));
    }
}
