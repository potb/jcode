use super::{Tool, ToolContext, ToolOutput};
use crate::ambient::{
    AmbientCycleResult, AmbientManager, AmbientState, CycleStatus, Priority, ScheduleRequest,
    ScheduleTarget, ScheduledItem,
};
use crate::ambient_runner::AmbientRunnerHandle;
use crate::safety::{self, PermissionRequest, PermissionResult, SafetySystem, Urgency};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Global state for ambient tools
// ---------------------------------------------------------------------------

/// Global ambient cycle result, set by EndAmbientCycleTool for the ambient
/// runner to collect after the cycle completes.
static AMBIENT_CYCLE_RESULT: OnceLock<Mutex<Option<AmbientCycleResult>>> = OnceLock::new();

fn cycle_result_slot() -> &'static Mutex<Option<AmbientCycleResult>> {
    AMBIENT_CYCLE_RESULT.get_or_init(|| Mutex::new(None))
}

/// Store a cycle result for the ambient runner to pick up.
pub fn store_cycle_result(result: AmbientCycleResult) {
    if let Ok(mut slot) = cycle_result_slot().lock() {
        *slot = Some(result);
    }
}

/// Take the stored cycle result (returns None if not set or already taken).
pub fn take_cycle_result() -> Option<AmbientCycleResult> {
    cycle_result_slot()
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
}

/// Global SafetySystem instance shared with ambient tools.
static SAFETY_SYSTEM: OnceLock<Arc<SafetySystem>> = OnceLock::new();
/// Shared schedule/ambient runner handle used to wake the background loop after
/// queue changes.
static SCHEDULE_RUNNER: OnceLock<Mutex<Option<AmbientRunnerHandle>>> = OnceLock::new();
/// Session IDs currently allowed to use ambient-only permission workflows.
static AMBIENT_SESSION_IDS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

pub fn init_safety_system(system: Arc<SafetySystem>) {
    let _ = SAFETY_SYSTEM.set(system);
}

pub fn init_schedule_runner(handle: AmbientRunnerHandle) {
    if let Ok(mut slot) = SCHEDULE_RUNNER.get_or_init(|| Mutex::new(None)).lock() {
        *slot = Some(handle);
    }
}

fn get_safety_system() -> Arc<SafetySystem> {
    SAFETY_SYSTEM
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(SafetySystem::new()))
}

fn ambient_session_ids() -> &'static Mutex<HashSet<String>> {
    AMBIENT_SESSION_IDS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Mark a session ID as ambient-enabled for ambient-only tooling.
pub fn register_ambient_session(session_id: impl Into<String>) {
    if let Ok(mut ids) = ambient_session_ids().lock() {
        ids.insert(session_id.into());
    }
}

/// Remove a session ID from the ambient-enabled set.
pub fn unregister_ambient_session(session_id: &str) {
    if let Ok(mut ids) = ambient_session_ids().lock() {
        ids.remove(session_id);
    }
}

fn is_ambient_session_registered(session_id: &str) -> bool {
    ambient_session_ids()
        .lock()
        .map(|ids| ids.contains(session_id))
        .unwrap_or(false)
}

fn ensure_ambient_session(ctx: &ToolContext) -> Result<()> {
    if is_ambient_session_registered(&ctx.session_id) {
        Ok(())
    } else {
        anyhow::bail!(
            "request_permission is only available to ambient sessions (session '{}')",
            ctx.session_id
        )
    }
}

// ===========================================================================
// EndAmbientCycleTool
// ===========================================================================

pub struct EndAmbientCycleTool;

impl Default for EndAmbientCycleTool {
    fn default() -> Self {
        Self::new()
    }
}

impl EndAmbientCycleTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct EndCycleInput {
    summary: String,
    #[serde(deserialize_with = "super::serde_coerce::u32_from_string_or_number")]
    memories_modified: u32,
    #[serde(deserialize_with = "super::serde_coerce::u32_from_string_or_number")]
    compactions: u32,
    #[serde(default)]
    proactive_work: Option<String>,
    #[serde(default)]
    significance: Option<String>,
    #[serde(default)]
    next_schedule: Option<NextScheduleInput>,
}

#[derive(Deserialize)]
struct NextScheduleInput {
    #[serde(
        default,
        deserialize_with = "super::serde_coerce::opt_u32_from_string_or_number"
    )]
    wake_in_minutes: Option<u32>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    priority: Option<String>,
}

