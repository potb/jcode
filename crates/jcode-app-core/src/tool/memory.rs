//! Memory tool for storing and recalling information across sessions

use super::{Tool, ToolContext, ToolOutput};
use crate::memory::{MemoryCategory, MemoryEntry, MemoryManager, MemoryScope};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct MemoryTool {
    manager: MemoryManager,
}

impl MemoryTool {
    pub fn new() -> Self {
        Self {
            manager: MemoryManager::new(),
        }
    }

    /// Create a memory tool in test mode (isolated storage)
    pub fn new_test() -> Self {
        Self {
            manager: MemoryManager::new_test(),
        }
    }

    fn parse_scope(scope: Option<&str>, default: MemoryScope) -> Result<MemoryScope> {
        match scope.unwrap_or(match default {
            MemoryScope::Project => "project",
            MemoryScope::Global => "global",
            MemoryScope::All => "all",
        }) {
            "project" => Ok(MemoryScope::Project),
            "global" => Ok(MemoryScope::Global),
            "all" => Ok(MemoryScope::All),
            other => Err(anyhow::anyhow!(
                "Unknown scope: {}. Use project, global, or all",
                other
            )),
        }
    }

    /// Scope the manager to the per-call working directory so project-scoped
    /// memories resolve to the right `projects/<hash>.json` store. The base
    /// manager is built once in `new()` with `project_dir: None`, which made
    /// project writes silently no-op and reads come back empty (issue #491).
    ///
    /// An explicit `project_dir` wins over the session's working directory.
    /// Sessions with no working directory at all (ambient cycles) would
    /// otherwise be unable to touch project memory for any project: their
    /// project reads come back empty and their project writes are dropped, so
    /// they can see per-project graphs listed in their prompt but never
    /// maintain them.
    fn scoped_manager(&self, ctx: &ToolContext, project_dir: Option<&str>) -> MemoryManager {
        let explicit = project_dir
            .map(str::trim)
            .filter(|dir| !dir.is_empty())
            .map(std::path::PathBuf::from);
        if let Some(dir) = explicit {
            return self.manager.clone().with_project_dir(dir);
        }
        match ctx.working_dir.as_deref() {
            Some(dir) if !dir.as_os_str().is_empty() => self.manager.clone().with_project_dir(dir),
            _ => self.manager.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct MemoryInput {
    action: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    scope: Option<String>,
    /// For link action: source memory ID
    #[serde(default)]
    from_id: Option<String>,
    /// For link action: target memory ID
    #[serde(default)]
    to_id: Option<String>,
    /// For link action: relationship weight (0.0-1.0)
    #[serde(default)]
    weight: Option<f32>,
    /// For related action: traversal depth (default: 2)
    #[serde(default)]
    depth: Option<usize>,
    /// For recall action: max results (default: 10)
    #[serde(default)]
    limit: Option<usize>,
    /// For recall action: retrieval mode
    #[serde(default)]
    mode: Option<String>,
    /// Target a specific project's memory store by path, instead of the
    /// session's working directory.
    #[serde(default)]
    project_dir: Option<String>,
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Manage memory."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["remember", "recall", "search", "list", "forget", "tag", "link", "related"],
                    "description": "Action."
                },
                "content": { "type": "string" },
                "category": {
                    "type": "string",
                    "enum": ["fact", "preference", "entity", "correction"]
                },
                "query": { "type": "string" },
                "id": { "type": "string" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "scope": { "type": "string", "enum": ["project", "global", "all"] },
                "project_dir": {
                    "type": "string",
                    "description": "Project path for project scope. Defaults to the session working directory; required when there is none."
                },
                "from_id": { "type": "string" },
                "to_id": { "type": "string" },
                "limit": { "type": "integer", "description": "Max results." }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        use crate::memory;
        use crate::memory_types::{MemoryEventKind, MemoryState};

        let input: MemoryInput = serde_json::from_value(input)?;
        let action_label = input.action.clone();
        let session_id = ctx.session_id.clone();
        let manager = self.scoped_manager(&ctx, input.project_dir.as_deref());

        match input.action.as_str() {
            "remember" => {
                let content = input
                    .content
                    .ok_or_else(|| anyhow::anyhow!("content required"))?;
                let category: MemoryCategory = input
                    .category
                    .as_deref()
                    .unwrap_or("fact")
                    .parse()
                    .map_err(|err| anyhow::anyhow!("invalid memory category: {}", err))?;
                let scope = input.scope.as_deref().unwrap_or("project");
                memory::set_state(MemoryState::ToolAction {
                    action: "remember".into(),
                    detail: truncate_for_widget(&content, 40),
                });
                let mut entry =
                    MemoryEntry::new(category.clone(), &content).with_source(ctx.session_id);
                if let Some(tags) = input.tags {
                    entry = entry.with_tags(tags);
                }
                let id = if scope == "global" {
                    manager.remember_global(entry)?
                } else {
                    manager.remember_project(entry)?
                };
                // The agent just wrote this memory itself; the content is in
                // the transcript (tool call + result), so auto-recall should
                // not inject it back into this session.
                memory::mark_memories_known(
                    &session_id,
                    std::slice::from_ref(&id),
                    "stored via memory tool in this session",
                );
                memory::add_event(MemoryEventKind::ToolRemembered {
                    content: truncate_for_widget(&content, 60),
                    scope: scope.to_string(),
                    category: category.to_string(),
                });
                memory::set_state(MemoryState::Idle);
                Ok(ToolOutput::new(format!(
                    "Remembered {} ({}): \"{}\" [id: {}]",
                    category, scope, content, id
                )))
            }
            "recall" => {
                let limit = input.limit.unwrap_or(10);
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                let mode = input.mode.as_deref().unwrap_or_else(|| {
                    if input.query.is_some() {
                        "cascade"
                    } else {
                        "recent"
                    }
                });

                match mode {
                    "recent" => {
                        memory::set_state(MemoryState::ToolAction {
                            action: "recall".into(),
                            detail: "recent".into(),
                        });
                        let result = match manager.get_prompt_memories_scoped(limit, scope) {
                            Some(memories) => {
                                let count =
                                    memories.lines().filter(|l| l.starts_with("- ")).count();
                                memory::add_event(MemoryEventKind::ToolRecalled {
                                    query: "(recent)".into(),
                                    count,
                                });
                                Ok(ToolOutput::new(format!("Recent memories:\n{}", memories)))
                            }
                            None => {
                                memory::add_event(MemoryEventKind::ToolRecalled {
                                    query: "(recent)".into(),
                                    count: 0,
                                });
                                Ok(ToolOutput::new("No memories stored yet."))
                            }
                        };
                        memory::set_state(MemoryState::Idle);
                        result
                    }
                    "semantic" | "cascade" => {
                        let query = match &input.query {
                            Some(q) => q.clone(),
                            None => {
                                return Err(anyhow::anyhow!(
                                    "query required for semantic/cascade mode"
                                ));
                            }
                        };
                        memory::set_state(MemoryState::ToolAction {
                            action: "recall".into(),
                            detail: truncate_for_widget(&query, 40),
                        });

                        let results = if mode == "cascade" {
                            manager
                                .find_similar_with_cascade_scoped(&query, 0.5, limit, scope)?
                        } else {
                            manager
                                .find_similar_scoped(&query, 0.5, limit, scope)?
                        };

                        memory::add_event(MemoryEventKind::ToolRecalled {
                            query: truncate_for_widget(&query, 40),
                            count: results.len(),
                        });
                        memory::set_state(MemoryState::Idle);

                        if results.is_empty() {
                            Ok(ToolOutput::new(format!(
                                "No memories found matching '{}'. Try recall without query to see recent memories.",
                                query
                            )))
                        } else {
                            let mut out = format!(
                                "Found {} relevant memories for '{}':\n\n",
                                results.len(),
                                query
                            );
                            for (entry, score) in results {
                                let tags_str = if entry.tags.is_empty() {
                                    String::new()
                                } else {
                                    format!(" [{}]", entry.tags.join(", "))
                                };
                                out.push_str(&format!(
                                    "- [{}] {}{}\n  id: {} (relevance: {:.0}%)\n\n",
                                    entry.category,
                                    entry.content,
                                    tags_str,
                                    entry.id,
                                    score * 100.0
                                ));
                            }
                            Ok(ToolOutput::new(out))
                        }
                    }
                    other => Err(anyhow::anyhow!(
                        "Unknown mode: {}. Use recent, semantic, or cascade",
                        other
                    )),
                }
            }
            "search" => {
                let query = input
                    .query
                    .ok_or_else(|| anyhow::anyhow!("query required"))?;
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                memory::set_state(MemoryState::ToolAction {
                    action: "search".into(),
                    detail: truncate_for_widget(&query, 40),
                });
                let results = manager.search_scoped(&query, scope)?;
                memory::add_event(MemoryEventKind::ToolRecalled {
                    query: truncate_for_widget(&query, 40),
                    count: results.len(),
                });
                memory::set_state(MemoryState::Idle);
                if results.is_empty() {
                    Ok(ToolOutput::new(format!("No memories matching '{}'", query)))
                } else {
                    let mut out = format!("Found {} memories:\n\n", results.len());
                    for e in results {
                        out.push_str(&format!(
                            "- [{}] {}\n  id: {}\n\n",
                            e.category, e.content, e.id
                        ));
                    }
                    Ok(ToolOutput::new(out))
                }
            }
            "list" => {
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                memory::set_state(MemoryState::ToolAction {
                    action: "list".into(),
                    detail: String::new(),
                });
                let all = manager.list_all_scoped(scope)?;
                memory::add_event(MemoryEventKind::ToolListed { count: all.len() });
                memory::set_state(MemoryState::Idle);
                if all.is_empty() {
                    Ok(ToolOutput::new("No memories stored."))
                } else {
                    let mut out = format!("All memories ({}):\n\n", all.len());
                    for e in all {
                        out.push_str(&format!(
                            "- [{}] {}\n  id: {}\n\n",
                            e.category, e.content, e.id
                        ));
                    }
                    Ok(ToolOutput::new(out))
                }
            }
            "forget" => {
                let id = input.id.ok_or_else(|| anyhow::anyhow!("id required"))?;
                memory::set_state(MemoryState::ToolAction {
                    action: "forget".into(),
                    detail: truncate_for_widget(&id, 30),
                });
                let found = manager.forget(&id)?;
                memory::add_event(MemoryEventKind::ToolForgot { id: id.clone() });
                memory::set_state(MemoryState::Idle);
                if found {
                    Ok(ToolOutput::new(format!("Forgot: {}", id)))
                } else {
                    Ok(ToolOutput::new(format!("Not found: {}", id)))
                }
            }
            "tag" => {
                let id = input.id.ok_or_else(|| anyhow::anyhow!("id required"))?;
                let tags = input.tags.ok_or_else(|| anyhow::anyhow!("tags required"))?;

                if tags.is_empty() {
                    return Err(anyhow::anyhow!("At least one tag required"));
                }

                memory::set_state(MemoryState::ToolAction {
                    action: "tag".into(),
                    detail: format!("{} +{}", truncate_for_widget(&id, 20), tags.join(",")),
                });
                for tag in &tags {
                    manager.tag_memory(&id, tag)?;
                }
                let tags_str = tags.join(", ");
                memory::add_event(MemoryEventKind::ToolTagged {
                    id: id.clone(),
                    tags: tags_str.clone(),
                });
                memory::set_state(MemoryState::Idle);

                Ok(ToolOutput::new(format!(
                    "Tagged memory {} with: {}",
                    id, tags_str
                )))
            }
            "link" => {
                let from_id = input
                    .from_id
                    .ok_or_else(|| anyhow::anyhow!("from_id required"))?;
                let to_id = input
                    .to_id
                    .ok_or_else(|| anyhow::anyhow!("to_id required"))?;
                let weight = input.weight.unwrap_or(0.5);

                memory::set_state(MemoryState::ToolAction {
                    action: "link".into(),
                    detail: format!(
                        "{} -> {}",
                        truncate_for_widget(&from_id, 15),
                        truncate_for_widget(&to_id, 15)
                    ),
                });
                manager.link_memories(&from_id, &to_id, weight)?;
                memory::add_event(MemoryEventKind::ToolLinked {
                    from: from_id.clone(),
                    to: to_id.clone(),
                });
                memory::set_state(MemoryState::Idle);
                Ok(ToolOutput::new(format!(
                    "Linked memories {} -> {} (weight {:.2})",
                    from_id, to_id, weight
                )))
            }
            "related" => {
                let id = input.id.ok_or_else(|| anyhow::anyhow!("id required"))?;
                let depth = input.depth.unwrap_or(2);

                memory::set_state(MemoryState::ToolAction {
                    action: "related".into(),
                    detail: truncate_for_widget(&id, 30),
                });
                let related = manager.get_related(&id, depth)?;
                memory::add_event(MemoryEventKind::ToolRecalled {
                    query: format!("related:{}", truncate_for_widget(&id, 20)),
                    count: related.len(),
                });
                memory::set_state(MemoryState::Idle);

                if related.is_empty() {
                    Ok(ToolOutput::new(format!(
                        "No related memories found for {}",
                        id
                    )))
                } else {
                    let mut out = format!(
                        "Found {} memories related to {} (depth {}):\n\n",
                        related.len(),
                        id,
                        depth
                    );
                    for e in related {
                        out.push_str(&format!(
                            "- [{}] {}\n  id: {}\n\n",
                            e.category, e.content, e.id
                        ));
                    }
                    Ok(ToolOutput::new(out))
                }
            }
            other => Err(anyhow::anyhow!("Unknown action: {}", other)),
        }
        .map_err(|err| {
            crate::logging::warn(&format!(
                "[tool:memory] action failed action={} session_id={} error={}",
                action_label, session_id, err
            ));
            err
        })
    }
}

fn truncate_for_widget(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let truncated: String = s.chars().take(max).collect();
        format!("{}…", truncated)
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    // Holding the process-wide test-env lock across awaits is deliberate: the
    // env mutation must stay exclusive for the whole test.
    #![allow(clippy::await_holding_lock)]
    use super::*;

    #[test]
    fn schema_only_advertises_core_memory_fields() {
        let schema = MemoryTool::new().parameters_schema();
        let props = schema["properties"]
            .as_object()
            .expect("memory schema should have properties");

        assert!(props.contains_key("action"));
        assert!(props.contains_key("content"));
        assert!(props.contains_key("category"));
        assert!(props.contains_key("query"));
        assert!(props.contains_key("id"));
        assert!(props.contains_key("tags"));
        assert!(props.contains_key("scope"));
        assert!(props.contains_key("from_id"));
        assert!(props.contains_key("to_id"));
        assert!(props.contains_key("limit"));
        assert!(!props.contains_key("weight"));
        assert!(!props.contains_key("depth"));
        assert!(!props.contains_key("mode"));
    }

    fn test_ctx(working_dir: Option<std::path::PathBuf>) -> ToolContext {
        ToolContext {
            session_id: "test-session".to_string(),
            message_id: "test-message".to_string(),
            tool_call_id: "test-tool-call".to_string(),
            working_dir,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        }
    }

    /// The ambient agent has no working directory, so without an explicit
    /// target its project-scoped reads come back empty and its project-scoped
    /// writes are silently dropped. It can therefore see every project graph
    /// listed in its prompt but never garden one. Naming the project must let a
    /// working-dir-less session read and write that project's store, and must
    /// not leak into a different project.
    #[tokio::test]
    async fn a_session_without_a_working_directory_can_garden_a_named_project() {
        let _guard = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let other = tempfile::tempdir().expect("other project");
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());

        let tool = MemoryTool::new();
        let no_working_dir = || test_ctx(None);

        // Without naming a project the write goes nowhere: that is the bug.
        tool.execute(
            json!({
                "action": "remember",
                "content": "unaddressed-project-note",
                "scope": "project"
            }),
            no_working_dir(),
        )
        .await
        .expect("remember should not error");

        // Naming the project makes the same write land.
        tool.execute(
            json!({
                "action": "remember",
                "content": "ambient-gardened-note",
                "scope": "project",
                "project_dir": project.path().to_string_lossy(),
            }),
            no_working_dir(),
        )
        .await
        .expect("targeted remember should succeed");

        let listed = tool
            .execute(
                json!({
                    "action": "list",
                    "scope": "project",
                    "project_dir": project.path().to_string_lossy(),
                }),
                no_working_dir(),
            )
            .await
            .expect("targeted list should succeed");
        assert!(
            listed.output.contains("ambient-gardened-note"),
            "a named project's memory must be readable without a working dir, got: {}",
            listed.output
        );
        assert!(
            !listed.output.contains("unaddressed-project-note"),
            "an unaddressed write must not silently land in this project"
        );

        // And it must not bleed into a different project's store.
        let other_listed = tool
            .execute(
                json!({
                    "action": "list",
                    "scope": "project",
                    "project_dir": other.path().to_string_lossy(),
                }),
                no_working_dir(),
            )
            .await
            .expect("other list should succeed");
        assert!(
            !other_listed.output.contains("ambient-gardened-note"),
            "memory must stay in the project it was addressed to, got: {}",
            other_listed.output
        );

        // Gardening means removing too, not only adding.
        let forgotten = tool
            .execute(
                json!({
                    "action": "search",
                    "query": "ambient-gardened-note",
                    "scope": "project",
                    "project_dir": project.path().to_string_lossy(),
                }),
                no_working_dir(),
            )
            .await
            .expect("targeted search should succeed");
        assert!(
            forgotten.output.contains("ambient-gardened-note"),
            "search must reach the named project too, got: {}",
            forgotten.output
        );

        if let Some(prev_home) = prev_home {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    /// Issue #491 regression: project-scoped remember followed by list must
    /// round-trip through the real (non-test-mode) manager when the tool
    /// context carries a working dir.
    #[tokio::test]
    async fn project_scope_round_trips_with_working_dir() {
        let _guard = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());

        let tool = MemoryTool::new();
        let remember = tool
            .execute(
                json!({
                    "action": "remember",
                    "content": "issue-491-probe",
                    "scope": "project"
                }),
                test_ctx(Some(project.path().to_path_buf())),
            )
            .await
            .expect("remember should succeed");
        assert!(remember.output.contains("issue-491-probe"));

        let list = tool
            .execute(
                json!({ "action": "list", "scope": "project" }),
                test_ctx(Some(project.path().to_path_buf())),
            )
            .await
            .expect("list should succeed");
        assert!(
            list.output.contains("issue-491-probe"),
            "project-scoped memory must persist and be listed, got: {}",
            list.output
        );

        if let Some(prev_home) = prev_home {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    /// A path that is not a directory is not a project. Registering one would
    /// let a single typo pin a bogus entry in the registry permanently, and the
    /// registry is what names graphs in ambient's per-project report.
    #[tokio::test]
    async fn reading_a_nonexistent_project_does_not_pollute_the_registry() {
        let _guard = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());

        let tool = MemoryTool::new();
        let listed = tool
            .execute(
                json!({
                    "action": "list",
                    "scope": "project",
                    "project_dir": "/nonexistent/path/for/registry/probe",
                }),
                test_ctx(None),
            )
            .await
            .expect("listing a missing project must not error");
        assert!(
            listed.output.contains("No memories"),
            "a missing project must read as empty, got: {}",
            listed.output
        );

        let registry = MemoryManager::load_projects_registry();
        assert!(
            !registry
                .values()
                .any(|dir| dir.contains("/nonexistent/path/for/registry/probe")),
            "a nonexistent path must never be registered, got: {registry:?}"
        );

        if let Some(prev_home) = prev_home {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    /// Reading a project's memory must register its path too. The registry is
    /// the only way something outside a project (ambient) can name a hash-named
    /// graph, and a project the user only reads from would otherwise stay an
    /// unreadable hash in ambient's report forever.
    #[tokio::test]
    async fn reading_a_project_registers_its_path_for_naming() {
        let _guard = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());

        // A read-only interaction: list, never remember.
        let tool = MemoryTool::new();
        tool.execute(
            json!({
                "action": "list",
                "scope": "project",
                "project_dir": project.path().to_string_lossy(),
            }),
            test_ctx(None),
        )
        .await
        .expect("list should succeed");

        let graph_id = MemoryManager::new()
            .with_project_dir(project.path())
            .project_graph_path()
            .expect("path")
            .expect("project dir set")
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("stem")
            .to_string();

        assert_eq!(
            MemoryManager::load_projects_registry()
                .get(&graph_id)
                .map(String::as_str),
            Some(project.path().to_string_lossy().as_ref()),
            "a read must be enough to name the project later"
        );

        if let Some(prev_home) = prev_home {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    /// Project graphs are named by a path hash, so anything surveying them from
    /// outside a project (the ambient agent) can only name them if writing a
    /// project memory also records the reverse mapping.
    #[tokio::test]
    async fn project_memory_write_records_the_project_path_for_later_lookup() {
        let _guard = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());

        let tool = MemoryTool::new();
        tool.execute(
            json!({
                "action": "remember",
                "content": "registry-probe",
                "scope": "project"
            }),
            test_ctx(Some(project.path().to_path_buf())),
        )
        .await
        .expect("remember should succeed");

        let manager = MemoryManager::new().with_project_dir(project.path());
        let graph_id = manager
            .project_graph_path()
            .expect("path")
            .expect("project dir set")
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("stem")
            .to_string();

        let registry = MemoryManager::load_projects_registry();
        assert_eq!(
            registry.get(&graph_id).map(String::as_str),
            Some(project.path().to_string_lossy().as_ref()),
            "the project path must be recoverable from the graph id, got: {:?}",
            registry
        );

        if let Some(prev_home) = prev_home {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    /// Issue #729 regression, behavioral rather than structural.
    ///
    /// `create_headless_session` used to call `enable_memory_test_mode()`
    /// unconditionally, so real swarm-spawned workers got throwaway storage and
    /// could never read what the session that spawned them remembered. The fix
    /// makes isolation an explicit per-caller choice, but the property that
    /// actually matters to a user is this: with the same working directory, a
    /// default registry's memory tool sees what was written, and a test-mode
    /// one does not.
    ///
    /// Driving `Tool::execute` (rather than inspecting a flag) means this stays
    /// honest even if the internals are refactored.
    #[tokio::test]
    async fn swarm_worker_memory_sees_the_spawning_session_only_without_isolation() {
        let _guard = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());

        // The session that spawns a worker records something project-scoped.
        let spawner = MemoryTool::new();
        spawner
            .execute(
                json!({
                    "action": "remember",
                    "content": "issue-729-spawner-note",
                    "scope": "project"
                }),
                test_ctx(Some(project.path().to_path_buf())),
            )
            .await
            .expect("spawner remember should succeed");

        // A worker that kept real memory (the fixed path) must see it.
        let worker = MemoryTool::new();
        let seen = worker
            .execute(
                json!({ "action": "list", "scope": "project" }),
                test_ctx(Some(project.path().to_path_buf())),
            )
            .await
            .expect("worker list should succeed");
        assert!(
            seen.output.contains("issue-729-spawner-note"),
            "a swarm worker must see the spawning session's project memory, got: {}",
            seen.output
        );

        // A worker forced into test mode (the pre-fix path) cannot, no matter
        // that it has the identical working directory. This is the defect.
        let isolated = MemoryTool::new_test();
        let blind = isolated
            .execute(
                json!({ "action": "list", "scope": "project" }),
                test_ctx(Some(project.path().to_path_buf())),
            )
            .await
            .expect("isolated list should succeed");
        assert!(
            !blind.output.contains("issue-729-spawner-note"),
            "test mode unexpectedly saw real project memory, so this test cannot \
             distinguish the two paths: {}",
            blind.output
        );

        if let Some(prev_home) = prev_home {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }
}
