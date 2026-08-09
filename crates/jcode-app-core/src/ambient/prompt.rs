use chrono::{DateTime, Utc};

use super::{AmbientState, Priority, ScheduleTarget, ScheduledItem, take_pending_directives};

// ---------------------------------------------------------------------------
// Ambient System Prompt Builder
// ---------------------------------------------------------------------------

/// Health stats for the memory graph, used in the ambient system prompt.
#[derive(Debug, Clone, Default)]
pub struct MemoryGraphHealth {
    pub total: usize,
    pub active: usize,
    pub inactive: usize,
    pub low_confidence: usize,
    pub contradictions: usize,
    pub missing_embeddings: usize,
    pub duplicate_candidates: usize,
    pub last_consolidation: Option<DateTime<Utc>>,
    /// Per-project graph breakdown, so the ambient agent can see that project
    /// memory exists at all and which project owns which memories.
    pub projects: Vec<ProjectGraphHealth>,
}

/// Health of a single per-project memory graph.
#[derive(Debug, Clone, Default)]
pub struct ProjectGraphHealth {
    /// Project working directory, when it could be resolved from sessions.
    pub working_dir: Option<String>,
    /// Graph file stem (the project path hash), always available.
    pub graph_id: String,
    pub total: usize,
    pub active: usize,
    pub low_confidence: usize,
    pub missing_embeddings: usize,
}

/// Summary of a recent session for the ambient prompt.
#[derive(Debug, Clone)]
pub struct RecentSessionInfo {
    pub id: String,
    pub status: String,
    pub topic: Option<String>,
    pub duration_secs: i64,
    pub extraction_status: String,
    /// Project this session ran in, when the session recorded one.
    pub working_dir: Option<String>,
}

/// Resource budget info for the ambient prompt.
#[derive(Debug, Clone, Default)]
pub struct ResourceBudget {
    pub provider: String,
    pub tokens_remaining_desc: String,
    pub window_resets_desc: String,
    pub user_usage_rate_desc: String,
    pub cycle_budget_desc: String,
}

/// Gather memory graph health stats from the MemoryManager.
///
/// The ambient agent has no working directory of its own, so
/// `memory_manager.load_project_graph()` would report an empty project graph
/// and hide every per-project memory the user has. Survey the project graph
/// directory directly so ambient can see and garden project memory too.
pub fn gather_memory_graph_health(
    memory_manager: &crate::memory::MemoryManager,
) -> MemoryGraphHealth {
    let mut health = MemoryGraphHealth::default();

    // Accumulate stats from project + global graphs
    for graph in [
        memory_manager.load_project_graph(),
        memory_manager.load_global_graph(),
    ]
    .into_iter()
    .flatten()
    {
        let active_count = graph.memories.values().filter(|m| m.active).count();
        let inactive_count = graph.memories.values().filter(|m| !m.active).count();
        health.total += graph.memories.len();
        health.active += active_count;
        health.inactive += inactive_count;

        // Low confidence: effective confidence < 0.1
        health.low_confidence += graph
            .memories
            .values()
            .filter(|m| m.active && m.effective_confidence() < 0.1)
            .count();

        // Missing embeddings
        health.missing_embeddings += graph
            .memories
            .values()
            .filter(|m| m.active && m.embedding.is_none())
            .count();

        // Count contradiction edges
        for edges in graph.edges.values() {
            for edge in edges {
                if matches!(edge.kind, crate::memory_graph::EdgeKind::Contradicts) {
                    health.contradictions += 1;
                }
            }
        }

        // Use last_cluster_update as a proxy for last consolidation
        if let Some(ts) = graph.metadata.last_cluster_update {
            match health.last_consolidation {
                Some(existing) if ts > existing => health.last_consolidation = Some(ts),
                None => health.last_consolidation = Some(ts),
                _ => {}
            }
        }
    }

    // Contradicts edges are bidirectional, so divide by 2
    health.contradictions /= 2;

    // Duplicate candidates would require embedding similarity scan;
    // placeholder for now — ambient agent will discover them during its cycle.
    health.duplicate_candidates = 0;

    // Fold in every per-project graph the manager itself cannot reach.
    let own_project_graph = memory_manager
        .project_graph_path()
        .ok()
        .flatten()
        .and_then(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(ToOwned::to_owned)
        });
    for project in gather_project_graph_health() {
        // Don't double count the project the manager already loaded above.
        if own_project_graph.as_deref() == Some(project.graph_id.as_str()) {
            health.projects.push(project);
            continue;
        }
        health.total += project.total;
        health.active += project.active;
        health.inactive += project.total.saturating_sub(project.active);
        health.low_confidence += project.low_confidence;
        health.missing_embeddings += project.missing_embeddings;
        health.projects.push(project);
    }
    health
        .projects
        .sort_by(|a, b| b.total.cmp(&a.total).then(a.graph_id.cmp(&b.graph_id)));

    health
}

