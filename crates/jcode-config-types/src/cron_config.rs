//! Config-declared recurring jobs ("jcode cron").
//!
//! These replace external timers (systemd, cron(8), launchd) for work the
//! user wants jcode itself to own: the daemon already runs a persistent
//! scheduling loop (see `ambient::runner`), so a job declared here rides that
//! loop instead of requiring a second process and a second failure mode to
//! reason about.
//!
//! Only the data contract lives here. Parsing `every`/`at` into an actual
//! next-fire time, persisting last-run state, and executing the job are
//! behavior that needs the filesystem and process spawning, so they live in
//! `jcode_app_core::cron` per the type-crate/behavior-crate split documented
//! in `docs/CRATE_OWNERSHIP_BOUNDARIES.md`.

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn default_timeout_secs() -> u64 {
    3600
}

/// A single recurring job declared in `[[cron]]` config blocks.
///
/// Exactly one of `every` (fixed interval) or `at` (wall-clock schedule)
/// should be set; behavior code treats both-set or neither-set as invalid
/// rather than guessing which one wins. Similarly exactly one of `command`
/// (run a shell command) or `prompt` (queue an agent task) should be set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CronJobConfig {
    /// Stable identifier for this job. Used as the key into the on-disk
    /// last-run state (`~/.jcode/cron/state.json`) and as the target of
    /// `cron:run:<id>`, so renaming it loses history and is equivalent to
    /// declaring a brand new job.
    pub id: String,

    /// Fixed interval between runs, e.g. `"30m"`, `"6h"`, `"1d"`.
    /// Mutually exclusive with `at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,

    /// Wall-clock schedule, e.g. `"daily 03:00"`, `"weekdays 09:00"`,
    /// `"mon,thu 18:30"`. Reuses the day-spec grammar from
    /// `ambient::schedule_window` so the two scheduling surfaces stay
    /// consistent for the user. Mutually exclusive with `every`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,

    /// Shell command to execute (exec mode). Run through the same
    /// shell-style argv parsing as `[hooks]` commands: no shell
    /// interpolation, just argv splitting, so `&&`/`|`/`$VAR` are not
    /// special. Mutually exclusive with `prompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Agent prompt to queue when this job fires (agent mode). Delivered
    /// through the same `ScheduledItem` machinery as `schedule_task`, so it
    /// gets the crash-recovery and delivery-target behavior that already
    /// exists for it. Mutually exclusive with `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,

    /// Working directory for the job. Exec mode: the process cwd. Prompt
    /// mode: the session's working directory. Defaults to jcode's own cwd
    /// when unset, i.e. wherever the daemon was started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,

    /// Where a prompt-mode job's result is delivered: `"ambient"`,
    /// `"spawn"`, or `"session:<id>"`. Mirrors `ScheduleTarget`. Ignored in
    /// exec mode, where there is no agent turn to deliver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,

    /// Whether this job is active (default: true). Kept rather than
    /// requiring the user to delete/re-add the block, so a temporarily
    /// unwanted job keeps its accumulated `last_run` history for when it is
    /// turned back on.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// If the daemon was down across a scheduled fire (crash, reboot, a
    /// `selfdev reload` that took longer than the interval), run the job
    /// once shortly after startup instead of waiting for the next natural
    /// slot (default: true). Off is for jobs where a missed run should just
    /// be skipped, e.g. a periodic reminder that is only useful at its exact
    /// time of day.
    #[serde(default = "default_true")]
    pub catch_up: bool,

    /// Whether `[ambient] active_windows` gates this job (default: false).
    /// `active_windows` exists to stop the ambient *agent* waking itself
    /// outside hours the user wants quiet. A cron job is user-declared clock
    /// work ("merge upstream every 6 hours"), not agent self-wake, so by
    /// default it must fire on schedule regardless of the ambient window.
    /// Set true for a job that should share the ambient quiet hours anyway.
    #[serde(default)]
    pub respect_windows: bool,

    /// Exec mode: kill the process if it has not exited after this many
    /// seconds (default: 3600). Ignored in prompt mode, which has its own
    /// turn/agent lifecycle.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for CronJobConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            every: None,
            at: None,
            command: None,
            prompt: None,
            working_dir: None,
            target: None,
            enabled: true,
            catch_up: true,
            respect_windows: false,
            timeout_secs: default_timeout_secs(),
        }
    }
}

impl CronJobConfig {
    /// Whether this job's mode fields form a valid combination.
    ///
    /// Behavior code (not this crate) decides what to do with an invalid
    /// job (warn and skip); this is a pure structural check so it is
    /// testable without touching disk.
    pub fn is_valid(&self) -> bool {
        if self.id.trim().is_empty() {
            return false;
        }
        let has_schedule = self.every.is_some() ^ self.at.is_some();
        let has_action = self.command.is_some() ^ self.prompt.is_some();
        has_schedule && has_action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_job_is_invalid_until_schedule_and_action_are_set() {
        let job = CronJobConfig::default();
        assert!(!job.is_valid(), "no id, no schedule, no action");
    }

    #[test]
    fn requires_exactly_one_schedule_field() {
        let mut job = CronJobConfig {
            id: "x".to_string(),
            command: Some("true".to_string()),
            ..Default::default()
        };
        assert!(!job.is_valid(), "neither every nor at set");

        job.every = Some("6h".to_string());
        assert!(job.is_valid());

        job.at = Some("daily 03:00".to_string());
        assert!(!job.is_valid(), "both every and at set");
    }

    #[test]
    fn requires_exactly_one_action_field() {
        let mut job = CronJobConfig {
            id: "x".to_string(),
            every: Some("6h".to_string()),
            ..Default::default()
        };
        assert!(!job.is_valid(), "neither command nor prompt set");

        job.command = Some("true".to_string());
        assert!(job.is_valid());

        job.prompt = Some("do something".to_string());
        assert!(!job.is_valid(), "both command and prompt set");
    }

    #[test]
    fn default_enabled_and_catch_up_are_true_respect_windows_is_false() {
        let job = CronJobConfig::default();
        assert!(job.enabled);
        assert!(job.catch_up);
        assert!(!job.respect_windows);
        assert_eq!(job.timeout_secs, 3600);
    }
}
