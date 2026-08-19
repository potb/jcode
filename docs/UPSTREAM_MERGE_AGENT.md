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

## Remotes

Assumes the standard fork layout, which `gh repo fork` produces:

| Remote | Points at | Written to? |
| --- | --- | --- |
| `origin` | your fork | yes, via a pull request into the base branch |
| `upstream` | the project you forked | never |

A clone with only `origin` still works; the script falls back to it and logs
that it did.

## What a run does

1. Fetch `upstream` (and `origin`). If the fork already contains upstream,
   publish any unpushed local commits and exit.
2. Build/refresh an isolated worktree at `~/.jcode/upstream-merge/worktree` on
   branch `auto/upstream-merge`, from the fork's base commit. **Your working
   tree is never touched.**
3. Attempt `git merge --no-ff` mechanically. Clean merge plus a passing check
   means no agent runs at all, and no notification is sent.
4. Otherwise hand the conflicts to a single-purpose jcode agent.
5. Notify through jcode's configured channels and leave the branch for review.

On a verified merge it publishes the result and then adopts it:

6. Push the merge commit to `auto/upstream-merge-pr` on your fork, open a pull
   request into the base branch, and merge it.
7. Move your local base branch onto the fork's new merge commit.

Publishing comes first on purpose. Keeping the fork current is what this job is
for, and it must not wait on your checkout being idle.

The publish step goes through a pull request because the fork's base branch is
protected by a ruleset that forbids direct pushes.

**This pull request is merged, not squashed**, even though ordinary pull
requests on the fork are squashed. The point of this job is for the fork to sit
on top of upstream, and that requires upstream's commits to be genuine ancestors
of your base branch, which only a two-parent merge commit provides. A squash
would replace them with one new commit merely containing the same code: upstream
SHAs would be unreachable, `git merge-base` would still report the old fork
point, and every later run would re-merge the same history.

`JCODE_UPSTREAM_MERGE_METHOD=squash` is supported but defeats the purpose.

## It keeps working while you work

Every step that moves history is built with `git merge-tree` and
`git commit-tree`, which need no index and no working tree. So an editing
session cannot stall the job, and your git config cannot break it: with
`merge.ff = only`, which is a common setting, an ordinary `git merge` of a
diverged branch aborts outright.

Your base branch is the one thing treated as yours:

- It never moves while that worktree has uncommitted changes.
- It never moves onto a commit missing anything it already carries.
- It moves as a plain ref update when no worktree has it checked out, so working
  on a feature branch does not hold the job up.

When the branch cannot move, the fork is still published and a later run adopts
the result once your tree settles. Nothing is left for you to do by hand.

Publishing only ever targets the fork, never upstream, and only the dedicated PR
branch is force-pushed, which this job owns entirely. A commit carrying a build
directory (`target`, `target-base`, `node_modules`, `.direnv`) is never pushed,
since one bad `git add` is gigabytes of objects and a push cannot be undone on
the remote. Override the list with `JCODE_UPSTREAM_FORBIDDEN_PATHS`.

Set `JCODE_UPSTREAM_PUSH=0` to keep everything local. Publishing needs `gh`
installed and authenticated.

## When your base branch and the fork's disagree

Two situations look identical at the SHA level, and both are normal:

- **A history rewrite on GitHub** (squashing, reordering, dropping commits)
  leaves the remote with no ancestry link to your local branch.
- **Both sides gained commits**, which is what happens whenever you commit
  locally while a pull request merges on the fork.

Content decides which one it is. The two branches are merged in memory and the
result compared against the remote:

- **Your branch adds nothing** (the usual squash or reorder): it is an outdated
  encoding of the same code, so the remote is adopted and the run continues.
- **Both sides carry real work**: they are merged, and the merge is published
  like any other. Nothing is lost and nothing needs your attention.
- **The two conflict**: nothing is published or moved, and you get one
  high-priority notification. Resolve it once and the job resumes on its own.

