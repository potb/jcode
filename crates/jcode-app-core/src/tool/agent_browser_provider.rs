//! agent-browser provider: maps jcode's normalized browser actions onto the
//! `agent-browser` CLI (vercel-labs/agent-browser), which drives Chrome over CDP.
//!
//! Every action goes through `agent-browser --json --session <jcode-session> ...`
//! so results are structured and each jcode session gets an isolated browser.

use super::{ToolContext, ToolOutput};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::path::PathBuf;

use super::browser::{BrowserInput, BrowserProvider};

pub struct AgentBrowserProvider;

#[async_trait::async_trait]
impl BrowserProvider for AgentBrowserProvider {
    fn id(&self) -> &'static str {
        "agent_browser"
    }

    fn supported_browsers(&self) -> &'static [&'static str] {
        &["chrome", "chromium", "edge", "brave"]
    }

    async fn status(&self, _ctx: &ToolContext) -> Result<ToolOutput> {
        let status = crate::agent_browser::inspect_status().await;
        let metadata = json!({
            "backend": if status.binary_installed { "agent_browser" } else { "unconfigured" },
            "browser": "chrome",
            "binary_installed": status.binary_installed,
            "binary_path": status.binary_path.as_ref().map(|p| p.display().to_string()),
            "version": status.version,
            "chrome_installed": status.chrome_installed,
            "responding": status.responding,
            "ready": status.ready,
            "diagnostics": status.diagnostics,
        });

        let body = if status.ready {
            format!(
                "agent-browser is installed and responding ({}).",
                status.version.as_deref().unwrap_or("unknown version")
            )
        } else if !status.binary_installed {
            "agent-browser is not installed yet. Use action='setup' to install it.".to_string()
        } else if !status.chrome_installed {
            "agent-browser is installed but no Chrome was found. Use action='setup' to download Chrome for Testing.".to_string()
        } else {
            "agent-browser is installed but not responding. Use action='setup' to repair."
                .to_string()
        };

        Ok(ToolOutput::new(body)
            .with_title("browser status")
            .with_metadata(metadata))
    }

    async fn setup(&self) -> Result<ToolOutput> {
        let log = crate::agent_browser::ensure_setup().await?;
        let status = crate::agent_browser::inspect_status().await;
        let title = if status.ready {
            "browser setup"
        } else {
            "browser setup (incomplete)"
        };
        Ok(ToolOutput::new(log).with_title(title).with_metadata(json!({
            "backend": "agent_browser",
            "browser": "chrome",
            "binary_installed": status.binary_installed,
            "chrome_installed": status.chrome_installed,
            "ready": status.ready,
        })))
    }

    async fn ensure_ready(&self) -> Result<Option<String>> {
        let status = crate::agent_browser::inspect_status().await;
        if status.ready {
            return Ok(None);
        }
        anyhow::bail!(
            "agent-browser is not ready ({}). Use the browser tool with action='status' to confirm, then action='setup' to install or repair. Do not retry browser actions until status reports ready.",
            if status.diagnostics.is_empty() {
                "unknown reason".to_string()
            } else {
                status.diagnostics.join("; ")
            }
        );
    }

    async fn execute(
        &self,
        action: &str,
        input: &BrowserInput,
        ctx: &ToolContext,
    ) -> Result<ToolOutput> {
        let plan = build_command(action, input)?;

        if plan.wants_screenshot_image {
            return screenshot_with_image(plan, ctx).await;
        }

        let value = run_cli(&plan.args, ctx).await?;
        Ok(render_output(action, plan.title, value))
    }
}

/// A single agent-browser CLI invocation.
#[derive(Debug)]
pub struct CommandPlan {
    pub args: Vec<String>,
    pub title: String,
    pub wants_screenshot_image: bool,
}

/// Resolve a jcode selector-ish target into an agent-browser selector.
///
/// agent-browser accepts CSS selectors and `@eN` refs from a snapshot. jcode's
/// `text` targeting maps onto agent-browser's `find text <text> <action>` form,
/// which is handled by the callers that support it.
fn selector_of(input: &BrowserInput) -> Option<String> {
    input.selector.clone()
}

