use super::*;
use crate::bus::{BackgroundTaskProgressKind, BackgroundTaskProgressSource, BusEvent};
use anyhow::anyhow;
use tempfile::tempdir;
use tokio::time::{Duration, sleep};

#[tokio::test]
async fn spawn_with_notify_emits_started_ui_activity() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let mut bus_rx = Bus::global().subscribe();

    let info = manager
        .spawn_with_notify(
            "bash",
            Some("checks".to_string()),
            None,
            "session-started",
            true,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(10)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    for _ in 0..20 {
        let event = tokio::time::timeout(Duration::from_millis(200), bus_rx.recv())
            .await
            .map_err(|err| anyhow!("timed out waiting for UI activity event: {err}"))?
            .map_err(|err| anyhow!("bus should stay open: {err}"))?;
        if let BusEvent::UiActivity(activity) = event
            && activity.session_id.as_deref() == Some("session-started")
            && activity.message.contains(&info.task_id)
        {
            assert_eq!(activity.kind, crate::bus::UiActivityKind::Background);
            assert!(activity.message.contains("Background task started"));
            assert!(activity.message.contains("checks"));
            assert_eq!(
                activity.status_notice.as_deref(),
                Some("Background task started · checks")
            );
            return Ok(());
        }
    }

    Err(anyhow!(
        "started UI activity event for task {} not received",
        info.task_id
    ))
}

#[tokio::test]
async fn update_delivery_applies_to_running_task_completion() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            None,
            "session-test",
            false,
            false,
            |output_path| async move {
                sleep(Duration::from_millis(25)).await;
                tokio::fs::write(&output_path, "hello").await?;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    let updated = manager
        .update_delivery(&info.task_id, true, true)
        .await
        .map_err(|err| anyhow!("update delivery should succeed: {err}"))?
        .ok_or_else(|| anyhow!("task should exist"))?;
    assert!(updated.notify);
    assert!(updated.wake);

    for _ in 0..40 {
        let status = manager
            .status(&info.task_id)
            .await
            .ok_or_else(|| anyhow!("status should exist"))?;
        if status.status != BackgroundTaskStatus::Running {
            assert!(status.notify);
            assert!(status.wake);
            assert_eq!(status.status, BackgroundTaskStatus::Completed);
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }

    Err(anyhow!("background task did not complete in time"))
}

#[tokio::test]
async fn update_progress_persists_status_and_emits_bus_event() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            None,
            "session-progress",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(50)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    let progress = BackgroundTaskProgress {
        kind: BackgroundTaskProgressKind::Determinate,
        percent: Some(42.0),
        message: Some("Running checks".to_string()),
        current: Some(21),
        total: Some(50),
        unit: Some("tests".to_string()),
        eta_seconds: Some(8),
        updated_at: Utc::now().to_rfc3339(),
        source: BackgroundTaskProgressSource::Reported,
    };

    let mut bus_rx = Bus::global().subscribe();
    let updated = manager
        .update_progress(&info.task_id, progress.clone())
        .await
        .map_err(|err| anyhow!("update progress should succeed: {err}"))?
        .ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(updated.progress, Some(progress.clone().normalize()));

    for _ in 0..20 {
        let event = tokio::time::timeout(Duration::from_millis(200), bus_rx.recv())
            .await
            .map_err(|err| anyhow!("timed out waiting for progress event: {err}"))?
            .map_err(|err| anyhow!("bus should stay open: {err}"))?;
        if let BusEvent::BackgroundTaskProgress(event) = event
            && event.task_id == info.task_id
        {
            assert_eq!(event.session_id, "session-progress");
            assert_eq!(event.progress, progress.normalize());
            return Ok(());
        }
    }

    Err(anyhow!(
        "progress event for task {} not received",
        info.task_id
    ))
}

#[tokio::test]
async fn wait_returns_when_task_finishes() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            None,
            "session-wait-finish",
            false,
            false,
            |output_path| async move {
                sleep(Duration::from_millis(25)).await;
                tokio::fs::write(&output_path, "done").await?;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    let wait_result = manager
        .wait(&info.task_id, Duration::from_secs(2), true)
        .await
        .ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Finished);
    assert_eq!(wait_result.task.status, BackgroundTaskStatus::Completed);
    assert_eq!(wait_result.task.exit_code, Some(0));
    Ok(())
}

