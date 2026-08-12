//! Whether the terminal actually delivers `Alt` to the application.
//!
//! On macOS the Option key is, by default, a *compose* key: the terminal turns
//! `Option+E` into an accent dead-key rather than sending `Alt+E`. Whether the
//! application ever sees an Alt modifier is therefore a per-terminal setting
//! (Alacritty `window.option_as_alt`, Ghostty `macos-option-as-alt`, iTerm2
//! "Esc+", Terminal.app "Use Option as Meta Key").
//!
//! This matters because it is *not* a conflict, yet it has exactly the same
//! symptom: with the default setting every `alt+` binding jcode has is silently
//! dead, no other app is intercepting anything, and the conflict report happily
//! says "no conflicts found". Detecting the setting lets jcode explain the whole
//! class of "my keybinding does nothing" reports instead of looking clean while
//! the key never arrives.
//!
//! The parsers here are pure and unit-tested; only [`detect_alt_delivery`]
//! touches the machine.

use serde::{Deserialize, Serialize};

/// How much of the Option/Alt key reaches the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AltDelivery {
    /// Could not determine the terminal's setting; say nothing rather than
    /// guess. This is also what every pre-existing snapshot deserializes to.
    #[default]
    Unknown,
    /// Alt reaches the application (either a non-macOS terminal, or Option was
    /// explicitly configured as Alt/Esc+).
    Delivered,
    /// Only the left Option key is sent as Alt; the right one composes.
    LeftOnly,
    /// Only the right Option key is sent as Alt; the left one composes.
    RightOnly,
    /// Option never arrives as Alt, so every `alt+` binding is dead.
    Never,
}

impl AltDelivery {
    /// Whether this state leaves at least one `alt+` chord unreachable, and so
    /// is worth telling the user about.
    pub fn is_degraded(self) -> bool {
        matches!(self, Self::Never | Self::LeftOnly | Self::RightOnly)
    }

    /// Stable token for the hint-debounce signature.
    pub fn token(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Delivered => "delivered",
            Self::LeftOnly => "left_only",
            Self::RightOnly => "right_only",
            Self::Never => "never",
        }
    }
}

/// Read `window.option_as_alt` out of an `alacritty.toml`.
///
/// Documented values are `"None"` (the default), `"OnlyLeft"`, `"OnlyRight"`
/// and `"Both"`. A config that does not set the key is the default, so absence
/// is reported as [`AltDelivery::Never`] rather than `Unknown`: the value is
/// known precisely *because* Alacritty compiles the default in.
pub fn parse_alacritty_option_as_alt(text: &str) -> AltDelivery {
    let Ok(value) = text.parse::<toml::Value>() else {
        return AltDelivery::Unknown;
    };
    let raw = value
        .get("window")
        .and_then(|w| w.get("option_as_alt"))
        .and_then(|v| v.as_str())
        .unwrap_or("None");
    match raw.to_ascii_lowercase().as_str() {
        "both" => AltDelivery::Delivered,
        "onlyleft" => AltDelivery::LeftOnly,
        "onlyright" => AltDelivery::RightOnly,
        "none" => AltDelivery::Never,
        _ => AltDelivery::Unknown,
    }
}

/// Read `macos-option-as-alt` out of `ghostty +show-config` output.
///
/// `+show-config` prints the *effective* configuration, defaults included, so
/// unlike Alacritty there is no default to hardcode here: a missing key means
/// this Ghostty build does not have the option (non-macOS), not that it is off.
/// Documented values are `true`/`false`/`left`/`right`.
pub fn parse_ghostty_option_as_alt(output: &str) -> AltDelivery {
    for line in output.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("macos-option-as-alt") else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        return match value.trim().to_ascii_lowercase().as_str() {
            "true" => AltDelivery::Delivered,
            "false" => AltDelivery::Never,
            "left" => AltDelivery::LeftOnly,
            "right" => AltDelivery::RightOnly,
            _ => AltDelivery::Unknown,
        };
    }
    AltDelivery::Unknown
}

/// Determine how the current terminal treats Option/Alt.
///
/// Only macOS has the compose-vs-Alt ambiguity: on Linux, BSD and Windows the
/// Alt modifier is delivered as `ESC`-prefixed input by every terminal jcode
/// supports, so the answer is unconditionally [`AltDelivery::Delivered`].
#[cfg(not(target_os = "macos"))]
pub fn detect_alt_delivery(_terminal: &str) -> AltDelivery {
    AltDelivery::Delivered
}

