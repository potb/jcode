# Crate Ownership and Modularization Boundaries

This document defines the target structure for keeping `jcode` modular without turning shared crates into a dumping ground. It is intentionally practical: use it when deciding whether to move a type, helper, or behavior out of the root crate.

## Goals

Primary goal: make normal development and selfdev builds faster by shrinking the root crate's recompilation surface. Structural cleanliness is valuable because it supports that compile-time goal.

- Move stable DTOs and protocol-safe state into small crates so changes in root behavior do not recompile those contracts, and changes in contracts recompile only focused dependents.
- Keep dependency-light crates dependency-light so they compile quickly and do not pull large runtime/TUI/provider graphs into unrelated builds.
- Keep root-only behavior, storage, process, TUI, server, and provider runtime logic in the root crate until a full dependency boundary can move without increasing dependency fan-out.
- Avoid cyclic dependencies and hidden coupling through broad `jcode-core` re-exports.
- Preserve serde compatibility and root re-exports during migrations unless all call sites are intentionally updated.
- Measure success by compile impact: fewer root edits, fewer root-owned DTOs, smaller dependency fan-out, and faster `cargo check --profile selfdev` / `selfdev build` after common changes.

## Ownership rules

### Type crates own stable data contracts

A `*-types` crate should contain:

- Plain data structures used by multiple crates or protocol layers.
- Serialization shape and small pure helper methods tied to the data contract.
- No filesystem, network, process, TUI, provider client, global state, or storage access.
- Dependencies limited to serde, chrono, and other type crates where necessary.

Examples: `jcode-session-types`, `jcode-side-panel-types`, `jcode-selfdev-types`, `jcode-background-types`.

### Domain behavior modules own root runtime behavior

Root modules should keep behavior when it needs:

- `crate::storage`, `crate::config`, `crate::logging`, `crate::server`, or process spawning.
- Provider HTTP clients and auth managers.
- Tokio runtime, background tasks, channels, global caches, file locks, or PID registries.
- TUI rendering and crossterm/ratatui state.

If a type has inherent methods that need these APIs, either leave the type in root or move behavior and dependencies together into a domain crate. Do not move only the struct if that forces illegal inherent impls in root.

### `jcode-core` is for genuinely shared primitives

`jcode-core` should contain:

- Cross-domain primitives that do not have an obvious domain crate yet.
- Very small, dependency-light helpers used by many crates.
- Temporary DTO staging only when creating a new domain type crate would be premature.

`jcode-core` should not accumulate every extracted DTO indefinitely. Once a cluster grows, split it into a focused domain crate.

### Compile-speed decision rule

Prefer a split when it reduces root crate churn or dependency fan-out. Do not split just to make files look tidier if the new crate adds dependencies, increases rebuild fan-out, or forces frequent cross-crate edits. A good split has at least one of these compile-time benefits:

- Common root behavior edits no longer touch stable type definitions.
- A type-only change can be checked by compiling a small type crate plus focused dependents.
- Heavy dependencies stay out of DTO crates.
- Multiple downstream crates can use a small contract without depending on the root crate.

### Re-export policy

During migrations:

1. Move the type to the target crate.
2. Keep the old root path as `pub use ...` to preserve call sites.
3. Validate focused tests and selfdev build/reload.
4. Later, remove obsolete root re-exports only after downstream crates can depend directly on the domain crate.

## Move checklist

Use this checklist for every type or pure-helper migration. Copy it into the PR/commit notes when a move is non-trivial.

1. Classify the candidate.
   - [ ] Is it a stable data contract or pure helper rather than root runtime behavior?
   - [ ] Does it have inherent methods?
   - [ ] Do those methods require root-only APIs such as storage, network clients, TUI state, process management, or globals?
   - [ ] If behavior must move too, can the full dependency boundary move without increasing fan-out?
2. Check compatibility.
   - [ ] Does its serde representation stay identical?
   - [ ] Are defaults, skips, renames, and enum discriminants preserved?
   - [ ] Are all field visibilities still appropriate?
   - [ ] Can root keep a compatibility re-export?
3. Check crate health.
   - [ ] Does the target crate already have the needed dependency policy?
   - [ ] Are new dependencies limited to type-crate-appropriate libraries, usually `serde`, `serde_json`, `chrono`, or sibling type crates?
   - [ ] Is the target crate still acyclic?
   - [ ] Did `cargo metadata`/`cargo check` avoid pulling root, TUI, provider, storage, server, or process dependencies into the type crate?
4. Validate.
   - [ ] Is there a focused test filter that covers the moved type?
   - [ ] Did `cargo check --profile selfdev -p <type-crate> -p jcode --bin jcode` pass?
   - [ ] Did relevant focused root tests pass?
   - [ ] Did `cargo fmt` pass?
   - [ ] Did selfdev build and reload pass from a clean committed HEAD?

## Dependency boundary guard