#[async_trait]
impl Tool for EndAmbientCycleTool {
    fn name(&self) -> &str {
        "end_ambient_cycle"
    }

    fn description(&self) -> &str {
        "End the current ambient cycle."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["summary", "memories_modified", "compactions"],
            "properties": {
                "intent": super::intent_schema_property(),
                "summary": {
                    "type": "string",
                    "description": "Human-readable summary of what was done this cycle"
                },
                "memories_modified": {
                    "type": "integer",
                    "description": "Count of memories created, merged, pruned, or updated"
                },
                "compactions": {
                    "type": "integer",
                    "description": "Number of context compactions during this cycle"
                },
                "proactive_work": {
                    "type": "string",
                    "description": "Description of proactive code changes, if any"
                },
                "significance": {
                    "type": "string",
                    "enum": ["routine", "notable"],
                    "description": "Does the user need to see this cycle? Use \"routine\" for garden/maintenance cycles where nothing changed for them (this sends NO notification, which is the default). Use \"notable\" only when they would want to know: you are blocked on them, you found something needing a decision, or you finished work they were waiting on. Permission requests, failures and code changes always notify regardless."
                },
                "next_schedule": {
                    "type": "object",
                    "description": "When to wake next and what to do",
                    "properties": {
                        "wake_in_minutes": {
                            "type": "integer",
                            "description": "Minutes until next wake"
                        },
                        "context": {
                            "type": "string",
                            "description": "What to do next cycle"
                        },
                        "priority": {
                            "type": "string",
                            "enum": ["low", "normal", "high"],
                            "description": "Priority for next cycle"
                        }
                    }
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: EndCycleInput = serde_json::from_value(input)?;

        let next_schedule = params.next_schedule.map(|ns| ScheduleRequest {
            wake_in_minutes: ns.wake_in_minutes,
            wake_at: None,
            context: ns.context.unwrap_or_default(),
            priority: parse_priority(ns.priority.as_deref()),
            target: ScheduleTarget::Ambient,
            created_by_session: ctx.session_id.clone(),
            working_dir: None,
            task_description: None,
            relevant_files: Vec::new(),
            git_branch: None,
            additional_context: None,
        });

        let now = Utc::now();
        let result = AmbientCycleResult {
            summary: params.summary.clone(),
            memories_modified: params.memories_modified,
            compactions: params.compactions,
            proactive_work: params.proactive_work,
            significance: params.significance,
            next_schedule: next_schedule.clone(),
            started_at: now, // approximate; the runner will override if it tracks start time
            ended_at: now,
            status: CycleStatus::Complete,
            conversation: None, // populated by the runner after cycle completes
        };

        // Store for the ambient runner to pick up
        store_cycle_result(result);

        // Also persist state immediately so a crash after this tool but before
        // the runner collects won't lose the cycle.
        if let Ok(mut state) = AmbientState::load() {
            let next_desc = if let Some(ref sched) = next_schedule {
                let mins = sched.wake_in_minutes.unwrap_or(30);
                format!("~{}", crate::ambient::format_minutes_human(mins))
            } else {
                "system default".to_string()
            };

            state.last_run = Some(now);
            state.last_summary = Some(params.summary.clone());
            state.last_compactions = Some(params.compactions);
            state.last_memories_modified = Some(params.memories_modified);
            state.total_cycles += 1;
            let _ = state.save();

            Ok(ToolOutput::new(format!(
                "Ambient cycle ended. Memories modified: {}, compactions: {}. Next wake: {}",
                params.memories_modified, params.compactions, next_desc
            ))
            .with_title("ambient cycle ended".to_string()))
        } else {
            Ok(ToolOutput::new(format!(
                "Ambient cycle ended (state save failed). Summary: {}",
                params.summary
            ))
            .with_title("ambient cycle ended".to_string()))
        }
    }
}

// ===========================================================================
// ScheduleAmbientTool
// ===========================================================================

pub struct ScheduleAmbientTool;

impl Default for ScheduleAmbientTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduleAmbientTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ScheduleInput {
    #[serde(
        default,
        deserialize_with = "super::serde_coerce::opt_u32_from_string_or_number"
    )]
    wake_in_minutes: Option<u32>,
    #[serde(default)]
    wake_at: Option<String>,
    context: String,
    #[serde(default)]
    priority: Option<String>,
}

#[async_trait]
impl Tool for ScheduleAmbientTool {
    fn name(&self) -> &str {
        "schedule_ambient"
    }

