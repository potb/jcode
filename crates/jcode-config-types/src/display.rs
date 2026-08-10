//! `[display]` section of the config: TUI/CLI presentation settings.

use crate::{
    ContextWidgetMode, DiagramDisplayMode, DiffDisplayMode, LatexRenderingMode,
    MarkdownSpacingMode, NativeScrollbarConfig, OverscrollStatusMode, ReasoningDisplayMode,
    SessionFactsMode, TodoWidgetMode, default_true,
};
use serde::{Deserialize, Serialize};

/// Display/UI configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// How to display file diffs (off/inline/full-inline/pinned/file, default: inline)
    #[serde(deserialize_with = "crate::serde_lenient::lenient_enum")]
    pub diff_mode: DiffDisplayMode,
    /// Legacy: "show_diffs = true/false" maps to diff_mode inline/off
    #[serde(default)]
    pub(crate) show_diffs: Option<bool>,
    /// Queue mode by default - wait until done before sending (default: false)
    pub queue_mode: bool,
    /// Automatically reload the remote server when a newer server binary is detected (default: true)
    pub auto_server_reload: bool,
    /// Capture mouse events (default: true). Enables scroll wheel but disables terminal selection.
    pub mouse_capture: bool,
    /// Enable debug socket for external control (default: false)
    pub debug_socket: bool,
    /// Render emoji in terminal-facing TUI and CLI output (default: true)
    pub emoji: bool,
    /// Center all content (default: false)
    pub centered: bool,
    /// Show thinking/reasoning content by default (default: false)
    pub show_thinking: bool,
    /// How to display reasoning/thinking content (off/full/current).
    /// When unset, falls back to `show_thinking` (true => full, false => off).
    #[serde(
        default,
        deserialize_with = "crate::serde_lenient::lenient_optional_enum"
    )]
    pub(crate) reasoning_display: Option<ReasoningDisplayMode>,
    /// How to display mermaid diagrams (none/margin/pinned, default: none).
    /// `none` still renders diagrams inline in the transcript via the inline
    /// image pipeline; `margin`/`pinned` add dedicated widget placements.
    #[serde(deserialize_with = "crate::serde_lenient::lenient_enum")]
    pub diagram_mode: DiagramDisplayMode,
    /// Markdown block spacing style (compact/document, default: compact)
    #[serde(deserialize_with = "crate::serde_lenient::lenient_enum")]
    pub markdown_spacing: MarkdownSpacingMode,
    /// LaTeX rendering style (none/unicode/image, default: image)
    #[serde(deserialize_with = "crate::serde_lenient::lenient_enum")]
    pub latex_rendering: LatexRenderingMode,
    /// Pin read images to side pane (default: true)
    pub pin_images: bool,
    /// Pin the full session todo list to the top of the chat transcript while
    /// it scrolls, like the sticky previous-prompt preview (default: false)
    #[serde(default)]
    pub pin_todos: bool,
    /// Whether the info widget shows the session todo list on the side of the
    /// chat (auto/on/off, default: auto). `auto` hides the side widget while
    /// `pin_todos` is on so the same list isn't rendered twice.
    #[serde(default, deserialize_with = "crate::serde_lenient::lenient_enum")]
    pub todo_widget: TodoWidgetMode,
    /// Whether the info widget shows the memory activity section (default:
    /// true). Set to false to hide saved/injected memory chatter from the HUD.
    #[serde(default = "default_true")]
    pub memory_widget: bool,
    /// Keep the current provider's usage limits pinned to the last line of the
    /// terminal, below the input (default: false). The line adapts to the
    /// terminal width: full labelled bars when wide, a compact
    /// `5h 62% · wk 81%` summary when narrow.
    #[serde(default)]
    pub pin_usage: bool,
    /// Show idle animation before first prompt (default: false)
    pub idle_animation: bool,
    /// Briefly animate user prompt line when it enters viewport (default: true)
    pub prompt_entry_animation: bool,
    /// Disable specific animation variants by name (e.g. ["donut", "orbit_rings"])
    pub disabled_animations: Vec<String>,
    /// Wrap long lines in the pinned diff pane (default: true)
    pub diff_line_wrap: bool,
    /// Performance tier override: auto/full/reduced/minimal (default: auto)
    pub performance: String,
    /// FPS for animations (startup, idle donut): 1-120 (default: 60)
    pub animation_fps: u32,
    /// FPS for active redraw (processing, streaming): 1-120 (default: 30)
    pub redraw_fps: u32,
    /// Show a truncated preview of the previous prompt at the top when it scrolls out of view (default: true)
    pub prompt_preview: bool,
    /// Render swarm/file-activity notifications in a compact single-line form
    /// instead of the full multi-line card with diff preview (default: false)
    pub compact_notifications: bool,
    /// Override the Alt/Option label shown in copy badges. Empty = auto (⌥ on macOS, Alt elsewhere).
    pub copy_badge_alt_label: String,
    /// Show the full agentgrep tool output inline in the transcript instead of
    /// just the one-line summary (default: false)
    #[serde(default)]
    pub show_agentgrep_output: bool,
    /// Show the dimmed technical detail (command, path, args) after the
    /// model-provided intent on tool rows (default: false). When off, rows
    /// that have an intent show only the intent; rows without an intent
    /// always fall back to the technical detail.
    #[serde(default)]
    pub tool_call_details: bool,
    /// Native terminal scrollbar configuration for scrollable panes
    pub native_scrollbars: NativeScrollbarConfig,
    /// Surface occasional "learn this keybinding" nudges when the user keeps
    /// performing an action the slow way (slash command) instead of using its
    /// configured shortcut (default: true). Set false to disable all such hints.
    #[serde(default = "default_true")]
    pub keybinding_hints: bool,
    /// Color theme: "auto" (detect terminal background), "dark", or "light".
    /// Auto queries the terminal's background color (OSC 11) at startup and
    /// adapts jcode's palette for light backgrounds. Default: auto.
    #[serde(default)]
    pub theme: String,
    /// Per-role color overrides, e.g. `user = "#8ab4f8"`. Any TUI color can be
    /// configured: the named roles are substituted directly, and ad hoc shades
    /// used by widgets follow the role they belong to. Run `/colors` to list
    /// roles and `/colors harmony` to score the result.
    #[serde(default)]
    pub colors: std::collections::BTreeMap<String, String>,
    /// Opt-in active sessions manager: pressing Left arrow on an empty input
    /// opens a picker scoped to live (open) sessions, showing which are still
    /// working and which are ready for input (default: false). The `/active`
    /// command works regardless of this setting.
    #[serde(default)]
    pub active_sessions_manager: bool,
    /// Include transcripts discovered from other agent CLIs (Claude Code,
    /// Codex, Pi, OpenCode, Cursor) in the session picker so they can be
    /// resumed or imported (default: true). Set false to show only jcode's own
    /// sessions (issue #674).
    #[serde(default = "default_true")]
    pub external_sessions: bool,
    /// When to show the overscroll status line below the input
    /// (off/on/overscroll, default: overscroll). "overscroll" is the elastic
    /// reveal when scrolling past the bottom, "on" keeps it always visible.
    #[serde(default, deserialize_with = "crate::serde_lenient::lenient_enum")]
    pub overscroll_status: OverscrollStatusMode,
    /// Which edge of the composer the session-fact stack (provider/auth, model
    /// and effort, working directory, context gauge) hugs: right (default),
    /// left, or off to hide it. The stack only ever paints into cells that are
    /// already blank, so moving it does not reflow the transcript or input.
    #[serde(default, deserialize_with = "crate::serde_lenient::lenient_enum")]
    pub session_facts: SessionFactsMode,
    /// Whether the info widget shows a context-usage card beside the chat
    /// (auto/on/off, default: auto). `auto` hides it while the session-fact
    /// stack is enabled, since that stack already reports context usage with
    /// its own gauge and the card would be the same number a second time.
    #[serde(default, deserialize_with = "crate::serde_lenient::lenient_enum")]
    pub context_widget: ContextWidgetMode,
}
impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            diff_mode: DiffDisplayMode::default(),
            show_diffs: None,
            pin_images: true,
            pin_todos: false,
            todo_widget: TodoWidgetMode::default(),
            memory_widget: true,
            pin_usage: false,
            queue_mode: false,
            auto_server_reload: true,
            mouse_capture: true,
            debug_socket: false,
            emoji: true,
            centered: false,
            show_thinking: false,
            reasoning_display: Some(ReasoningDisplayMode::Off),
            diagram_mode: DiagramDisplayMode::default(),
            markdown_spacing: MarkdownSpacingMode::default(),
            latex_rendering: LatexRenderingMode::default(),
            idle_animation: false,
            prompt_entry_animation: true,
            disabled_animations: Vec::new(),
            diff_line_wrap: true,
            performance: String::new(),
            animation_fps: 60,
            redraw_fps: 60,
            prompt_preview: true,
            compact_notifications: false,
            copy_badge_alt_label: String::new(),
            show_agentgrep_output: false,
            tool_call_details: false,
            native_scrollbars: NativeScrollbarConfig::default(),
            keybinding_hints: true,
            theme: String::new(),
            colors: std::collections::BTreeMap::new(),
            active_sessions_manager: false,
            external_sessions: true,
            overscroll_status: OverscrollStatusMode::default(),
            session_facts: SessionFactsMode::default(),
            context_widget: ContextWidgetMode::default(),
        }
    }
}
impl DisplayConfig {
    pub fn apply_legacy_compat(&mut self) {
        if let Some(show) = self.show_diffs.take() {
            self.diff_mode = if show {
                DiffDisplayMode::Inline
            } else {
                DiffDisplayMode::Off
            };
        }
    }