Run this guard after adding or changing any type crate dependency:

```sh
python3 scripts/check_dependency_boundaries.py
```

The guard blocks direct dependencies from `jcode-*-types` crates to root/runtime-heavy internal crates such as `jcode`, `jcode-core`, provider crates, TUI crates, protocol/runtime crates, and desktop/mobile crates. Type crates may depend on external lightweight libraries and other type crates. If a new internal dependency is needed, first decide whether it should itself be a type crate.

## Test policy

Prefer focused filters for validation. Broad filters often select unrelated stateful, timing-sensitive, or benchmark tests.

Known broad-filter hazards observed during modularization:

- `side_panel` selects unrelated pinned UI/layout and latency benchmark tests.
- `usage` selects app-display tests in addition to pure usage tests.
- `session::` selects live-attach server tests and picker behavior beyond session persistence.
- `ambient` selects TUI/helper integration tests with config and schedule state beyond ambient module persistence/runtime tests.

Document precise filters next to each domain crate/module. Broad filters are still useful for periodic sweeps, but they should not block a DTO-only extraction when precise tests and compile checks pass.

Focused validation matrix after the current DTO splits:

| Area | Fast compile check | Focused root tests used during split | Notes |
| --- | --- | --- | --- |
| Usage DTOs | `cargo check --profile selfdev -p jcode-usage-types -p jcode --bin jcode` | Prefer exact tests under usage/copilot usage modules. Avoid bare `usage` as a required gate because it selects display/UI tests too. | DTO crate owns report and local counter contracts. Runtime fetch/cache/display stay root. |
| Gateway DTOs | `cargo check --profile selfdev -p jcode-gateway-types -p jcode --bin jcode` | Focus gateway persistence/auth tests by exact test names when available. | Pairing/token HTTP/WebSocket behavior stays root. |
| Ambient DTOs | `cargo check --profile selfdev -p jcode-ambient-types -p jcode --bin jcode` | Scheduler/type consumers only. | Ambient DTO crate owns usage records only. Queue/runtime/prompt behavior stays root. |
| Ambient behavior modules | `cargo check --profile selfdev -p jcode --bin jcode` | `cargo test --profile selfdev -p jcode ambient::ambient_tests --lib`; `cargo test --profile selfdev -p jcode ambient::scheduler::tests --lib`; `cargo test --profile selfdev -p jcode ambient::runner::runner_tests --lib` | Avoid bare `ambient` as a required gate for module-only refactors because it selects cross-module TUI/config state tests. |
| Memory activity DTOs | `cargo check --profile selfdev -p jcode-memory-types -p jcode-core -p jcode --bin jcode` | `cargo test --profile selfdev -p jcode runtime_memory_log --lib`; `cargo test --profile selfdev -p jcode tui::info_widget::tests --lib` | `memory::activity` currently matches no tests, so use consumer tests. |
| Goal/todo/catchup core DTOs | `cargo check --profile selfdev -p jcode-core -p jcode --bin jcode` | Exact goal/todo/catchup filters if behavior changes. | Currently small/stable enough to leave in `jcode-core`; revisit if churn grows. |


## Compile baseline observations

Measured on 2026-04-30 with `scripts/dev_cargo.sh check --profile selfdev -p jcode --bin jcode` after the compile-speed boundary doc commit. This is a coarse mtime-touch benchmark, not a full statistical study, but it is enough to guide priorities.

| Scenario | Observed time | Interpretation |
| --- | ---: | --- |
| No-op check after recent doc-only commit | ~65.8s | Environment/cache state can dominate a first check. Treat as warmup/noise baseline, not pure no-op steady state. |
| Touch root behavior module (usage, then at src/usage.rs in the root crate, now `crates/jcode-base/src/usage/`) | ~6.25s | A root-only behavior edit can be relatively cheap when dependencies are already built. |
| Touch the usage DTO module (then `jcode-core`, now `crates/jcode-usage-types/`) | ~65.35s | Editing `jcode-core` invalidates broad downstream dependents. Avoid adding high-churn domain DTOs to `jcode-core`. |

The paths above are the ones measured on 2026-04-30 and are kept as recorded; both modules have since moved, which is exactly the outcome the measurement argued for.

Implication: the compile-speed target is not simply "move things out of root". Moving stable, low-churn contracts out of root is good, but putting many high-churn domain DTOs into `jcode-core` can be counterproductive because `jcode-core` has high fan-out. Prefer focused leaf crates such as `jcode-usage-types`, `jcode-gateway-types`, and `jcode-ambient-types` for domain DTOs that are likely to change.

## `jcode-core` fan-out audit

This audit is resolved. `jcode-core` now contains only general utilities — `console`, `env`, `fs`, `id`, `output_style`, `panic_util`, `stdin_detect`, `util` — and no domain DTO modules at all. Every module the table below staged for a move has moved.