    fn description(&self) -> &str {
        "Schedule an ambient task."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["context"],
            "properties": {
                "intent": super::intent_schema_property(),
                "wake_in_minutes": {
                    "type": "integer",
                    "description": "Minutes from now to wake"
                },
                "wake_at": {
                    "type": "string",
                    "description": "ISO 8601 timestamp for when to wake (alternative to wake_in_minutes)"
                },
                "context": {
                    "type": "string",
                    "description": "What to do when waking — stored in the scheduled queue"
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "normal", "high"],
                    "description": "Priority for this scheduled task (default: normal)"
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: ScheduleInput = serde_json::from_value(input)?;

        let wake_at = if let Some(ref ts) = params.wake_at {
            Some(
                ts.parse::<chrono::DateTime<Utc>>()
                    .map_err(|e| anyhow::anyhow!("Invalid wake_at timestamp: {}", e))?,
            )
        } else {
            None
        };

        let request = ScheduleRequest {
            wake_in_minutes: params.wake_in_minutes,
            wake_at,
            context: params.context.clone(),
            priority: parse_priority(params.priority.as_deref()),
            target: ScheduleTarget::Ambient,
            created_by_session: ctx.session_id,
            working_dir: None,
            task_description: None,
            relevant_files: Vec::new(),
            git_branch: None,
            additional_context: None,
        };

        let mut manager = AmbientManager::new()?;
        let id = manager.schedule(request)?;
        nudge_schedule_runner();

        let when = if let Some(ref ts) = params.wake_at {
            ts.clone()
        } else if let Some(mins) = params.wake_in_minutes {
            format!("in {}", crate::ambient::format_minutes_human(mins))
        } else {
            "in 30m (default)".to_string()
        };

        Ok(
            ToolOutput::new(format!("Scheduled ambient task {} for {}", id, when))
                .with_title(format!("scheduled: {}", params.context)),
        )
    }
}

// ===========================================================================
// RequestPermissionTool
// ===========================================================================

pub struct RequestPermissionTool;

impl Default for RequestPermissionTool {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestPermissionTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct RequestPermissionInput {
    action: String,
    description: String,
    rationale: String,
    #[serde(default)]
    urgency: Option<String>,
    #[serde(
        default = "default_false",
        deserialize_with = "super::serde_coerce::bool_from_string_or_bool"
    )]
    wait: bool,
    #[serde(default)]
    context: Option<Value>,
}

fn default_false() -> bool {
    false
}

fn extract_context_string(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            value.as_str().and_then(|s| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
        })
    })
}

fn extract_context_list(map: &Map<String, Value>, keys: &[&str]) -> Vec<String> {
    for key in keys {
        let Some(value) = map.get(*key) else {
            continue;
        };

        if let Some(items) = value.as_array() {
            let list: Vec<String> = items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect();
            if !list.is_empty() {
                return list;
            }
        } else if let Some(single) = value.as_str() {
            let trimmed = single.trim();
            if !trimmed.is_empty() {
                return vec![trimmed.to_string()];
            }
        }
    }
    Vec::new()
}

fn build_permission_review_context(
    action: &str,
    description: &str,
    rationale: &str,
    context: Option<&Value>,
) -> Value {
    let context_obj = context.and_then(Value::as_object);

    let summary = context_obj
        .and_then(|m| extract_context_string(m, &["summary", "what", "activity_summary"]))
        .unwrap_or_else(|| description.to_string());

    let why_permission_needed = context_obj
        .and_then(|m| {
            extract_context_string(
                m,
                &[
                    "why_permission_needed",
                    "why",
                    "reason",
                    "rationale",
                    "justification",
                ],
            )
        })
        .unwrap_or_else(|| rationale.to_string());

    let mut review = Map::new();
    review.insert("summary".to_string(), Value::String(summary));
    review.insert(
        "why_permission_needed".to_string(),
        Value::String(why_permission_needed),
    );
    review.insert(
        "requested_action".to_string(),
        Value::String(action.to_string()),
    );

    let string_fields: [(&str, &[&str]); 4] = [
        (
            "current_activity",
            &["current_activity", "activity", "task", "current_task"],
        ),
        (
            "expected_outcome",
            &["expected_outcome", "outcome", "success_criteria", "success"],
        ),
        ("impact", &["impact", "user_impact"]),
        ("rollback_plan", &["rollback_plan", "rollback"]),
    ];

    if let Some(map) = context_obj {
        for (field_name, keys) in string_fields {
            if let Some(value) = extract_context_string(map, keys) {
                review.insert(field_name.to_string(), Value::String(value));
            }
        }

        let list_fields: [(&str, &[&str]); 4] = [
            (
                "planned_steps",
                &["planned_steps", "steps", "plan", "checklist"],
            ),
            ("files", &["files", "file_paths", "planned_files"]),
            ("commands", &["commands", "planned_commands"]),
            ("risks", &["risks", "risk", "safety_risks"]),
        ];

        for (field_name, keys) in list_fields {
            let items = extract_context_list(map, keys);
            if !items.is_empty() {
                review.insert(
                    field_name.to_string(),
                    Value::Array(items.into_iter().map(Value::String).collect()),
                );
            }
        }
    }

    if let Some(raw) = context
        && !raw.is_object()
    {
        review.insert("notes".to_string(), raw.clone());
    }

    Value::Object(review)
}