    /// Resolve the effective reasoning display mode. Prefers the explicit
    /// `reasoning_display` field, falling back to the legacy `show_thinking`
    /// boolean (true => Full, false => Off) when unset.
    pub fn reasoning_display(&self) -> ReasoningDisplayMode {
        self.reasoning_display.unwrap_or(if self.show_thinking {
            ReasoningDisplayMode::Full
        } else {
            ReasoningDisplayMode::Off
        })
    }

    /// Whether the user explicitly chose a reasoning display mode, as opposed
    /// to inheriting the legacy `show_thinking` fallback. Front-ends use this
    /// to apply their own default without overriding a deliberate choice.
    pub fn has_explicit_reasoning_display(&self) -> bool {
        self.reasoning_display.is_some()
    }

    /// Set the reasoning display mode and keep `show_thinking` in sync so the
    /// provider request path (which still keys off `show_thinking`) requests
    /// reasoning whenever any display mode is active.
    pub fn set_reasoning_display(&mut self, mode: ReasoningDisplayMode) {
        self.reasoning_display = Some(mode);
        self.show_thinking = !matches!(mode, ReasoningDisplayMode::Off);
    }

    /// Whether reasoning content should be generated/requested at all.
    pub fn reasoning_enabled(&self) -> bool {
        !matches!(self.reasoning_display(), ReasoningDisplayMode::Off)
    }
}

