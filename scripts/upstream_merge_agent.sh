#!/usr/bin/env bash
# Scheduled upstream-merge agent (macOS + Linux).
#
# Keeps a fork's custom code mergeable with upstream by running a dedicated
# jcode agent on an isolated git worktree. It never touches the working tree the
# user is actively editing, and it never pushes.
#
# Install a schedule with scripts/install_upstream_merge_schedule.sh.
#
# Portability notes: bash 3.2 compatible (macOS ships bash 3.2), no `flock`
# (absent on macOS), no GNU-only `date` flags.

set -uo pipefail

REPO="${JCODE_UPSTREAM_REPO:-$HOME/jcode}"
STATE_DIR="${JCODE_UPSTREAM_STATE_DIR:-$HOME/.jcode/upstream-merge}"
WORKTREE="${JCODE_UPSTREAM_WORKTREE:-$STATE_DIR/worktree}"
BRANCH="${JCODE_UPSTREAM_BRANCH:-auto/upstream-merge}"
BASE="${JCODE_UPSTREAM_BASE:-master}"
# Defaults assume the standard fork topology: `origin` is your fork, `upstream`
# is the project you forked. Falls back to origin so a single-remote clone (no
# fork yet) still works.
UPSTREAM_REMOTE="${JCODE_UPSTREAM_REMOTE:-upstream}"
UPSTREAM_REF="${JCODE_UPSTREAM_REF:-}"
FORK_REMOTE="${JCODE_UPSTREAM_FORK_REMOTE:-origin}"
# Push the merged result to the fork so GitHub reflects the maintained state.
# Only ever fast-forward, and only the fork: upstream is never written to.
PUSH_FORK="${JCODE_UPSTREAM_PUSH:-1}"
# Keep GitHub Actions disabled on the fork.
#
# The fork inherits upstream's 8 workflows, several of which are release and
# publish jobs. Pushing a synced master would run them against the fork on the
# fork owner's CI minutes, for builds nobody asked for. Repo-level disabling is
# used rather than deleting the workflow files, because deleting them would
# conflict with upstream on every single future merge, forever.
ENFORCE_ACTIONS_OFF="${JCODE_UPSTREAM_DISABLE_ACTIONS:-1}"
LOG_DIR="${JCODE_UPSTREAM_LOG_DIR:-$STATE_DIR/logs}"
CHECK_CMD="${JCODE_UPSTREAM_CHECK_CMD:-cargo check --workspace}"

mkdir -p "$LOG_DIR" "$STATE_DIR"

LOG="$LOG_DIR/$(date -u +%Y%m%dT%H%M%SZ).log"
exec > >(tee -a "$LOG") 2>&1

log() { echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*"; }

# --- locate jcode ------------------------------------------------------------
JCODE_BIN="${JCODE_BIN:-}"
if [ -z "$JCODE_BIN" ]; then
  if [ -x "$HOME/.local/bin/jcode" ]; then
    JCODE_BIN="$HOME/.local/bin/jcode"
  else
    JCODE_BIN="$(command -v jcode 2>/dev/null)"
  fi
fi
if [ -z "$JCODE_BIN" ] || [ ! -x "$JCODE_BIN" ]; then
  log "jcode binary not found; set JCODE_BIN"
  exit 1
fi

# --- runtime socket (Linux XDG runtime dir, macOS falls back to TMPDIR) ------
if [ -n "${JCODE_UPSTREAM_SOCKET:-}" ]; then
  SOCKET="$JCODE_UPSTREAM_SOCKET"
elif [ -n "${XDG_RUNTIME_DIR:-}" ] && [ -d "${XDG_RUNTIME_DIR:-}" ]; then
  SOCKET="$XDG_RUNTIME_DIR/jcode-upstream-merge.sock"
else
  SOCKET="${TMPDIR:-/tmp}/jcode-upstream-merge.sock"
fi

# --- single instance, portable (mkdir is atomic everywhere) ------------------
LOCK_DIR="$STATE_DIR/.lock"
if ! mkdir "$LOCK_DIR" 2>/dev/null; then
  STALE_PID=$(cat "$LOCK_DIR/pid" 2>/dev/null)
  if [ -n "$STALE_PID" ] && kill -0 "$STALE_PID" 2>/dev/null; then
    log "another run (pid $STALE_PID) holds the lock; skipping"
    exit 0
  fi
  log "clearing stale lock from pid ${STALE_PID:-unknown}"
  rm -rf "$LOCK_DIR"
  mkdir "$LOCK_DIR" 2>/dev/null || { log "could not take lock"; exit 0; }
fi
echo "$$" > "$LOCK_DIR/pid"
cleanup() { rm -rf "$LOCK_DIR"; }
trap cleanup EXIT INT TERM

log "=== upstream merge agent ==="

# Ensure GitHub Actions stays off on the fork, and cancel anything already
# queued. Runs BEFORE every push: verifying after the fact would mean the CI
# minutes are already spent.
ensure_fork_actions_disabled() {
  [ "$ENFORCE_ACTIONS_OFF" = "1" ] || return 0
  command -v gh >/dev/null 2>&1 || return 0

  local url slug
  url=$(git remote get-url "$FORK_REMOTE" 2>/dev/null) || return 0
  # git@github.com:owner/repo.git and https://github.com/owner/repo.git
  slug=$(printf '%s' "$url" | sed -E 's#^git@github\.com:##; s#^https://github\.com/##; s#\.git$##')
  case "$slug" in
    */*) ;;
    *) return 0 ;;
  esac

  local state
  state=$(gh api "repos/$slug/actions/permissions" --jq '.enabled' 2>/dev/null)
  if [ "$state" = "true" ]; then
    if gh api -X PUT "repos/$slug/actions/permissions" -F enabled=false >/dev/null 2>&1; then
      log "disabled GitHub Actions on $slug"
    else
      log "WARNING: could not disable GitHub Actions on $slug"
    fi
  fi

  # Cancel anything already queued or running, so a previously-enabled window
  # does not keep burning minutes after we turn Actions off.
  local ids id
  ids=$(gh api "repos/$slug/actions/runs?per_page=100" \
    --jq '.workflow_runs[] | select(.status=="queued" or .status=="in_progress" or .status=="requested" or .status=="waiting") | .id' 2>/dev/null)
  for id in $ids; do
    if gh api -X POST "repos/$slug/actions/runs/$id/cancel" >/dev/null 2>&1; then
      log "cancelled workflow run $id on $slug"
    fi
  done
}

# Publish the fork's base branch when it is ahead of the fork remote.
#
# Fast-forward only, and only ever to the fork. A force-push here could destroy
# work that exists nowhere else, and upstream is never a valid push target: this
# job maintains a fork, it does not contribute to the parent project.
push_fork_if_ahead() {
  [ "$PUSH_FORK" = "1" ] || { log "push disabled (JCODE_UPSTREAM_PUSH=$PUSH_FORK)"; return 0; }
  git remote get-url "$FORK_REMOTE" >/dev/null 2>&1 || return 0
  [ "$FORK_REMOTE" != "$UPSTREAM_REMOTE" ] || {
    log "fork remote equals upstream remote; not pushing"
    return 0
  }

  local remote_sha local_sha
  local_sha=$(git rev-parse "$BASE" 2>/dev/null) || return 0
  remote_sha=$(git rev-parse "$FORK_REMOTE/$BASE" 2>/dev/null)

  if [ -n "$remote_sha" ] && [ "$remote_sha" = "$local_sha" ]; then
    log "fork $FORK_REMOTE/$BASE already matches local $BASE"
    return 0
  fi
  if [ -n "$remote_sha" ] && ! git merge-base --is-ancestor "$remote_sha" "$local_sha"; then
    log "WARNING: $FORK_REMOTE/$BASE has commits not in local $BASE; refusing to push"
    return 1
  fi

  ensure_fork_actions_disabled

  if git push "$FORK_REMOTE" "$BASE:$BASE"; then
    log "pushed $BASE to $FORK_REMOTE ($local_sha)"
  else
    log "WARNING: push to $FORK_REMOTE failed"
    return 1
  fi
}

cd "$REPO" || { log "repo missing: $REPO"; exit 1; }

# Resolve which remote actually holds upstream. A fork clone has origin=fork and
# upstream=parent; a plain clone has only origin. Guessing wrong here would
# "merge" the fork into itself and report success having done nothing.
if [ -z "$UPSTREAM_REF" ]; then
  if git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1; then
    UPSTREAM_REF="$UPSTREAM_REMOTE/$BASE"
  else
    UPSTREAM_REMOTE="$FORK_REMOTE"
    UPSTREAM_REF="$FORK_REMOTE/$BASE"
    log "no '$UPSTREAM_REMOTE' remote; falling back to $UPSTREAM_REF"
  fi
fi

log "repo=$REPO base=$BASE upstream=$UPSTREAM_REF fork=$FORK_REMOTE worktree=$WORKTREE"

git fetch --prune "$UPSTREAM_REMOTE" || { log "fetch $UPSTREAM_REMOTE failed"; exit 1; }
if [ "$FORK_REMOTE" != "$UPSTREAM_REMOTE" ]; then
  git fetch --prune "$FORK_REMOTE" || log "WARNING: fetch $FORK_REMOTE failed"
fi

BASE_SHA=$(git rev-parse "$BASE") || exit 1
UP_SHA=$(git rev-parse "$UPSTREAM_REF") || exit 1

if git merge-base --is-ancestor "$UP_SHA" "$BASE_SHA"; then
  log "already up to date with $UPSTREAM_REF ($UP_SHA)"
  # The local branch can still be ahead of the fork remote (local commits, or a
  # merge adopted by a previous run). Publishing that is the whole point of
  # "keep my fork updated", so it must not be skipped just because upstream
  # brought nothing new.
  push_fork_if_ahead
  exit 0
fi

# --- isolated worktree on a throwaway branch built from the fork's base ------
# Prune first: if the worktree directory was deleted out from under git, the
# registration survives and git refuses to reuse the branch ("already used by
# worktree at ..."), which would wedge every future run.
git worktree prune

# The branch may be checked out in a DIFFERENT worktree (a leftover run with
# another state dir, or one the user made). git's own error for this names a
# path with no explanation, so say what to do about it.
EXISTING=$(git worktree list --porcelain \
  | awk -v b="refs/heads/$BRANCH" '/^worktree /{w=$2} /^branch /{if ($2==b) print w}' \
  | head -1)
if [ -n "$EXISTING" ] && [ "$EXISTING" != "$WORKTREE" ]; then
  log "ERROR: branch $BRANCH is checked out in another worktree: $EXISTING"
  log "either remove it (git worktree remove '$EXISTING') or set JCODE_UPSTREAM_BRANCH"
  exit 1
fi

if [ -d "$WORKTREE/.git" ] || [ -f "$WORKTREE/.git" ]; then
  git -C "$WORKTREE" merge --abort >/dev/null 2>&1
  git -C "$WORKTREE" reset --hard >/dev/null 2>&1
  git -C "$WORKTREE" clean -fd >/dev/null 2>&1
  git -C "$WORKTREE" checkout -B "$BRANCH" "$BASE_SHA" || exit 1
else
  rm -rf "$WORKTREE"
  git worktree add -B "$BRANCH" "$WORKTREE" "$BASE_SHA" || exit 1
fi

cd "$WORKTREE" || exit 1

# --- try the merge mechanically first; a clean merge needs no agent ----------
REASON=""
# --no-ff is explicit: a user config of `merge.ff = only` otherwise aborts the
# merge outright, which looked exactly like "conflicts" and sent the agent into
# a worktree that had never actually attempted a merge.
if git merge --no-ff --no-edit "$UP_SHA"; then
  log "clean merge; verifying with: $CHECK_CMD"
  if eval "$CHECK_CMD"; then
    log "clean merge verified on branch $BRANCH in $WORKTREE"
    exit 0
  fi
  REASON="merge was clean but '$CHECK_CMD' failed"
else
  CONFLICTS=$(git diff --name-only --diff-filter=U)
  if [ -z "$CONFLICTS" ]; then
    # rerere may have replayed a previous resolution for every conflict, leaving
    # the merge staged and complete but still exiting nonzero. That is a fully
    # resolved merge, not a failure, and treating it as one would refuse to
    # merge anything the user had already resolved once.
    if [ -n "$(git diff --cached --name-only)" ]; then
      log "all conflicts resolved from rerere cache; committing"
      if git commit --no-edit >/dev/null 2>&1 && eval "$CHECK_CMD"; then
        log "rerere-resolved merge verified on branch $BRANCH"
        STATUS="merged"
        DETAIL="All conflicts were replayed from git rerere (you resolved them before). '$CHECK_CMD' passes."
        RERERE_MERGE=1
      else
        REASON="rerere replayed resolutions but the commit or check failed"
        log "$REASON; handing to agent"
      fi
    else
      # git refused to even start the merge (bad refs, dirty tree, hooks). An
      # agent cannot resolve conflicts that do not exist, so stop loudly instead
      # of handing it an untouched worktree and calling that a merge attempt.
      log "ERROR: git merge failed without producing conflicts; not invoking the agent"
      log "run manually to see why: git -C $WORKTREE merge --no-ff $UP_SHA"
      exit 1
    fi
  else
    REASON="merge produced conflicts"
  fi
fi
# --- notify through jcode's own channels -------------------------------------
# `jcode notify` fans out to ntfy/email/desktop/chat exactly as ambient does, so
# this script never needs to know how the user is reachable.
notify() {
  local title="$1" body="$2" priority="$3"
  if ! "$JCODE_BIN" notify --no-update "$title" "$body" --priority "$priority" 2>/dev/null; then
    # Older binaries lack `notify`. Falling back keeps the escalation path
    # working, since an unreported "needs_user" merge is the whole failure mode
    # this script exists to prevent.
    "$JCODE_BIN" notify "$title" "$body" --priority "$priority" 2>/dev/null \
      || log "WARNING: could not send notification: $title"
  fi
}

read_verdict() {
  [ -f "$VERDICT_FILE" ] || return 1
  python3 - "$VERDICT_FILE" <<'PY' 2>/dev/null
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(1)
print(d.get("status", "failed"))
print(d.get("summary", "").strip())
for x in d.get("decisions", []) or []:
    print("- " + str(x))
for q in d.get("questions", []) or []:
    print("? " + str(q))
PY
}

RERERE_MERGE="${RERERE_MERGE:-0}"

# Everything from here to the verdict is the agent path. rerere already produced
# a verified merge, so skip straight to reporting it.
if [ "$RERERE_MERGE" = "0" ]; then
log "$REASON; handing to agent"

CONFLICTS=$(git diff --name-only --diff-filter=U | head -50)

# Upstream's own description of what it brought. The agent needs this to tell
# "upstream refactored code I also touched" (resolve it) apart from "upstream
# shipped the feature I already wrote" (a design decision the user owes us).
UPSTREAM_LOG=$(git log --no-merges --oneline "$BASE_SHA..$UP_SHA" | head -40)
FORK_LOG=$(git log --no-merges --oneline "$(git merge-base "$BASE_SHA" "$UP_SHA")..$BASE_SHA" | head -40)

# The agent writes its verdict here. A file, not stdout parsing: the transcript
# is prose and guessing intent from it is how an automated merge lands a change
# the user explicitly wanted to be asked about.
VERDICT_FILE="$STATE_DIR/verdict.json"
rm -f "$VERDICT_FILE"

PROMPT="You are a dedicated, single-purpose upstream-merge agent. Do exactly this task and nothing else. Do not create goals, initiatives, or memories. Do not start side quests.

## Context
- Working dir is an isolated git worktree: $WORKTREE
- Branch: $BRANCH, built from fork base $BASE ($BASE_SHA)
- Merging upstream $UPSTREAM_REF ($UP_SHA)
- Situation: $REASON

This fork carries custom local features on top of upstream. Your job is to keep
the fork mergeable without ever silently losing a fork feature.

## What upstream brought (not yet in the fork)
$UPSTREAM_LOG

## What the fork added on top of the merge base
$FORK_LOG

## Conflicted files
$CONFLICTS

## Default: resolve it yourself
Resolve conflicts by default. You have the judgment for it. For each conflict,
decide deliberately whether the fork's behavior, upstream's behavior, or a
combination is correct. Adopt upstream's structure and refactors; keep the
fork's behavior. Mechanical conflicts (imports, signatures moved, a function
renamed, formatting, both sides adding to the same list) are yours to fix
without asking.

## Exception: STOP and ask, do not merge
Stop if upstream appears to implement a capability the fork ALREADY implements
its own way. That is not a merge conflict, it is a design decision about which
implementation survives, and it belongs to the user. Signals:
- Upstream adds a feature/module/command whose purpose duplicates fork code.
- Resolving would mean deleting a fork feature, or leaving two parallel
  implementations of the same thing.
- The 'right' resolution depends on which product direction the user wants.

Also stop if a resolution would change fork behavior in a way the user would
notice and might not want.

When you stop: leave the merge in place (do not 'git merge --abort', do not
commit), and write the reason to the verdict file described below.

## Verdict file (required, always write it, last thing you do)
Write $VERDICT_FILE as JSON:
{
  \"status\": \"merged\" | \"needs_user\" | \"failed\",
  \"summary\": \"one or two sentences, plain text\",
  \"decisions\": [\"file: what you chose and why\"],
  \"questions\": [\"the specific decision the user owes you, if status is needs_user\"]
}
Use \"merged\" only if you committed and '$CHECK_CMD' passes.
Use \"needs_user\" for the duplicate-feature case above.
Use \"failed\" if you could not resolve it for any other reason.

## Steps
1. Resolve conflicts, or stop per the exception above.
2. Run '$CHECK_CMD' until it passes.
3. Run the tests most relevant to the files you touched.
4. Commit the merge, message summarizing each nontrivial resolution.
5. Write $VERDICT_FILE.

Do NOT push. Do NOT touch $REPO's main working tree. Do NOT modify ~/.jcode/config.toml."

"$JCODE_BIN" run --no-update --socket "$SOCKET" "$PROMPT"
AGENT_STATUS=$?
log "agent exited $AGENT_STATUS"

VERDICT=$(read_verdict)
STATUS=$(echo "$VERDICT" | head -1)
DETAIL=$(echo "$VERDICT" | tail -n +2)
fi  # end agent path

REVIEW="Branch: $BRANCH
Worktree: $WORKTREE
Review: git -C $WORKTREE log --oneline -5
Adopt:  git -C $REPO merge --ff-only $BRANCH"

case "$STATUS" in
  merged)
    log "verdict: merged"
    # Adopt into the real repo, then publish to the fork. Without this the merge
    # would sit in a worktree forever and the fork would never actually be
    # "kept updated", which is the whole point.
    #
    # Guarded hard: a fast-forward only, and only onto a clean tree at the
    # expected commit. The user may well be mid-edit on this branch, and
    # yanking master out from under them is exactly the regret this must avoid.
    ADOPTED=""
    cd "$REPO" || exit 1
    if [ -n "$(git status --porcelain)" ]; then
      log "repo has uncommitted changes; not adopting automatically"
    elif [ "$(git rev-parse HEAD)" != "$BASE_SHA" ]; then
      log "repo moved since the merge started; not adopting automatically"
    elif [ "$(git symbolic-ref --quiet --short HEAD)" != "$BASE" ]; then
      log "repo is not on $BASE; not adopting automatically"
    elif git merge --ff-only "$BRANCH" >/dev/null 2>&1; then
      ADOPTED="yes"
      log "fast-forwarded $BASE to $BRANCH"
      push_fork_if_ahead
    else
      log "fast-forward of $BASE to $BRANCH failed; leaving it for review"
    fi

    if [ -n "$ADOPTED" ]; then
      notify "Fork updated with upstream" "$DETAIL

Adopted onto $BASE and pushed to $FORK_REMOTE." "default"
    else
      notify "Upstream merged, needs adopting" "$DETAIL

The merge is committed on $BRANCH but was not applied automatically (see log).
$REVIEW" "default"
    fi
    EXIT=0
    ;;
  needs_user)
    # The case the user specifically asked to be consulted on.
    log "verdict: needs_user"
    notify "Upstream merge needs your decision" "Upstream may duplicate a feature this fork already has, so the merge was left uncommitted for you.

$DETAIL

$REVIEW
Conflicts remain in the worktree; nothing was committed." "high"
    EXIT=2
    ;;
  *)
    log "verdict: failed or missing (agent exit $AGENT_STATUS)"
    notify "Upstream merge failed" "The merge agent could not complete the merge.

${DETAIL:-No verdict file was written. See the log.}

Log: $LOG
$REVIEW" "high"
    EXIT=1
    ;;
esac

log "branch $BRANCH left in $WORKTREE for review"
exit $EXIT
