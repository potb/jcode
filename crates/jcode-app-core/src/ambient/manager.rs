use anyhow::Result;
use chrono::{DateTime, Utc};

use super::paths::{ambient_dir, inflight_path, queue_path, transcripts_dir};
use super::{
    AmbientCycleResult, AmbientState, AmbientStatus, ScheduleRequest, ScheduledItem, ScheduledQueue,
};
use crate::config::config;

// ---------------------------------------------------------------------------
// AmbientManager
// ---------------------------------------------------------------------------

pub struct AmbientManager {
    state: AmbientState,
    queue: ScheduledQueue,
}

impl AmbientManager {
    pub fn new() -> Result<Self> {
        // Ensure storage layout exists
        let _ = ambient_dir()?;
        let _ = transcripts_dir()?;

        let state = AmbientState::load()?;
        let queue = ScheduledQueue::load(queue_path()?);

        Ok(Self { state, queue })
    }

    pub fn is_enabled() -> bool {
        config().ambient.enabled
    }

    /// Check whether it's time to run a cycle based on current state and queue.
    pub fn should_run(&self) -> bool {
        if !Self::is_enabled() {
            return false;
        }

        match &self.state.status {
            AmbientStatus::Disabled | AmbientStatus::Paused { .. } => false,
            AmbientStatus::Running { .. } => false, // already running
            AmbientStatus::Idle => true,
            // A due queue item must be able to trigger a cycle on its own.
            // `next_wake` only reflects the adaptive maintenance interval, which
            // is set from the end of the last cycle and is routinely hours out.
            // Without this an item explicitly scheduled for, say, 45 minutes
            // from now would sit unrun until that unrelated interval elapsed.
            AmbientStatus::Scheduled { next_wake } => {
                Utc::now() >= *next_wake || self.has_due_ambient_item()
            }
        }
    }

    /// Earliest scheduled time among queue items that run as an ambient cycle.
    ///
    /// Items targeting a specific session (direct delivery) are excluded: they
    /// are handed to that session instead of starting a cycle, and the runner
    /// tracks their deadlines separately.
    pub fn next_ambient_item_due(&self) -> Option<DateTime<Utc>> {
        self.queue
            .items()
            .iter()
            .filter(|item| !item.target.is_direct_delivery())
            .map(|item| item.scheduled_for)
            .min()
    }

    /// Whether any ambient-targeted queue item is due now.
    pub fn has_due_ambient_item(&self) -> bool {
        self.next_ambient_item_due()
            .is_some_and(|due| Utc::now() >= due)
    }

    pub fn record_cycle_result(&mut self, result: AmbientCycleResult) -> Result<()> {
        self.state.record_cycle(&result);
        self.state.save()?;

        // If the cycle produced a schedule request, enqueue it
        if let Some(ref req) = result.next_schedule {
            self.schedule(req.clone())?;
        }

        Ok(())
    }

    /// Remove and return all ready scheduled items.
    pub fn take_ready_items(&mut self) -> Vec<ScheduledItem> {
        self.queue.pop_ready()
    }

    /// Put previously claimed items back on the queue.
    ///
    /// Claiming is optimistic: items leave the queue when a cycle is handed
    /// them. If that cycle then cannot run, the work would be lost outright,
    /// which is worse than the re-delivery claiming exists to prevent. Restoring
    /// them keeps the failure recoverable on the next wake.
    pub fn requeue_items(&mut self, items: Vec<ScheduledItem>) {
        for item in items {
            self.queue.push(item);
        }
        // The claim is over either way, so the crash-recovery record must go.
        // Leaving it would resurrect these items a second time at next startup.
        let _ = clear_inflight_items();
    }