pub fn build_command(action: &str, input: &BrowserInput) -> Result<CommandPlan> {
    let mut args: Vec<String> = Vec::new();
    let mut wants_screenshot_image = false;

    match action {
        "open" => {
            let url = input
                .url
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("url is required for open"))?;
            if input.new_tab.unwrap_or(false) {
                args.extend(["tab".into(), "new".into(), url.to_string()]);
            } else {
                args.extend(["open".into(), url.to_string()]);
            }
        }
        "snapshot" => {
            args.push("snapshot".into());
        }
        "interactables" => {
            args.extend(["snapshot".into(), "-i".into()]);
        }
        "get_content" => match input.format.as_deref().unwrap_or("text") {
            "html" => {
                args.extend([
                    "get".into(),
                    "html".into(),
                    selector_of(input).unwrap_or_else(|| "body".into()),
                ]);
            }
            "title" => args.extend(["get".into(), "title".into()]),
            "annotated" => args.push("snapshot".into()),
            _ => {
                args.extend([
                    "get".into(),
                    "text".into(),
                    selector_of(input).unwrap_or_else(|| "body".into()),
                ]);
            }
        },
        "click" => {
            if let Some(selector) = selector_of(input) {
                args.extend(["click".into(), selector]);
            } else if let Some(text) = &input.text {
                args.extend(["find".into(), "text".into(), text.clone(), "click".into()]);
            } else if let (Some(x), Some(y)) = (input.x, input.y) {
                // Coordinate click: move then press/release.
                args.extend([
                    "mouse".into(),
                    "move".into(),
                    x.to_string(),
                    y.to_string(),
                ]);
            } else {
                anyhow::bail!("click requires selector, text, or x/y coordinates");
            }
            if input.new_tab.unwrap_or(false) {
                args.push("--new-tab".into());
            }
        }
        "type" => {
            let text = input
                .text
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("text is required for type"))?;
            let selector = selector_of(input)
                .ok_or_else(|| anyhow::anyhow!("selector is required for type"))?;
            // `fill` clears first; `type` appends. jcode's `clear` flag picks.
            let verb = if input.clear.unwrap_or(true) {
                "fill"
            } else {
                "type"
            };
            args.extend([verb.into(), selector, text.to_string()]);
        }
        "fill_form" => {
            // agent-browser has no batch form fill; use `batch` to keep it one call.
            let fields = input
                .fields
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("fields are required for fill_form"))?;
            args.push("batch".into());
            for field in fields {
                let cmd = match (&field.value, field.checked) {
                    (Some(value), _) => {
                        json!(["fill", field.selector.clone(), value.clone()])
                    }
                    (None, Some(true)) => json!(["check", field.selector.clone()]),
                    (None, Some(false)) => json!(["uncheck", field.selector.clone()]),
                    (None, None) => {
                        anyhow::bail!("field {} needs value or checked", field.selector)
                    }
                };
                args.push(serde_json::to_string(&cmd)?);
            }
        }
        "select" => {
            let selector = selector_of(input)
                .ok_or_else(|| anyhow::anyhow!("selector is required for select"))?;
            let value = input.text.as_deref().ok_or_else(|| {
                anyhow::anyhow!("text is required for select and is used as the option value")
            })?;
            args.extend(["select".into(), selector, value.to_string()]);
        }
        "wait" => {
            args.push("wait".into());
            if let Some(selector) = selector_of(input) {
                args.push(selector);
            } else if let Some(contains) = &input.contains {
                args.extend(["--text".into(), contains.clone()]);
            } else if let Some(text) = &input.text {
                args.extend(["--text".into(), text.clone()]);
            } else {
                anyhow::bail!("wait requires selector, text, or contains");
            }
        }
        "screenshot" => {
            args.push("screenshot".into());
            wants_screenshot_image = true;
        }
        "eval" => {
            let script = input
                .script
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("script is required for eval"))?;
            // base64 avoids all shell/arg-quoting hazards for arbitrary JS.
            args.extend(["eval".into(), "-b".into(), STANDARD.encode(script)]);
        }
        "scroll" => {
            if let Some(selector) = selector_of(input) {
                args.extend(["scrollintoview".into(), selector]);
            } else if let Some(position) = input.position.as_deref() {
                match position {
                    "top" => args.extend(["scroll".into(), "up".into(), "999999".into()]),
                    "bottom" => args.extend(["scroll".into(), "down".into(), "999999".into()]),
                    other => anyhow::bail!("unsupported scroll position: {other}"),
                }
            } else if let Some(y) = input.y.or(input.scroll_to.as_ref().and_then(|s| s.y)) {
                let (dir, px) = if y < 0.0 { ("up", -y) } else { ("down", y) };
                args.extend(["scroll".into(), dir.into(), (px as i64).to_string()]);
            } else {
                anyhow::bail!("scroll requires selector, position, or y offset");
            }
        }
        "upload" => {
            let selector = selector_of(input)
                .ok_or_else(|| anyhow::anyhow!("selector is required for upload"))?;
            let path = input
                .path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("path is required for upload"))?;
            args.extend(["upload".into(), selector, path.to_string()]);
        }
        "press" => {
            let key = input
                .key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("key is required for press"))?;
            if let Some(selector) = selector_of(input) {
                // Focus the target first so the key lands on it.
                args.push("batch".into());
                args.push(serde_json::to_string(&json!(["focus", selector]))?);
                args.push(serde_json::to_string(&json!(["press", key]))?);
            } else {
                args.extend(["press".into(), key.to_string()]);
            }
        }
        "list_tabs" => args.extend(["tab".into(), "list".into()]),
        "new_tab" => {
            args.extend(["tab".into(), "new".into()]);
            if let Some(url) = &input.url {
                args.push(url.clone());
            }
        }
        "select_tab" => {
            let tab_id = input
                .tab_id
                .ok_or_else(|| anyhow::anyhow!("tab_id is required for select_tab"))?;
            // agent-browser >=0.30 requires stable `t<N>` handles and rejects bare
            // integers; older builds accept the `t<N>` form too, so always send it.
            args.extend(["tab".into(), format!("t{tab_id}")]);
        }
        "get_active_tab" => args.extend(["get".into(), "url".into()]),
        "list_frames" => {
            // No direct equivalent; enumerate iframes from the page.
            args.extend([
                "eval".into(),
                "-b".into(),
                STANDARD.encode(
                    "Array.from(document.querySelectorAll('iframe')).map((f,i)=>({index:i,src:f.src,name:f.name,id:f.id}))",
                ),
            ]);
        }
        "provider_command" => {
            let provider_action = input.provider_action.as_deref().ok_or_else(|| {
                anyhow::anyhow!("provider_action is required when action='provider_command'")
            })?;
            args.push(provider_action.to_string());
            if let Some(Value::Array(extra)) = &input.params {
                for value in extra {
                    args.push(match value {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    });
                }
            }
        }
        other => anyhow::bail!("Unsupported browser action: {}", other),
    }

    Ok(CommandPlan {
        args,
        title: format!("browser {action}"),
        wants_screenshot_image,
    })
}

