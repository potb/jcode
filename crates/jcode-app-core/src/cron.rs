//! Config-declared recurring jobs ("jcode cron").
//!
//! Replaces external timers (systemd, cron(8), launchd) for work the user
//! wants tied to jcode's own lifecycle. The daemon already runs a persistent
//! scheduling loop for ambient mode (`ambient::runner`) even when ambient
//! itself is disabled — see the comment on `AmbientRunnerHandle::run_loop`'s
//! caller in `server.rs` — so cron jobs ride that same loop instead of
//! requiring a second process (and a second set of "did it actually fire"
//! failure modes) alongside it.
//!
//! Split into three pieces, mirroring the ambient module's own split:
//!
//! - [`schedule`]: pure calendar math (`every`/`at` parsing, `next_fire`).
//!   No I/O, so it is cheap to test exhaustively against a fixed clock.
//! - [`state`]: on-disk last-run history, keyed by job id.
//! - [`exec`]: running a shell command with a timeout and logging its output.
//!
//! This top-level module is the seam behavior plugs into: `tick` is what the
//! ambient runner loop calls once per iteration, and `list_snapshot` /
//! `run_job_now` are what the debug socket (`cron:list`, `cron:run:<id>`)
//! calls for visibility and manual triggers.

pub mod exec;
pub mod schedule;
pub mod state;

use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::Duration;

use crate::ambient::{AmbientManager, Priority, ScheduleRequest, ScheduleTarget};
use crate::config::{CronJobConfig, config};
use crate::logging;
pub use state::{CronJobState, CronState, LastStatus};

/// Job ids with an exec-mode run currently in flight, guarding against a slow
/// command still running when the next tick decides it is due again. Process
/// lifetime is the right scope for this: only one daemon owns the ambient
/// runner loop cron rides on (see `AmbientLock` for the cross-process analog
/// ambient cycles use), so an in-memory set is enough — nothing else in this
/// process can start a second run of the same job id.
static RUNNING_EXEC_JOBS: LazyLock<StdMutex<HashSet<String>>> =
    LazyLock::new(|| StdMutex::new(HashSet::new()));

/// Serializes read-modify-write access to `state.json`. Exec jobs finish on
/// their own tokio task and record their result independently; without this,
/// two jobs completing close together could both load the file before either
/// saves, and the second save would silently erase the first job's record.
static STATE_WRITE_LOCK: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

/// Job ids already warned about for a malformed config.
///
/// The runner re-evaluates every job on every pass, so warning inline turns a
/// single typo into one identical log line per pass forever (measured: 36
/// copies in 80 seconds at a 5s cadence, growing without bound). The condition
/// is a static config error, so it is worth saying exactly once. Cleared by
/// [`forget_invalid_warnings`] when the config is reloaded, so a job that is
/// fixed and then broken again warns again.
static WARNED_INVALID_JOBS: LazyLock<StdMutex<HashSet<String>>> =
    LazyLock::new(|| StdMutex::new(HashSet::new()));

/// True the first time a given job id is seen invalid, false afterwards.
fn should_warn_invalid(job_id: &str) -> bool {
    WARNED_INVALID_JOBS
        .lock()
        .map(|mut warned| warned.insert(job_id.to_string()))
        .unwrap_or(true)
}

/// Drop the warned-about set so a reloaded config reports its errors afresh.
pub fn forget_invalid_warnings() {
    if let Ok(mut warned) = WARNED_INVALID_JOBS.lock() {
        warned.clear();
    }
}

fn mark_running(job_id: &str) -> bool {
    RUNNING_EXEC_JOBS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(job_id.to_string())
}

fn clear_running(job_id: &str) {
    RUNNING_EXEC_JOBS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(job_id);
}

pub fn is_running(job_id: &str) -> bool {
    RUNNING_EXEC_JOBS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(job_id)
}

fn record_run(
    job_id: &str,
    ended_at: DateTime<Utc>,
    status: LastStatus,
    exit_code: Option<i32>,
    duration_ms: u64,
) {
    let _guard = STATE_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut state = CronState::load();
    if let Err(e) = state.record_run(job_id, ended_at, status, exit_code, duration_ms) {
        logging::error(&format!(
            "cron: failed to persist run state for '{job_id}': {e}"
        ));
    }
}

