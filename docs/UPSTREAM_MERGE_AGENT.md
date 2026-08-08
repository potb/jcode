# Scheduled Upstream Merge Agent

Keeps a fork of jcode (or any repo) mergeable with upstream by running a
dedicated, single-purpose agent on a schedule. It is deliberately **not** an
ambient agent: ambient mode allows only one instance at a time and reasons about
goals and initiatives, so using it here would compete with whatever you actually
assigned your ambient agent.

This is one task, on a timer, in its own isolated worktree.

## Why not ambient mode

- Only one ambient cycle ever runs (`AmbientLock`, `~/.jcode/ambient/ambient.lock`).
  A long merge would block your real ambient work.
- Ambient is goal/initiative-driven and self-scheduling. You want a fixed task on
  a fixed cadence.

## Install

```bash
scripts/install_upstream_merge_schedule.sh --interval-hours 6
```

Picks the right scheduler for the platform:

| Platform | Mechanism | Location |
| --- | --- | --- |
| Linux | systemd user timer | `~/.config/systemd/user/jcode-upstream-merge.{service,timer}` |
| macOS | launchd user agent | `~/Library/LaunchAgents/com.jcode.upstream-merge.plist` |
| other | prints a crontab line | — |

Other commands:

```bash
scripts/install_upstream_merge_schedule.sh --status
scripts/install_upstream_merge_schedule.sh --run-now     # run once, right now
scripts/install_upstream_merge_schedule.sh --uninstall
```

## What a run does

1. `git fetch origin`. If the fork already contains upstream, exit silently.
2. Build/refresh an isolated worktree at `~/.jcode/upstream-merge/worktree` on
   branch `auto/upstream-merge`, from the fork's base commit. **Your working
   tree is never touched.**
3. Attempt `git merge --no-ff` mechanically. Clean merge plus a passing check
   means no agent runs at all, and no notification is sent.
4. Otherwise hand the conflicts to a single-purpose jcode agent.
5. Notify through jcode's configured channels and leave the branch for review.

Nothing is ever pushed, and the merge is never applied to your working tree
automatically. Adopt a good result yourself:

```bash
git -C ~/jcode merge --ff-only auto/upstream-merge
```

## Agent policy: resolve by default, stop on duplicated features

The agent resolves conflicts on its own by default. Mechanical conflicts
(imports, renames, moved signatures, both sides appending to a list) and
"upstream refactored code the fork also touched" are its job.

It stops and asks when upstream appears to **implement something the fork
already implements its own way**. That is not a merge conflict, it is a product
decision about which implementation survives, so it belongs to you. In that case
the agent commits nothing, leaves the conflicts in place, and escalates.

To judge this it is given both sides' history: upstream commits not yet in the
fork, and fork commits since the merge base.

The agent reports via `~/.jcode/upstream-merge/verdict.json`:

```json
{
  "status": "merged | needs_user | failed",
  "summary": "...",
  "decisions": ["file: what was chosen and why"],
  "questions": ["the decision the user owes, when needs_user"]
}
```

A file rather than parsed stdout, on purpose: transcripts are prose, and
guessing intent from prose is how an automated merge lands a change you
explicitly wanted to be asked about.

Exit codes: `0` merged, `2` needs your decision, `1` failed.

## Notifications

The script shells out to `jcode notify`, which uses the same
`NotificationDispatcher` ambient mode uses. Whatever you configured under
`[safety]` (ntfy, email, desktop, Telegram, Discord) receives it, and the script
itself knows nothing about how you are reachable.

```bash
jcode notify "title" "body" --priority high
echo "body" | jcode notify "title" --priority urgent
```

ntfy topics are readable by anyone who knows the name, so the dispatcher sends
the safe body there; `--safe-body` overrides what goes to ntfy specifically.

Priorities: `merged` notifies at default, `needs_user` and `failed` at high.

## Configuration

All via environment variables, so the systemd unit and plist stay generic:

| Variable | Default | Meaning |
| --- | --- | --- |
| `JCODE_UPSTREAM_REPO` | `~/jcode` | Fork repo |
| `JCODE_UPSTREAM_BASE` | `master` | Fork branch to merge into |
| `JCODE_UPSTREAM_REF` | `origin/master` | Upstream ref to merge from |
| `JCODE_UPSTREAM_BRANCH` | `auto/upstream-merge` | Scratch result branch |
| `JCODE_UPSTREAM_STATE_DIR` | `~/.jcode/upstream-merge` | Worktree, logs, lock, verdict |
| `JCODE_UPSTREAM_CHECK_CMD` | `cargo check --workspace` | Build check the agent must make pass |
| `JCODE_BIN` | `~/.local/bin/jcode` | jcode binary |

If your fork tracks a separate upstream remote, add it and point at it:

```bash
git remote add upstream https://github.com/1jehuang/jcode.git
JCODE_UPSTREAM_REF=upstream/master scripts/install_upstream_merge_schedule.sh
```

## Portability notes

Written for bash 3.2 (macOS ships 3.2), no `flock` (absent on macOS), no
GNU-only `date` flags. Concurrency is guarded by an atomic `mkdir` lock with
stale-PID recovery, so an interrupted run does not wedge the schedule.

## Logs

`~/.jcode/upstream-merge/logs/<timestamp>.log`, one per run.