/// Survey every per-project memory graph under `~/.jcode/memory/projects/`.
pub fn gather_project_graph_health() -> Vec<ProjectGraphHealth> {
    let Ok(dir) = crate::memory::MemoryManager::projects_memory_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let registry = crate::memory::MemoryManager::load_projects_registry();
    let mut out = Vec::new();
    let mut unnamed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e != "json").unwrap_or(true) {
            continue;
        }
        let Some(graph_id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // The reverse-mapping registry itself is not a project graph.
        if graph_id == "index" {
            continue;
        }
        let Ok(graph) = crate::storage::read_json::<crate::memory_graph::MemoryGraph>(&path) else {
            continue;
        };
        let working_dir = registry.get(graph_id).cloned();
        if working_dir.is_none() {
            unnamed.push(graph_id.to_string());
        }
        out.push(ProjectGraphHealth {
            working_dir,
            graph_id: graph_id.to_string(),
            total: graph.memories.len(),
            active: graph.memories.values().filter(|m| m.active).count(),
            low_confidence: graph
                .memories
                .values()
                .filter(|m| m.active && m.effective_confidence() < 0.1)
                .count(),
            missing_embeddings: graph
                .memories
                .values()
                .filter(|m| m.active && m.embedding.is_none())
                .count(),
        });
    }

    // Graphs written before the registry existed have no recorded path. Recover
    // their names from session history rather than showing the user a hash.
    if !unnamed.is_empty() {
        let recovered = project_dirs_from_session_history(&unnamed);
        for project in out.iter_mut() {
            if project.working_dir.is_none() {
                project.working_dir = recovered.get(&project.graph_id).cloned();
            }
        }
    }
    out
}

/// Recover graph-id -> project-dir pairs for the given ids from session files.
///
/// Deserializes only the working directory field, so this stays cheap even
/// though the sessions directory can hold tens of thousands of transcripts.
/// Stops as soon as every requested id is named.
fn project_dirs_from_session_history(
    wanted: &[String],
) -> std::collections::HashMap<String, String> {
    #[derive(serde::Deserialize)]
    struct WorkingDirOnly {
        #[serde(default)]
        working_dir: Option<String>,
    }

    let mut found = std::collections::HashMap::new();
    let wanted: std::collections::HashSet<&str> = wanted.iter().map(String::as_str).collect();
    let Ok(sessions_dir) = crate::storage::jcode_dir().map(|d| d.join("sessions")) else {
        return found;
    };
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        return found;
    };

    let mut candidates: Vec<(std::path::PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().map(|e| e != "json").unwrap_or(true) {
                return None;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            Some((path, modified))
        })
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.truncate(PROJECT_NAME_SESSION_SCAN_LIMIT);

    let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (path, _) in candidates {
        if found.len() == wanted.len() {
            break;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<WorkingDirOnly>(&text) else {
            continue;
        };
        let Some(dir) = parsed.working_dir else {
            continue;
        };
        if !seen_dirs.insert(dir.clone()) {
            continue;
        }
        if let Some(id) = project_graph_id_for_dir(&dir)
            && wanted.contains(id.as_str())
        {
            found.entry(id).or_insert(dir);
        }
    }
    found
}

