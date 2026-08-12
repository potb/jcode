//! Discover global key bindings declared by third-party apps that grab hotkeys
//! *before* the terminal (and therefore jcode) ever sees them.
//!
//! macOS lets window managers and automation tools register system-wide hotkeys.
//! When one of those overlaps a key jcode wants (the classic case is a tiling WM
//! binding `Cmd+J`/`Cmd+K` to window focus, which shadows jcode's prompt
//! navigation), the keystroke never reaches the terminal and the jcode binding
//! silently does nothing. The terminal/macOS scanners cannot see these, so we
//! read the relevant app config files directly.
//!
//! Each app has its own config grammar, so there is one pure parser per app
//! (`parse_*`) plus a thin reader (`read_*`) that locates and loads the file.
//! Parsers are unit-tested without touching the machine.

use std::path::{Path, PathBuf};

use super::chord::KeyChord;
use super::source::{AltSide, DiscoveredBinding, KeySource};

/// A modifier mask accumulated while parsing a chord, kept separate from the key
/// token so apps that express the "hyper" key as a bundle of modifiers can be
/// expanded uniformly.
#[derive(Clone, Copy, Default)]
struct Mods {
    cmd: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
    /// Which physical Option key the declaration named, if any. Accumulated
    /// alongside the flags so a sided spelling is not lost when `alt` collapses.
    alt_side: AltSide,
}

impl Mods {
    fn or(self, other: Mods) -> Mods {
        Mods {
            cmd: self.cmd || other.cmd,
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
            alt_side: self.alt_side.merge(other.alt_side),
        }
    }
}

/// Apply a single modifier token to `mods`, expanding `hyper` via `hyper_mods`.
/// Returns `true` if the token was a recognized modifier, `false` if it should
/// be treated as the primary key.
///
/// Side-specific spellings matter here. skhd distinguishes left and right
/// modifiers (`lalt`, `ralt`, `lcmd`, `rcmd`, `lctrl`, `rctrl`, `lshift`,
/// `rshift`), and a config that uses them is common on layouts where the right
/// Option key is AltGr (qwerty-fr, US-International, most EU layouts): the WM
/// binds `lalt` only, precisely so `ralt` stays free for composing accented
/// characters. Treating `lalt` as an unknown token silently downgraded every
/// such binding to a bare, unmodified key, so `lalt - h` was recorded as plain
/// `h` and never matched jcode's `alt+h`. Conflict detection then reported a
/// clean bill of health on exactly the setups most likely to have conflicts.
///
/// jcode's own chords have no notion of sidedness, so both sides collapse onto
/// the single `alt`/`cmd`/`ctrl`/`shift` flag. That is the correct comparison:
/// the terminal reports "alt" regardless of which physical key produced it.
/// The side is still *recorded* in `mods.alt_side`, because a terminal that
/// sends only one Option key as Alt makes a binding on the other side
/// unreachable, and reporting it as a conflict would be a false positive.
/// Sidedness is only tracked for Option: Cmd/Ctrl/Shift have no equivalent
/// compose-key ambiguity, so their sided spellings carry no extra information.
fn apply_modifier(token: &str, mods: &mut Mods, hyper_mods: Mods) -> bool {
    match token.trim().to_ascii_lowercase().as_str() {
        "cmd" | "command" | "super" | "win" | "windows" | "lcmd" | "rcmd" => mods.cmd = true,
        "ctrl" | "control" | "lctrl" | "rctrl" => mods.ctrl = true,
        "alt" | "opt" | "option" => mods.alt = true,
        "lalt" | "lopt" => {
            mods.alt = true;
            mods.alt_side = mods.alt_side.merge(AltSide::Left);
        }
        "ralt" | "ropt" => {
            mods.alt = true;
            mods.alt_side = mods.alt_side.merge(AltSide::Right);
        }
        "shift" | "lshift" | "rshift" => mods.shift = true,
        // "hyper" is an app-defined alias for some bundle of real modifiers.
        "hyper" => *mods = mods.or(hyper_mods),
        // "fn" is not representable as a jcode modifier; ignore it so the rest of
        // the chord still parses.
        "fn" | "function" => {}
        _ => return false,
    }
    true
}

