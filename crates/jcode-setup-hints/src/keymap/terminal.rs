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
//!
//! Whether the terminal delivers the Option/Alt modifier *at all* is a separate
//! question, answered by the sibling [`super::alt`] module.
//!
//! WezTerm sits with Ghostty rather than Alacritty: `wezterm show-keys` prints
//! the effective key tables (defaults merged with the user's Lua config), so we
//! ask it instead of encoding a table that would drift.
//!
//! kitty sits with Alacritty: `kitty.conf` is plain text, but the defaults are
//! compiled in and `kitty --debug-config` prints only the *diff* against them,
//! so [`read_kitty_keybinds`] encodes the upstream table and layers the user's
//! `map` directives (including `clear_all_shortcuts` and unbinds) on top.
//!
//! Terminal.app is a fourth shape again: it has no config file and no dump
//! command, but it stores its *user-rebound* keys in the `com.apple.Terminal`
//! preference domain, so [`read_apple_terminal_keybinds`] reads them live
//! through the shared plist pipeline. See that function for why only rebound
//! keys are visible, and why that is the interesting set anyway.

use super::chord::KeyChord;
use super::source::{AltSide, DiscoveredBinding, KeySource};

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
        // Terminal and system declarations never name an Option side.
        alt_side: AltSide::Unspecified,
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
                // Terminal and system declarations never name an Option side.
                alt_side: AltSide::Unspecified,
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

/// The user's `alacritty.toml`, from the first path in the documented search
/// order that exists and is readable, or `None` when the user has no config at
/// all (in which case Alacritty's compiled-in defaults apply unmodified).
pub fn read_alacritty_config() -> Option<String> {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let xdg = std::path::PathBuf::from(xdg);
        paths.push(xdg.join("alacritty/alacritty.toml"));
        paths.push(xdg.join("alacritty.toml"));
    }
    if let Some(home) = dirs::home_dir() {
        paths.extend(ALACRITTY_CONFIG_CANDIDATES.iter().map(|rel| home.join(rel)));
    }
    paths
        .into_iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
}

