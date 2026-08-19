#!/usr/bin/env bash
# Harness test for publish_fork_if_ahead: real git repos, stubbed `gh`.
#
# Verifies the pull-request publish path end to end without touching GitHub:
# the PR branch is pushed, a PR is opened and merged, and the local base branch
# is adopted onto the result.
#
# The default method must be a real merge: the fork is meant to sit on top of
# upstream, so upstream's commits have to stay genuine ancestors of the base
# branch, which only a two-parent merge commit provides. The opt-in squash path
# is covered too, since it leaves the local base ancestry-divergent.
set -uo pipefail

# Resolve before any cd: the rest of the test runs inside a temp repo.
SCRIPT="$(cd "$(dirname "$0")/.." && pwd)/upstream_merge_agent.sh"

# Commit identity for the stub `gh` merge, which runs outside any work tree.
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
export HOME="$TMP/home"
mkdir -p "$HOME/bin"

# --- stub gh: records its invocations, performs the merge on the bare remote --
cat > "$HOME/bin/gh" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$GH_CALLS"
case "$1 $2" in
  "pr list") echo "" ;;
  "pr create") echo "https://github.com/o/r/pull/7" ;;
  "pr merge")
    # A bare repo cannot `git merge`, so build the commit directly. Both shapes
    # are exactly what GitHub produces: a two-parent merge keeping both sides'
    # SHAs, or a single-parent squash sharing no ancestry with the PR branch.
    base=$(git --git-dir="$REMOTE" rev-parse master)
    head=$(git --git-dir="$REMOTE" rev-parse auto/upstream-merge-pr)
    tree=$(git --git-dir="$REMOTE" rev-parse "$head^{tree}")
    parents="-p $base -p $head"
    for a in "$@"; do
      case "$a" in --squash) parents="-p $base" ;; esac
    done
    # shellcheck disable=SC2086
    new=$(git --git-dir="$REMOTE" commit-tree "$tree" $parents \
      -m "Merge upstream into master (#7)") || { echo "merge failed" >&2; exit 1; }
    git --git-dir="$REMOTE" update-ref refs/heads/master "$new"
    ;;
  "api "*) ;;
esac
exit 0
STUB
chmod +x "$HOME/bin/gh"
PATH="$HOME/bin:$PATH"

export REMOTE="$TMP/remote.git" GH_CALLS="$TMP/gh-calls"
: > "$GH_CALLS"

# --- a bare "fork" plus a clone that is one commit ahead of it ---------------
git init -q --bare -b master "$REMOTE"
REPO="$TMP/repo"
git clone -q "$REMOTE" "$REPO" 2>/dev/null
cd "$REPO"
git config user.email t@t; git config user.name t
# The whole job used to break under this: `git merge` refuses a diverged branch
# outright, so no publish path may depend on one.
git config merge.ff only
echo one > f; git add f; git commit -qm one
git push -q origin master
git remote add upstream "$TMP/upstream-placeholder"
echo two >> f; git commit -qam two
AHEAD=$(git rev-parse master)

# --- load the script's functions without running it --------------------------
FORK_REMOTE=origin UPSTREAM_REMOTE=upstream BASE=master
PUSH_FORK=1 PR_MERGE_METHOD="${METHOD:-merge}" PR_BRANCH=auto/upstream-merge-pr
REPO="$REPO"
UPSTREAM_REF=upstream/master UP_SHA=deadbeef ENFORCE_ACTIONS_OFF=0 AUTO_MERGE_PR=1
FORBIDDEN_PUBLISH_PATHS="target target-base"
NOTIFIED=""
log() { echo "[log] $*" >&2; }
notify() { NOTIFIED="$1"; echo "[notify] $1" >&2; }
ensure_fork_actions_disabled() { :; }
eval "$(awk '/^fork_slug\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^publish_fork_if_ahead\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^worktree_holding_branch\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^adopt_base_ref\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^publish_tree_is_safe\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^merged_tree_of\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^merge_commit_of\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^work_is_contained_in\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^reconcile_diverged_base\(\) \{/,/^}/' "$SCRIPT")"

fail() { echo "FAIL: $*"; exit 1; }

publish_fork_if_ahead || fail "publish returned nonzero"

grep -q "pr create" "$GH_CALLS" || fail "no pull request was created"
grep -q -- "--merge" "$GH_CALLS" || fail "pull request was not merged with --merge"
grep -q -- "--squash" "$GH_CALLS" && fail "the default publish must not squash"

# The invariant the whole job exists for: what was published is reachable from
# the fork's base branch, not merely copied into it.
git --git-dir="$REMOTE" merge-base --is-ancestor "$AHEAD" master \
  || fail "the published commits are not ancestors of the fork's base branch"
[ "$(git --git-dir="$REMOTE" rev-list --parents -n1 master | wc -w)" = 3 ] \
  || fail "the fork's base did not gain a two-parent merge commit"

git --git-dir="$REMOTE" rev-parse auto/upstream-merge-pr >/dev/null 2>&1 \
  || fail "PR branch was not pushed to the fork"
[ "$(git --git-dir="$REMOTE" rev-parse "master^{tree}")" = "$(git -C "$REPO" rev-parse "$AHEAD^{tree}")" ] \
  || fail "the fork's base does not contain the published work"
