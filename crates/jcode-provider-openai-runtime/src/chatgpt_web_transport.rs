//! Browser transport for the ChatGPT web route.
//!
//! The ChatGPT web model drives a real, logged-in chatgpt.com session. That was
//! originally possible only through the Firefox Agent Bridge, which forks a tab
//! from the user's running Firefox. This module keeps that behavior and adds an
//! agent-browser backend, so the route no longer depends on Firefox alone.
//!
//! The page-driving logic is identical across backends: only session setup,
//! teardown, and the four DOM primitives differ.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Which browser backend serves the ChatGPT web route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebBackend {
    /// Fork a tab from the user's running Firefox via the agent bridge.
    FirefoxBridge,
    /// Drive Chrome through the agent-browser CLI.
    AgentBrowser,
}

impl WebBackend {
    /// Resolve the backend for this run.
    ///
    /// Firefox remains the default because it is the configuration this route
    /// was built and proven against. agent-browser is opt-in until it has the
    /// same track record.
    pub(crate) fn resolve() -> Self {
        match std::env::var("JCODE_CHATGPT_WEB_BACKEND")
            .ok()
            .as_deref()
            .map(str::trim)
        {
            Some("agent-browser") | Some("agent_browser") | Some("chrome") => {
                WebBackend::AgentBrowser
            }
            _ => WebBackend::FirefoxBridge,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            WebBackend::FirefoxBridge => "firefox",
            WebBackend::AgentBrowser => "chrome",
        }
    }
}

/// Chrome profile whose login state the agent-browser backend reuses.
///
/// chatgpt.com sits behind a bot check that a fresh automation profile does not
/// pass, so this must name a profile that is already signed in.
fn chrome_profile() -> String {
    std::env::var("JCODE_CHATGPT_WEB_PROFILE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Default".to_string())
}

fn next_session_name() -> String {
    let sequence = SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("jcode-chatgpt-web-{millis}-{sequence}")
}

/// An open ChatGPT page, plus the transport needed to drive it.
pub(crate) struct WebSession {
    backend: WebBackend,
    /// Firefox: the forked tab id. agent-browser: unused.
    tab_id: u64,
    /// Firefox: the fork name. agent-browser: the session name.
    name: String,
}

impl WebSession {
    pub(crate) fn backend(&self) -> WebBackend {
        self.backend
    }

    /// Open chatgpt.com in an isolated, temporary browsing context.
    pub(crate) async fn open(backend: WebBackend, url: &str) -> Result<Self> {
        match backend {
            WebBackend::FirefoxBridge => Self::open_firefox(url).await,
            WebBackend::AgentBrowser => Self::open_agent_browser(url).await,
        }
    }

    async fn open_firefox(url: &str) -> Result<Self> {
        let source = firefox::command("getActiveTab", json!({}))
            .await
            .context("Failed to find a Firefox tab to duplicate for ChatGPT")?;
        let source_tab_id = source
            .get("tabId")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("Browser bridge did not return an active tab id"))?;