/// macOS implementation: consult whichever terminal is actually in use.
/// Terminals whose setting jcode cannot read yet (iTerm2, Terminal.app,
/// WezTerm, kitty) report [`AltDelivery::Unknown`], which is rendered as
/// silence rather than as a guess.
#[cfg(target_os = "macos")]
pub fn detect_alt_delivery(terminal: &str) -> AltDelivery {
    match terminal {
        "Alacritty" => alacritty_alt_delivery(),
        "Ghostty" => ghostty_alt_delivery(),
        _ => AltDelivery::Unknown,
    }
}

#[cfg(any(test, target_os = "macos"))]
fn alacritty_alt_delivery() -> AltDelivery {
    if std::env::var_os("ALACRITTY_WINDOW_ID").is_none() {
        return AltDelivery::Unknown;
    }
    match super::terminal::read_alacritty_config() {
        // No config file at all still means the compiled-in default applies.
        None => AltDelivery::Never,
        Some(text) => parse_alacritty_option_as_alt(&text),
    }
}

#[cfg(any(test, target_os = "macos"))]
fn ghostty_alt_delivery() -> AltDelivery {
    use std::process::Command;
    const CANDIDATES: [&str; 2] = [
        "/Applications/Ghostty.app/Contents/MacOS/ghostty",
        "ghostty",
    ];
    for bin in CANDIDATES {
        let Ok(output) = Command::new(bin).arg("+show-config").output() else {
            continue;
        };
        if output.status.success() && !output.stdout.is_empty() {
            return parse_ghostty_option_as_alt(&String::from_utf8_lossy(&output.stdout));
        }
    }
    AltDelivery::Unknown
}

/// A short status-line phrasing for a degraded state. Distinct from
/// [`explain`]: the one-liner must stay accurate for the side-specific cases,
/// where saying "does not send Option as Alt" would be plainly false.
pub fn status_phrase(delivery: AltDelivery, terminal: &str) -> Option<String> {
    match delivery {
        AltDelivery::Never => Some(format!(
            "{terminal} does not send Option as Alt, so your `alt+` keybindings never arrive. Run /keys for details."
        )),
        AltDelivery::LeftOnly => Some(format!(
            "{terminal} sends only the left Option key as Alt, so `alt+` keybindings work only from the left. Run /keys for details."
        )),
        AltDelivery::RightOnly => Some(format!(
            "{terminal} sends only the right Option key as Alt, so `alt+` keybindings work only from the right. Run /keys for details."
        )),
        AltDelivery::Unknown | AltDelivery::Delivered => None,
    }
}

