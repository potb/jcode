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

/// One configured project in the ambient rotation, in priority order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedProject {
    /// Absolute project path, `~` expanded, no trailing slash.
    pub path: String,
    /// PR review target as `owner/repo`, empty when the project's own `origin`
    /// is the right target.
    pub pr_repo: String,
    /// Standing instructions declared in config: inline text, a referenced
    /// file, or both.
    pub instructions: String,
    /// Wall-clock window specs during which this project may be worked on.
    /// Empty means no schedule of its own.
    pub active_windows: Vec<String>,
}

/// The user's configured projects, highest priority first.
///
/// Order comes from `[[ambient.projects]]`, which TOML preserves, so the order
/// written in the file is the priority order. The older `project_priority`
/// list and `pr_repos` map are merged in behind it: an existing config must
/// keep working, and a project named in both places should not appear twice.
fn configured_projects() -> Vec<ResolvedProject> {
    let ambient = &crate::config::config().ambient;
    let mut out: Vec<ResolvedProject> = Vec::new();

    let mut push = |path: &str, pr_repo: &str, instructions: String, active_windows: Vec<String>| {
        let path = path.trim();
        if path.is_empty() {
            return;
        }
        let path = expand_project_path(path);
        let pr_repo = pr_repo.trim().to_string();
        match out.iter_mut().find(|p| p.path == path) {
            // First mention wins the rank; a PR target from a later, less
            // specific source still fills an empty one.
            Some(existing) => {
                if existing.pr_repo.is_empty() {
                    existing.pr_repo = pr_repo;
                }
                if existing.instructions.is_empty() {
                    existing.instructions = instructions;
                }
                if existing.active_windows.is_empty() {
                    existing.active_windows = active_windows;
                }
            }
            None => out.push(ResolvedProject {
                path,
                pr_repo,
                instructions,
                active_windows,
            }),
        }
    };

    for project in &ambient.projects {
        push(
            &project.path,
            &project.pr_repo,
            configured_project_instructions(project),
            project.active_windows.clone(),
        );
    }
    for path in &ambient.project_priority {
        push(path, "", String::new(), Vec::new());
    }
    for (path, repo) in &ambient.pr_repos {
        push(path, repo, String::new(), Vec::new());
    }

    // The legacy single `pr_repo` names a repository, not a path, so it can
    // only fill in a PR target for an already-listed project whose directory
    // ends with that repo name.
    let legacy = ambient.pr_repo.trim();
    if !legacy.is_empty()
        && let Some(repo_name) = legacy.rsplit('/').next()
    {
        for project in out.iter_mut() {
            if project.pr_repo.is_empty()
                && project.path.rsplit('/').next() == Some(repo_name)
            {
                project.pr_repo = legacy.to_string();
            }
        }
    }

    out
}

/// Just the project paths, highest priority first.
fn configured_project_priority() -> Vec<String> {
    workable_projects()
        .into_iter()
        .map(|p| p.path)
        .collect()
}

/// Standing instructions declared for a project in config: the inline
/// `instructions`, plus the contents of `instructions_file` when set.
///
/// Both are allowed together so a short rule can sit inline while a longer
/// document lives in a file, without forcing a choice between them.
fn configured_project_instructions(project: &crate::config::AmbientProject) -> String {
    let mut parts: Vec<String> = Vec::new();

    let inline = project.instructions.trim();
    if !inline.is_empty() {
        parts.push(inline.to_string());
    }

    let file = project.instructions_file.trim();
    if !file.is_empty() {
        let path = if file.starts_with('/') || file.starts_with("~/") {
            std::path::PathBuf::from(expand_project_path(file))
        } else {
            // A bare name is resolved against the instructions directory, so
            // `instructions_file = "jcode.md"` keeps working after the file is
            // moved out of the slug-named layout.
            match crate::storage::jcode_dir() {
                Ok(dir) => dir.join("ambient").join(AMBIENT_INSTRUCTIONS_DIR).join(file),
                Err(_) => std::path::PathBuf::from(file),
            }
        };
        match read_instructions_file(&path) {
            Some(text) => parts.push(text),
            // Silence here would look identical to "no instructions", so a
            // path the user believes is loaded must complain when it is not.
            None => crate::logging::warn(&format!(
                "Ambient: instructions_file '{}' for project '{}' could not be read;                  that project's rules are NOT in the prompt",
                path.display(),
                project.path
            )),
        }
    }

    parts.join("\n\n")
}