#[tokio::test]
async fn wait_returns_on_progress_checkpoint() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            None,
            "session-wait-progress",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_secs(2)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    let progress = BackgroundTaskProgress {
        kind: BackgroundTaskProgressKind::Determinate,
        percent: Some(25.0),
        message: Some("checkpoint".to_string()),
        current: Some(1),
        total: Some(4),
        unit: Some("steps".to_string()),
        eta_seconds: Some(3),
        updated_at: Utc::now().to_rfc3339(),
        source: BackgroundTaskProgressSource::Reported,
    };

    let waiter = manager.wait(&info.task_id, Duration::from_secs(2), true);
    let updater = async {
        sleep(Duration::from_millis(25)).await;
        manager
            .update_progress(&info.task_id, progress.clone())
            .await
            .map_err(|err| anyhow!("progress update should succeed: {err}"))?
            .ok_or_else(|| anyhow!("task should exist"))?;
        Result::<()>::Ok(())
    };
    let (wait_result, updater_result) = tokio::join!(waiter, updater);
    updater_result?;
    let wait_result = wait_result.ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Progress);
    assert_eq!(wait_result.task.status, BackgroundTaskStatus::Running);
    assert_eq!(wait_result.task.progress, Some(progress.normalize()));
    assert!(wait_result.progress_event.is_some());
    Ok(())
}

#[tokio::test]
async fn wait_returns_on_timeout() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            None,
            "session-wait-timeout",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(250)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    let wait_result = manager
        .wait(&info.task_id, Duration::from_millis(25), true)
        .await
        .ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Timeout);
    assert_eq!(wait_result.task.status, BackgroundTaskStatus::Running);
    Ok(())
}

fn running_status_fixture(task_id: &str, session_id: &str) -> TaskStatusFile {
    TaskStatusFile {
        task_id: task_id.to_string(),
        tool_name: "swarm".to_string(),
        display_name: None,
        command: None,
        session_id: session_id.to_string(),
        status: BackgroundTaskStatus::Running,
        exit_code: None,
        error: None,
        started_at: Utc::now().to_rfc3339(),
        completed_at: None,
        duration_secs: None,
        pid: None,
        owner_pid: None,
        owner_instance: None,
        detached: false,
        notify: false,
        wake: false,
        progress: None,
        event_history: Vec::new(),
    }
}

async fn write_status_fixture(manager: &BackgroundTaskManager, status: &TaskStatusFile) {
    let path = manager.status_path_for(&status.task_id);
    let json = serde_json::to_string_pretty(status).expect("serialize status fixture");
    tokio::fs::write(&path, json).await.expect("write fixture");
}

#[tokio::test]
async fn tasks_map_prunes_entry_after_natural_completion() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            None,
            "session-prune",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(10)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;
    assert!(
        manager.is_live_task(&info.task_id),
        "task should be live right after spawn"
    );

    for _ in 0..200 {
        let status = manager
            .status(&info.task_id)
            .await
            .ok_or_else(|| anyhow!("status should exist"))?;
        if status.status != BackgroundTaskStatus::Running && !manager.is_live_task(&info.task_id) {
            // Pruned only after the status file was finalized, so the live
            // map never claims a task whose status file is already terminal.
            let (running_count, labels, _) = manager.running_snapshot();
            assert_eq!(running_count, 0, "snapshot should not count finished tasks");
            assert!(labels.is_empty());
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }

    Err(anyhow!(
        "task {} was not pruned from the live map after completion",
        info.task_id
    ))
}

#[tokio::test]
async fn reconcile_marks_orphan_from_reloaded_process_failed() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // Same PID, different instance token: exactly what an exec-based server
    // reload leaves behind.
    let mut orphan = running_status_fixture("orphan1aaaa", "session-orphan");
    orphan.owner_pid = Some(std::process::id());
    orphan.owner_instance = Some("previous-process-image".to_string());
    write_status_fixture(&manager, &orphan).await;

    let reconciled = manager.reconcile_orphaned_tasks().await;
    assert_eq!(reconciled, 1);

    let status = manager
        .status("orphan1aaaa")
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);
    let error = status.error.unwrap_or_default();
    assert!(
        error.contains("orphaned") && error.contains("reloaded"),
        "error should explain the reload orphaning, got: {error}"
    );
    assert!(status.completed_at.is_some());
    Ok(())
}

