//! Human-readable rendering of the keymap snapshot and detected conflicts,
//! shared by the `/keys` command and the startup conflict hint.

use jcode_config_types::KeybindingsConfig;

use super::KeymapSnapshot;
use super::conflicts::{Conflict, detect_conflicts};
use super::source::KeySource;

/// Render a full diagnostic report: detected terminal, discovered binding
/// counts, and any conflicts with jcode's configured bindings.
pub fn render_report(cfg: &KeybindingsConfig, snapshot: &KeymapSnapshot) -> String {
    let mut out = String::new();
    out.push_str("Keymap diagnostics\n");
    out.push_str(&format!(
        "Terminal: {}{}\n",
        snapshot.terminal,
        if snapshot.terminal_version.is_empty() {
            String::new()
        } else {
            format!(" {}", snapshot.terminal_version)
        }
    ));
    out.push_str(&format!("OS: {}\n", snapshot.os));

    let term_count = snapshot.from_source(KeySource::Terminal).count();
    let sys_count = snapshot.from_source(KeySource::MacosSystem).count();
    let app_count = snapshot.from_source(KeySource::ExternalApp).count();
    out.push_str(&format!(
        "Discovered bindings: {term_count} terminal, {sys_count} macOS system, {app_count} app\n",
    ));

    if app_count > 0 {
        let mut tools: Vec<&str> = snapshot
            .from_source(KeySource::ExternalApp)
            .map(|b| b.tool.as_str())
            .filter(|t| !t.is_empty())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        tools.dedup();
        if !tools.is_empty() {
            out.push_str(&format!("Apps scanned: {}\n", tools.join(", ")));
        }
    }

    if term_count == 0 && sys_count == 0 && app_count == 0 {
        out.push_str(
            "\nNo machine bindings were discovered. jcode can read Ghostty bindings, macOS\n\
             system shortcuts, and a few window managers (OmniWM, AeroSpace, skhd); other\n\
             terminals and tools are not yet inspected, so conflicts there will not be\n\
             detected.\n",
        );
    }

    // The Option-key state goes after the discovery summary and before the
    // conflict list: it is not a conflict, but it decides whether any `alt+`
    // conflict listed below could even fire.
    if let Some(note) = super::alt::explain(snapshot.alt_delivery, &snapshot.terminal) {
        out.push_str(&format!("\n⚠ {note}\n"));
    }

    let conflicts = detect_conflicts(cfg, snapshot);
    out.push('\n');
    if conflicts.is_empty() {
        out.push_str("No conflicts found between your jcode keybindings and the machine.\n");
    } else {
        out.push_str(&format!(
            "{} potential conflict{} found:\n\n",
            conflicts.len(),
            if conflicts.len() == 1 { "" } else { "s" }
        ));
        for c in &conflicts {
            out.push_str(&render_conflict_block(c));
            out.push('\n');
        }
        out.push_str(
            "These keys may be captured by your terminal, macOS, or another app (window\n\
             manager, launcher) before jcode sees them.\n\
             To fix: rebind the jcode action in ~/.jcode/config.toml under [keybindings],\n\
             or change the conflicting shortcut in the other app's settings.\n",
        );
    }

    out
}

fn render_conflict_block(c: &Conflict) -> String {
    let interceptor_desc = match c.interceptor.source {
        KeySource::MacosSystem => format!("macOS: {}", c.interceptor.action),
        KeySource::Terminal => {
            if c.interceptor.action.is_empty() {
                "terminal action".to_string()
            } else {
                format!("terminal: {}", c.interceptor.action)
            }
        }
        KeySource::ExternalApp => {
            let tool = if c.interceptor.tool.is_empty() {
                "app"
            } else {
                c.interceptor.tool.as_str()
            };
            if c.interceptor.action.is_empty() {
                tool.to_string()
            } else {
                format!("{tool}: {}", c.interceptor.action)
            }
        }
    };
    format!(
        "  ⚠ {key}\n      jcode: {action} ({field} = \"{raw}\")\n      taken by {interceptor}\n",
        key = c.jcode.chord.display(),
        action = c.jcode.action,
        field = c.jcode.field,
        raw = c.jcode.raw,
        interceptor = interceptor_desc,
    )
}

