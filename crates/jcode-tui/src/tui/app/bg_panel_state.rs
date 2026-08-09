//! Keyboard-driven state for the inline background-task panel.
//!
//! Mirrors the swarm panel's model (`cycle_swarm_panel_view` and friends) so
//! the two inline panels behave identically: Alt-chord to cycle
//! chat → controls → full page, Alt+arrows to move the selection, Esc to
//! leave, and plain typing always flowing through to the chat input because
//! the panel is an overlay rather than a modal.

use super::App;

/// Whether the pre-existing "move the running tool to the background" action
/// owns this key press, so the panel must not claim it.
///
/// Alt+B is shared between two features that both legitimately want that
/// mnemonic. They are cleanly separable by context:
///
/// - While a tool is running, Alt+B sends it to the background. That action is
///   what *creates* background tasks, so the panel shadowing it would break
///   the feature that fills the panel (and a `/tips` entry advertises it).
/// - The rest of the time that action is a no-op, so the panel takes the key.
///
/// Only the shared chord is guarded: if the user rebinds
/// `keybindings.background_panel_focus` to something else, the panel claims it
/// unconditionally and the two features stop competing entirely.
pub(crate) fn background_tool_action_owns_key(
    app: &App,
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};

    let is_alt_b = (matches!(code, KeyCode::Char('b')) && modifiers.contains(KeyModifiers::ALT))
        || crate::tui::keybind::shortcut_char_for_macos_option_key(code, modifiers) == Some('b');
    if !is_alt_b {
        return false;
    }
    matches!(app.status, crate::tui::ProcessingStatus::RunningTool(_))
}

/// The three Alt+B background-panel views. Repeated presses cycle in
/// declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BgPanelView {
    Chat,
    Controls,
    FullPage,
}

/// Outcome of an Alt+B press.
///
/// `Chat` alone could not distinguish "you closed the panel" from "there was
/// nothing to open", so the no-tasks case reported "Background view closed"
/// when nothing had ever been open. Callers need the difference to say
/// something true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BgPanelCycle {
    Opened(BgPanelView),
    Closed,
    NothingToShow,
}

/// What a key press should do while the background panel is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BgPanelAction {
    SelectNext,
    SelectPrev,
    ToggleAllSessions,
    Exit,
}