/// Whether a project's own wall-clock windows are open right now.
///
/// Fails OPEN, like the global window check: an unparseable entry must not
/// silently fence a project off forever. `ignore_active_windows` covers the
/// per-project schedules too, so the one override still means "run anywhere,
/// anytime" without deleting a schedule the user tuned.
fn project_window_open(project: &ResolvedProject) -> bool {
    if project.active_windows.is_empty() {
        return true;
    }
    if crate::config::config().ambient.ignore_active_windows {
        return true;
    }
    let (windows, bad) = super::schedule_window::parse_windows(&project.active_windows);
    if !bad.is_empty() {
        crate::logging::warn(&format!(
            "Ambient: ignoring unparseable active_windows entries for project '{}': {}",
            project.path,
            bad.join(", ")
        ));
    }
    if windows.is_empty() {
        return true;
    }
    super::schedule_window::evaluate(&windows, &chrono::Local::now()).is_open()
}

/// The projects workable right now: configured order, minus any whose own
/// window is currently closed.
///
/// Filtering here rather than at the point of use keeps one answer to "which
/// projects may I work on", so the walk order, the PR instructions, and the
/// per-project instruction sections cannot disagree with each other.
fn workable_projects() -> Vec<ResolvedProject> {
    configured_projects()
        .into_iter()
        .filter(project_window_open)
        .collect()
}

/// Expand a configured project path: `~/` against `HOME`, trailing slash off.
fn expand_project_path(path: &str) -> String {
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        match std::env::var("HOME") {
            Ok(home) => format!("{}/{}", home.trim_end_matches('/'), rest),
            Err(_) => path.to_string(),
        }
    } else {
        path.to_string()
    };
    expanded.trim_end_matches('/').to_string()
}

/// Whether a session working directory belongs to a configured project.
///
/// A session started in a subdirectory of the project still belongs to it, so
/// this is a path-boundary prefix test rather than string equality. The
/// boundary check matters: `/src/jcode-cron` must not match `/src/jcode`.
fn paths_match(session_dir: &str, project: &str) -> bool {
    let dir = session_dir.trim_end_matches('/');
    let proj = project.trim_end_matches('/');
    dir == proj || dir.strip_prefix(proj).is_some_and(|r| r.starts_with('/'))
}

