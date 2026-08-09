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
        configured.contains("Never open a PR against the upstream"),
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
    let entry = crate::memory::MemoryEntry::new(
        crate::memory::MemoryCategory::Fact,
        "phantom probe",
    );
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
    let named: Vec<String> = health.iter().filter_map(|p| p.working_dir.clone()).collect();
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