#[cfg(test)]
mod todo_widget_mode_tests {
    use super::DisplayConfig;
    use crate::TodoWidgetMode;

    #[test]
    fn defaults_to_auto_and_yields_to_the_pinned_band() {
        let cfg = DisplayConfig::default();
        assert_eq!(cfg.todo_widget, TodoWidgetMode::Auto);
        assert!(cfg.todo_widget.visible(false));
        assert!(!cfg.todo_widget.visible(true));
        assert!(TodoWidgetMode::On.visible(true));
        assert!(!TodoWidgetMode::Off.visible(false));
    }

    #[test]
    fn parses_from_config_toml_including_bool_spellings() {
        for (raw, expected) in [
            ("auto", TodoWidgetMode::Auto),
            ("on", TodoWidgetMode::On),
            ("true", TodoWidgetMode::On),
            ("always", TodoWidgetMode::On),
            ("off", TodoWidgetMode::Off),
            ("never", TodoWidgetMode::Off),
            ("hidden", TodoWidgetMode::Off),
        ] {
            let cfg: DisplayConfig =
                toml::from_str(&format!("todo_widget = \"{}\"", raw)).expect(raw);
            assert_eq!(cfg.todo_widget, expected, "toml value {}", raw);
            assert_eq!(TodoWidgetMode::parse(raw), Some(expected), "parse {}", raw);
        }

        // Garbage falls back to the default instead of failing the whole config.
        let cfg: DisplayConfig = toml::from_str("todo_widget = \"nonsense\"").unwrap();
        assert_eq!(cfg.todo_widget, TodoWidgetMode::Auto);
        assert_eq!(TodoWidgetMode::parse("nonsense"), None);
    }

    #[test]
    fn round_trips_through_serialization() {
        let mut cfg = DisplayConfig::default();
        cfg.todo_widget = TodoWidgetMode::Off;
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("todo_widget = \"off\""), "{}", text);
        let back: DisplayConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.todo_widget, TodoWidgetMode::Off);
    }
}

#[cfg(test)]
mod memory_widget_tests {
    use super::DisplayConfig;