#[tokio::test]
async fn reconcile_marks_orphan_from_dead_process_failed() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // A child process that has already exited and been reaped gives us a PID
    // that is provably not running.
    let mut child = std::process::Command::new("true")
        .spawn()
        .map_err(|err| anyhow!("spawn child: {err}"))?;
    let dead_pid = child.id();
    child.wait().map_err(|err| anyhow!("wait child: {err}"))?;

    let mut orphan = running_status_fixture("orphan2bbbb", "session-orphan-dead");
    orphan.owner_pid = Some(dead_pid);
    orphan.owner_instance = Some("some-dead-instance".to_string());
    write_status_fixture(&manager, &orphan).await;

    let reconciled = manager.reconcile_orphaned_tasks().await;
    assert_eq!(reconciled, 1);
    let status = manager
        .status("orphan2bbbb")
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);
    Ok(())
}

#[tokio::test]
async fn reconcile_leaves_non_orphans_alone() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // Owned by this exact process image: could still be bootstrapping.
    let mut own = running_status_fixture("keep1aaaa", "session-keep");
    own.owner_pid = Some(std::process::id());
    own.owner_instance = Some(model::process_instance_token().to_string());
    write_status_fixture(&manager, &own).await;

    // Legacy file without owner metadata: no safe liveness signal, leave it.
    let legacy = running_status_fixture("keep2bbbb", "session-keep");
    write_status_fixture(&manager, &legacy).await;

    // Owned by a live foreign process (PID 1 is always alive on Unix).
    let mut foreign = running_status_fixture("keep3cccc", "session-keep");
    foreign.owner_pid = Some(1);
    foreign.owner_instance = Some("init-instance".to_string());
    write_status_fixture(&manager, &foreign).await;

    // Detached with a live pid: reconciled by the detached path, not this one.
    let mut detached = running_status_fixture("keep4dddd", "session-keep");
    detached.detached = true;
    detached.pid = Some(std::process::id());
    write_status_fixture(&manager, &detached).await;

    let reconciled = manager.reconcile_orphaned_tasks().await;
    assert_eq!(reconciled, 0);

    for task_id in ["keep1aaaa", "keep2bbbb", "keep3cccc", "keep4dddd"] {
        let status = manager
            .status(task_id)
            .await
            .ok_or_else(|| anyhow!("status for {task_id} should exist"))?;
        assert_eq!(
            status.status,
            BackgroundTaskStatus::Running,
            "{task_id} should not be reconciled"
        );
    }
    Ok(())
}

#[tokio::test]
async fn status_read_self_heals_orphaned_task() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let mut orphan = running_status_fixture("orphan3cccc", "session-orphan-read");
    orphan.owner_pid = Some(std::process::id());
    orphan.owner_instance = Some("previous-process-image".to_string());
    write_status_fixture(&manager, &orphan).await;

    // A plain status read (used by bg status / bg wait) heals the phantom
    // without waiting for the startup sweep.
    let status = manager
        .status("orphan3cccc")
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);

    // And wait() returns immediately instead of blocking to timeout.
    let wait_result = manager
        .wait("orphan3cccc", Duration::from_secs(5), false)
        .await
        .ok_or_else(|| anyhow!("wait should find the task"))?;
    assert_eq!(
        wait_result.reason,
        BackgroundTaskWaitReason::AlreadyFinished
    );
    Ok(())
}

#[tokio::test]
async fn abort_live_tasks_for_reload_finalizes_running_tasks() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // A long-running task that would otherwise vanish across an exec reload.
    let info = manager
        .spawn_with_notify(
            "selfdev-build",
            Some("selfdev build".to_string()),
            None,
            "session-reload-abort",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_secs(60)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;
    assert!(manager.is_live_task(&info.task_id));

    let aborted = manager.abort_live_tasks_for_reload().await;
    assert_eq!(aborted, 1);
    assert!(
        !manager.is_live_task(&info.task_id),
        "live map should be drained"
    );

    let status = manager
        .status(&info.task_id)
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);
    let error = status.error.unwrap_or_default();
    assert!(
        error.contains("Interrupted by server reload"),
        "error should explain the reload interruption, got: {error}"
    );
    assert!(status.completed_at.is_some());

    // Idempotent: a second sweep finds nothing.
    assert_eq!(manager.abort_live_tasks_for_reload().await, 0);
    Ok(())
}

