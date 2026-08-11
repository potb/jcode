# Ambient Mode

> **Status:** Design
> **Updated:** 2026-02-08

A proactive, always-on agent mode that works autonomously without user prompting. Like a brain consolidating memories during sleep, ambient mode tends to the memory graph, identifies useful work, and acts on the user's behalf — all while staying within resource limits.

## Overview

Ambient mode operates as a background loop that:
1. **Gardens** — consolidates, prunes, and strengthens the memory graph
2. **Scouts** — analyzes recent sessions, git history, and memories to understand what the user cares about
3. **Works** — proactively completes tasks the user would appreciate being surprised by

These aren't separate phases. The agent does all three in a single pass — while looking at memories it naturally discovers maintenance work and identifies proactive opportunities simultaneously.

**Key Design Decisions:**
1. **Single agent at a time** — only one ambient instance ever runs, no parallelism
2. **Subscription-first** — defaults to OAuth (OpenAI/Anthropic), never uses API keys unless explicitly configured
3. **User priority** — interactive sessions always take precedence over ambient work
4. **Strong models** — uses the strongest available model from the selected provider so the agent can reason well about what's actually useful
5. **Self-scheduling** — the agent decides when to wake next, constrained by adaptive resource limits

---

## Architecture

```mermaid
graph TB
    subgraph "Scheduling Layer"
        EV[Event Triggers<br/>session close, crash, git push]
        TM[Timer<br/>agent-scheduled wake]
        RC[Resource Calculator<br/>adaptive interval]
        SQ[(Scheduled Queue<br/>persistent)]
    end

    subgraph "Ambient Agent"
        QC[Check Queue]
        SC[Scout<br/>memories + sessions + git]
        GD[Garden<br/>consolidate + prune + verify]
        WK[Work<br/>proactive tasks]
        SA[schedule_ambient tool<br/>set next wake + context]
    end

    subgraph "Resource Awareness"
        UH[Usage History<br/>rolling window]
        RL[Rate Limits<br/>per provider]
        AU[Ambient Usage<br/>current window]
        AC[Active Sessions<br/>user activity]
    end

    subgraph "Outputs"
        MG[(Memory Graph<br/>consolidated)]
        CM[Commits & Changes]
        IW[Info Widget<br/>TUI display]
    end

    EV -->|wake early| RC
    TM -->|scheduled wake| RC
    RC -->|"gate: safe to run?"| QC
    SQ -->|pending items| QC
    QC --> SC
    SC --> GD
    SC --> WK
    GD --> MG
    WK --> CM
    SA -->|next wake + context| SQ
    SA -->|proposed interval| RC

    UH --> RC
    RL --> RC
    AU --> RC
    AC --> RC

    QC --> IW
    SC --> IW
    GD --> IW
    WK --> IW

    style EV fill:#fff3e0
    style TM fill:#fff3e0
    style RC fill:#ffcdd2
    style SQ fill:#e3f2fd
    style QC fill:#e8f5e9
    style SC fill:#e8f5e9
    style GD fill:#e8f5e9
    style WK fill:#e8f5e9
```

---

## Ambient Cycle

Each ambient cycle follows a single flow. The agent doesn't switch between "modes" — it naturally handles gardening, scouting, and work in one pass.

```mermaid
sequenceDiagram
    participant SYS as System Scheduler
    participant RES as Resource Calculator
    participant AMB as Ambient Agent
    participant MEM as Memory Graph
    participant CB as Codebase
    participant Q as Scheduled Queue

    SYS->>RES: Timer/event fired
    RES->>RES: Check usage headroom
    alt Over budget
        RES->>SYS: Delay (recalculate interval)
    else Safe to run
        RES->>AMB: Spawn ambient agent
    end

    AMB->>Q: Check scheduled queue
    alt Has queued items
        Q-->>AMB: Return items + context
        AMB->>MEM: Scout relevant memories for queued work
        MEM-->>AMB: Context memories
        AMB->>CB: Execute queued work
    end

    AMB->>MEM: Load memory graph
    MEM-->>AMB: Full graph state

    Note over AMB: Garden pass
    AMB->>AMB: Find duplicates → merge & reinforce
    AMB->>AMB: Find contradictions → resolve
    AMB->>AMB: Find decayed memories → prune or re-verify
    AMB->>CB: Verify stale facts against codebase
    CB-->>AMB: Verification results
    AMB->>MEM: Apply consolidation changes

    Note over AMB: Scout pass (simultaneous)
    AMB->>AMB: Analyze recent sessions for missed extractions
    AMB->>AMB: Check git history for active work
    AMB->>AMB: Identify proactive work opportunities

    Note over AMB: Work pass
    AMB->>CB: Execute proactive tasks
    AMB->>MEM: Store new memories from findings

    AMB->>AMB: end_ambient_cycle(summary, schedule)
    AMB->>SYS: Done (summary → widget + email)
```

---

## Ambient Agent Tools

The ambient agent has access to a subset of jcode tools plus ambient-specific tools.

### `end_ambient_cycle` (required)

Every ambient cycle **must** end with this tool call. The system uses the summary for the notification email and the info widget.

```rust
// Tool: end_ambient_cycle
{
    "summary": "Merged 3 duplicate memories, pruned 2 stale facts,
                extracted memories from crashed session jcode-red-fox-1234",
    "memories_modified": 8,
    "compactions": 2,
    "proactive_work": null,
    "next_schedule": {
        "wake_in_minutes": 25,
        "context": "Verify 4 remaining stale facts"
    }
}
```

| Field | Required | Description |
|-------|----------|-------------|
| `summary` | yes | Human-readable summary of what was done (goes into email/widget) |
| `memories_modified` | yes | Count of memories created/merged/pruned/updated |
| `compactions` | yes | Number of context compactions during this cycle |
| `proactive_work` | no | Description of proactive code changes, if any |
| `next_schedule` | no | When to wake next + context (falls back to system default if omitted) |