/// Layer the user's `alacritty.toml` (first match in the documented search
/// order) on top of `defaults`.
fn apply_alacritty_user_config(mut effective: Vec<DiscoveredBinding>) -> Vec<DiscoveredBinding> {
    let Some(text) = read_alacritty_config() else {
        return effective;
    };
    let (user, unbound) = parse_alacritty_bindings(&text);
    effective.retain(|b| !unbound.contains(&b.chord));
    for binding in user {
        effective.retain(|b| b.chord != binding.chord);
        effective.push(binding);
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
            // Terminal and system declarations never name an Option side.
            alt_side: AltSide::Unspecified,
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

// ---------------------------------------------------------------------------
// WezTerm
// ---------------------------------------------------------------------------

/// Translate a WezTerm key name onto jcode's vocabulary.
///
/// WezTerm accepts `phys:` (physical position) and `mapped:` (post-layout)
/// prefixes on key names; both denote the same physical chord for our purposes,
/// so the prefix is stripped. Its arrow spellings (`UpArrow`) and `Escape` are
/// mapped onto the names [`KeyChord::normalize_key`] already understands.
fn wezterm_key_name(raw: &str) -> String {
    let key = raw
        .strip_prefix("phys:")
        .or_else(|| raw.strip_prefix("mapped:"))
        .unwrap_or(raw);
    match key.to_ascii_lowercase().as_str() {
        "uparrow" => "up".to_string(),
        "downarrow" => "down".to_string(),
        "leftarrow" => "left".to_string(),
        "rightarrow" => "right".to_string(),
        other => other.to_string(),
    }
}

/// Parse one binding row of `wezterm show-keys` output, e.g.
///
/// ```text
///     SHIFT | CTRL         Tab              ->   ActivateTabRelative(-1)
/// ```
///
/// The row is `[modifiers] key -> action`, where modifiers are `|`-separated
/// and may be absent. Returns `None` for rows that are not single-chord
/// bindings.
fn parse_wezterm_row(line: &str) -> Option<DiscoveredBinding> {
    let (trigger, action) = line.split_once("->")?;
    let action = action.trim();
    if action.is_empty() {
        return None;
    }

    let mut cmd = false;
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut key: Option<String> = None;

    for token in trigger.split_whitespace() {
        if token == "|" {
            continue;
        }
        match token.to_ascii_uppercase().as_str() {
            "SUPER" | "CMD" | "WIN" => cmd = true,
            "CTRL" => ctrl = true,
            "ALT" | "OPT" => alt = true,
            "SHIFT" => shift = true,
            // A LEADER prefix makes this a multi-key sequence, which we do not
            // model: the chord alone never reaches jcode as a single press.
            "LEADER" => return None,
            _ => key = Some(token.to_string()),
        }
    }

    let key = key?;
    Some(DiscoveredBinding {
        chord: KeyChord::new(cmd, ctrl, alt, shift, &wezterm_key_name(&key)),
        source: KeySource::Terminal,
        action: action.to_string(),
        raw: line.trim().to_string(),
        tool: "WezTerm".to_string(),
        // Terminal and system declarations never name an Option side.
        alt_side: AltSide::Unspecified,
    })
}

/// Parse the output of `wezterm show-keys`.
///
/// Only the "Default key table" section is read. The `copy_mode` and
/// `search_mode` tables apply solely while WezTerm is in those modes, which a
/// running TUI session is not, so including them would manufacture conflicts
/// that can never fire — the same rationale that excludes Alacritty's Vi-mode
/// bindings. The `Mouse` section is not key input at all.
pub fn parse_wezterm_keys(output: &str) -> Vec<DiscoveredBinding> {
    let mut bindings = Vec::new();
    let mut in_default_table = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.chars().all(|c| c == '-') {
            continue;
        }
        // Section headers are unindented; binding rows are always indented.
        if !line.starts_with([' ', '\t']) {
            in_default_table = trimmed.eq_ignore_ascii_case("Default key table");
            continue;
        }
        if !in_default_table {
            continue;
        }
        if let Some(binding) = parse_wezterm_row(line) {
            bindings.push(binding);
        }
    }

    bindings
}

/// Effective WezTerm bindings for this machine, via `wezterm show-keys`, which
/// merges the compiled-in defaults with the user's Lua config. Returns nothing
/// when WezTerm is not the active terminal, so an unused terminal never
/// generates conflict noise.
///
/// Asking the binary beats encoding a table: WezTerm's default key set is large
/// and configured in Lua, which we could not evaluate anyway.
pub fn read_wezterm_keybinds() -> Vec<DiscoveredBinding> {
    use std::process::Command;

    if !std::env::var("TERM_PROGRAM").is_ok_and(|v| v.eq_ignore_ascii_case("WezTerm")) {
        return Vec::new();
    }
    let Ok(output) = Command::new("wezterm").arg("show-keys").output() else {
        return Vec::new();
    };
    if !output.status.success() || output.stdout.is_empty() {
        return Vec::new();
    }
    parse_wezterm_keys(&String::from_utf8_lossy(&output.stdout))
}

// ---------------------------------------------------------------------------
// kitty
// ---------------------------------------------------------------------------

/// kitty's default value for `kitty_mod`, the modifier alias every built-in
/// shortcut is written against. A user can rebind it in `kitty.conf`, which
/// moves the *entire* default table onto a different modifier at once — so it
/// has to be read before any default chord can be resolved.
const KITTY_MOD_DEFAULT: &str = "ctrl+shift";

/// kitty's compiled-in shortcuts that apply on every platform, transcribed from
/// upstream `kitty/options/definition.py`.
///
/// kitty sits with Alacritty rather than Ghostty/WezTerm: `kitty.conf` is plain
/// text, but the defaults are compiled into the binary and the config file that
/// ships is entirely commented out, so a user's config says nothing about which
/// chords are actually taken. There is no "dump my effective keymap" command
/// either (`kitty --debug-config` prints only the *diff* against defaults), so
/// the table has to be encoded.
///
/// Multi-key sequences (`kitty_mod+p>f` and friends) are excluded: they are
/// two-chord sequences, and only the first chord is consumed, which we do not
/// model. Mouse maps are not key input.
const KITTY_COMMON_DEFAULTS: &[(&str, &str)] = &[
    ("kitty_mod+c", "copy_to_clipboard"),
    ("kitty_mod+v", "paste_from_clipboard"),
    ("kitty_mod+s", "paste_from_selection"),
    ("shift+insert", "paste_from_selection"),
    ("kitty_mod+o", "pass_selection_to_program"),
    ("kitty_mod+up", "scroll_line_up"),
    ("kitty_mod+k", "scroll_line_up"),
    ("kitty_mod+down", "scroll_line_down"),
    ("kitty_mod+j", "scroll_line_down"),
    ("kitty_mod+page_up", "scroll_page_up"),
    ("kitty_mod+page_down", "scroll_page_down"),
    ("kitty_mod+home", "scroll_home"),
    ("kitty_mod+end", "scroll_end"),
    ("kitty_mod+z", "scroll_to_prompt"),
    ("kitty_mod+h", "show_scrollback"),
    ("kitty_mod+g", "show_last_command_output"),
    ("kitty_mod+/", "search_scrollback"),
    ("kitty_mod+enter", "new_window"),
    ("kitty_mod+n", "new_os_window"),
    ("kitty_mod+w", "close_window"),
    ("kitty_mod+]", "next_window"),
    ("kitty_mod+[", "previous_window"),
    ("kitty_mod+f", "move_window_forward"),
    ("kitty_mod+b", "move_window_backward"),
    ("kitty_mod+`", "move_window_to_top"),
    ("kitty_mod+r", "start_resizing_window"),
    ("kitty_mod+1", "first_window"),
    ("kitty_mod+2", "second_window"),
    ("kitty_mod+3", "third_window"),
    ("kitty_mod+4", "fourth_window"),
    ("kitty_mod+5", "fifth_window"),
    ("kitty_mod+6", "sixth_window"),
    ("kitty_mod+7", "seventh_window"),
    ("kitty_mod+8", "eighth_window"),
    ("kitty_mod+9", "ninth_window"),
    ("kitty_mod+0", "tenth_window"),
    ("kitty_mod+f7", "focus_visible_window"),
    ("kitty_mod+f8", "swap_with_window"),
    ("kitty_mod+right", "next_tab"),
    ("ctrl+tab", "next_tab"),
    ("kitty_mod+left", "previous_tab"),
    ("ctrl+shift+tab", "previous_tab"),
    ("kitty_mod+t", "new_tab"),
    ("kitty_mod+q", "close_tab"),
    ("kitty_mod+.", "move_tab_forward"),
    ("kitty_mod+,", "move_tab_backward"),
    ("kitty_mod+alt+t", "set_tab_title"),
    ("kitty_mod+l", "next_layout"),
    ("kitty_mod+equal", "change_font_size"),
    ("kitty_mod+plus", "change_font_size"),
    ("kitty_mod+minus", "change_font_size"),
    ("kitty_mod+backspace", "change_font_size"),
    ("kitty_mod+e", "open_url_with_hints"),
    ("kitty_mod+f11", "toggle_fullscreen"),
    ("kitty_mod+f10", "toggle_maximized"),
    ("kitty_mod+u", "input_unicode_character"),
    ("kitty_mod+f2", "edit_config_file"),
    ("kitty_mod+escape", "kitty_shell"),
    ("kitty_mod+delete", "clear_terminal"),
    ("kitty_mod+f5", "load_config_file"),
    ("kitty_mod+f6", "debug_config"),
];

/// kitty's compiled-in shortcuts that exist only on macOS (the `only='macos'`
/// entries upstream). These are Cmd-based and must not leak onto other
/// platforms, where the physical key produces no Cmd modifier at all.
const KITTY_MACOS_DEFAULTS: &[(&str, &str)] = &[
    ("cmd+c", "copy_or_noop"),
    ("cmd+v", "paste_from_clipboard"),
    ("opt+cmd+page_up", "scroll_line_up"),
    ("cmd+up", "scroll_line_up"),
    ("opt+cmd+page_down", "scroll_line_down"),
    ("cmd+down", "scroll_line_down"),
    ("cmd+page_up", "scroll_page_up"),
    ("cmd+page_down", "scroll_page_down"),
    ("cmd+home", "scroll_home"),
    ("cmd+end", "scroll_end"),
    ("cmd+enter", "new_window"),
    ("cmd+n", "new_os_window"),
    ("shift+cmd+d", "close_window"),
    ("cmd+r", "start_resizing_window"),
    ("cmd+1", "first_window"),
    ("cmd+2", "second_window"),
    ("cmd+3", "third_window"),
    ("cmd+4", "fourth_window"),
    ("cmd+5", "fifth_window"),
    ("cmd+6", "sixth_window"),
    ("cmd+7", "seventh_window"),
    ("cmd+8", "eighth_window"),
    ("cmd+9", "ninth_window"),
    ("shift+cmd+]", "next_tab"),
    ("shift+cmd+[", "previous_tab"),
    ("cmd+t", "new_tab"),
    ("cmd+w", "close_tab"),
    ("shift+cmd+w", "close_os_window"),
    ("shift+cmd+i", "set_tab_title"),
    ("cmd+plus", "change_font_size"),
    ("cmd+equal", "change_font_size"),
    ("shift+cmd+equal", "change_font_size"),
    ("cmd+minus", "change_font_size"),
    ("shift+cmd+minus", "change_font_size"),
    ("cmd+0", "change_font_size"),
    ("ctrl+cmd+f", "toggle_fullscreen"),
    ("opt+cmd+s", "toggle_macos_secure_keyboard_entry"),
    ("ctrl+cmd+space", "input_unicode_character"),
    ("cmd+,", "edit_config_file"),
    ("opt+cmd+r", "clear_terminal"),
    ("cmd+k", "clear_terminal"),
    ("opt+cmd+k", "clear_terminal"),
    ("cmd+l", "clear_terminal"),
    ("ctrl+cmd+l", "clear_terminal"),
    ("cmd+h", "hide_macos_app"),
    ("opt+cmd+h", "hide_macos_other_apps"),
    ("cmd+m", "minimize_macos_window"),
    ("cmd+q", "quit"),
];

/// Expand a kitty chord spec into a [`KeyChord`], substituting `kitty_mod` for
/// the effective modifier alias.
///
/// Returns `None` for multi-key sequences (`a>b`) and for specs that name no
/// key at all — `map kitty_mod+space` with no action is kitty's syntax for
/// *removing* a binding, which frees the chord rather than taking it.
fn kitty_chord(spec: &str, kitty_mod: &str) -> Option<KeyChord> {
    if spec.contains('>') {
        return None;
    }
    let expanded = spec.replace("kitty_mod", kitty_mod);
    let mut cmd = false;
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut key: Option<String> = None;

    for tok in split_trigger_tokens(&expanded) {
        match tok.to_ascii_lowercase().as_str() {
            "super" | "cmd" | "command" => cmd = true,
            "ctrl" | "control" => ctrl = true,
            "alt" | "opt" | "option" => alt = true,
            "shift" => shift = true,
            // kitty spells hyper/meta as distinct modifiers; jcode has no
            // vocabulary for them, and a chord we cannot represent would
            // compare equal to a different one if we dropped the modifier.
            "hyper" | "meta" => return None,
            other => key = Some(kitty_key(other)),
        }
    }

    Some(KeyChord::new(cmd, ctrl, alt, shift, &key?))
}

/// Translate kitty's key spellings onto jcode's vocabulary. Most names already
/// match after [`KeyChord::normalize_key`]; these are the ones that do not.
fn kitty_key(raw: &str) -> String {
    match raw {
        "plus" => "+".to_string(),
        "equal" => "=".to_string(),
        "minus" => "-".to_string(),
        // Numpad keys are physically distinct from the main-row ones and
        // never collide with a jcode chord, so they are dropped upstream of
        // here by never appearing in the tables.
        other => other.to_string(),
    }
}

/// Build [`DiscoveredBinding`]s from a kitty `(spec, action)` table.
fn kitty_table_bindings(table: &[(&str, &str)], kitty_mod: &str) -> Vec<DiscoveredBinding> {
    table
        .iter()
        .filter_map(|(spec, action)| {
            Some(DiscoveredBinding {
                chord: kitty_chord(spec, kitty_mod)?,
                source: KeySource::Terminal,
                action: (*action).to_string(),
                raw: (*spec).to_string(),
                tool: "kitty".to_string(),
                // Terminal and system declarations never name an Option side.
                alt_side: AltSide::Unspecified,
            })
        })
        .collect()
}

/// kitty's documented defaults for this platform, resolved against `kitty_mod`.
fn kitty_default_bindings(kitty_mod: &str) -> Vec<DiscoveredBinding> {
    let mut binds = kitty_table_bindings(KITTY_COMMON_DEFAULTS, kitty_mod);
    if cfg!(target_os = "macos") {
        binds.extend(kitty_table_bindings(KITTY_MACOS_DEFAULTS, kitty_mod));
    }
    binds
}

/// The value of `kitty_mod` declared in a `kitty.conf`, or kitty's default.
pub fn parse_kitty_mod(text: &str) -> String {
    text.lines()
        .rev()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let rest = line.strip_prefix("kitty_mod")?;
            // Guard against `kitty_mod_something`: the option name must be
            // followed by whitespace.
            if !rest.starts_with(char::is_whitespace) {
                return None;
            }
            let value = rest.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
        .next()
        .unwrap_or_else(|| KITTY_MOD_DEFAULT.to_string())
}

/// Parse the `map` directives of a `kitty.conf`.
///
/// Returns `(bindings, unbound_chords, cleared_all)`.
///
/// Three kitty behaviours matter for conflict detection:
/// - `map <chord>` with no action *unbinds* the chord, handing it back to the
///   program running in the terminal. That frees a default, so it is recorded
///   as a removal rather than a binding.
/// - `map <chord> no_op` does the same thing under kitty's older spelling.
///   `discard_event`, by contrast, swallows the key, so it still conflicts.
/// - `clear_all_shortcuts yes` drops every default declared *above* it, which
///   is the documented way to start from a blank slate.
pub fn parse_kitty_maps(text: &str) -> (Vec<DiscoveredBinding>, Vec<KeyChord>, bool) {
    let kitty_mod = parse_kitty_mod(text);
    let mut bindings = Vec::new();
    let mut unbound = Vec::new();
    let mut cleared = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("clear_all_shortcuts") {
            let value = rest.trim().to_ascii_lowercase();
            if value == "yes" || value == "y" || value == "true" {
                cleared = true;
                bindings.clear();
                unbound.clear();
            }
            continue;
        }
        // `map [--options] <chord> [action...]`
        let mut parts = line.split_whitespace();
        if parts.next() != Some("map") {
            continue;
        }
        let mut parts = parts.skip_while(|t| t.starts_with("--"));
        let Some(spec) = parts.next() else {
            continue;
        };
        let Some(chord) = kitty_chord(spec, &kitty_mod) else {
            continue;
        };
        let action = parts.collect::<Vec<_>>().join(" ");
        if action.is_empty() || action == "no_op" {
            unbound.push(chord);
            continue;
        }
        bindings.push(DiscoveredBinding {
            chord,
            source: KeySource::Terminal,
            action,
            raw: line.to_string(),
            tool: "kitty".to_string(),
            // Terminal and system declarations never name an Option side.
            alt_side: AltSide::Unspecified,
        });
    }

    (bindings, unbound, cleared)
}

