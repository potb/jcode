//! Discover key bindings declared by terminal emulators.
//!
//! Different terminals store bindings in different ways. The most reliable
//! approach for Ghostty is to ask it for its *effective* binding set via
//! `ghostty +list-keybinds`, which merges built-in defaults with the user's
//! config. The parsing is pure and unit-tested; only [`read_ghostty_keybinds`]
//! shells out.
//!
//! Alacritty has no equivalent "dump my effective keymap" command, so it needs
//! the opposite approach: its defaults are compiled into the binary and are only
//! documented in `alacritty-bindings(5)`. [`alacritty_macos_default_bindings`]
//! encodes the macOS subset of that table, and [`parse_alacritty_bindings`]
//! layers the user's `[[keyboard.bindings]]` overrides on top. Without this,
//! every Alacritty default was invisible to conflict detection: a jcode chord
//! like `cmd+b` (which Alacritty consumes for `SearchBackward`) looked free
//! while never reaching the TUI.

use super::chord::KeyChord;
use super::source::{DiscoveredBinding, KeySource};

/// Parse a single Ghostty keybind line of the form:
///
/// ```text
/// keybind = super+shift+,=reload_config
/// super+backspace=text:\x17
/// ```
///
/// The left side (up to the first top-level `=`) is the trigger; the right side
/// is the action. The trigger is `mod+mod+key`. Returns `None` for lines that
/// are not bindings (comments, blanks, multi-key sequences we do not model).
pub fn parse_ghostty_keybind_line(line: &str) -> Option<DiscoveredBinding> {
    let mut line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // Strip an optional leading `keybind =` / `keybind:` prefix.
    if let Some(rest) = line.strip_prefix("keybind") {
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':'))?;
        line = rest.trim();
    }

    // Split trigger=action on the first '='.
    let eq = line.find('=')?;
    let trigger = line[..eq].trim();
    let action = line[eq + 1..].trim();
    if trigger.is_empty() {
        return None;
    }

    let chord = parse_trigger(trigger)?;
    Some(DiscoveredBinding {
        chord,
        source: KeySource::Terminal,
        action: action.to_string(),
        raw: line.to_string(),
        tool: String::new(),
    })
}

/// Parse a `mod+mod+key` trigger into a chord. Returns `None` for triggers that
/// describe a multi-key sequence (Ghostty uses `>` between chords) since we only
/// model single chords for conflict detection.
fn parse_trigger(trigger: &str) -> Option<KeyChord> {
    if trigger.contains('>') {
        return None;
    }
    // Ghostty exposes a few logical triggers (mapped to the platform's native
    // shortcut) that are not real key chords. They can never collide with a
    // jcode binding, so drop them to keep the snapshot clean.
    if matches!(
        trigger.to_ascii_lowercase().as_str(),
        "copy" | "paste" | "unbind" | "ignore"
    ) {
        return None;
    }
    let mut cmd = false;
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut key: Option<String> = None;

    // Split on '+', but a trailing '+' means the key itself is '+'.
    let tokens = split_trigger_tokens(trigger);
    for tok in tokens {
        match tok.to_ascii_lowercase().as_str() {
            "super" | "cmd" | "command" => cmd = true,
            "ctrl" | "control" => ctrl = true,
            "alt" | "opt" | "option" => alt = true,
            "shift" => shift = true,
            other => {
                // Last non-modifier token wins as the key.
                key = Some(other.to_string());
            }
        }
    }

    let key = key?;
    Some(KeyChord::new(cmd, ctrl, alt, shift, &key))
}

