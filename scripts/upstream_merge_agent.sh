#!/usr/bin/env bash
# Scheduled upstream-merge agent (macOS + Linux).
#
# Keeps a fork's custom code mergeable with upstream by running a dedicated
# jcode agent on an isolated git worktree. It never touches the working tree the
# user is actively editing, and it never writes to upstream. The merged result
# is published to the fork through a pull request (see publish_fork_if_ahead).
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
# Publish the merged result to the fork so GitHub reflects the maintained state.
# The fork's default branch is protected by a ruleset that requires a pull
# request, so the branch is pushed and a PR is opened and merged through the
# API instead of pushing to the base branch directly. Upstream is never
# written to.
PUSH_FORK="${JCODE_UPSTREAM_PUSH:-1}"
# Pull request merge method. Must stay "merge" for this job, even though
# ordinary pull requests on this fork are squashed.
#
# The whole point here is for the fork to sit on top of upstream. That requires
# upstream's commits to be genuine ancestors of the fork's base branch, which
# only a real two-parent merge commit gives. A squash would replace them with a
# single new commit that merely contains the same code: upstream SHAs would no
# longer be reachable, `git merge-base` would still report the old fork point,
# and every later run would try to merge the same upstream history again.
PR_MERGE_METHOD="${JCODE_UPSTREAM_MERGE_METHOD:-merge}"
# Whether this job merges its own pull request. Defaults to 1: staying aligned
# with upstream is the entire point, so the update lands on the fork's base
# branch without waiting for a human.
#
# The pull request is not ceremony. The base branch is protected by a ruleset
# that requires one, and it doubles as the recovery path: if the merge fails
# (checks red, a conflict GitHub sees that git did not, no credentials), the
# branch stays open and every later run force-pushes the refreshed merge onto
# it, so upstream updates accumulate in that one pull request instead of being
# lost. Set to 0 to always stop at the open pull request and merge by hand.
AUTO_MERGE_PR="${JCODE_UPSTREAM_AUTO_MERGE:-1}"
# Branch the pull request is opened from. Owned entirely by this job.
PR_BRANCH="${JCODE_UPSTREAM_PR_BRANCH:-auto/upstream-merge-pr}"
# Keep GitHub Actions disabled on the fork.
#
# The fork inherits upstream's 8 workflows, several of which are release and
# publish jobs. Pushing a synced master would run them against the fork on the
# fork owner's CI minutes, for builds nobody asked for. Repo-level disabling is
# used rather than deleting the workflow files, because deleting them would
# conflict with upstream on every single future merge, forever.
ENFORCE_ACTIONS_OFF="${JCODE_UPSTREAM_DISABLE_ACTIONS:-1}"
LOG_DIR="${JCODE_UPSTREAM_LOG_DIR:-$STATE_DIR/logs}"
# How long the same notification stays suppressed, in hours. The job runs on a
# short schedule, so a condition the user has to fix by hand would otherwise be
# reported on every single run, which trains them to ignore it.
NOTIFY_REPEAT_HOURS="${JCODE_UPSTREAM_NOTIFY_REPEAT_HOURS:-24}"
# Paths that must never reach the fork. A build directory committed by accident
# is gigabytes of objects, and pushing it is not undoable on the remote.
FORBIDDEN_PUBLISH_PATHS="${JCODE_UPSTREAM_FORBIDDEN_PATHS:-target target-base node_modules .direnv}"
# Verification for a merge, mechanical or agent-produced.
#
# `cargo check --workspace` alone is not enough: the ratchet gates
# (scripts/*_budget.json) are baselines that only upstream's own CI would
# enforce, and Actions are deliberately disabled on the fork (see
# ENFORCE_ACTIONS_OFF above). So an upstream merge that adds oversized files,
# panic-prone calls or swallowed errors compiles fine, lands on master, and
# leaves every ratchet script failing from then on — at which point the gates
# report a red baseline no matter what the *next* change does, and stop being
# able to reject anything. That is how all four drifted red before this was
# added. Checking them at merge time keeps the baselines describing the fork's
# real state, and makes the growth visible in the merge's own log.
RATCHET_CMD='python3 scripts/check_code_size_budget.py && python3 scripts/check_test_size_budget.py && python3 scripts/check_panic_budget.py && python3 scripts/check_swallowed_error_budget.py && python3 scripts/check_wildcard_reexport_budget.py'
CHECK_CMD="${JCODE_UPSTREAM_CHECK_CMD:-cargo check --workspace && cargo fmt --all -- --check && $RATCHET_CMD}"