/// kitty's config search order. `$KITTY_CONFIG_DIRECTORY` wins when set, then
/// `$XDG_CONFIG_HOME/kitty`, then `~/.config/kitty`.
pub fn read_kitty_config() -> Option<String> {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("KITTY_CONFIG_DIRECTORY") {
        paths.push(std::path::PathBuf::from(dir).join("kitty.conf"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        paths.push(std::path::PathBuf::from(xdg).join("kitty/kitty.conf"));
    }
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".config/kitty/kitty.conf"));
    }
    paths
        .into_iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
}

/// Layer a `kitty.conf` on top of the compiled-in defaults.
pub fn apply_kitty_user_config(text: &str) -> Vec<DiscoveredBinding> {
    let kitty_mod = parse_kitty_mod(text);
    let (user, unbound, cleared) = parse_kitty_maps(text);
    let mut effective = if cleared {
        Vec::new()
    } else {
        kitty_default_bindings(&kitty_mod)
    };
    effective.retain(|b| !unbound.contains(&b.chord));
    for binding in user {
        effective.retain(|b| b.chord != binding.chord);
        effective.push(binding);
    }
    effective
}

/// Effective kitty bindings for this machine: documented defaults with the
/// user's `kitty.conf` layered on top. Returns nothing when kitty is not the
/// active terminal, so an unused terminal never generates conflict noise.
pub fn read_kitty_keybinds() -> Vec<DiscoveredBinding> {
    // kitty exports KITTY_WINDOW_ID (and KITTY_PID) into every shell it spawns.
    if std::env::var_os("KITTY_WINDOW_ID").is_none() {
        return Vec::new();
    }
    match read_kitty_config() {
        Some(text) => apply_kitty_user_config(&text),
        None => kitty_default_bindings(KITTY_MOD_DEFAULT),
    }
}