### `schedule_ambient`

Can also be called mid-cycle to queue future work:

```rust
// Tool: schedule_ambient
{
    "wake_in_minutes": 15,
    "context": "Check if CI passed for auth refactor PR",
    "priority": "normal"
}
```

### `todos`

The agent should use a todos tool to plan its cycle. This provides:
- Visibility into what the agent planned vs what it actually did
- If the cycle is interrupted, we know what's left
- Structure for the agent's reasoning

### `request_permission`

From the [Safety System](./SAFETY_SYSTEM.md). Used for any Tier 2 action.

---

## Handling Unexpected Stops

The model may stop unexpectedly (output length limit, API error, random stop). The system handles this:

```mermaid
stateDiagram-v2
    [*] --> Running: Cycle started

    Running --> Stopped: Model output ends

    Stopped --> CheckTool{Called end_ambient_cycle?}

    CheckTool --> Complete: Yes → normal completion
    CheckTool --> Continuation: No → send continuation message

    Continuation --> Running: Model continues work
    Continuation --> Stopped: Model stops again

    Stopped --> ForcedEnd: Second stop without end_ambient_cycle
    ForcedEnd --> Incomplete: Generate partial transcript,\nschedule default wake

    Complete --> [*]
    Incomplete --> [*]
```

**Continuation message** (injected as user message):

```
You stopped unexpectedly without calling end_ambient_cycle.
If you are done with your work, call end_ambient_cycle with a
summary of what you accomplished and schedule your next wake.
If you are not done, continue what you were doing.
```

**If no `end_ambient_cycle` is called after two attempts:**
- System generates a partial transcript marked as `incomplete`
- Compaction count is pulled from system metrics
- Default wake interval is scheduled
- Warning logged for debugging

**If no `schedule_ambient` or `next_schedule` in `end_ambient_cycle`:**
- System schedules a default wake at `max_interval_minutes` from config
- Warning logged — the agent should always schedule its next wake

---

## System Prompt

The ambient agent's system prompt is built dynamically each cycle with real data. The prompt gives the agent information to reason with, not rigid instructions for how to think.

```
You are the ambient agent for jcode. You operate autonomously without
user prompting. Your job is to maintain and improve the user's
development environment.

## Current State
- Last ambient cycle: {timestamp} ({time_ago})
- Machine was off/idle since: {if applicable}
- Active user sessions: {count, or "none"}
- Cycle budget: ~{estimated_max_tokens} tokens

## Scheduled Queue
{queued items with context, or "empty — do general ambient work"}

## Recent Sessions (since last cycle)
{for each session:
  - id, status (closed/crashed/active), duration, topic summary
  - extraction status (extracted/missed/partial)
}

## Memory Graph Health
- Total memories: {count} ({active} active, {inactive} inactive)
- Memories with confidence < 0.1: {count}
- Unresolved contradictions: {count}
- Memories without embeddings: {count}
- Duplicate candidates (similarity > 0.95): {count}
- Last consolidation: {timestamp}

## User Feedback History
{recent memories about ambient approval/rejection patterns}

## Resource Budget
- Provider: {name}
- Tokens remaining in window: {count}
- Window resets: {timestamp}
- User usage rate: {tokens/min average}
- Budget for this cycle: stay under {limit} tokens

## Instructions

Start by using the todos tool to plan what you'll do this cycle.

Priority order:
1. Execute any scheduled queue items first.
2. Garden the memory graph — consolidate duplicates, resolve
   contradictions, prune dead memories, verify stale facts,
   extract from missed sessions.
3. Scout for proactive work (only if enabled and past cold start) —
   look at recent sessions and git history to identify useful work
   the user would appreciate.

For gardening: focus on highest-value maintenance first. Duplicates
and contradictions before pruning. Verify stale facts only if you
have budget left.

For proactive work: be conservative. A bad surprise is worse than
no surprise. Check the user feedback memories — if they've rejected
similar work before, don't do it. Code changes must go on a worktree
branch with a PR via request_permission.

When done, you MUST call end_ambient_cycle with a summary of
everything you did, including compaction count. Always schedule
your next wake time with context for what you plan to do next.
```

---

## Usage Calculation

### Tracking

Every API call (user or ambient) is logged:

```rust
struct UsageRecord {
    timestamp: DateTime<Utc>,
    source: UsageSource,      // User | Ambient
    tokens_input: u32,
    tokens_output: u32,
    provider: String,
}
```

### Rate Limit Discovery

Rate limits are learned from provider response headers:

```
x-ratelimit-limit-requests: 50
x-ratelimit-remaining-requests: 42
x-ratelimit-limit-tokens: 100000
x-ratelimit-remaining-tokens: 85000
x-ratelimit-reset-requests: 2026-02-08T15:00:00Z
```

When headers aren't available, fall back to conservative defaults and adjust based on whether rate limit errors occur.

### Subscription Headroom (what actually runs)

The header-driven algorithm below was never reached in practice. Nothing
populated `RateLimitInfo`, so every caller passed `None` and the interval
collapsed to a constant `max_interval_minutes` — the adaptive path was dead
code. Subscription auth is the reason: OAuth requests do not carry
`x-ratelimit-*` headers at all. The quota lives in the usage endpoint that
already backs the TUI info widget.

So the real signal is a **utilization fraction per rolling window** plus a reset
time, read from the cached snapshots in `jcode-base`'s `usage` module (no extra
network traffic). Both Anthropic and OpenAI/Codex report that shape, and a user
with both subscriptions is bound by whichever window is closest to exhaustion.