async fn run_cli(args: &[String], ctx: &ToolContext) -> Result<Value> {
    let bin = crate::agent_browser::resolve_binary()
        .ok_or_else(|| anyhow::anyhow!("agent-browser binary not found. Run action='setup'."))?;

    let session = crate::agent_browser::session_name(&ctx.session_id);

    let mut command = tokio::process::Command::new(&bin);
    command.arg("--json");
    command.arg("--session").arg(&session);

    // Opt-in reuse of the user's real Chrome profile so existing logins and
    // cookies apply. agent-browser copies the profile to a temp snapshot, so the
    // user's live profile is never mutated. This is the capability the Firefox
    // bridge got for free by driving the user's actual browser.
    if let Ok(profile) = std::env::var("JCODE_BROWSER_PROFILE")
        && !profile.is_empty()
    {
        command.arg("--profile").arg(profile);
    }

    for arg in args {
        command.arg(arg);
    }
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let output = command
        .output()
        .await
        .with_context(|| format!("Failed to run agent-browser {:?}", args))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    let parsed: Option<Value> = serde_json::from_str(&stdout).ok();

    // agent-browser reports failures in the JSON envelope even on exit 0.
    if let Some(value) = &parsed
        && value.get("success") == Some(&Value::Bool(false))
    {
        let message = value
            .get("error")
            .and_then(|e| {
                e.get("message")
                    .and_then(|m| m.as_str())
                    .or_else(|| e.as_str())
            })
            .unwrap_or("agent-browser reported failure");
        anyhow::bail!("{message}");
    }

    if !output.status.success() {
        let details = match (stdout.is_empty(), stderr.is_empty()) {
            (false, false) => format!("{stderr}\n{stdout}"),
            (true, false) => stderr,
            (false, true) => stdout,
            (true, true) => "agent-browser failed with no output".to_string(),
        };
        anyhow::bail!("{details}");
    }

    Ok(match parsed {
        Some(Value::Object(map)) => map
            .get("data")
            .cloned()
            .unwrap_or_else(|| Value::Object(map.clone())),
        Some(other) => other,
        None if stdout.is_empty() => json!({ "ok": true }),
        None => json!({ "raw": stdout }),
    })
}