#[async_trait]
impl Tool for RequestPermissionTool {
    fn name(&self) -> &str {
        "request_permission"
    }

    fn description(&self) -> &str {
        "Request user permission."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action", "description", "rationale"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "description": "The action requiring permission (e.g., 'create_pull_request', 'push', 'edit')"
                },
                "description": {
                    "type": "string",
                    "description": "What the action will do"
                },
                "rationale": {
                    "type": "string",
                    "description": "Why this action is beneficial"
                },
                "urgency": {
                    "type": "string",
                    "enum": ["low", "normal", "high"],
                    "description": "How urgent the permission request is (default: normal)"
                },
                "wait": {
                    "type": "boolean",
                    "description": "If true, block until user decides (with timeout). If false, queue and continue."
                },
                "context": {
                    "type": "object",
                    "description": "Structured reviewer context. Include summary of current work and why permission is needed.",
                    "properties": {
                        "summary": {
                            "type": "string",
                            "description": "One-paragraph summary of what you are currently doing"
                        },
                        "why_permission_needed": {
                            "type": "string",
                            "description": "Why this action needs user approval right now"
                        },
                        "current_activity": {
                            "type": "string",
                            "description": "Current task or ambient objective"
                        },
                        "planned_steps": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Short ordered plan of intended steps"
                        },
                        "files": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Files expected to be created/modified"
                        },
                        "commands": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Commands expected to be executed"
                        },
                        "risks": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Known risks or side effects"
                        },
                        "rollback_plan": {
                            "type": "string",
                            "description": "How to back out changes if needed"
                        },
                        "expected_outcome": {
                            "type": "string",
                            "description": "What successful completion should look like"
                        }
                    },
                    "additionalProperties": true
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        ensure_ambient_session(&ctx)?;

        let params: RequestPermissionInput = serde_json::from_value(input)?;

        let urgency = match params.urgency.as_deref() {
            Some("low") => Urgency::Low,
            Some("high") => Urgency::High,
            _ => Urgency::Normal,
        };

        let request_id = safety::new_request_id();
        let now = Utc::now();
        let review = build_permission_review_context(
            &params.action,
            &params.description,
            &params.rationale,
            params.context.as_ref(),
        );
        let mut request_context = json!({
            "session_id": ctx.session_id,
            "message_id": ctx.message_id,
            "tool_call_id": ctx.tool_call_id,
            "working_dir": ctx.working_dir.as_ref().map(|p| p.display().to_string()),
            "requested_at": now.to_rfc3339(),
        });
        if let Some(obj) = request_context.as_object_mut() {
            obj.insert("review".to_string(), review);
            if let Some(user_context) = params.context {
                obj.insert("details".to_string(), user_context);
            }
        }

        let request = PermissionRequest {
            id: request_id.clone(),
            action: params.action.clone(),
            description: params.description.clone(),
            rationale: params.rationale.clone(),
            urgency,
            wait: params.wait,
            created_at: now,
            context: Some(request_context),
        };

        let system = get_safety_system();

        // Auto-approve mode: the user has pre-authorized ambient work, so
        // queueing here would only stall the cycle waiting for an answer that
        // never comes. Record an approval decision so the audit trail still
        // shows what was done and on whose authority.
        if crate::config::config().ambient.auto_approve_permissions {
            let msg = format!(
                "Auto-approved by ambient.auto_approve_permissions (action '{}').",
                params.action
            );
            system.record_decision(&request_id, true, "ambient_auto_approve", Some(msg.clone()))?;
            return Ok(ToolOutput::new(format!(
                "Permission approved: {} Proceed without waiting.",
                msg
            )));
        }

        let result = system.request_permission(request);

        let output = match result {
            PermissionResult::Approved { ref message } => {
                let msg = message.as_deref().unwrap_or("no message");
                format!("Permission approved: {}", msg)
            }
            PermissionResult::Denied { ref reason } => {
                let reason = reason.as_deref().unwrap_or("no reason given");
                format!("Permission denied: {}", reason)
            }
            PermissionResult::Queued { ref request_id } => {
                format!(
                    "Permission request queued (id: {}). \
                     Action '{}' is pending user review.",
                    request_id, params.action
                )
            }
            PermissionResult::Timeout => {
                "Permission request timed out. The user did not respond in time.".to_string()
            }
        };

        Ok(ToolOutput::new(output).with_title(format!("permission: {}", params.action)))
    }
}