```
# Every reported window, both providers (5-hour, weekly, spark)
binding = max(windows, key=utilization)      # closest to exhaustion wins
remaining = 1 - binding.utilization

# Ambient's share of what is left
usable = remaining * (1 - user_budget_reserve)

# Pace is inversely proportional to remaining quota, scaled so a full window
# under the default reserve lands exactly on min_interval.
interval = min_interval * (default_ambient_share / usable)

# Never idle past a refill, then clamp to the configured bounds.
interval = min(interval, time_until_reset)
interval = clamp(interval, min_interval, max_interval)
```

Window *duration* deliberately does not enter the arithmetic. Spreading the
remaining quota over the time left before reset inverts the intent: a fresh
7-day window would pace slower than a fresh 5-hour one, because the same
per-cycle cost is charged as a fraction of a much longer window. The fraction
left right now is the signal; duration only says how soon it refills, which
caps the interval.

Properties worth preserving:

- **Monotonic** — less headroom never yields a shorter interval.
- **Bounded** — `min_interval_minutes` and `max_interval_minutes` are hard
  bounds no quota reading can breach.
- **Fails conservative** — no quota data (API key auth, or usage not yet
  fetched) falls back to `max_interval_minutes`. Absent data is never read as a
  full window.
- **Backoff still applies** — a 429 doubles the interval on top of this.

With `min = 5` and `max = 15`, a fresh window runs every 5 minutes and stretches
to 15 once roughly 70% of the window is spent.

### Adaptive Interval Algorithm

> Historical: the header-driven path. Retained for callers that do have
> `RateLimitInfo`; unreached under subscription auth.

```
# Known from headers or defaults
window_remaining = reset_time - now
tokens_remaining = ratelimit_remaining_tokens
requests_remaining = ratelimit_remaining_requests

# Estimate user consumption from rolling history
user_rate = rolling_average(
    usage_log.filter(source=User, last_hour),
    per_minute
)

# Project user usage for rest of window
user_projected = user_rate * window_remaining

# Reserve 20% buffer so user never feels throttled
ambient_budget = (tokens_remaining - user_projected) * 0.8

# Estimate cost per ambient cycle from recent cycles
tokens_per_cycle = rolling_average(
    recent_ambient_cycles.last(5).tokens_used
)

# How many cycles fit in remaining budget?
cycles_available = ambient_budget / tokens_per_cycle

# Spread evenly across remaining window
if cycles_available > 0:
    interval = window_remaining / cycles_available
else:
    interval = window_remaining  # wait for reset

# Clamp to configured bounds
interval = clamp(interval, min_interval, max_interval)
```

### Behavioral Rules

| Condition | Behavior |
|-----------|----------|
| User is active in a session | Pause ambient (or multiply interval by 3-5x) |
| User has been idle for hours | Run cycles more frequently |
| Hit a rate limit | Exponential backoff (double interval each time) |
| No rate limit errors for N cycles | Gradually decrease interval |
| No headers available | Start with 30min interval, adjust from errors |
| Approaching end of window with budget left | Squeeze in extra cycles |
| Over 80% of budget consumed | Fall back to max_interval |

---

## Memory Consolidation

### Two-Layer Architecture

Memory consolidation happens at two levels, mirroring how the brain encodes during the day and consolidates during sleep.

```mermaid
graph LR
    subgraph "Layer 1: Sidecar (every turn, fast)"
        S1[Memory retrieved<br/>for relevance check]
        S2{New memory<br/>similar to existing?}
        S3[Reinforce existing<br/>+ breadcrumb]
        S4[Create new memory]
        S5[Supersede if<br/>contradicts]
    end

    subgraph "Layer 2: Ambient Garden (background, deep)"
        A1[Full graph scan]
        A2[Cross-session<br/>dedup]
        A3[Fact verification<br/>against codebase]
        A4[Retroactive<br/>session extraction]
        A5[Prune dead<br/>memories]
        A6[Relationship<br/>discovery]
    end

    S1 --> S2
    S2 -->|yes| S3
    S2 -->|no| S4
    S2 -->|contradicts| S5

    A1 --> A2
    A1 --> A3
    A1 --> A4
    A1 --> A5
    A1 --> A6

    style S1 fill:#e8f5e9
    style S2 fill:#e8f5e9
    style S3 fill:#e8f5e9
    style S4 fill:#e8f5e9
    style S5 fill:#e8f5e9
    style A1 fill:#e3f2fd
    style A2 fill:#e3f2fd
    style A3 fill:#e3f2fd
    style A4 fill:#e3f2fd
    style A5 fill:#e3f2fd
    style A6 fill:#e3f2fd
```

### Layer 1: Sidecar Consolidation

Runs after every turn, only on memories already retrieved for relevance checking. Zero added latency — runs after results are returned to the main agent.

**Operations:**
- **Duplicate detection** — if the sidecar is about to create a memory that's semantically identical to one it just retrieved, reinforce the existing one instead
- **Contradiction detection** — if a new memory contradicts an existing one in the retrieved set, supersede the old one
- **Reinforcement** — bump strength on memories that keep appearing relevant

**Cost:** Near zero. Only operates on memories already in hand.

### Layer 2: Ambient Garden

Deep consolidation that runs during ambient cycles. Has access to the full memory graph and codebase.

**Operations:**

| Operation | Description | Trigger |
|-----------|-------------|---------|
| **Graph-wide dedup** | Find semantically similar memories across entire graph | Embedding similarity > 0.95 |
| **Contradiction resolution** | Resolve `Contradicts` edges by checking current state | Contradicts edges exist |
| **Fact verification** | Check factual memories against codebase | Facts older than confidence half-life |
| **Retroactive extraction** | Analyze recent sessions that lack memory extraction | Sessions with status Crashed, Closed without extraction |
| **Pruning** | Remove memories with near-zero confidence and low strength | confidence < 0.05 AND strength <= 1 |
| **Relationship discovery** | Find new connections between memories | Co-occurrence in sessions, semantic similarity |
| **Embedding backfill** | Generate embeddings for memories that lack them | embedding is None |
| **Cluster refinement** | Re-run clustering on updated embeddings | Every N ambient cycles |

