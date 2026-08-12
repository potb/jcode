//! Where a discovered key binding came from, and the binding record itself.

use serde::{Deserialize, Serialize};

use super::chord::KeyChord;

/// Which physical Alt/Option key a discovered binding is bound to.
///
/// [`KeyChord`] deliberately has no sidedness: it is the comparison key of the
/// conflict index, and the terminal reports plain "alt" regardless of which
/// physical key produced it, so collapsing both sides is the right identity for
/// matching. But the *declaration* often is sided, and that distinction carries
/// real information we would otherwise throw away.
///
/// It matters on layouts where the right Option key is AltGr (qwerty-fr,
/// US-International, most EU layouts). Such users configure their window
/// manager with `lalt` precisely so `ralt` stays free for composing accented
/// characters. If the terminal is in turn configured to send only *one* side as
/// Alt, a binding on the other side can never reach the application as an Alt
/// chord, so pairing it with a jcode `alt+` chord is a false positive: it sends
/// the user hunting for an interception that cannot happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AltSide {
    /// The declaration did not name a side (`alt`, `option`), or the chord has
    /// no Alt modifier at all. Either way, assume it can fire.
    #[default]
    Unspecified,
    /// Bound to the left Option key only (`lalt`, `left_option`).
    Left,
    /// Bound to the right Option key only (`ralt`, `right_option`).
    Right,
}

impl AltSide {
    /// Merge two side observations. A chord that names both sides (`lalt + ralt`)
    /// is effectively side-agnostic, and so is anything mixed with an unsided
    /// spelling.
    pub fn merge(self, other: AltSide) -> AltSide {
        match (self, other) {
            (a, AltSide::Unspecified) => a,
            (AltSide::Unspecified, b) => b,
            (a, b) if a == b => a,
            // Left + Right named together: both physical keys work.
            _ => AltSide::Unspecified,
        }
    }

    /// Human-facing suffix for reports, e.g. " (left Option)".
    pub fn label(self) -> &'static str {
        match self {
            AltSide::Unspecified => "",
            AltSide::Left => " (left Option)",
            AltSide::Right => " (right Option)",
        }
    }
}

/// The origin of a discovered binding on the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    /// A macOS system-wide shortcut (`com.apple.symbolichotkeys`).
    MacosSystem,
    /// A binding declared by the terminal emulator (config or built-in default).
    Terminal,
    /// A binding declared by a third-party app that grabs global hotkeys before
    /// the terminal sees them: window managers (OmniWM, AeroSpace, yabai/skhd),
    /// automation tools (Hammerspoon), launchers (Raycast), etc. The specific
    /// app is named in [`DiscoveredBinding::tool`].
    ExternalApp,
}

impl KeySource {
    pub fn label(self) -> &'static str {
        match self {
            KeySource::MacosSystem => "macOS system shortcut",
            KeySource::Terminal => "terminal",
            KeySource::ExternalApp => "external app",
        }
    }
}

/// A key binding discovered on the machine that may intercept input before it
/// reaches jcode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredBinding {
    /// The normalized chord this binding triggers on.
    pub chord: KeyChord,
    /// Which layer owns this binding.
    pub source: KeySource,
    /// What the binding does, e.g. "Spotlight: Show search" or
    /// "copy_to_clipboard:mixed".
    pub action: String,
    /// The raw declaration we parsed, for debugging (e.g. the original config
    /// line or the symbolic-hotkey id).
    pub raw: String,
    /// For [`KeySource::ExternalApp`], the human-facing name of the app that
    /// owns this binding (e.g. "OmniWM", "AeroSpace", "skhd"). Empty for the
    /// macOS system and terminal sources, where the source label is enough.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    /// Which physical Option key the declaration named, when it named one.
    /// See [`AltSide`]; only meaningful when `chord.alt` is set.
    #[serde(default, skip_serializing_if = "is_unspecified_side")]
    pub alt_side: AltSide,
}

fn is_unspecified_side(side: &AltSide) -> bool {
    matches!(side, AltSide::Unspecified)
}
