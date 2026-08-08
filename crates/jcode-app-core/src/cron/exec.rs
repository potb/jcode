//! Exec-mode cron jobs: run a shell command with a timeout and log its
//! output.
//!
//! Commands are parsed argv-style (same grammar as `[hooks]` commands, see
//! `crate::terminal_launch::parse_hook_command`) and executed directly, not
//! through a shell. That means `&&`, `|`, and `$VAR` are not special; a job
//! that needs them should be a small script file instead. This mirrors the
//! `[hooks]` contract on purpose rather than inventing a second convention
//! for "how jcode runs an external command".

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

use super::state::logs_dir;

/// Rotate a job's log once it exceeds this size, keeping one previous file
/// (`<id>.log.1`). A cron job that runs every few minutes forever would
/// otherwise grow its log without bound.
const LOG_ROTATE_BYTES: u64 = 2 * 1024 * 1024;

/// Outcome of running an exec-mode job.
pub struct ExecOutcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration: Duration,
    /// Set when the command could not even be parsed or spawned, e.g. a
    /// malformed command line or a missing binary. Distinct from a normal
    /// non-zero exit, which is a successful *run* of a failing command.
    pub spawn_error: Option<String>,
}

impl ExecOutcome {
    pub fn succeeded(&self) -> bool {
        self.spawn_error.is_none() && !self.timed_out && self.exit_code == Some(0)
    }
}

fn job_log_path(job_id: &str) -> Result<PathBuf> {
    Ok(logs_dir()?.join(format!("{job_id}.log")))
}

/// Rotate `<id>.log` to `<id>.log.1` if it has grown past the size cap.
/// Best-effort: a failure here must not stop the job from running, since
/// losing old log history is far cheaper than skipping a scheduled job.
fn rotate_log_if_large(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() < LOG_ROTATE_BYTES {
        return;
    }
    let rotated = path.with_extension("log.1");
    let _ = std::fs::rename(path, rotated);
}

/// Expand a job's `~/`-prefixed working directory the same way hook program
/// paths are expanded, since both are user-facing paths typed by hand in
/// config rather than produced by the shell.
fn expand_working_dir(working_dir: &str) -> PathBuf {
    if let Some(rest) = working_dir.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(working_dir)
}

/// Run `command` for job `job_id`, killing it after `timeout` and appending
/// its combined stdout/stderr to `~/.jcode/cron/logs/<job_id>.log`.
pub async fn run_job_command(
    job_id: &str,
    command: &str,
    working_dir: Option<&str>,
    timeout: Duration,
) -> ExecOutcome {
    let started = Instant::now();

    let parts = match crate::terminal_launch::parse_hook_command(command) {
        Ok(parts) => parts,
        Err(error) => {
            return ExecOutcome {
                exit_code: None,
                timed_out: false,
                duration: started.elapsed(),
                spawn_error: Some(format!("failed to parse command: {error}")),
            };
        }
    };
    // `parse_hook_command` guarantees at least one part on success.
    let (program, args) = parts.split_first().expect("parsed command is non-empty");

    let mut cmd = tokio::process::Command::new(crate::terminal_launch::expand_home(program));
    cmd.args(args);
    if let Some(dir) = working_dir {
        cmd.current_dir(expand_working_dir(dir));
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ExecOutcome {
                exit_code: None,
                timed_out: false,
                duration: started.elapsed(),
                spawn_error: Some(format!("failed to start '{command}': {error}")),
            };
        }
    };

    // Drain stdout/stderr concurrently with waiting rather than after: a
    // chatty command can fill the pipe buffer and deadlock a wait-then-read
    // sequence once the child blocks on a full pipe.
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let read_stdout = async move {
        let mut buf = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut buf).await;
        }
        buf
    };
    let read_stderr = async move {
        let mut buf = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_end(&mut buf).await;
        }
        buf
    };

    let run = async {
        let (status, stdout_buf, stderr_buf) =
            tokio::join!(child.wait(), read_stdout, read_stderr);
        (status, stdout_buf, stderr_buf)
    };

    let (exit_code, timed_out, stdout_buf, stderr_buf) =
        match tokio::time::timeout(timeout, run).await {
            Ok((Ok(status), out, err)) => (status.code(), false, out, err),
            Ok((Err(_wait_error), out, err)) => (None, false, out, err),
            Err(_elapsed) => {
                let _ = child.kill().await;
                (None, true, Vec::new(), Vec::new())
            }
        };

    if let Err(error) = write_log(job_id, command, &stdout_buf, &stderr_buf, timed_out) {
        crate::logging::warn(&format!(
            "cron job '{job_id}': failed to write log: {error}"
        ));
    }

    ExecOutcome {
        exit_code,
        timed_out,
        duration: started.elapsed(),
        spawn_error: None,
    }
}

fn write_log(
    job_id: &str,
    command: &str,
    stdout_buf: &[u8],
    stderr_buf: &[u8],
    timed_out: bool,
) -> Result<()> {
    let path = job_log_path(job_id)?;
    rotate_log_if_large(&path);

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening cron log {}", path.display()))?;

    writeln!(
        file,
        "=== {} | {} ===",
        chrono::Utc::now().to_rfc3339(),
        command
    )?;
    if !stdout_buf.is_empty() {
        file.write_all(stdout_buf)?;
        if !stdout_buf.ends_with(b"\n") {
            writeln!(file)?;
        }
    }
    if !stderr_buf.is_empty() {
        writeln!(file, "--- stderr ---")?;
        file.write_all(stderr_buf)?;
        if !stderr_buf.ends_with(b"\n") {
            writeln!(file)?;
        }
    }
    if timed_out {
        writeln!(file, "*** killed: exceeded timeout ***")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod exec_tests;