### Reinforcement Provenance

When a memory is reinforced (by sidecar or ambient), the system records a breadcrumb for traceability:

```rust
pub struct Reinforcement {
    pub session_id: String,
    pub message_index: usize,
    pub timestamp: DateTime<Utc>,
}

pub struct MemoryEntry {
    // ... existing fields ...
    pub reinforcements: Vec<Reinforcement>,
}

impl MemoryEntry {
    pub fn reinforce(&mut self, session_id: &str, message_index: usize) {
        self.strength += 1;
        self.updated_at = Utc::now();
        self.reinforcements.push(Reinforcement {
            session_id: session_id.to_string(),
            message_index,
            timestamp: Utc::now(),
        });
    }
}
```

The consolidation agent can later trace back through reinforcements to understand *why* a memory has the strength it does, and whether those reinforcements still hold.

---

## Scheduling

### Two-Layer Scheduling

```mermaid
graph TB
    subgraph "Agent Layer (proposes)"
        AT[schedule_ambient tool]
        AT -->|"wake in 15m,<br/>context: check CI"| PROP[Proposed Schedule]
    end

    subgraph "System Layer (constrains)"
        PROP --> ADAPT[Adaptive Calculator]
        MAX[Max Interval Ceiling] --> ADAPT
        MIN[Min Interval Floor] --> ADAPT
        ADAPT --> FINAL[Final Schedule]
    end

    subgraph "Adaptive Calculator Inputs"
        UH[User usage history<br/>rolling window]
        AU[Ambient usage<br/>current window]
        RL[Provider rate limits<br/>from headers]
        TW[Time remaining<br/>in limit window]
        AS[Active sessions<br/>user currently working?]
    end

    UH --> ADAPT
    AU --> ADAPT
    RL --> ADAPT
    TW --> ADAPT
    AS --> ADAPT

    FINAL -->|"actual: 28m<br/>(headroom limited)"| TIMER[System Timer]

    style AT fill:#e8f5e9
    style ADAPT fill:#ffcdd2
    style FINAL fill:#e3f2fd
```

### Active Windows (wall-clock constraints)

Pace control answers *how often*; windows answer *when at all*. They are
independent: a window says the agent may not run on Sunday, and headroom
still decides how fast it runs on Tuesday.

```toml
[ambient]
# Weekdays, business hours only. Nothing at night, nothing on weekends.
active_windows = ["weekdays 09:00-19:00"]
```

**Allowed ranges, not forbidden ones.** An allow-list fails closed: a range you
forgot to write is quiet time. A deny-list fails open, and the rule you forgot
to write is the machine waking you at 3am.

**Empty means unrestricted.** `active_windows = []` (the default) runs exactly
as before, so existing configs are unaffected. Empty could not mean "never run"
without silently disabling ambient for everyone who never asked for a
constraint.

Day specs: `mon`, `mon-fri`, `mon,wed,fri`, `weekdays`, `weekends`, `daily`.
Day ranges may wrap the week (`fri-mon`). Times are `HH:MM-HH:MM`, start
inclusive, end exclusive, with `24:00` accepted for end-of-day.

A range whose end is at or before its start wraps past midnight, and the tail
belongs to the day the window *opened*:

```toml
# Friday 22:00 through Saturday 02:00. Saturday itself stays quiet.
active_windows = ["fri 22:00-02:00"]
```

Multiple windows union:

```toml
active_windows = ["weekdays 09:00-19:00", "sat 10:00-14:00"]
```

Semantics worth knowing:

- **Local time.** Windows are wall-clock statements about your week, so they
  follow you across DST instead of drifting an hour twice a year.
- **Queued work is deferred, never dropped.** An item coming due inside a
  closed window runs when the window next opens.
- **Direct deliveries are exempt.** An item targeted at a session you are
  sitting in is handed over regardless: you are present by definition, so
  withholding it would be the constraint working against you.
- **Sleeps until reopening**, re-checking hourly, rather than polling every
  30s all weekend. The hourly ceiling keeps config edits and manual triggers
  from being ignored for days.
- **Fails open on a bad spec.** Unparseable entries are logged and skipped; if
  none survive, ambient runs unrestricted rather than being disabled by a typo.
- **`jcode ambient trigger` overrides the window.** An explicit human request
  is not the scheduled work the constraint exists to hold back.

#### Suspending the windows without losing them

To run around the clock for a while, do not delete the schedule: set the flag.

```toml
[ambient]
active_windows = ["weekdays 09:00-23:00"]   # kept, just not enforced
ignore_active_windows = true
```

A tuned schedule is worth keeping, and deleting it is the only other way to
escape it. With the flag set every window decision sees an unrestricted clock:
cycle gating, sleep length, and `[[cron]]` jobs with `respect_windows = true`.
Clear the flag and the original quiet hours come back with nothing to retype.

`ambient:status` reports both views, so the suspension is never mistaken for
lost config: `active_windows` is what you configured,
`active_windows_enforced` is what is actually in force (`unrestricted` while
ignored), and `active_windows_ignored` says which mode you are in.

### Notifications (what reaches your phone)

Windows decide *when* the agent runs; this decides *when it interrupts you*.
Both exist for the same reason: an agent that pages you for nothing is one
you will mute, and a muted channel costs every future alert that mattered.

Configure the channel under `[safety]`:

```toml
[safety]
ntfy_topic = "jcode-<random>"      # ntfy.sh topics are PUBLIC to anyone who knows the name
ntfy_server = "https://ntfy.sh"
ntfy_detailed = true               # send the real summary, not just counts (default: false)
```