/// Session files scanned when naming otherwise-anonymous project graphs.
const PROJECT_NAME_SESSION_SCAN_LIMIT: usize = 2000;

/// Resolve the project graph id for a working directory.
fn project_graph_id_for_dir(dir: &str) -> Option<String> {
    crate::memory::MemoryManager::new()
        .with_project_dir(dir)
        .project_graph_path()
        .ok()
        .flatten()
        .and_then(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(ToOwned::to_owned)
        })
}

/// Gather feedback memories relevant to ambient mode.
///
/// Pulls from two sources:
/// 1. Recent ambient transcripts (summaries of past cycles)
/// 2. Memory graph entries tagged "ambient" or "system"
///
/// Returns formatted strings for inclusion in the ambient system prompt.
pub fn gather_feedback_memories(memory_manager: &crate::memory::MemoryManager) -> Vec<String> {
    let mut feedback = Vec::new();

    // --- Source 1: Recent ambient transcripts ---
    let transcripts_dir = match crate::storage::jcode_dir() {
        Ok(d) => d.join("ambient").join("transcripts"),
        Err(_) => return feedback,
    };

    if transcripts_dir.exists()
        && let Ok(dir) = std::fs::read_dir(&transcripts_dir)
    {
        let mut files: Vec<_> = dir.flatten().collect();
        // Sort by filename descending (most recent first)
        files.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
        // Only look at the last 5 transcripts
        files.truncate(5);

        for entry in files {
            if let Ok(content) = std::fs::read_to_string(entry.path())
                && let Ok(transcript) =
                    serde_json::from_str::<crate::safety::AmbientTranscript>(&content)
            {
                let status = format!("{:?}", transcript.status);
                let summary = transcript.summary.as_deref().unwrap_or("no summary");
                let age = format_duration_rough(Utc::now() - transcript.started_at);
                feedback.push(format!(
                    "Past cycle ({} ago, {}): {} memories modified, {} compactions — {}",
                    age,
                    status.to_lowercase(),
                    transcript.memories_modified,
                    transcript.compactions,
                    summary,
                ));
            }
        }
    }

    // --- Source 2: Memory graph entries tagged "ambient" or "system" ---
    for graph in [
        memory_manager.load_project_graph(),
        memory_manager.load_global_graph(),
    ]
    .into_iter()
    .flatten()
    {
        for memory in graph.memories.values() {
            if !memory.active {
                continue;
            }
            let has_ambient_tag = memory.tags.iter().any(|t| t == "ambient" || t == "system");
            if has_ambient_tag {
                feedback.push(format!("Memory [{}]: {}", memory.id, memory.content));
            }
        }
    }

    feedback
}