// ---------------------------------------------------------------------------
// Terminal.app
// ---------------------------------------------------------------------------

/// Map one Terminal.app private-use function-key code point to a key token.
///
/// Terminal.app spells non-printable keys with the AppKit `NSxxxFunctionKey`
/// constants, which live in the Unicode private-use area (`U+F700..`). A raw
/// `U+F704` in the plist means F1, not an unknown glyph, so these have to be
/// translated rather than passed through as a literal key name.
fn apple_terminal_function_key(ch: char) -> Option<&'static str> {
    Some(match ch as u32 {
        0xF700 => "up",
        0xF701 => "down",
        0xF702 => "left",
        0xF703 => "right",
        0xF704 => "f1",
        0xF705 => "f2",
        0xF706 => "f3",
        0xF707 => "f4",
        0xF708 => "f5",
        0xF709 => "f6",
        0xF70A => "f7",
        0xF70B => "f8",
        0xF70C => "f9",
        0xF70D => "f10",
        0xF70E => "f11",
        0xF70F => "f12",
        0xF728 => "delete",
        0xF729 => "home",
        0xF72B => "end",
        0xF72C => "pageup",
        0xF72D => "pagedown",
        0xF739 => "clear",
        _ => return None,
    })
}

/// Parse one `keyMapBoundKeys` key, e.g. `"^$\u{F702}"` (Ctrl+Shift+Left).
///
/// The format is a run of symbolic modifier prefixes followed by exactly one
/// key character. Apple's documented prefixes are `^` Control, `~` Option,
/// `$` Shift and `@` Command; `#` marks the numeric keypad, which jcode has no
/// vocabulary for.
///
/// Returns `None` for anything that is not a usable, representable chord:
///
/// * **no modifiers at all** — a bare key rebinding (Terminal.app lets you bind
///   a plain `F1`) is not an interception of a *chord*, and reporting it would
///   claim conflicts on unmodified keys;
/// * a **keypad** (`#`) chord, since `KeyChord` cannot distinguish keypad `1`
///   from the main-row `1` and would otherwise claim the wrong key is taken;
/// * anything without exactly one key character left after the prefixes.
fn parse_apple_terminal_key(spec: &str) -> Option<KeyChord> {
    let (mut cmd, mut ctrl, mut alt, mut shift) = (false, false, false, false);
    let mut rest = spec;

    loop {
        let mut chars = rest.chars();
        let Some(c) = chars.next() else { break };
        match c {
            '^' => ctrl = true,
            '~' => alt = true,
            '$' => shift = true,
            '@' => cmd = true,
            // Keypad marker: skip the whole chord rather than silently
            // reporting it as the main-row key of the same name.
            '#' => return None,
            _ => break,
        }
        rest = chars.as_str();
    }

    if !(cmd || ctrl || alt || shift) {
        return None;
    }

    let mut key_chars = rest.chars();
    let ch = key_chars.next()?;
    if key_chars.next().is_some() {
        return None;
    }

    let key = match apple_terminal_function_key(ch) {
        Some(name) => name.to_string(),
        None => {
            // A remaining private-use code point is a function key we do not
            // have a name for; emitting it raw would be an unreadable glyph.
            if ('\u{F700}'..='\u{F8FF}').contains(&ch) {
                return None;
            }
            ch.to_string()
        }
    };

    Some(KeyChord::new(cmd, ctrl, alt, shift, &key))
}

/// Parse Terminal.app's preferences, as JSON produced by `export_domain_json`.
///
/// Bindings are nested per profile: `"Window Settings" -> <profile> ->
/// "keyMapBoundKeys" -> { "<spec>": "<escape sequence>" }`. Every profile is
/// scanned rather than only the default one, because the conflict question is
/// "can this chord reach jcode", and a chord rebound in any profile the user
/// might open is a chord that can be swallowed.
///
/// A binding whose value is an **empty string** is a key the user has bound to
/// send nothing. That still consumes the keystroke, so it is reported like any
/// other binding.
pub fn parse_apple_terminal_keymap(json: &str) -> Vec<DiscoveredBinding> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(profiles) = value.get("Window Settings").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (profile, settings) in profiles {
        let Some(map) = settings.get("keyMapBoundKeys").and_then(|v| v.as_object()) else {
            continue;
        };
        for spec in map.keys() {
            let Some(chord) = parse_apple_terminal_key(spec) else {
                continue;
            };
            // The same chord rebound in several profiles is one conflict, not
            // one per profile.
            if !seen.insert(chord.canonical()) {
                continue;
            }
            out.push(DiscoveredBinding {
                chord,
                source: KeySource::Terminal,
                action: format!("Terminal.app: remapped key ({profile} profile)"),
                raw: format!("keyMapBoundKeys[{spec:?}] in {profile}"),
                tool: "Terminal.app".to_string(),
                alt_side: AltSide::Unspecified,
            });
        }
    }
    out.sort_by(|a, b| a.chord.canonical().cmp(&b.chord.canonical()));
    out
}