mkdir -p "$LOG_DIR" "$STATE_DIR"

LOG="$LOG_DIR/$(date -u +%Y%m%dT%H%M%SZ).log"
exec > >(tee -a "$LOG") 2>&1

# Stderr, not stdout: both are teed into the same log file, and functions here
# return values through stdout, which log lines would otherwise corrupt.
log() { echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*" >&2; }

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

# --- notify through jcode's own channels -------------------------------------
# `jcode notify` fans out to ntfy/email/desktop/chat exactly as ambient does, so
# this script never needs to know how the user is reachable.
#
# Repeats of an identical title are suppressed for NOTIFY_REPEAT_HOURS. Every
# condition that reaches a notification here needs a human, and the schedule
# fires far more often than a human answers, so without this the same message
# arrives dozens of times and becomes noise to swipe away.
notify() {
  local title="$1" body="$2" priority="$3"
  if notify_is_muted "$title"; then
    log "notification suppressed (sent within ${NOTIFY_REPEAT_HOURS}h): $title"
    return 0
  fi
  notify_record "$title"
  if ! "$JCODE_BIN" notify --no-update "$title" "$body" --priority "$priority" 2>/dev/null; then
    # Older binaries lack `notify`. Falling back keeps the escalation path
    # working, since an unreported "needs_user" merge is the whole failure mode
    # this script exists to prevent.
    "$JCODE_BIN" notify "$title" "$body" --priority "$priority" 2>/dev/null \
      || log "WARNING: could not send notification: $title"
  fi
}

# Where the last send time of one notification title is remembered. The title is
# reduced to a filename-safe key rather than hashed, so the state directory
# stays readable when someone wonders why a message stopped arriving.
notify_stamp_file() {
  local key
  key=$(printf '%s' "$1" | tr -cs 'A-Za-z0-9' '-' | cut -c1-80)
  printf '%s/notified/%s' "$STATE_DIR" "$key"
}

notify_is_muted() {
  local stamp now last
  [ "$NOTIFY_REPEAT_HOURS" != "0" ] || return 1
  stamp=$(notify_stamp_file "$1")
  [ -f "$stamp" ] || return 1
  last=$(cat "$stamp" 2>/dev/null)
  case "$last" in ''|*[!0-9]*) return 1 ;; esac
  now=$(date -u +%s)
  [ $((now - last)) -lt $((NOTIFY_REPEAT_HOURS * 3600)) ]
}

notify_record() {
  local stamp
  stamp=$(notify_stamp_file "$1")
  mkdir -p "$(dirname "$stamp")"
  date -u +%s > "$stamp"
}

# The one worktree that has a branch checked out, empty when no worktree does.
# A branch nobody has checked out can be moved with update-ref alone, which is
# what lets this job keep working while the user is mid-edit elsewhere.
worktree_holding_branch() {
  git -C "$REPO" worktree list --porcelain \
    | awk -v b="refs/heads/$1" '/^worktree /{w=$2} /^branch /{if ($2==b) print w}' \
    | head -1
}

# Move the real repo's base branch to a commit this job produced or published.
#
# Never over uncommitted work, and never onto a commit that lacks anything the
# branch already has: the user is often mid-edit here, and dropping their work
# is the one regret this job must never cause. The target is usually a
# descendant; it is a rewrite of the same content when the fork squashes.
# When the branch is not checked out anywhere the ref moves directly, so working
# on a feature branch no longer stalls the job.
#
# Failure is normal and not an error: the fork is already published by the time
# this runs, and the next run adopts the same commit once the tree settles.
adopt_base_ref() {
  local target="$1" local_sha holder
  local_sha=$(git -C "$REPO" rev-parse "$BASE" 2>/dev/null) || return 1
  [ "$local_sha" != "$target" ] || return 0
  if ! git -C "$REPO" merge-base --is-ancestor "$local_sha" "$target" \
    && ! work_is_contained_in "$local_sha" "$target"; then
    log "$target does not contain everything on local $BASE; leaving the branch alone"
    return 1
  fi

  holder=$(worktree_holding_branch "$BASE")
  if [ -z "$holder" ]; then
    if git -C "$REPO" update-ref "refs/heads/$BASE" "$target" "$local_sha"; then
      log "moved $BASE to $target (not checked out anywhere)"
      return 0
    fi
    log "WARNING: could not move $BASE to $target"
    return 1
  fi

  if [ -n "$(git -C "$holder" status --porcelain)" ]; then
    log "$holder has uncommitted changes; leaving $BASE at $local_sha for now"
    return 1
  fi
  if git -C "$holder" reset --hard "$target" >/dev/null 2>&1; then
    log "moved $BASE to $target in $holder"
    return 0
  fi
  log "WARNING: could not move $BASE to $target in $holder"
  return 1
}