**Routine cycles are silent.** Most cycles are gardening: queue empty,
memories healthy, nothing changed for you. Those send nothing.

The decision is NOT a threshold on counts, because counts cannot express it.
From real transcripts: a garden-only cycle reported `memories_modified = 2`,
while the cycle announcing "#763 and #764 are both MERGED" reported `1`.
Gardening *is* memory work, so the number says nothing about whether a human
cares. Only the agent knows, so it declares it on `end_ambient_cycle`:

| `significance` | Meaning | Notifies |
|---|---|---|
| `"routine"` | Gardening, memory upkeep, queue checks, "nothing to do" | No |
| `"notable"` | Blocked on you, needs a decision, finished work you awaited | Yes |
| *unset* | Agent did not say | No |

Unset is silent because garden cycles are the majority and none of them
declare anything, so notifying-on-unset would reproduce exactly the noise this
removes. That default is only safe because three cases notify on **structure
alone**, without the agent's cooperation:

- **Pending permission requests** - the entire point of the channel.
- **A failed cycle** - it may have died before reaching its own reporting
  code, so its label (or silence) proves nothing.
- **Proactive code changes** - code changed; never routine.

So a cycle cannot mute a permission request or a crash by calling itself
routine. When debugging a missing notification, check the log for
`routine cycle, no notification sent` - that line means the gate decided,
as distinct from a broken channel.

### Two-way channels (how the agent reaches you, and you reach it)

ntfy is one-way: it pushes a line to your phone and nothing comes back. The
`send_message` tool posts to a different set of channels, each of which can
also carry your reply back as a directive for the next cycle. All are off by
default, and **a cycle with none of them enabled has no delivery path at all**
— its `end_ambient_cycle` summary is the only thing you will ever see.

| Channel | Enable with | Also needs |
|---|---|---|
| `telegram` | `telegram_enabled` | `telegram_bot_token`, `telegram_chat_id` |
| `discord` | `discord_enabled` | `discord_bot_token`, `discord_channel_id` |
| `github` | `github_enabled` | `github_repo`, plus a token (config, `GITHUB_TOKEN`/`GH_TOKEN`, or `gh auth`) |
| `jade_relay` | `jade_relay_enabled` | `jade_relay_api_base`, `jade_relay_token`, `jade_relay_session_id` |

The matching `*_reply_enabled` flag is what turns an inbound message into an
agent directive; without it the channel is send-only.

GitHub is the least setup for a phone-reachable channel, since it needs no bot
or hosted service: ambient opens one issue per topic, your comments come back
as directives, and closing the issue marks the topic settled. The
`github_issue` tool is the rest of that lifecycle (`list`, `open`, `comment`,
`close`) and it refuses with `The GitHub channel is disabled
(safety.github_enabled)` when the flag is off — note that the tool merely
*existing* does not mean the channel is on.

**Enabling a channel is not the same as configuring it.** A channel that is
switched on but missing a credential is skipped at registry build time rather
than failing loudly. `send_message` names those explicitly, so a reply like:

```
No messaging channels configured. Enable telegram, discord, or github under
[safety] in config. Skipped: github: enabled but incomplete (github_repo set,
token missing).
```

means the fix is the missing token, *not* enabling some other channel.

### Agent-Initiated Scheduling

The ambient agent has a `schedule_ambient` tool to request its next wake-up:

```rust
// Tool: schedule_ambient
{
    "wake_in_minutes": 15,           // or "wake_at": "2026-02-08T15:30:00Z"
    "context": "Check if CI passed for auth refactor PR",
    "priority": "normal"             // "low" | "normal" | "high"
}
```

The context is stored in the scheduled queue so when the agent wakes up, it knows what it planned to do.

### Adaptive Resource Calculation

The system calculates the safe interval based on usage patterns:

```
headroom = rate_limit - (user_usage_rate + ambient_usage_rate)
safe_interval = max(min_interval, target_budget_fraction / headroom)
```

**Inputs:**
- **User usage rate** — rolling average of tokens/requests per hour from interactive sessions
- **Ambient usage rate** — tokens/requests consumed by ambient in current window
- **Rate limits** — known per-provider limits (from response headers or config)
- **Time in window** — how much of the rate limit window remains
- **Active sessions** — if user is currently in a session, ambient pauses or throttles heavily

**Behavior:**
- Agent says "wake in 10m" but system calculates "not safe until 30m" → pushed to 30m
- Agent says "wake in 6h" but system sees unused budget → pulled forward to max interval
- User starts interactive session → ambient pauses, resumes when user goes idle
- Approaching rate limit → ambient backs off exponentially

### Event Triggers

Certain events can wake ambient early (still subject to resource gate):

| Event | Priority | Rationale |
|-------|----------|-----------|
| Session crashed | High | Likely missed memory extraction |
| Session closed | Normal | May have unextracted memories |
| Git push | Low | Codebase changed, facts may be stale |
| User idle > threshold | Low | Good time for ambient work |
| Explicit `/ambient` command | Immediate | User requested |

### Scheduled Queue

Persistent queue of scheduled ambient tasks:

```rust
pub struct ScheduledItem {
    pub id: String,
    pub scheduled_for: DateTime<Utc>,
    pub context: String,
    pub priority: Priority,
    pub created_by_session: String,     // which ambient cycle created this
    pub created_at: DateTime<Utc>,
}

pub enum Priority {
    Low,
    Normal,
    High,
}
```

**Queue rules:**
- Checked first when ambient wakes up
- Items sorted by priority then scheduled time
- Expired items (past their scheduled_for) are still executed
- System can delay items if over budget, but won't drop them
- Only one ambient agent at a time — if one is running, new triggers queue up

---

## Provider & Model Selection

