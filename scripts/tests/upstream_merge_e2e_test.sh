#!/usr/bin/env bash
# End-to-end test of the whole upstream_merge_agent.sh run, not just its
# functions: real git repos, a stubbed `gh` and a stubbed `jcode`, no network.
#
# The scenario is the one the job exists for and the one it used to fail:
# upstream has new commits, the fork's base branch moved on GitHub, the local
# clone has committed work of its own, and the user is mid-edit with uncommitted
# changes the whole time.
set -uo pipefail

SCRIPT="$(cd "$(dirname "$0")/.." && pwd)/upstream_merge_agent.sh"

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
export HOME="$TMP/home"
mkdir -p "$HOME/bin"

fail() { echo "FAIL: $*"; exit 1; }

export FORK="$TMP/fork.git" UPSTREAM="$TMP/upstream.git"
export GH_CALLS="$TMP/gh-calls" NOTIFICATIONS="$TMP/notifications"
: > "$GH_CALLS"; : > "$NOTIFICATIONS"

cat > "$HOME/bin/gh" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$GH_CALLS"
case "$1 $2" in
  "pr list") echo "" ;;
  "pr create") echo "https://github.com/o/r/pull/1" ;;
  "pr merge")
    base=$(git --git-dir="$FORK" rev-parse master)
    head=$(git --git-dir="$FORK" rev-parse auto/upstream-merge-pr)
    tree=$(git --git-dir="$FORK" rev-parse "$head^{tree}")
    new=$(git --git-dir="$FORK" commit-tree "$tree" -p "$base" -p "$head" \
      -m "Merge upstream into master (#1)") || exit 1
    git --git-dir="$FORK" update-ref refs/heads/master "$new"
    ;;
esac
exit 0
STUB

# The agent path must never be reached here: every merge in this test is clean,
# so a `jcode run` invocation means the mechanical path silently broke.
cat > "$HOME/bin/jcode" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  notify) shift; echo "$*" >> "$NOTIFICATIONS" ;;
  run) echo "UNEXPECTED AGENT INVOCATION" >> "$NOTIFICATIONS"; exit 1 ;;
esac
exit 0
STUB
chmod +x "$HOME/bin/gh" "$HOME/bin/jcode"
export PATH="$HOME/bin:$PATH"

# --- upstream, a fork of it, and a clone of the fork -------------------------
git init -q --bare -b master "$UPSTREAM"
SEED="$TMP/seed"
git init -q -b master "$SEED"
( cd "$SEED" && git config user.email t@t && git config user.name t \
  && echo seed > shared.txt && git add . && git commit -qm seed \
  && git push -q "$UPSTREAM" master )
git clone -q --bare "$UPSTREAM" "$FORK"

REPO="$TMP/repo"
git clone -q "$FORK" "$REPO"
cd "$REPO"
git config user.email t@t; git config user.name t
# The config that broke every merge the old script attempted.
git config merge.ff only
git remote add upstream "$UPSTREAM"
git fetch -q upstream

# Upstream ships a change, in a file nobody else touches.
( cd "$SEED" && echo feature > upstream_feature.txt && git add . \
  && git commit -qm "upstream: add a feature" && git push -q "$UPSTREAM" master )

# The fork's base branch moves on GitHub, as when a pull request is merged.
FORK_CLONE="$TMP/forkclone"
git clone -q "$FORK" "$FORK_CLONE"
( cd "$FORK_CLONE" && git config user.email t@t && git config user.name t \
  && echo fork > fork_only.txt && git add . && git commit -qm "fork: merged pull request" \
  && git push -q origin master )

# The local clone commits work of its own, and the user is mid-edit.
echo local > local_only.txt
git add local_only.txt; git commit -qm "local: work in progress"
LOCAL_ONLY=$(git rev-parse master)
echo scratch > uncommitted.txt

# --- run the real script, start to finish -----------------------------------
JCODE_BIN="$HOME/bin/jcode" \
JCODE_UPSTREAM_REPO="$REPO" \
JCODE_UPSTREAM_STATE_DIR="$TMP/state" \
JCODE_UPSTREAM_WORKTREE="$TMP/state/worktree" \
JCODE_UPSTREAM_CHECK_CMD="true" \
JCODE_UPSTREAM_DISABLE_ACTIONS=0 \
  bash "$SCRIPT" > "$TMP/run.log" 2>&1
STATUS=$?

grep -q "UNEXPECTED AGENT INVOCATION" "$NOTIFICATIONS" \
  && fail "a clean merge must not invoke the agent"
[ "$STATUS" -eq 0 ] || { sed -n '1,60p' "$TMP/run.log"; fail "the run exited $STATUS"; }

# Everything from all three sides is on the fork, and nothing was lost.
for f in upstream_feature.txt fork_only.txt local_only.txt; do
  git --git-dir="$FORK" cat-file -e "master:$f" 2>/dev/null \
    || fail "$f never reached the fork"
done
git --git-dir="$FORK" merge-base --is-ancestor "$LOCAL_ONLY" master \
  || fail "local commits are not ancestors of the fork's base branch"

# The user's edit session is untouched, and their branch was not moved over it.
[ "$(cat "$REPO/uncommitted.txt")" = scratch ] || fail "uncommitted work was destroyed"
[ "$(git -C "$REPO" rev-parse master)" = "$LOCAL_ONLY" ] \
  || fail "the base branch moved while the tree was dirty"

# No alarm was raised: high priority is reserved for what a human must fix, and
# nothing in this run qualifies.
grep -q -- "--priority high" "$NOTIFICATIONS" \
  && { cat "$NOTIFICATIONS"; fail "an ordinary run must not raise an alarm"; }

# --- once the edit session ends, the local branch catches up -----------------
rm -f "$REPO/uncommitted.txt"
JCODE_BIN="$HOME/bin/jcode" \
JCODE_UPSTREAM_REPO="$REPO" \
JCODE_UPSTREAM_STATE_DIR="$TMP/state" \
JCODE_UPSTREAM_WORKTREE="$TMP/state/worktree" \
JCODE_UPSTREAM_CHECK_CMD="true" \
JCODE_UPSTREAM_DISABLE_ACTIONS=0 \
  bash "$SCRIPT" > "$TMP/run2.log" 2>&1
[ $? -eq 0 ] || { sed -n '1,60p' "$TMP/run2.log"; fail "the second run failed"; }

[ "$(git -C "$REPO" rev-parse master)" = "$(git --git-dir="$FORK" rev-parse master)" ] \
  || fail "the local base never caught up with the fork once the tree was clean"

# --- a third run has nothing to do and must stay silent ----------------------
: > "$GH_CALLS"
JCODE_BIN="$HOME/bin/jcode" \
JCODE_UPSTREAM_REPO="$REPO" \
JCODE_UPSTREAM_STATE_DIR="$TMP/state" \
JCODE_UPSTREAM_WORKTREE="$TMP/state/worktree" \
JCODE_UPSTREAM_CHECK_CMD="true" \
JCODE_UPSTREAM_DISABLE_ACTIONS=0 \
  bash "$SCRIPT" > "$TMP/run3.log" 2>&1
[ $? -eq 0 ] || { sed -n '1,60p' "$TMP/run3.log"; fail "the idempotent run failed"; }
grep -q "pr create" "$GH_CALLS" && fail "an up-to-date fork must not open a pull request"

echo "PASS"