#[tokio::test]
async fn abort_live_tasks_for_reload_keeps_naturally_finished_status() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            None,
            "session-reload-finished",
            false,
            false,
            |_output_path| async move { Ok(TaskResult::completed(Some(0))) },
        )
        .await;

    // Let the task finish and write its terminal status.
    for _ in 0..200 {
        let status = manager
            .status(&info.task_id)
            .await
            .ok_or_else(|| anyhow!("status should exist"))?;
        if status.status != BackgroundTaskStatus::Running {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        manager.abort_live_tasks_for_reload().await,
        0,
        "a naturally completed task should not be counted as finalized"
    );

    let status = manager
        .status(&info.task_id)
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(
        status.status,
        BackgroundTaskStatus::Completed,
        "a task that finished before the sweep must keep its real status"
    );
    Ok(())
}

#[tokio::test]
async fn spawn_with_notify_persists_the_full_command_in_the_status_file() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let full_command =
        "cargo test -p jcode-base --all-features -- --nocapture --test-threads=1".to_string();

    let info = manager
        .spawn_with_notify(
            "bash",
            Some("cargo test".to_string()),
            Some(full_command.clone()),
            "session-command",
            false,
            false,
            |_output_path| async move { Ok(TaskResult::completed(Some(0))) },
        )
        .await;

    // The initial status file carries the verbatim command alongside the short
    // display name, so a reader never has to reconstruct it from the summary.
    let initial = manager
        .status(&info.task_id)
        .await
        .expect("status file should exist right after spawn");
    assert_eq!(initial.command.as_deref(), Some(full_command.as_str()));
    assert_eq!(initial.display_name.as_deref(), Some("cargo test"));

    // And it survives the terminal rewrite, so completed tasks stay inspectable.
    for _ in 0..50 {
        let status = manager
            .status(&info.task_id)
            .await
            .expect("status file should remain readable");
        if status.status != BackgroundTaskStatus::Running {
            assert_eq!(status.command.as_deref(), Some(full_command.as_str()));
            return Ok(());
        }
        sleep(Duration::from_millis(20)).await;
    }

    Err(anyhow!(
        "task {} never reached a terminal status",
        info.task_id
    ))
}

#[test]
fn task_status_file_without_a_command_field_still_deserializes() {
    // Status files written by older builds have no `command` key at all;
    // reading one must not fail, it must just report an unknown command.
    let json = serde_json::json!({
        "task_id": "legacy1",
        "tool_name": "bash",
        "display_name": "old task",
        "session_id": "s1",
        "status": "running",
        "exit_code": null,
        "error": null,
        "started_at": Utc::now().to_rfc3339(),
        "completed_at": null,
        "duration_secs": null,
    });

    let status: TaskStatusFile =
        serde_json::from_value(json).expect("legacy status files must stay readable");
    assert_eq!(status.command, None);
    assert_eq!(status.display_name.as_deref(), Some("old task"));
}

