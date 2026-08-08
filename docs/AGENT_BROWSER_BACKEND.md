# Replacing the Firefox browser backend with agent-browser

Status: prototype landed, Firefox still present
Scope: the `browser` tool and `jcode browser` CLI

## Summary

jcode's `browser` tool can be served by
[vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser) instead of
the Firefox Agent Bridge. All 22 jcode browser actions map onto it, and the
mapping is implemented and verified end to end.

The Firefox backend is still there and still the fallback. This document records
what a full removal would take and what it would cost.

## Why consider it

The Firefox bridge needs three moving parts installed and version-matched: a CLI
binary, a native messaging host, and a Firefox extension whose install requires a
human to click "Add" in a browser popup. That last step cannot be automated,
which is why headless swarm workers are told not to touch the browser tool at
all. When any part drifts, the tool reports
`bridge: not responding / extension mismatch` and every browser action fails.

agent-browser is a single static binary that drives Chrome over CDP through a
background daemon. No extension, no native host, no human in the loop, so it
works in headless and swarm contexts where the Firefox path cannot be set up.

## What is implemented

- `jcode-base/src/agent_browser.rs`: binary discovery (env override, jcode-managed
  copy, PATH), GitHub release install, Chrome detection, version gate, status.
- `jcode-app-core/src/tool/agent_browser_provider.rs`: all 22 actions mapped onto
  CLI invocations, `--json` for structured results, `--session <jcode-session>`
  for per-session browser isolation.
- Routing in `tool/browser.rs`: `chrome`/`chromium`/`edge`/`brave` go to
  agent-browser, `firefox` to the bridge, `auto` prefers agent-browser when its
  binary is present. `JCODE_BROWSER_BACKEND` overrides.

### Action mapping

| jcode action | agent-browser |
|---|---|
| `open` | `open <url>`, or `tab new <url>` when `new_tab` |
| `snapshot` / `interactables` | `snapshot` / `snapshot -i` |
| `get_content` | `get text|html|title`, or `snapshot` for `annotated` |
| `click` | `click <sel>`, or `find text <text> click` |
| `type` | `fill` (default) or `type` when `clear=false` |
| `fill_form` | one `fill`/`check`/`uncheck` per field |
| `select` | `select <sel> <value>` |
| `wait` | `wait <sel>` or `wait --text` |
| `screenshot` | `screenshot` (image attached to tool output) |
| `eval` | `eval -b <base64>` (avoids shell quoting hazards) |
| `scroll` | `scrollintoview`, or `scroll up/down <px>` |
| `upload` | `upload <sel> <path>` |
| `press` | `press <key>`, preceded by `focus <sel>` when targeted |
| `list_tabs` / `new_tab` / `select_tab` | `tab list` / `tab new` / `tab <handle>` |
| `get_active_tab` | `get url` |
| `list_frames` | `eval` enumerating iframes |
| `provider_command` | passthrough |

## Verified behavior

Run through a real jcode agent session, not just unit tests:

- Opened Hacker News, listed 229 interactables, clicked a story, read the article
  text, took a screenshot, and evaluated `document.title` on the destination.
- Form handling on a local page produced
  `{"email":"a@b.c","tos":true,"pick":"b","log":"key:X"}`, covering `fill_form`,
  `select`, and a targeted `press`.
- Multi-tab switching verified on both 0.13.0 and 0.33.2.
- 18-action compatibility audit run against both versions.

Roughly 150ms per command, since the daemon stays warm between calls.

## Version compatibility

agent-browser moves fast and has made breaking changes. Testing 0.13.0 against
0.33.2 side by side found three bugs in the first draft of this provider, and two
upstream defects.

Provider bugs, all fixed:

1. `select_tab` did nothing on builds before 0.30. Those releases treat `tab t<N>`
   as "list tabs" and return success, so the agent would carry on acting against
   the wrong page. The handle form is now chosen from the detected version, and an
   unknown version prefers the form that fails loudly over the one that no-ops.
   The switch is also verified afterward.
2. `fill_form` and targeted `press` used the `batch` subcommand, which does not
   exist before 0.16 and whose argument form differs across releases. jcode now
   runs the steps itself, stopping at the first failure.
