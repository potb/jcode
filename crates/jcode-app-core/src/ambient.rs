use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod cycle_significance;
#[cfg(test)]
mod cycle_significance_tests;
mod directives;
pub mod gates;
#[cfg(test)]
mod gates_tests;
pub mod headroom;
mod manager;
mod paths;
mod persistence;
pub(crate) mod project_schedule;
#[cfg(test)]
mod project_schedule_tests;
pub(crate) mod prompt;
pub mod runner;
pub mod schedule_window;
#[cfg(test)]
mod schedule_window_tests;
pub mod scheduler;
mod state_file;
#[cfg(test)]
mod state_file_tests;
#[cfg(test)]
mod test_env;

pub use directives::{
    UserDirective, add_directive, has_pending_directives, load_directives, take_pending_directives,
};
pub use manager::AmbientManager;
pub use persistence::{
    AmbientLock, ScheduledQueue, is_locked_by_another_process, is_locked_by_another_process_for,
};
pub use project_schedule::{ProjectKey, ProjectWakeLedger};
#[cfg(test)]
pub(crate) use prompt::format_duration_rough;
pub use prompt::{
    MemoryGraphHealth, ProjectGraphHealth, RecentSessionInfo, ResourceBudget,
    build_ambient_system_prompt, format_minutes_human, format_scheduled_session_message,
    gather_feedback_memories, gather_memory_graph_health, gather_project_graph_health,
    gather_recent_sessions,
};
pub use state_file::AmbientStateFile;

use crate::storage;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Context passed from the ambient runner to a visible TUI cycle.
/// Saved to `~/.jcode/ambient/visible_cycle.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibleCycleContext {
    pub system_prompt: String,
    pub initial_message: String,
}

impl VisibleCycleContext {
    pub fn context_path() -> Result<PathBuf> {
        Ok(storage::jcode_dir()?
            .join("ambient")
            .join("visible_cycle.json"))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::context_path()?;
        if let Some(parent) = path.parent() {
            storage::ensure_dir(parent)?;
        }
        storage::write_json(&path, self)
    }

    pub fn load() -> Result<Self> {
        let path = Self::context_path()?;
        storage::read_json(&path)
    }

    pub fn result_path() -> Result<PathBuf> {
        Ok(storage::jcode_dir()?
            .join("ambient")
            .join("cycle_result.json"))
    }
}

/// Ambient mode status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum AmbientStatus {
    #[default]
    Idle,
    Running {
        detail: String,
    },
    Scheduled {
        next_wake: DateTime<Utc>,
    },
    Paused {
        reason: String,
    },
    Disabled,
}

/// Priority for scheduled items
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Low,
    Normal,
    High,
}

/// Where a scheduled task should be delivered when it becomes due.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleTarget {
    /// Wake the ambient agent and hand it the queued task.
    #[default]
    Ambient,
    /// Deliver the reminder back into a specific interactive session.
    Session { session_id: String },
    /// Spawn a single new session derived from the originating session.
    Spawn { parent_session_id: String },
}

impl ScheduleTarget {
    pub fn is_direct_delivery(&self) -> bool {
        matches!(self, Self::Session { .. } | Self::Spawn { .. })
    }
}

/// A scheduled ambient task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledItem {
    pub id: String,
    pub scheduled_for: DateTime<Utc>,
    pub context: String,
    pub priority: Priority,
    #[serde(default)]
    pub target: ScheduleTarget,
    pub created_by_session: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// Canonical path of the configured project this item belongs to.
    ///
    /// See `docs/AMBIENT_PER_PROJECT.md`; read it via [`Self::project_key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relevant_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

impl ScheduledItem {
    /// The project owning this item, resolving `working_dir` when the stored
    /// `project` is absent, as it is for items queued by an older build.
    pub fn project_key(&self) -> Option<String> {
        self.project
            .clone()
            .or_else(|| prompt::resolve_project_key(self.working_dir.as_deref()))
    }
}

/// Persistent ambient state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AmbientState {
    pub status: AmbientStatus,
    pub last_run: Option<DateTime<Utc>>,
    pub last_summary: Option<String>,
    pub last_compactions: Option<u32>,
    pub last_memories_modified: Option<u32>,
    pub total_cycles: u64,
}

/// Result from an ambient cycle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmbientCycleResult {
    pub summary: String,
    pub memories_modified: u32,
    pub compactions: u32,
    pub proactive_work: Option<String>,
    /// The agent's own claim about whether the user needs to see this cycle
    /// ("routine" / "notable"). Absent when it did not say; see
    /// `cycle_significance` for how that is resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub significance: Option<String>,
    pub next_schedule: Option<ScheduleRequest>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub status: CycleStatus,
    /// Session ID of the agent that ran this cycle.
    ///
    /// `AmbientTranscript::session_id` is a synthetic `ambient_<timestamp>` cycle
    /// label, not a real session, so without this the transcript cannot be
    /// linked back to the session the picker lists (issue #26).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// Full conversation transcript (markdown) for email notifications
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CycleStatus {
    Complete,
    Interrupted,
    Incomplete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRequest {
    pub wake_in_minutes: Option<u32>,
    pub wake_at: Option<DateTime<Utc>>,
    pub context: String,
    pub priority: Priority,
    #[serde(default)]
    pub target: ScheduleTarget,
    #[serde(default)]
    pub created_by_session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relevant_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "ambient_tests.rs"]
mod ambient_tests;