// ===========================================================================
// ScheduleTool — available to normal sessions to queue future ambient tasks
// ===========================================================================

pub struct ScheduleTool;

impl Default for ScheduleTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduleTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ScheduleToolInput {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    schedule_id: Option<String>,
    #[serde(default)]
    task: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_coerce::opt_u32_from_string_or_number"
    )]
    wake_in_minutes: Option<u32>,
    #[serde(default)]
    wake_at: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    relevant_files: Vec<String>,
    #[serde(default)]
    background_context: Option<String>,
    #[serde(default)]
    success_criteria: Option<String>,
    #[serde(default)]
    target: Option<String>,
}

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        "schedule"
    }

    fn description(&self) -> &str {
        "Schedule, list, or cancel future tasks."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "cancel"],
                    "description": "Action to perform. Defaults to create for backwards compatibility."
                },
                "schedule_id": {
                    "type": "string",
                    "description": "Scheduled task ID. Required for action=cancel."
                },
                "task": {
                    "type": "string",
                    "description": "Task. Required for action=create."
                },
                "wake_in_minutes": { "type": "integer" },
                "wake_at": { "type": "string" },
                "priority": {
                    "type": "string",
                    "enum": ["low", "normal", "high"]
                },
                "relevant_files": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "background_context": {
                    "type": "string",
                    "description": "Optional background context for the scheduled task."
                },
                "success_criteria": { "type": "string" },
                "target": {
                    "type": "string",
                    "enum": ["resume", "spawn", "ambient"],
                    "description": "Delivery target. Defaults to resuming this session; 'spawn' runs one new child session."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: ScheduleToolInput = serde_json::from_value(input)?;

        match params.action.as_deref().unwrap_or("create") {
            "create" => self.execute_create(params, ctx).await,
            "list" => self.execute_list().await,
            "cancel" => self.execute_cancel(params).await,
            other => anyhow::bail!(
                "Invalid action '{}'. Expected one of: create, list, cancel",
                other
            ),
        }
    }
}