# Refuse to publish a commit that carries a build directory.
#
# One accidental `git add` of a target directory is gigabytes of objects, and a
# push cannot be taken back from the remote's history. Checked here rather than
# trusted to .gitignore, because the commit that did this locally had already
# slipped past it.
publish_tree_is_safe() {
  local sha="$1" path bad=""
  for path in $FORBIDDEN_PUBLISH_PATHS; do
    if git -C "$REPO" cat-file -e "$sha:$path" 2>/dev/null; then
      bad="$bad $path"
    fi
  done
  [ -n "$bad" ] || return 0
  log "WARNING: refusing to publish $sha; it contains build output:$bad"
  notify "Upstream merge would publish build output" \
"The commit to publish contains:$bad

That is build output, not source, so nothing was pushed. Remove those paths from
the history of $BASE (they are usually one bad 'git add' in a merge commit),
then this job resumes on its own.

Inspect: git -C $REPO log --oneline --all --$(printf '%s' "$bad" | awk '{print $1}')" "high"
  return 1
}

# The fork's owner/repo slug, derived from the fork remote URL.
fork_slug() {
  local url
  url=$(git remote get-url "$FORK_REMOTE" 2>/dev/null) || return 1
  printf '%s' "$url" | sed -E 's#^git@github\.com:##; s#^https://github\.com/##; s#\.git$##'
}

# Merge two commits into a new commit without a working tree.
#
# Printed on success, empty on conflict. Index-free so it is safe while the user
# is mid-edit: no checkout, no stash, no dirty-tree dance, and a user config of
# `merge.ff = only` cannot abort it the way `git merge` is aborted.
merge_commit_of() {
  local first="$1" second="$2" message="$3" tree
  tree=$(merged_tree_of "$first" "$second") || return 1
  git -C "$REPO" commit-tree "$tree" -p "$first" -p "$second" -m "$message" 2>/dev/null
}

# The tree two commits merge to, empty and nonzero when they conflict.
#
# `git merge-tree` still prints a tree for a conflicting merge, with the
# conflict markers written into the files, so its exit status is the only honest
# signal and is captured separately rather than through a pipeline.
merged_tree_of() {
  local out status
  out=$(git -C "$REPO" merge-tree --write-tree "$1" "$2" 2>/dev/null)
  status=$?
  [ "$status" -eq 0 ] || return 1
  printf '%s' "$out" | head -1
}

# Whether every change on `side` is already contained in `into`.
#
# Compares content, not ancestry, because a rewritten branch (commits squashed,
# reordered, or dropped while the code stays the same) has no ancestry relation
# left to compare. Patch ids are not enough: squashing several commits produces
# a combined patch matching none of the originals, so the most common rewrite of
# all would read as lost work.
work_is_contained_in() {
  local side="$1" into="$2" into_tree merged_tree
  git -C "$REPO" diff --quiet "$side" "$into" 2>/dev/null && return 0
  into_tree=$(git -C "$REPO" rev-parse "$into^{tree}" 2>/dev/null) || return 1
  merged_tree=$(merged_tree_of "$into" "$side") || return 1
  [ "$merged_tree" = "$into_tree" ]
}

