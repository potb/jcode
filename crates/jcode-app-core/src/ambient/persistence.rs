use anyhow::Result;
use chrono::Utc;
use std::path::PathBuf;

use super::paths::project_lock_path;
use super::state_file::AmbientStateFile;
use super::{AmbientCycleResult, AmbientState, AmbientStatus, CycleStatus, ScheduledItem};
use crate::storage;

// ---------------------------------------------------------------------------
// AmbientState persistence
// ---------------------------------------------------------------------------

impl AmbientState {
    /// The global slot of `state.json`, migrating a pre-envelope file on read.
    pub fn load() -> Result<Self> {
        Ok(AmbientStateFile::load()?.global)
    }

    /// Write this state into the global slot, leaving per-project state intact.
    /// Fails rather than overwriting a `state.json` it could not read.
    pub fn save(&self) -> Result<()> {
        let mut file = AmbientStateFile::load()?;
        file.global = self.clone();
        file.save()
    }

    pub fn record_cycle(&mut self, result: &AmbientCycleResult) {
        self.last_run = Some(result.ended_at);
        self.last_summary = Some(result.summary.clone());
        self.last_compactions = Some(result.compactions);
        self.last_memories_modified = Some(result.memories_modified);
        self.total_cycles += 1;

        match result.status {
            CycleStatus::Complete => {
                if let Some(ref req) = result.next_schedule {
                    let next = req.wake_at.unwrap_or_else(|| {
                        Utc::now()
                            + chrono::Duration::minutes(req.wake_in_minutes.unwrap_or(30) as i64)
                    });
                    self.status = AmbientStatus::Scheduled { next_wake: next };
                } else {
                    self.status = AmbientStatus::Idle;
                }
            }
            CycleStatus::Interrupted | CycleStatus::Incomplete => {
                self.status = AmbientStatus::Idle;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ScheduledQueue
// ---------------------------------------------------------------------------

pub struct ScheduledQueue {
    items: Vec<ScheduledItem>,
    path: PathBuf,
}

impl ScheduledQueue {
    pub fn load(path: PathBuf) -> Self {
        let items: Vec<ScheduledItem> = if path.exists() {
            storage::read_json(&path).unwrap_or_default()
        } else {
            Vec::new()
        };
        Self { items, path }
    }

    pub fn save(&self) -> Result<()> {
        storage::write_json(&self.path, &self.items)
    }

    pub fn push(&mut self, item: ScheduledItem) {
        self.items.push(item);
        let _ = self.save();
    }

    /// Remove a scheduled item by ID, persisting the queue when found.
    pub fn remove_by_id(&mut self, id: &str) -> Result<Option<ScheduledItem>> {
        let Some(index) = self.items.iter().position(|item| item.id == id) else {
            return Ok(None);
        };

        let item = self.items.remove(index);
        self.save()?;
        Ok(Some(item))
    }

    /// Pop items whose `scheduled_for` is in the past, sorted by priority
    /// (highest first) then by time (earliest first).
    pub fn pop_ready(&mut self) -> Vec<ScheduledItem> {
        let now = Utc::now();
        let (ready, remaining): (Vec<_>, Vec<_>) =
            self.items.drain(..).partition(|i| i.scheduled_for <= now);

        self.items = remaining;

        let mut ready = ready;
        // Sort: highest priority first, then earliest scheduled_for
        ready.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.scheduled_for.cmp(&b.scheduled_for))
        });

        if !ready.is_empty() {
            let _ = self.save();
        }

        ready
    }

    /// Remove and return ready items targeted at a specific direct-delivery session,
    /// leaving ambient-targeted queue items intact for the ambient agent to process.
    pub fn take_ready_direct_items(&mut self) -> Vec<ScheduledItem> {
        self.take_ready_matching(|item| item.target.is_direct_delivery())
    }

    /// Remove and return ready items that an ambient cycle handles itself.
    ///
    /// The complement of [`Self::take_ready_direct_items`]: these are the items
    /// a cycle receives in its prompt, so they must be removed as they are
    /// handed over, or every later cycle replays them.
    pub fn take_ready_ambient_items(&mut self) -> Vec<ScheduledItem> {
        self.take_ready_matching(|item| !item.target.is_direct_delivery())
    }

    /// Drain the due items matching `predicate`, highest priority first.
    fn take_ready_matching(
        &mut self,
        predicate: impl Fn(&ScheduledItem) -> bool,
    ) -> Vec<ScheduledItem> {
        let now = Utc::now();
        let mut ready = Vec::new();
        let mut remaining = Vec::with_capacity(self.items.len());

        for item in self.items.drain(..) {
            if item.scheduled_for <= now && predicate(&item) {
                ready.push(item);
            } else {
                remaining.push(item);
            }
        }

        self.items = remaining;

        if !ready.is_empty() {
            let _ = self.save();
        }

        ready.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.scheduled_for.cmp(&b.scheduled_for))
        });

        ready
    }

    pub fn peek_next(&self) -> Option<&ScheduledItem> {
        self.items.iter().min_by_key(|i| i.scheduled_for)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[ScheduledItem] {
        &self.items
    }
}

// ---------------------------------------------------------------------------
// AmbientLock  (single-instance guard)
// ---------------------------------------------------------------------------

pub struct AmbientLock {
    pub(crate) lock_path: PathBuf,
}

impl AmbientLock {
    /// Try to acquire the global ambient lock.
    /// Returns `Ok(Some(lock))` if acquired, `Ok(None)` if another instance
    /// already holds it, or `Err` on I/O failure.
    pub fn try_acquire() -> Result<Option<Self>> {
        Self::try_acquire_for(None)
    }

    /// Per-project variant, so one project's cycle does not exclude another's.
    /// See `docs/AMBIENT_PER_PROJECT.md`.
    pub fn try_acquire_for(project: Option<&str>) -> Result<Option<Self>> {
        let path = project_lock_path(project)?;

        // Check existing lock
        if path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&path)
                && let Ok(pid) = contents.trim().parse::<u32>()
                && is_pid_alive(pid)
                // A lock naming *this* process cannot be held by a cycle that
                // is still running: `jcode server reload` re-execs in place and
                // keeps the PID, so a lock left by the pre-exec image looks
                // alive and would deadlock the runner against its own ghost.
                && pid != std::process::id()
            {
                return Ok(None); // Another instance is running
            }
            let _ = std::fs::remove_file(&path);
        }

        // Write our PID
        let pid = std::process::id();
        if let Some(parent) = path.parent() {
            storage::ensure_dir(parent)?;
        }
        std::fs::write(&path, pid.to_string())?;

        Ok(Some(Self { lock_path: path }))
    }

    pub fn release(self) -> Result<()> {
        let _ = std::fs::remove_file(&self.lock_path);
        // Drop runs, but we already cleaned up
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for AmbientLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// Whether a *different* live process currently holds the global ambient lock.
/// See `docs/AMBIENT_MODE.md` for why startup recovery needs this.
pub fn is_locked_by_another_process() -> bool {
    is_locked_by_another_process_for(None)
}

/// Per-project variant of [`is_locked_by_another_process`].
pub fn is_locked_by_another_process_for(project: Option<&str>) -> bool {
    let Ok(path) = project_lock_path(project) else {
        return false;
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return false;
    };
    match contents.trim().parse::<u32>() {
        Ok(pid) => pid != std::process::id() && is_pid_alive(pid),
        Err(_) => false,
    }
}

fn is_pid_alive(pid: u32) -> bool {
    crate::platform::is_process_running(pid)
}
