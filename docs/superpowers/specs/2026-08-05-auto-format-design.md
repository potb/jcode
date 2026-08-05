# Auto-Format on Agent Edits — Design

Date: 2026-08-05
Status: approved (brainstorming session)

## Goal

When the agent writes or edits a file, format it automatically with the
project's own formatter (opencode-style). Never a "random default": a
formatter runs only when the project demonstrably uses it, or when it is
the language's canonical toolchain formatter.

## Decisions (from brainstorming)

| Question | Decision |
|---|---|
| Enablement policy | opencode-parity + config overrides: evidence-gated formatters need project evidence; canonical toolchain formatters run on PATH presence. `[formatter]` config can disable/override/add. |
| Model feedback | One-line notice appended to tool output: `formatted with <name>`. No diff. |
| Catalog scope | Mirror LSP-supported languages: rustfmt, gofmt, prettier, biome, ruff, uv, clang-format. |
| Ordering vs LSP | Format first, then LSP diagnostics, so diagnostics see final content. |

## Catalog v1

| Formatter | Extensions | Evidence required | Command |
|---|---|---|---|
| rustfmt | rs | binary on PATH (canonical) | `rustfmt --edition 2024 $FILE` (edition read from the walked-up `Cargo.toml` when present, default 2024) |
| gofmt | go | binary on PATH (canonical) | `gofmt -w $FILE` |
| prettier | js jsx mjs cjs ts tsx mts cts html htm css scss sass less vue svelte json jsonc yaml yml toml xml md mdx graphql gql | `prettier` in dependencies/devDependencies of a `package.json` found walking up; binary from nearest `node_modules/.bin/prettier` | `prettier --write $FILE` |
| biome | same list as prettier | `biome.json`/`biome.jsonc` found walking up; binary from nearest `node_modules/.bin/biome` | `biome format --write $FILE` |
| ruff | py pyi | `ruff` on PATH AND config evidence: `[tool.ruff]` in a walked-up `pyproject.toml`, or `ruff.toml`/`.ruff.toml` present, or `ruff` mentioned in `requirements.txt`/`Pipfile` | `ruff format $FILE` |
| uv | py pyi | only when ruff is NOT enabled; `uv` on PATH and `uv format --help` exits 0 | `uv format -- $FILE` |
| clang-format | c h cpp hpp cc hh cxx hxx m mm | `.clang-format` found walking up AND binary on PATH | `clang-format -i $FILE` |

Evidence resolution walks up from the file's directory to the workspace
root (session working_dir fallback), like LSP root-marker resolution.

## Architecture

### New crate: `crates/jcode-fmt`

Mirrors `jcode-lsp`'s shape. Depends on `tokio`, `anyhow`, `serde`,
`serde_json`, `jcode-config-types`, `jcode-logging`. No app-core dependency.

Public API:

```rust
pub fn configure(cfg: jcode_config_types::FormatterConfig); // process-global, like jcode_lsp::configure
pub fn is_enabled() -> bool;                                 // master switch
/// Format the file in place with every enabled formatter matching its
/// extension (catalog order). Returns a short notice like
/// "formatted with prettier" when at least one formatter changed or
/// processed the file, None otherwise. Total: never fails, never panics.
pub async fn format_file(path: &std::path::Path) -> Option<String>;
```

Internals:

- **Catalog** — data entries: `id`, `command: Vec<String>` (with `$FILE`
  placeholder), `extensions`, evidence check kind (path-only, package-json-dep,
  config-file, ruff-style, uv-style).
- **Evidence cache** — process-global `LazyLock<RwLock<HashMap>>` keyed by
  `(formatter_id, workspace_dir)` → `Option<Vec<String>>` (resolved command or
  disabled). PATH lookups cached like jcode-lsp. Mid-session installs need a
  config override or process restart (accepted, same as LSP).
- **Execution** — `tokio::process::Command`, cwd = workspace dir, stdin/stdout/
  stderr ignored, 5s timeout per file. Non-zero exit or timeout → log debug,
  return as if not formatted. Clean exit → emit the notice (no diffing; the
  file is canonical after a clean run whether or not bytes changed).

### Config

`[formatter]` section in config.toml (jcode-config-types), mirroring `[lsp]`:

```toml
[formatter]
enabled = true                    # master switch, default true

[formatter.servers.prettier]
disabled = false                  # disable a built-in
command = ["prettier", "--write", "$FILE"]  # override / custom
extensions = ["ts", "tsx"]
```

Unknown ids define custom formatters (command + extensions required, no
evidence gate beyond the command's binary resolving). Built-ins can be
partially overridden. Reuse `LspServerConfig`-style struct:
`FormatterConfig { enabled, servers: HashMap<String, FormatterServerConfig> }`,
`FormatterServerConfig { disabled, command, extensions }`.

### Wiring (jcode-app-core)

In `tool/lsp_feedback.rs` (rename to `tool/file_feedback.rs` or keep module
and add sibling fn): after the disk write and BEFORE `diagnostics_after_write`:

```rust
pub(crate) async fn format_after_write(path: &Path) -> Option<String>
```

Call sites: write.rs, edit.rs, multiedit.rs, apply_patch.rs — same spots as
LSP diagnostics. Output order in the tool result:

```
<normal tool output>
formatted with prettier

<diagnostics file="...">...</diagnostics>
```

apply_patch: formats the same first-5-files set, sharing the existing 6s
total budget (format + diagnostics both inside it).

Read path untouched. No formatting on `lsp` tool rename (rename output is
server-shaped, not agent-shaped).

## Error handling

- Formatter crash/non-zero exit/timeout → skip silently (debug log), tool
  output unchanged.
- Binary vanishes mid-session → execution fails → skip silently.
- Formatter rewrites file but LSP buffer is stale → the subsequent
  `diagnostics_block` didChange sends the fresh disk content (it reads the
  file), so diagnostics stay coherent.
- Never formats: binary files, files >1MB (reuse jcode-lsp thresholds).

## Testing

- **Unit** (jcode-fmt): evidence gating per formatter kind (package.json dep
  detection, biome config walk-up, ruff pyproject `[tool.ruff]` parse, uv
  fallback suppression when ruff enabled), config merge
  (disable/override/custom), `$FILE` substitution.
- **Integration** (jcode-fmt): a fake formatter (shell script or tiny bin
  writing uppercase content) registered via config custom entry; assert file
  content changed on disk, notice returned, timeout path (sleeping script)
  returns None in bounded time, non-zero exit returns None.
- **Dogfood**: prettier on a ts scratch workspace, rustfmt on jcode itself
  (both installed here).

## Rollout

Default on (`[formatter] enabled = true`). Kill switch. No evidence = no
formatter = invisible.

## Non-goals (v1)

- Formatters beyond the LSP language set (ktlint, mix, dart, etc.) — addable
  via config custom entries or a later catalog bump.
- LSP textDocument/formatting as a fallback formatter.
- Format-on-read or bulk format commands.
- Diff reporting of formatter changes.