# Reconcile a local base branch that has no ancestry relation to the fork's.
#
# Two very different situations produce the same SHA-level symptom, and treating
# them alike is what wedged this job before:
#
#   1. A history rewrite on GitHub re-encoded the same code. Local is then an
#      outdated encoding and is reset onto the remote.
#   2. Both sides genuinely gained commits, which is the normal outcome of the
#      user committing locally while a pull request merged on the fork. The
#      honest resolution is a merge commit, published like any other.
#
# Only conflicting content is left to the user, since nothing here can decide
# which side of a conflicting hunk is meant to win.
#
# Prints the commit the base branch should become, empty when it must not move.
reconcile_diverged_base() {
  local remote_sha="$1" local_sha="$2" merged
  log "$FORK_REMOTE/$BASE ($remote_sha) and local $BASE ($local_sha) have diverged; reconciling"

  if work_is_contained_in "$local_sha" "$remote_sha"; then
    log "every local change is already in $FORK_REMOTE/$BASE; adopting the remote"
    printf '%s' "$remote_sha"
    return 0
  fi

  merged=$(merge_commit_of "$local_sha" "$remote_sha" \
"Merge $FORK_REMOTE/$BASE into $BASE

Both sides gained commits: the fork's base branch moved on GitHub while this
clone committed work of its own. Merged by scripts/upstream_merge_agent.sh.")
  if [ -n "$merged" ]; then
    log "merged $FORK_REMOTE/$BASE into local $BASE as $merged"
    printf '%s' "$merged"
    return 0
  fi

  log "WARNING: local $BASE and $FORK_REMOTE/$BASE conflict; refusing to publish"
  notify "Fork $BASE conflicts with your local $BASE" \
"$FORK_REMOTE/$BASE and your local $BASE both changed the same lines, so they cannot be merged automatically.

Nothing was published, reset, or lost. Resolve it once and this job resumes on its own.

Inspect: git -C $REPO log --oneline $FORK_REMOTE/$BASE..$BASE
Resolve:  git -C $REPO merge --no-ff $FORK_REMOTE/$BASE" "high"
  return 1
}

# Publish a commit to the fork when it is ahead of the fork remote. Defaults to
# whatever local $BASE points at.
#
# The base branch is protected by a repository ruleset requiring a pull
# request, so this pushes the commits to a PR branch, opens a pull request, and
# merges it with the "merge" method (see PR_MERGE_METHOD).
#
# Only ever the fork: upstream is never a valid push target, since this job
# maintains a fork rather than contributing to the parent project.
publish_fork_if_ahead() {
  [ "$PUSH_FORK" = "1" ] || { log "publish disabled (JCODE_UPSTREAM_PUSH=$PUSH_FORK)"; return 0; }
  git remote get-url "$FORK_REMOTE" >/dev/null 2>&1 || return 0
  [ "$FORK_REMOTE" != "$UPSTREAM_REMOTE" ] || {
    log "fork remote equals upstream remote; not publishing"
    return 0
  }

  # What gets published is a commit, not a branch. The user's base branch may be
  # checked out, dirty, or behind, and none of that should stop the fork from
  # being updated; the branch is moved to match afterwards, if and when it can.
  local remote_sha local_sha publish_sha
  local_sha="${1:-}"
  [ -n "$local_sha" ] || local_sha=$(git -C "$REPO" rev-parse "$BASE" 2>/dev/null) || return 0
  remote_sha=$(git -C "$REPO" rev-parse "$FORK_REMOTE/$BASE" 2>/dev/null)
  publish_sha="$local_sha"

  if [ -n "$remote_sha" ]; then
    if [ "$remote_sha" = "$local_sha" ]; then
      log "fork $FORK_REMOTE/$BASE already matches local $BASE"
      return 0
    fi
    if git -C "$REPO" merge-base --is-ancestor "$local_sha" "$remote_sha"; then
      log "$FORK_REMOTE/$BASE is ahead of local $BASE (a pull request was merged); adopting it"
      adopt_base_ref "$remote_sha"
      return 0
    fi
    if ! git -C "$REPO" merge-base --is-ancestor "$remote_sha" "$local_sha"; then
      publish_sha=$(reconcile_diverged_base "$remote_sha" "$local_sha") || return 1
      if [ "$publish_sha" = "$remote_sha" ]; then
        log "nothing local to publish; adopting $FORK_REMOTE/$BASE"
        adopt_base_ref "$remote_sha"
        return 0
      fi
    fi
  fi

  publish_tree_is_safe "$publish_sha" || return 1

  ensure_fork_actions_disabled

  if ! command -v gh >/dev/null 2>&1; then
    log "WARNING: gh is required to open a pull request but is not installed"
    return 1
  fi
  # A locked login keyring, which is the normal state right after boot until the
  # desktop session unlocks it, leaves gh installed but unable to reach its
  # credentials. Checking before the push matters: otherwise the push succeeds,
  # every later gh call fails, and the branch is left on the fork with no pull
  # request behind it.
  local auth_out
  if ! auth_out=$(gh auth status 2>&1); then
    log "WARNING: gh is not authenticated; not opening a pull request"
    log "$(printf '%s' "$auth_out" | tail -3)"
    notify "Upstream merge needs GitHub auth" "The merge is committed locally, but gh could not authenticate, so nothing was published.

Unlock the login keyring (or run 'gh auth login'), then rerun the job.

Log: $LOG" "high"
    return 1
  fi
  local slug
  slug=$(fork_slug) || { log "WARNING: could not derive the fork slug"; return 1; }

  # Force-push is safe here and nowhere else: the pull request branch is owned
  # entirely by this job and is rebuilt from the fork's base on every run.
  #
  # The lease is stated explicitly from ls-remote rather than left to the
  # remote-tracking ref. The fork's fetch refspec need only cover the base
  # branch, and then no refs/remotes entry for the pull request branch ever
  # exists, so a bare --force-with-lease has nothing to compare and rejects the
  # push as "stale info" on every run after the one that created the branch.
  local pr_remote_sha lease
  pr_remote_sha=$(git ls-remote "$FORK_REMOTE" "refs/heads/$PR_BRANCH" 2>/dev/null | awk '{print $1}')
  if [ -n "$pr_remote_sha" ]; then
    lease="--force-with-lease=refs/heads/$PR_BRANCH:$pr_remote_sha"
  else
    # The branch does not exist yet, so there is nothing to clobber.
    lease="--force"
  fi
  if ! git -C "$REPO" push "$lease" "$FORK_REMOTE" "$publish_sha:refs/heads/$PR_BRANCH"; then
    log "WARNING: push of $PR_BRANCH to $FORK_REMOTE failed"
    return 1
  fi
  log "pushed $publish_sha to $FORK_REMOTE/$PR_BRANCH"

  # The pull request branch is rebuilt from the current base on every run, so an
  # open pull request nobody merged yet gains the newer upstream commits instead
  # of a second pull request appearing beside it. Updates accumulate in one pull
  # request until it is merged.
  local pr body
  body="Automated upstream merge from \`$UPSTREAM_REF\` ($UP_SHA).

**Merge this, do not squash.** The fork is meant to sit on top of upstream, which
requires upstream's commits to stay genuine ancestors of \`$BASE\`. A squash would
replace them with one new commit containing the same code, leaving upstream SHAs
unreachable and every later run re-merging the same history.

This is normally merged by the job itself the moment it is opened. If you are
reading it, that merge did not go through: later runs force-push the refreshed
merge onto \`$PR_BRANCH\` and retry, so further upstream updates accumulate here
rather than being lost.

Last updated $(date -u +%Y-%m-%dT%H:%M:%SZ) by scripts/upstream_merge_agent.sh."

  pr=$(gh pr list --repo "$slug" --head "$PR_BRANCH" --base "$BASE" --state open \
    --json number --jq '.[0].number' 2>/dev/null)
  if [ -z "$pr" ] || [ "$pr" = "null" ]; then
    # Capture the exit status rather than piping to `tail`, whose own status
    # would hide the failure and report a nonexistent pull request as opened.
    local create_out create_status
    create_out=$(gh pr create --repo "$slug" --head "$PR_BRANCH" --base "$BASE" \
      --title "Merge upstream into $BASE" \
      --body "$body" 2>&1)
    create_status=$?
    if [ "$create_status" -ne 0 ]; then
      log "WARNING: could not open a pull request for $PR_BRANCH"
      log "$(printf '%s' "$create_out" | tail -3)"
      notify "Upstream merge could not open a pull request" "$PR_BRANCH is pushed to the fork, but 'gh pr create' failed, so no pull request exists for it.

$(printf '%s' "$create_out" | tail -3)

Log: $LOG" "high"
      return 1
    fi
    pr=$(printf '%s' "$create_out" | tail -1)
    log "opened pull request: $pr"
    pr=$(printf '%s' "$pr" | sed -E 's#.*/pull/([0-9]+).*#\1#')
  else
    log "reusing open pull request #$pr; it now carries upstream $UP_SHA"
    # The force-push above already updated the diff. This only keeps the
    # description from still naming an older upstream commit.
    gh pr edit "$pr" --repo "$slug" --body "$body" >/dev/null 2>&1 \
      || log "WARNING: could not refresh the body of pull request #$pr"
  fi
  case "$pr" in
    ''|*[!0-9]*) log "WARNING: could not determine the pull request number"; return 1 ;;
  esac

  if [ "$AUTO_MERGE_PR" != "1" ]; then
    log "pull request #$pr left open for review (JCODE_UPSTREAM_AUTO_MERGE=$AUTO_MERGE_PR)"
    notify "Upstream merge pull request ready" "Pull request #$pr on $slug carries upstream $UP_SHA and is waiting for you.

https://github.com/$slug/pull/$pr" "default"
    return 0
  fi

  # --merge, not --squash: see PR_MERGE_METHOD. Output is captured rather
  # than piped, because a pipeline's status is the last command's and would
  # report every failed merge as a success.
  local merge_out
  if merge_out=$(gh pr merge "$pr" --repo "$slug" "--$PR_MERGE_METHOD" 2>&1); then
    log "merged pull request #$pr into $BASE"
  else
    # Leave it open rather than escalating to a hard failure. The branch is the
    # accumulation point: the next run rebuilds it with the newer upstream and
    # retries the merge on this same pull request, so a transient red check or a
    # missing credential costs a cycle instead of an update.
    log "WARNING: could not merge pull request #$pr; leaving it open"
    log "$(printf '%s' "$merge_out" | tail -3)"
    notify "Upstream merge pull request could not be merged" "Pull request #$pr on $slug carries upstream $UP_SHA but could not be merged automatically.

$(printf '%s' "$merge_out" | tail -3)

Later runs keep adding upstream to it, so nothing is lost. Merge it by hand, or fix what blocks it.

https://github.com/$slug/pull/$pr" "high"
    return 1
  fi

  # The merge commit GitHub created is not in the local repo yet, so local $BASE
  # is behind by exactly that commit. Adopting it keeps the next run's "is the
  # fork ahead" question honest instead of re-publishing forever.
  git -C "$REPO" fetch --prune "$FORK_REMOTE" >/dev/null 2>&1 || true
  local new_remote_sha
  new_remote_sha=$(git -C "$REPO" rev-parse "$FORK_REMOTE/$BASE" 2>/dev/null)
  [ -n "$new_remote_sha" ] && adopt_base_ref "$new_remote_sha"
  return 0
}

# The commit the upstream merge should be built on: local $BASE plus whatever
# the fork's base branch gained on GitHub.
#
# That remote-only work is normally this job's own pull request being merged.
# Building the upstream merge on a stale local base instead would diverge from
# the fork for no reason and make the publish step re-reconcile every run.
#
# Printed rather than checked out. Nothing here touches the branch or any
# working tree, so an active edit session cannot stall the job, and a user
# config of `merge.ff = only` cannot abort the merge.
starting_commit() {
  local remote_sha local_sha
  local_sha=$(git -C "$REPO" rev-parse "$BASE" 2>/dev/null) || return 1
  if ! git -C "$REPO" remote get-url "$FORK_REMOTE" >/dev/null 2>&1 \
    || [ "$FORK_REMOTE" = "$UPSTREAM_REMOTE" ]; then
    printf '%s' "$local_sha"
    return 0
  fi

  remote_sha=$(git -C "$REPO" rev-parse "$FORK_REMOTE/$BASE" 2>/dev/null)
  if [ -z "$remote_sha" ] || [ "$remote_sha" = "$local_sha" ] \
    || git -C "$REPO" merge-base --is-ancestor "$remote_sha" "$local_sha"; then
    printf '%s' "$local_sha"
    return 0
  fi

  if git -C "$REPO" merge-base --is-ancestor "$local_sha" "$remote_sha"; then
    log "local $BASE is behind $FORK_REMOTE/$BASE ($remote_sha); building on the remote"
    adopt_base_ref "$remote_sha"
    printf '%s' "$remote_sha"
    return 0
  fi

  reconcile_diverged_base "$remote_sha" "$local_sha"
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

BASE_SHA=$(starting_commit)
if [ -z "$BASE_SHA" ]; then
  log "cannot determine a base to merge onto; stopping"
  exit 1
fi
UP_SHA=$(git rev-parse "$UPSTREAM_REF") || exit 1

if git merge-base --is-ancestor "$UP_SHA" "$BASE_SHA"; then
  log "already up to date with $UPSTREAM_REF ($UP_SHA)"
  # The local branch can still be ahead of the fork remote (local commits, or a
  # merge adopted by a previous run). Publishing that is the whole point of
  # "keep my fork updated", so it must not be skipped just because upstream
  # brought nothing new.
  publish_fork_if_ahead
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
EXISTING=$(worktree_holding_branch "$BRANCH")
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
    # A verified merge still has to be adopted and published, exactly like one
    # the agent produced. Exiting here instead left the most common case of all,
    # an upstream change that merges cleanly, sitting in the worktree forever
    # while the fork was never updated.
    log "clean merge verified on branch $BRANCH in $WORKTREE"
    STATUS="merged"
    DETAIL="Upstream merged cleanly with no conflicts. '$CHECK_CMD' passes."
    MECHANICAL_MERGE=1
  else
    REASON="merge was clean but '$CHECK_CMD' failed"
  fi
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
        MECHANICAL_MERGE=1
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

# Set when git itself produced a verified merge, either cleanly or by replaying
# rerere resolutions. No agent is needed in that case.
MECHANICAL_MERGE="${MECHANICAL_MERGE:-0}"

# Everything from here to the verdict is the agent path, which a merge git
# already completed and verified skips entirely.
if [ "$MECHANICAL_MERGE" = "0" ]; then
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

## Read this first
'docs/UPSTREAM_DIVERGENCE.md' in the worktree records how this fork diverges:
which files replace upstream behaviour (fork side wins), which upstream files
the fork deleted as orphans (keep them deleted), which files diverge through
formatting only (always take upstream), and the resolutions already agreed in
earlier merges. Read it before resolving anything; it will answer most of the
conflicts below directly.

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
   The ratchet scripts in that command are baselines, not correctness checks.
   If one fails purely because upstream's own code is bigger, or uses more
   .unwrap()/.expect(), that growth is not yours to fix: re-baseline it with
   'python3 scripts/<script>.py --update', commit the JSON with the merge, and
   say so in the commit message. Only investigate when the growth comes from a
   conflict YOU resolved.
   'cargo fmt --all -- --check' in that command is mechanical: if it fails,
   just run 'cargo fmt --all' and include the result in the merge commit.
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
    # Publish the merge commit itself, then move $BASE to it if that is safe.
    # The fork is what this job exists to keep updated, and holding the merge
    # back because the user happens to be mid-edit is what left it sitting in a
    # worktree while the fork fell further behind.
    cd "$REPO" || exit 1
    MERGE_SHA=$(git rev-parse "$BRANCH") || exit 1
    PUBLISHED=""
    publish_fork_if_ahead "$MERGE_SHA" && PUBLISHED="yes"
    ADOPTED=""
    adopt_base_ref "$(git rev-parse "$FORK_REMOTE/$BASE" 2>/dev/null || echo "$MERGE_SHA")" \
      && ADOPTED="yes"

    if [ -n "$PUBLISHED" ] && [ -n "$ADOPTED" ]; then
      notify "Fork updated with upstream" "$DETAIL

Adopted onto $BASE and pushed to $FORK_REMOTE." "default"
    elif [ -n "$PUBLISHED" ]; then
      notify "Fork updated with upstream" "$DETAIL

Pushed to $FORK_REMOTE. Your local $BASE was left where it is (uncommitted work or a conflicting edit), and a later run adopts it once the tree settles." "default"
    else
      notify "Upstream merged, needs publishing" "$DETAIL

The merge is committed on $BRANCH but could not be published (see log).
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