/// Gather recent sessions since a given timestamp.
pub fn gather_recent_sessions(since: Option<DateTime<Utc>>) -> Vec<RecentSessionInfo> {
    let sessions_dir = match crate::storage::jcode_dir() {
        Ok(d) => d.join("sessions"),
        Err(_) => return Vec::new(),
    };
    if !sessions_dir.exists() {
        return Vec::new();
    }

    let cutoff = since.unwrap_or_else(|| Utc::now() - chrono::Duration::hours(24));

    // Pre-filter candidate session files by filesystem mtime BEFORE loading and
    // parsing them. The sessions directory can hold tens of thousands of files;
    // fully parsing every one via Session::load just to drop those older than
    // the cutoff is O(all_sessions * parse). A session updated after the cutoff
    // has a recent mtime, so we keep only files whose mtime is at or after the
    // cutoff (minus a small margin for clock/write skew), then load newest-first
    // and stop once we have enough recent sessions.
    const RECENT_SESSION_LIMIT: usize = 20;
    let mtime_cutoff = cutoff - chrono::Duration::hours(1);

    let mut candidates: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.extension().map(|e| e == "json").unwrap_or(false) {
                continue;
            }
            let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
                // If we can't read mtime, keep the file as a candidate so we
                // don't silently drop a possibly-recent session.
                candidates.push((path, std::time::SystemTime::UNIX_EPOCH));
                continue;
            };
            let modified_dt: DateTime<Utc> = modified.into();
            if modified_dt < mtime_cutoff {
                continue;
            }
            candidates.push((path, modified));
        }
    }
    // Newest files first so we can stop early once we have enough.
    candidates.sort_by(|a, b| b.1.cmp(&a.1));

    let mut recent = Vec::new();
    // Load somewhat more than the final limit by mtime so the subsequent
    // id-based sort/truncate picks the true most-recent set even when file
    // mtime order and id (timestamp) order disagree near the boundary, while
    // still bounding work far below "load every session file".
    let load_budget = RECENT_SESSION_LIMIT
        .saturating_mul(4)
        .max(RECENT_SESSION_LIMIT);
    let mut loaded = 0usize;
    for (path, _modified) in candidates {
        if loaded >= load_budget {
            break;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && let Ok(session) = crate::session::Session::load(stem)
        {
            loaded += 1;
            // Skip debug sessions
            if session.is_debug {
                continue;
            }
            // Only include sessions updated after cutoff
            if session.updated_at < cutoff {
                continue;
            }
            let duration = (session.updated_at - session.created_at)
                .num_seconds()
                .max(0);
            let extraction = if session.messages.is_empty() {
                "no messages"
            } else {
                // Heuristic: if session closed normally, assume extracted
                match &session.status {
                    crate::session::SessionStatus::Closed => "extracted",
                    crate::session::SessionStatus::Crashed { .. } => "missed",
                    crate::session::SessionStatus::Active => "in progress",
                    _ => "unknown",
                }
            };
            recent.push(RecentSessionInfo {
                id: session.id.clone(),
                status: session.status.display().to_string(),
                topic: session.display_title().map(ToOwned::to_owned),
                duration_secs: duration,
                extraction_status: extraction.to_string(),
                working_dir: session.working_dir.clone(),
            });
        }
    }

    // Sort by most recent first (id embeds a timestamp).
    recent.sort_by(|a, b| b.id.cmp(&a.id));
    recent.truncate(RECENT_SESSION_LIMIT);
    recent
}

