use super::*;
use chrono::Duration;

#[test]
fn test_ambient_status_default() {
    let status = AmbientStatus::default();
    assert_eq!(status, AmbientStatus::Idle);
}

#[test]
fn test_priority_ordering() {
    assert!(Priority::High > Priority::Normal);
    assert!(Priority::Normal > Priority::Low);
}

#[test]
fn test_scheduled_queue_push_and_pop() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let mut queue = ScheduledQueue::load(path);
    assert!(queue.is_empty());

    let past = Utc::now() - Duration::minutes(5);
    let future = Utc::now() + Duration::hours(1);

    queue.push(ScheduledItem {
        id: "s1".into(),
        scheduled_for: past,
        context: "past item".into(),
        priority: Priority::Low,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".into(),
        created_at: Utc::now(),
        project: None,
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    });

    queue.push(ScheduledItem {
        id: "s2".into(),
        scheduled_for: future,
        context: "future item".into(),
        priority: Priority::High,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".into(),
        created_at: Utc::now(),
        project: None,
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    });

    assert_eq!(queue.len(), 2);

    let ready = queue.pop_ready();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, "s1");

    // Future item still in queue
    assert_eq!(queue.len(), 1);
    assert_eq!(queue.peek_next().unwrap().id, "s2");
}

#[test]
fn test_scheduled_queue_remove_by_id_persists_remaining_items() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let mut queue = ScheduledQueue::load(path.clone());
    let future = Utc::now() + Duration::hours(1);

    queue.push(ScheduledItem {
        id: "keep".into(),
        scheduled_for: future,
        context: "keep item".into(),
        priority: Priority::Normal,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".into(),
        created_at: Utc::now(),
        project: None,
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    });
    queue.push(ScheduledItem {
        id: "cancel".into(),
        scheduled_for: future,
        context: "cancel item".into(),
        priority: Priority::High,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".into(),
        created_at: Utc::now(),
        project: None,
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    });

    let removed = queue.remove_by_id("cancel").unwrap().unwrap();
    assert_eq!(removed.id, "cancel");
    assert!(queue.remove_by_id("missing").unwrap().is_none());

    let reloaded = ScheduledQueue::load(path);
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded.items()[0].id, "keep");
}

#[test]
fn test_pop_ready_sorts_by_priority_then_time() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let mut queue = ScheduledQueue::load(path);
    let past1 = Utc::now() - Duration::minutes(10);
    let past2 = Utc::now() - Duration::minutes(5);

    queue.push(ScheduledItem {
        id: "low_early".into(),
        scheduled_for: past1,
        context: "low early".into(),
        priority: Priority::Low,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".into(),
        created_at: Utc::now(),
        project: None,
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    });

    queue.push(ScheduledItem {
        id: "high_late".into(),
        scheduled_for: past2,
        context: "high late".into(),
        priority: Priority::High,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".into(),
        created_at: Utc::now(),
        project: None,
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    });

    let ready = queue.pop_ready();
    assert_eq!(ready.len(), 2);
    // High priority should come first
    assert_eq!(ready[0].id, "high_late");
    assert_eq!(ready[1].id, "low_early");
}

#[test]
fn test_take_ready_direct_items_only_removes_direct_targets() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let mut queue = ScheduledQueue::load(path);
    let past = Utc::now() - Duration::minutes(5);

    queue.push(ScheduledItem {
        id: "session_due".into(),
        scheduled_for: past,
        context: "scheduled session task".into(),
        priority: Priority::Normal,
        target: ScheduleTarget::Session {
            session_id: "session_123".into(),
        },
        created_by_session: "session_123".into(),
        created_at: Utc::now(),
        project: None,
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    });

    queue.push(ScheduledItem {
        id: "spawn_due".into(),
        scheduled_for: past,
        context: "spawned session task".into(),
        priority: Priority::High,
        target: ScheduleTarget::Spawn {
            parent_session_id: "session_123".into(),
        },
        created_by_session: "session_123".into(),
        created_at: Utc::now(),
        project: None,
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    });

    queue.push(ScheduledItem {
        id: "ambient_due".into(),
        scheduled_for: past,
        context: "scheduled ambient task".into(),
        priority: Priority::High,
        target: ScheduleTarget::Ambient,
        created_by_session: "ambient".into(),
        created_at: Utc::now(),
        project: None,
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    });

    let ready_direct = queue.take_ready_direct_items();
    assert_eq!(ready_direct.len(), 2);
    assert_eq!(ready_direct[0].id, "spawn_due");
    assert_eq!(ready_direct[1].id, "session_due");
    assert_eq!(queue.len(), 1);
    assert_eq!(queue.items()[0].id, "ambient_due");
}

#[test]
fn test_ambient_state_record_cycle() {
    let mut state = AmbientState::default();
    assert_eq!(state.total_cycles, 0);

    let result = AmbientCycleResult {
        summary: "Merged 2 duplicates".into(),
        memories_modified: 3,
        compactions: 1,
        proactive_work: None,
        significance: None,
        next_schedule: None,
        started_at: Utc::now() - Duration::seconds(30),
        ended_at: Utc::now(),
        status: CycleStatus::Complete,
        conversation: None,
        agent_session_id: None,
    };

    state.record_cycle(&result);
    assert_eq!(state.total_cycles, 1);
    assert_eq!(state.last_summary.as_deref(), Some("Merged 2 duplicates"));
    assert_eq!(state.last_compactions, Some(1));
    assert_eq!(state.last_memories_modified, Some(3));
    assert_eq!(state.status, AmbientStatus::Idle);
}

#[test]
fn test_ambient_state_record_cycle_with_schedule() {
    let mut state = AmbientState::default();

    let result = AmbientCycleResult {
        summary: "Done".into(),
        memories_modified: 0,
        compactions: 0,
        proactive_work: None,
        significance: None,
        next_schedule: Some(ScheduleRequest {
            wake_in_minutes: Some(15),
            wake_at: None,
            context: "check CI".into(),
            priority: Priority::Normal,
            target: ScheduleTarget::Ambient,
            created_by_session: "ambient_test".into(),
            working_dir: None,
            task_description: None,
            relevant_files: Vec::new(),
            git_branch: None,
            additional_context: None,
        }),
        started_at: Utc::now() - Duration::seconds(10),
        ended_at: Utc::now(),
        status: CycleStatus::Complete,
        conversation: None,
        agent_session_id: None,
    };

    state.record_cycle(&result);
    assert!(matches!(state.status, AmbientStatus::Scheduled { .. }));
}

#[test]
fn test_ambient_lock_release() {
    // Use a temp dir so we don't conflict with real state
    let tmp_dir = tempfile::tempdir().unwrap();
    let lock_file = tmp_dir.path().join("test.lock");

    // Manually create a lock to test release/drop
    std::fs::write(&lock_file, std::process::id().to_string()).unwrap();
    let lock = AmbientLock {
        lock_path: lock_file.clone(),
    };
    lock.release().unwrap();
    assert!(!lock_file.exists());
}

#[test]
fn test_schedule_id_format() {
    let id = format!("sched_{:08x}", rand::random::<u32>());
    assert!(id.starts_with("sched_"));
    assert_eq!(id.len(), 6 + 8); // "sched_" + 8 hex chars
}

#[test]
fn test_format_duration_rough() {
    assert_eq!(format_duration_rough(Duration::seconds(30)), "30s");
    assert_eq!(format_duration_rough(Duration::minutes(5)), "5m");
    assert_eq!(format_duration_rough(Duration::hours(2)), "2h");
    assert_eq!(
        format_duration_rough(Duration::hours(2) + Duration::minutes(30)),
        "2h 30m"
    );
    assert_eq!(format_duration_rough(Duration::days(3)), "3d");
    assert_eq!(format_duration_rough(Duration::seconds(-5)), "0s");
}