/// Effective Terminal.app key remappings for this machine.
///
/// Terminal.app has no config file and no way to dump its keymap, but its
/// *rebound* keys live in the `com.apple.Terminal` preference domain, so they
/// are read live rather than transcribed — which is why this needs no entry in
/// [`super::drift`].
///
/// Scope limit, stated so a clean report is not over-read: the built-in menu
/// shortcuts (`Cmd+T`, `Cmd+N`, `Cmd+K`, …) are owned by AppKit and appear
/// nowhere in the plist, so only keys the user has actually remapped in
/// Settings → Profiles → Keyboard are visible here. That is a floor, not a
/// ceiling. It is also the set most likely to surprise: a default Cmd shortcut
/// is one the user knows about, while a remapping made months ago is exactly
/// the kind of thing that silently eats a jcode chord.
///
/// Returns nothing unless Terminal.app is the active terminal, so an unused
/// terminal never generates conflict noise.
pub fn read_apple_terminal_keybinds() -> Vec<DiscoveredBinding> {
    // Terminal.app sets TERM_PROGRAM=Apple_Terminal in every shell it spawns.
    if !std::env::var("TERM_PROGRAM")
        .map(|v| v.eq_ignore_ascii_case("Apple_Terminal"))
        .unwrap_or(false)
    {
        return Vec::new();
    }
    match super::macos_hotkeys::export_domain_json("com.apple.Terminal") {
        Some(json) => parse_apple_terminal_keymap(&json),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// iTerm2
// ---------------------------------------------------------------------------

/// `NSEventModifierFlagNumericPad`. iTerm2 keeps this bit in the mask it
/// stores, and `KeyChord` cannot distinguish keypad `1` from the main-row `1`,
/// so a chord carrying it is skipped rather than reported against the wrong
/// key.
const NS_NUMERIC_PAD: u64 = 0x0020_0000;

/// Parse one `Key Mappings` key, e.g. `"0xf702-0x120000"` (Ctrl+Shift+Left).
///
/// The format is `<character>-<modifier mask>`, both hex, where the character
/// is the `charactersIgnoringModifiers` code point (so non-printable keys are
/// the AppKit private-use constants, exactly as in Terminal.app) and the mask
/// is a raw `NSEventModifierFlags` value — the same encoding the macOS system
/// decoder and Rectangle already use, so the flag constants are shared. iTerm2
/// 3.5 appends a third `-<keycode>` component, which is ignored: the character
/// already identifies the key, and the extra field only disambiguates layouts.
///
/// Returns `None` for anything that is not a usable, representable chord:
///
/// * **no modifiers at all** — iTerm2 lets you map a bare key, which is not an
///   interception of a *chord*, and reporting it would manufacture conflicts on
///   unmodified keys;
/// * a chord carrying **`fn`**, skipped whole rather than having the flag
///   dropped, which would make `fn+f1` compare equal to a plain `f1`;
/// * a **numeric-keypad** chord, for the reason on [`NS_NUMERIC_PAD`];
/// * an unnamed private-use code point, which would render as an unreadable
///   glyph.
fn parse_iterm2_key(spec: &str) -> Option<KeyChord> {
    let mut parts = spec.split('-');
    let code = parse_hex_u32(parts.next()?)?;
    let mask = u64::from(parse_hex_u32(parts.next()?)?);

    use super::macos_hotkeys::{NS_COMMAND, NS_CONTROL, NS_FUNCTION, NS_OPTION, NS_SHIFT};
    if mask & NS_FUNCTION != 0 || mask & NS_NUMERIC_PAD != 0 {
        return None;
    }
    let cmd = mask & NS_COMMAND != 0;
    let ctrl = mask & NS_CONTROL != 0;
    let alt = mask & NS_OPTION != 0;
    let shift = mask & NS_SHIFT != 0;
    if !(cmd || ctrl || alt || shift) {
        return None;
    }

    let ch = char::from_u32(code)?;
    // Non-printable keys use the same `NSxxxFunctionKey` constants Terminal.app
    // writes, so the translation table is shared rather than duplicated.
    let key = match apple_terminal_function_key(ch) {
        Some(name) => name.to_string(),
        None => {
            if ('\u{F700}'..='\u{F8FF}').contains(&ch) {
                return None;
            }
            ch.to_string()
        }
    };
    Some(KeyChord::new(cmd, ctrl, alt, shift, &key))
}

/// Parse a `0x`-prefixed (or bare) hex field from an iTerm2 mapping key.
fn parse_hex_u32(field: &str) -> Option<u32> {
    let trimmed = field.trim();
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u32::from_str_radix(digits, 16).ok()
}

/// Parse iTerm2's preferences, as JSON produced by `export_domain_json`.
///
/// Bindings live in two places, both scanned: `GlobalKeyMap` at the top level
/// (mappings that apply everywhere) and a per-profile `Key Mappings` dictionary
/// under each entry of the `New Bookmarks` array. Every profile is scanned
/// rather than only the default one, because the conflict question is "can this
/// chord reach jcode", and a chord remapped in any profile the user might open
/// is a chord that can be swallowed.
///
/// The stored action is an integer opcode from iTerm2's own internal
/// enumeration, which is undocumented and has been renumbered across versions.
/// Guessing at it would send the user looking for the wrong setting, so the
/// binding is reported by *where it is configured* instead — which is what they
/// need in order to find and change it.
pub fn parse_iterm2_keymap(json: &str) -> Vec<DiscoveredBinding> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };

    let mut scopes: Vec<(String, &serde_json::Value)> = Vec::new();
    if let Some(global) = value.get("GlobalKeyMap") {
        scopes.push(("global".to_string(), global));
    }
    if let Some(profiles) = value.get("New Bookmarks").and_then(|v| v.as_array()) {
        for (index, profile) in profiles.iter().enumerate() {
            let Some(map) = profile.get("Key Mappings") else {
                continue;
            };
            let name = profile
                .get("Name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("profile #{}", index + 1));
            scopes.push((name, map));
        }
    }

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (scope, map) in scopes {
        let Some(map) = map.as_object() else { continue };
        for spec in map.keys() {
            let Some(chord) = parse_iterm2_key(spec) else {
                continue;
            };
            // The same chord mapped in several profiles is one conflict, not
            // one per profile.
            if !seen.insert(chord.canonical()) {
                continue;
            }
            let where_ = if scope == "global" {
                "global key map".to_string()
            } else {
                format!("{scope} profile")
            };
            out.push(DiscoveredBinding {
                chord,
                source: KeySource::Terminal,
                action: format!("iTerm2: key mapping ({where_})"),
                raw: format!("{spec} in {where_}"),
                tool: "iTerm2".to_string(),
                alt_side: AltSide::Unspecified,
            });
        }
    }
    out.sort_by(|a, b| a.chord.canonical().cmp(&b.chord.canonical()));
    out
}

/// Effective iTerm2 key mappings for this machine.
///
/// Like Terminal.app, iTerm2 has no config file to read and no command that
/// dumps its keymap, but its mappings live in the `com.googlecode.iterm2`
/// preference domain, so they are read **live** rather than transcribed — which
/// is why this needs no entry in [`super::drift`].
///
/// Scope limit, stated so a clean report is not over-read: iTerm2's *menu*
/// shortcuts (`Cmd+T`, `Cmd+D`, `Cmd+W`, …) are AppKit menu items and are not
/// part of `Key Mappings`, so only mappings present in Settings → Keys and
/// Settings → Profiles → Keys are visible here. That is a floor, not a ceiling,
/// and it is also the set most likely to surprise: a default menu shortcut is
/// one the user knows about, while a mapping added months ago is exactly the
/// kind of thing that silently eats a jcode chord.
///
/// Returns nothing unless iTerm2 is the active terminal, so an unused terminal
/// never generates conflict noise.
pub fn read_iterm2_keybinds() -> Vec<DiscoveredBinding> {
    // iTerm2 sets TERM_PROGRAM=iTerm.app in every shell it spawns.
    if !std::env::var("TERM_PROGRAM")
        .map(|v| v.eq_ignore_ascii_case("iTerm.app"))
        .unwrap_or(false)
    {
        return Vec::new();
    }
    match super::macos_hotkeys::export_domain_json("com.googlecode.iterm2") {
        Some(json) => parse_iterm2_keymap(&json),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A structurally faithful `com.apple.Terminal` export: bindings nested
    /// per profile under "Window Settings", with function keys written as
    /// private-use code points.
    const APPLE_TERMINAL_SAMPLE: &str = "{
  \"Default Window Settings\": \"Pro\",
  \"Window Settings\": {
    \"Basic\": {
      \"name\": \"Basic\",
      \"keyMapBoundKeys\": {
        \"^$\\uF702\": \"\\u001b[1;6D\",
        \"~\\uF704\": \"\\u001b[1;3P\",
        \"@k\": \"\"
      }
    },
    \"Pro\": {
      \"name\": \"Pro\",
      \"keyMapBoundKeys\": {
        \"^$\\uF702\": \"\\u001b[1;6D\",
        \"$\\uF72C\": \"\\u001b[5;2~\"
      }
    }
  }
}";

    fn chords(binds: &[DiscoveredBinding]) -> Vec<String> {
        binds.iter().map(|b| b.chord.canonical()).collect()
    }

    #[test]
    fn apple_terminal_decodes_modifier_prefixes_and_function_keys() {
        let binds = parse_apple_terminal_keymap(APPLE_TERMINAL_SAMPLE);
        let got = chords(&binds);
        assert!(got.contains(&"ctrl+shift+left".to_string()), "{got:?}");
        assert!(got.contains(&"alt+f1".to_string()), "{got:?}");
        assert!(got.contains(&"shift+pageup".to_string()), "{got:?}");
    }

    #[test]
    fn apple_terminal_reports_binding_that_sends_nothing() {
        // An empty escape sequence still swallows the keystroke.
        let binds = parse_apple_terminal_keymap(APPLE_TERMINAL_SAMPLE);
        assert!(chords(&binds).contains(&"cmd+k".to_string()));
    }

    #[test]
    fn apple_terminal_scans_every_profile_but_dedupes_shared_chords() {
        let binds = parse_apple_terminal_keymap(APPLE_TERMINAL_SAMPLE);
        let n = chords(&binds)
            .iter()
            .filter(|c| *c == "ctrl+shift+left")
            .count();
        assert_eq!(n, 1, "chord bound in two profiles is one conflict");
        // ...but a chord only present in the second profile is still found.
        assert!(chords(&binds).contains(&"shift+pageup".to_string()));
    }

    #[test]
    fn apple_terminal_labels_source_and_tool() {
        let binds = parse_apple_terminal_keymap(APPLE_TERMINAL_SAMPLE);
        let b = binds.first().expect("at least one binding");
        assert_eq!(b.source, KeySource::Terminal);
        assert_eq!(b.tool, "Terminal.app");
        assert_eq!(b.alt_side, AltSide::Unspecified);
    }

    #[test]
    fn apple_terminal_skips_unmodified_key() {
        // A bare rebound F1 is not an interception of a chord; reporting it
        // would claim conflicts on unmodified keys.
        assert!(parse_apple_terminal_key("\u{F704}").is_none());
        assert!(parse_apple_terminal_key("a").is_none());
    }

    #[test]
    fn apple_terminal_skips_keypad_chords() {
        // KeyChord cannot tell keypad 1 from main-row 1, so reporting it would
        // claim the wrong key is taken.
        assert!(parse_apple_terminal_key("#^1").is_none());
        assert!(parse_apple_terminal_key("^#1").is_none());
    }

    #[test]
    fn apple_terminal_skips_unknown_private_use_key() {
        // Emitting the raw code point would render as an unreadable glyph.
        assert!(parse_apple_terminal_key("^\u{F7FF}").is_none());
    }

    #[test]
    fn apple_terminal_skips_malformed_specs() {
        assert!(parse_apple_terminal_key("").is_none());
        assert!(parse_apple_terminal_key("^").is_none(), "no key char");
        assert!(parse_apple_terminal_key("^ab").is_none(), "two key chars");
    }

    #[test]
    fn apple_terminal_ignores_foreign_json_shapes() {
        // Rectangle/Raycast-shaped input must not be mistaken for a keymap.
        assert!(parse_apple_terminal_keymap("{\"raycastGlobalHotkey\":\"Command-49\"}").is_empty());
        assert!(parse_apple_terminal_keymap("not json").is_empty());
        assert!(parse_apple_terminal_keymap("{\"Window Settings\":{}}").is_empty());
    }

    #[test]
    fn apple_terminal_accepts_all_four_modifier_prefixes() {
        let chord = parse_apple_terminal_key("@^~$x").expect("full modifier set");
        assert_eq!(chord.canonical(), "cmd+ctrl+alt+shift+x");
    }

    /// A trimmed but structurally faithful `wezterm show-keys` output.
    const WEZTERM_SAMPLE: &str = "\
Default key table
-----------------
\tCTRL                 Tab                ->   ActivateTabRelative(1)
\tSHIFT | CTRL         Tab                ->   ActivateTabRelative(-1)
\tSUPER                c                  ->   CopyTo(Clipboard)
\t                     F11                ->   ToggleFullScreen
\tSHIFT | CTRL         UpArrow            ->   ScrollByLine(-1)
\tLEADER | CTRL        a                  ->   SendKey