impl ScheduleTool {
    async fn execute_create(
        &self,
        params: ScheduleToolInput,
        ctx: ToolContext,
    ) -> Result<ToolOutput> {
        let task = params
            .task
            .clone()
            .ok_or_else(|| anyhow::anyhow!("task is required for action=create"))?;

        if params.wake_in_minutes.is_none() && params.wake_at.is_none() {
            anyhow::bail!(
                "Either wake_in_minutes or wake_at is required. \
                 This tool is for scheduling future tasks."
            );
        }

        let wake_at = if let Some(ref ts) = params.wake_at {
            Some(
                ts.parse::<chrono::DateTime<Utc>>()
                    .map_err(|e| anyhow::anyhow!("Invalid wake_at timestamp: {}", e))?,
            )
        } else {
            None
        };

        let working_dir = ctx.working_dir.as_ref().map(|p| p.display().to_string());

        let git_branch = ctx
            .working_dir
            .as_ref()
            .and_then(|wd| {
                std::process::Command::new("git")
                    .args(["rev-parse", "--abbrev-ref", "HEAD"])
                    .current_dir(wd)
                    .output()
                    .ok()
            })
            .and_then(|out| {
                if out.status.success() {
                    String::from_utf8(out.stdout)
                        .ok()
                        .map(|s| s.trim().to_string())
                } else {
                    None
                }
            });

        let target = parse_schedule_target(params.target.as_deref(), &ctx.session_id)?;
        let target_summary = format_schedule_target(&target);

        let request = ScheduleRequest {
            wake_in_minutes: params.wake_in_minutes,
            wake_at,
            context: task.clone(),
            priority: parse_priority(params.priority.as_deref()),
            target,
            created_by_session: ctx.session_id.clone(),
            working_dir: working_dir.clone(),
            task_description: Some(task.clone()),
            relevant_files: params.relevant_files.clone(),
            git_branch,
            additional_context: {
                let mut parts = Vec::new();
                if let Some(ref bg) = params.background_context {
                    parts.push(format!("Background: {}", bg));
                }
                if let Some(ref sc) = params.success_criteria {
                    parts.push(format!("Success criteria: {}", sc));
                }
                parts.push(format!("Scheduled by session: {}", ctx.session_id));
                Some(parts.join("\n"))
            },
        };

        let mut manager = AmbientManager::new()?;
        let id = manager.schedule(request)?;
        nudge_schedule_runner();

        let when = if let Some(ref ts) = params.wake_at {
            ts.clone()
        } else if let Some(mins) = params.wake_in_minutes {
            format!("in {}", crate::ambient::format_minutes_human(mins))
        } else {
            "unspecified".to_string()
        };

        let mut summary = format!("Scheduled task '{}' for {} (id: {})", task, when, id);
        if let Some(ref wd) = working_dir {
            summary.push_str(&format!("\nWorking directory: {}", wd));
        }
        if !params.relevant_files.is_empty() {
            summary.push_str(&format!(
                "\nRelevant files: {}",
                params.relevant_files.join(", ")
            ));
        }
        summary.push_str(&format!("\nTarget: {}", target_summary));

        Ok(ToolOutput::new(summary).with_title(format!("scheduled: {}", task)))
    }

    async fn execute_list(&self) -> Result<ToolOutput> {
        let manager = AmbientManager::new()?;
        let mut items: Vec<&ScheduledItem> = manager.queue().items().iter().collect();
        items.sort_by_key(|item| item.scheduled_for);

        if items.is_empty() {
            return Ok(ToolOutput::new("No scheduled tasks."));
        }

        let mut summary = format!("{} scheduled task(s):", items.len());
        for item in items {
            summary.push('\n');
            summary.push_str(&format_scheduled_item(item));
        }

        Ok(ToolOutput::new(summary).with_title("scheduled tasks"))
    }

    async fn execute_cancel(&self, params: ScheduleToolInput) -> Result<ToolOutput> {
        let id = params
            .schedule_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("schedule_id is required for action=cancel"))?;

        let mut manager = AmbientManager::new()?;
        let Some(item) = manager.cancel_schedule(id)? else {
            anyhow::bail!("No scheduled task found with id '{}'", id);
        };
        nudge_schedule_runner();

        Ok(ToolOutput::new(format!(
            "Cancelled scheduled task '{}' for {} (id: {})",
            item.task_description.as_deref().unwrap_or(&item.context),
            item.scheduled_for,
            item.id
        ))
        .with_title(format!("cancelled: {}", item.id)))
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn parse_priority(s: Option<&str>) -> Priority {
    match s {
        Some("low") => Priority::Low,
        Some("high") => Priority::High,
        _ => Priority::Normal,
    }
}

fn parse_schedule_target(s: Option<&str>, session_id: &str) -> Result<ScheduleTarget> {
    Ok(match s {
        Some("ambient") => ScheduleTarget::Ambient,
        Some("spawn") => ScheduleTarget::Spawn {
            parent_session_id: session_id.to_string(),
        },
        Some("resume") | None => ScheduleTarget::Session {
            session_id: session_id.to_string(),
        },
        Some(other) => anyhow::bail!(
            "Invalid target '{}'. Expected one of: resume, spawn, ambient",
            other
        ),
    })
}

fn format_schedule_target(target: &ScheduleTarget) -> String {
    match target {
        ScheduleTarget::Ambient => "ambient agent".to_string(),
        ScheduleTarget::Session { session_id } => format!("resume session {}", session_id),
        ScheduleTarget::Spawn { parent_session_id } => {
            format!("spawn one child session from {}", parent_session_id)
        }
    }
}

fn format_scheduled_item(item: &ScheduledItem) -> String {
    format!(
        "- {} | {} | {:?} | {} | {}",
        item.id,
        item.scheduled_for,
        item.priority,
        format_schedule_target(&item.target),
        item.task_description.as_deref().unwrap_or(&item.context)
    )
}

fn nudge_schedule_runner() {
    let runner = SCHEDULE_RUNNER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|slot| slot.clone());
    if let Some(runner) = runner {
        runner.nudge();
    }
}

