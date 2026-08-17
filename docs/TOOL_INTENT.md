# Tool intent

Every tool call carries an optional-in-shape, required-in-schema `intent`: a
short label stating why the call is being made. It exists for the person
watching the session, not for the model's own reasoning.

## Why it exists

A tool row rendered from raw parameters tells the reader what was executed but
not why. `$ git log --oneline -30 --since="3 weeks ago" -- crates/...` is
precise and nearly unreadable at a glance. `git history for permission code`
answers the question the reader actually has. The parameters remain available
behind `display.tool_call_details`.

## Where it is added

`jcode_tool_core::ensure_intent_in_schema` injects the property and marks it
required. It runs centrally in `Tool::to_definition`, so every registry-backed
tool and every MCP proxy gets an `intent` without wiring it per tool.

The Anthropic OAuth (subscription) transport is the one exception. That endpoint
expects the Claude Code builtin tool names with compatible schemas, so
`jcode-provider-anthropic` keeps hand-written definitions for `Agent`, `Bash`,
`Edit`, `Glob`, `Grep`, `Read`, `Skill`, and `Write`. Those never reach
`ensure_intent_in_schema`, and they declare `additionalProperties: false`, so
until the schemas said otherwise a subscription model could not send an intent
for the eight most common tools even though the UI was ready to display one.
`with_curated_intent` closes that gap, and a test pins its description string to
`jcode_tool_core::TOOL_INTENT_DESCRIPTION` so the two cannot drift.

## How it is displayed

`get_tool_activity_detail` and `tool_row_line` in
`crates/jcode-tui/src/tui/ui_tools.rs` prefer the intent and append the
technical summary only when `display.tool_call_details` is on. An error summary
always renders, with or without an intent, so failures stay diagnosable.

When a tool call has no intent the row falls back to the parameter summary. That
fallback is now reached only for genuinely intent-less calls, such as older
sessions replayed from disk, rather than for whole classes of tool.