Key Table: copy_mode
--------------------
\t                     Tab                ->   CopyMode(MoveForwardWord)
\tCTRL                 q                  ->   CopyMode(Close)

Mouse
-----
\tSHIFT                Down { streak: 1 }  ->   ExtendSelectionToMouseCursor(None)
";

    fn wezterm_chords(text: &str) -> Vec<String> {
        parse_wezterm_keys(text)
            .iter()
            .map(|b| b.chord.canonical())
            .collect()
    }

    #[test]
    fn parses_wezterm_default_table() {
        let chords = wezterm_chords(WEZTERM_SAMPLE);
        assert!(chords.contains(&"ctrl+tab".to_string()), "{chords:?}");
        assert!(chords.contains(&"ctrl+shift+tab".to_string()), "{chords:?}");
        assert!(chords.contains(&"cmd+c".to_string()), "{chords:?}");
        // A binding with no modifiers at all is still a real interception.
        assert!(chords.contains(&"f11".to_string()), "{chords:?}");
    }

    #[test]
    fn wezterm_arrow_names_are_normalized() {
        let chords = wezterm_chords(WEZTERM_SAMPLE);
        assert!(chords.contains(&"ctrl+shift+up".to_string()), "{chords:?}");
    }

    #[test]
    fn wezterm_skips_copy_and_search_mode_tables() {
        // `CTRL q` only exists in the copy_mode table, which cannot fire during
        // a normal TUI session; reporting it would be a phantom conflict.
        let chords = wezterm_chords(WEZTERM_SAMPLE);
        assert!(!chords.contains(&"ctrl+q".to_string()), "{chords:?}");
    }

    #[test]
    fn wezterm_skips_leader_sequences() {
        // LEADER makes this a two-step sequence, not a single chord.
        let chords = wezterm_chords(WEZTERM_SAMPLE);
        assert!(!chords.contains(&"ctrl+a".to_string()), "{chords:?}");
    }

    #[test]
    fn wezterm_skips_mouse_section() {
        assert!(
            parse_wezterm_keys(WEZTERM_SAMPLE)
                .iter()
                .all(|b| !b.raw.contains("streak")),
            "mouse rows must not become key bindings"
        );
    }

    #[test]
    fn wezterm_key_prefixes_are_stripped() {
        assert_eq!(wezterm_key_name("phys:Space"), "space");
        assert_eq!(wezterm_key_name("mapped:k"), "k");
    }

    #[test]
    fn wezterm_bindings_are_labelled() {
        let binds = parse_wezterm_keys(WEZTERM_SAMPLE);
        assert!(binds.iter().all(|b| b.tool == "WezTerm"));
        assert!(binds.iter().all(|b| b.source == KeySource::Terminal));
    }

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
            alt_delivery: Default::default(),
            tool_versions: Vec::new(),
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
            alt_delivery: Default::default(),
            tool_versions: Vec::new(),
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

    // -----------------------------------------------------------------
    // kitty
    // -----------------------------------------------------------------

    #[test]
    fn kitty_defaults_cover_the_chords_that_shadow_jcode() {
        let binds = kitty_default_bindings(KITTY_MOD_DEFAULT);
        let find = |c: &str| binds.iter().find(|b| b.chord.canonical() == c);

        // ctrl+shift+k / ctrl+shift+j are the interesting pair: kitty scrolls
        // by line on them, so a jcode binding there never reaches the TUI.
        assert_eq!(
            find("ctrl+shift+k").map(|b| b.action.as_str()),
            Some("scroll_line_up")
        );
        assert_eq!(
            find("ctrl+shift+j").map(|b| b.action.as_str()),
            Some("scroll_line_down")
        );
        // ctrl+tab is bound without kitty_mod and is a very common TUI chord.
        assert_eq!(
            find("ctrl+tab").map(|b| b.action.as_str()),
            Some("next_tab")
        );
        assert_eq!(
            find("ctrl+shift+tab").map(|b| b.action.as_str()),
            Some("previous_tab")
        );
        assert!(binds.iter().all(|b| b.tool == "kitty"));
        assert!(binds.iter().all(|b| b.source == KeySource::Terminal));
    }

    #[test]
    fn kitty_macos_only_defaults_do_not_leak_onto_other_platforms() {
        // The Cmd table is `only='macos'` upstream. On Linux those physical
        // keys produce no Cmd modifier at all, so reporting them would be a
        // pure fabrication.
        if cfg!(target_os = "macos") {
            return;
        }
        let binds = kitty_default_bindings(KITTY_MOD_DEFAULT);
        assert!(
            binds
                .iter()
                .all(|b| !b.chord.canonical().starts_with("cmd+")),
            "cmd bindings leaked onto a non-macOS platform"
        );
    }

    #[test]
    fn kitty_mod_defaults_to_ctrl_shift_and_is_overridable() {
        assert_eq!(parse_kitty_mod(""), "ctrl+shift");
        assert_eq!(parse_kitty_mod("font_size 12\n"), "ctrl+shift");
        assert_eq!(parse_kitty_mod("kitty_mod ctrl+alt\n"), "ctrl+alt");
        // A commented-out sample line must not be mistaken for a setting; the
        // shipped kitty.conf is entirely comments.
        assert_eq!(parse_kitty_mod("# kitty_mod ctrl+alt\n"), "ctrl+shift");
        // Last one wins, matching kitty's own last-wins option semantics.
        assert_eq!(
            parse_kitty_mod("kitty_mod ctrl+alt\nkitty_mod super\n"),
            "super"
        );
    }

    #[test]
    fn kitty_mod_override_moves_the_whole_default_table() {
        // This is the reason kitty_mod has to be read before resolving any
        // default: rebinding it relocates every built-in shortcut at once, so
        // a table hardcoded to ctrl+shift would report conflicts on chords
        // that are actually free and miss the ones that are taken.
        let binds = apply_kitty_user_config("kitty_mod ctrl+alt\n");
        let chords: Vec<String> = binds.iter().map(|b| b.chord.canonical()).collect();
        assert!(chords.contains(&"ctrl+alt+k".to_string()), "got {chords:?}");
        assert!(!chords.contains(&"ctrl+shift+k".to_string()));
    }

    #[test]
    fn kitty_map_with_no_action_frees_the_chord() {
        // `map kitty_mod+space` with no action is kitty's documented way to
        // hand a chord back to the program running in the terminal, so it must
        // stop being reported as a conflict rather than start being one.
        let (binds, unbound, cleared) = parse_kitty_maps("map kitty_mod+k\n");
        assert!(binds.is_empty());
        assert!(!cleared);
        assert_eq!(unbound.len(), 1);
        assert_eq!(unbound[0].canonical(), "ctrl+shift+k");

        let effective = apply_kitty_user_config("map kitty_mod+k\n");
        assert!(
            effective
                .iter()
                .all(|b| b.chord.canonical() != "ctrl+shift+k")
        );
    }

    #[test]
    fn kitty_no_op_frees_but_discard_event_does_not() {
        // no_op passes the key through; discard_event swallows it, so only the
        // former frees the chord.
        let (_, unbound, _) = parse_kitty_maps("map ctrl+alt+f1 no_op\n");
        assert_eq!(unbound.len(), 1);

        let (binds, unbound, _) = parse_kitty_maps("map ctrl+alt+f1 discard_event\n");
        assert!(unbound.is_empty());
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].action, "discard_event");
    }

    #[test]
    fn kitty_clear_all_shortcuts_empties_the_defaults() {
        let binds = apply_kitty_user_config("clear_all_shortcuts yes\n");
        assert!(binds.is_empty(), "got {binds:?}");
        // Bindings declared after the clear survive it.
        let binds = apply_kitty_user_config("clear_all_shortcuts yes\nmap ctrl+g new_window\n");
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].chord.canonical(), "ctrl+g");
    }

    #[test]
    fn kitty_user_map_overrides_a_default_on_the_same_chord() {
        let binds = apply_kitty_user_config("map kitty_mod+k new_window\n");
        let hit: Vec<&DiscoveredBinding> = binds
            .iter()
            .filter(|b| b.chord.canonical() == "ctrl+shift+k")
            .collect();
        assert_eq!(hit.len(), 1, "duplicate entries for one chord: {hit:?}");
        assert_eq!(hit[0].action, "new_window");
    }

    #[test]
    fn kitty_multi_key_sequences_are_not_reported() {
        // `kitty_mod+p>f` is a two-chord sequence, which we do not model. The
        // defaults table has several; none may surface as a single chord.
        assert!(kitty_chord("kitty_mod+p>f", KITTY_MOD_DEFAULT).is_none());
        let (binds, _, _) = parse_kitty_maps("map kitty_mod+p>f kitten hints\n");
        assert!(binds.is_empty());
    }

    #[test]
    fn kitty_map_options_are_skipped_before_the_chord() {
        // Every upstream default carries `--allow-fallback=shifted,ascii`, so
        // a parser that took the first token as the chord would read nothing.
        let (binds, _, _) =
            parse_kitty_maps("map --allow-fallback=shifted,ascii ctrl+g new_window\n");
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].chord.canonical(), "ctrl+g");
        assert_eq!(binds[0].action, "new_window");
    }

    #[test]
    fn kitty_hyper_and_meta_chords_are_skipped_not_downgraded() {
        // jcode has no vocabulary for hyper/meta. Dropping the modifier would
        // make `hyper+k` compare equal to plain `k` and manufacture a conflict.
        assert!(kitty_chord("hyper+k", KITTY_MOD_DEFAULT).is_none());
        assert!(kitty_chord("meta+k", KITTY_MOD_DEFAULT).is_none());
    }

    #[test]
    fn jcode_binding_on_ctrl_shift_k_conflicts_with_kitty_scroll() {
        use crate::keymap::{KeymapSnapshot, detect_conflicts};
        use jcode_config_types::KeybindingsConfig;

        let snapshot = KeymapSnapshot {
            alt_delivery: Default::default(),
            tool_versions: Vec::new(),
            version: 1,
            captured_at: String::new(),
            os: "linux".to_string(),
            terminal: "kitty".to_string(),
            terminal_version: String::new(),
            bindings: kitty_default_bindings(KITTY_MOD_DEFAULT),
        };
        let cfg = KeybindingsConfig {
            open_resume: "ctrl+shift+k".to_string(),
            ..Default::default()
        };
        let conflicts = detect_conflicts(&cfg, &snapshot);
        assert!(
            conflicts
                .iter()
                .any(|c| c.jcode.field == "keybindings.open_resume"),
            "expected ctrl+shift+k to conflict with kitty scroll_line_up, got {conflicts:?}"
        );
    }

    // -----------------------------------------------------------------
    // iTerm2
    // -----------------------------------------------------------------

    /// A structurally faithful `com.googlecode.iterm2` export: a global key
    /// map plus per-profile mappings under "New Bookmarks", keyed by
    /// `<hex character>-<hex NSEventModifierFlags>`.
    const ITERM2_SAMPLE: &str = r#"{
  "GlobalKeyMap": {
    "0xf702-0xc0000": { "Action": 10, "Text": "" }
  },
  "New Bookmarks": [
    {
      "Name": "Default",
      "Key Mappings": {
        "0x6b-0x100000": { "Action": 11, "Text": "" },
        "0xf729-0x20000": { "Action": 12, "Text": "" }
      }
    },
    {
      "Name": "Work",
      "Key Mappings": {
        "0x6b-0x100000": { "Action": 11, "Text": "" },
        "0x62-0x80000": { "Action": 13, "Text": "" }
      }
    }
  ]
}"#;

    #[test]
    fn iterm2_reads_both_global_and_per_profile_mappings() {
        let binds = parse_iterm2_keymap(ITERM2_SAMPLE);
        let chords: Vec<String> = binds.iter().map(|b| b.chord.canonical()).collect();
        // Global: Ctrl+Option+Left (0xf702 is NSLeftArrowFunctionKey).
        assert!(chords.contains(&"ctrl+alt+left".to_string()), "{chords:?}");
        // Default profile: Cmd+K and Shift+Home.
        assert!(chords.contains(&"cmd+k".to_string()), "{chords:?}");
        assert!(chords.contains(&"shift+home".to_string()), "{chords:?}");
        // A non-default profile is scanned too: a chord remapped there can
        // still swallow the keystroke whenever that profile is open.
        assert!(chords.contains(&"alt+b".to_string()), "{chords:?}");
        assert!(binds.iter().all(|b| b.tool == "iTerm2"));
        assert!(binds.iter().all(|b| b.source == KeySource::Terminal));
    }

    #[test]
    fn iterm2_chord_mapped_in_several_profiles_is_reported_once() {
        // Cmd+K is mapped in both profiles of the sample. Reporting it twice
        // would tell the user they have two conflicts to resolve when they
        // have one.
        let binds = parse_iterm2_keymap(ITERM2_SAMPLE);
        let hits = binds
            .iter()
            .filter(|b| b.chord.canonical() == "cmd+k")
            .count();
        assert_eq!(hits, 1, "{binds:?}");
    }

    #[test]
    fn iterm2_unmodified_mapping_is_not_a_conflict() {
        // iTerm2 allows mapping a bare key. That is not an interception of a
        // chord, and reporting it would manufacture conflicts on plain keys.
        assert!(parse_iterm2_key("0xf704-0x0").is_none());
        assert!(parse_iterm2_keymap(r#"{"GlobalKeyMap":{"0xf704-0x0":{"Action":1}}}"#).is_empty());
    }

    #[test]
    fn iterm2_fn_and_keypad_chords_are_skipped_whole() {
        // `fn` has no representation in KeyChord: dropping the flag would make
        // fn+F1 compare equal to a plain F1 and claim an unrelated key is
        // taken. Keypad is the same problem for digits.
        assert!(parse_iterm2_key("0xf704-0x900000").is_none());
        assert!(parse_iterm2_key("0x31-0x300000").is_none());
        // The same mask without those bits is a normal chord.
        assert_eq!(
            parse_iterm2_key("0x31-0x100000").map(|c| c.canonical()),
            Some("cmd+1".to_string())
        );
    }

    #[test]
    fn iterm2_unnamed_private_use_key_is_skipped_not_rendered_raw() {
        // U+F7XX code points we have no name for would print as an unreadable
        // glyph in the report, which is worse than saying nothing.
        assert!(parse_iterm2_key("0xf7ff-0x100000").is_none());
        // A named one decodes: 0xf704 is NSF1FunctionKey, i.e. F1 not a glyph.
        assert_eq!(
            parse_iterm2_key("0xf704-0x100000").map(|c| c.canonical()),
            Some("cmd+f1".to_string())
        );
    }

    #[test]
    fn iterm2_trailing_keycode_component_is_tolerated() {
        // iTerm2 3.5 writes a third `-<keycode>` field. A parser that required
        // exactly two components would silently see no bindings at all on
        // current versions.
        assert_eq!(
            parse_iterm2_key("0x6b-0x100000-40").map(|c| c.canonical()),
            Some("cmd+k".to_string())
        );
    }

    #[test]
    fn iterm2_malformed_input_is_ignored_not_fatal() {
        assert!(parse_iterm2_keymap("not json {{{").is_empty());
        assert!(parse_iterm2_keymap("{}").is_empty());
        assert!(parse_iterm2_key("garbage").is_none());
        assert!(parse_iterm2_key("0x6b").is_none());
    }

    #[test]
    fn jcode_binding_on_cmd_k_conflicts_with_an_iterm2_mapping() {
        use crate::keymap::{KeymapSnapshot, detect_conflicts};
        use jcode_config_types::KeybindingsConfig;

        let snapshot = KeymapSnapshot {
            alt_delivery: Default::default(),
            version: 1,
            captured_at: String::new(),
            os: "macos".to_string(),
            terminal: "iTerm2".to_string(),
            terminal_version: String::new(),
            tool_versions: Vec::new(),
            bindings: parse_iterm2_keymap(ITERM2_SAMPLE),
        };
        let cfg = KeybindingsConfig {
            open_resume: "cmd+k".to_string(),
            ..Default::default()
        };
        let conflicts = detect_conflicts(&cfg, &snapshot);
        assert!(
            conflicts
                .iter()
                .any(|c| c.jcode.field == "keybindings.open_resume"),
            "expected cmd+k to conflict with the iTerm2 mapping, got {conflicts:?}"
        );
    }
}