    #[test]
    fn memory_widget_defaults_on_and_parses_off() {
        let cfg: DisplayConfig = toml::from_str("").unwrap();
        assert!(cfg.memory_widget);
        let cfg: DisplayConfig = toml::from_str("memory_widget = false").unwrap();
        assert!(!cfg.memory_widget);
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("memory_widget = false"), "{}", text);
    }
}
#[cfg(test)]
mod session_facts_mode_tests {
    use super::DisplayConfig;
    use crate::SessionFactsMode;

    #[test]
    fn defaults_to_the_right_edge_so_existing_layouts_are_unchanged() {
        let cfg = DisplayConfig::default();
        assert_eq!(cfg.session_facts, SessionFactsMode::Right);
        assert!(cfg.session_facts.enabled());
        assert!(SessionFactsMode::Left.enabled());
        assert!(!SessionFactsMode::Off.enabled());
    }

    #[test]
    fn flips_between_edges_but_leaves_off_alone() {
        assert_eq!(SessionFactsMode::Right.flipped(), SessionFactsMode::Left);
        assert_eq!(SessionFactsMode::Left.flipped(), SessionFactsMode::Right);
        // `/facts` with no argument toggles sides; it must not silently
        // resurrect a stack the user turned off.
        assert_eq!(SessionFactsMode::Off.flipped(), SessionFactsMode::Off);
    }

    #[test]
    fn parses_from_config_toml_including_bool_spellings() {
        for (raw, expected) in [
            ("right", SessionFactsMode::Right),
            ("on", SessionFactsMode::Right),
            ("true", SessionFactsMode::Right),
            ("left", SessionFactsMode::Left),
            ("off", SessionFactsMode::Off),
            ("never", SessionFactsMode::Off),
            ("hidden", SessionFactsMode::Off),
        ] {
            let cfg: DisplayConfig =
                toml::from_str(&format!("session_facts = \"{}\"", raw)).expect(raw);
            assert_eq!(cfg.session_facts, expected, "toml value {}", raw);
            assert_eq!(
                SessionFactsMode::parse(raw),
                Some(expected),
                "parse {}",
                raw
            );
        }

        // Garbage falls back to the default instead of failing the whole config.
        let cfg: DisplayConfig = toml::from_str("session_facts = \"sideways\"").unwrap();
        assert_eq!(cfg.session_facts, SessionFactsMode::Right);
        assert_eq!(SessionFactsMode::parse("sideways"), None);
    }

    #[test]
    fn round_trips_through_serialization() {
        let mut cfg = DisplayConfig::default();
        cfg.session_facts = SessionFactsMode::Left;
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("session_facts = \"left\""), "{}", text);
        let back: DisplayConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.session_facts, SessionFactsMode::Left);
    }
}

#[cfg(test)]
mod context_widget_mode_tests {
    use super::DisplayConfig;
    use crate::ContextWidgetMode;

    #[test]
    fn defaults_to_auto_and_yields_to_the_fact_stack() {
        let cfg = DisplayConfig::default();
        assert_eq!(cfg.context_widget, ContextWidgetMode::Auto);
        // No fact stack drawing context: the card is the only reading, so it shows.
        assert!(cfg.context_widget.visible(false));
        // Stack is showing context: the card would be the same number twice.
        assert!(!cfg.context_widget.visible(true));
        assert!(ContextWidgetMode::On.visible(true));
        assert!(!ContextWidgetMode::Off.visible(false));
    }

    #[test]
    fn parses_from_config_toml_including_bool_spellings() {
        for (raw, expected) in [
            ("auto", ContextWidgetMode::Auto),
            ("on", ContextWidgetMode::On),
            ("true", ContextWidgetMode::On),
            ("off", ContextWidgetMode::Off),
            ("never", ContextWidgetMode::Off),
            ("hidden", ContextWidgetMode::Off),
        ] {
            let cfg: DisplayConfig =
                toml::from_str(&format!("context_widget = \"{}\"", raw)).expect(raw);
            assert_eq!(cfg.context_widget, expected, "toml value {}", raw);
            assert_eq!(ContextWidgetMode::parse(raw), Some(expected), "parse {}", raw);
        }

        let cfg: DisplayConfig = toml::from_str("context_widget = \"nonsense\"").unwrap();
        assert_eq!(cfg.context_widget, ContextWidgetMode::Auto);
        assert_eq!(ContextWidgetMode::parse("nonsense"), None);
    }

    #[test]
    fn round_trips_through_serialization() {
        let mut cfg = DisplayConfig::default();
        cfg.context_widget = ContextWidgetMode::Off;
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("context_widget = \"off\""), "{}", text);
        let back: DisplayConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.context_widget, ContextWidgetMode::Off);
    }
}