/// Parse a "Hyper" definition (e.g. OmniWM's `hyperTrigger = "Option"`, or a
/// compound like "Cmd+Ctrl+Alt+Shift") into the modifier bundle it stands for.
fn parse_hyper_mods(spec: &str) -> Mods {
    let mut mods = Mods::default();
    for token in spec.split(['+', '-']) {
        // Recurse-safe: "hyper" inside a hyper definition is meaningless, so pass
        // an empty bundle.
        apply_modifier(token, &mut mods, Mods::default());
    }
    mods
}

/// Build a chord from a list of tokens (modifiers + one key), where modifiers and
/// the key are already separated out. `hyper_mods` expands any `hyper` token.
/// Returns the chord plus which physical Option key the declaration named, or
/// `None` if no primary key token was found.
fn chord_from_tokens<'a>(
    tokens: impl IntoIterator<Item = &'a str>,
    hyper_mods: Mods,
) -> Option<(KeyChord, AltSide)> {
    let mut mods = Mods::default();
    let mut key: Option<String> = None;
    for token in tokens {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if !apply_modifier(token, &mut mods, hyper_mods) {
            // Last non-modifier token wins as the key.
            key = Some(token.to_string());
        }
    }
    let key = key?;
    Some((
        KeyChord::new(mods.cmd, mods.ctrl, mods.alt, mods.shift, &key),
        mods.alt_side,
    ))
}

// ---------------------------------------------------------------------------
// OmniWM (~/.config/omniwm/settings.toml)
// ---------------------------------------------------------------------------