Repeats of the same notification are suppressed for 24 hours, so a condition
waiting on you is reported once rather than on every scheduled run. Tune with
`JCODE_UPSTREAM_NOTIFY_REPEAT_HOURS`, or set it to `0` to always notify.

## GitHub Actions are kept disabled on the fork

A fork inherits all of upstream's workflows, including release and publish
jobs. A synced `master` would trigger them and spend your CI minutes building
artifacts nobody asked for.

Before every publish the script confirms Actions are disabled on the fork via the
API, disables them if something re-enabled them, and cancels any queued or
running workflow. This is done at the repo level rather than by deleting
`.github/workflows/`, because deleting those files would conflict with upstream
on every future merge, forever.

Opt out with `JCODE_UPSTREAM_DISABLE_ACTIONS=0`. To re-enable by hand, PUT
`enabled=true` to the repo's `actions/permissions` API endpoint.

## Already-resolved conflicts are free

If `git rerere` is enabled, a conflict you resolved once is replayed
automatically on later runs. The script detects a fully rerere-resolved merge,
commits and verifies it directly, and never spends an agent run on it.

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
| `JCODE_UPSTREAM_REMOTE` | `upstream` | Remote holding the parent project |
| `JCODE_UPSTREAM_REF` | `<remote>/<base>` | Upstream ref to merge from |
| `JCODE_UPSTREAM_FORK_REMOTE` | `origin` | Remote holding your fork |
| `JCODE_UPSTREAM_PUSH` | `1` | Publish the adopted merge to the fork via a pull request |
| `JCODE_UPSTREAM_PR_BRANCH` | `auto/upstream-merge-pr` | Branch the pull request is opened from |
| `JCODE_UPSTREAM_MERGE_METHOD` | `merge` | Pull request merge method; keep `merge` to stay on top of upstream |
| `JCODE_UPSTREAM_DISABLE_ACTIONS` | `1` | Keep GitHub Actions off on the fork |
| `JCODE_UPSTREAM_BRANCH` | `auto/upstream-merge` | Scratch result branch |
| `JCODE_UPSTREAM_STATE_DIR` | `~/.jcode/upstream-merge` | Worktree, logs, lock, verdict |
| `JCODE_UPSTREAM_CHECK_CMD` | `cargo check --workspace` | Build check the agent must make pass |
| `JCODE_UPSTREAM_NOTIFY_REPEAT_HOURS` | `24` | Suppress a repeated notification title for this long; `0` always notifies |
| `JCODE_UPSTREAM_FORBIDDEN_PATHS` | `target target-base node_modules .direnv` | Paths that must never be published |
| `JCODE_BIN` | `~/.local/bin/jcode` | jcode binary |

Setting up a fork from scratch:

```bash
gh repo fork 1jehuang/jcode --clone=false
gh api -X PUT "repos/OWNER/jcode/actions/permissions" -F enabled=false
git remote set-url origin "git@github.com:OWNER/jcode.git"
git remote add upstream "https://github.com/1jehuang/jcode.git"
scripts/install_upstream_merge_schedule.sh --interval-hours 6
```

## Portability notes

Written for bash 3.2 (macOS ships 3.2), no `flock` (absent on macOS), no
GNU-only `date` flags. Concurrency is guarded by an atomic `mkdir` lock with
stale-PID recovery, so an interrupted run does not wedge the schedule.

## Logs

`~/.jcode/upstream-merge/logs/<timestamp>.log`, one per run.

## Tests

```bash
scripts/tests/run_all.sh
```

Real git repos with `gh` and `jcode` stubbed, no network. `upstream_merge_e2e_test.sh`
runs the whole script through the situation that broke it: new upstream commits,
a fork base branch that moved on GitHub, local commits, and an uncommitted edit
session throughout, under `merge.ff = only`. No cargo target reaches this
scheduled shell job, so its regressions are only ever caught here.
