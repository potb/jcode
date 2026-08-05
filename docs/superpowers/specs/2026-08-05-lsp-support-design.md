# LSP Support in jcode — Design

Date: 2026-08-05
Status: approved (brainstorming session)

## Goal

Give jcode the same LSP awareness opencode has, plus agent-driven navigation/refactoring:

1. Diagnostics appended to write/edit tool output so the model sees breakage immediately.
2. A generic multi-server LSP client that works with any stdio LSP server.
3. An agent-facing `lsp` tool: definition, references, hover, symbols, diagnostics, rename.

## Decisions (from brainstorming)

| Question | Decision |
|---|---|
| Server discovery | PATH-only. No auto-install. Custom servers via config. |
| Write feedback | Blocking wait with timeout; diagnostics appended to tool output. Timeout = silent skip, never stall. |
| Tool surface | One `lsp` tool with an `action` enum, rename included. |
| Server lifecycle | Lazy spawn per (server, workspace root), lives until process exit. |
| Catalog v1 | rust-analyzer, typescript-language-server, pyright, gopls, clangd (+ json/yaml servers when present). |
| Diagnostics scope | Errors inline; warnings shown only when the file has zero errors. |
| Transport crate | `async-lsp` (oxalica) + `lsp-types`. Only maintained Rust crate with real client-side support (tokio, server-initiated notifications, rust-analyzer client example). |

## Architecture

### New crate: `crates/jcode-lsp`

Depends on `async-lsp` (tokio feature), `lsp-types`, `tokio`, `anyhow`, `serde`. No dependency on app-core (app-core depends on it).

Components:

- **`ServerSpec` catalog** — data entries: `id`, `command: Vec<String>`, `extensions: Vec<&str>`, `root_markers: Vec<&str>` (e.g. `Cargo.toml`, `package.json`, `go.mod`, `compile_commands.json`), optional `initialization_options`. Built-in catalog for the v1 six; merged with user config.
- **`LspRegistry`** — process-global (`LazyLock`), like `SESSION_TOOL_POLICIES` in app-core. Maps `(server_id, workspace_root) → Arc<LspClient>`. Lazy spawn on first touch. Concurrent sessions on the same repo share one server instance (rust-analyzer RAM). Negative PATH lookups cached per process.
- **`LspClient`** — wraps an async-lsp `ServerSocket` main loop task:
  - Spawns the server process (stdio), runs `initialize`/`initialized` handshake, stores server capabilities.
  - Tracks open documents (uri → version), sends `didOpen`/`didChange` on touch.
  - Collects diagnostics per file: push (`textDocument/publishDiagnostics`) merged with pull (`textDocument/diagnostic`) when the server advertises it.
  - Request API used by the `lsp` tool: definition, references, hover, documentSymbol, workspaceSymbol, rename (returns `WorkspaceEdit`).
- **Workspace root resolution** — walk up from the file looking for the spec's `root_markers`; fall back to the session working_dir.

### Config

`[lsp]` section in `config.toml` (jcode-config-types):

```toml
[lsp]
enabled = true            # default true; silent no-op when no servers on PATH

[lsp.servers.rust-analyzer]
disabled = false          # disable a built-in
command = ["rust-analyzer"]   # override / add custom server
extensions = ["rs"]
root_markers = ["Cargo.toml"]
```

Unknown server ids in config define new servers. Built-in entries can be partially overridden.

### Diagnostics on read/write/edit

Integration is an explicit helper, not a generic Registry hook (avoids fragile tool-name/input sniffing):

```rust
// jcode-lsp
pub async fn touch_background(path: &Path);            // read: warm up, didOpen, no wait
pub async fn diagnostics_block(path: &Path) -> Option<String>; // write/edit: wait + format
```

Call sites in `crates/jcode-app-core/src/tool/`:

- **read.rs** — after successful text read: fire-and-forget `touch_background`.
- **write.rs, edit.rs, multiedit.rs, apply_patch.rs** — after the file write: `didChange`, then `diagnostics_block`, append result to `ToolOutput.output` when non-empty.

Wait strategy (opencode-adapted):

- Debounce 150 ms after a version-matched `publishDiagnostics` push.
- Cap **1.5 s** when the server was already running, **5 s** when this touch spawned it.
- Pull requests run in parallel with a 3 s per-request cap; first batch with results unblocks.
- Timeout or any LSP error → return output unchanged. The whole path is wrapped; LSP can never fail a tool call.

Output format:

```
<diagnostics file="src/foo.rs">
ERROR [12:5] cannot find value `x` in this scope
</diagnostics>
```

- Errors only, max 20 per file.
- File has zero errors → warnings shown instead (`WARN [l:c] msg`), max 10.
- Cross-file: write/edit also reports files that gained new errors among open documents, capped 5 files (catches broken callers after signature changes).

Side fix while there: multiedit.rs does not publish `BusEvent::FileTouch` (write.rs/edit.rs do). Add it.

### `lsp` tool

Registered in `Registry` only when `[lsp].enabled` and at least one catalog server exists on PATH.

Schema: `action` enum + args:

| action | args | returns |
|---|---|---|
| `definition` | `file`, `line`, `column` | locations (path:line:col + line text) |
| `references` | `file`, `line`, `column` | locations, capped with count |
| `hover` | `file`, `line`, `column` | hover markdown/plaintext |
| `symbols` | `file` (document) or `query` (workspace) | symbol list with kinds + locations |
| `diagnostics` | `file` | current diagnostics for the file |
| `rename` | `file`, `line`, `column`, `new_name` | applies WorkspaceEdit, lists changed files |

`line`/`column` are 1-based in the tool schema, converted to LSP 0-based internally. Positions map through UTF-16 code units per LSP default (negotiate `positionEncoding` utf-8 when offered).

`rename` applies edits only to files inside the workspace root, publishes `FileTouch` for each changed file, and reports the full changed-file list.

## Error handling

- **Server crash**: detected by the client task; respawn once on next touch; after 3 crashes per process, disable that server and log a warning.
- **No server for filetype / not on PATH**: silent no-op for diagnostics; `lsp` tool returns a clear error naming the binary looked for.
- **Non-UTF8 / huge / binary files**: skip LSP (reuse read.rs binary/size detection thresholds).
- **Shutdown**: best-effort `shutdown` + `exit` on process exit; kill after 500 ms.

## Testing

- **Unit** (jcode-lsp): catalog extension/root-marker resolution, diagnostics formatting (errors-only, warning fallback, caps, cross-file cap), config merge (override/disable/custom).
- **Integration**: a fake LSP server test binary (tiny Rust bin speaking stdio JSON-RPC with scripted responses) exercises: handshake, didOpen → publishDiagnostics → formatted append, pull diagnostics, timeout path, crash → respawn → give-up. CI needs no real language servers.
- **Dogfood**: selfdev build, rust-analyzer against the jcode repo itself.

## Rollout

Default on (`[lsp] enabled = true`). Kill switch `enabled = false`. No server installed = feature is invisible.

## Non-goals (v1)

- Auto-installing language servers.
- Idle shutdown of servers (revisit if RAM complaints).
- Code actions / formatting / call hierarchy (v2 candidates).
- Multiple servers per extension beyond the catalog's natural overlap.