/// Parse an OmniWM `settings.toml` into discovered bindings. OmniWM stores
/// hotkeys as an array of tables:
///
/// ```toml
/// hyperTrigger = "Option"
///
/// [[hotkeys]]
/// binding = "Command+J"
/// id = "focus.down"
/// ```
///
/// `binding = "Unassigned"` entries are skipped. The `hyperTrigger` value (or a
/// default of Option) expands any `Hyper+...` binding.
pub fn parse_omniwm(text: &str) -> Vec<DiscoveredBinding> {
    let Ok(value) = text.parse::<toml::Value>() else {
        return Vec::new();
    };

    // Hyper expands to the configured trigger; OmniWM defaults to Option.
    let hyper_mods = value
        .get("general")
        .and_then(|g| g.get("hyperTrigger"))
        .or_else(|| value.get("hyperTrigger"))
        .and_then(|h| h.as_str())
        .map(parse_hyper_mods)
        .unwrap_or(Mods {
            alt: true,
            ..Mods::default()
        });

    let Some(hotkeys) = value.get("hotkeys").and_then(|h| h.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for hk in hotkeys {
        let Some(binding) = hk.get("binding").and_then(|b| b.as_str()) else {
            continue;
        };
        if binding.trim().eq_ignore_ascii_case("unassigned") || binding.trim().is_empty() {
            continue;
        }
        let action = hk
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string();
        if let Some((chord, alt_side)) = chord_from_tokens(binding.split(['+', '-']), hyper_mods) {
            out.push(DiscoveredBinding {
                chord,
                source: KeySource::ExternalApp,
                action,
                raw: binding.to_string(),
                tool: "OmniWM".to_string(),
                alt_side,
            });
        }
    }
    out
}

/// Read and parse OmniWM's config, if present.
pub fn read_omniwm() -> Vec<DiscoveredBinding> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let path = home.join(".config/omniwm/settings.toml");
    read_to_string(&path)
        .map(|t| parse_omniwm(&t))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// AeroSpace (~/.aerospace.toml or ~/.config/aerospace/aerospace.toml)
// ---------------------------------------------------------------------------

/// Parse an AeroSpace config into discovered bindings. AeroSpace declares
/// bindings under `[mode.<name>.binding]` tables where the key is a chord like
/// `alt-h` and the value is the command:
///
/// ```toml
/// [mode.main.binding]
/// alt-h = 'focus left'
/// cmd-shift-l = ['move right', 'mode main']
/// ```
pub fn parse_aerospace(text: &str) -> Vec<DiscoveredBinding> {
    let Ok(value) = text.parse::<toml::Value>() else {
        return Vec::new();
    };
    let Some(modes) = value.get("mode").and_then(|m| m.as_table()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for mode in modes.values() {
        let Some(bindings) = mode.get("binding").and_then(|b| b.as_table()) else {
            continue;
        };
        for (chord_str, action_val) in bindings {
            // AeroSpace separates modifiers and key with '-'.
            let Some((chord, alt_side)) = chord_from_tokens(chord_str.split('-'), Mods::default())
            else {
                continue;
            };
            let action = aerospace_action_label(action_val);
            out.push(DiscoveredBinding {
                chord,
                source: KeySource::ExternalApp,
                action,
                raw: chord_str.clone(),
                tool: "AeroSpace".to_string(),
                alt_side,
            });
        }
    }
    out
}

fn aerospace_action_label(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join("; "),
        other => other.to_string(),
    }
}

/// Read and parse AeroSpace's config from either supported location.
pub fn read_aerospace() -> Vec<DiscoveredBinding> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    for rel in [".aerospace.toml", ".config/aerospace/aerospace.toml"] {
        let path = home.join(rel);
        if let Some(text) = read_to_string(&path) {
            return parse_aerospace(&text);
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// skhd (~/.config/skhd/skhdrc or ~/.skhdrc)
// ---------------------------------------------------------------------------

/// Parse an skhd config into discovered bindings. skhd lines look like:
///
/// ```text
/// cmd - h : yabai -m window --focus west
/// cmd + shift - 0x2C : echo hi
/// # comment
/// :: mode @ : ...        # mode declaration, ignored
/// ```
///
/// The activation (left of the first `:`) is `mods - key`, where modifiers are
/// joined with `+` and separated from the key by `-`.
pub fn parse_skhd(text: &str) -> Vec<DiscoveredBinding> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("::") {
            continue;
        }
        // Activation is everything before the first ':' that starts the command.
        let Some(colon) = line.find(':') else {
            continue;
        };
        let activation = line[..colon].trim();
        let action = line[colon + 1..].trim();
        if activation.is_empty() {
            continue;
        }
        // Strip an optional leading mode list ("mode_name <") if present.
        let activation = activation.rsplit('<').next().unwrap_or(activation).trim();

        // Split modifiers from key on the first '-'. Modifiers use '+'.
        let (mods_part, key_part) = match activation.split_once('-') {
            Some((m, k)) => (m, k),
            None => ("", activation),
        };
        let tokens = mods_part
            .split('+')
            .chain(std::iter::once(key_part))
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some((chord, alt_side)) = chord_from_tokens(tokens, Mods::default()) {
            out.push(DiscoveredBinding {
                chord,
                source: KeySource::ExternalApp,
                action: action.to_string(),
                raw: activation.to_string(),
                tool: "skhd".to_string(),
                alt_side,
            });
        }
    }
    out
}

/// Read and parse skhd's config from either supported location.
pub fn read_skhd() -> Vec<DiscoveredBinding> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    for rel in [".config/skhd/skhdrc", ".skhdrc"] {
        let path = home.join(rel);
        if let Some(text) = read_to_string(&path) {
            return parse_skhd(&text);
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Karabiner-Elements (~/.config/karabiner/karabiner.json)
// ---------------------------------------------------------------------------

/// Map a Karabiner `key_code` spelling onto jcode's vocabulary. Karabiner uses
/// its own names for most non-alphanumeric keys, so they have to be translated
/// before [`KeyChord::normalize_key`] can canonicalize them. Unknown names pass
/// through unchanged (single letters and digits already match).
fn karabiner_key_token(name: &str) -> &str {
    match name {
        "spacebar" => "space",
        "return_or_enter" => "enter",
        "delete_or_backspace" => "backspace",
        "delete_forward" => "delete",
        "escape" => "esc",
        "up_arrow" => "up",
        "down_arrow" => "down",
        "left_arrow" => "left",
        "right_arrow" => "right",
        "page_up" => "pageup",
        "page_down" => "pagedown",
        "hyphen" => "-",
        "equal_sign" => "=",
        "open_bracket" => "[",
        "close_bracket" => "]",
        "backslash" => "\\",
        "semicolon" => ";",
        "quote" => "'",
        "grave_accent_and_tilde" => "`",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        other => other,
    }
}

/// Translate a Karabiner modifier name into a chord token. Karabiner names the
/// sides explicitly (`left_command`) as well as offering the side-agnostic form
/// (`command`), and both must collapse onto jcode's single flag.
///
/// `any` is deliberately not translated: it means "with any modifiers", which
/// describes a family of chords rather than one, and expanding it would
/// manufacture conflicts that may not exist.
fn karabiner_modifier(name: &str) -> Option<&'static str> {
    Some(match name {
        "command" | "left_command" | "right_command" => "cmd",
        "control" | "left_control" | "right_control" => "ctrl",
        "option" => "alt",
        "left_option" => "lalt",
        "right_option" => "ralt",
        "shift" | "left_shift" | "right_shift" => "shift",
        "fn" => "fn",
        _ => return None,
    })
}

/// Parse a Karabiner-Elements `karabiner.json` into discovered bindings.
///
/// Karabiner matters more than any other app here: it remaps at the HID level,
/// *before* the window server, so its rules win over the window manager, the
/// terminal, and jcode alike. A machine running a Karabiner rule on a chord
/// jcode wants will never deliver that chord, and nothing downstream can tell.
///
/// Only the **selected** profile is read, since the others are inert. Within it
/// we take `complex_modifications` manipulators of type `basic`, using the
/// `from` chord and its **mandatory** modifiers (optional modifiers merely
/// tolerate extra keys and do not define the trigger).
///
/// Manipulators scoped to particular applications via a
/// `frontmost_application_if`/`unless` condition are skipped: whether they apply
/// to the terminal depends on a bundle-identifier list we would have to match
/// against the running terminal, and guessing wrong invents a conflict that the
/// user cannot find. Unconditional rules, which are the common case, are
/// reported.
pub fn parse_karabiner(text: &str) -> Vec<DiscoveredBinding> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(profiles) = value.get("profiles").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    // Exactly one profile is active at a time; the rest are stored but inert.
    // If none is flagged, Karabiner falls back to the first.
    let profile = profiles
        .iter()
        .find(|p| p.get("selected").and_then(serde_json::Value::as_bool) == Some(true))
        .or_else(|| profiles.first());
    let Some(profile) = profile else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let rules = profile
        .get("complex_modifications")
        .and_then(|c| c.get("rules"))
        .and_then(|r| r.as_array());
    for rule in rules.into_iter().flatten() {
        let rule_desc = rule
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let manipulators = rule.get("manipulators").and_then(|m| m.as_array());
        for man in manipulators.into_iter().flatten() {
            // Only "basic" manipulators describe a key trigger; the type key is
            // optional and defaults to "basic".
            let kind = man
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("basic");
            if kind != "basic" {
                continue;
            }
            if manipulator_is_app_scoped(man) {
                continue;
            }
            let Some(from) = man.get("from") else {
                continue;
            };
            let Some(key_code) = from.get("key_code").and_then(serde_json::Value::as_str) else {
                // `consumer_key_code`, `pointing_button`, `any` and
                // `simultaneous` triggers are not single chords.
                continue;
            };

            let mut mods = Mods::default();
            let mandatory = from
                .get("modifiers")
                .and_then(|m| m.get("mandatory"))
                .and_then(|m| m.as_array());
            for m in mandatory.into_iter().flatten() {
                let Some(name) = m.as_str() else { continue };
                if let Some(token) = karabiner_modifier(name) {
                    apply_modifier(token, &mut mods, Mods::default());
                }
            }

            let key = karabiner_key_token(key_code);
            let chord = KeyChord::new(mods.cmd, mods.ctrl, mods.alt, mods.shift, key);
            let action = if rule_desc.is_empty() {
                man.get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("remapped by Karabiner")
                    .to_string()
            } else {
                rule_desc.to_string()
            };
            out.push(DiscoveredBinding {
                chord,
                source: KeySource::ExternalApp,
                action,
                raw: key_code.to_string(),
                tool: "Karabiner-Elements".to_string(),
                alt_side: mods.alt_side,
            });
        }
    }
    out
}

/// True if the manipulator only fires for particular applications, which makes
/// its effect on the terminal unknowable from the config alone.
fn manipulator_is_app_scoped(man: &serde_json::Value) -> bool {
    let Some(conditions) = man.get("conditions").and_then(|c| c.as_array()) else {
        return false;
    };
    conditions.iter().any(|c| {
        matches!(
            c.get("type").and_then(serde_json::Value::as_str),
            Some("frontmost_application_if") | Some("frontmost_application_unless")
        )
    })
}

/// Read and parse Karabiner-Elements' config.
pub fn read_karabiner() -> Vec<DiscoveredBinding> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let path = home.join(".config/karabiner/karabiner.json");
    match read_to_string(&path) {
        Some(text) => parse_karabiner(&text),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Hammerspoon (~/.hammerspoon/init.lua)
// ---------------------------------------------------------------------------

/// Parse a Hammerspoon `init.lua` for global hotkeys.
///
/// Hammerspoon is a Lua runtime, so a complete answer would require executing
/// the config. We deliberately read only the literal, unambiguous form:
///
/// ```lua
/// hs.hotkey.bind({"cmd", "alt"}, "K", function() ... end)
/// ```
///
/// Bindings whose modifier argument is a variable (the widespread
/// `hs.hotkey.bind(hyper, "j", ...)` idiom, where `hyper` is defined elsewhere)
/// are skipped rather than guessed at. That makes this scanner incomplete by
/// construction, which is the right trade: a missed binding leaves detection no
/// worse than today, while a guessed one would point the user at a conflict
/// that does not exist.
pub fn parse_hammerspoon(text: &str) -> Vec<DiscoveredBinding> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("--") {
            continue;
        }
        let mut rest = line;
        // A single line can carry more than one bind call.
        while let Some(idx) = rest.find("hs.hotkey.bind") {
            let after = &rest[idx + "hs.hotkey.bind".len()..];
            rest = after;
            // Skip bindSpec and any other suffixed variant.
            let Some(open) = after.find('(') else {
                continue;
            };
            if after[..open].chars().any(|c| c.is_alphanumeric()) {
                continue;
            }
            let args = &after[open + 1..];
            // Modifier list must be a literal Lua table.
            let args_trimmed = args.trim_start();
            if !args_trimmed.starts_with('{') {
                continue;
            }
            let Some(close) = args_trimmed.find('}') else {
                continue;
            };
            let mods_src = &args_trimmed[1..close];
            let mut mods = Mods::default();
            for token in mods_src.split(',') {
                let token = token.trim().trim_matches(['"', '\'']).trim();
                if token.is_empty() {
                    continue;
                }
                apply_modifier(token, &mut mods, Mods::default());
            }
            // The key is the next quoted string after the table.
            let tail = &args_trimmed[close + 1..];
            let Some((key, _)) = next_quoted(tail) else {
                continue;
            };
            if key.is_empty() {
                continue;
            }
            out.push(DiscoveredBinding {
                chord: KeyChord::new(mods.cmd, mods.ctrl, mods.alt, mods.shift, &key),
                source: KeySource::ExternalApp,
                action: "Hammerspoon hotkey".to_string(),
                raw: line.to_string(),
                tool: "Hammerspoon".to_string(),
                alt_side: mods.alt_side,
            });
        }
    }
    out
}

