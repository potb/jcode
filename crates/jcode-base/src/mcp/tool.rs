//! MCP Tool - wraps MCP server tools for jcode's tool system

use super::manager::McpManager;
use super::protocol::{ContentBlock, McpToolDef};
use anyhow::Result;
use async_trait::async_trait;
use jcode_tool_core::{Tool, ToolContext};
use jcode_tool_types::ToolOutput;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A tool that proxies to an MCP server
pub struct McpTool {
    server_name: String,
    tool_def: McpToolDef,
    manager: Arc<RwLock<McpManager>>,
}

impl McpTool {
    pub fn new(
        server_name: String,
        tool_def: McpToolDef,
        manager: Arc<RwLock<McpManager>>,
    ) -> Self {
        Self {
            server_name,
            tool_def,
            manager,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        // This will be overridden in registration with prefixed name
        &self.tool_def.name
    }

    fn description(&self) -> &str {
        self.tool_def.description.as_deref().unwrap_or("MCP tool")
    }

    fn parameters_schema(&self) -> Value {
        self.tool_def.input_schema.clone()
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let mut input = if input.is_null() {
            Value::Object(serde_json::Map::new())
        } else {
            input
        };
        // `intent` is a jcode-injected display-only parameter (see
        // ensure_intent_in_schema). Strip it before forwarding unless the
        // MCP server's own schema declares an `intent` property.
        let server_declares_intent = self
            .tool_def
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .is_some_and(|p| p.contains_key("intent"));
        if !server_declares_intent && let Some(object) = input.as_object_mut() {
            object.remove("intent");
        }
        let manager = self.manager.read().await;
        let result = manager
            .call_tool(&self.server_name, &self.tool_def.name, input)
            .await?;

        // Convert MCP content blocks to output string
        let mut output_parts = Vec::new();
        for block in result.content {
            match block {
                ContentBlock::Text { text } => {
                    output_parts.push(text);
                }
                ContentBlock::Image { data, mime_type } => {
                    output_parts.push(format!("[Image: {} ({} bytes)]", mime_type, data.len()));
                }
                ContentBlock::Resource { resource } => {
                    if let Some(text) = resource.text {
                        output_parts.push(text);
                    } else if let Some(blob) = resource.blob {
                        output_parts.push(format!(
                            "[Resource: {} ({} bytes)]",
                            resource.uri,
                            blob.len()
                        ));
                    } else {
                        output_parts.push(format!("[Resource: {}]", resource.uri));
                    }
                }
            }
        }

        let output = output_parts.join("\n");
        let title = format!("mcp:{}:{}", self.server_name, self.tool_def.name);

        if result.is_error {
            Ok(ToolOutput::new(format!("Error: {}", output)).with_title(title))
        } else {
            Ok(ToolOutput::new(output).with_title(title))
        }
    }
}

/// Create tools from an MCP manager
pub async fn create_mcp_tools(manager: Arc<RwLock<McpManager>>) -> Vec<(String, Arc<dyn Tool>)> {
    let mgr = manager.read().await;
    let all_tools = mgr.all_tools().await;
    drop(mgr);

    let mut tools = Vec::new();
    for (server_name, tool_def) in all_tools {
        let prefixed_name = mcp_tool_name(&server_name, &tool_def.name);
        let mcp_tool = McpTool::new(server_name, tool_def, Arc::clone(&manager));
        tools.push((prefixed_name, Arc::new(mcp_tool) as Arc<dyn Tool>));
    }
    tools
}

/// Build proxy tools for a single server from cached schemas, without requiring
/// a live connection. Used to advertise a server's tools immediately at spawn
/// (the proxy connects on first call). The returned tools are functionally
/// identical to live ones; only their definitions come from the disk cache.
pub fn create_mcp_tools_from_cached(
    server_name: &str,
    tool_defs: &[McpToolDef],
    manager: Arc<RwLock<McpManager>>,
) -> Vec<(String, Arc<dyn Tool>)> {
    tool_defs
        .iter()
        .map(|tool_def| {
            let prefixed_name = mcp_tool_name(server_name, &tool_def.name);
            let mcp_tool = McpTool::new(
                server_name.to_string(),
                tool_def.clone(),
                Arc::clone(&manager),
            );
            (prefixed_name, Arc::new(mcp_tool) as Arc<dyn Tool>)
        })
        .collect()
}

/// Maximum tool-name length accepted by the strictest provider we target.
/// Anthropic rejects anything outside `^[a-zA-Z0-9_-]{1,128}$`, and the OpenAI
/// function-calling schema applies the same 128-character limit.
const MAX_TOOL_NAME_LEN: usize = 128;

/// Rewrite one segment of an MCP tool name (a server name or a remote tool
/// name) into the `[a-zA-Z0-9_-]` alphabet that providers accept.
///
/// MCP server names come from user config and remote tool names come from the
/// server itself, so both can legally contain spaces, dots, slashes, or
/// non-ASCII text. Those characters reach the provider verbatim in the
/// advertised tool name and get the whole request rejected (issue #895:
/// `tools.18.custom.name: String should match pattern
/// '^[a-zA-Z0-9_-]{1,128}$'`). Every disallowed character becomes `_`; the
/// mapping is deliberately not reversible because nothing needs to reverse it.
/// `McpTool` keeps the original server and tool names internally and dispatches
/// with those, so only the advertised name changes.
pub fn sanitize_mcp_name_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The registry prefix under which one MCP server's tools are advertised.
/// Callers that filter or unregister a server's tools must use this rather than
/// interpolating the raw server name, or they will miss every server whose name
/// needed sanitizing.
pub fn mcp_tool_prefix(server_name: &str) -> String {
    format!("mcp__{}__", sanitize_mcp_name_segment(server_name))
}

/// The provider-safe advertised name for one MCP tool.
///
/// Both segments are sanitized and the result is truncated to
/// [`MAX_TOOL_NAME_LEN`]. Truncation is safe on a character boundary because
/// sanitization has already reduced the string to single-byte ASCII.
pub fn mcp_tool_name(server_name: &str, tool_name: &str) -> String {
    let mut name = format!(
        "mcp__{}__{}",
        sanitize_mcp_name_segment(server_name),
        sanitize_mcp_name_segment(tool_name)
    );
    if name.len() > MAX_TOOL_NAME_LEN {
        name.truncate(MAX_TOOL_NAME_LEN);
    }
    name
}

#[cfg(test)]
mod tool_name_tests {
    use super::*;