`jcode-base` keeps one-line compatibility re-exports (`crates/jcode-base/src/env.rs`, `id.rs`, `stdin_detect.rs` are each a single `pub use jcode_core::*;`), so call sites did not have to change.

`jcode-core` should still be treated as a high-fan-out crate: it is cheap to compile but broadly depended on, so a touch there invalidates wide downstream checks.

Standing rule from this audit:

1. Keep stable general utilities in `jcode-core`.
2. Do not add domain DTOs to `jcode-core`. Give them a focused leaf `*-types` crate instead.

Historical record of the staging modules and where each one landed:

| Module | Contents | Outcome |
| --- | --- | --- |
| `ambient_usage_types` | Ambient scheduler usage records/rate limit DTOs | moved to `jcode-ambient-types` |
| `copilot_usage_types` | Local Copilot usage counters | moved to `jcode-usage-types` |
| `gateway_types` | Paired device and pairing code persisted records | moved to `jcode-gateway-types`; pairing/token behavior stayed behavioral |
| `memory_types` | Memory activity DTOs | moved to `jcode-memory-types` |
| `usage_types` | Provider usage report DTOs | moved to `jcode-usage-types`; fetch/cache/display stayed behavioral |
| `catchup_types`, `goal_types`, `todo_types` | Catch-up, goal, and todo state DTOs | left `jcode-core`; the optional grouped task-state crate was not created, and `jcode-task-types` exists for task DTOs |
| `env`, `id`, `panic_util`, `stdin_detect`, `util` | General utilities | stayed in `jcode-core`, as intended. `util` should still not become a catch-all |

## Target domain type crates

The four planned domain type crates all exist: `jcode-usage-types`, `jcode-gateway-types`, `jcode-ambient-types`, `jcode-memory-types`, alongside `jcode-auth-types`, `jcode-background-types`, `jcode-batch-types`, `jcode-config-types`, `jcode-message-types`, `jcode-selfdev-types`, `jcode-session-types`, `jcode-side-panel-types`, `jcode-task-types`, and `jcode-tool-types`.

Remaining opportunities, not commitments:

- `jcode-ambient-types` holds usage records and rate-limit DTOs only. `AmbientState` still lives in `crates/jcode-app-core/src/ambient.rs` because its load/save/record behavior is only partly separated; `persistence.rs` now owns the lock and queue, so the remaining blocker is narrower than when this was written.
- `GatewayConfig` ownership (config crate vs gateway types) is still undecided.
- Mobile gateway protocol-safe DTOs have not been needed yet.

## Big module refactor targets

All four modules listed here have been split. Each is now a parent module that declares submodules and re-exports them, rather than one large file. The sections below record what landed, so the split shape is documented rather than re-planned.

### Session (`crates/jcode-base/src/session.rs` + `session/`)

Split into `model.rs`, `persistence.rs`, `journal.rs`, `crash.rs`, `render.rs`, `memory_profile.rs`, `maintenance.rs`, `storage_paths.rs`, `load_telemetry.rs`. Startup stubs landed as `Session::load_startup_stub` in `persistence.rs`. DTOs live in `jcode-session-types`.

The parent file still holds the `Session` struct itself and is the largest remaining file in the group, so further extraction is possible but no longer urgent.

### Ambient (`crates/jcode-app-core/src/ambient.rs` + `ambient/`)

Split into `manager.rs`, `runner.rs`, `scheduler.rs`, `schedule_window.rs`, `persistence.rs` (lock + scheduled queue), `directives.rs`, `prompt.rs`, `paths.rs`, `gates.rs`, `headroom.rs`, `cycle_significance.rs`.

`AmbientState` still lives in the parent module. The original constraint still applies in part: do not move it into `jcode-ambient-types` until its load/save/record behavior is fully separated.

### Usage (`crates/jcode-base/src/usage.rs` + `usage/`)

Split into `provider_fetch.rs`, `openai_helpers.rs`, `cache.rs`, `snapshot.rs`, `push.rs`, `poller.rs`, `lease.rs`, `display.rs`, `accessors.rs`, `api_keys.rs`, `model.rs`. Public report DTOs live in `jcode-usage-types`.

### Gateway (`crates/jcode-base/src/gateway.rs` + `gateway/`)

Split into `registry.rs` (persistence), `auth.rs` (pairing/token and WebSocket auth), `control.rs` (shared logic behind `/remote` and `jcode pair`). HTTP routes and the WebSocket relay remain in the parent module. Persisted records live in `jcode-gateway-types`.

## Definition of “optimal enough”

The structure is good enough when:

- Each type crate has a clear domain and minimal dependency set.
- `jcode-core` contains only true primitives or documented temporary staging modules.
- Root modules no longer mix large DTO blocks, persistence, runtime orchestration, and rendering in one file.
- Every domain has focused validation commands.
- Selfdev build/reload works cleanly after every structural change.
