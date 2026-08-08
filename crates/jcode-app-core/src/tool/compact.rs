//! `compact` tool — lets the agent inspect context pressure and request a
//! manual compaction of its own conversation history.
//!
//! Compaction itself is *not* reimplemented here. Doing the work inside the
//! tool would mean summarizing a message list loaded from disk, which can drift
//! from the agent's in-memory history and corrupt the compaction cutoff. So the
//! tool records a per-session request, and the agent honors it at the single
//! existing compaction site (`Agent::messages_for_provider`) where the real
//! provider handle and the authoritative message list are both available.
//!
//! The request is consumed on the agent's next provider call, which happens
//! immediately after the tool result is appended, so a request placed mid-turn
//! takes effect within the same turn.

use anyhow::Result;
use async_trait::async_trait;
use super::{Tool, ToolContext, ToolOutput};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::RwLock;

use crate::compaction::CompactionManager;

/// Sessions with a pending manual compaction request.
fn pending() -> &'static Mutex<HashSet<String>> {
    static PENDING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Record a manual compaction request for `session_id`.
pub fn request_compaction(session_id: &str) {
    if let Ok(mut set) = pending().lock() {
        set.insert(session_id.to_string());
    }
}

/// Consume a pending manual compaction request for `session_id`.
///
/// Returns true exactly once per request, so a request can never be applied
/// twice.
pub fn take_request(session_id: &str) -> bool {
    match pending().lock() {
        Ok(mut set) => set.remove(session_id),
        Err(_) => false,
    }
}

/// Drop any pending request for `session_id` without acting on it.
pub fn clear_request(session_id: &str) {
    if let Ok(mut set) = pending().lock() {
        set.remove(session_id);
    }
}

#[derive(Debug, Deserialize)]
struct CompactInput {
    #[serde(default)]
    action: Option<String>,
}

pub struct CompactTool {
    compaction: Arc<RwLock<CompactionManager>>,
}

impl CompactTool {
    pub fn new(compaction: Arc<RwLock<CompactionManager>>) -> Self {
        Self { compaction }
    }
}

/// Render the current context-pressure status block.
pub fn format_status(manager: &CompactionManager) -> String {
    let stats = manager.stats();
    format!(
        "Context status:\n\
         - Turns recorded: {}\n\
         - Summary present: {}\n\
         - Compaction in progress: {}\n\
         - Estimated tokens: ~{}k / {}k budget ({:.1}%)\n\
         - Compactions so far: {}",
        stats.total_turns,
        if stats.has_summary { "yes" } else { "no" },
        if stats.is_compacting { "yes" } else { "no" },
        stats.effective_tokens / 1000,
        manager.token_budget() / 1000,
        stats.context_usage * 100.0,
        manager.compacted_count(),
    )
}

#[async_trait]
impl Tool for CompactTool {
    fn name(&self) -> &str {
        "compact"
    }

    fn description(&self) -> &str {
        "Inspect context pressure, or request compaction of your own older conversation history into a summary. Use action='status' to check usage and action='now' to request compaction. Compaction runs in the background and applies on a later turn; recent turns are always preserved. Request it when context usage is high and older history is no longer needed, not routinely."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "now", "cancel"],
                    "description": "status (default) reports context usage; now requests a background compaction; cancel withdraws a request not yet applied."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: CompactInput = serde_json::from_value(input).unwrap_or(CompactInput {
            action: Some("status".to_string()),
        });
        let action = params.action.as_deref().unwrap_or("status").to_lowercase();

        let manager = self.compaction.read().await;
        let status = format_status(&manager);
        let is_compacting = manager.is_compacting();
        drop(manager);

        match action.as_str() {
            "status" => Ok(ToolOutput::new(status)),
            "cancel" => {
                clear_request(&ctx.session_id);
                Ok(ToolOutput::new(format!(
                    "{status}\n\nPending compaction request cleared (a compaction already running is unaffected)."
                )))
            }
            "now" => {
                if is_compacting {
                    return Ok(ToolOutput::new(format!(
                        "{status}\n\nCompaction is already running; no new request queued."
                    )));
                }
                request_compaction(&ctx.session_id);
                Ok(ToolOutput::new(format!(
                    "{status}\n\nCompaction requested. It starts on the next provider call and \
                     applies once the summary is ready; recent turns are preserved. \
                     If context usage is too low or history too short, the request is \
                     declined and history is left untouched."
                )))
            }
            other => Err(anyhow::anyhow!(
                "Unknown action '{other}'. Use status, now, or cancel."
            )),
        }
    }
}

#[cfg(test)]
#[path = "compact_tests.rs"]
mod tests;