/// Split a trigger on '+' while treating a literal '+' key correctly. For
/// example `super++` is `["super", "+"]` and `ctrl+shift++` is
/// `["ctrl", "shift", "+"]`.
fn split_trigger_tokens(trigger: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = trigger.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '+' {
            if cur.is_empty() {
                // A '+' with nothing before it is the literal '+' key.
                // Only treat it as the key when it is not a separator between
                // two names (i.e. previous char was already a separator).
                let is_trailing_or_double = i + 1 == chars.len() || chars[i + 1] == '+';
                if is_trailing_or_double || tokens.is_empty() {
                    tokens.push("+".to_string());
                    continue;
                }
            }
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Parse the full output of `ghostty +list-keybinds` (or a Ghostty config file)
/// into discovered bindings.
pub fn parse_ghostty_keybinds(output: &str) -> Vec<DiscoveredBinding> {
    output
        .lines()
        .filter_map(parse_ghostty_keybind_line)
        .collect()
}

/// Run `ghostty +list-keybinds` and parse its output. Returns an empty vec on
/// any failure. Tries the bundled macOS binary first, then `ghostty` on PATH.
#[cfg(target_os = "macos")]
pub fn read_ghostty_keybinds() -> Vec<DiscoveredBinding> {
    use std::process::Command;

    const CANDIDATES: [&str; 2] = [
        "/Applications/Ghostty.app/Contents/MacOS/ghostty",
        "ghostty",
    ];
    for bin in CANDIDATES {
        let Ok(output) = Command::new(bin).arg("+list-keybinds").output() else {
            continue;
        };
        if output.status.success() && !output.stdout.is_empty() {
            let text = String::from_utf8_lossy(&output.stdout);
            return parse_ghostty_keybinds(&text);
        }
    }
    Vec::new()
}

#[cfg(not(target_os = "macos"))]
pub fn read_ghostty_keybinds() -> Vec<DiscoveredBinding> {
    use std::process::Command;
    let Ok(output) = Command::new("ghostty").arg("+list-keybinds").output() else {
        return Vec::new();
    };
    if output.status.success() && !output.stdout.is_empty() {
        let text = String::from_utf8_lossy(&output.stdout);
        return parse_ghostty_keybinds(&text);
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Alacritty
// ---------------------------------------------------------------------------

/// Alacritty's compiled-in macOS key bindings, from `alacritty-bindings(5)`.
///
/// These are not discoverable at runtime: Alacritty ships no `+list-keybinds`
/// equivalent and writes no default config file, so the only way to know that
/// `Cmd+B` is `SearchBackward` is to encode the documented table. Each entry is
/// `(chord, action)` using jcode's canonical chord spelling.
///
/// Only bindings that can realistically shadow a jcode chord are listed. Vi and
/// search mode bindings are deliberately excluded: they apply only while
/// Alacritty is in those modes, which a TUI session is not.
#[cfg(any(test, target_os = "macos"))]
const ALACRITTY_MACOS_DEFAULTS: &[(&str, &str)] = &[
    ("cmd+k", "ClearHistory"),
    ("cmd+0", "ResetFontSize"),
    ("cmd+=", "IncreaseFontSize"),
    ("cmd+-", "DecreaseFontSize"),
    ("cmd+v", "Paste"),
    ("cmd+c", "Copy"),
    ("cmd+h", "Hide"),
    ("cmd+alt+h", "HideOtherApplications"),
    ("cmd+m", "Minimize"),
    ("cmd+q", "Quit"),
    ("cmd+w", "Quit"),
    ("cmd+n", "CreateNewWindow"),
    ("cmd+t", "CreateNewTab"),
    ("cmd+ctrl+f", "ToggleFullscreen"),
    ("cmd+f", "SearchForward"),
    ("cmd+b", "SearchBackward"),
    ("cmd+shift+]", "SelectNextTab"),
    ("cmd+shift+[", "SelectPreviousTab"),
    ("cmd+tab", "SelectNextTab"),
    ("cmd+shift+tab", "SelectPreviousTab"),
    ("cmd+1", "SelectTab1"),
    ("cmd+2", "SelectTab2"),
    ("cmd+3", "SelectTab3"),
    ("cmd+4", "SelectTab4"),
    ("cmd+5", "SelectTab5"),
    ("cmd+6", "SelectTab6"),
    ("cmd+7", "SelectTab7"),
    ("cmd+8", "SelectTab8"),
    ("cmd+9", "SelectLastTab"),
];

/// Alacritty's compiled-in bindings that apply on *every* platform, from the
/// "KEY BINDINGS" table of `alacritty-bindings(5)`.
///
/// `Ctrl+L` is the interesting one for jcode: it is bound outside Vi/Search
/// mode, so it is live in a normal TUI session on macOS as well as on Linux.
/// Mode-specific (Vi/Search) entries are excluded for the same reason as in
/// the macOS table.
const ALACRITTY_COMMON_DEFAULTS: &[(&str, &str)] = &[
    ("ctrl+l", "ClearLogNotice"),
    ("shift+pageup", "ScrollPageUp"),
    ("shift+pagedown", "ScrollPageDown"),
    ("shift+home", "ScrollToTop"),
    ("shift+end", "ScrollToBottom"),
];

/// Alacritty's compiled-in bindings for Windows, Linux and BSD, from the
/// "Windows, Linux, and BSD only" table of `alacritty-bindings(5)`.
///
/// Without this table a Linux user got *no* Alacritty coverage at all: the
/// scanner only knew the Cmd-based macOS defaults, so a clean conflict report
/// on Linux was not evidence of anything. `Ctrl+Shift+B/F` in particular
/// shadow chords in the same way `Cmd+B/F` do on macOS.
#[cfg(any(test, not(target_os = "macos")))]
const ALACRITTY_UNIX_DEFAULTS: &[(&str, &str)] = &[
    ("ctrl+shift+v", "Paste"),
    ("ctrl+shift+c", "Copy"),
    ("ctrl+shift+f", "SearchForward"),
    ("ctrl+shift+b", "SearchBackward"),
    ("shift+insert", "PasteSelection"),
    ("ctrl+0", "ResetFontSize"),
    ("ctrl+=", "IncreaseFontSize"),
    ("ctrl++", "IncreaseFontSize"),
    ("ctrl+-", "DecreaseFontSize"),
];

/// Build [`DiscoveredBinding`]s from a `(chord, action)` table.
fn alacritty_table_bindings(table: &[(&str, &str)]) -> Vec<DiscoveredBinding> {
    table
        .iter()
        .filter_map(|(chord, action)| {
            Some(DiscoveredBinding {
                chord: KeyChord::parse(chord)?,
                source: KeySource::Terminal,
                action: (*action).to_string(),
                raw: (*chord).to_string(),
                tool: "Alacritty".to_string(),
            })
        })
        .collect()
}

/// The documented macOS default bindings, as [`DiscoveredBinding`]s.
#[cfg(any(test, target_os = "macos"))]
pub fn alacritty_macos_default_bindings() -> Vec<DiscoveredBinding> {
    let mut binds = alacritty_table_bindings(ALACRITTY_MACOS_DEFAULTS);
    binds.extend(alacritty_table_bindings(ALACRITTY_COMMON_DEFAULTS));
    binds
}

/// The documented Windows/Linux/BSD default bindings, as
/// [`DiscoveredBinding`]s.
#[cfg(any(test, not(target_os = "macos")))]
pub fn alacritty_unix_default_bindings() -> Vec<DiscoveredBinding> {
    let mut binds = alacritty_table_bindings(ALACRITTY_UNIX_DEFAULTS);
    binds.extend(alacritty_table_bindings(ALACRITTY_COMMON_DEFAULTS));
    binds
}

/// Alacritty's config search order, relative to `$HOME`. `$XDG_CONFIG_HOME` is
/// handled separately by the caller since it is an absolute path.
const ALACRITTY_CONFIG_CANDIDATES: [&str; 3] = [
    ".config/alacritty/alacritty.toml",
    ".alacritty.toml",
    ".config/alacritty.toml",
];

/// Layer the user's `alacritty.toml` (first match in the documented search
/// order) on top of `defaults`.
fn apply_alacritty_user_config(mut effective: Vec<DiscoveredBinding>) -> Vec<DiscoveredBinding> {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let xdg = std::path::PathBuf::from(xdg);
        paths.push(xdg.join("alacritty/alacritty.toml"));
        paths.push(xdg.join("alacritty.toml"));
    }
    if let Some(home) = dirs::home_dir() {
        paths.extend(ALACRITTY_CONFIG_CANDIDATES.iter().map(|rel| home.join(rel)));
    }
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (user, unbound) = parse_alacritty_bindings(&text);
        effective.retain(|b| !unbound.contains(&b.chord));
        for binding in user {
            effective.retain(|b| b.chord != binding.chord);
            effective.push(binding);
        }
        break;
    }
    effective
}

/// Parse the `[[keyboard.bindings]]` array of an `alacritty.toml`.
///
/// A user binding shadows the default for the same chord, and can also free a
/// default chord up: Alacritty documents `action = "ReceiveChar"` and
/// `action = "None"` as the ways to unset a built-in binding, so those entries
/// are recorded as *removals* rather than bindings. The caller merges this with
/// [`alacritty_macos_default_bindings`].
///
/// Returns `(bindings, unbound_chords)`.
pub fn parse_alacritty_bindings(text: &str) -> (Vec<DiscoveredBinding>, Vec<KeyChord>) {
    let mut bindings = Vec::new();
    let mut unbound = Vec::new();

    let Ok(value) = text.parse::<toml::Value>() else {
        return (bindings, unbound);
    };
    let Some(entries) = value
        .get("keyboard")
        .and_then(|k| k.get("bindings"))
        .and_then(|b| b.as_array())
    else {
        return (bindings, unbound);
    };

    for entry in entries {
        let Some(key) = entry.get("key").and_then(|k| k.as_str()) else {
            continue;
        };
        // `mods` is a `|`-separated list: "Command|Shift".
        let mods = entry
            .get("mods")
            .and_then(|m| m.as_str())
            .unwrap_or_default();
        let mut parts: Vec<&str> = mods
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        parts.push(key);
        let Some(chord) = KeyChord::parse(&parts.join("+")) else {
            continue;
        };

        let action = entry.get("action").and_then(|a| a.as_str()).unwrap_or("");
        if action.eq_ignore_ascii_case("ReceiveChar") || action.eq_ignore_ascii_case("None") {
            unbound.push(chord);
            continue;
        }

        // `chars` entries send a literal string; they still consume the chord.
        let label = if action.is_empty() {
            match entry.get("chars").and_then(|c| c.as_str()) {
                Some(_) => "sends literal text".to_string(),
                None => continue,
            }
        } else {
            action.to_string()
        };

        bindings.push(DiscoveredBinding {
            chord,
            source: KeySource::Terminal,
            action: label,
            raw: format!("{mods}+{key}"),
            tool: "Alacritty".to_string(),
        });
    }

    (bindings, unbound)
}

/// Effective Alacritty bindings for this machine: documented defaults, with the
/// user's config layered on top. Returns nothing when Alacritty is not the
/// active terminal, so an unused terminal never generates conflict noise.
#[cfg(target_os = "macos")]
pub fn read_alacritty_keybinds() -> Vec<DiscoveredBinding> {
    // Only report bindings for the terminal actually in use. Alacritty sets
    // ALACRITTY_WINDOW_ID in every shell it spawns.
    if std::env::var_os("ALACRITTY_WINDOW_ID").is_none() {
        return Vec::new();
    }
    apply_alacritty_user_config(alacritty_macos_default_bindings())
}

#[cfg(not(target_os = "macos"))]
pub fn read_alacritty_keybinds() -> Vec<DiscoveredBinding> {
    if std::env::var_os("ALACRITTY_WINDOW_ID").is_none() {
        return Vec::new();
    }
    apply_alacritty_user_config(alacritty_unix_default_bindings())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listed_keybind() {
        let b = parse_ghostty_keybind_line("keybind = super+c=copy_to_clipboard:mixed").unwrap();
        assert_eq!(b.chord.canonical(), "cmd+c");
        assert_eq!(b.action, "copy_to_clipboard:mixed");
    }

    #[test]
    fn parses_bare_config_line() {
        let b = parse_ghostty_keybind_line("super+backspace=text:\\x17").unwrap();
        assert_eq!(b.chord.canonical(), "cmd+backspace");
        assert_eq!(b.action, "text:\\x17");
    }

    #[test]
    fn parses_named_punctuation_key() {
        let b = parse_ghostty_keybind_line("keybind = super+shift+,=reload_config").unwrap();
        assert_eq!(b.chord.canonical(), "cmd+shift+,");
    }

    #[test]
    fn parses_digit_key() {
        let b = parse_ghostty_keybind_line("keybind = super+digit_1=goto_tab:1").unwrap();
        assert_eq!(b.chord.canonical(), "cmd+1");
    }

    #[test]
    fn parses_literal_plus_key() {
        let b = parse_ghostty_keybind_line("keybind = super++=increase_font_size:1").unwrap();
        assert_eq!(b.chord.canonical(), "cmd++");
    }

    #[test]
    fn skips_comments_and_blanks() {
        assert!(parse_ghostty_keybind_line("# a comment").is_none());
        assert!(parse_ghostty_keybind_line("   ").is_none());
    }

    #[test]
    fn skips_multi_key_sequences() {
        assert!(parse_ghostty_keybind_line("keybind = ctrl+a>n=new_window").is_none());
    }

    #[test]
    fn skips_logical_copy_paste_triggers() {
        assert!(parse_ghostty_keybind_line("keybind = copy=copy_to_clipboard:mixed").is_none());
        assert!(parse_ghostty_keybind_line("keybind = paste=paste_from_clipboard").is_none());
    }

    #[test]
    fn parses_full_output() {
        let out = "\
keybind = super+c=copy_to_clipboard:mixed
keybind = super+v=paste_from_clipboard
# comment
keybind = super+enter=new_window
";
        let binds = parse_ghostty_keybinds(out);
        assert_eq!(binds.len(), 3);
    }

    #[test]
    fn alacritty_defaults_include_the_chords_that_shadow_jcode() {
        let binds = alacritty_macos_default_bindings();
        let find = |c: &str| binds.iter().find(|b| b.chord.canonical() == c);

        // cmd+b is the regression that motivated this scanner: Alacritty
        // consumes it for SearchBackward, so jcode's default `open_resume`
        // binding never reached the TUI and looked like a jcode bug.
        assert_eq!(
            find("cmd+b").map(|b| b.action.as_str()),
            Some("SearchBackward")
        );
        assert_eq!(
            find("cmd+f").map(|b| b.action.as_str()),
            Some("SearchForward")
        );
        assert_eq!(
            find("cmd+k").map(|b| b.action.as_str()),
            Some("ClearHistory")
        );
        assert!(binds.iter().all(|b| b.tool == "Alacritty"));
        assert!(binds.iter().all(|b| b.source == KeySource::Terminal));
    }

    #[test]
    fn alacritty_unix_defaults_cover_the_ctrl_shift_table() {
        // Before this table existed, a Linux user got zero Alacritty bindings,
        // so a "no conflicts" report there meant nothing.
        let binds = alacritty_unix_default_bindings();
        let find = |c: &str| binds.iter().find(|b| b.chord.canonical() == c);

        assert_eq!(
            find("ctrl+shift+b").map(|b| b.action.as_str()),
            Some("SearchBackward")
        );
        assert_eq!(
            find("ctrl+shift+f").map(|b| b.action.as_str()),
            Some("SearchForward")
        );
        assert_eq!(
            find("ctrl+shift+c").map(|b| b.action.as_str()),
            Some("Copy")
        );
        assert_eq!(
            find("ctrl+shift+v").map(|b| b.action.as_str()),
            Some("Paste")
        );
        assert_eq!(
            find("shift+insert").map(|b| b.action.as_str()),
            Some("PasteSelection")
        );
        assert_eq!(
            find("ctrl+0").map(|b| b.action.as_str()),
            Some("ResetFontSize")
        );
        assert!(binds.iter().all(|b| b.tool == "Alacritty"));
        assert!(binds.iter().all(|b| b.source == KeySource::Terminal));
        // The Cmd-based macOS table must NOT leak onto other platforms.
        assert!(
            binds
                .iter()
                .all(|b| !b.chord.canonical().starts_with("cmd+"))
        );
    }

    #[test]
    fn alacritty_cross_platform_defaults_are_in_both_tables() {
        // `ctrl+l` is bound outside Vi/Search mode on every platform, so it is
        // live in a normal TUI session and belongs in both tables.
        for binds in [
            alacritty_macos_default_bindings(),
            alacritty_unix_default_bindings(),
        ] {
            let find = |c: &str| binds.iter().find(|b| b.chord.canonical() == c);
            assert_eq!(
                find("ctrl+l").map(|b| b.action.as_str()),
                Some("ClearLogNotice")
            );
            assert_eq!(
                find("shift+pageup").map(|b| b.action.as_str()),
                Some("ScrollPageUp")
            );
        }
    }

    #[test]
    fn jcode_default_scroll_bookmark_survives_but_ctrl_shift_b_conflicts_on_linux() {
        use crate::keymap::{KeymapSnapshot, detect_conflicts};
        use jcode_config_types::KeybindingsConfig;

        let snapshot = KeymapSnapshot {
            version: 1,
            captured_at: String::new(),
            os: "linux".to_string(),
            terminal: "Alacritty".to_string(),
            terminal_version: String::new(),
            bindings: alacritty_unix_default_bindings(),
        };
        let cfg = KeybindingsConfig {
            open_resume: "ctrl+shift+b".to_string(),
            ..Default::default()
        };
        let conflicts = detect_conflicts(&cfg, &snapshot);
        assert!(
            conflicts
                .iter()
                .any(|c| c.jcode.field == "keybindings.open_resume"),
            "expected ctrl+shift+b to conflict with Alacritty SearchBackward, got {conflicts:?}"
        );
    }

    #[test]
    fn alacritty_user_bindings_are_parsed_from_config() {
        let cfg = r#"
[[keyboard.bindings]]
key = "Return"
mods = "Shift"
chars = "\u001B\r"

[[keyboard.bindings]]
key = "N"
mods = "Command|Shift"
action = "CreateNewWindow"
"#;
        let (binds, unbound) = parse_alacritty_bindings(cfg);
        assert!(unbound.is_empty());
        let chords: Vec<String> = binds.iter().map(|b| b.chord.canonical()).collect();
        assert!(
            chords.contains(&"shift+enter".to_string()),
            "got {chords:?}"
        );
        assert!(
            chords.contains(&"cmd+shift+n".to_string()),
            "got {chords:?}"
        );
    }

    #[test]
    fn alacritty_receivechar_frees_a_default_chord() {
        // Alacritty documents ReceiveChar/None as the way to unset a built-in
        // binding. A user who does that has deliberately handed the chord back
        // to the application, so it must stop being reported as a conflict.
        let cfg = r#"
[[keyboard.bindings]]
key = "B"
mods = "Command"
action = "ReceiveChar"
"#;
        let (binds, unbound) = parse_alacritty_bindings(cfg);
        assert!(binds.is_empty());
        assert_eq!(unbound.len(), 1);
        assert_eq!(unbound[0].canonical(), "cmd+b");
    }

    #[test]
    fn alacritty_malformed_config_is_ignored_not_fatal() {
        let (binds, unbound) = parse_alacritty_bindings("this is not toml {{{");
        assert!(binds.is_empty() && unbound.is_empty());
        // A config with no bindings table is fine too.
        let (binds, _) = parse_alacritty_bindings("[font]\nsize = 12\n");
        assert!(binds.is_empty());
    }

    #[test]
    fn jcode_default_open_resume_conflicts_with_alacritty_search_backward() {
        // End-to-end: with the Alacritty table in the snapshot, jcode's own
        // macOS default for `open_resume` (cmd+b) is now correctly reported
        // instead of silently doing nothing.
        use crate::keymap::{KeymapSnapshot, detect_conflicts};
        use jcode_config_types::KeybindingsConfig;

        let snapshot = KeymapSnapshot {
            version: 1,
            captured_at: String::new(),
            os: "macos".to_string(),
            terminal: "Alacritty".to_string(),
            terminal_version: String::new(),
            bindings: alacritty_macos_default_bindings(),
        };
        let cfg = KeybindingsConfig {
            open_resume: "cmd+b".to_string(),
            ..Default::default()
        };
        let conflicts = detect_conflicts(&cfg, &snapshot);
        assert!(
            conflicts
                .iter()
                .any(|c| c.jcode.field == "keybindings.open_resume"),
            "expected cmd+b to conflict with Alacritty SearchBackward, got {conflicts:?}"
        );
    }
}
