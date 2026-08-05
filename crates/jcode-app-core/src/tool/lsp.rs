use super::{Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const MAX_REFERENCES_SHOWN: usize = 50;

pub struct LspTool;

impl LspTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct LspInput {
    #[serde(default)]
    #[allow(dead_code)]
    intent: Option<String>,
    action: String,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    column: Option<u32>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    new_name: Option<String>,
}

/// Format a path for display: relative to `base` when it is a descendant,
/// otherwise the absolute path.
fn display_path(path: &Path, base: Option<&Path>) -> String {
    if let Some(base) = base
        && let Ok(rel) = path.strip_prefix(base)
    {
        return rel.display().to_string();
    }
    path.display().to_string()
}

fn format_location(loc: &jcode_lsp::LocationInfo, base: Option<&Path>) -> String {
    let path = display_path(&loc.path, base);
    match &loc.line_text {
        Some(text) if !text.is_empty() => {
            format!("{}:{}:{}  {}", path, loc.line, loc.column, text)
        }
        _ => format!("{}:{}:{}", path, loc.line, loc.column),
    }
}

fn format_symbol(sym: &jcode_lsp::SymbolInfo, base: Option<&Path>) -> String {
    format!(
        "{} {} — {}:{}",
        sym.kind,
        sym.name,
        display_path(&sym.path, base),
        sym.line
    )
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn description(&self) -> &str {
        "LSP queries: definition, references, hover, symbols, diagnostics, rename."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["definition", "references", "hover", "symbols", "diagnostics", "rename"],
                    "description": "LSP operation to perform."
                },
                "file": {
                    "type": "string",
                    "description": "File path. Required for all actions except workspace-wide `symbols` search (use `query` instead)."
                },
                "line": {
                    "type": "integer",
                    "description": "1-based line number. Required for definition, references, hover, rename."
                },
                "column": {
                    "type": "integer",
                    "description": "1-based column number. Required for definition, references, hover, rename."
                },
                "query": {
                    "type": "string",
                    "description": "Workspace symbol search query. Used by `symbols` when `file` is omitted."
                },
                "new_name": {
                    "type": "string",
                    "description": "New identifier name. Required for rename."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: LspInput = serde_json::from_value(input)?;

        jcode_lsp::configure(crate::config::config().lsp.clone());

        match params.action.as_str() {
            "definition" => self.locations_action(&params, &ctx, true).await,
            "references" => self.locations_action(&params, &ctx, false).await,
            "hover" => self.hover_action(&params, &ctx).await,
            "symbols" => self.symbols_action(&params, &ctx).await,
            "diagnostics" => self.diagnostics_action(&params, &ctx).await,
            "rename" => self.rename_action(&params, &ctx).await,
            other => Err(anyhow!(
                "unknown lsp action `{other}`; expected one of definition, references, hover, symbols, diagnostics, rename"
            )),
        }
    }
}

impl LspTool {
    async fn locations_action(
        &self,
        params: &LspInput,
        ctx: &ToolContext,
        is_definition: bool,
    ) -> Result<ToolOutput> {
        let file = params
            .file
            .as_ref()
            .ok_or_else(|| anyhow!("`file` is required for this action"))?;
        let line = params
            .line
            .ok_or_else(|| anyhow!("`line` is required for this action"))?;
        let column = params
            .column
            .ok_or_else(|| anyhow!("`column` is required for this action"))?;

        let path = ctx.resolve_path(Path::new(file));
        let handle = jcode_lsp::handle_for(&path).await?;
        let locations = if is_definition {
            handle.definition(line, column).await?
        } else {
            handle.references(line, column).await?
        };

        if locations.is_empty() {
            return Ok(ToolOutput::new(if is_definition {
                "No definition found.".to_string()
            } else {
                "No references found.".to_string()
            }));
        }

        let base = ctx.working_dir.as_deref();
        let total = locations.len();
        let shown = if is_definition {
            total
        } else {
            total.min(MAX_REFERENCES_SHOWN)
        };
        let mut out = String::new();
        for loc in locations.iter().take(shown) {
            out.push_str(&format_location(loc, base));
            out.push('\n');
        }
        if !is_definition && total > MAX_REFERENCES_SHOWN {
            out.push_str(&format!("… and {} more\n", total - MAX_REFERENCES_SHOWN));
        }
        Ok(ToolOutput::new(out.trim_end().to_string()))
    }

    async fn hover_action(&self, params: &LspInput, ctx: &ToolContext) -> Result<ToolOutput> {
        let file = params
            .file
            .as_ref()
            .ok_or_else(|| anyhow!("`file` is required for hover"))?;
        let line = params
            .line
            .ok_or_else(|| anyhow!("`line` is required for hover"))?;
        let column = params
            .column
            .ok_or_else(|| anyhow!("`column` is required for hover"))?;

        let path = ctx.resolve_path(Path::new(file));
        let handle = jcode_lsp::handle_for(&path).await?;
        match handle.hover(line, column).await? {
            Some(text) if !text.trim().is_empty() => Ok(ToolOutput::new(text)),
            _ => Ok(ToolOutput::new("No hover information.".to_string())),
        }
    }

    async fn symbols_action(&self, params: &LspInput, ctx: &ToolContext) -> Result<ToolOutput> {
        let base = ctx.working_dir.as_deref();
        let symbols = if let Some(file) = params.file.as_ref() {
            let path = ctx.resolve_path(Path::new(file));
            let handle = jcode_lsp::handle_for(&path).await?;
            handle.document_symbols().await?
        } else if let Some(query) = params.query.as_ref() {
            // Workspace symbol search: resolve a server from the working dir.
            let anchor = ctx
                .working_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            let handle = jcode_lsp::workspace_handle_for(&anchor).await?;
            handle.workspace_symbols(query).await?
        } else {
            return Err(anyhow!(
                "either `file` (document symbols) or `query` (workspace symbols) is required"
            ));
        };

        if symbols.is_empty() {
            return Ok(ToolOutput::new("No symbols found.".to_string()));
        }
        let out = symbols
            .iter()
            .map(|s| format_symbol(s, base))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::new(out))
    }

    async fn diagnostics_action(&self, params: &LspInput, ctx: &ToolContext) -> Result<ToolOutput> {
        let file = params
            .file
            .as_ref()
            .ok_or_else(|| anyhow!("`file` is required for diagnostics"))?;
        let path = ctx.resolve_path(Path::new(file));
        let handle = jcode_lsp::handle_for(&path).await?;
        let diags = handle.file_diagnostics().await?;
        if diags.trim().is_empty() {
            Ok(ToolOutput::new("No diagnostics.".to_string()))
        } else {
            Ok(ToolOutput::new(diags))
        }
    }

    async fn rename_action(&self, params: &LspInput, ctx: &ToolContext) -> Result<ToolOutput> {
        let file = params
            .file
            .as_ref()
            .ok_or_else(|| anyhow!("`file` is required for rename"))?;
        let line = params
            .line
            .ok_or_else(|| anyhow!("`line` is required for rename"))?;
        let column = params
            .column
            .ok_or_else(|| anyhow!("`column` is required for rename"))?;
        let new_name = params
            .new_name
            .as_ref()
            .ok_or_else(|| anyhow!("`new_name` is required for rename"))?;

        let path = ctx.resolve_path(Path::new(file));
        let handle = jcode_lsp::handle_for(&path).await?;
        let outcome = handle.rename(line, column, new_name).await?;

        let base = ctx.working_dir.as_deref();
        for changed in &outcome.changed_files {
            Bus::global().publish(BusEvent::FileTouch(FileTouch {
                session_id: ctx.session_id.clone(),
                path: changed.clone(),
                op: FileOp::Edit,
                intent: params.intent.clone().filter(|v| !v.trim().is_empty()),
                summary: Some("lsp rename".to_string()),
                detail: None,
            }));
        }

        if outcome.changed_files.is_empty() {
            return Ok(ToolOutput::new(
                "Rename applied; no files changed.".to_string(),
            ));
        }

        let list = outcome
            .changed_files
            .iter()
            .map(|p| display_path(p, base))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::new(format!(
            "Renamed to `{}` in {} file(s):\n{}",
            new_name,
            outcome.changed_files.len(),
            list
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolExecutionMode;

    fn test_ctx() -> ToolContext {
        ToolContext {
            session_id: "test-session".to_string(),
            message_id: "test-message".to_string(),
            tool_call_id: "test-call".to_string(),
            working_dir: Some(PathBuf::from("/tmp")),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    #[test]
    fn schema_declares_action_enum_and_required() {
        let tool = LspTool::new();
        let schema = tool.parameters_schema();
        let actions = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        for expected in [
            "definition",
            "references",
            "hover",
            "symbols",
            "diagnostics",
            "rename",
        ] {
            assert!(actions.contains(&expected), "missing action {expected}");
        }
        let required = schema["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(required.contains(&"action"));
    }

    #[tokio::test]
    async fn definition_without_line_errors() {
        let tool = LspTool::new();
        let input = json!({
            "action": "definition",
            "file": "src/main.rs",
            "column": 3
        });
        let err = tool
            .execute(input, test_ctx())
            .await
            .expect_err("missing line should error");
        assert!(
            err.to_string().contains("line"),
            "error should mention `line`, got: {err}"
        );
    }

    #[tokio::test]
    async fn definition_without_file_errors() {
        let tool = LspTool::new();
        let input = json!({
            "action": "definition",
            "line": 1,
            "column": 1
        });
        let err = tool
            .execute(input, test_ctx())
            .await
            .expect_err("missing file should error");
        assert!(
            err.to_string().contains("file"),
            "error should mention `file`, got: {err}"
        );
    }

    #[tokio::test]
    async fn rename_without_new_name_errors() {
        let tool = LspTool::new();
        let input = json!({
            "action": "rename",
            "file": "src/main.rs",
            "line": 1,
            "column": 1
        });
        let err = tool
            .execute(input, test_ctx())
            .await
            .expect_err("missing new_name should error");
        assert!(
            err.to_string().contains("new_name"),
            "error should mention `new_name`, got: {err}"
        );
    }

    #[tokio::test]
    async fn symbols_without_file_or_query_errors() {
        let tool = LspTool::new();
        let input = json!({ "action": "symbols" });
        let err = tool
            .execute(input, test_ctx())
            .await
            .expect_err("missing file/query should error");
        let msg = err.to_string();
        assert!(msg.contains("file") && msg.contains("query"), "{msg}");
    }

    #[tokio::test]
    async fn diagnostics_without_file_errors() {
        let tool = LspTool::new();
        let input = json!({ "action": "diagnostics" });
        let err = tool
            .execute(input, test_ctx())
            .await
            .expect_err("missing file should error");
        assert!(err.to_string().contains("file"));
    }

    #[tokio::test]
    async fn unknown_action_errors() {
        let tool = LspTool::new();
        let input = json!({ "action": "bogus" });
        let err = tool
            .execute(input, test_ctx())
            .await
            .expect_err("unknown action should error");
        assert!(err.to_string().contains("bogus"));
    }
}