#[test]
fn test_build_ambient_system_prompt_minimal() {
    let state = AmbientState::default();
    let queue = vec![];
    let health = MemoryGraphHealth::default();
    let sessions = vec![];
    let feedback: Vec<String> = vec![];
    let budget = ResourceBudget {
        provider: "anthropic-oauth".into(),
        tokens_remaining_desc: "unknown".into(),
        window_resets_desc: "unknown".into(),
        user_usage_rate_desc: "0 tokens/min".into(),
        cycle_budget_desc: "stay under 50k tokens".into(),
    };

    let prompt =
        build_ambient_system_prompt(&state, &queue, &health, &sessions, &feedback, &budget, 0);

    assert!(prompt.contains("ambient agent for jcode"));
    assert!(prompt.contains("## Current State"));
    assert!(prompt.contains("never (first run)"));
    assert!(prompt.contains("Active user sessions: none"));
    assert!(prompt.contains("## Scheduled Queue"));
    assert!(prompt.contains("Empty"));
    assert!(prompt.contains("## Memory Graph Health"));
    assert!(prompt.contains("Total memories: 0"));
    assert!(prompt.contains("## User Feedback History"));
    assert!(prompt.contains("No feedback memories"));
    assert!(prompt.contains("## Resource Budget"));
    assert!(prompt.contains("anthropic-oauth"));
    assert!(prompt.contains("## Instructions"));
    assert!(prompt.contains("end_ambient_cycle"));
    assert!(prompt.contains("reviewer-ready"));
    assert!(prompt.contains("context.why_permission_needed"));
}

/// The whole point of issue #22 direction 4: the ambient prompt is installed
/// through `system_prompt_override`, which drops the base prompt, so if this
/// section is missing a cycle is judged by gates it was never told about.
#[test]
fn test_build_ambient_system_prompt_includes_todo_guidance() {
    let state = AmbientState::default();
    let queue = vec![];
    let health = MemoryGraphHealth::default();
    let sessions = vec![];
    let feedback: Vec<String> = vec![];
    let budget = ResourceBudget {
        provider: "anthropic-oauth".into(),
        tokens_remaining_desc: "unknown".into(),
        window_resets_desc: "unknown".into(),
        user_usage_rate_desc: "0 tokens/min".into(),
        cycle_budget_desc: "stay under 50k tokens".into(),
    };

    let prompt =
        build_ambient_system_prompt(&state, &queue, &health, &sessions, &feedback, &budget, 0);

    assert!(prompt.contains("## Todo Discipline"));
    assert!(
        prompt.contains(crate::ambient::gates::AMBIENT_TODO_GUIDANCE),
        "the guidance must be the shared constant, not a drifting copy"
    );
    // Each gate the runner can raise should be recognisable in the guidance,
    // or the cycle cannot act on the follow-up it eventually receives.
    assert!(prompt.contains("completion_confidence"));
    assert!(prompt.contains("feedback loop"));

    // Markdown headings must stay separated from the section that follows, or
    // the next heading is swallowed into this one's last bullet.
    assert!(prompt.contains("\n\n## Messaging Check-ins"));
}

/// The pass thresholds are private policy. Naming them here would tell a cycle
/// which value clears the bar, which is an invitation to write that value
/// instead of doing the work it stands for.
#[test]
fn test_ambient_todo_guidance_does_not_leak_gate_thresholds() {
    let guidance = crate::ambient::gates::AMBIENT_TODO_GUIDANCE.to_lowercase();
    // Compare whole tokens, not substrings: the passing value `verified` is a
    // substring of the ordinary English word "unverified", and matching that
    // would fail on prose that names no threshold at all.
    let tokens: Vec<&str> = guidance
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| !token.is_empty())
        .collect();

    for threshold in [
        "workflow_validated",
        "outcome_delivered",
        "necessary_followthrough",
        "acceptance_aligned",
        "edge_and_integration_paths",
        "validated",
        "verified",
    ] {
        assert!(
            !tokens.contains(&threshold),
            "guidance names the passing value {threshold:?}, which turns the gate into a form to fill in"
        );
    }
}

#[test]
fn test_build_ambient_system_prompt_with_data() {
    let state = AmbientState {
        last_run: Some(Utc::now() - Duration::minutes(15)),
        total_cycles: 7,
        ..Default::default()
    };

    let queue = vec![ScheduledItem {
        id: "sched_001".into(),
        scheduled_for: Utc::now(),
        context: "Check CI status".into(),
        priority: Priority::High,
        target: ScheduleTarget::Ambient,
        created_by_session: "session_abc".into(),
        created_at: Utc::now() - Duration::minutes(10),
        project: None,
        working_dir: Some("/home/user/project".into()),
        task_description: Some("Check CI status for the main branch".into()),
        relevant_files: vec!["src/main.rs".into()],
        git_branch: Some("main".into()),
        additional_context: Some("Background: Tests were flaky yesterday".into()),
    }];

    let health = MemoryGraphHealth {
        total: 42,
        active: 38,
        inactive: 4,
        low_confidence: 3,
        contradictions: 1,
        missing_embeddings: 5,
        duplicate_candidates: 0,
        last_consolidation: Some(Utc::now() - Duration::hours(2)),
        projects: Vec::new(),
    };

    let sessions = vec![RecentSessionInfo {
        id: "session_fox_123".into(),
        status: "closed".into(),
        topic: Some("Fix auth bug".into()),
        duration_secs: 900,
        extraction_status: "extracted".into(),
        working_dir: Some("/home/potb/projects/potb/config".into()),
    }];

    let feedback = vec![
        "User approved ambient fixing typos in docs".into(),
        "User rejected ambient refactoring tests".into(),
    ];

    let budget = ResourceBudget {
        provider: "openai-oauth".into(),
        tokens_remaining_desc: "~85k".into(),
        window_resets_desc: "in 3h 20m".into(),
        user_usage_rate_desc: "120 tokens/min".into(),
        cycle_budget_desc: "stay under 15k tokens".into(),
    };

    let prompt =
        build_ambient_system_prompt(&state, &queue, &health, &sessions, &feedback, &budget, 2);

    assert!(prompt.contains("15m ago"));
    assert!(prompt.contains("Active user sessions: 2"));
    assert!(prompt.contains("Total cycles completed: 7"));
    assert!(prompt.contains("Check CI status"));
    assert!(prompt.contains("HIGH"));
    assert!(prompt.contains("42"));
    assert!(prompt.contains("38 active"));
    assert!(prompt.contains("confidence < 0.1: 3"));
    assert!(prompt.contains("contradictions: 1"));
    assert!(prompt.contains("without embeddings: 5"));
    assert!(prompt.contains("Fix auth bug"));
    assert!(prompt.contains("approved ambient fixing typos"));
    assert!(prompt.contains("rejected ambient refactoring"));
    assert!(prompt.contains("openai-oauth"));
    assert!(prompt.contains("~85k"));
    assert!(prompt.contains("Working dir: /home/user/project"));
    assert!(prompt.contains("Details: Check CI status for the main branch"));
    assert!(prompt.contains("Files: src/main.rs"));
    assert!(prompt.contains("Branch: main"));
    assert!(prompt.contains("Tests were flaky yesterday"));
}