/// Return the first single- or double-quoted string in `s`, plus the offset just
/// past its closing quote.
fn next_quoted(s: &str) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'"' || b == b'\'')?;
    let quote = bytes[start];
    let end = bytes[start + 1..].iter().position(|&b| b == quote)? + start + 1;
    Some((s[start + 1..end].to_string(), end + 1))
}

/// Read and parse Hammerspoon's config.
pub fn read_hammerspoon() -> Vec<DiscoveredBinding> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let path = home.join(".hammerspoon/init.lua");
    match read_to_string(&path) {
        Some(text) => parse_hammerspoon(&text),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

/// Read every supported external app's bindings on this machine.
///
/// yabai is intentionally absent: it registers no hotkeys of its own. `yabairc`
/// is a shell script of `yabai -m config` calls, and every yabai key binding in
/// practice lives in skhd, which is already scanned above. Adding a `yabairc`
/// parser would find nothing to parse.
pub fn read_external_bindings() -> Vec<DiscoveredBinding> {
    let mut out = Vec::new();
    out.extend(read_omniwm());
    out.extend(read_aerospace());
    out.extend(read_skhd());
    out.extend(read_karabiner());
    out.extend(read_hammerspoon());
    out
}

fn read_to_string(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Exposed for tests/diagnostics: the config paths we look for, relative to the
/// home directory.
pub fn external_config_paths() -> Vec<PathBuf> {
    [
        ".config/omniwm/settings.toml",
        ".aerospace.toml",
        ".config/aerospace/aerospace.toml",
        ".config/skhd/skhdrc",
        ".skhdrc",
        ".config/karabiner/karabiner.json",
        ".hammerspoon/init.lua",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KARABINER_SAMPLE: &str = r#"{
  "profiles": [
    {
      "name": "Unused",
      "selected": false,
      "complex_modifications": {
        "rules": [
          {
            "description": "inert profile rule",
            "manipulators": [
              { "type": "basic",
                "from": { "key_code": "z", "modifiers": { "mandatory": ["command"] } },
                "to": [{ "key_code": "y" }] }
            ]
          }
        ]
      }
    },
    {
      "name": "Default",
      "selected": true,
      "complex_modifications": {
        "rules": [
          {
            "description": "Window focus",
            "manipulators": [
              { "type": "basic",
                "from": { "key_code": "j", "modifiers": { "mandatory": ["left_command"] } },
                "to": [{ "key_code": "down_arrow" }] },
              { "type": "basic",
                "from": { "key_code": "spacebar", "modifiers": { "mandatory": ["option", "shift"] } },
                "to": [{ "key_code": "tab" }] }
            ]
          }
        ]
      }
    }
  ]
}"#;

    #[test]
    fn karabiner_reads_only_the_selected_profile() {
        let binds = parse_karabiner(KARABINER_SAMPLE);
        // The unselected profile's cmd+z rule is inert and must not be reported.
        assert!(!binds.iter().any(|b| b.chord.canonical() == "cmd+z"));
        assert_eq!(binds.len(), 2);
        assert!(binds.iter().any(|b| b.chord.canonical() == "cmd+j"));
    }

    #[test]
    fn karabiner_translates_key_names_and_sided_modifiers() {
        let binds = parse_karabiner(KARABINER_SAMPLE);
        // left_command collapses onto cmd, spacebar onto space.
        let j = binds
            .iter()
            .find(|b| b.chord.canonical() == "cmd+j")
            .expect("cmd+j");
        assert_eq!(j.tool, "Karabiner-Elements");
        assert_eq!(j.source, KeySource::ExternalApp);
        assert_eq!(j.action, "Window focus");
        assert!(
            binds
                .iter()
                .any(|b| b.chord.canonical() == "alt+shift+space")
        );
    }

    #[test]
    fn karabiner_ignores_optional_modifiers() {
        // Optional modifiers merely tolerate extra keys; only mandatory ones
        // define the trigger, so this is ctrl+k and not ctrl+shift+k.
        let cfg = r#"{"profiles":[{"selected":true,"complex_modifications":{"rules":[
          {"description":"r","manipulators":[
            {"type":"basic","from":{"key_code":"k","modifiers":{"mandatory":["control"],"optional":["shift"]}}}]}]}}]}"#;
        let binds = parse_karabiner(cfg);
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].chord.canonical(), "ctrl+k");
    }

    #[test]
    fn karabiner_skips_app_scoped_and_non_key_triggers() {
        // A rule limited to specific apps may not apply to the terminal, and a
        // pointing-button trigger is not a chord at all. Reporting either would
        // send the user hunting for a conflict that is not there.
        let cfg = r#"{"profiles":[{"selected":true,"complex_modifications":{"rules":[
          {"description":"scoped","manipulators":[
            {"type":"basic","from":{"key_code":"t","modifiers":{"mandatory":["command"]}},
             "conditions":[{"type":"frontmost_application_if","bundle_identifiers":["^com\\.apple\\.Safari$"]}]}]},
          {"description":"mouse","manipulators":[
            {"type":"basic","from":{"pointing_button":"button1"}}]}]}}]}"#;
        assert!(parse_karabiner(cfg).is_empty());
    }

    #[test]
    fn karabiner_tolerates_malformed_config() {
        assert!(parse_karabiner("not json").is_empty());
        assert!(parse_karabiner("{}").is_empty());
    }

    #[test]
    fn hammerspoon_parses_literal_modifier_tables() {
        let cfg = r#"
-- hs.hotkey.bind({"cmd"}, "Q", quit)   commented out, must be ignored
hs.hotkey.bind({"cmd", "alt"}, "K", function() hs.alert("hi") end)
hs.hotkey.bind({'ctrl','shift'}, 'Left', nil, function() end)
"#;
        let binds = parse_hammerspoon(cfg);
        assert_eq!(binds.len(), 2);
        assert_eq!(binds[0].chord.canonical(), "cmd+alt+k");
        assert_eq!(binds[0].tool, "Hammerspoon");
        assert_eq!(binds[0].source, KeySource::ExternalApp);
        // "Left" normalizes onto jcode's arrow token.
        assert_eq!(binds[1].chord.canonical(), "ctrl+shift+left");
    }

    #[test]
    fn hammerspoon_skips_variable_modifier_bindings() {
        // `hyper` is defined elsewhere in Lua and we do not execute the config,
        // so this binding is unknowable. Skipping it keeps detection no worse
        // than before; guessing would invent a conflict.
        let cfg = r#"
local hyper = {"cmd", "alt", "ctrl", "shift"}
hs.hotkey.bind(hyper, "j", function() end)
"#;
        assert!(parse_hammerspoon(cfg).is_empty());
    }

    #[test]
    fn external_config_paths_include_the_new_sources() {
        let paths = external_config_paths();
        assert!(paths.contains(&PathBuf::from(".config/karabiner/karabiner.json")));
        assert!(paths.contains(&PathBuf::from(".hammerspoon/init.lua")));
    }

    #[test]
    fn omniwm_cmd_jk_focus_bindings() {
        let cfg = r#"
[general]
hyperTrigger = "Option"

[[hotkeys]]
binding = "Command+J"
id = "focus.down"

[[hotkeys]]
binding = "Command+K"
id = "focus.up"

[[hotkeys]]
binding = "Unassigned"
id = "focusPrevious"

[[hotkeys]]
binding = "Hyper+1"
id = "switchWorkspace.0"
"#;
        let binds = parse_omniwm(cfg);
        // Cmd+J, Cmd+K, and Hyper(=Option=alt)+1; Unassigned is skipped.
        assert_eq!(binds.len(), 3);
        let jk: Vec<_> = binds
            .iter()
            .filter(|b| b.chord.canonical() == "cmd+j" || b.chord.canonical() == "cmd+k")
            .collect();
        assert_eq!(jk.len(), 2);
        for b in &jk {
            assert_eq!(b.source, KeySource::ExternalApp);
            assert_eq!(b.tool, "OmniWM");
        }
        // Hyper+1 expands to alt+1 (Option trigger).
        assert!(binds.iter().any(|b| b.chord.canonical() == "alt+1"));
    }

    #[test]
    fn omniwm_hyper_defaults_to_option_when_unset() {
        let cfg = r#"
[[hotkeys]]
binding = "Hyper+2"
id = "switchWorkspace.1"
"#;
        let binds = parse_omniwm(cfg);
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].chord.canonical(), "alt+2");
    }

    #[test]
    fn omniwm_cmd_shift_move_bindings() {
        let cfg = r#"
[[hotkeys]]
binding = "Command+Shift+K"
id = "move.up"
"#;
        let binds = parse_omniwm(cfg);
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].chord.canonical(), "cmd+shift+k");
        assert_eq!(binds[0].action, "move.up");
    }

    #[test]
    fn aerospace_binding_section() {
        let cfg = r#"
[mode.main.binding]
alt-h = 'focus left'
cmd-shift-l = ['move right', 'mode main']
"#;
        let binds = parse_aerospace(cfg);
        assert_eq!(binds.len(), 2);
        let h = binds
            .iter()
            .find(|b| b.chord.canonical() == "alt+h")
            .unwrap();
        assert_eq!(h.action, "focus left");
        assert_eq!(h.tool, "AeroSpace");
        let l = binds
            .iter()
            .find(|b| b.chord.canonical() == "cmd+shift+l")
            .unwrap();
        assert_eq!(l.action, "move right; mode main");
    }

    #[test]
    fn skhd_basic_lines() {
        let cfg = "\
# comment\n\
cmd - h : yabai -m window --focus west\n\
cmd + shift - j : yabai -m window --swap south\n\
:: default : echo mode\n\
";
        let binds = parse_skhd(cfg);
        assert_eq!(binds.len(), 2);
        assert_eq!(binds[0].chord.canonical(), "cmd+h");
        assert_eq!(binds[0].tool, "skhd");
        assert_eq!(binds[1].chord.canonical(), "cmd+shift+j");
    }

    #[test]
    fn skhd_keyless_modifier_only_is_skipped() {
        // A bare modifier with no key cannot form a chord.
        let binds = parse_skhd("cmd - : noop\n");
        assert!(binds.is_empty());
    }

    #[test]
    fn skhd_records_which_option_side_was_declared() {
        // The modifier flag must still collapse (the terminal reports plain
        // "alt"), but the declared side is retained so a terminal that only
        // delivers one Option key can suppress the unreachable half.
        let binds = parse_skhd(
            "lalt - h : aerospace focus left\n\
             ralt - e : echo altgr\n\
             alt - j : aerospace focus down\n\
             lalt + ralt - k : both sides\n",
        );
        let side = |key: &str| {
            binds
                .iter()
                .find(|b| b.chord.key == key)
                .unwrap_or_else(|| panic!("no binding for {key}"))
                .alt_side
        };
        assert!(binds.iter().all(|b| b.chord.alt), "alt flag must survive");
        assert_eq!(side("h"), AltSide::Left);
        assert_eq!(side("e"), AltSide::Right);
        assert_eq!(
            side("j"),
            AltSide::Unspecified,
            "unsided spelling stays unsided"
        );
        assert_eq!(
            side("k"),
            AltSide::Unspecified,
            "naming both sides is equivalent to side-agnostic"
        );
    }

    #[test]
    fn karabiner_records_which_option_side_was_declared() {
        let json = r#"{
          "profiles": [{
            "selected": true,
            "complex_modifications": { "rules": [{
              "description": "left only",
              "manipulators": [{
                "type": "basic",
                "from": { "key_code": "h", "modifiers": { "mandatory": ["left_option"] } }
              }]
            }] }
          }]
        }"#;
        let binds = parse_karabiner(json);
        assert_eq!(binds.len(), 1);
        assert!(binds[0].chord.alt);
        assert_eq!(binds[0].alt_side, AltSide::Left);
    }

    #[test]
    fn sided_cmd_ctrl_shift_do_not_set_an_option_side() {
        // Only Option has the compose-key ambiguity. A sided Cmd spelling must
        // not be mistaken for an Option side, or an unrelated binding would be
        // silently suppressed.
        let binds = parse_skhd("lcmd + rshift - h : something\n");
        assert_eq!(binds.len(), 1);
        assert!(!binds[0].chord.alt);
        assert_eq!(binds[0].alt_side, AltSide::Unspecified);
    }

    #[test]
    fn skhd_side_specific_modifiers_keep_their_modifier() {
        // Regression: `lalt`/`ralt` (and the other l*/r* spellings) used to fall
        // through `apply_modifier` as unrecognized tokens. The token was then
        // treated as the primary key and overwritten by the real key, so
        // `lalt - h` parsed as a bare `h`. Every binding in a left/right-aware
        // skhd config lost its modifier and could never match a jcode chord,
        // which made conflict detection silently report "no conflicts".
        //
        // This layout is the norm when the right Option key is AltGr: the WM
        // binds `lalt` only so `ralt` stays free for accented characters.
        let cfg = "\
lalt - h : aerospace split horizontal\n\
lalt + shift - 1 : aerospace move-node-to-workspace 1\n\
ralt - e : echo altgr\n\
lcmd - k : echo left cmd\n\
rctrl - g : echo right ctrl\n\
";
        let binds = parse_skhd(cfg);
        let chords: Vec<String> = binds.iter().map(|b| b.chord.canonical()).collect();
        assert_eq!(
            chords,
            vec!["alt+h", "alt+shift+1", "alt+e", "cmd+k", "ctrl+g"],
            "side-specific modifiers must collapse onto jcode's sideless flags"
        );
    }

    #[test]
    fn skhd_lalt_binding_is_detected_as_a_conflict() {
        // End-to-end guard for the bug above: a jcode `alt+h` binding and an
        // skhd `lalt - h` binding are the same physical chord, and skhd wins
        // because it grabs the key before the terminal sees it. Detection must
        // surface that rather than reporting a clean keymap.
        use crate::keymap::{KeymapSnapshot, detect_conflicts};
        use jcode_config_types::KeybindingsConfig;

        let snapshot = KeymapSnapshot {
            alt_delivery: Default::default(),
            version: 1,
            captured_at: String::new(),
            os: "macos".to_string(),
            terminal: "Alacritty".to_string(),
            terminal_version: String::new(),
            bindings: parse_skhd("lalt - h : aerospace split horizontal\n"),
        };
        let cfg = KeybindingsConfig {
            workspace_left: "alt+h".to_string(),
            ..Default::default()
        };
        let conflicts = detect_conflicts(&cfg, &snapshot);
        assert!(
            conflicts
                .iter()
                .any(|c| c.jcode.field == "keybindings.workspace_left"),
            "expected skhd's lalt-h to be reported against jcode's alt+h, got {conflicts:?}"
        );
    }

    #[test]
    fn malformed_toml_yields_nothing() {
        assert!(parse_omniwm("this is = = not toml").is_empty());
        assert!(parse_aerospace("[[[bad").is_empty());
    }
}