[ "$(git -C "$REPO" rev-parse master)" = "$(git --git-dir="$REMOTE" rev-parse master)" ] \
  || fail "local base was not adopted onto the fork's merge commit"

# A second run has nothing to publish and must stay silent.
: > "$GH_CALLS"
publish_fork_if_ahead || fail "idempotent run returned nonzero"
[ -s "$GH_CALLS" ] && fail "an up-to-date fork must not open a pull request"

# --- the opt-in squash path still publishes and still adopts ----------------
# A squash shares no ancestry with what was pushed, so adopting it can only work
# through the rewrite path. Verified here so the escape hatch is not silently
# broken by the merge-by-default decision.
: > "$GH_CALLS"
echo three >> f; git -C "$REPO" commit -qam three
SQUASH_AHEAD=$(git -C "$REPO" rev-parse master)
METHOD=squash PR_MERGE_METHOD=squash publish_fork_if_ahead || fail "squash publish returned nonzero"

grep -q -- "--squash" "$GH_CALLS" || fail "the squash opt-in did not squash"
[ "$(git --git-dir="$REMOTE" rev-list --parents -n1 master | wc -w)" = 2 ] \
  || fail "a squash must produce a single-parent commit"
git --git-dir="$REMOTE" merge-base --is-ancestor "$SQUASH_AHEAD" master \
  && fail "a squash cannot keep the pushed commits as ancestors"
[ "$(git -C "$REPO" rev-parse master)" = "$(git --git-dir="$REMOTE" rev-parse master)" ] \
  || fail "local base was not adopted onto the squash commit"

# --- the fork is updated even while the user is mid-edit --------------------
# The job runs on a schedule and the user is editing whenever they are editing.
# Publishing has to happen anyway; only the local branch waits.
: > "$GH_CALLS"
PR_MERGE_METHOD=merge
echo four >> f; git -C "$REPO" commit -qam four
DIRTY_AHEAD=$(git -C "$REPO" rev-parse master)
echo scratch > "$REPO/uncommitted.txt"
publish_fork_if_ahead || fail "an edit session must not block publishing"
grep -q "pr create" "$GH_CALLS" || fail "no pull request while the tree was dirty"
git --git-dir="$REMOTE" merge-base --is-ancestor "$DIRTY_AHEAD" master \
  || fail "the work was not published while the tree was dirty"
[ "$(git -C "$REPO" rev-parse master)" = "$DIRTY_AHEAD" ] \
  || fail "the base branch moved under an active edit session"
[ "$(cat "$REPO/uncommitted.txt")" = scratch ] || fail "uncommitted work was touched"
rm -f "$REPO/uncommitted.txt"

# The next clean run adopts what was published, so nothing is left dangling.
publish_fork_if_ahead || fail "adopting after the edit session returned nonzero"
[ "$(git -C "$REPO" rev-parse master)" = "$(git --git-dir="$REMOTE" rev-parse master)" ] \
  || fail "the published merge was never adopted once the tree was clean"

# --- both sides gained commits: merge and publish, do not stall -------------
# The exact shape that used to notify "local work would be lost" on every run.
: > "$GH_CALLS"; NOTIFIED=""
git --git-dir="$REMOTE" symbolic-ref HEAD refs/heads/master
REMOTE_ONLY=$(mktemp -d)
git clone -q "$REMOTE" "$REMOTE_ONLY/c"
( cd "$REMOTE_ONLY/c" && git config user.email t@t && git config user.name t \
  && echo remote-side > remote.txt && git add remote.txt \
  && git commit -qm "remote work" && git push -q origin master )
git -C "$REPO" fetch -q origin
echo local-side > "$REPO/local.txt"
git -C "$REPO" add local.txt; git -C "$REPO" commit -qm "local work"
LOCAL_ONLY=$(git -C "$REPO" rev-parse master)
REMOTE_ONLY_SHA=$(git -C "$REPO" rev-parse origin/master)
publish_fork_if_ahead || fail "a diverged base must still publish"
[ -z "$NOTIFIED" ] || fail "an ordinary divergence must not notify the user"
git --git-dir="$REMOTE" merge-base --is-ancestor "$LOCAL_ONLY" master \
  || fail "local-only work was dropped by the divergence merge"
git --git-dir="$REMOTE" merge-base --is-ancestor "$REMOTE_ONLY_SHA" master \
  || fail "remote-only work was dropped by the divergence merge"
rm -rf "$REMOTE_ONLY"

# --- build output is never published ----------------------------------------
# One bad `git add` of a target directory is gigabytes of objects, and a push
# cannot be undone on the remote.
: > "$GH_CALLS"; NOTIFIED=""
mkdir -p "$REPO/target-base/debug"
echo binary > "$REPO/target-base/debug/artifact"
git -C "$REPO" add -f target-base
git -C "$REPO" commit -qm "accidental build output"
if publish_fork_if_ahead; then
  fail "a commit carrying build output must not be published"
fi
[ -s "$GH_CALLS" ] && fail "nothing may be pushed when build output is present"
[ -n "$NOTIFIED" ] || fail "refusing to publish build output has to notify"
git -C "$REPO" reset -q --hard HEAD~1
rm -rf "$REPO/target-base"

echo "PASS"