/// The TUI client renders in a different process from the server that actually
/// runs tools, so the background widget must see running tasks it does not own.
/// Simulate that split with two managers over one shared task directory.
#[tokio::test]
async fn running_snapshot_for_session_sees_tasks_owned_by_another_process() -> Result<()> {
    let tmp = tempdir()?;
    let server = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let client = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = server
        .spawn_with_notify(
            "bash",
            Some("long build".to_string()),
            None,
            "session-split",
            true,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(400)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    // The client owns no task futures, so the process-local snapshot is blind.
    let (local_count, _, _) = client.running_snapshot();
    assert_eq!(local_count, 0, "client process owns no tasks");

    let (count, labels, _) = client.running_snapshot_for_session("session-split");
    assert_eq!(count, 1, "client should see the server's running task");
    assert_eq!(labels, vec!["long build".to_string()]);

    // Tasks for other sessions must not leak into this session's widget.
    let (other_count, _, _) = client.running_snapshot_for_session("session-other");
    assert_eq!(other_count, 0);

    // The owning process must not double count its own task.
    let (server_count, _, _) = server.running_snapshot_for_session("session-split");
    assert_eq!(server_count, 1, "task should be counted exactly once");

    drop(info);
    Ok(())
}

/// The sleep inhibitor consults `has_running_tasks` across process boundaries
/// (the daemon owns tasks, the TUI renders them), so it must answer from status
/// files rather than the in-process map (#29).
#[tokio::test]
async fn has_running_tasks_sees_a_live_task_from_a_status_file() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    assert!(
        !manager.scan_any_running_task(),
        "an empty task directory means nothing is running"
    );

    let mut status = running_status_fixture("task-live", "session-live");
    // No owner metadata: treated as live rather than mislabeled as dead.
    write_status_fixture(&manager, &status).await;
    assert!(manager.has_running_tasks());

    status.status = BackgroundTaskStatus::Completed;
    write_status_fixture(&manager, &status).await;
    // Bypass the short cache so the state change is observed immediately.
    assert!(!manager.scan_any_running_task());

    Ok(())
}

#[tokio::test]
async fn has_running_tasks_ignores_a_task_whose_owner_died() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let mut status = running_status_fixture("task-orphan", "session-orphan");
    // PID 1 is always alive; a very high unused PID stands in for a dead owner.
    status.owner_pid = Some(4_194_303);
    write_status_fixture(&manager, &status).await;

    assert!(
        !manager.scan_any_running_task(),
        "a crashed owner must not hold the sleep inhibitor open forever"
    );

    Ok(())
}

#[tokio::test]
async fn has_running_tasks_caches_within_its_ttl() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let mut status = running_status_fixture("task-cached", "session-cached");
    write_status_fixture(&manager, &status).await;
    assert!(manager.has_running_tasks());

    status.status = BackgroundTaskStatus::Completed;
    write_status_fixture(&manager, &status).await;
    assert!(
        manager.has_running_tasks(),
        "the cached answer is reused inside the TTL so per-frame callers stay cheap"
    );

    Ok(())
}

/// The status directory is an archive: it keeps one file per task ever run,
/// while the interesting files are the few still running. A finished task can
/// never run again, so once its file has been read in a terminal state the
/// scans behind the status widget must stop opening it. Without that, the cost
/// of asking "what is running in this session" grows with everything the user
/// has ever run: the machine this was found on had 115 files to surface 1 row,
/// and the scan sat on the render thread.
#[tokio::test]
async fn a_settled_status_file_is_never_parsed_twice() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // One live task, plus an archive of finished ones. The scan only trusts a
    // `Running` file while a live process owns it, so point the fixture at this
    // test process.
    let mut live = running_status_fixture("live111aaaa", "session-a");
    live.owner_pid = Some(std::process::id());
    live.owner_instance = Some(model::process_instance_token().to_string());
    write_status_fixture(&manager, &live).await;
    for index in 0..24 {
        let mut finished = running_status_fixture(&format!("done{index:06}xx"), "session-a");
        finished.status = BackgroundTaskStatus::Completed;
        finished.exit_code = Some(0);
        finished.completed_at = Some(Utc::now().to_rfc3339());
        write_status_fixture(&manager, &finished).await;
    }

    // First scan has to look at everything: it cannot know what is terminal
    // until it reads each file once.
    manager.reset_status_parse_count_for_tests();
    let first = manager.running_rows_for_session("session-a");
    let first_parses = manager.status_parse_count_for_tests();
    assert_eq!(
        first_parses, 25,
        "the first scan must read every status file once"
    );

    // A later scan must not re-read anything that has not changed on disk. The
    // cache in front of this scan is time-based, so wait it out to be sure the
    // second call really re-scans rather than being served that snapshot.
    //
    // Zero, not one: the archive is skipped without a syscall because it is
    // settled, and the live file is served from the parse memo because its
    // mtime and length are untouched. `a_running_status_file_is_reparsed_when_it_changes`
    // is the other half of this contract and pins that a live task's own
    // updates still get re-read.
    tokio::time::sleep(Duration::from_millis(600)).await;
    manager.reset_status_parse_count_for_tests();
    let second = manager.running_rows_for_session("session-a");
    let second_parses = manager.status_parse_count_for_tests();
    assert_eq!(
        second_parses, 0,
        "an unchanged re-scan must not reparse the archive"
    );

    // Skipping work must not change the answer.
    let ids: Vec<&str> = first.iter().map(|row| row.task_id.as_str()).collect();
    assert_eq!(ids, vec!["live111aaaa"]);
    let ids: Vec<&str> = second.iter().map(|row| row.task_id.as_str()).collect();
    assert_eq!(ids, vec!["live111aaaa"]);

    Ok(())
}

