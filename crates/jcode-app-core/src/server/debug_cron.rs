use crate::cron;
use anyhow::Result;

pub(super) async fn maybe_handle_cron_command(cmd: &str) -> Result<Option<String>> {
    if cmd == "cron:list" {
        return Ok(Some(cron_list_json()));
    }

    if let Some(id) = cmd.strip_prefix("cron:run:") {
        let id = id.trim();
        if id.is_empty() {
            return Err(anyhow::anyhow!("Usage: cron:run:<id>"));
        }
        let output = cron::run_job_now(id).await?;
        return Ok(Some(output));
    }

    if cmd == "cron:help" {
        return Ok(Some(
            r#"Cron debug commands (cron: prefix):
  cron:list      - Configured [[cron]] jobs: schedule, enabled, last run/status, next run
  cron:run:<id>  - Run one job immediately, bypassing its schedule
  cron:help      - Cron command reference"#
                .to_string(),
        ));
    }

    Ok(None)
}

fn cron_list_json() -> String {
    let now = chrono::Utc::now();
    let jobs: Vec<serde_json::Value> = cron::list_snapshot()
        .into_iter()
        .map(|job| {
            let in_human = job.next_run.map(|next| {
                let mins = (next - now).num_minutes().max(0) as u32;
                crate::ambient::format_minutes_human(mins)
            });
            serde_json::json!({
                "id": job.id,
                "schedule": job.schedule_description,
                "enabled": job.enabled,
                "valid": job.valid,
                "running": job.running,
                "last_run": job.last_run.map(|t| t.to_rfc3339()),
                "last_status": job.last_status.map(|s| format!("{:?}", s).to_lowercase()),
                "consecutive_failures": job.consecutive_failures,
                "next_run": job.next_run.map(|t| t.to_rfc3339()),
                "next_run_in": in_human,
            })
        })
        .collect();
    serde_json::to_string_pretty(&jobs).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
#[path = "debug_cron_tests.rs"]
mod debug_cron_tests;