/// Build the dynamic system prompt for an ambient cycle.
///
/// Populates the template from AMBIENT_MODE.md with real data from the
/// current state, queue, memory graph, sessions, and resource budget.
pub fn build_ambient_system_prompt(
    state: &AmbientState,
    queue: &[ScheduledItem],
    graph_health: &MemoryGraphHealth,
    recent_sessions: &[RecentSessionInfo],
    feedback_memories: &[String],
    budget: &ResourceBudget,
    active_user_sessions: usize,
) -> String {
    let mut prompt = String::with_capacity(4096);

    prompt.push_str(
        "You are the ambient agent for jcode. You operate autonomously without \
         user prompting. Your job is to maintain and improve the user's \
         development environment.\n\n",
    );

    // --- Current State ---
    prompt.push_str("## Current State\n");
    if let Some(last_run) = state.last_run {
        let ago = Utc::now() - last_run;
        let ago_str = format_duration_rough(ago);
        prompt.push_str(&format!(
            "- Last ambient cycle: {} ({} ago)\n",
            last_run.format("%Y-%m-%d %H:%M UTC"),
            ago_str,
        ));
    } else {
        prompt.push_str("- Last ambient cycle: never (first run)\n");
    }
    if active_user_sessions > 0 {
        prompt.push_str(&format!(
            "- Active user sessions: {}\n",
            active_user_sessions
        ));
    } else {
        prompt.push_str("- Active user sessions: none\n");
    }
    prompt.push_str(&format!(
        "- Total cycles completed: {}\n",
        state.total_cycles
    ));
    prompt.push('\n');

    // --- Scheduled Queue ---
    prompt.push_str("## Scheduled Queue\n");
    if queue.is_empty() {
        prompt.push_str("Empty -- do general ambient work.\n");
    } else {
        for item in queue {
            let age = Utc::now() - item.created_at;
            let priority = match item.priority {
                Priority::Low => "low",
                Priority::Normal => "normal",
                Priority::High => "HIGH",
            };
            prompt.push_str(&format!(
                "- [{}] {} (scheduled {} ago, priority: {})\n",
                item.id,
                item.context,
                format_duration_rough(age),
                priority,
            ));
            match &item.target {
                ScheduleTarget::Ambient => {}
                ScheduleTarget::Session { session_id } => {
                    prompt.push_str(&format!("  Target session: {}\n", session_id));
                }
                ScheduleTarget::Spawn { parent_session_id } => {
                    prompt.push_str(&format!("  Spawn from session: {}\n", parent_session_id));
                }
            }
            if let Some(ref dir) = item.working_dir {
                prompt.push_str(&format!("  Working dir: {}\n", dir));
            }
            if let Some(ref desc) = item.task_description {
                prompt.push_str(&format!("  Details: {}\n", desc));
            }
            if !item.relevant_files.is_empty() {
                prompt.push_str(&format!("  Files: {}\n", item.relevant_files.join(", ")));
            }
            if let Some(ref branch) = item.git_branch {
                prompt.push_str(&format!("  Branch: {}\n", branch));
            }
            if let Some(ref ctx) = item.additional_context {
                for line in ctx.lines() {
                    prompt.push_str(&format!("  {}\n", line));
                }
            }
        }
    }
    prompt.push('\n');

    // --- Recent Sessions ---
    prompt.push_str("## Recent Sessions (since last cycle)\n");
    if recent_sessions.is_empty() {
        prompt.push_str("No sessions since last cycle.\n");
    } else {
        for s in recent_sessions {
            let topic = s.topic.as_deref().unwrap_or("(no title)");
            let dur = format_duration_rough(chrono::Duration::seconds(s.duration_secs));
            let project = s.working_dir.as_deref().unwrap_or("(no project)");
            prompt.push_str(&format!(
                "- {} | {} | {} | {} | project: {} | extraction: {}\n",
                s.id, s.status, dur, topic, project, s.extraction_status,
            ));
        }
    }
    prompt.push('\n');

    // --- Projects seen recently ---
    // Ambient has no working directory of its own, so without this it cannot
    // tell which repo any of the work above belongs to.
    let mut project_counts: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for s in recent_sessions {
        if let Some(dir) = s.working_dir.as_deref() {
            *project_counts.entry(dir).or_insert(0) += 1;
        }
    }
    if !project_counts.is_empty() {
        prompt.push_str("## Projects Active Recently\n");
        let mut ranked: Vec<_> = project_counts.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        for (dir, count) in ranked {
            prompt.push_str(&format!("- {} ({} session(s))\n", dir, count));
        }
        prompt.push_str(
            "To work in one of these projects, use its path as the working directory: \
             per-project memory, AGENTS.md, and git state only resolve when a \
             working directory is set.\n",
        );
        prompt.push('\n');
    }

    // --- Memory Graph Health ---
    prompt.push_str("## Memory Graph Health\n");
    prompt.push_str(&format!(
        "- Total memories: {} ({} active, {} inactive)\n",
        graph_health.total, graph_health.active, graph_health.inactive,
    ));
    prompt.push_str(&format!(
        "- Memories with confidence < 0.1: {}\n",
        graph_health.low_confidence,
    ));
    prompt.push_str(&format!(
        "- Unresolved contradictions: {}\n",
        graph_health.contradictions,
    ));
    prompt.push_str(&format!(
        "- Memories without embeddings: {}\n",
        graph_health.missing_embeddings,
    ));
    if graph_health.duplicate_candidates > 0 {
        prompt.push_str(&format!(
            "- Duplicate candidates (similarity > 0.95): {}\n",
            graph_health.duplicate_candidates,
        ));
    } else {
        prompt.push_str("- Duplicate candidates: run embedding scan to detect\n");
    }
    if let Some(ts) = graph_health.last_consolidation {
        let ago = format_duration_rough(Utc::now() - ts);
        prompt.push_str(&format!("- Last consolidation: {} ago\n", ago));
    } else {
        prompt.push_str("- Last consolidation: never\n");
    }
    if !graph_health.projects.is_empty() {
        prompt.push_str("- Per-project memory graphs:\n");
        for p in &graph_health.projects {
            let name = p.working_dir.as_deref().unwrap_or(p.graph_id.as_str());
            prompt.push_str(&format!(
                "  - {}: {} memories ({} active, {} low confidence, {} without embeddings)\n",
                name, p.total, p.active, p.low_confidence, p.missing_embeddings,
            ));
        }
        prompt.push_str(
            "  You have no working directory, so project-scoped memory actions \
             default to nothing. To read or garden one of these graphs, pass the \
             project path explicitly: memory(action=\"list\", scope=\"project\", \
             project_dir=\"<path above>\"), and likewise for remember/forget/tag. \
             Without project_dir a project-scoped write is silently dropped.\n",
        );
    }
    prompt.push('\n');

    // --- User Feedback History ---
    prompt.push_str("## User Feedback History\n");
    if feedback_memories.is_empty() {
        prompt.push_str("No feedback memories found about ambient mode yet.\n");
    } else {
        for mem in feedback_memories {
            prompt.push_str(&format!("- {}\n", mem));
        }
    }
    prompt.push('\n');

    // --- Resource Budget ---
    prompt.push_str("## Resource Budget\n");
    prompt.push_str(&format!("- Provider: {}\n", budget.provider));
    prompt.push_str(&format!(
        "- Tokens remaining in window: {}\n",
        budget.tokens_remaining_desc,
    ));
    prompt.push_str(&format!("- Window resets: {}\n", budget.window_resets_desc));
    prompt.push_str(&format!(
        "- User usage rate: {}\n",
        budget.user_usage_rate_desc,
    ));
    prompt.push_str(&format!(
        "- Budget for this cycle: {}\n",
        budget.cycle_budget_desc,
    ));
    prompt.push('\n');

    // --- User Directives (from email/Telegram replies) ---
    let pending_directives = take_pending_directives();
    if !pending_directives.is_empty() {
        prompt.push_str("## User Directives (from replies)\n");
        prompt.push_str(
            "The user replied to ambient notifications with these instructions. \
             Address them as your **top priority** this cycle.\n\n",
        );
        for dir in &pending_directives {
            let ago = format_duration_rough(Utc::now() - dir.received_at);
            prompt.push_str(&format!(
                "- [reply to cycle {}] ({} ago): {}\n",
                dir.in_reply_to_cycle, ago, dir.text,
            ));
        }
        prompt.push('\n');
    }

    // --- Instructions ---
    prompt.push_str(
        "## Instructions\n\n\
         Use the tools that are already available to you in this session. Do \
         not search for tools — there is no tool-search/discovery tool, and \
         the tools you need are listed below and in your tool definitions.\n\n\
         Key tools for this cycle (use these exact names):\n\
         - `todo` — plan and track what you'll do this cycle.\n\
         - `end_ambient_cycle` — REQUIRED to finish the cycle (see below).\n\
         - `schedule_ambient` — schedule your next wake time.\n\
         - `request_permission` — get approval before any code change.\n\
         - `send_message` — keep the user informed.\n\
         Standard tools (`bash`, `read`, `write`, `edit`, `memory`, etc.) are \
         also available.\n\n\
         Start by using the `todo` tool to plan what you'll do this cycle.\n\n\
         Priority order:\n\
         1. Execute any scheduled queue items first.\n\
         2. Garden the memory graph -- consolidate duplicates, resolve \
            contradictions, prune dead memories, verify stale facts, \
            extract from missed sessions.\n\
         3. Scout for proactive work, if enabled below -- look at recent \
            sessions and git history to identify useful work the user would \
            appreciate.\n\n\
         For gardening: focus on highest-value maintenance first. Duplicates \
         and contradictions before pruning. Verify stale facts only if you \
         have budget left.\n\n\
         For proactive work: be conservative. A bad surprise is worse than \
         no surprise. Check the user feedback memories -- if they've rejected \
         similar work before, don't do it. Code changes must go on a worktree \
         branch with a PR via request_permission.\n\n\
        Code work is NOT delivered until it is a pull request the user can \
        review. A pushed branch with no PR is invisible to them: they see no \
        work at all. So for every code change, finish the cycle by opening a \
        PR against the user's own fork, never upstream:\n\
        - branch from the CURRENT remote head, never from a stale local base\n\
        - `git push -u origin <branch>`\n\
        - `gh pr create --repo <fork> --base <default branch> --fill`, then \
        report the PR URL in your end_ambient_cycle summary\n\
        If `gh pr create` fails with a permissions error on a fork, the cause \
        is almost always that gh defaulted to the UPSTREAM repo: pass \
        `--repo <owner>/<repo>` explicitly for the fork rather than giving up \
        and leaving the branch unreviewed.\n\n\
         Every request_permission call must be reviewer-ready. Include:\n\
         - description: concise summary of what you are about to do\n\
         - rationale: why approval is needed right now\n\
         - context.summary: what you are working on in this cycle\n\
         - context.why_permission_needed: explicit justification for permission\n\
         - context.planned_steps, context.files, context.commands (if known)\n\
         - context.risks and context.rollback_plan (if relevant)\n\n\
         Good sources for scouting proactive work:\n\
         - Todoist (via MCP) — check for relevant tasks and deadlines\n\
         - Canvas (via MCP) — check for upcoming assignments or deadlines\n\
         - Git history — recent commits, open branches, stale PRs\n\
         - Session history — patterns in what the user works on\n\n\
         When done, you MUST call end_ambient_cycle with a summary of \
         everything you did, including compaction count. Always schedule \
         your next wake time with context for what you plan to do next.\n\n\
         Set `significance` on every end_ambient_cycle call, and default to \
         \"routine\". It decides whether the user's PHONE buzzes.\n\
         - \"routine\": gardening, memory upkeep, queue checks, \"nothing to \
         do\", re-verifying something already known. Sends NO notification. \
         This is most cycles, and touching memories does not change that: \
         gardening IS memory work.\n\
         - \"notable\": ONLY when the user would want their phone to buzz. \
         You are blocked on them, something needs their decision, or you \
         finished work they were waiting on. If in doubt, choose routine: \
         a missed notification costs one cycle of delay, whereas noise \
         trains them to ignore the channel and costs every future alert.\n\
         Permission requests, failures and code changes always notify on \
         their own, so you never need \"notable\" to reach the user for those.\n\n\
         ## Messaging Check-ins\n\n\
         You have a `send_message` tool. Use it to keep the user informed \
         about what you're doing. Send a brief message when you start a cycle \
         and when you finish significant work. Keep messages short and useful — \
         the user should be able to glance at their messages and know what's happening \
         without opening jcode. You can optionally target a specific channel \
         (e.g. telegram, discord) or omit channel to send to all.\n",
    );

    // Name the exact repo PRs must target. Left implicit, `gh pr create`
    // defaults to the UPSTREAM of a fork and fails with a permissions error,
    // which is how a cycle's work ended up stranded on a pushed branch the
    // user never saw.
    let pr_repo = crate::config::config().ambient.pr_repo.trim().to_string();
    if !pr_repo.is_empty() {
        prompt.push_str(&format!(
            "\n## Pull Requests\n\
             Open every pull request against `{pr_repo}`, using \
             `gh pr create --repo {pr_repo} --fill`. This is the user's own \
             fork and the only place they review your work. Never open a PR \
             against the upstream repository, and never leave code work as a \
             pushed branch with no PR: to the user that is indistinguishable \
             from having done nothing.\n"
        ));
    }

    // Tell the agent whether proactive work is actually enabled.
    //
    // The prompt used to say "only if enabled" while nothing ever substituted
    // whether it WAS enabled, and `ambient.proactive_work` was read nowhere in
    // the runner: a config knob with no consumer. The agent was left guessing
    // at a condition it could not evaluate, so it stayed cautious and mostly
    // gardened, which reads as "the agent does not do much".
    if crate::config::config().ambient.proactive_work {
        prompt.push_str(
            "\n## Proactive Work\n\n\
             `ambient.proactive_work` is ENABLED. Once the queue and gardening \
             are handled, actively look for useful work rather than ending the \
             cycle early: stale branches, failing or flaky tests, TODOs left in \
             recent commits, dependency or documentation drift. Prefer small, \
             self-contained changes you can finish and verify within one cycle.\n\n\
             Be conservative about what you pick, not about whether to pick \
             anything. A bad surprise is worse than no surprise, so check the \
             feedback memories first and skip anything resembling work the user \
             has rejected before.\n\n",
        );
    } else {
        prompt.push_str(
            "\n## Proactive Work\n\n\
             `ambient.proactive_work` is DISABLED, so this is a garden-only \
             cycle. Execute queued items and maintain the memory graph, but do \
             not go looking for code changes to make. If you notice something \
             worth doing, record it in the cycle summary instead of acting on \
             it.\n\n",
        );
    }

    // When the user has pre-authorized ambient work, say so explicitly.
    // Otherwise the agent keeps calling request_permission out of caution,
    // which is now a no-op round trip that only wastes cycle budget.
    if crate::config::config().ambient.auto_approve_permissions {
        prompt.push_str(
            "\n## Permissions\n\n\
             The user has enabled `ambient.auto_approve_permissions`, so every \
             `request_permission` call is approved immediately and never reaches \
             a human. Do not use it as a safety net or a way to defer a decision: \
             nobody is on the other end. Prefer just doing the work.\n\n\
             This makes your own judgment the only real check. Stay inside the \
             scope of the initiative or task you were given, keep changes on \
             branches with PRs, and never take an action that would be hard to \
             undo (force-push, merge, branch or data deletion, anything \
             destructive or externally visible) unless it was explicitly asked \
             for. When something falls outside your scope, stop and report it \
             via `send_message` and in your cycle summary instead of approving \
             yourself into it.\n",
        );
    }

    prompt
}