/// The mirror of the test above: skipping must be driven by the file's *state*,
/// never by "have I seen this path before". A running task whose progress is
/// rewritten has to be re-read, or the widget would freeze at whatever the
/// first scan happened to catch.
#[tokio::test]
async fn a_running_status_file_is_reparsed_when_it_changes() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let mut status = running_status_fixture("live222bbbb", "session-b");
    status.owner_pid = Some(std::process::id());
    status.owner_instance = Some(model::process_instance_token().to_string());
    status.display_name = Some("first".to_string());
    write_status_fixture(&manager, &status).await;

    let rows = manager.running_rows_for_session("session-b");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].label, "first");

    // Rewrite the same path with a new label. Length changes here, and mtime
    // may not have moved on a coarse-grained filesystem, so this also pins that
    // the stamp is not mtime alone.
    status.display_name = Some("second stage".to_string());
    write_status_fixture(&manager, &status).await;

    tokio::time::sleep(Duration::from_millis(600)).await;
    let rows = manager.running_rows_for_session("session-b");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].label, "second stage",
        "a running task's own updates must still reach the widget"
    );

    Ok(())
}

/// The background panel shows a fixed number of trailing lines, so the read
/// behind it must cost the same whether a task printed one line or a hundred
/// megabytes of build log. Reading the whole file to discard nearly all of it
/// put the size of the log on the render path.
#[tokio::test]
async fn output_tail_sync_reads_only_the_end_of_a_large_output() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // 2 MB of output, with a recognizable last line.
    let mut output = String::new();
    for index in 0..40_000 {
        output.push_str(&format!("line {index} of compiler noise\n"));
    }
    output.push_str("FINAL LINE\n");
    let path = manager.output_path_for("tail111aaaa");
    tokio::fs::write(&path, &output).await?;
    assert!(
        output.len() > 1_000_000,
        "fixture should be big enough to matter, was {}",
        output.len()
    );

    let tail = manager
        .output_tail_sync("tail111aaaa", 4096)
        .ok_or_else(|| anyhow!("tail should read"))?;

    assert!(
        tail.len() <= 4096,
        "the read must be bounded by the budget, got {} bytes",
        tail.len()
    );
    assert!(
        tail.ends_with("FINAL LINE\n"),
        "the tail must be the *end* of the file"
    );

    // A byte-aligned cut can land mid-line, so the first line may be a
    // fragment. Callers take whole trailing lines, so what matters is that
    // every line after the first is intact.
    let lines: Vec<&str> = tail.lines().collect();
    assert!(lines.len() > 1);
    for line in &lines[1..] {
        assert!(
            line.starts_with("line ") || *line == "FINAL LINE",
            "expected intact lines after the first, got {line:?}"
        );
    }

    Ok(())
}

/// A file smaller than the budget must come back whole, not truncated or
/// padded, and must not error on the seek that a large file needs.
#[tokio::test]
async fn output_tail_sync_returns_a_small_output_in_full() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let path = manager.output_path_for("tail222bbbb");
    tokio::fs::write(&path, b"one\ntwo\n").await?;

    let tail = manager
        .output_tail_sync("tail222bbbb", 64 * 1024)
        .ok_or_else(|| anyhow!("tail should read"))?;
    assert_eq!(tail, "one\ntwo\n");

    // A missing file is a normal state (a task that has not written yet), not
    // an error to surface.
    assert!(manager.output_tail_sync("nosuchtask", 1024).is_none());

    Ok(())
}