/// Where a cron job's `target` string maps to in the existing
/// `ScheduleTarget` machinery.
///
/// `"spawn"` deliberately synthesizes a parent session id that does not
/// exist (`cron:<id>`). A cron-triggered prompt has no originating
/// interactive session to spawn *from*, and `spawn_session_for_scheduled_item`
/// already handles an unresolvable parent by logging a warning and creating
/// a fresh standalone session instead (with `working_dir` still applied) —
/// exactly the "one-off session" behavior a cron job wants, achieved by
/// reusing the existing fallback rather than adding a new delivery path.
fn parse_cron_target(job: &CronJobConfig) -> ScheduleTarget {
    match job.target.as_deref() {
        Some("spawn") => ScheduleTarget::Spawn {
            parent_session_id: format!("cron:{}", job.id),
        },
        Some(rest) if rest.starts_with("session:") => ScheduleTarget::Session {
            session_id: rest["session:".len()..].to_string(),
        },
        _ => ScheduleTarget::Ambient,
    }
}

/// Queue a prompt-mode job through the same `ScheduledItem` machinery as the
/// `schedule_task` tool, so it inherits that pipeline's crash-recovery and
/// delivery-target behavior for free. Returns the queued item id.
///
/// "Firing" a prompt job means successfully enqueuing it; the queue delivery
/// (running the ambient cycle / resuming the session) is the *existing*
/// runner loop's job, tracked by its own state. Cron's own `last_run` here
/// records "did the schedule request land", not "did the resulting turn
/// succeed" — the two are different pipelines by design.
///
/// A fire is skipped while this job's previous item is still queued. Cron
/// ticks regardless of `ambient.enabled` (that is the point: user-declared
/// clock work is not agent self-wake), but prompt delivery *is* the ambient
/// pipeline, so with ambient off nothing ever drains the queue. Without this
/// guard a `[[cron]]` prompt job silently grows the queue forever: measured
/// at 9 overdue items within a minute at an 8s cadence. This mirrors the
/// [`mark_running`] guard exec jobs already get, at the level prompt mode
/// actually has: one outstanding request per job.
fn run_prompt_job(job: &CronJobConfig, slot: Option<DateTime<Utc>>) -> anyhow::Result<String> {
    let prompt = job
        .prompt
        .clone()
        .ok_or_else(|| anyhow::anyhow!("cron job '{}' has no prompt configured", job.id))?;

    let mut mgr = AmbientManager::new()?;
    let origin = format!("cron:{}", job.id);
    if let Some(pending) = mgr
        .queue()
        .items()
        .iter()
        .find(|item| item.created_by_session == origin)
    {
        anyhow::bail!(
            "cron job '{}' still has a queued fire ({}) waiting for delivery; \
             skipping this tick",
            job.id,
            pending.id
        );
    }

    let request = ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(Utc::now()),
        context: prompt,
        priority: Priority::Normal,
        target: parse_cron_target(job),
        created_by_session: origin,
        working_dir: job.working_dir.clone(),
        task_description: Some(format!("Cron job: {}", job.id)),
        relevant_files: Vec::new(),
        git_branch: None,
        additional_context: None,
    };
    let queued_id = mgr.schedule(request)?;
    record_run(
        &job.id,
        slot.unwrap_or_else(Utc::now),
        LastStatus::Success,
        None,
        0,
    );
    Ok(queued_id)
}

/// Run an exec-mode job to completion and record its outcome.
///
/// `slot` is the scheduled instant this run belongs to, or `None` for a manual
/// `cron:run:<id>`. It becomes the recorded `last_run`, which is what the next
/// fire is computed from. Recording the *completion* time instead makes an
/// interval job drift by one cycle's latency every cycle: `last_run` lands a
/// few milliseconds past the slot, the next deadline inherits that offset, and
/// the runner's sleep math rounds the sub-second remainder up to a whole
/// second, so an `every = "5s"` job settles into a measured 6.00s cadence.
/// Anchoring on the slot keeps fires on the schedule's grid.
///
/// Self-guarding via [`mark_running`]/[`clear_running`]: a second call for
/// the same job id while one is already in flight is a no-op rather than a
/// second concurrent process, satisfying "never run two instances of the
/// same job concurrently" even when callers race (the loop tick and a manual
/// `cron:run:<id>` landing at the same moment).
pub async fn run_exec_job(job: CronJobConfig, slot: Option<DateTime<Utc>>) {
    let Some(command) = job.command.clone() else {
        logging::warn(&format!(
            "cron: job '{}' has no command configured; skipping exec run",
            job.id
        ));
        return;
    };
    if !mark_running(&job.id) {
        logging::info(&format!(
            "cron: job '{}' is still running from a previous fire; skipping this tick",
            job.id
        ));
        return;
    }

    let timeout = Duration::from_secs(job.timeout_secs.max(1));
    let outcome =
        exec::run_job_command(&job.id, &command, job.working_dir.as_deref(), timeout).await;
    let duration_ms = outcome.duration.as_millis().min(u128::from(u64::MAX)) as u64;

    if let Some(error) = &outcome.spawn_error {
        logging::warn(&format!("cron: job '{}' failed to run: {}", job.id, error));
    } else if outcome.timed_out {
        logging::warn(&format!(
            "cron: job '{}' exceeded its {}s timeout and was killed",
            job.id,
            timeout.as_secs()
        ));
    }

    let status = if outcome.spawn_error.is_some() {
        LastStatus::Failure
    } else if outcome.timed_out {
        LastStatus::Timeout
    } else if outcome.exit_code == Some(0) {
        LastStatus::Success
    } else {
        LastStatus::Failure
    };

    record_run(
        &job.id,
        slot.unwrap_or_else(Utc::now),
        status,
        outcome.exit_code,
        duration_ms,
    );
    clear_running(&job.id);
}

