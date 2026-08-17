# Per-project ambient mode

Tracking issue: [#126](https://github.com/potb/jcode/issues/126).

Ambient mode used to be global: one queue, one `state.json`, one scheduler,
one lock, and one prompt that concatenated every configured project and let
the model choose where to work. The issue asked for those to become per-project
so that a busy project cannot starve a quiet one and a cycle's history is
attributable to the project it was about.

All four stages have shipped, and this document describes the result. The
conversion was staged because state, queue, scheduler and lock all have to
agree on *which project an item belongs to* before any of them can be split,
and the stage headings below are kept because they explain why each mechanism
is shaped the way it is.

## Project identity

`ambient::prompt::resolve_project_key` maps a working directory to the
canonical path of the configured project that owns it, or `None` when no
configured project does.

Three properties matter, and they are the reason identity is resolved in one
place rather than re-derived from a raw `working_dir` string at each use site:

- **Canonical.** The answer is the *configured* path (`~` expanded, no trailing
  slash), never the caller's directory. Two items created in different
  subdirectories of the same project therefore key to the same project, which
  is what makes the key usable as a state or queue partition.
- **Path-boundary matching.** A subdirectory belongs to its project, but
  `/src/jcode-cron` must not match `/src/jcode`. This is the same rule
  `priority_rank` uses.
- **Window-independent.** Identity reads the configured projects, not the
  currently *workable* ones. If it used the workable list, an item would change
  owner when a project's `active_windows` closed — a silent reassignment at a
  wall-clock boundary, and exactly the class of bug #128 fixed on the gating
  side.

`None` is a real category, not an error: gardening and other non-project cycles
legitimately belong to no project.

`ScheduledItem::project` records the key at schedule time and is populated by
`AmbientManager::schedule`. It is `#[serde(default)]`, so an existing
`queue.json` written before this field loads unchanged with `project: None`,
and `ScheduledItem::project_key()` falls back to resolving `working_dir` on
read. That fallback is what keeps items queued by an older build attributable
without a migration pass over the file.

## Per-project state

Stage 2. `state.json` used to hold one flat `AmbientState`, so a cycle spent on
one project overwrote the visible history of every other one. It now holds an
envelope, `AmbientStateFile`:

- `global` — exactly the old shape. Cycles that belong to no project
  (gardening, memory work) are real, and the daemon's own scheduling status is
  a single process-wide fact, so both keep living here. `ambient status`, the
  scheduler and the prompt read this slot, and still do after stage 4: a
  focused cycle writes its project slot *and* this one.
- `projects` — a map keyed by the stage 1 project identity, so a project's
  cycle count, last run and last summary are attributable to it.

`AmbientStateFile::record_cycle(project, result)` updates the project slot when
there is one, and always updates `global`.

### Migration

The acceptance criterion on #126 says migrate, not discard, so a pre-envelope
`state.json` is not defaulted away: it parses as a bare object with neither
`global` nor `projects`, and the whole legacy record is adopted as `global`.
The live daemon's 145+ cycle count therefore survives the upgrade. Reading
never rewrites the file; the envelope shape reaches disk on the next save.

`AmbientState::load`/`save` remain the daemon's read and write path and now go
through the envelope. `save` re-reads the file and replaces only the `global`
slot, so a component that knows nothing about per-project state cannot destroy
it.

## Per-project scheduler and lock

Stage 3. It added the mechanism without changing observable behaviour, which
stage 4 then wired into the runner: the loop registers every workable project
in a `ProjectWakeLedger` and takes the per-project lock described below.

### Lock

`AmbientLock::try_acquire_for(project)` takes a lock per project, under
`~/.jcode/ambient/locks/`. Two projects can therefore run concurrently, which
is the point: a long cycle in one project must not exclude every other one.

`project = None` maps to the pre-existing `~/.jcode/ambient/ambient.lock`, byte
for byte, so a daemon on the unconverted path still contends with older builds
on exactly the file they used. Single-instance protection is unchanged *within*
a project: a live foreign PID still blocks acquisition, and the `server reload`
re-exec case (a lock naming our own PID is stale by definition) is inherited
rather than reimplemented.

The lock file name is a sanitized tail of the project path plus a hash of the
*whole* key. The sanitizer is lossy — a project key is an absolute path, so it
contains separators — and the hash is what guarantees uniqueness. Without it,
`/a/b` and `/a.b` would sanitize to the same name and silently serialize.

### Turn-taking

`ProjectWakeLedger` records when each project may next run and answers "which
project is due now". It deliberately does *not* hold quota headroom or
rate-limit backoff: a provider limit is account-wide, not per project, so those
stay in `AdaptiveScheduler`. What is genuinely per project is whose turn it is.

Two properties are what make it fair rather than merely partitioned:

- **A newly registered project is due immediately**, rather than waiting out an
  interval it was never part of.
- **The due project that has waited longest goes first.** Round-robin by
  insertion order would let a project with cheap, fast cycles keep coming back
  around ahead of one that has been waiting — the starvation #126 is about.

`None` is a scheduling participant like any other, since gardening cycles are
real work that must still get turns.

## Per-project prompt and cycle

Stage 4, and the first stage with an observable behaviour change: a cycle now
belongs to one project.

### Choosing the project

`select_cycle_project` answers "whose cycle is this". A due queue item wins,
because an item queued for a project is explicit work for it and outranks a
rotation; otherwise the ledger's longest-waiting due project takes the turn,
and `None` — the unfocused, project-less cycle — falls out when nothing is due.
An item with no project of its own does *not* force an unfocused cycle, since
gardening work is doable inside any cycle while a due project is not.

Every workable project is registered on each loop pass, so a project added to
config while the daemon is running joins the rotation without a restart. Only
*workable* projects are registered: a project whose window is closed cannot
take a turn, which is the acceptance criterion #126 states.

The cycle then takes that project's lock rather than the global one, so two
projects can run concurrently, and records its result through
`record_cycle_result_for`, which writes the project slot and the global slot in
one load/save of the envelope.

### The focused prompt

`build_ambient_system_prompt_for(focus, ...)` renders the prompt for one
project. `focus = None` is byte for byte the old prompt, so an unfocused cycle
and a config with no projects are unchanged.

With a focus, everything project-shaped narrows to that project: the priority
walk, the "projects active recently" list, the PR targets, the per-project
standing instructions, and the recent sessions. Naming another project at all
is what let a cycle work outside its own turn, so the filter is on the data,
not only on the instructions. The "work through the priority list" section is
dropped entirely, since telling a one-project cycle to move on to the next
project contradicts its own scope.

Session filtering is path-boundary matching, not equality: a session started in
a subdirectory of the focused project belongs to it.