### Default Priority

```mermaid
graph TD
    START[Ambient Mode Start] --> CHECK1{OpenAI OAuth<br/>available?}
    CHECK1 -->|yes| OAI[Use OpenAI<br/>strongest available]
    CHECK1 -->|no| CHECK2{Anthropic OAuth<br/>available?}
    CHECK2 -->|yes| ANT[Use Anthropic<br/>strongest available]
    CHECK2 -->|no| CHECK3{API key or OpenRouter +<br/>config opt-in?}
    CHECK3 -->|yes| API[Use API/OpenRouter<br/>with budget cap]
    CHECK3 -->|no| DISABLED[Ambient mode disabled<br/>no provider available]

    style OAI fill:#e8f5e9
    style ANT fill:#fff3e0
    style API fill:#ffcdd2
    style DISABLED fill:#f5f5f5
```

**Rationale:**
- **OpenAI first** — separate rate limit pool from Anthropic, so ambient doesn't compete with interactive sessions
- **Anthropic second** — also subscription-based (OAuth), no per-token cost
- **OpenRouter/API keys last** — these are pay-per-token; opt-in only via config to avoid silently burning credits
- **Strong models** — ambient needs good judgment about what work is valuable. A weak model would do the wrong proactive work and annoy the user.

### Model Selection

| Provider | Default Model | Rationale |
|----------|--------------|-----------|
| OpenAI OAuth | Strongest available (e.g. `5.2-codex-xhigh`) | Best reasoning for judgment calls |
| Anthropic OAuth | Strongest available (e.g. `claude-opus-4-6`) | Best available on Anthropic |
| OpenRouter (opt-in) | Strongest available | Pay-per-token, requires config opt-in |
| API key (opt-in) | Configurable | User chooses cost/capability tradeoff |

### Resource Rules

1. **Subscription (OAuth — OpenAI/Anthropic):** Ambient is allowed, subject to adaptive rate limiting
2. **Pay-per-token (API keys, OpenRouter):** Off by default. Enable in config with optional daily budget cap
3. **User active:** Ambient pauses or throttles to minimum when user has an active session
4. **Rate limited:** If ambient hits a rate limit, back off aggressively (exponential backoff)
5. **Separate pools:** Prefer OpenAI for ambient when Anthropic is used interactively (and vice versa)

---

## Standing Instructions

Config booleans decide *whether* ambient works, never *what* the user wants
done. Standing instructions are where that intent goes, in prose.

- **Global**: `~/.jcode/ambient-instructions.md`
- **Per project**: `~/.jcode/ambient/instructions/<flattened-path>.md`

Both are optional, read fresh at the start of every cycle (edit them and the
next wake picks the change up, no restart), and skipped entirely when empty.

Per-project instructions deliberately live under `~/.jcode/`, **not** inside the
project. A dotfile committed into every repo would put the user's private notes
to their own agent into diffs, reviews and other people's checkouts. The file
name flattens the absolute path, so `~/work/api` and `~/personal/api` do not
collide. To find the path for a project, see `project_instructions_path` in
`crates/jcode-app-core/src/ambient/prompt.rs`.

The prompt states that these instructions **outrank** the agent's own cautious
defaults and any memory it wrote in an earlier cycle. That ordering matters: an
observation like "the user has live sessions in this worktree, stay out" is true
for an afternoon, but stored as a memory it reads as a permanent rule and can
quietly fence off a whole repo for days. The prompt now also tells the agent to
re-check any avoid-this-area memory against present reality and rewrite or forget
it instead of skipping the work again.

---

## Proactive Work

### What Ambient Does

The agent uses memories, recent sessions, and git history to identify useful work:

```mermaid
graph LR
    subgraph "Context Gathering"
        M[Memories<br/>user preferences,<br/>priorities]
        S[Recent Sessions<br/>what user was<br/>working on]
        G[Git History<br/>active branches,<br/>recent changes]
    end

    subgraph "Inference"
        I[What does the user<br/>care about most?]
        U[What upcoming work<br/>is there?]
        O[What would surprise<br/>the user positively?]
    end

    subgraph "Actions"
        T[Write/fix tests]
        R[Small refactors]
        D[Update stale docs]
        F[Fix obvious issues]
        C[Clean up TODOs]
    end

    M --> I
    S --> I
    G --> I
    I --> O
    U --> O
    O --> T
    O --> R
    O --> D
    O --> F
    O --> C
```

### Safety

Ambient mode operates under the [Safety System](./SAFETY_SYSTEM.md) — a human-in-the-loop layer that classifies actions, requests permission for anything risky, and notifies the user via email/SMS/desktop.

Key constraints for ambient:
- **All actions classified** — auto-allowed (read, local branches, memory ops), requires permission (PRs, pushes, communication), or always denied (force-push, delete remote branches)
- **Commits to a separate branch** — never pushes to main/master directly
- **Code changes require worktree + PR** — modifications always go through review
- **Small, focused changes** — no large refactors without user request
- **Session transcript** — full log of every action, sent as summary after each cycle
- **Respects .gitignore and sensitive files** — same security rules as interactive mode
- **Can be reviewed** — user sees ambient work in the TUI and pending permission requests

---

## Info Widget

The TUI displays ambient mode status alongside existing widgets (memory, tokens, etc.).

### Widget Content

```
╭─ Ambient ─────────────────────────╮
│ ● Running (garden + scout)        │
│ Queue: 2 items (next: check CI)   │
│ Last: 12m ago — pruned 3, merged 1│
│ Next: ~18m (adaptive)             │
│ Budget: ██████░░░░ 58% remaining  │
╰───────────────────────────────────╯
```

**Fields:**

