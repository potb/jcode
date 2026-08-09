use crate::ambient_runner::AmbientRunnerHandle;
use crate::provider::Provider;
use anyhow::Result;
use std::sync::Arc;

pub(super) async fn maybe_handle_ambient_command(
    cmd: &str,
    ambient_runner: &Option<AmbientRunnerHandle>,
    provider: &Arc<dyn Provider>,
) -> Result<Option<String>> {
    if cmd == "ambient:status" {
        let output = if let Some(runner) = ambient_runner {
            runner.status_json().await
        } else {
            serde_json::json!({
                "enabled": false,
                "status": "disabled",
                "message": "Ambient mode is not enabled in config"
            })
            .to_string()
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:queue" {
        let output = if let Some(runner) = ambient_runner {
            runner.queue_json().await
        } else {
            "[]".to_string()
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:trigger" {
        let output = if let Some(runner) = ambient_runner {
            runner.trigger().await;
            "Ambient cycle triggered".to_string()
        } else {
            return Err(anyhow::anyhow!("Ambient mode is not enabled"));
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:log" {
        let output = if let Some(runner) = ambient_runner {
            runner.log_json().await
        } else {
            "[]".to_string()
        };
        return Ok(Some(output));
    }

    // Dump the context an ambient cycle would actually see. `cargo build`
    // proves nothing about the prompt; this makes it inspectable at runtime.
    if cmd == "ambient:prompt" {
        let state = crate::ambient::AmbientState::load().unwrap_or_default();
        let manager = crate::memory::MemoryManager::new();
        let queue = match crate::ambient::AmbientManager::new() {
            Ok(mgr) => mgr.queue().items().to_vec(),
            Err(_) => Vec::new(),
        };
        let prompt = crate::ambient::build_ambient_system_prompt(
            &state,
            &queue,
            &crate::ambient::gather_memory_graph_health(&manager),
            &crate::ambient::gather_recent_sessions(state.last_run),
            &crate::ambient::gather_feedback_memories(&manager),
            &crate::ambient::ResourceBudget {
                provider: provider.name().to_string(),
                tokens_remaining_desc: "unknown (debug dump)".to_string(),
                window_resets_desc: "unknown".to_string(),
                user_usage_rate_desc: "unknown".to_string(),
                cycle_budget_desc: "unknown".to_string(),
            },
            0,
        );
        return Ok(Some(prompt));
    }

    if cmd == "ambient:permissions" {
        let output = if let Some(runner) = ambient_runner {
            let _ = runner
                .safety()
                .expire_dead_session_requests("debug_socket_gc");
            let pending = runner.safety().pending_requests();
            let items: Vec<serde_json::Value> = pending
                .iter()
                .map(|request| {
                    let review_summary = request
                        .context
                        .as_ref()
                        .and_then(|ctx| ctx.get("review"))
                        .and_then(|review| review.get("summary"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&request.description);
                    let review_why = request
                        .context
                        .as_ref()
                        .and_then(|ctx| ctx.get("review"))
                        .and_then(|review| review.get("why_permission_needed"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&request.rationale);
                    serde_json::json!({
                        "id": request.id,
                        "action": request.action,
                        "description": request.description,
                        "rationale": request.rationale,
                        "summary": review_summary,
                        "why_permission_needed": review_why,
                        "urgency": format!("{:?}", request.urgency),
                        "wait": request.wait,
                        "created_at": request.created_at.to_rfc3339(),
                        "context": request.context,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
        } else {
            "[]".to_string()
        };
        return Ok(Some(output));
    }

    if cmd.starts_with("ambient:approve:") {
        let request_id = cmd.strip_prefix("ambient:approve:").unwrap_or("").trim();
        if request_id.is_empty() {
            return Err(anyhow::anyhow!("Usage: ambient:approve:<request_id>"));
        }
        let output = if let Some(runner) = ambient_runner {
            runner
                .safety()
                .record_decision(request_id, true, "debug_socket", None)?;
            format!("Approved: {}", request_id)
        } else {
            return Err(anyhow::anyhow!("Ambient mode is not enabled"));
        };
        return Ok(Some(output));
    }

    if cmd.starts_with("ambient:deny:") {
        let rest = cmd.strip_prefix("ambient:deny:").unwrap_or("").trim();
        if rest.is_empty() {
            return Err(anyhow::anyhow!("Usage: ambient:deny:<request_id> [reason]"));
        }
        let output = if let Some(runner) = ambient_runner {
            let mut parts = rest.splitn(2, char::is_whitespace);
            let request_id = parts.next().unwrap_or("").trim();
            let message = parts
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            runner
                .safety()
                .record_decision(request_id, false, "debug_socket", message)?;
            format!("Denied: {}", request_id)
        } else {
            return Err(anyhow::anyhow!("Ambient mode is not enabled"));
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:stop" {
        let output = if let Some(runner) = ambient_runner {
            runner.stop().await;
            "Ambient mode stopped".to_string()
        } else {
            return Err(anyhow::anyhow!("Ambient mode is not enabled"));
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:start" {
        let output = if let Some(runner) = ambient_runner {
            if runner.start(Arc::clone(provider)).await {
                "Ambient mode started".to_string()
            } else {
                "Ambient mode is already running".to_string()
            }
        } else {
            return Err(anyhow::anyhow!("Ambient mode is not enabled in config"));
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:help" {
        return Ok(Some(
            r#"Ambient mode debug commands (ambient: prefix):
  ambient:status              - Ambient + schedule runner state, counts, next due items
  ambient:queue               - Scheduled queue contents with target/session metadata
  ambient:trigger             - Manually trigger an ambient cycle
  ambient:log                 - Recent transcript summaries
  ambient:permissions         - List pending permission requests
  ambient:approve:<id>        - Approve a permission request
  ambient:deny:<id> [reason]  - Deny a permission request (optional reason)
  ambient:start               - Start/restart ambient mode
  ambient:stop                - Stop ambient mode"#
                .to_string(),
        ));
    }

    Ok(None)
}