/// A pushed branch with no PR is invisible to the user: they see no work at
/// all. When a PR target is configured the prompt must name that exact repo,
/// because `gh pr create` otherwise defaults to a fork's UPSTREAM and fails.
#[test]
fn ambient_prompt_names_the_configured_pull_request_repo() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let render = |extra: &str| {
        std::fs::write(
            temp.path().join("config.toml"),
            format!("[ambient]\nenabled = true\n{extra}"),
        )
        .expect("write config");
        crate::config::invalidate_config_cache();
        build_ambient_system_prompt(
            &AmbientState::default(),
            &[],
            &MemoryGraphHealth::default(),
            &[],
            &[],
            &ResourceBudget::default(),
            0,
        )
    };

    let configured = render("pr_repo = \"potb/jcode\"\n");
    assert!(
        configured.contains("gh pr create --repo potb/jcode"),
        "the prompt must spell out the exact PR command for the fork"
    );
    assert!(
        configured.contains("never against its upstream"),
        "the upstream default is the failure mode, so it must be called out"
    );

    let unset = render("");
    assert!(
        !unset.contains("## Pull Requests"),
        "with no target configured the prompt must not invent one"
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

#[test]
fn ambient_prompt_names_the_project_each_recent_session_ran_in() {
    // Ambient has no working directory of its own, so without the project
    // recorded per session it cannot tell jcode work from private_project work.
    let state = AmbientState::default();
    let queue = vec![];
    let health = MemoryGraphHealth::default();
    let sessions = vec![
        RecentSessionInfo {
            id: "session_a".into(),
            status: "closed".into(),
            topic: Some("Fix ambient scope".into()),
            duration_secs: 600,
            extraction_status: "extracted".into(),
            working_dir: Some("/home/potb/jcode".into()),
        },
        RecentSessionInfo {
            id: "session_b".into(),
            status: "closed".into(),
            topic: Some("PrivateProject deploy".into()),
            duration_secs: 300,
            extraction_status: "extracted".into(),
            working_dir: Some("/home/potb/projects/workspace/private_project".into()),
        },
        RecentSessionInfo {
            id: "session_c".into(),
            status: "closed".into(),
            topic: Some("Ambient cycle".into()),
            duration_secs: 60,
            extraction_status: "extracted".into(),
            working_dir: None,
        },
    ];
    let feedback: Vec<String> = vec![];
    let budget = ResourceBudget::default();

    let prompt =
        build_ambient_system_prompt(&state, &queue, &health, &sessions, &feedback, &budget, 0);

    assert!(
        prompt.contains("project: /home/potb/jcode"),
        "each session line must name its project"
    );
    assert!(prompt.contains("project: /home/potb/projects/workspace/private_project"));
    assert!(
        prompt.contains("project: (no project)"),
        "a session without a working directory must be marked as such, not \
         silently attributed to another project"
    );
    assert!(prompt.contains("## Projects Active Recently"));
    assert!(prompt.contains("/home/potb/jcode (1 session(s))"));
}

#[test]
fn ambient_prompt_lists_per_project_memory_graphs() {
    let state = AmbientState::default();
    let queue = vec![];
    let health = MemoryGraphHealth {
        total: 20,
        active: 18,
        inactive: 2,
        projects: vec![
            ProjectGraphHealth {
                working_dir: Some("/home/potb/projects/potb/config".into()),
                graph_id: "aaaa".into(),
                total: 13,
                active: 12,
                low_confidence: 1,
                missing_embeddings: 4,
            },
            ProjectGraphHealth {
                working_dir: None,
                graph_id: "bbbb".into(),
                total: 2,
                active: 2,
                low_confidence: 0,
                missing_embeddings: 0,
            },
        ],
        ..MemoryGraphHealth::default()
    };
    let sessions = vec![];
    let feedback: Vec<String> = vec![];
    let budget = ResourceBudget::default();

    let prompt =
        build_ambient_system_prompt(&state, &queue, &health, &sessions, &feedback, &budget, 0);

    assert!(prompt.contains("Per-project memory graphs:"));
    assert!(
        prompt.contains("/home/potb/projects/potb/config: 13 memories"),
        "a named project graph must show its path and size"
    );
    assert!(
        prompt.contains("bbbb: 2 memories"),
        "an unnamed project graph must still be listed by id rather than dropped"
    );
}

/// A registry entry whose store has been deleted must not appear as a phantom
/// project. Ambient reports these to the user as real memory to garden.
#[test]
fn a_project_whose_store_is_gone_is_not_reported() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", home.path());

    let manager = crate::memory::MemoryManager::new().with_project_dir(project.path());
    let mut graph = manager.load_project_graph().expect("load");
    let entry =
        crate::memory::MemoryEntry::new(crate::memory::MemoryCategory::Fact, "phantom probe");
    graph.memories.insert(entry.id.clone(), entry);
    manager.save_project_graph(&graph).expect("save");

    let path = manager
        .project_graph_path()
        .expect("path")
        .expect("project dir set");
    assert!(
        crate::ambient::gather_project_graph_health()
            .iter()
            .any(|p| p.working_dir.as_deref() == Some(project.path().to_string_lossy().as_ref())),
        "the project must be reported while its store exists"
    );

    // The store is deleted but the registry entry remains.
    std::fs::remove_file(&path).expect("remove store");
    assert!(
        crate::memory::MemoryManager::load_projects_registry()
            .values()
            .any(|dir| dir == project.path().to_string_lossy().as_ref()),
        "the registry entry is expected to survive; that is what makes this a real risk"
    );
    assert!(
        !crate::ambient::gather_project_graph_health()
            .iter()
            .any(|p| p.working_dir.as_deref() == Some(project.path().to_string_lossy().as_ref())),
        "a project with no store must not be reported as memory to garden"
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn project_graph_health_survey_reads_every_project_store() {
    // The survey must find project memory without the caller having a project
    // directory, which is exactly the ambient agent's situation.
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let alpha = tempfile::tempdir().expect("alpha project");
    let beta = tempfile::tempdir().expect("beta project");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", home.path());

    for project in [alpha.path(), beta.path()] {
        let manager = crate::memory::MemoryManager::new().with_project_dir(project);
        let mut graph = manager.load_project_graph().expect("load");
        let entry = crate::memory::MemoryEntry::new(
            crate::memory::MemoryCategory::Fact,
            &format!("survey probe for {}", project.display()),
        );
        graph.memories.insert(entry.id.clone(), entry);
        manager.save_project_graph(&graph).expect("save");
    }

    // A manager with no project directory sees nothing on its own...
    let blind = crate::memory::MemoryManager::new();
    assert!(
        blind
            .load_project_graph()
            .expect("load")
            .memories
            .is_empty(),
        "a manager without a project dir must not resolve any project graph"
    );

    // ...but the survey finds both projects and names them.
    let health = crate::ambient::gather_project_graph_health();
    let named: Vec<String> = health
        .iter()
        .filter_map(|p| p.working_dir.clone())
        .collect();
    for project in [alpha.path(), beta.path()] {
        let want = project.to_string_lossy().to_string();
        assert!(
            named.contains(&want),
            "survey must name project {want}, got {named:?}"
        );
    }
    for p in &health {
        assert_ne!(
            p.graph_id, "index",
            "the id->path registry is not a project graph"
        );
        assert_eq!(p.total, 1, "each probe project holds exactly one memory");
        assert!(p.active <= p.total);
    }

    // And the aggregate health rolls the project memories up for ambient.
    let rolled = crate::ambient::gather_memory_graph_health(&blind);
    assert!(
        rolled.total >= 2,
        "ambient's aggregate health must include per-project memories, got {}",
        rolled.total
    );
    assert_eq!(rolled.projects.len(), health.len());

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn test_scheduled_queue_items_accessor() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let mut queue = ScheduledQueue::load(path);

    queue.push(ScheduledItem {
        id: "s1".into(),
        scheduled_for: Utc::now(),
        context: "test item".into(),
        priority: Priority::Normal,
        target: ScheduleTarget::Ambient,
        created_by_session: "test".into(),
        created_at: Utc::now(),
        project: None,
        working_dir: None,
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    });

    let items = queue.items();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "s1");
}

/// With auto-approve on, `request_permission` is a no-op that always says yes,
/// so the agent's own judgement is the ONLY remaining check on destructive
/// work. That makes this prompt section a safety control, not documentation:
/// if it silently stops being emitted, the agent keeps calling
/// request_permission believing a human might say no, and nothing ever
/// refuses. Assert it appears exactly when the flag is on.
#[test]
fn auto_approve_prompt_warns_that_no_human_will_answer() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_flag = std::env::var_os("JCODE_AMBIENT_AUTO_APPROVE");
    jcode_base::env::set_var("JCODE_HOME", temp.path());

    let state = AmbientState::default();
    let queue = vec![];
    let health = MemoryGraphHealth::default();
    let sessions = vec![];
    let feedback: Vec<String> = vec![];
    let budget = ResourceBudget {
        provider: "anthropic-oauth".into(),
        tokens_remaining_desc: "unknown".into(),
        window_resets_desc: "unknown".into(),
        user_usage_rate_desc: "0 tokens/min".into(),
        cycle_budget_desc: "stay under 50k tokens".into(),
    };

    jcode_base::env::set_var("JCODE_AMBIENT_AUTO_APPROVE", "true");
    crate::config::invalidate_config_cache();
    let on = build_ambient_system_prompt(&state, &queue, &health, &sessions, &feedback, &budget, 0);

    assert!(
        on.contains("## Permissions"),
        "auto-approve must add a Permissions section"
    );
    assert!(
        on.contains("nobody is on the other end"),
        "the agent must be told its requests reach no human, or it will treat \
         request_permission as a safety net that does not exist"
    );
    for hazard in ["force-push", "merge", "destructive"] {
        assert!(
            on.contains(hazard),
            "the irreversible-action warning must still name '{hazard}'"
        );
    }

    jcode_base::env::set_var("JCODE_AMBIENT_AUTO_APPROVE", "false");
    crate::config::invalidate_config_cache();
    let off =
        build_ambient_system_prompt(&state, &queue, &health, &sessions, &feedback, &budget, 0);
    assert!(
        !off.contains("nobody is on the other end"),
        "with approvals off a human DOES answer; telling the agent otherwise \
         would wrongly discourage it from asking"
    );

    match prev_flag {
        Some(v) => jcode_base::env::set_var("JCODE_AMBIENT_AUTO_APPROVE", v),
        None => jcode_base::env::remove_var("JCODE_AMBIENT_AUTO_APPROVE"),
    }
    match prev_home {
        Some(v) => jcode_base::env::set_var("JCODE_HOME", v),
        None => jcode_base::env::remove_var("JCODE_HOME"),
    }
    crate::config::invalidate_config_cache();
}