/// A compact one-line status string suitable for a startup notice, or `None`
/// when there are no conflicts.
pub fn render_status_line(cfg: &KeybindingsConfig, snapshot: &KeymapSnapshot) -> Option<String> {
    let conflicts = detect_conflicts(cfg, snapshot);
    if conflicts.is_empty() {
        // A degraded Option key is not a conflict, but it has the same symptom:
        // the binding silently does nothing. Surface it on its own rather than
        // reporting a clean bill of health.
        if alt_warning_applies(snapshot.alt_delivery, jcode_has_alt_binding(cfg)) {
            return super::alt::status_phrase(snapshot.alt_delivery, &snapshot.terminal);
        }
        return None;
    }
    let keys: Vec<String> = conflicts
        .iter()
        .map(|c| c.jcode.chord.display())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    Some(format!(
        "Keybinding conflict: {} may be intercepted by your terminal/OS/apps. Run /keys for details.",
        keys.join(", ")
    ))
}

/// Whether any configured jcode binding uses Alt. Without one, a terminal that
/// swallows Option costs the user nothing and must not be reported.
fn jcode_has_alt_binding(cfg: &KeybindingsConfig) -> bool {
    super::conflicts::jcode_bindings(cfg)
        .iter()
        .any(|b| b.chord.alt)
}

/// Whether a degraded Option key is worth warning about: only when it is
/// actually degraded *and* the user has an `alt+` binding that it would break.
fn alt_warning_applies(delivery: super::AltDelivery, has_alt_binding: bool) -> bool {
    delivery.is_degraded() && has_alt_binding
}

/// The Alt-delivery contribution to the startup-hint debounce signature, or
/// `None` when there is nothing to say. Without this, a machine with zero
/// conflicts produces an empty signature and the notice is treated as
/// "resolved silently", i.e. never shown.
pub fn alt_notice_signature(cfg: &KeybindingsConfig, snapshot: &KeymapSnapshot) -> Option<String> {
    alt_warning_applies(snapshot.alt_delivery, jcode_has_alt_binding(cfg)).then(|| {
        format!(
            "alt-delivery|{}|{}",
            snapshot.terminal,
            snapshot.alt_delivery.token()
        )
    })
}

/// Conflicts that exist under `after` but did not exist under `before`.
///
/// Startup already reports the conflicts a user's config has; the interesting
/// event when the config is *written* is the one the write just created. So
/// this diffs the two conflict sets and keeps only what is new, which keeps a
/// user who edits an unrelated setting from being re-told about a conflict
/// they already chose to live with.
///
/// Identity is `(jcode field, jcode chord, interceptor chord)`, matching
/// [`super::conflicts::conflict_signature`]: rebinding a conflicting action
/// onto a different but still-occupied chord is genuinely a new conflict and
/// must be reported.
pub fn new_conflicts(
    before: &KeybindingsConfig,
    after: &KeybindingsConfig,
    snapshot: &KeymapSnapshot,
) -> Vec<Conflict> {
    let existing: std::collections::HashSet<(String, String, String)> =
        detect_conflicts(before, snapshot)
            .iter()
            .map(conflict_identity)
            .collect();
    detect_conflicts(after, snapshot)
        .into_iter()
        .filter(|c| !existing.contains(&conflict_identity(c)))
        .collect()
}

fn conflict_identity(c: &Conflict) -> (String, String, String) {
    (
        c.jcode.field.clone(),
        c.jcode.chord.canonical(),
        c.interceptor.chord.canonical(),
    )
}