| Field | Description |
|-------|-------------|
| **Status** | `idle` / `running (detail)` / `scheduled` / `paused (rate limited)` |
| **Queue** | Count of scheduled items + preview of next one's context |
| **Last cycle** | Time since last run + summary of what it did |
| **Next wake** | Estimated time until next cycle (from adaptive calculator) |
| **Budget** | Visual bar showing usage: user + ambient + remaining headroom |

### Budget Breakdown

The budget bar shows three segments:

```
User usage     Ambient usage    Remaining
████████████   ████             ░░░░░░░░░░
   45%           12%               43%
```

This gives the user immediate visibility into whether ambient is being too aggressive.

---

## Configuration

```toml
[ambient]
# Enable ambient mode (default: false until stable)
enabled = false

# Provider override (default: auto-select per priority chain)
# provider = "openai"

# Model override (default: provider's strongest)
# model = "5.2-codex-xhigh"

# Allow API key usage (default: false, only OAuth)
allow_api_keys = false

# Daily token budget when using API keys (ignored for OAuth)
# api_daily_budget = 100000

# Minimum interval between cycles in minutes (default: 5)
min_interval_minutes = 5

# Maximum interval between cycles in minutes (default: 120)
#
# This is a hard ceiling, and also the interval used whenever quota data is
# unavailable. Cycles run closer to `min_interval_minutes` while the
# subscription window has headroom (see "Subscription Headroom" above), so the
# gap between the two bounds is the range ambient actually paces within.
max_interval_minutes = 120

# Pause ambient when user has active session (default: true)
pause_on_active_session = true

# Enable proactive work (vs garden-only mode) (default: true)
proactive_work = true

# Proactive work branch prefix (default: "ambient/")
work_branch_prefix = "ambient/"

# Show ambient cycles in the session picker (default: false)
visible = false

# Auto-approve the agent's `request_permission` calls (default: false)
#
# Ambient runs unattended, so a permission request otherwise stalls the cycle
# waiting for a human who is not watching. When enabled, requests are approved
# automatically and recorded in the audit trail, and the cycle prompt tells the
# agent this is in effect.
auto_approve_permissions = false
```

---

## Storage

```
~/.jcode/ambient/
├── state.json              # Current ambient state (status, last run, etc.)
├── queue.json              # Scheduled queue (persistent across restarts)
├── usage.json              # Usage history for adaptive calculation
└── logs/
    └── ambient-YYYY-MM-DD.log  # Daily ambient activity logs
```

---

## Context Window Management

Ambient mode uses the same compaction strategy as interactive sessions: **compact at 80% context window usage.** No special handling needed — if an ambient cycle is analyzing a large memory graph or many sessions, it compacts and continues.

---

## User Feedback via Memory

Ambient learns from the user's approval/rejection decisions through the memory system itself. No separate feedback mechanism is needed.

- **User rejects a proactive change** → ambient stores a memory: *"User rejected ambient PR to refactor auth tests — prefers not to have tests auto-modified"*
- **User approves** → memory: *"User approved ambient fixing typos in docs"*
- **Pattern emerges** → these memories get reinforced over time, naturally influencing what ambient prioritizes

This works because ambient already scouts memories before deciding what to do. Its own approval/rejection history becomes part of the context it reasons about, and these memories consolidate, decay, and reinforce like everything else in the graph.

---

## Crash Safety & Recovery

Ambient must assume the process can die at any point (battery death, crash, OOM, etc.) and design so nothing is lost or corrupted.

### Principles

- **Atomic writes** — memory graph and state files are written to a temp file first, then atomically renamed. A crash mid-write doesn't corrupt existing data.
- **Incremental checkpointing** — if ambient is halfway through gardening 50 memories and crashes, it shouldn't redo the ones already finished. A "last processed" marker tracks progress within a cycle.
- **Persistent queue survives crashes** — scheduled queue and permission requests are on disk, not in memory. They survive restarts.
- **Interrupted transcripts** — if a cycle doesn't complete, the transcript is marked as `interrupted` rather than `completed`, so the user knows it didn't finish.

### Recovery on Restart

When ambient starts after an unexpected shutdown:

1. **Don't replay missed cycles** — don't try to run every cycle that was scheduled while the machine was off. Just run one cycle that examines current state.
2. **Check time since last run** — if the gap is large (hours/days), there may be a backlog of crashed sessions to extract, stale memories to verify, etc. The agent handles this naturally since it always checks current state rather than diffing from last run.
3. **Expired scheduled items** — still execute them. The context the agent stored is still valid, the work is just late.
4. **Resume, don't restart** — if a cycle was interrupted mid-way, check the checkpoint and continue from where it left off rather than starting over.

### State Diagram

```mermaid
stateDiagram-v2
    [*] --> Starting: jcode starts
    Starting --> CheckLastRun: ambient enabled?

    CheckLastRun --> NormalCycle: last run recent
    CheckLastRun --> CatchUpCycle: last run stale (hours/days)
    CheckLastRun --> ResumeCycle: interrupted cycle found

    NormalCycle --> Sleeping: cycle complete
    CatchUpCycle --> Sleeping: cycle complete
    ResumeCycle --> Sleeping: cycle complete

    Sleeping --> NormalCycle: timer/event fires
    Sleeping --> [*]: machine off / crash

    note right of CatchUpCycle: Single cycle examining\ncurrent state, not\nreplaying missed cycles

    note right of ResumeCycle: Continue from\ncheckpoint marker
```

---

## Cold Start

First time ambient runs, there's no usage history, no patterns, no feedback memories. Bootstrapping strategy:

- **Start conservative** — garden-only (memory maintenance), no proactive work until ambient has enough context
- **Build usage baseline** — first few cycles just observe and track usage patterns for the adaptive scheduler
- **Proactive work unlocks gradually** — after N successful garden cycles with user-approved results, ambient can start scouting for proactive work
- **Or user opts in immediately** — config option to skip the warm-up if the user trusts it

