# Per-project ambient mode

Tracking issue: [#126](https://github.com/potb/jcode/issues/126).

Ambient mode is currently global: one queue, one `state.json`, one scheduler,
one lock, and one prompt that concatenates every configured project and lets
the model choose where to work. The issue asks for those to become per-project
so that a busy project cannot starve a quiet one and a cycle's history is
attributable to the project it was about.

The conversion is staged, because state, queue, scheduler and lock all have to
agree on *which project an item belongs to* before any of them can be split.
This document covers stage 1: project identity.

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
  scheduler and the prompt still read this slot and are unchanged by stage 2.
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

## Remaining stages

3. Per-project scheduler and lock (only meaningful once 1 and 2 exist).
4. Per-project prompt and cycle — the observable behaviour change, last.
