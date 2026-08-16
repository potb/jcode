//! Per-project wake ledger. See `docs/AMBIENT_PER_PROJECT.md`.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::collections::BTreeMap;
use std::time::Duration;

/// A project key, or `None` for cycles belonging to no project.
pub type ProjectKey = Option<String>;

/// When each project is next allowed to run a cycle: turn-taking only, with
/// quota and backoff left account-wide. See `docs/AMBIENT_PER_PROJECT.md`.
#[derive(Debug, Default, Clone)]
pub struct ProjectWakeLedger {
    next_wake: BTreeMap<ProjectKey, DateTime<Utc>>,
}

impl ProjectWakeLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a project for scheduling, due immediately when it is new.
    pub fn register(&mut self, project: ProjectKey, now: DateTime<Utc>) {
        self.next_wake.entry(project).or_insert(now);
    }

    /// Record that `project` just ran, and is not due again for `interval`.
    pub fn record_cycle(&mut self, project: ProjectKey, now: DateTime<Utc>, interval: Duration) {
        let delay = ChronoDuration::from_std(interval).unwrap_or_else(|_| ChronoDuration::zero());
        self.next_wake.insert(project, now + delay);
    }

    pub fn next_wake_for(&self, project: &ProjectKey) -> Option<DateTime<Utc>> {
        self.next_wake.get(project).copied()
    }

    /// The due project that has waited longest, if any.
    pub fn due_project(&self, now: DateTime<Utc>) -> Option<ProjectKey> {
        self.next_wake
            .iter()
            .filter(|(_, due)| **due <= now)
            .min_by_key(|(project, due)| (**due, (*project).clone()))
            .map(|(project, _)| project.clone())
    }

    /// The earliest wake time across all projects, for sizing a sleep.
    pub fn earliest_wake(&self) -> Option<DateTime<Utc>> {
        self.next_wake.values().min().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.next_wake.is_empty()
    }
}