    /// Remove and return the ready items that a cycle handles itself.
    ///
    /// A cycle is handed these in its prompt, so they must leave the queue at
    /// the same time. Leaving them behind replays the same instructions to
    /// every subsequent cycle forever, and leaves `overdue_queue_count` stuck
    /// above zero no matter how much work actually got done.
    pub fn take_ready_ambient_items(&mut self) -> Vec<ScheduledItem> {
        let claimed = self.queue.take_ready_ambient_items();
        // Record the claim before the cycle acts on it. If this process dies
        // mid-cycle no in-process undo can run, so startup replays this file
        // back onto the queue instead of the work vanishing.
        if !claimed.is_empty() {
            let _ = write_inflight_items(&claimed);
        }
        claimed
    }

    /// Mark the claimed items as fully handled, dropping the recovery record.
    pub fn clear_inflight(&mut self) {
        let _ = clear_inflight_items();
    }

    /// Restore items abandoned by a previous process that died mid-cycle.
    ///
    /// Returns how many were recovered. Safe to call when there is nothing to
    /// do, and idempotent: the record is cleared as the items go back.
    pub fn recover_inflight_items(&mut self) -> usize {
        let abandoned = read_inflight_items();
        if abandoned.is_empty() {
            let _ = clear_inflight_items();
            return 0;
        }
        // Don't duplicate anything a previous partial recovery already restored.
        let known: std::collections::HashSet<String> =
            self.queue.items().iter().map(|i| i.id.clone()).collect();
        let mut restored = 0;
        for item in abandoned {
            if !known.contains(&item.id) {
                self.queue.push(item);
                restored += 1;
            }
        }
        let _ = clear_inflight_items();
        restored
    }

    /// Remove and return only ready items targeted at direct delivery into a
    /// specific resumed or spawned session.
    pub fn take_ready_direct_items(&mut self) -> Vec<ScheduledItem> {
        self.queue.take_ready_direct_items()
    }

    /// Add a schedule request to the queue. Returns the item ID.
    pub fn schedule(&mut self, request: ScheduleRequest) -> Result<String> {
        let id = format!("sched_{:08x}", rand::random::<u32>());
        let scheduled_for = request.wake_at.unwrap_or_else(|| {
            Utc::now() + chrono::Duration::minutes(request.wake_in_minutes.unwrap_or(30) as i64)
        });

        let item = ScheduledItem {
            id: id.clone(),
            scheduled_for,
            context: request.context,
            priority: request.priority,
            target: request.target,
            created_by_session: request.created_by_session,
            created_at: Utc::now(),
            working_dir: request.working_dir,
            task_description: request.task_description,
            relevant_files: request.relevant_files,
            git_branch: request.git_branch,
            additional_context: request.additional_context,
        };

        self.queue.push(item);
        Ok(id)
    }

    /// Cancel a queued scheduled item by ID.
    pub fn cancel_schedule(&mut self, id: &str) -> Result<Option<ScheduledItem>> {
        self.queue.remove_by_id(id)
    }

    pub fn state(&self) -> &AmbientState {
        &self.state
    }

    pub fn queue(&self) -> &ScheduledQueue {
        &self.queue
    }
}

// ---------------------------------------------------------------------------
// In-flight claim record (crash recovery)
// ---------------------------------------------------------------------------

/// Persist the items a cycle has claimed so a process that dies mid-cycle does
/// not take them with it.
fn write_inflight_items(items: &[ScheduledItem]) -> Result<()> {
    let path = inflight_path()?;
    if let Some(parent) = path.parent() {
        crate::storage::ensure_dir(parent)?;
    }
    crate::storage::write_json(&path, &items.to_vec())
}

/// Read the claim record left by a previous process. A missing or corrupt file
/// simply means nothing to recover; this must never be fatal at startup.
fn read_inflight_items() -> Vec<ScheduledItem> {
    inflight_path()
        .ok()
        .filter(|p| p.exists())
        .and_then(|p| crate::storage::read_json::<Vec<ScheduledItem>>(&p).ok())
        .unwrap_or_default()
}

fn clear_inflight_items() -> Result<()> {
    if let Ok(path) = inflight_path()
        && path.exists()
    {
        let _ = std::fs::remove_file(&path);
    }
    Ok(())
}