/// The user-facing explanation of a degraded state, or `None` when Alt is fine
/// or unknown. `terminal` is the detected terminal label.
pub fn explain(delivery: AltDelivery, terminal: &str) -> Option<String> {
    let setting = match terminal {
        "Alacritty" => "`window.option_as_alt` in alacritty.toml",
        "Ghostty" => "`macos-option-as-alt` in your Ghostty config",
        _ => "your terminal's Option-key setting",
    };
    match delivery {
        AltDelivery::Never => Some(format!(
            "{terminal} never sends Option as Alt, so every `alt+` binding is dead\n\
             regardless of conflicts.\nFix: set {setting} to send Alt."
        )),
        AltDelivery::LeftOnly => Some(format!(
            "{terminal} sends only the LEFT Option key as Alt; the right Option key\n\
             composes instead, so `alt+` chords work only from the left.\n\
             Setting: {setting}."
        )),
        AltDelivery::RightOnly => Some(format!(
            "{terminal} sends only the RIGHT Option key as Alt; the left Option key\n\
             composes instead, so `alt+` chords work only from the right.\n\
             Setting: {setting}."
        )),
        AltDelivery::Unknown | AltDelivery::Delivered => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alacritty_default_is_never() {
        // The key point of the whole module: an Alacritty user who never touched
        // this setting has NO working alt+ bindings, and nothing in the config
        // file says so, because the default is compiled in.
        assert_eq!(parse_alacritty_option_as_alt(""), AltDelivery::Never);
        assert_eq!(
            parse_alacritty_option_as_alt("[font]\nsize = 12\n"),
            AltDelivery::Never
        );
    }

    #[test]
    fn alacritty_explicit_values_are_read() {
        let cases = [
            ("Both", AltDelivery::Delivered),
            ("OnlyLeft", AltDelivery::LeftOnly),
            ("OnlyRight", AltDelivery::RightOnly),
            ("None", AltDelivery::Never),
        ];
        for (raw, expected) in cases {
            let cfg = format!("[window]\noption_as_alt = \"{raw}\"\n");
            assert_eq!(parse_alacritty_option_as_alt(&cfg), expected, "for {raw}");
        }
    }

    #[test]
    fn alacritty_malformed_config_is_unknown_not_never() {
        // A broken config must not be reported as a confident "Alt is dead":
        // we genuinely do not know what Alacritty loaded.
        assert_eq!(
            parse_alacritty_option_as_alt("not toml {{{"),
            AltDelivery::Unknown
        );
    }

    #[test]
    fn ghostty_effective_config_is_read() {
        let out = "font-size = 13\nmacos-option-as-alt = true\ntheme = dark\n";
        assert_eq!(parse_ghostty_option_as_alt(out), AltDelivery::Delivered);
        assert_eq!(
            parse_ghostty_option_as_alt("macos-option-as-alt = false"),
            AltDelivery::Never
        );
        assert_eq!(
            parse_ghostty_option_as_alt("macos-option-as-alt = left"),
            AltDelivery::LeftOnly
        );
        assert_eq!(
            parse_ghostty_option_as_alt("macos-option-as-alt = right"),
            AltDelivery::RightOnly
        );
    }

    #[test]
    fn ghostty_missing_key_is_unknown() {
        // A Linux Ghostty build has no such option; absence is not "off".
        assert_eq!(
            parse_ghostty_option_as_alt("font-size = 13\n"),
            AltDelivery::Unknown
        );
        // A near-miss key name must not be matched.
        assert_eq!(
            parse_ghostty_option_as_alt("macos-option-as-alt-other = true\n"),
            AltDelivery::Unknown
        );
    }

    #[test]
    fn status_phrase_does_not_claim_alt_is_dead_for_side_specific_states() {
        // Regression: the one-liner used to say "does not send Option as Alt"
        // for OnlyLeft/OnlyRight, which is false and sends the user hunting for
        // the wrong problem.
        let left = status_phrase(AltDelivery::LeftOnly, "Alacritty").unwrap();
        assert!(left.contains("only the left"), "got {left}");
        assert!(!left.contains("does not send Option as Alt"), "got {left}");

        let right = status_phrase(AltDelivery::RightOnly, "Ghostty").unwrap();
        assert!(right.contains("only the right"), "got {right}");
        assert!(
            !right.contains("does not send Option as Alt"),
            "got {right}"
        );

        let never = status_phrase(AltDelivery::Never, "Alacritty").unwrap();
        assert!(never.contains("does not send Option as Alt"), "got {never}");

        assert!(status_phrase(AltDelivery::Delivered, "Ghostty").is_none());
        assert!(status_phrase(AltDelivery::Unknown, "iTerm2").is_none());
    }

    #[test]
    fn only_degraded_states_are_explained() {
        assert!(explain(AltDelivery::Unknown, "iTerm2").is_none());
        assert!(explain(AltDelivery::Delivered, "Ghostty").is_none());
        let never = explain(AltDelivery::Never, "Alacritty").unwrap();
        assert!(never.contains("option_as_alt"), "got {never}");
        assert!(
            explain(AltDelivery::LeftOnly, "Ghostty")
                .unwrap()
                .contains("LEFT")
        );
        assert!(
            explain(AltDelivery::RightOnly, "Ghostty")
                .unwrap()
                .contains("RIGHT")
        );
    }

    #[test]
    fn degraded_predicate_matches_the_states_that_lose_keys() {
        assert!(AltDelivery::Never.is_degraded());
        assert!(AltDelivery::LeftOnly.is_degraded());
        assert!(AltDelivery::RightOnly.is_degraded());
        assert!(!AltDelivery::Delivered.is_degraded());
        assert!(!AltDelivery::Unknown.is_degraded());
    }

    #[test]
    fn unknown_is_the_default_so_old_snapshots_stay_silent() {
        let parsed: AltDelivery = serde_json::from_str("\"unknown\"").unwrap();
        assert_eq!(parsed, AltDelivery::Unknown);
        assert_eq!(AltDelivery::default(), AltDelivery::Unknown);
    }
}