async fn screenshot_with_image(plan: CommandPlan, ctx: &ToolContext) -> Result<ToolOutput> {
    let result = run_cli(&plan.args, ctx).await?;
    let saved = result
        .get("path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);

    let mut output = ToolOutput::new(match &saved {
        Some(path) => format!("Captured browser screenshot to {}.", path.display()),
        None => "Captured browser screenshot.".to_string(),
    })
    .with_title(plan.title)
    .with_metadata(result.clone());

    if let Some(path) = saved
        && let Ok(bytes) = tokio::fs::read(&path).await
    {
        output = output.with_labeled_image(
            "image/png",
            STANDARD.encode(&bytes),
            format!("browser screenshot: {}", path.display()),
        );
        let _ = tokio::fs::remove_file(&path).await;
    }

    Ok(output)
}

fn render_output(action: &str, title: String, result: Value) -> ToolOutput {
    let body = match action {
        "snapshot" | "interactables" => result
            .get("snapshot")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| pretty(&result)),
        "get_content" => result
            .get("text")
            .or_else(|| result.get("html"))
            .or_else(|| result.get("title"))
            .or_else(|| result.get("snapshot"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| pretty(&result)),
        "eval" => match result.get("result") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => pretty(other),
            None => pretty(&result),
        },
        "list_tabs" => format_tabs(&result),
        _ => pretty(&result),
    };

    ToolOutput::new(body)
        .with_title(title)
        .with_metadata(result)
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn format_tabs(result: &Value) -> String {
    let Some(tabs) = result.get("tabs").and_then(|v| v.as_array()) else {
        return pretty(result);
    };
    if tabs.is_empty() {
        return "No open tabs.".to_string();
    }
    let mut lines = Vec::new();
    for tab in tabs {
        let marker = if tab.get("active") == Some(&Value::Bool(true)) {
            "*"
        } else {
            " "
        };
        // agent-browser >=0.30 returns stable string handles in `tabId`; older
        // builds return a positional `index`.
        let handle = tab
            .get("tabId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                tab.get("index")
                    .and_then(|v| v.as_i64())
                    .map(|i| format!("t{i}"))
            })
            .unwrap_or_else(|| "?".into());
        let title = tab.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
        lines.push(format!("{marker} {handle}  {title}  {url}"));
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "agent_browser_provider_tests.rs"]
mod agent_browser_provider_tests;
