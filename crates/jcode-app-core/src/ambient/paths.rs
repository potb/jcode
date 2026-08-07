use anyhow::Result;
use std::path::PathBuf;

use crate::storage;

// ---------------------------------------------------------------------------
// Storage paths
// ---------------------------------------------------------------------------

pub(super) fn ambient_dir() -> Result<PathBuf> {
    let dir = storage::jcode_dir()?.join("ambient");
    storage::ensure_dir(&dir)?;
    Ok(dir)
}

pub(super) fn state_path() -> Result<PathBuf> {
    Ok(ambient_dir()?.join("state.json"))
}

pub(super) fn queue_path() -> Result<PathBuf> {
    Ok(ambient_dir()?.join("queue.json"))
}

pub(super) fn lock_path() -> Result<PathBuf> {
    Ok(ambient_dir()?.join("ambient.lock"))
}

/// Items a cycle has claimed but not yet finished acting on.
///
/// Claiming removes items from the queue so they are not replayed, which means
/// a process that dies mid-cycle would otherwise destroy them. Recording the
/// claim here lets the next startup put them back.
pub(super) fn inflight_path() -> Result<PathBuf> {
    Ok(ambient_dir()?.join("inflight.json"))
}

pub(super) fn transcripts_dir() -> Result<PathBuf> {
    let dir = ambient_dir()?.join("transcripts");
    storage::ensure_dir(&dir)?;
    Ok(dir)
}