pub fn format_scheduled_session_message(item: &ScheduledItem) -> String {
    let mut lines = vec![
        "[Scheduled task]".to_string(),
        "A scheduled task for this session is now due.".to_string(),
        String::new(),
        format!(
            "Task: {}",
            item.task_description.as_deref().unwrap_or(&item.context)
        ),
    ];

    if let Some(ref dir) = item.working_dir {
        lines.push(format!("Working directory: {}", dir));
    }
    if !item.relevant_files.is_empty() {
        lines.push(format!(
            "Relevant files: {}",
            item.relevant_files.join(", ")
        ));
    }
    if let Some(ref branch) = item.git_branch {
        lines.push(format!("Branch: {}", branch));
    }
    if let Some(ref ctx) = item.additional_context {
        lines.push(String::new());
        lines.push(ctx.clone());
    }

    lines.join("\n")
}

/// Format a chrono::Duration into a rough human-readable string.
pub(crate) fn format_duration_rough(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m > 0 {
            format!("{}h {}m", h, m)
        } else {
            format!("{}h", h)
        }
    } else {
        let days = secs / 86400;
        format!("{}d", days)
    }
}

/// Format a number of minutes into a human-friendly string.
/// E.g. 5 → "5m", 90 → "1h 30m", 370 → "6h 10m", 1500 → "1d 1h"
pub fn format_minutes_human(mins: u32) -> String {
    if mins < 60 {
        format!("{}m", mins)
    } else if mins < 1440 {
        let h = mins / 60;
        let m = mins % 60;
        if m > 0 {
            format!("{}h {}m", h, m)
        } else {
            format!("{}h", h)
        }
    } else {
        let d = mins / 1440;
        let h = (mins % 1440) / 60;
        if h > 0 {
            format!("{}d {}h", d, h)
        } else {
            format!("{}d", d)
        }
    }
}
