# How this fork diverges from upstream

Read this before resolving an upstream merge conflict. It records the places
where the fork **changed or removed** upstream code, because those are the only
places a merge has to pick a side. Additions cannot conflict and are not listed.

All figures are measured against the current merge base
`bf4d79bed` (`git merge-base master upstream/master`). Regenerate them with
[Refreshing the numbers](#refreshing-the-numbers) after each merge; do not trust
the counts blindly once master has moved.

## 1. Merge policy

1. **Adopt upstream's structure, keep the fork's behaviour.** Upstream refactors,
   renames, and moves are welcome. Upstream re-implementing something the fork
   already does its own way is not a conflict, it is a product decision — stop
   and ask (this rule is already in `scripts/upstream_merge_agent.sh`).
2. **Formatting-only divergence always yields to upstream.** See section 6.
3. **A deleted file that upstream modifies stays deleted** unless the fork
   actually needs it. See section 3; the deletions were orphan cleanups, so
   `git rm` is the correct resolution, not "restore because upstream touched it".
4. **Append-mostly config files**: keep both sides, fork entries after upstream
   entries. See section 5.
5. **Ratchet JSON files** (`scripts/*_budget.json`) are baselines,
   not correctness checks. Re-baseline with `python3 scripts/<script>.py --update`
   and say so in the merge commit rather than trying to shrink upstream's code.

## 2. Behaviour replacements — the fork side wins

| File | +/- | Fork commit | What upstream does | What we do |
|---|---|---|---|---|
| `crates/jcode-provider-openai-runtime/src/chatgpt_web.rs` | 73/194 | `670d6e116` | Drives the ChatGPT web route through direct Firefox-bridge calls (`ensure_browser_ready_noninteractive`, `open_chatgpt_tab`, `wait_for_editor`) | Route is backend-agnostic; the transport lives in the fork-only `chatgpt_web_transport.rs`. **Do not reintroduce direct bridge calls**; port upstream's logic onto the transport trait instead. |
| `crates/jcode-app-core/src/tool/browser.rs` | 74/40 | `a81aedb14` | Firefox is the only browser backend | Backend dispatch so agent-browser sits beside Firefox. Upstream additions belong inside the Firefox arm. |

## 3. Deletions — 17 upstream files the fork removed

This is the entry that changed most recently, and the one most likely to be
mis-resolved: earlier analyses of this fork stated "no file has ever been
deleted", which is **no longer true**.

| Commit | Count | Files |
|---|---|---|
| `b93779f8f` (#98) | 15 | `crates/jcode-app-core/src/`: `message_notifications.rs`, `protocol_memory.rs`, `protocol_tests.rs` + `protocol_tests/{comm_requests,comm_responses,core_events,misc_events,randomized}.rs`, `session_active_pids.rs`, `stdin_detect_tests.rs`, `telemetry_state.rs`, `telemetry_tests.rs`, `usage_display.rs`, `usage_openai.rs`, `usage_tests.rs` |
| `10ac93d1a` (#99) | 2 | `crates/jcode-tui/src/tui/`: `info_widget_timeline.rs`, `swarm_plan_graph.rs` |

All of them were **orphans left by the crate split**: not declared by any `mod`
statement, so they compiled into nothing. They are still orphaned *in upstream*
too — upstream's `crates/jcode-app-core/src/lib.rs` does not declare
`usage_tests` or `telemetry_state` either, it simply has not cleaned them up.

**Resolution rule:** if an upstream commit edits one of these files, the merge
surfaces a modify/delete conflict. Take the deletion (`git rm <file>`). Restoring
it adds dead code back and re-arms the `check_module_files.py` gate. The live
code that replaced each one lives in the split-out crates (for example
`crates/jcode-protocol/src/protocol_tests.rs`), so the behaviour is not lost.

## 4. Rewritten upstream surfaces — expect noisy conflicts

Large fork rewrites of upstream control flow or layout. Upstream edits here will
collide; resolve by re-applying upstream's *intent* onto the fork's structure.

| File | +/- | Nature |
|---|---|---|
| `crates/jcode-app-core/src/ambient/runner.rs` | 740/74 | Ambient mode: the largest fork feature, rewrites upstream control flow |
| `crates/jcode-tui/src/tui/app/tui_state.rs` | 485/354 | Fork state fields threaded through upstream's state struct |
| `crates/jcode-tui/src/tui/ui_input.rs` | 371/66 | Session facts moved beside the input |
| `crates/jcode-tui/src/tui/info_widget.rs` | 354/57 | Pinned usage/todos/memory sections, shrink-on-smaller-content |
| `src/cli/commands.rs` | 237/83 | Fork subcommands interleaved with upstream's |
| `crates/jcode-tui/src/tui/app/input.rs` | 119/60 | Fork keybindings |

## 5. Structural moves and append-mostly files

- `crates/jcode-tui-render/src/swarm_gallery.rs` (5/79) → helpers extracted to the
  fork-owned `crates/jcode-tui-render/src/gallery_text.rs` (295 lines): first
  `clamp_line_to_width` and friends, later `wrap_text` for the background panel
  (#46). Upstream edits to those helpers must land in `gallery_text.rs`. This file
  keeps attracting shared helpers, so check it before concluding code vanished.
- `crates/jcode-config-types/src/lib.rs` (+691/-4), `display.rs` (+247/-2),
  fork-only `cron_config.rs` (+203): append-mostly, but upstream appends to the
  same structs, so they conflict constantly. **Keep both sides**, fork fields after
  upstream fields, and never drop an upstream field to resolve.
- `scripts/dev_cargo.sh` (+496/-18) and
  `crates/jcode-app-core/src/tool/discover.rs` (+10/-7) are the only two files
  that have ever produced conflicts in *both* historical merges. Treat them as
  known hotspots.

## 6. Formatting-only divergence — always take upstream

18 modified files diverge **only** through the two whole-tree formatting commits
`e03ad77ed` (`chore(lint): make the workspace rustfmt- and clippy-clean`) and
`5d60044f4` (`chore(fmt)`, #109). They carry zero intent. The churn looks alarming
and means nothing: `server/debug_server_state.rs` shows 104/104, all of it rustfmt
reflow of one `#[cfg(test)] mod tests`; likewise `mermaid_content.rs` (51/51) and
`math/layout.rs` (21/21).

```
crates/jcode-app-core/src/server/background_tasks.rs
crates/jcode-app-core/src/server/debug_server_state.rs
crates/jcode-app-core/src/tool/session_search_index.rs
crates/jcode-app-core/src/turn_cancel_registry.rs
crates/jcode-base/src/mcp/pool.rs
crates/jcode-build-support/src/storage_helpers.rs
crates/jcode-harness-api/src/harness_api_tests/capability_coverage.rs
crates/jcode-harness-api/src/harness_api_tests/schema_snapshot.rs
crates/jcode-math/src/layout.rs
crates/jcode-message-types/src/lib.rs
crates/jcode-provider-openrouter-runtime/src/openrouter_catalog_merge_tests.rs
crates/jcode-sdk/src/client.rs
crates/jcode-sdk/src/structured.rs
crates/jcode-transport/src/unix.rs
crates/jcode-tui-mermaid/src/mermaid_content.rs
crates/jcode-tui-mermaid/src/mermaid_tests/part_02.rs
crates/jcode-tui-mermaid/src/mermaid_viewport.rs
crates/jcode-tui/src/tui/ui_tests/palette_topology.rs
```

Take upstream's side wholesale, then run `cargo fmt --all` once at the end. The
merge agent's `CHECK_CMD` enforces `cargo fmt --all -- --check`, so a formatting
slip fails the merge rather than reaching master.

## 7. Conflict resolutions already made

Kept from merge `4f05a16da` (upstream `278d4e4c4`), because they are good
precedents and would otherwise be buried in a merge commit body:

- `crates/jcode-tui/src/tui/app/tui_lifecycle.rs`: **kept both sides.** Upstream's
  reload fast-start handoff sits alongside the fork's keybinding-snapshot
  comparison (only announce a reload when bindings actually changed) and the
  fork's background-task-panel state fields.
- `crates/jcode-tui/src/tui/app/remote_tests.rs`: **took upstream.** Its
  expectation that regaining focus does a differential redraw rather than a full
  repaint is a perf change, and the old assertion was upstream behaviour, not a
  fork feature.
- `README.md`: took upstream's tagline typo fix and the Trendshift badge.

## Refreshing the numbers

```bash
MB=$(git merge-base master upstream/master)

# headline counts
git rev-list --count "$MB"..master                       # fork commits ahead
git diff --name-status "$MB" master | awk '{print $1}' | sort | uniq -c

# files ranked by upstream lines removed (what a merge must arbitrate)
git diff --numstat "$MB" master -- '*.rs' '*.sh' '*.toml' | sort -k2 -rn | head -25

# formatting-only files: touched by nothing but the whole-tree format commits
FMT=$(git log --format=%h "$MB"..master --grep='^chore(fmt)' --grep='^chore(lint)')
for f in $(git diff --name-only --diff-filter=M "$MB" master); do
  only=1
  for h in $(git log --format=%h "$MB"..master -- "$f"); do
    echo "$FMT" | grep -q "$h" || { only=0; break; }
  done
  [ "$only" = 1 ] && echo "$f"
done

# deletions and the commit that made each one
for f in $(git diff --name-only --diff-filter=D "$MB" master); do
  echo "$(git log --format='%h %s' -1 "$MB"..master -- "$f") | $f"
done
```

Related docs, deliberately not duplicated here: `AMBIENT_MODE.md`,
`AGENT_BROWSER_BACKEND.md`, `UPSTREAM_MERGE_AGENT.md`.