/// Index of `dir` in the configured priority list, or `usize::MAX` when it is
/// not a priority project. Lower sorts first.
pub(crate) fn priority_rank(priority: &[String], dir: &str) -> usize {
    priority
        .iter()
        .position(|p| paths_match(dir, p))
        .unwrap_or(usize::MAX)
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
        let priority = configured_project_priority();
        // Session count answers "where has the user been", not "what matters".
        // A configured priority is an explicit answer to the second question,
        // so it outranks activity; unlisted projects keep the activity order
        // behind the listed ones.
        let mut ranked: Vec<_> = project_counts.into_iter().collect();
        ranked.sort_by(|a, b| {
            let rank_a = priority_rank(&priority, a.0);
            let rank_b = priority_rank(&priority, b.0);
            rank_a.cmp(&rank_b).then(b.1.cmp(&a.1)).then(a.0.cmp(b.0))
        });
        let any_prioritized = ranked
            .iter()
            .any(|(dir, _)| priority_rank(&priority, dir) < usize::MAX);
        for (dir, count) in &ranked {
            let tag = if priority_rank(&priority, dir) < usize::MAX {
                " [priority]"
            } else {
                ""
            };
            prompt.push_str(&format!("- {} ({} session(s)){}\n", dir, count, tag));
        }
        if any_prioritized {
            prompt.push_str(
                "Projects marked [priority] are the user's stated priorities, listed \
                 highest first. When choosing proactive work, exhaust useful work in \
                 a higher-priority project before spending a cycle on a lower one, \
                 regardless of which project has more recent sessions. Queued and \
                 scheduled items still run when due.\n",
            );
        }
        prompt.push_str(
            "To work in one of these projects, use its path as the working directory: \
             per-project memory, AGENTS.md, and git state only resolve when a \
             working directory is set.\n",
        );
        prompt.push('\n');
    }

    // A priority entry the user configured but that has no recent sessions is
    // exactly the case this knob exists for: the important project is the one
    // being neglected. Listing only "projects active recently" would hide it.
    {
        let priority = configured_project_priority();
        let seen: std::collections::BTreeSet<&str> = recent_sessions
            .iter()
            .filter_map(|s| s.working_dir.as_deref())
            .collect();
        let idle: Vec<&String> = priority
            .iter()
            .filter(|p| !seen.iter().any(|s| paths_match(s, p)))
            .collect();
        if !idle.is_empty() {
            prompt.push_str("## Priority Projects With No Recent Sessions\n");
            for p in idle {
                prompt.push_str(&format!("- {}\n", p));
            }
            prompt.push_str(
                "These rank above the active projects above. No recent session does \
                 not mean no work: check their branches, tests, and open PRs before \
                 falling back to a lower-priority project.\n\n",
            );
        }
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
    // A memory you wrote about a passing situation ("the user has live sessions
    // in X, stay out") is an observation with a timestamp, not a standing rule.
    // Re-check the condition before letting one block work; if it no longer
    // holds, or it never named an expiry, rewrite or forget it this cycle.
    prompt.push_str(
        "Treat your own memories that tell you to AVOID an area as observations \
         with an expiry, not as permanent rules. Before letting one stop you, \
         re-check whether the condition it describes still holds right now. If \
         it does not, forget or rewrite it in this cycle rather than skipping \
         the work again. Repeatedly declining to work somewhere because of a \
         note you wrote yourself is how a whole project goes untouched for days.\n",
    );
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

    // The user is frequently away from the machine but can always reach
    // GitHub, so issues are the one reply path that works from a phone and is
    // still restricted to people who can comment on the repo.
    let safety = &crate::config::config().safety;
    if safety.github_enabled
        && let Some(repo) = safety.github_repo.clone()
    {
        let label = safety.github_label.clone();
        prompt.push_str(&format!(
            "\n## GitHub Issues\n\n\
             The user is often away from this machine but can always reach \
             GitHub. `{repo}` is where you talk to them: `send_message` with \
             channel `github` OPENS A NEW ISSUE labelled `{label}`, one per \
             topic, and their comments on those issues arrive as directives on \
             your next cycle, tagged `[github#N message from user]`.\n\n\
             Because each topic is its own issue, treat it as a unit of work \
             with a lifecycle:\n\
             - One issue per distinct question, decision or piece of work. Do \
             not pile unrelated things into one thread, and do not open a new \
             issue for something that already has one.\n\
             - The first line of your message becomes the issue title, so make \
             it a specific, readable summary.\n\
             - Continue an existing topic with `github_issue` \
             (action=\"comment\", issue=N) rather than opening a duplicate.\n\
             - When a topic is resolved, CLOSE it with `github_issue` \
             (action=\"close\", issue=N, comment=\"<what the outcome was>\"). \
             An open issue means unfinished business, so leaving settled ones \
             open makes the list useless. Only the user closes an issue that \
             was waiting on their decision.\n\
             - `github_issue` (action=\"list\") shows the open topics. Check it \
             each cycle: open issues are your backlog, not a notification log.\n\n\
             Raising a question there does NOT license you to stop. Open the \
             issue, state the assumption you are proceeding on, and continue \
             the work in the same cycle. Their answer is a correction to apply \
             later, never a gate to wait behind.\n"
        ));
    }

    // Name the exact repo PRs must target. Left implicit, `gh pr create`
    // defaults to the UPSTREAM of a fork and fails with a permissions error,
    // which is how a cycle's work ended up stranded on a pushed branch the
    // user never saw.
    //
    // The setting names ONE repo, so it cannot mean "every PR everywhere":
    // once ambient works across several projects, an unscoped rule would send
    // another project's PR to this fork. It is an override for the repo it
    // names; elsewhere the project's own `origin` is correct.
    let pr_repo = crate::config::config().ambient.pr_repo.trim().to_string();
    let projects = workable_projects();
    let forked: Vec<&ResolvedProject> = projects.iter().filter(|p| !p.pr_repo.is_empty()).collect();
    let direct: Vec<&ResolvedProject> = projects.iter().filter(|p| p.pr_repo.is_empty()).collect();

    if !projects.is_empty() || !pr_repo.is_empty() {
        prompt.push_str(
            "\n## Pull Requests\n\
             Projects come in two shapes and the difference decides where BOTH \
             the branch and the PR go. Getting it wrong is not a style issue: \
             pushing a fork project's branch to upstream fails on permissions, \
             and a PR opened against upstream from a fork workflow is rejected, \
             which is how work ends up stranded where the user never sees it.\n",
        );

        if !forked.is_empty() {
            prompt.push_str(
                "\nFORK PROJECTS. The user works through their own fork and \
                 reviews everything there. Upstream is READ-ONLY to you: never \
                 push a branch to it and never open a PR against it.\n",
            );
            for project in &forked {
                let repo = &project.pr_repo;
                let path = &project.path;
                prompt.push_str(&format!(
                    "- `{path}`: push the branch to the fork remote for `{repo}` \
                     (check `git remote -v`; it is usually not `origin`), then \
                     `gh pr create --repo {repo} --fill`.\n"
                ));
            }
        }

        if !direct.is_empty() {
            prompt.push_str(
                "\nDIRECT-ACCESS PROJECTS. The user pushes to these repos \
                 directly, so there is no fork in the picture:\n",
            );
            for project in &direct {
                prompt.push_str(&format!(
                    "- `{}`: push the branch to `origin` and open the PR with a \
                     plain `gh pr create --fill` from that working directory.\n",
                    project.path
                ));
            }
            prompt.push_str(
                "Do NOT route these through any fork listed above: a repo named \
                 for one project is never the right target for another.\n",
            );
        }

        if !pr_repo.is_empty() && forked.iter().all(|p| p.pr_repo != pr_repo) {
            let repo_name = pr_repo.rsplit('/').next().unwrap_or(&pr_repo);
            prompt.push_str(&format!(
                "\nFor work in the `{repo_name}` repository, open pull requests \
                 against `{pr_repo}` with `gh pr create --repo {pr_repo} --fill`; \
                 never against its upstream.\n"
            ));
        }

        prompt.push_str(
            "\nFor a project not named above, target its own `origin` and check \
             `git remote -v` if unsure. Never leave code work as a pushed branch \
             with no PR: to the user that is indistinguishable from having done \
             nothing.\n",
        );
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

    // Walking the priority list, rather than stopping at the first project
    // that happens to be quiet.
    //
    // Observed: a cycle checked the top-priority repo, found every PR green and
    // nothing to do, and ended right there. From the user's side that is an
    // idle cycle even though the second and third projects on their own list
    // were never looked at. "No work in project 1" is a reason to move to
    // project 2, not a reason to end the cycle.
    {
        let priority = configured_project_priority();
        if crate::config::config().ambient.proactive_work && !priority.is_empty() {
            prompt.push_str(
                "\n## Work Through The Priority List, Do Not Stop At The First Quiet Project\n\n\
                 The user's configured project list is an ORDER to walk, not a \
                 single target. Finding no useful work in the highest-priority \
                 project does NOT end the cycle: it means you move to the next \
                 project on the list and look there. Only after you have actually \
                 examined every project below may you end the cycle for lack of \
                 work.\n\n\
                 In priority order:\n\n",
            );
            for (idx, project) in priority.iter().enumerate() {
                prompt.push_str(&format!("{}. {}\n", idx + 1, project));
            }
            prompt.push_str(
                "\nFor each one, set it as your working directory and actually look \
                 before deciding it is quiet: open PRs and their CI state, the \
                 branch's position against its remote, failing or flaky tests, \
                 TODOs in recent commits, and dependency or documentation drift. \
                 \"Everything was green last cycle\" is not a check.\n\n\
                 When you end the cycle, your summary must say which projects you \
                 examined and what you found in each, so a cycle that really had \
                 nothing to do is distinguishable from one that stopped early. \
                 Stopping at project 1 while later projects went unexamined is the \
                 specific failure this section exists to prevent.\n\n",
            );

            // Name the projects held back by their own schedule. Omitting them
            // silently would read as "that project is finished", and the agent
            // would have no way to tell a closed window from a project the user
            // removed.
            let closed: Vec<ResolvedProject> = configured_projects()
                .into_iter()
                .filter(|project| !project_window_open(project))
                .collect();
            if !closed.is_empty() {
                prompt.push_str(
                    "Outside their configured hours right now, so NOT part of this \
                     cycle (they are not finished, and not yours to work on until \
                     their window opens):\n",
                );
                for project in &closed {
                    prompt.push_str(&format!(
                        "- {} (allowed: {})\n",
                        project.path,
                        project.active_windows.join(", ")
                    ));
                }
                prompt.push('\n');
            }
        }
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
             Never park work in an \"awaiting approval\" or \"waiting for the \
             user\" state, and never carry such an item across cycles. There is \
             no human in the loop between cycles, so a task that waits is a task \
             that never happens. When the user has asked for something, execute \
             it in the cycle you pick it up: decide, do it, and report the result. \
             If you are unsure between two reasonable options, pick the safer one \
             and say which you picked rather than asking and stopping.\n\n\
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

    // --- User standing instructions ---
    //
    // Config booleans can only say "do work" or "don't". They cannot say what
    // kind of work matters, what is off limits, or how assertive to be. Users
    // need a place to state that in prose, and it has to outrank the agent's
    // own accumulated caution: memories written during one busy afternoon were
    // otherwise treated as permanent rules and quietly fenced whole repos off.
    if let Some(instructions) = user_ambient_instructions() {
        prompt.push_str(
            "\n## Standing Instructions From The User\n\n\
             These come from the user's own ambient instructions file. They are \
             a direct statement of what they want you to do, so they OUTRANK \
             your own habits, your cautious defaults, and any memory you wrote \
             in an earlier cycle. If a memory says to avoid an area but these \
             instructions ask for work there, follow the instructions and \
             correct the memory.\n\n",
        );
        prompt.push_str(instructions.trim());
        prompt.push_str("\n\n");
    }

    // Per-project instructions, for every project the user is likely to touch
    // this cycle: the ones seen recently plus the configured priorities. The
    // agent picks its own working directory, so it needs these up front rather
    // than after it has already decided where to work.
    {
        let projects = workable_projects();
        let mut dirs: Vec<String> = Vec::new();
        for dir in recent_sessions.iter().filter_map(|s| s.working_dir.clone()) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        for project in &projects {
            if !dirs.iter().any(|d| paths_match(d, &project.path)) {
                dirs.push(project.path.clone());
            }
        }
        let mut sections = String::new();
        for dir in &dirs {
            // Config wins over the slug-named file: an instruction the user can
            // see in their config must not be silently overridden by a file
            // they forgot exists.
            let from_config = projects
                .iter()
                .find(|p| paths_match(dir, &p.path))
                .map(|p| p.instructions.clone())
                .filter(|text| !text.is_empty());
            let text = from_config.or_else(|| project_ambient_instructions(dir));
            if let Some(text) = text {
                sections.push_str(&format!("### {}\n{}\n\n", dir, text.trim()));
            }
        }
        if !sections.is_empty() {
            prompt.push_str(
                "\n## Per-Project Standing Instructions\n\n\
                 The user wrote these for specific projects. They apply whenever \
                 you work in that project and carry the same weight as the \
                 standing instructions above.\n\n",
            );
            prompt.push_str(&sections);
        }
    }

    prompt
}

/// Standing ambient instructions written by the user, if any.
///
/// Read from `~/.jcode/ambient-instructions.md`. Read fresh on every cycle
/// rather than cached, so editing the file takes effect on the next wake
/// without restarting the daemon.
pub fn user_ambient_instructions() -> Option<String> {
    let path = crate::storage::jcode_dir()
        .ok()?
        .join(AMBIENT_INSTRUCTIONS_FILE);
    read_instructions_file(&path)
}

/// Per-project standing instructions for `project_dir`, if any.
///
/// These live centrally under `~/.jcode/ambient/instructions/`, NOT inside the
/// project itself. A dotfile committed into every repo the user works in is
/// their instructions to their own agent leaking into shared source trees,
/// where it shows up in diffs, reviews and other people's checkouts. Keeping
/// the whole set in one place also means they can be read and edited together.
///
/// The file name is derived from the absolute project path, so two projects
/// with the same directory name do not collide.
pub fn project_ambient_instructions(project_dir: &str) -> Option<String> {
    let path = project_instructions_path(project_dir)?;
    read_instructions_file(&path)
}

/// Path of the central instructions file for a project directory.
pub fn project_instructions_path(project_dir: &str) -> Option<std::path::PathBuf> {
    let dir = crate::storage::jcode_dir()
        .ok()?
        .join("ambient")
        .join(AMBIENT_INSTRUCTIONS_DIR);
    Some(dir.join(format!("{}.md", project_instructions_slug(project_dir))))
}

/// Filesystem-safe, collision-resistant slug for an absolute project path.
///
/// The basename alone would map `~/work/api` and `~/personal/api` onto the same
/// file, so the full path is flattened instead.
pub fn project_instructions_slug(project_dir: &str) -> String {
    let trimmed = project_dir.trim_end_matches('/');
    let flattened: String = trimmed
        .trim_start_matches('/')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if flattened.is_empty() {
        "root".to_string()
    } else {
        flattened
    }
}

fn read_instructions_file(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// File name, under `~/.jcode/`, holding the user's standing ambient
/// instructions.
pub const AMBIENT_INSTRUCTIONS_FILE: &str = "ambient-instructions.md";

/// Directory, under `~/.jcode/ambient/`, holding per-project instruction files.
pub const AMBIENT_INSTRUCTIONS_DIR: &str = "instructions";

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