        let name = next_session_name();
        let fork = firefox::command(
            "fork",
            json!({ "tabId": source_tab_id, "paths": [{ "name": name }] }),
        )
        .await
        .context("Failed to create a temporary Firefox tab for ChatGPT")?;
        let tab_id = fork
            .get("forks")
            .and_then(Value::as_array)
            .and_then(|forks| forks.first())
            .and_then(|fork| fork.get("tabId"))
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                anyhow::anyhow!("Browser bridge did not return the forked ChatGPT tab id")
            })?;

        let session = Self {
            backend: WebBackend::FirefoxBridge,
            tab_id,
            name,
        };

        if let Err(err) = firefox::command(
            "navigate",
            json!({ "tabId": tab_id, "url": url, "wait": true }),
        )
        .await
        {
            let _ = session.close().await;
            return Err(err).context("Failed to open ChatGPT in the temporary Firefox tab");
        }

        Ok(session)
    }

    async fn open_agent_browser(url: &str) -> Result<Self> {
        let status = jcode_base::agent_browser::inspect_status().await;
        if !status.ready {
            anyhow::bail!(
                "The ChatGPT web route is set to the agent-browser backend, but it is not ready: {}. Run `jcode browser setup`.",
                if status.diagnostics.is_empty() {
                    "unknown reason".to_string()
                } else {
                    status.diagnostics.join("; ")
                }
            );
        }

        let session = Self {
            backend: WebBackend::AgentBrowser,
            tab_id: 0,
            name: next_session_name(),
        };

        session
            .agent_browser_call(&["open".to_string(), url.to_string()], Duration::from_secs(120))
            .await
            .context(
                "Failed to open ChatGPT in agent-browser. Confirm the Chrome profile named by JCODE_CHATGPT_WEB_PROFILE is signed in at chatgpt.com",
            )?;

        Ok(session)
    }

    /// Close the temporary context, clearing the prompt content it held.
    pub(crate) async fn close(&self) -> Result<()> {
        match self.backend {
            WebBackend::FirefoxBridge => self.close_firefox().await,
            WebBackend::AgentBrowser => {
                self.agent_browser_call(&["close".to_string()], Duration::from_secs(30))
                    .await
                    .context("Failed to close the temporary agent-browser ChatGPT session")?;
                Ok(())
            }
        }
    }

    async fn close_firefox(&self) -> Result<()> {
        match firefox::command("killFork", json!({ "fork": self.name })).await {
            Ok(_) => Ok(()),
            Err(close_err) => {
                firefox::command(
                    "navigate",
                    json!({ "tabId": self.tab_id, "url": "about:blank", "wait": true }),
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to close the owned browser tab ({close_err:#}) and failed to clear its sensitive prompt content"
                    )
                })?;
                Err(close_err).context(
                    "Failed to close the owned browser tab; its sensitive content was cleared to about:blank",
                )
            }
        }
    }

    /// Wait for a selector to appear.
    pub(crate) async fn wait_for(&self, selector: &str, timeout: Duration) -> Result<()> {
        match self.backend {
            WebBackend::FirefoxBridge => {
                firefox::command(
                    "waitFor",
                    json!({
                        "tabId": self.tab_id,
                        "selector": selector,
                        "timeout": timeout.as_millis() as u64,
                    }),
                )
                .await?;
            }
            WebBackend::AgentBrowser => {
                self.agent_browser_call(
                    &[
                        "wait".to_string(),
                        selector.to_string(),
                        "--timeout".to_string(),
                        timeout.as_millis().to_string(),
                    ],
                    timeout + Duration::from_secs(15),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Evaluate a script written as a function body and return its result.
    pub(crate) async fn evaluate(&self, script: &str) -> Result<Value> {
        match self.backend {
            WebBackend::FirefoxBridge => {
                let output = firefox::command(
                    "evaluate",
                    json!({ "tabId": self.tab_id, "script": script }),
                )
                .await?;
                output.get("result").cloned().ok_or_else(|| {
                    anyhow::anyhow!("Browser evaluate response did not contain a result")
                })
            }
            WebBackend::AgentBrowser => {
                // agent-browser evaluates an expression, so a function-body
                // script with a top-level `return` must be wrapped.
                let wrapped = format!("(() => {{\n{script}\n}})()");
                let encoded = base64_encode(wrapped.as_bytes());
                let value = self
                    .agent_browser_call(
                        &["eval".to_string(), "-b".to_string(), encoded],
                        Duration::from_secs(60),
                    )
                    .await?;
                Ok(value.get("result").cloned().unwrap_or(Value::Null))
            }
        }
    }

    /// Click an element.
    pub(crate) async fn click(&self, selector: &str) -> Result<()> {
        match self.backend {
            WebBackend::FirefoxBridge => {
                firefox::command(
                    "click",
                    json!({ "tabId": self.tab_id, "selector": selector }),
                )
                .await?;
            }
            WebBackend::AgentBrowser => {
                self.agent_browser_call(
                    &["click".to_string(), selector.to_string()],
                    Duration::from_secs(60),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Replace an input's contents.
    pub(crate) async fn fill(&self, selector: &str, value: &str) -> Result<()> {
        match self.backend {
            WebBackend::FirefoxBridge => {
                firefox::command(
                    "fillForm",
                    json!({
                        "tabId": self.tab_id,
                        "fields": [{ "selector": selector, "value": value }]
                    }),
                )
                .await?;
            }
            WebBackend::AgentBrowser => {
                self.agent_browser_call(
                    &["fill".to_string(), selector.to_string(), value.to_string()],
                    Duration::from_secs(120),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Append to an input without clearing it.
    pub(crate) async fn append(&self, selector: &str, text: &str) -> Result<()> {
        match self.backend {
            WebBackend::FirefoxBridge => {
                firefox::command(
                    "type",
                    json!({
                        "tabId": self.tab_id,
                        "selector": selector,
                        "text": text,
                        "clear": false,
                        "append": true
                    }),
                )
                .await?;
            }
            WebBackend::AgentBrowser => {
                self.agent_browser_call(
                    &["type".to_string(), selector.to_string(), text.to_string()],
                    Duration::from_secs(120),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Invoke the agent-browser CLI for this session.
    ///
    /// `--profile` and `--headed` are passed on every call, not just the first.
    /// agent-browser resolves the browser per invocation, so omitting them later
    /// silently attaches to a different, logged-out browser.
    async fn agent_browser_call(&self, args: &[String], timeout: Duration) -> Result<Value> {
        let binary = jcode_base::agent_browser::resolve_binary().ok_or_else(|| {
            anyhow::anyhow!("agent-browser is not installed. Run `jcode browser setup`.")
        })?;

        let mut command = tokio::process::Command::new(binary);
        command
            .arg("--json")
            .arg("--session")
            .arg(&self.name)
            .arg("--profile")
            .arg(chrome_profile());

        // chatgpt.com's bot check does not pass in headless Chrome, so this
        // route always runs headed.
        command.arg("--headed");

        for arg in args {
            command.arg(arg);
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let output = tokio::time::timeout(timeout, command.output())
            .await
            .with_context(|| {
                format!(
                    "agent-browser {:?} timed out after {}s",
                    args,
                    timeout.as_secs()
                )
            })?
            .with_context(|| format!("Failed to run agent-browser {args:?}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let parsed: Option<Value> = serde_json::from_str(&stdout).ok();

        if let Some(value) = &parsed
            && value.get("success") == Some(&Value::Bool(false))
        {
            let message = value
                .get("error")
                .and_then(|error| {
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| error.as_str())
                })
                .unwrap_or("agent-browser reported failure");
            anyhow::bail!("{message}");
        }

        if !output.status.success() {
            let detail = match (stdout.is_empty(), stderr.is_empty()) {
                (false, false) => format!("{stderr}\n{stdout}"),
                (true, false) => stderr,
                (false, true) => stdout,
                (true, true) => format!("agent-browser {args:?} failed"),
            };
            anyhow::bail!("{detail}");
        }

        Ok(match parsed {
            Some(Value::Object(map)) => map
                .get("data")
                .cloned()
                .unwrap_or_else(|| Value::Object(map.clone())),
            Some(other) => other,
            None => json!({ "ok": true }),
        })
    }
}

/// Standard base64, used to hand arbitrary JS to the CLI without quoting risk.
///
/// Written out rather than pulling a dependency into this crate for one call.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

mod firefox {
    use super::*;

    /// Run one Firefox Agent Bridge command.
    pub(super) async fn command(action: &str, params: Value) -> Result<Value> {
        let binary = jcode_base::browser::browser_binary_path();
        if !binary.exists() {
            anyhow::bail!(
                "Browser bridge binary is not installed. Run `jcode browser setup` once, then log in at chatgpt.com in Firefox"
            );
        }

        let params = serde_json::to_string(&params)?;
        let mut command = tokio::process::Command::new(binary);
        command
            .arg(action)
            .arg(params)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let output = tokio::time::timeout(Duration::from_secs(45), command.output())
            .await
            .with_context(|| format!("Browser bridge action '{action}' timed out"))?
            .with_context(|| format!("Failed to run browser bridge action '{action}'"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !output.status.success() {
            let detail = match (stdout.is_empty(), stderr.is_empty()) {
                (false, false) => format!("{stderr}\n{stdout}"),
                (false, true) => stdout,
                (true, false) => stderr,
                (true, true) => format!("browser bridge action '{action}' failed"),
            };
            anyhow::bail!(detail);
        }
        if stdout.is_empty() {
            return Ok(json!({ "ok": true }));
        }
        serde_json::from_str(&stdout)
            .with_context(|| format!("Browser bridge action '{action}' returned invalid JSON"))
    }
}

#[cfg(test)]
#[path = "chatgpt_web_transport_tests.rs"]
mod chatgpt_web_transport_tests;