    /// Every character a provider would reject has to be gone, and the parts
    /// that are already legal have to survive untouched.
    #[test]
    fn sanitizes_only_the_characters_providers_reject() {
        assert_eq!(sanitize_mcp_name_segment("plain-name_1"), "plain-name_1");
        assert_eq!(sanitize_mcp_name_segment("my server"), "my_server");
        assert_eq!(sanitize_mcp_name_segment("a.b/c:d"), "a_b_c_d");
        assert_eq!(sanitize_mcp_name_segment("ação"), "a__o");
    }

    /// Issue #895: a configured server name containing a space produced an
    /// advertised tool name that Anthropic and OpenAI both rejected with
    /// `String should match pattern '^[a-zA-Z0-9_-]{1,128}$'`.
    #[test]
    fn server_names_with_spaces_produce_provider_safe_tool_names() {
        let name = mcp_tool_name("My MCP Server", "read file");
        assert_eq!(name, "mcp__My_MCP_Server__read_file");
        assert!(is_provider_safe(&name), "not provider-safe: {name}");
    }

    /// The prefix used for filtering and unregistration must line up with the
    /// names actually registered, otherwise disconnect leaks stale tools.
    #[test]
    fn prefix_matches_the_registered_name() {
        let server = "My MCP Server";
        let prefix = mcp_tool_prefix(server);
        assert_eq!(prefix, "mcp__My_MCP_Server__");
        assert!(mcp_tool_name(server, "do thing").starts_with(&prefix));
    }