3. `parse_version` rejected `v`-prefixed versions.

Upstream defects on 0.13.0, which motivated the version gate:

- `wait` on text hangs ~150s, then fails with a daemon read error.
- `upload` hangs the same way.

Both work on 0.33.2. A hang is the worst failure mode for an agent, so anything
below `MINIMUM_SUPPORTED_VERSION` (0.30.0) is reported not ready, actions fail
immediately with an upgrade hint, and `setup` installs a current copy into
`~/.jcode/agent-browser`.

## Parameter handling

The `browser` tool schema is shared with the Firefox backend, so some parameters
have no agent-browser equivalent. Silently ignoring them is the worst option: the
caller believes an action was scoped when it was not. Each one is therefore
implemented, documented as a no-op, or rejected with a concrete alternative.

| Parameter | Handling |
|---|---|
| `timeout_ms` | Implemented, maps to `wait --timeout` |
| `submit` | Implemented, presses Enter after filling |
| `clear` | Implemented, selects `fill` vs `type` |
| `focus`, `behavior` | Accepted and ignored. Presentation-only hints with no observable effect on a headless browser's page state |
| `frame_id` | Rejected. agent-browser addresses frames by selector; the error points at `provider_command` with `provider_action='frame'` |
| `all_frames=true` | Rejected, with a per-frame alternative |
| `window_id` | Rejected. Each jcode session already gets its own isolated browser |
| `page_world=false` | Rejected. agent-browser's eval always runs in the page world |
| `wait=false` on `open` | Rejected. Navigation always waits for load |

Values that happen to match agent-browser's own default (`all_frames=false`,
`page_world=true`, `wait=true`) pass through, since honoring them is a no-op.

## Runtime behavior

- **Every invocation is bounded.** A wedged daemon would otherwise block the whole
  agent turn with no recovery. Waits get their own budget plus slack so an
  explicit long wait is not cut short.
- **Daemon bookkeeping is stripped.** agent-browser >=0.30 attaches a `lifecycle`
  object to nearly every response. On `get url` that is 261 bytes of state around
  73 bytes of answer, repeated on every call, so it is removed before the result
  reaches the model.

## What a full Firefox removal still needs

`crates/jcode-provider-openai-runtime/src/chatgpt_web.rs` (915 lines) is a second,
independent consumer of the Firefox bridge. It backs the `gpt-5.6-pro-web` model
route by driving the user's real, logged-in Firefox: it calls `fork` to duplicate
an authenticated chatgpt.com tab, drives the composer, then `killFork`.

This does not port cleanly:

- agent-browser has no `fork` equivalent.
- The premise is riding the user's existing Firefox login. The nearest equivalent
  is `--profile Default` against Chrome, which reuses a Chrome profile via a
  read-only snapshot, so it only helps if the user is logged into ChatGPT in
  Chrome.

Until that route is ported or dropped, removing the Firefox bridge removes the
`gpt-5.6-pro-web` model. That is a separate piece of work.

## Capability differences

Recovered:

- Reusing real logins. `JCODE_BROWSER_PROFILE=Default` makes agent-browser copy a
  Chrome profile to a temp snapshot, so existing cookies and sessions apply and
  the user's live profile is never mutated.

Not equivalent:

- Per-session binding to a visible OS window. The Firefox bridge pins each jcode
  session to its own real window on one shared Firefox. agent-browser isolates by
  session but is headless by default, so there is nothing for the user to watch
  unless `--headed` is used.
- Firefox itself. agent-browser drives Chromium-family browsers, plus Safari and
  iOS via WebDriver. Anything that must be tested in Gecko still needs the bridge.

## Configuration

| Variable | Effect |
|---|---|
| `JCODE_BROWSER_BACKEND` | `firefox` or `agent-browser` to pin the `auto` choice |
| `JCODE_AGENT_BROWSER_BIN` | Use a specific agent-browser binary |
| `JCODE_BROWSER_PROFILE` | Chrome profile name or path to reuse logins |

## Recommendation

Keep both backends for now. agent-browser is the better default: it installs
unattended, works headless, and does not rot the way the extension pairing does.
Keep the Firefox bridge until `chatgpt_web.rs` is resolved, and for Gecko-specific
work.
