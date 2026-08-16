use anyhow::Result;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use crate::storage;

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

/// Lock file for `project`, or [`lock_path`] when it belongs to no project.
/// See `docs/AMBIENT_PER_PROJECT.md`.
pub(super) fn project_lock_path(project: Option<&str>) -> Result<PathBuf> {
    let Some(project) = project else {
        return lock_path();
    };
    let dir = ambient_dir()?.join("locks");
    storage::ensure_dir(&dir)?;
    Ok(dir.join(format!("{}.lock", unique_file_stem_for(project))))
}

fn unique_file_stem_for(project: &str) -> String {
    format!(
        "{}-{:016x}",
        last_chars(&path_component_safe(project), READABLE_STEM_CHARS),
        hash_of(project)
    )
}

const READABLE_STEM_CHARS: usize = 48;

fn hash_of(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn path_component_safe(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn last_chars(value: &str, count: usize) -> String {
    let skip = value.chars().count().saturating_sub(count);
    value.chars().skip(skip).collect()
}

/// Items a cycle has claimed but not yet finished acting on, so a process that
/// dies mid-cycle does not destroy them. See `docs/AMBIENT_MODE.md`.
pub(super) fn inflight_path() -> Result<PathBuf> {
    Ok(ambient_dir()?.join("inflight.json"))
}

pub(super) fn transcripts_dir() -> Result<PathBuf> {
    let dir = ambient_dir()?.join("transcripts");
    storage::ensure_dir(&dir)?;
    Ok(dir)
}