/// `ambient.proactive_work` must actually reach the prompt.
///
/// It previously did not. The runner read the field nowhere, and the prompt
/// hardcoded "only if enabled" with nothing ever substituting whether it was:
/// a config knob with no consumer. The agent was told to evaluate a condition
/// it had no access to, so it stayed cautious and mostly gardened, which is
/// what "the agent does not do much" looked like from outside.
#[test]
fn proactive_work_setting_reaches_the_prompt() {
    fn prompt_with(enabled: bool) -> String {
        let _guard = jcode_base::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("temp dir");
        jcode_base::env::set_var("JCODE_HOME", temp.path());
        jcode_base::env::set_var(
            "JCODE_AMBIENT_PROACTIVE",
            if enabled { "true" } else { "false" },
        );
        jcode_base::config::invalidate_config_cache();

        let budget = ResourceBudget {
            provider: "anthropic-oauth".into(),
            tokens_remaining_desc: "unknown".into(),
            window_resets_desc: "unknown".into(),
            user_usage_rate_desc: "0 tokens/min".into(),
            cycle_budget_desc: "stay under 50k tokens".into(),
        };
        let out = build_ambient_system_prompt(
            &AmbientState::default(),
            &[],
            &MemoryGraphHealth::default(),
            &[],
            &[],
            &budget,
            0,
        );
        jcode_base::env::remove_var("JCODE_AMBIENT_PROACTIVE");
        jcode_base::config::invalidate_config_cache();
        out
    }

    let enabled = prompt_with(true);
    assert!(
        enabled.contains("is ENABLED"),
        "an enabled setting must be stated in the prompt"
    );
    assert!(
        !enabled.contains("garden-only"),
        "an enabled cycle must not be told it is garden-only"
    );

    let disabled = prompt_with(false);
    assert!(
        disabled.contains("is DISABLED") && disabled.contains("garden-only"),
        "a disabled setting must be stated too, so the agent stops guessing"
    );

    assert_ne!(
        enabled, disabled,
        "the flag must change the prompt; identical output means it is inert"
    );
}

