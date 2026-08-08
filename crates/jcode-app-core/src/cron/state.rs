//! On-disk state for cron jobs: `~/.jcode/cron/state.json`.
//!
//! State is keyed by job `id` and persisted after every run. The daemon
//! restarts often (crash, `selfdev reload` re-execing the process image), so
//! anything kept only in memory would either double-fire a job that was
//! mid-run when the process died, or silently forget it ran at all and fire
//! it again immediately. Disk state is the only thing both restart paths
//! agree on.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::storage;

/// Recorded outcome of a job's most recent run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LastStatus {
    Success,
    Failure,
    /// Killed after exceeding `timeout_secs` (exec mode only).
    Timeout,
}

/// Persisted state for one job, keyed by `CronJobConfig::id` in
/// [`CronState`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronJobState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<LastStatus>,
    /// Exec mode only; `None` for prompt-mode jobs and for exec jobs that
    /// never completed a process (timeout, spawn failure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_duration_ms: Option<u64>,
    /// Resets to 0 on any success. Exists so a job silently failing every
    /// cycle is visible in `cron:list` without grepping logs.
    #[serde(default)]
    pub consecutive_failures: u32,
}

/// All persisted cron state, keyed by job id.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronState(BTreeMap<String, CronJobState>);

fn cron_dir() -> Result<PathBuf> {
    let dir = storage::jcode_dir()?.join("cron");
    storage::ensure_dir(&dir)?;
    Ok(dir)
}

fn state_path() -> Result<PathBuf> {
    Ok(cron_dir()?.join("state.json"))
}

/// Log directory for exec-mode job output (`~/.jcode/cron/logs/<id>.log`).
pub(super) fn logs_dir() -> Result<PathBuf> {
    let dir = cron_dir()?.join("logs");
    storage::ensure_dir(&dir)?;
    Ok(dir)
}

impl CronState {
    pub fn load() -> Self {
        match state_path() {
            Ok(path) if path.exists() => storage::read_json(&path).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        storage::write_json(&state_path()?, self)
    }

    pub fn get(&self, id: &str) -> Option<&CronJobState> {
        self.0.get(id)
    }

    pub fn last_run(&self, id: &str) -> Option<DateTime<Utc>> {
        self.0.get(id).and_then(|s| s.last_run)
    }

    /// Record a completed run and persist immediately. Persisting inline
    /// rather than batching matters here: a crash between two jobs in the
    /// same due-check pass must lose at most the in-flight job's record, not
    /// every job that ran earlier in the same pass.
    pub fn record_run(
        &mut self,
        id: &str,
        ended_at: DateTime<Utc>,
        status: LastStatus,
        exit_code: Option<i32>,
        duration_ms: u64,
    ) -> Result<()> {
        let entry = self.0.entry(id.to_string()).or_default();
        entry.last_run = Some(ended_at);
        entry.last_status = Some(status);
        entry.last_exit_code = exit_code;
        entry.last_duration_ms = Some(duration_ms);
        match status {
            LastStatus::Success => entry.consecutive_failures = 0,
            LastStatus::Failure | LastStatus::Timeout => entry.consecutive_failures += 1,
        }
        self.save()
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;