    /// Anthropic caps names at 128 characters, so an MCP server with a very
    /// long name must not push us over the limit.
    #[test]
    fn long_names_are_truncated_to_the_provider_limit() {
        let name = mcp_tool_name(&"s".repeat(200), &"t".repeat(200));
        assert_eq!(name.len(), MAX_TOOL_NAME_LEN);
        assert!(is_provider_safe(&name), "not provider-safe: {name}");
    }

    fn is_provider_safe(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= MAX_TOOL_NAME_LEN
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }
}

#[cfg(all(test, unix))]
mod space_named_server_tests {
    use super::*;
    use crate::mcp::{McpConfig, McpServerConfig};
    use jcode_tool_core::{ToolContext, ToolExecutionMode};
    use std::io::Write;

    /// Minimal stdio MCP server: answers initialize, tools/list, tools/call.
    /// The advertised remote tool name also contains a space, so both halves of
    /// the prefixed name need sanitizing.
    fn write_fake_mcp_server(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("fake-mcp-server.sh");
        let script = r##"#!/bin/sh
while IFS= read -r line; do
  id=$(echo "$line" | grep -o '"id":[0-9]*' | grep -o '[0-9]*' | head -1)
  case "$line" in
    *'"initialize"'*)
      echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0.0.1"}}}'
      ;;
    *'"tools/list"'*)
      echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"tools":[{"name":"read note","description":"fake tool","inputSchema":{"type":"object"}}]}}'
      ;;
    *'"tools/call"'*)
      echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"content":[{"type":"text","text":"note read"}],"isError":false}}'
      ;;
    *'"shutdown"'*)
      exit 0
      ;;
  esac
done
"##;
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        drop(file);
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn test_context() -> ToolContext {
        ToolContext {
            session_id: "test-session".to_string(),
            message_id: "test-message".to_string(),
            tool_call_id: "test-call".to_string(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    /// Issue #895: with a server configured as `My MCP Server`, jcode advertised
    /// `mcp__My MCP Server__read note`, which every provider rejected. The
    /// advertised name must now be provider-safe while the tool still dispatches
    /// to the server under its real, unsanitized name.
    #[tokio::test]
    async fn space_named_server_advertises_a_safe_name_and_still_dispatches() {
        let temp = tempfile::tempdir().unwrap();
        let command = write_fake_mcp_server(temp.path())
            .to_string_lossy()
            .to_string();

        let server_name = "My MCP Server";
        let mut config = McpConfig::default();
        config.servers.insert(
            server_name.to_string(),
            McpServerConfig {
                command,
                args: vec![],
                env: std::collections::HashMap::new(),
                shared: false,
                transport: None,
                url: None,
                headers: std::collections::HashMap::new(),
                enabled: None,
                disabled: None,
            },
        );
        let server_config = config.servers.get(server_name).unwrap().clone();
        let manager = Arc::new(RwLock::new(McpManager::with_config(config)));
        manager
            .read()
            .await
            .connect(server_name, &server_config)
            .await
            .expect("fake MCP server must connect");

        let tools = create_mcp_tools(Arc::clone(&manager)).await;
        assert_eq!(tools.len(), 1, "one tool from the fake server");
        let (advertised, tool) = &tools[0];
        assert_eq!(advertised, "mcp__My_MCP_Server__read_note");
        assert!(
            advertised
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "advertised name must satisfy the provider pattern: {advertised}"
        );

        // Dispatch still reaches the server, which only knows the raw names.
        let output = tool
            .execute(serde_json::json!({}), test_context())
            .await
            .expect("call through the space-named server");
        assert!(
            output.output.contains("note read"),
            "unexpected output: {}",
            output.output
        );

        manager.read().await.disconnect_all().await;
    }
}