/// Spawn an exec-mode job on its own task so a slow command cannot block the
/// runner loop (up to `timeout_secs`, default one hour) from servicing
/// everything else the loop is responsible for.
fn spawn_exec_job(job: CronJobConfig, slot: Option<DateTime<Utc>>) {
    tokio::spawn(run_exec_job(job, slot));
}

fn fire(job: CronJobConfig, slot: Option<DateTime<Utc>>) {
    if job.command.is_some() {
        spawn_exec_job(job, slot);
    } else if job.prompt.is_some() {
        if let Err(e) = run_prompt_job(&job, slot) {
            // Not necessarily a failure: the commonest case is the previous
            // fire still sitting undelivered in the queue, which is the guard
            // working as intended rather than something going wrong.
            logging::info(&format!("cron: prompt job '{}' not queued: {}", job.id, e));
        }
    }
}

/// The two deadlines cron feeds into the runner loop's sleep calculation,
/// mirroring the direct/ambient split the loop already does for the
/// scheduled-item queue (`next_direct_due` / `next_ambient_due`).
///
/// `unblocked` is jobs with `respect_windows = false` (the default): they are
/// user-declared clock work and must be able to shorten the sleep even while
/// a wall-clock window is closed, the same way a direct-delivery deadline
/// does. `windowed` is jobs with `respect_windows = true`: they share
/// ambient's quiet hours, so they must NOT shorten a closed-window sleep,
/// exactly like an ambient-targeted queue deadline.
#[derive(Debug, Clone, Copy, Default)]
pub struct CronDeadlines {
    pub unblocked: Option<DateTime<Utc>>,
    pub windowed: Option<DateTime<Utc>>,
}

/// Shared core of [`tick`] and [`peek_next_due`]: walk the configured jobs,
/// firing due ones only when `fire_due` is set, and return the earliest
/// still-future fire time for each of the two deadline categories.
fn evaluate(window_open: bool, fire_due: bool) -> CronDeadlines {
    let jobs = config().cron.clone();
    if jobs.is_empty() {
        return CronDeadlines::default();
    }
    let state = CronState::load();
    let now = Utc::now();
    let mut deadlines = CronDeadlines::default();

    for job in jobs {
        if !job.is_valid() {
            if should_warn_invalid(&job.id) {
                logging::warn(&format!(
                    "cron: job '{}' has an invalid config (needs exactly one of every/at \
                     and exactly one of command/prompt); skipping",
                    job.id
                ));
            }
            continue;
        }
        if !job.enabled {
            continue;
        }
        let last_run = state.last_run(&job.id);
        let Some(next) = schedule::next_fire(&job, last_run, now) else {
            continue;
        };
        if next > now {
            let slot = if job.respect_windows {
                &mut deadlines.windowed
            } else {
                &mut deadlines.unblocked
            };
            *slot = Some(slot.map_or(next, |e: DateTime<Utc>| e.min(next)));
            continue;
        }
        if job.respect_windows && !window_open {
            continue;
        }
        if fire_due {
            // The instant this run is anchored to. Normally that is `next`,
            // the slot the job was due at, which keeps an interval job on the
            // schedule's grid instead of drifting by one execution's latency
            // per cycle.
            //
            // A catch-up fire is different. `next` is then a slot from while
            // the daemon was down, and recording it verbatim leaves the job
            // still overdue, so the very next pass fires the following missed
            // slot, and the one after that, draining the whole backlog one
            // fire at a time. `catch_up` promises "rerun a fire missed while
            // the daemon was down", singular, so walk the anchor forward to
            // the newest grid point that is not in the future: the job runs
            // once for the outage and resumes on the grid.
            let anchor = schedule::latest_slot_not_after(&job, next, now);
            // Fold in the fire AFTER this one. Without it a job that just
            // fired contributes no deadline at all, the loop falls back to its
            // 30s idle poll, and an `every = "5s"` job quietly runs every
            // thirty seconds.
            let projected = schedule::next_fire(&job, Some(anchor), anchor);
            fire(job.clone(), Some(anchor));
            if let Some(next_after) = projected {
                let slot = if job.respect_windows {
                    &mut deadlines.windowed
                } else {
                    &mut deadlines.unblocked
                };
                *slot = Some(slot.map_or(next_after, |e: DateTime<Utc>| e.min(next_after)));
            }
        }
    }

    deadlines
}