---

## Per-Project Configuration

### Pull requests: where the work shows up

Ambient's code work is only visible to you as a pull request. A pushed branch
with no PR looks identical to having done nothing.

```toml
[ambient]
pr_repo = "you/your-fork"   # PRs for THAT repo open here
```

With this set, the cycle prompt names the exact `gh pr create` command for that
fork and tells the agent never to target the upstream repository.

The setting names one repo, so it is scoped to that repo. Work in any other
project targets that project's own `origin`; the prompt says so explicitly,
because an unscoped "open every PR here" rule sends an unrelated project's
branch to this fork the moment ambient works across more than one repo.

Two traps this closes, both observed in real cycles:

- On a fork, `gh pr create` defaults to the UPSTREAM repo and fails with a
  permissions error even when you are an admin of your fork. The agent read
  that as "cannot open PRs" and left the branch unreviewed. Running
  `gh repo set-default OWNER/REPO` once per clone fixes the same failure for
  interactive sessions.
- Branches cut from a stale local base carry unrelated reverts. The agent must
  branch from the current remote head, and check that the diff against the
  default branch touches only files its task is about.

### Multi-project context

A single ambient agent serves every project, one cycle at a time. The cycle
itself has no working directory, so per-project context does not load
automatically. Two pieces of project awareness are therefore built into the
cycle prompt:

- **Recent Sessions** lines carry `project: <working dir>`, and a
  **Projects Active Recently** section ranks those directories. Without this the
  agent cannot tell which repo yesterday's work belonged to.
- **Memory Graph Health** lists every per-project memory graph found under
  `~/.jcode/memory/projects/`, with each project's path, size, and gardening
  backlog, and rolls those counts into the totals.

#### Choosing which project to work on

Session count answers "where has the user been", not "what matters". Left to
activity alone, the repo you happen to sit in all day crowds out the one you
actually care about. State the order instead:

```toml
[ambient]
project_priority = ["/home/you/work/main-app", "/home/you/src/side-project"]
```

Listed projects sort above unlisted ones in **Projects Active Recently**,
regardless of session counts, and are tagged `[priority]` so the agent can act
on the ranking rather than just read it. The prompt tells it to exhaust useful
work in a higher-priority project before dropping to a lower one. Queued and
scheduled items still run when they come due.

A listed project with no recent sessions is *not* dropped: it gets its own
**Priority Projects With No Recent Sessions** section. That is the case the
setting exists for, since the important project is often precisely the
neglected one.

Paths are absolute (a leading `~` is expanded) and match on directory
boundaries, so a session in a subdirectory counts toward its project while a
name-prefix sibling like `/src/jcode-cron` does not match `/src/jcode`.

Project graph files are named by a hash of the project path. Saving a project
memory records the reverse mapping in
`~/.jcode/memory/projects/index.json`; graphs written before that registry
existed are named by scanning recent session files for a matching working
directory, and fall back to showing the hash.

Reading is not the same as writing. `MemoryManager` only resolves a project
graph when it has a project directory, so a project-scoped `remember` from a
cycle with no working directory is dropped. To act on a specific project,
schedule work with that project's `working_dir`: the runner then gives the child
session the right directory, which restores its `AGENTS.md`, git state, and
project memory.

`ambient:prompt` on the debug socket dumps the exact context a cycle would see,
which is the quickest way to confirm what the agent can and cannot observe.

Some projects may need different ambient behavior (e.g. sensitive work projects, personal repos with different preferences):

```toml
# In project-level .jcode/config.toml
[ambient]
# Disable ambient entirely for this project
enabled = false

# Or restrict to garden-only (no proactive code changes)
proactive_work = false
```

---

## Multi-Machine (Deferred)

When ambient runs on multiple machines (e.g. laptop + desktop), shared state could conflict: double-processing sessions, conflicting memory edits, overlapping proactive work.

This is a distributed systems problem that will be addressed once ambient is stable on a single machine. Potential approaches:
- Machine ID on memory writes for conflict resolution
- Lock file or leader election for exclusive operations
- Git worktrees are already isolated, so proactive work is naturally conflict-free

---

## Implementation Phases

### Phase 1: Foundation
- [ ] Ambient agent loop (spawn, run, sleep)
- [ ] Single-instance guard
- [ ] Basic scheduling (fixed interval with max ceiling)
- [ ] Provider selection chain (OpenAI OAuth → Anthropic OAuth → pay-per-token opt-in → disabled)
- [ ] Configuration (`[ambient]` section in config)
- [ ] Storage layout

### Phase 2: Memory Consolidation — Garden
- [ ] Full graph-wide dedup scan
- [ ] Fact verification against codebase
- [ ] Retroactive session extraction (crashed/missed sessions)
- [ ] Pruning dead memories (low confidence + low strength)
- [ ] Relationship discovery across sessions
- [ ] Embedding backfill
- [ ] Contradiction resolution

### Phase 3: Scheduling
- [ ] `schedule_ambient` tool for agent self-scheduling
- [ ] Scheduled queue (persistent, with context)
- [ ] Adaptive resource calculator
- [ ] Usage history tracking
- [ ] Rate limit awareness (from provider response headers)
- [ ] Event triggers (session close, crash, git push)
- [ ] Active session detection → pause/throttle

### Phase 4: Proactive Work
- [ ] Scout: analyze recent sessions + git history
- [ ] Infer user priorities from memories
- [ ] Identify actionable work
- [ ] Execute on separate branch
- [ ] Report results

### Phase 5: Info Widget
- [ ] Ambient status display in TUI
- [ ] Queue preview
- [ ] Last cycle summary
- [ ] Next wake estimate
- [ ] Budget bar (user vs ambient vs remaining)

---

*Last updated: 2026-02-08*