#[test]
fn ambient_prompt_ranks_configured_priority_projects_above_busier_ones() {
    // The whole point of the knob: the priority project must win even when a
    // different project has more recent sessions, since session count measures
    // where the user has been, not what matters.
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let session = |id: &str, dir: &str| RecentSessionInfo {
        id: id.into(),
        status: "closed".into(),
        topic: Some("work".into()),
        duration_secs: 60,
        extraction_status: "extracted".into(),
        working_dir: Some(dir.into()),
    };
    // jcode is busier: three sessions against private_project's one.
    let sessions = vec![
        session("s1", "/home/potb/jcode"),
        session("s2", "/home/potb/jcode"),
        session("s3", "/home/potb/jcode/crates/jcode-tui"),
        session("s4", "/home/potb/projects/workspace/private_project"),
    ];

    let render = |extra: &str| {
        std::fs::write(
            temp.path().join("config.toml"),
            format!("[ambient]\nenabled = true\n{extra}"),
        )
        .expect("write config");
        crate::config::invalidate_config_cache();
        build_ambient_system_prompt(
            &AmbientState::default(),
            &[],
            &MemoryGraphHealth::default(),
            &sessions,
            &[],
            &ResourceBudget::default(),
            0,
        )
    };

    let prioritized = render(
        "project_priority = [\"/home/potb/projects/workspace/private_project\", \"/home/potb/jcode\"]\n",
    );
    let section = prioritized
        .split("## Projects Active Recently")
        .nth(1)
        .expect("projects section");
    let private_project = section
        .find("/home/potb/projects/workspace/private_project")
        .expect("private_project listed");
    let jcode = section.find("/home/potb/jcode (").expect("jcode listed");
    assert!(
        private_project < jcode,
        "private_project is the configured first priority, so it must outrank the \
         busier jcode project; got section:\n{section}"
    );
    assert!(
        section.contains("/home/potb/projects/workspace/private_project (1 session(s)) [priority]"),
        "priority projects must be marked so the agent can act on the ranking"
    );
    assert!(
        prioritized.contains("exhaust useful work in a higher-priority project"),
        "an ordering the agent is not told to honour is just cosmetics"
    );

    // A session in a subdirectory belongs to its project.
    assert!(
        section.contains("/home/potb/jcode/crates/jcode-tui (1 session(s)) [priority]"),
        "a subdirectory session must inherit its project's priority"
    );

    // Without the config, activity order rules and nothing is tagged.
    let unset = render("");
    let section = unset
        .split("## Projects Active Recently")
        .nth(1)
        .expect("projects section");
    assert!(
        !section.contains("[priority]"),
        "with no priority configured the prompt must not invent one"
    );
    assert!(!unset.contains("exhaust useful work in a higher-priority project"));
    let private_project = section
        .find("/home/potb/projects/workspace/private_project")
        .expect("private_project listed");
    let jcode = section.find("/home/potb/jcode (").expect("jcode listed");
    assert!(
        jcode < private_project,
        "unconfigured, the busier project still sorts first"
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

#[test]
fn ambient_prompt_surfaces_a_priority_project_with_no_recent_sessions() {
    // The neglected important project is exactly the case the knob exists for.
    // Listing only "projects active recently" would hide it entirely.
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let sessions = vec![RecentSessionInfo {
        id: "s1".into(),
        status: "closed".into(),
        topic: Some("work".into()),
        duration_secs: 60,
        extraction_status: "extracted".into(),
        working_dir: Some("/home/potb/jcode".into()),
    }];

    std::fs::write(
        temp.path().join("config.toml"),
        "[ambient]\nenabled = true\nproject_priority = [\"/home/potb/projects/workspace/private_project\"]\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let prompt = build_ambient_system_prompt(
        &AmbientState::default(),
        &[],
        &MemoryGraphHealth::default(),
        &sessions,
        &[],
        &ResourceBudget::default(),
        0,
    );

    assert!(
        prompt.contains("## Priority Projects With No Recent Sessions"),
        "an idle priority project must still be surfaced"
    );
    let idle = prompt
        .split("## Priority Projects With No Recent Sessions")
        .nth(1)
        .expect("idle section");
    assert!(idle.contains("/home/potb/projects/workspace/private_project"));
    assert!(
        idle.contains("No recent session does not mean no work"),
        "the agent must be told why an idle project still deserves a look"
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

#[test]
fn priority_matching_respects_path_boundaries() {
    // `/home/potb/jcode-cron` is a different project from `/home/potb/jcode`.
    // A naive prefix test silently merges them.
    let priority = vec!["/home/potb/jcode".to_string()];
    assert_eq!(
        crate::ambient::prompt::priority_rank(&priority, "/home/potb/jcode"),
        0
    );
    assert_eq!(
        crate::ambient::prompt::priority_rank(&priority, "/home/potb/jcode/crates/x"),
        0,
        "a subdirectory belongs to its project"
    );
    assert_eq!(
        crate::ambient::prompt::priority_rank(&priority, "/home/potb/jcode/"),
        0,
        "a trailing slash is the same directory"
    );
    assert_eq!(
        crate::ambient::prompt::priority_rank(&priority, "/home/potb/jcode-cron"),
        usize::MAX,
        "a sibling sharing a name prefix is a different project"
    );
    assert_eq!(
        crate::ambient::prompt::priority_rank(&priority, "/home/potb/other"),
        usize::MAX
    );
}

#[test]
fn ambient_prompt_scopes_the_pr_repo_override_to_its_own_repository() {
    // `pr_repo` names one repo. Told to send "every" PR there, ambient would
    // push a private_project branch to the jcode fork once it works across projects.
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    std::fs::write(
        temp.path().join("config.toml"),
        "[ambient]\nenabled = true\npr_repo = \"potb/jcode\"\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let prompt = build_ambient_system_prompt(
        &AmbientState::default(),
        &[],
        &MemoryGraphHealth::default(),
        &[],
        &[],
        &ResourceBudget::default(),
        0,
    );

    assert!(
        prompt.contains("For work in the `jcode` repository"),
        "the override must be scoped to the repo it names"
    );
    assert!(prompt.contains("gh pr create --repo potb/jcode"));
    assert!(
        prompt.contains("target its own `origin`"),
        "another project's PRs must not be routed to this fork"
    );
    assert!(
        !prompt.contains("Open every pull request against"),
        "the unscoped instruction is the bug being fixed"
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// Config booleans can only say whether to work, never what the user wants
/// done. The instructions file is where that intent lives, and it has to
/// outrank the caution the agent talks itself into across cycles.
#[test]
fn ambient_prompt_includes_the_users_standing_instructions() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    std::fs::write(
        temp.path().join("config.toml"),
        "[ambient]\nenabled = true\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let render = || {
        build_ambient_system_prompt(
            &AmbientState::default(),
            &[],
            &MemoryGraphHealth::default(),
            &[],
            &[],
            &ResourceBudget::default(),
            0,
        )
    };

    let without = render();
    assert!(
        !without.contains("## Standing Instructions From The User"),
        "with no instructions file the prompt must not invent the section"
    );

    std::fs::write(
        temp.path()
            .join(crate::ambient::prompt::AMBIENT_INSTRUCTIONS_FILE),
        "Ship at least one PR per day. Do not ask, just do it.",
    )
    .expect("write instructions");

    let with = render();
    assert!(
        with.contains("Ship at least one PR per day"),
        "the user's own words must reach the prompt verbatim"
    );
    assert!(
        with.contains("OUTRANK"),
        "instructions must be stated as outranking self-written caution"
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// Per-project instructions are the user talking to their own agent. Writing
/// them into the project would put them in diffs, reviews and other people's
/// checkouts, so they stay under ~/.jcode and never touch the repo.
#[test]
fn project_instructions_live_under_jcode_home_not_in_the_project() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let path = crate::ambient::prompt::project_instructions_path("/home/user/work/api")
        .expect("instructions path");
    assert!(
        path.starts_with(temp.path()),
        "instructions must live under JCODE_HOME, got {}",
        path.display()
    );
    assert!(
        !path.to_string_lossy().contains("/home/user/work/api/"),
        "nothing may be written inside the project directory"
    );

    // Same basename, different projects: these must not share a file.
    let a = crate::ambient::prompt::project_instructions_slug("/home/user/work/api");
    let b = crate::ambient::prompt::project_instructions_slug("/home/user/personal/api");
    assert_ne!(a, b, "the full path must disambiguate identical basenames");
    // A trailing slash is the same project, not a second one.
    assert_eq!(
        a,
        crate::ambient::prompt::project_instructions_slug("/home/user/work/api/")
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// The instructions have to arrive before the agent picks a project, so they
/// are rendered for recently-seen projects rather than on demand afterwards.
#[test]
fn per_project_instructions_render_for_recently_seen_projects() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    std::fs::write(
        temp.path().join("config.toml"),
        "[ambient]\nenabled = true\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let path = crate::ambient::prompt::project_instructions_path("/home/user/work/api")
        .expect("instructions path");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, "Keep the migration tests green.").expect("write");

    let sessions = vec![RecentSessionInfo {
        id: "session_a".into(),
        status: "closed".into(),
        topic: Some("api work".into()),
        duration_secs: 300,
        extraction_status: "extracted".into(),
        working_dir: Some("/home/user/work/api".into()),
    }];
    let prompt = build_ambient_system_prompt(
        &AmbientState::default(),
        &[],
        &MemoryGraphHealth::default(),
        &sessions,
        &[],
        &ResourceBudget::default(),
        0,
    );
    assert!(prompt.contains("## Per-Project Standing Instructions"));
    assert!(prompt.contains("Keep the migration tests green."));
    assert!(prompt.contains("### /home/user/work/api"));

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// A quiet top-priority project used to end the whole cycle: the agent checked
/// project 1, found nothing to do, and stopped while the user's other listed
/// projects were never examined. The priority list is an order to walk.
#[test]
fn ambient_prompt_tells_the_agent_to_walk_down_the_priority_list() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let render = |extra: &str| {
        std::fs::write(
            temp.path().join("config.toml"),
            format!("[ambient]\nenabled = true\n{extra}"),
        )
        .expect("write config");
        crate::config::invalidate_config_cache();
        build_ambient_system_prompt(
            &AmbientState::default(),
            &[],
            &MemoryGraphHealth::default(),
            &[],
            &[],
            &ResourceBudget::default(),
            0,
        )
    };

    let with_priority = render(
        "proactive_work = true\nproject_priority = [\"/home/potb/jcode\", \
         \"/home/potb/projects/workspace/private_project\"]\n",
    );
    assert!(
        with_priority.contains("Do Not Stop At The First Quiet Project"),
        "the walk-the-list rule must reach the prompt, or the agent keeps \
         ending cycles at project 1"
    );
    let walk = with_priority
        .split("Do Not Stop At The First Quiet Project")
        .nth(1)
        .expect("walk section");
    let first = walk.find("1. /home/potb/jcode").expect("first listed");
    let second = walk
        .find("2. /home/potb/projects/workspace/private_project")
        .expect("second listed");
    assert!(
        first < second,
        "the list must be rendered in the user's configured order"
    );

    // Garden-only cycles are not supposed to go hunting for code work, so the
    // walk instruction must not contradict a disabled proactive_work.
    let garden_only = render("proactive_work = false\nproject_priority = [\"/home/potb/jcode\"]\n");
    assert!(
        !garden_only.contains("Do Not Stop At The First Quiet Project"),
        "a garden-only cycle must not be told to hunt for work across projects"
    );

    // No configured priority means there is no list to walk.
    let unconfigured = render("proactive_work = true\n");
    assert!(
        !unconfigured.contains("Do Not Stop At The First Quiet Project"),
        "without project_priority there is no order to walk"
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// `pr_repo` names one repository, which is wrong as soon as ambient rotates
/// across projects: without a project attached, one project's PR eventually
/// goes to another project's fork.
#[test]
fn ambient_prompt_names_a_pull_request_target_per_project() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    std::fs::write(
        temp.path().join("config.toml"),
        "[ambient]\nenabled = true\n\n[ambient.pr_repos]\n\
         \"/home/potb/jcode\" = \"potb/jcode\"\n\
         \"/home/potb/projects/workspace/private_project\" = \"potb/private_project\"\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let prompt = build_ambient_system_prompt(
        &AmbientState::default(),
        &[],
        &MemoryGraphHealth::default(),
        &[],
        &[],
        &ResourceBudget::default(),
        0,
    );

    let section = prompt.split("## Pull Requests").nth(1).expect("PR section");
    assert!(
        section.contains("`/home/potb/jcode`") && section.contains("--repo potb/jcode"),
        "each project must carry its own PR target; got:\n{section}"
    );
    assert!(
        section.contains("`/home/potb/projects/workspace/private_project`")
            && section.contains("--repo potb/private_project"),
        "the second project's target must be stated too, or its PRs land in the \
         first project's fork; got:\n{section}"
    );
    assert!(
        section.contains("origin"),
        "projects with no entry still need the origin fallback stated"
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// `[[ambient.projects]]` is an array of tables, and TOML preserves its order,
/// so the order written in the file is the priority order. Keeping rank and PR
/// target in one place means adding a repo is one edit, not two.
#[test]
fn ambient_projects_array_sets_priority_order_and_pr_targets() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let render = |config: &str| {
        std::fs::write(temp.path().join("config.toml"), config).expect("write config");
        crate::config::invalidate_config_cache();
        build_ambient_system_prompt(
            &AmbientState::default(),
            &[],
            &MemoryGraphHealth::default(),
            &[],
            &[],
            &ResourceBudget::default(),
            0,
        )
    };

    // private_project first, and it has NO pr_repo: the user pushes to it directly, so
    // its PRs must go to its own origin rather than an invented fork.
    let prompt = render(
        "[ambient]\nenabled = true\nproactive_work = true\n\n\
         [[ambient.projects]]\npath = \"/home/potb/projects/workspace/private_project\"\n\n\
         [[ambient.projects]]\npath = \"/home/potb/jcode\"\npr_repo = \"potb/jcode\"\n",
    );

    let walk = prompt
        .split("Do Not Stop At The First Quiet Project")
        .nth(1)
        .expect("walk section");
    let private_project = walk
        .find("1. /home/potb/projects/workspace/private_project")
        .expect("private_project first");
    let jcode = walk.find("2. /home/potb/jcode").expect("jcode second");
    assert!(
        private_project < jcode,
        "file order is the priority order; got:\n{walk}"
    );

    let prs = prompt.split("## Pull Requests").nth(1).expect("PR section");
    assert!(
        prs.contains("`/home/potb/jcode`") && prs.contains("--repo potb/jcode"),
        "a project's own pr_repo must be used for it; got:\n{prs}"
    );
    assert!(
        !prs.contains("private_project`: push the branch to the fork remote"),
        "a project with no pr_repo must fall through to its origin, not borrow \
         another project's fork; got:\n{prs}"
    );

    // The older split keys keep working, and a project named by both must not
    // be listed twice.
    let legacy = render(
        "[ambient]\nenabled = true\nproactive_work = true\n\
         project_priority = [\"/home/potb/jcode\"]\n\n\
         [ambient.pr_repos]\n\"/home/potb/jcode\" = \"potb/jcode\"\n",
    );
    let legacy_walk = legacy
        .split("Do Not Stop At The First Quiet Project")
        .nth(1)
        .expect("walk section");
    assert!(legacy_walk.contains("1. /home/potb/jcode"));
    assert!(
        !legacy_walk.contains("2. /home/potb/jcode"),
        "a project named by both old keys must appear once"
    );
    assert!(
        legacy
            .split("## Pull Requests")
            .nth(1)
            .expect("PR section")
            .contains("--repo potb/jcode"),
        "the old pr_repos map must still route PRs"
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// The user's real setup: private_project with direct push access, jcode through a fork
/// where everything must go to the fork and never upstream. The prompt has to
/// separate the two, since the branch push differs as well as the PR target.
#[test]
fn ambient_prompt_separates_fork_projects_from_direct_access_projects() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    std::fs::write(
        temp.path().join("config.toml"),
        "[ambient]\nenabled = true\nproactive_work = true\n\n\
         [[ambient.projects]]\npath = \"/home/potb/jcode\"\npr_repo = \"potb/jcode\"\n\n\
         [[ambient.projects]]\npath = \"/home/potb/projects/workspace/private_project\"\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let prompt = build_ambient_system_prompt(
        &AmbientState::default(),
        &[],
        &MemoryGraphHealth::default(),
        &[],
        &[],
        &ResourceBudget::default(),
        0,
    );
    let prs = prompt.split("## Pull Requests").nth(1).expect("PR section");
    // Stop at the next section so later prose (which legitimately lists both
    // projects) cannot satisfy these assertions.
    let prs = prs.split("\n## ").next().expect("PR section body");

    let fork_idx = prs.find("FORK PROJECTS").expect("fork flow stated");
    let direct_idx = prs
        .find("DIRECT-ACCESS PROJECTS")
        .expect("direct flow stated");

    let fork_block = &prs[fork_idx..direct_idx];
    assert!(
        fork_block.contains("/home/potb/jcode") && fork_block.contains("--repo potb/jcode"),
        "the forked project belongs to the fork flow; got:\n{fork_block}"
    );
    assert!(
        !fork_block.contains("private_project"),
        "a direct-access project must not be described as a fork project"
    );

    let direct_block = &prs[direct_idx..];
    assert!(
        direct_block.contains("/home/potb/projects/workspace/private_project"),
        "the direct-access project belongs to the direct flow; got:\n{direct_block}"
    );
    assert!(
        !direct_block.contains("potb/jcode"),
        "the direct-access project must not be routed through another \
         project's fork; got:\n{direct_block}"
    );

    // Branch destination, not just the PR: pushing a fork project's branch to
    // upstream fails on permissions and strands the work.
    assert!(
        fork_block.contains("push the branch to the fork remote"),
        "the fork flow must say where the BRANCH goes; got:\n{fork_block}"
    );
    assert!(
        direct_block.contains("push the branch to `origin`"),
        "the direct flow must say where the BRANCH goes; got:\n{direct_block}"
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// Per-project instructions lived only in `~/.jcode/ambient/instructions/`
/// under a flattened-path filename, so nothing in the config revealed they
/// existed. They must be declarable from config, inline or by file reference.
#[test]
fn per_project_instructions_can_be_declared_in_config() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let instructions_dir = temp.path().join("ambient").join("instructions");
    std::fs::create_dir_all(&instructions_dir).expect("instructions dir");
    std::fs::write(
        instructions_dir.join("private_project.md"),
        "Production SaaS. Never touch migrations.",
    )
    .expect("write instructions file");

    let render = |config: &str| {
        std::fs::write(temp.path().join("config.toml"), config).expect("write config");
        crate::config::invalidate_config_cache();
        build_ambient_system_prompt(
            &AmbientState::default(),
            &[],
            &MemoryGraphHealth::default(),
            &[],
            &[],
            &ResourceBudget::default(),
            0,
        )
    };

    let prompt = render(
        "[ambient]\nenabled = true\n\n\
         [[ambient.projects]]\npath = \"/home/potb/jcode\"\n\
         instructions = \"Always work in a git worktree.\"\n\n\
         [[ambient.projects]]\npath = \"/home/potb/projects/workspace/private_project\"\n\
         instructions_file = \"private_project.md\"\n",
    );

    let section = prompt
        .split("## Per-Project Standing Instructions")
        .nth(1)
        .expect("per-project section");
    assert!(
        section.contains("### /home/potb/jcode")
            && section.contains("Always work in a git worktree."),
        "inline instructions must reach the prompt; got:\n{section}"
    );
    assert!(
        section.contains("### /home/potb/projects/workspace/private_project")
            && section.contains("Never touch migrations."),
        "a referenced instructions file must be loaded; got:\n{section}"
    );

    // The legacy slug-named file must keep working for projects that have not
    // been migrated.
    std::fs::write(
        instructions_dir.join("home-potb-legacy.md"),
        "Legacy rules still apply.",
    )
    .expect("write legacy file");
    let legacy = render(
        "[ambient]\nenabled = true\n\n\
         [[ambient.projects]]\npath = \"/home/potb/legacy\"\n",
    );
    assert!(
        legacy.contains("Legacy rules still apply."),
        "an unmigrated project must keep its slug-named instructions file"
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// One global `active_windows` forces the strictest project's hours onto every
/// other project, costing every cycle in between. Windows belong per project.
#[test]
fn per_project_active_windows_gate_only_their_own_project() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    // A window that is open now, and one that never overlaps it, expressed
    // against the current local weekday so the test does not depend on when it
    // runs.
    let now = chrono::Local::now();
    let day = match chrono::Datelike::weekday(&now) {
        chrono::Weekday::Mon => "mon",
        chrono::Weekday::Tue => "tue",
        chrono::Weekday::Wed => "wed",
        chrono::Weekday::Thu => "thu",
        chrono::Weekday::Fri => "fri",
        chrono::Weekday::Sat => "sat",
        chrono::Weekday::Sun => "sun",
    };
    let open_spec = format!("{day} 00:00-23:59");
    let hour = chrono::Timelike::hour(&now);
    // A one-hour window that cannot contain "now".
    let closed_start = (hour + 2) % 24;
    let closed_spec = format!("{day} {closed_start:02}:00-{closed_start:02}:30");

    let render = |config: &str| {
        std::fs::write(temp.path().join("config.toml"), config).expect("write config");
        crate::config::invalidate_config_cache();
        build_ambient_system_prompt(
            &AmbientState::default(),
            &[],
            &MemoryGraphHealth::default(),
            &[],
            &[],
            &ResourceBudget::default(),
            0,
        )
    };

    let prompt = render(&format!(
        "[ambient]\nenabled = true\nproactive_work = true\n\n\
         [[ambient.projects]]\npath = \"/home/potb/jcode\"\n\n\
         [[ambient.projects]]\npath = \"/home/potb/private_project\"\n\
         active_windows = [\"{closed_spec}\"]\n"
    ));

    let walk = prompt
        .split("Do Not Stop At The First Quiet Project")
        .nth(1)
        .expect("walk section");
    assert!(
        walk.contains("1. /home/potb/jcode"),
        "a project with no window of its own is always workable; got:\n{walk}"
    );
    assert!(
        !walk.contains("2. /home/potb/private_project"),
        "a project outside its own window must not be offered as work; got:\n{walk}"
    );
    assert!(
        walk.contains("/home/potb/private_project (allowed:"),
        "a project held back by its schedule must be named as such, or its \
         absence reads as 'finished'; got:\n{walk}"
    );

    // Its own window being open puts it back in the rotation.
    let open_prompt = render(&format!(
        "[ambient]\nenabled = true\nproactive_work = true\n\n\
         [[ambient.projects]]\npath = \"/home/potb/jcode\"\n\n\
         [[ambient.projects]]\npath = \"/home/potb/private_project\"\n\
         active_windows = [\"{open_spec}\"]\n"
    ));
    assert!(
        open_prompt
            .split("Do Not Stop At The First Quiet Project")
            .nth(1)
            .expect("walk section")
            .contains("2. /home/potb/private_project"),
        "an open per-project window must not exclude the project"
    );

    // The global escape hatch covers per-project schedules too, or it would
    // only half-mean "run anytime".
    let ignored = render(&format!(
        "[ambient]\nenabled = true\nproactive_work = true\n\
         ignore_active_windows = true\n\n\
         [[ambient.projects]]\npath = \"/home/potb/jcode\"\n\n\
         [[ambient.projects]]\npath = \"/home/potb/private_project\"\n\
         active_windows = [\"{closed_spec}\"]\n"
    ));
    assert!(
        ignored
            .split("Do Not Stop At The First Quiet Project")
            .nth(1)
            .expect("walk section")
            .contains("2. /home/potb/private_project"),
        "ignore_active_windows must also ignore per-project windows"
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// A session started in a subdirectory belongs to its project, so its
/// instructions must render under the project root. Observed live: a session in
/// `<project>/crates/jcode-app-core` produced a heading naming that
/// subdirectory as though it were the project.
#[test]
fn per_project_instructions_render_under_the_project_root() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    std::fs::write(
        temp.path().join("config.toml"),
        "[ambient]\nenabled = true\n\n\
         [[ambient.projects]]\npath = \"/home/potb/jcode\"\n\
         instructions = \"Always work in a git worktree.\"\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let session = |id: &str, dir: &str| RecentSessionInfo {
        id: id.into(),
        status: "closed".into(),
        topic: Some("work".into()),
        duration_secs: 60,
        extraction_status: "extracted".into(),
        working_dir: Some(dir.into()),
    };
    let sessions = vec![
        session("s1", "/home/potb/jcode/crates/jcode-app-core"),
        session("s2", "/home/potb/jcode"),
    ];

    let prompt = build_ambient_system_prompt(
        &AmbientState::default(),
        &[],
        &MemoryGraphHealth::default(),
        &sessions,
        &[],
        &ResourceBudget::default(),
        0,
    );
    let section = prompt
        .split("## Per-Project Standing Instructions")
        .nth(1)
        .expect("per-project section");

    assert!(
        section.contains("### /home/potb/jcode\n"),
        "instructions belong under the project root; got:\n{section}"
    );
    assert!(
        !section.contains("### /home/potb/jcode/crates"),
        "a subdirectory must not be presented as its own project; got:\n{section}"
    );
    assert_eq!(
        section.matches("Always work in a git worktree.").count(),
        1,
        "the project's rules must appear once, not once per subdirectory seen; \
         got:\n{section}"
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}

/// A cycle gets its whole system prompt from `system_prompt_override`, so the
/// "Available Skills" catalogue an interactive session receives never reached
/// it: the `skill_manage` tool was registered and the agent had no way to know
/// what it could load. An installed skill is only useful if it is named.
#[test]
fn ambient_prompt_lists_installed_skills_and_how_to_load_them() {
    let skills = vec![
        crate::prompt::SkillInfo {
            name: "gh-stack".into(),
            description: "Manage stacked branches and pull requests with the gh-stack CLI.".into(),
        },
        crate::prompt::SkillInfo {
            name: "garden-memory".into(),
            description: "Consolidate and prune the memory graph.".into(),
        },
    ];

    let mut prompt = String::new();
    crate::ambient::prompt::append_available_skills(&mut prompt, &skills);

    assert!(prompt.contains("# Available Skills"));
    assert!(prompt.contains("`/gh-stack `"));
    assert!(prompt.contains("`/garden-memory `"));
    // Nobody types slash commands at an unattended cycle, so the catalogue is
    // useless without the tool call that actually loads one.
    assert!(prompt.contains("skill_manage"));
    assert!(prompt.contains("action=\"load\""));
}

/// With no skills installed the section is omitted entirely rather than
/// rendered as an empty heading, which would read as "skills exist but none
/// apply" and waste prompt budget on every cycle.
#[test]
fn ambient_prompt_omits_skills_section_when_none_installed() {
    let mut prompt = String::new();
    crate::ambient::prompt::append_available_skills(&mut prompt, &[]);
    assert!(prompt.is_empty());
}

/// Project identity must be canonical and boundary-safe: a queue item created
/// in a subdirectory belongs to its project, a `~` path and a trailing slash
/// resolve to the same key, and a sibling sharing a name prefix does not.
///
/// This is the property everything later in issue #126 is partitioned by, so a
/// wrong answer here silently mixes two projects' state, queues and locks.
#[test]
fn scheduled_item_project_key_is_canonical_and_boundary_safe() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_user_home = std::env::var_os("HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    crate::env::set_var("HOME", "/home/potb");
    std::fs::write(
        temp.path().join("config.toml"),
        "[ambient]\nenabled = true\n\n\
         [[ambient.projects]]\npath = \"~/jcode\"\n\n\
         [[ambient.projects]]\npath = \"/home/potb/projects/costo/beakon\"\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let key = |dir: Option<&str>| crate::ambient::prompt::resolve_project_key(dir);

    assert_eq!(
        key(Some("/home/potb/jcode")).as_deref(),
        Some("/home/potb/jcode"),
        "a configured `~` path must resolve to its expanded canonical form"
    );
    assert_eq!(
        key(Some("/home/potb/jcode/crates/jcode-app-core")).as_deref(),
        Some("/home/potb/jcode"),
        "a subdirectory must key to the project, not to itself"
    );
    assert_eq!(
        key(Some("/home/potb/jcode/")).as_deref(),
        Some("/home/potb/jcode"),
        "a trailing slash is the same directory"
    );
    assert_eq!(
        key(Some("/home/potb/jcode-cron")),
        None,
        "a sibling sharing a name prefix is a different project"
    );
    assert_eq!(key(Some("/home/potb")), None, "a parent is not the project");
    assert_eq!(key(None), None);
    assert_eq!(key(Some("   ")), None, "a blank working dir owns nothing");
    assert_eq!(
        key(Some("/home/potb/projects/costo/beakon")).as_deref(),
        Some("/home/potb/projects/costo/beakon"),
        "each configured project resolves to itself"
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    if let Some(prev) = prev_user_home {
        crate::env::set_var("HOME", prev);
    }
    crate::config::invalidate_config_cache();
}

/// A `queue.json` written before `project` existed must keep loading, and its
/// items must still be attributable: the acceptance criterion on #126 says
/// migrate, not discard. `project_key()` falls back to resolving `working_dir`,
/// so an old item answers the same as a freshly scheduled one.
#[test]
fn legacy_queue_items_without_a_project_field_still_resolve_one() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_user_home = std::env::var_os("HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    crate::env::set_var("HOME", "/home/potb");
    std::fs::write(
        temp.path().join("config.toml"),
        "[ambient]\nenabled = true\n\n[[ambient.projects]]\npath = \"/home/potb/jcode\"\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let queue_path = temp.path().join("legacy_queue.json");
    std::fs::write(
        &queue_path,
        r#"[{"id":"sched_old","scheduled_for":"2026-01-01T00:00:00Z",
             "context":"queued by an older build","priority":"Normal",
             "created_by_session":"s1","created_at":"2026-01-01T00:00:00Z",
             "working_dir":"/home/potb/jcode/crates"}]"#,
    )
    .expect("write legacy queue");

    let queue = ScheduledQueue::load(queue_path);
    assert_eq!(queue.len(), 1, "a pre-field queue must still load");
    let item = &queue.items()[0];
    assert_eq!(item.project, None, "the file has no stored project");
    assert_eq!(
        item.project_key().as_deref(),
        Some("/home/potb/jcode"),
        "an old item must still be attributable via its working_dir"
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    if let Some(prev) = prev_user_home {
        crate::env::set_var("HOME", prev);
    }
    crate::config::invalidate_config_cache();
}

/// Scheduling stamps the project onto the item. Resolving lazily on every read
/// would let an item change owner when the config changes under it, so the key
/// is recorded once, at the moment the item is created.
#[test]
fn scheduling_stamps_the_project_onto_the_queued_item() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    std::fs::write(
        temp.path().join("config.toml"),
        "[ambient]\nenabled = true\n\n[[ambient.projects]]\npath = \"/home/potb/jcode\"\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();

    let mut manager = crate::ambient::AmbientManager::new().expect("manager");
    let request = |working_dir: Option<&str>| ScheduleRequest {
        wake_in_minutes: Some(30),
        wake_at: None,
        context: "later".into(),
        priority: Priority::Normal,
        target: ScheduleTarget::Ambient,
        created_by_session: "s1".into(),
        working_dir: working_dir.map(ToOwned::to_owned),
        task_description: None,
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    };

    let in_project = manager
        .schedule(request(Some("/home/potb/jcode/crates/jcode-tui")))
        .expect("schedule");
    let elsewhere = manager
        .schedule(request(Some("/tmp/unconfigured")))
        .expect("schedule");

    let find = |id: &str| {
        manager
            .queue()
            .items()
            .iter()
            .find(|item| item.id == id)
            .expect("queued item")
            .clone()
    };
    assert_eq!(
        find(&in_project).project.as_deref(),
        Some("/home/potb/jcode"),
        "a project item must carry its canonical key from the moment it is queued"
    );
    assert_eq!(
        find(&elsewhere).project,
        None,
        "work outside every configured project belongs to no project"
    );

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::invalidate_config_cache();
}