// ---------------------------------------------------------------------------
// SendChannelMessageTool — send messages via any configured channel
// ---------------------------------------------------------------------------

/// Explain any channel the user switched on that could not be registered.
///
/// Without this, a half-configured channel (say `github_enabled = true` with
/// no token) is indistinguishable from having configured nothing at all, so
/// the caller is told to "enable telegram or discord" when the real problem is
/// a missing credential on a channel they already enabled.
fn skipped_suffix(registry: &crate::channel::ChannelRegistry) -> String {
    let reasons = registry.skipped_reasons();
    if reasons.is_empty() {
        return String::new();
    }
    format!(" Skipped: {}.", reasons.join("; "))
}

pub struct SendChannelMessageTool;

impl Default for SendChannelMessageTool {
    fn default() -> Self {
        Self::new()
    }
}

impl SendChannelMessageTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SendChannelMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a user message."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "message": {
                    "type": "string",
                    "description": "The message text to send"
                },
                "channel": {
                    "type": "string",
                    "description": "Optional: specific channel to send to (e.g. 'telegram', 'discord', 'github'). Omit to send to all. On github this opens a NEW issue per message."
                },
                "thread": {
                    "type": "string",
                    "description": "Optional: existing thread to reply in instead of starting a new one. For github, the issue number (e.g. '12')."
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> Result<ToolOutput> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required parameter: message"))?;

        let channel_name = args.get("channel").and_then(|v| v.as_str());
        // Replying in an existing thread is what keeps one topic in one place;
        // without it every answer would open a fresh issue and the thread the
        // user is reading would go silent.
        let thread = args
            .get("thread")
            .and_then(|v| v.as_str())
            .map(|t| t.trim())
            .filter(|t| !t.is_empty());

        let config = crate::config::config();
        let registry = crate::channel::ChannelRegistry::from_config(&config.safety);

        if let Some(name) = channel_name {
            match registry.find_by_name(name) {
                Some(ch) => match match thread {
                    Some(t) => ch.send_to_thread(t, message).await,
                    None => ch.send(message).await,
                } {
                    Ok(()) => Ok(ToolOutput::new(match thread {
                        Some(t) => format!("Message sent via {} in thread {}.", name, t),
                        None => format!("Message sent via {}.", name),
                    })),
                    Err(e) => Ok(ToolOutput::new(format!(
                        "Failed to send via {}: {}",
                        name, e
                    ))),
                },
                None => {
                    let available = registry.channel_names();
                    Ok(ToolOutput::new(format!(
                        "Channel '{}' not found. Available: {}{}",
                        name,
                        if available.is_empty() {
                            "none configured".to_string()
                        } else {
                            available.join(", ")
                        },
                        skipped_suffix(&registry)
                    )))
                }
            }
        } else {
            let channels = registry.send_enabled();
            if channels.is_empty() {
                return Ok(ToolOutput::new(format!(
                    "No messaging channels configured. Enable telegram, discord, or github \
                     under [safety] in config.{}",
                    skipped_suffix(&registry)
                )));
            }
            let mut results = Vec::new();
            for ch in &channels {
                match ch.send(message).await {
                    Ok(()) => results.push(format!("✓ {}", ch.name())),
                    Err(e) => results.push(format!("✗ {}: {}", ch.name(), e)),
                }
            }
            Ok(ToolOutput::new(format!(
                "Message sent: {}",
                results.join(", ")
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// GitHubIssueTool — manage the per-topic issue threads
// ---------------------------------------------------------------------------

/// Manage the GitHub issues the ambient agent uses as topic threads.
///
/// `send_message` opens new topics; this tool is the rest of the lifecycle:
/// list what is still open, continue a topic, and close it when resolved. Open
/// issues are only a useful backlog if resolved ones actually get closed.
pub struct GitHubIssueTool;

impl Default for GitHubIssueTool {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubIssueTool {
    pub fn new() -> Self {
        Self
    }

    fn channel() -> Result<crate::channel::GitHubChannel> {
        let config = crate::config::config();
        let safety = &config.safety;
        if !safety.github_enabled {
            anyhow::bail!("The GitHub channel is disabled (safety.github_enabled).");
        }
        let repo = safety
            .github_repo
            .clone()
            .ok_or_else(|| anyhow::anyhow!("safety.github_repo is not set"))?;
        let token = crate::channel::GitHubChannel::resolve_token(safety.github_token.as_deref())
            .ok_or_else(|| {
                anyhow::anyhow!("No GitHub token available (config, GITHUB_TOKEN, or `gh auth`)")
            })?;
        Ok(crate::channel::GitHubChannel::new(
            repo,
            safety.github_label.clone(),
            token,
            safety.github_allowed_logins.clone(),
            safety.github_reply_enabled,
            safety.github_poll_seconds,
        ))
    }
}

#[async_trait]
impl Tool for GitHubIssueTool {
    fn name(&self) -> &str {
        "github_issue"
    }

    fn description(&self) -> &str {
        "List, comment on, open, or close the GitHub issues used as topic threads with the user."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["list", "open", "comment", "close"],
                    "description": "list = open topics; open = new topic issue; comment = continue a topic; close = mark a topic resolved"
                },
                "issue": {
                    "type": "integer",
                    "description": "Issue number, required for comment and close"
                },
                "title": {
                    "type": "string",
                    "description": "Title for action=open. Defaults to the first line of body."
                },
                "body": {
                    "type": "string",
                    "description": "Issue body for action=open, or the comment text for action=comment"
                },
                "comment": {
                    "type": "string",
                    "description": "Optional closing comment posted before closing, for action=close"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> Result<ToolOutput> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list")
            .to_lowercase();

        let channel = match Self::channel() {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::new(e.to_string())),
        };

        let issue_num = args.get("issue").and_then(|v| v.as_u64());
        let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");

        match action.as_str() {
            "list" => match channel.list_open_topics().await {
                Ok(items) if items.is_empty() => {
                    Ok(ToolOutput::new("No open topic issues.".to_string()))
                }
                Ok(items) => {
                    let lines: Vec<String> =
                        items.iter().map(|(n, t)| format!("#{} {}", n, t)).collect();
                    Ok(ToolOutput::new(format!(
                        "Open topics:\n{}",
                        lines.join("\n")
                    )))
                }
                Err(e) => Ok(ToolOutput::new(format!("Failed to list issues: {}", e))),
            },
            "open" => {
                if body.trim().is_empty() {
                    return Ok(ToolOutput::new("action=open requires body".to_string()));
                }
                let (derived_title, derived_body) = crate::channel::split_title_body(body);
                let title = args
                    .get("title")
                    .and_then(|v| v.as_str())
                    .filter(|t| !t.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or(derived_title);
                match channel.open_issue(&title, &derived_body).await {
                    Ok(n) => Ok(ToolOutput::new(format!("Opened issue #{}.", n))),
                    Err(e) => Ok(ToolOutput::new(format!("Failed to open issue: {}", e))),
                }
            }
            "comment" => {
                let Some(n) = issue_num else {
                    return Ok(ToolOutput::new("action=comment requires issue".to_string()));
                };
                if body.trim().is_empty() {
                    return Ok(ToolOutput::new("action=comment requires body".to_string()));
                }
                match channel.comment(n, body).await {
                    Ok(()) => Ok(ToolOutput::new(format!("Commented on #{}.", n))),
                    Err(e) => Ok(ToolOutput::new(format!("Failed to comment: {}", e))),
                }
            }
            "close" => {
                let Some(n) = issue_num else {
                    return Ok(ToolOutput::new("action=close requires issue".to_string()));
                };
                // Close with a reason where one was given: an issue that just
                // goes quiet tells the user nothing about how it ended.
                if let Some(c) = args
                    .get("comment")
                    .and_then(|v| v.as_str())
                    .filter(|c| !c.trim().is_empty())
                    && let Err(e) = channel.comment(n, c).await
                {
                    return Ok(ToolOutput::new(format!(
                        "Failed to post closing comment: {}",
                        e
                    )));
                }
                match channel.close_issue(n).await {
                    Ok(()) => Ok(ToolOutput::new(format!("Closed #{}.", n))),
                    Err(e) => Ok(ToolOutput::new(format!("Failed to close: {}", e))),
                }
            }
            other => Ok(ToolOutput::new(format!(
                "Unknown action '{}'. Use list, open, comment, or close.",
                other
            ))),
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "ambient/tests.rs"]
mod tests;