/// Report for a config write that introduced keybinding conflicts, or `None`
/// when the edit created none.
///
/// Deliberately quiet about pre-existing conflicts and about Alt delivery: the
/// startup hint owns those, and repeating them on every unrelated config write
/// is how a useful warning becomes noise that gets ignored.
pub fn render_new_conflict_notice(
    before: &KeybindingsConfig,
    after: &KeybindingsConfig,
    snapshot: &KeymapSnapshot,
) -> Option<String> {
    let conflicts = new_conflicts(before, after, snapshot);
    if conflicts.is_empty() {
        return None;
    }
    let mut out = format!(
        "WARNING: this config edit introduced {} keybinding conflict{}:\n\n",
        conflicts.len(),
        if conflicts.len() == 1 { "" } else { "s" }
    );
    for c in &conflicts {
        out.push_str(&render_conflict_block(c));
        out.push('\n');
    }
    out.push_str(
        "These keys may be captured by your terminal, macOS, or another app before jcode\n\
         sees them, so the binding you just set may do nothing. Pick a different chord,\n\
         or change the conflicting shortcut in the other app. Run /keys for the full report.",
    );
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::KeyChord;
    use crate::keymap::source::DiscoveredBinding;

    fn snapshot_with(bindings: Vec<DiscoveredBinding>) -> KeymapSnapshot {
        KeymapSnapshot {
            version: 1,
            captured_at: "0".to_string(),
            os: "macos".to_string(),
            terminal: "Ghostty".to_string(),
            terminal_version: "1.3.1".to_string(),
            alt_delivery: crate::keymap::AltDelivery::Unknown,
            bindings,
        }
    }

    fn term(keys: &str, action: &str) -> DiscoveredBinding {
        DiscoveredBinding {
            chord: KeyChord::parse(keys).unwrap(),
            source: KeySource::Terminal,
            action: action.to_string(),
            raw: format!("{keys}={action}"),
            tool: String::new(),
        }
    }

    #[test]
    fn report_lists_conflicts_with_field_names() {
        let cfg = KeybindingsConfig::default();
        let snap = snapshot_with(vec![term("ctrl+tab", "next_tab")]);
        let report = render_report(&cfg, &snap);
        assert!(report.contains("Ghostty 1.3.1"));
        assert!(report.contains("keybindings.model_switch_next"));
        assert!(report.contains("next_tab"));
        assert!(report.contains("Ctrl+Tab"));
    }

    #[test]
    fn report_says_clean_when_no_conflicts() {
        let cfg = KeybindingsConfig::default();
        let snap = snapshot_with(vec![term("cmd+t", "new_tab")]);
        let report = render_report(&cfg, &snap);
        assert!(report.contains("No conflicts found"));
    }

    #[test]
    fn status_line_present_only_on_conflict() {
        let cfg = KeybindingsConfig::default();
        let clean = snapshot_with(vec![term("cmd+t", "new_tab")]);
        assert!(render_status_line(&cfg, &clean).is_none());

        let dirty = snapshot_with(vec![term("ctrl+tab", "next_tab")]);
        let line = render_status_line(&cfg, &dirty).unwrap();
        assert!(line.contains("Ctrl+Tab"));
        assert!(line.contains("/keys"));
    }

    #[test]
    fn dead_option_key_is_reported_even_with_zero_conflicts() {
        // The gap this closes: nothing intercepts the chord, so the old report
        // said "no conflicts" while every alt+ binding was silently dead.
        use crate::keymap::AltDelivery;
        let cfg = KeybindingsConfig::default();
        assert!(
            jcode_has_alt_binding(&cfg),
            "defaults must contain an alt+ chord for this test to mean anything"
        );

        let mut snap = snapshot_with(vec![term("cmd+t", "new_tab")]);
        snap.terminal = "Alacritty".to_string();
        snap.alt_delivery = AltDelivery::Never;

        let line = render_status_line(&cfg, &snap).expect("should warn about the dead Option key");
        assert!(line.contains("Alacritty"), "got {line}");
        assert!(line.contains("alt+"), "got {line}");

        let report = render_report(&cfg, &snap);
        assert!(report.contains("never sends Option as Alt"), "got {report}");
        // It must still say the conflict search itself came back clean.
        assert!(report.contains("No conflicts found"), "got {report}");
    }

    #[test]
    fn healthy_or_unknown_option_key_stays_silent() {
        use crate::keymap::AltDelivery;
        let cfg = KeybindingsConfig::default();
        for delivery in [AltDelivery::Delivered, AltDelivery::Unknown] {
            let mut snap = snapshot_with(vec![term("cmd+t", "new_tab")]);
            snap.alt_delivery = delivery;
            assert!(
                render_status_line(&cfg, &snap).is_none(),
                "{delivery:?} must not warn"
            );
            assert!(!render_report(&cfg, &snap).contains("Option as Alt"));
        }
    }

    #[test]
    fn dead_option_key_is_silent_when_no_alt_binding_is_configured() {
        use crate::keymap::AltDelivery;
        // Every alt+ default rebound away: the terminal setting now costs the
        // user nothing, so warning would be pure noise.
        let mut cfg = KeybindingsConfig::default();
        cfg.scroll_prompt_up = "none".to_string();
        for b in super::super::conflicts::jcode_bindings(&cfg) {
            assert!(
                !b.chord.alt || b.field != "keybindings.scroll_prompt_up",
                "disabled binding still enumerated"
            );
        }
        // The predicate, exercised directly: with no alt chord in the list the
        // degraded terminal must not produce a warning.
        assert!(!alt_warning_applies(AltDelivery::Never, false));
        assert!(alt_warning_applies(AltDelivery::Never, true));
        assert!(!alt_warning_applies(AltDelivery::Delivered, true));
        assert!(!alt_warning_applies(AltDelivery::Unknown, true));
    }

    #[test]
    fn an_edit_onto_an_occupied_chord_is_reported_as_new() {
        // The whole point of gap 6: the user binds an action to a chord the
        // terminal already owns, and finds out now rather than at next launch.
        let snap = snapshot_with(vec![term("ctrl+tab", "next_tab")]);
        let before = KeybindingsConfig {
            model_switch_next: "ctrl+shift+m".to_string(),
            ..KeybindingsConfig::default()
        };
        let after = KeybindingsConfig {
            model_switch_next: "ctrl+tab".to_string(),
            ..KeybindingsConfig::default()
        };
        let notice = render_new_conflict_notice(&before, &after, &snap)
            .expect("binding onto an occupied chord must be reported");
        assert!(notice.contains("keybindings.model_switch_next"), "{notice}");
        assert!(notice.contains("Ctrl+Tab"), "{notice}");
        assert!(notice.contains("next_tab"), "{notice}");
    }

    #[test]
    fn a_preexisting_conflict_is_not_re_reported() {
        // Startup already told the user about this one. Repeating it on an
        // unrelated keybinding edit is exactly the noise that trains people to
        // ignore the warning.
        let snap = snapshot_with(vec![term("ctrl+tab", "next_tab")]);
        let before = KeybindingsConfig::default();
        let after = KeybindingsConfig {
            scroll_up: "ctrl+y".to_string(),
            ..KeybindingsConfig::default()
        };
        assert!(!detect_conflicts(&before, &snap).is_empty());
        assert!(
            render_new_conflict_notice(&before, &after, &snap).is_none(),
            "only conflicts introduced by this edit should be reported"
        );
    }

    #[test]
    fn resolving_a_conflict_reports_nothing() {
        let snap = snapshot_with(vec![term("ctrl+tab", "next_tab")]);
        let before = KeybindingsConfig::default();
        let after = KeybindingsConfig {
            model_switch_next: "ctrl+shift+m".to_string(),
            ..KeybindingsConfig::default()
        };
        assert!(render_new_conflict_notice(&before, &after, &snap).is_none());
    }

    #[test]
    fn moving_a_conflict_onto_a_different_occupied_chord_is_new() {
        // The conflicting field is the same, so a field-only identity would
        // treat this as "already known" and stay silent, even though the user
        // just jumped out of one frying pan into another.
        let snap = snapshot_with(vec![term("ctrl+tab", "next_tab"), term("cmd+t", "new_tab")]);
        let before = KeybindingsConfig::default();
        let after = KeybindingsConfig {
            model_switch_next: "cmd+t".to_string(),
            ..KeybindingsConfig::default()
        };
        let notice = render_new_conflict_notice(&before, &after, &snap)
            .expect("a move onto a different occupied chord is a new conflict");
        assert!(notice.contains("new_tab"), "{notice}");
    }

    #[test]
    fn config_edit_entry_point_ignores_non_keybinding_edits() {
        // Must not even consult the machine snapshot for an unrelated setting.
        assert!(
            crate::keymap::new_conflict_notice_for_config_edit(
                "[ui]\ntheme = \"dark\"\n",
                "[ui]\ntheme = \"light\"\n",
            )
            .is_none()
        );
    }

    #[test]
    fn config_text_keybindings_fall_back_to_defaults() {
        let parsed = crate::keymap::keybindings_from_config_text("[ui]\ntheme = \"dark\"\n");
        assert_eq!(parsed, KeybindingsConfig::default());

        // A partial table keeps defaults for everything it does not mention,
        // so an unmentioned binding is still checked for conflicts.
        let partial =
            crate::keymap::keybindings_from_config_text("[keybindings]\nscroll_up = \"ctrl+y\"\n");
        assert_eq!(partial.scroll_up, "ctrl+y");
        assert_eq!(
            partial.model_switch_next,
            KeybindingsConfig::default().model_switch_next
        );

        // Broken TOML must not panic; defaults match Config::load's behaviour.
        assert_eq!(
            crate::keymap::keybindings_from_config_text("[keybindings"),
            KeybindingsConfig::default()
        );
    }
}