/// Map a key to a focused-background-panel action.
///
/// Deliberately narrow, for the same reason as the swarm panel: the user may
/// keep typing into the chat input while watching a build, so only Esc and
/// Alt-chords are claimed.
/// - Alt+↑ / Alt+↓ (also Alt+k / Alt+j): move the selection
/// - Alt+a: toggle between this session's tasks and every session's. This is
///   shared with "copy the chat viewport" (local chain, empty input); the
///   panel only sees the key while focused, which is a mode the user entered
///   deliberately, so the copy binding keeps working everywhere else.
/// - Esc: exit the panel
pub(crate) fn bg_panel_action_for_key(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<BgPanelAction> {
    use crossterm::event::{KeyCode, KeyModifiers};
    if code == KeyCode::Esc && modifiers.is_empty() {
        return Some(BgPanelAction::Exit);
    }
    let alt = modifiers.contains(KeyModifiers::ALT);
    // macOS Option+letter often arrives as a transformed glyph with no ALT
    // modifier; normalize through the shared shortcut helper.
    let macos_letter = crate::tui::keybind::shortcut_char_for_macos_option_key(code, modifiers);
    // Alt+J/K mirror the swarm panel's vim aliases. Note that both panels are
    // shadowed by workspace navigation, which also defaults to alt+j/alt+k and
    // is dispatched earlier: with a workspace enabled those chords move
    // workspace focus and never reach either panel. Alt+Up/Down are the
    // reliable selection keys, which is why they are listed first in the
    // status hint. Not "fixed" here on purpose: diverging from the swarm panel
    // would make two adjacent panels behave differently for the same keys.
    match code {
        KeyCode::Down | KeyCode::Char('j') if alt => Some(BgPanelAction::SelectNext),
        KeyCode::Up | KeyCode::Char('k') if alt => Some(BgPanelAction::SelectPrev),
        KeyCode::Char('a') if alt => Some(BgPanelAction::ToggleAllSessions),
        _ => match macos_letter {
            Some('j') => Some(BgPanelAction::SelectNext),
            Some('k') => Some(BgPanelAction::SelectPrev),
            Some('a') => Some(BgPanelAction::ToggleAllSessions),
            _ => None,
        },
    }
}

/// Where the selection lands after moving by `delta` over `count` tasks.
///
/// Split out of `move_bg_panel_selection` because that method reads the
/// process-global snapshot cache for its count, which made the clamping rule
/// untestable without ambient state. The rule is what matters: an index past
/// the end renders a blank pane or indexes out of bounds, and an off-by-one
/// here escaped the entire suite when mutated.
fn moved_selection(current: usize, count: usize, delta: isize) -> usize {
    if count == 0 {
        return current;
    }
    let last = count - 1;
    let cur = current.min(last) as isize;
    (cur + delta).clamp(0, last as isize) as usize
}

impl App {
    /// Session id used to scope the panel to "this session".
    pub(crate) fn bg_panel_session_id(&self) -> Option<String> {
        if self.is_remote {
            self.remote_session_id.clone()
        } else {
            Some(self.session.id.clone())
        }
    }

    /// Tasks the panel should show, after the session filter.
    ///
    /// The snapshot itself is machine-wide (tasks are spawned by the server
    /// process, so scoping the *read* to a session would show nothing in a
    /// remote client). Filtering afterwards keeps the "all sessions" toggle a
    /// pure display concern, and means a wrong session id degrades to an empty
    /// list the user can escape with Alt+A rather than a panel that never
    /// appears.
    pub(crate) fn bg_panel_tasks(&self) -> Vec<jcode_tui_render::background_gallery::BgTask> {
        let session = self.bg_panel_session_id();
        let all = super::bg_panel::tasks_snapshot(session.as_deref());
        if self.bg_panel_show_all_sessions {
            return all;
        }
        all.into_iter()
            .filter(|task| task.is_current_session)
            .collect()
    }

    /// How many tasks the panel would show, without materializing them.
    ///
    /// Visibility and selection clamping run several times per frame, so they
    /// use this instead of cloning the task vector to read its length.
    pub(crate) fn bg_panel_task_count(&self) -> usize {
        let session = self.bg_panel_session_id();
        super::bg_panel::tasks_count(session.as_deref(), !self.bg_panel_show_all_sessions)
    }

    /// Whether the panel has anything to show. Drives strip visibility.
    pub(crate) fn bg_panel_active(&self) -> bool {
        self.bg_panel_task_count() > 0
    }

    pub(crate) fn bg_panel_selected(&self) -> usize {
        let count = self.bg_panel_task_count();
        if count == 0 {
            0
        } else {
            self.bg_panel_selected.min(count - 1)
        }
    }

    pub(crate) fn bg_panel_focused(&self) -> bool {
        self.bg_panel_focused
    }

    pub(crate) fn bg_panel_full_page(&self) -> bool {
        self.bg_panel_full_page && self.bg_panel_active()
    }

    pub(crate) fn bg_panel_show_all_sessions(&self) -> bool {
        self.bg_panel_show_all_sessions
    }

    /// Cycle chat → inline controls → full background page → chat.
    pub(crate) fn cycle_bg_panel_view(&mut self) -> BgPanelCycle {
        // Refresh before deciding: a user pressing the key right after
        // starting a task should not be told there is nothing to show because
        // the cached snapshot predates it.
        super::bg_panel::invalidate_cache();
        if !self.bg_panel_active() {
            self.bg_panel_focused = false;
            self.bg_panel_full_page = false;
            return BgPanelCycle::NothingToShow;
        }

        let next = match (self.bg_panel_focused, self.bg_panel_full_page) {
            (false, _) => BgPanelView::Controls,
            (true, false) => BgPanelView::FullPage,
            (true, true) => BgPanelView::Chat,
        };
        match next {
            BgPanelView::Chat => {
                self.bg_panel_focused = false;
                self.bg_panel_full_page = false;
            }
            BgPanelView::Controls => {
                self.bg_panel_focused = true;
                self.bg_panel_full_page = false;
            }
            BgPanelView::FullPage => {
                self.bg_panel_focused = true;
                self.bg_panel_full_page = true;
            }
        }
        if next != BgPanelView::Chat {
            let count = self.bg_panel_task_count();
            self.bg_panel_selected = self.bg_panel_selected.min(count.saturating_sub(1));
            return BgPanelCycle::Opened(next);
        }
        BgPanelCycle::Closed
    }

    /// Move the selection by `delta`, saturating at the ends.
    pub(crate) fn move_bg_panel_selection(&mut self, delta: isize) {
        let count = self.bg_panel_task_count();
        self.bg_panel_selected = moved_selection(self.bg_panel_selected, count, delta);
    }

    /// Handle a key while the background panel is focused. Returns true when
    /// the key was consumed.
    pub(crate) fn handle_bg_panel_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) -> bool {
        if !self.bg_panel_focused || !self.bg_panel_active() {
            return false;
        }
        match bg_panel_action_for_key(code, modifiers) {
            Some(BgPanelAction::SelectNext) => {
                self.move_bg_panel_selection(1);
                true
            }
            Some(BgPanelAction::SelectPrev) => {
                self.move_bg_panel_selection(-1);
                true
            }
            Some(BgPanelAction::ToggleAllSessions) => {
                self.bg_panel_show_all_sessions = !self.bg_panel_show_all_sessions;
                // The visible list just changed size; keep the selection in it.
                let count = self.bg_panel_task_count();
                self.bg_panel_selected = self.bg_panel_selected.min(count.saturating_sub(1));
                self.set_status_notice(if self.bg_panel_show_all_sessions {
                    "Background: all sessions"
                } else {
                    "Background: this session"
                });
                true
            }
            Some(BgPanelAction::Exit) => {
                self.bg_panel_focused = false;
                self.bg_panel_full_page = false;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests;
