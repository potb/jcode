//! `state.json` envelope. See `docs/AMBIENT_PER_PROJECT.md`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::paths::state_path;
use super::{AmbientCycleResult, AmbientState};
use crate::storage;

/// The on-disk contents of `state.json`: global state plus a per-project map.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AmbientStateFile {
    /// State for cycles belonging to no single project, and the daemon's own
    /// process-wide scheduling status.
    #[serde(default)]
    pub global: AmbientState,
    /// State keyed by canonical project path, as resolved by stage 1.
    #[serde(default)]
    pub projects: BTreeMap<String, AmbientState>,
}

impl AmbientStateFile {
    pub fn load() -> Result<Self> {
        let path = state_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::load_from(&path)
    }

    /// Read an envelope from `path`, upgrading a pre-envelope file in memory.
    /// Reading never rewrites the file.
    pub fn load_from(path: &Path) -> Result<Self> {
        let value: serde_json::Value = storage::read_json(path)?;
        Self::from_value(value)
    }

    fn from_value(value: serde_json::Value) -> Result<Self> {
        if Self::looks_like_envelope(&value) {
            Ok(serde_json::from_value(value)?)
        } else {
            Self::adopting_legacy_flat_state(value)
        }
    }

    fn looks_like_envelope(value: &serde_json::Value) -> bool {
        value
            .as_object()
            .is_some_and(|map| map.contains_key("global") || map.contains_key("projects"))
    }

    fn adopting_legacy_flat_state(value: serde_json::Value) -> Result<Self> {
        Ok(Self {
            global: serde_json::from_value(value)?,
            projects: BTreeMap::new(),
        })
    }

    pub fn save(&self) -> Result<()> {
        storage::write_json(&state_path()?, self)
    }

    /// State recorded for `project`, defaulted when it has no history yet.
    pub fn project(&self, project: &str) -> AmbientState {
        self.projects.get(project).cloned().unwrap_or_default()
    }

    /// Mutable state for `project`, created on first use.
    pub fn project_mut(&mut self, project: &str) -> &mut AmbientState {
        self.projects.entry(project.to_string()).or_default()
    }

    /// Record a finished cycle against its project, if any, and always against
    /// the global slot that `ambient status` and the scheduler read.
    pub fn record_cycle(&mut self, project: Option<&str>, result: &AmbientCycleResult) {
        if let Some(project) = project {
            self.project_mut(project).record_cycle(result);
        }
        self.global.record_cycle(result);
    }
}