/// Run any due cron jobs and return the earliest upcoming fire time in each
/// deadline category.
///
/// Called once per ambient runner loop iteration, before the loop decides
/// how long to sleep — mirrors how the loop already folds queued-item
/// deadlines into its sleep calculation, just for a second source of
/// deadlines.
pub fn tick(window_open: bool) -> CronDeadlines {
    evaluate(window_open, true)
}

/// Same deadline calculation as [`tick`], without firing anything.
///
/// Used to recompute the next cron deadline *after* a (possibly long-running)
/// ambient cycle, the same way the runner already recomputes
/// `AmbientManager::next_item_due` post-cycle instead of trusting a
/// pre-cycle snapshot.
pub fn peek_next_due(window_open: bool) -> CronDeadlines {
    evaluate(window_open, false)
}

/// A snapshot of one job's schedule and last-run history, for `cron:list`.
#[derive(Debug, Clone)]
pub struct CronJobSnapshot {
    pub id: String,
    pub schedule_description: String,
    pub enabled: bool,
    pub valid: bool,
    pub last_run: Option<DateTime<Utc>>,
    pub last_status: Option<LastStatus>,
    pub consecutive_failures: u32,
    pub next_run: Option<DateTime<Utc>>,
    pub running: bool,
}

/// Snapshot every configured cron job for the debug socket. Reads config and
/// state; never fires anything, so it is safe to call as often as a status
/// poll wants.
pub fn list_snapshot() -> Vec<CronJobSnapshot> {
    let jobs = config().cron.clone();
    let state = CronState::load();
    let now = Utc::now();

    jobs.into_iter()
        .map(|job| {
            let valid = job.is_valid();
            let last_run = state.last_run(&job.id);
            let job_state = state.get(&job.id);
            let next_run = if valid && job.enabled {
                schedule::next_fire(&job, last_run, now)
            } else {
                None
            };
            CronJobSnapshot {
                schedule_description: schedule::describe_schedule(&job),
                enabled: job.enabled,
                valid,
                last_run,
                last_status: job_state.and_then(|s| s.last_status),
                consecutive_failures: job_state.map(|s| s.consecutive_failures).unwrap_or(0),
                next_run,
                running: is_running(&job.id),
                id: job.id,
            }
        })
        .collect()
}

/// The single nearest upcoming fire time across every configured job,
/// whichever deadline category is sooner. Used by `ambient:status` and
/// `cron:list` so "when does anything next fire" has one shared answer
/// rather than each call site computing it slightly differently.
///
/// Deadlines already in the past are skipped. A job can legitimately sit
/// overdue without firing: `respect_windows` outside its window, or a prompt
/// job whose previous fire is still queued. Reporting those as "next due"
/// answers a different question than the field name asks, and shows up as an
/// `ambient:status` that claims work was due minutes ago while the schedule is
/// in fact healthy (observed at 106s in the past with everything working as
/// designed). When every job is in that state there is no meaningful next
/// time, and `None` says so.
pub fn next_due() -> Option<DateTime<Utc>> {
    let now = Utc::now();
    list_snapshot()
        .into_iter()
        .filter_map(|job| job.next_run)
        .filter(|next| *next >= now)
        .min()
}

/// Force one job to run right now, bypassing its schedule (but not its
/// enabled flag, validity, or the exec concurrency guard). Used by
/// `cron:run:<id>`.
pub async fn run_job_now(id: &str) -> anyhow::Result<String> {
    let job = config()
        .cron
        .iter()
        .find(|j| j.id == id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no cron job with id '{id}'"))?;

    if !job.is_valid() {
        anyhow::bail!(
            "cron job '{id}' has an invalid config (needs exactly one of every/at \
             and exactly one of command/prompt)"
        );
    }
    if !job.enabled {
        anyhow::bail!("cron job '{id}' is disabled");
    }

    if job.command.is_some() {
        if is_running(&job.id) {
            anyhow::bail!("cron job '{id}' is already running");
        }
        let label = job.id.clone();
        spawn_exec_job(job, None);
        Ok(format!("started exec job '{label}'"))
    } else if job.prompt.is_some() {
        let queued_id = run_prompt_job(&job, None)?;
        Ok(format!(
            "queued prompt job '{id}' as scheduled item {queued_id}"
        ))
    } else {
        anyhow::bail!("cron job '{id}' has neither command nor prompt configured")
    }
}

#[cfg(test)]
#[path = "cron_tests.rs"]
mod cron_tests;
