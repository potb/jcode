#!/usr/bin/env bash
# Harness test for reconcile_diverged_base: what happens when the fork's base
# branch on GitHub has no ancestry relation to the local one.
#
# Real git repos, no network. The cases that matter:
#   1. squashed/reordered but content preserved -> adopt the rewrite
#   2. squashed and then advanced               -> adopt the rewrite
#   3. both sides gained real work              -> merge them, lose nothing
#   4. conflicting content                      -> refuse, notify, touch nothing
#   5. uncommitted work in the checkout          -> reconcile anyway
set -uo pipefail

# Resolve before any cd: the rest of the test runs inside temp repos.
SCRIPT="$(cd "$(dirname "$0")/.." && pwd)/upstream_merge_agent.sh"

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

BASE=master FORK_REMOTE=origin
NOTIFIED=""
log() { echo "[log] $*" >&2; }
notify() { NOTIFIED="$1"; echo "[notify] $1" >&2; }
eval "$(awk '/^merged_tree_of\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^merge_commit_of\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^work_is_contained_in\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^reconcile_diverged_base\(\) \{/,/^}/' "$SCRIPT")"

fail() { echo "FAIL: $*"; exit 1; }

# Build a repo whose local master has two commits and whose origin/master is a
# rewritten or independently advanced version of the same history.
setup() {
  local name="$1" style="$2"
  REPO="$TMP/$name"
  git init -q -b master "$REPO"
  cd "$REPO"
  git config user.email t@t; git config user.name t
  # The failure this whole rewrite is about: with merge.ff=only, `git merge` of
  # a diverged branch aborts outright, so nothing here may depend on it.
  git config merge.ff only
  echo base > base.txt; git add .; git commit -qm base
  local root; root=$(git rev-parse HEAD)

  echo a > a.txt; git add .; git commit -qm "add a"
  echo b > b.txt; git add .; git commit -qm "add b"

  # The rewritten remote branch, built on a detached head then stored as a
  # remote-tracking ref: no second repo or network needed.
  git checkout -q --detach "$root"
  case "$style" in
    squashed)  # both commits' work, as one commit
      echo a > a.txt; echo b > b.txt; git add .; git commit -qm "add a and b" ;;
    reordered) # same commits, opposite order
      echo b > b.txt; git add .; git commit -qm "add b"
      echo a > a.txt; git add .; git commit -qm "add a" ;;
    diverged)  # both sides carry work the other lacks
      echo remote > remote.txt; git add .; git commit -qm "add remote work" ;;
    conflicting) # both sides changed the same line differently
      echo remote > a.txt; git add .; git commit -qm "conflicting a" ;;
    squashed_plus) # squashed, then the remote moved on
      echo a > a.txt; echo b > b.txt; git add .; git commit -qm "add a and b"
      echo c > c.txt; git add .; git commit -qm "add c" ;;
  esac
  git update-ref refs/remotes/origin/master HEAD
  git checkout -q master
  REMOTE_SHA=$(git rev-parse origin/master)
  LOCAL_SHA=$(git rev-parse master)
}

# --- 1. squashed: content preserved, so the rewrite is adopted ---------------
setup squashed squashed
NOTIFIED=""
OUT=$(reconcile_diverged_base "$REMOTE_SHA" "$LOCAL_SHA") || fail "squash rewrite should reconcile"
[ "$OUT" = "$REMOTE_SHA" ] || fail "a squashed rewrite should resolve to the remote"
[ -z "$NOTIFIED" ] || fail "a recoverable rewrite must not notify"

# --- 2. reordered: same patches, different order -----------------------------
setup reordered reordered
OUT=$(reconcile_diverged_base "$REMOTE_SHA" "$LOCAL_SHA") || fail "reordered rewrite should reconcile"
[ "$OUT" = "$REMOTE_SHA" ] || fail "a reordered rewrite should resolve to the remote"

# --- 3. squashed and then advanced: trees differ, content still contained ----
setup squashed_plus squashed_plus
NOTIFIED=""
OUT=$(reconcile_diverged_base "$REMOTE_SHA" "$LOCAL_SHA") \
  || fail "a squashed rewrite that also advanced should reconcile"
[ "$OUT" = "$REMOTE_SHA" ] || fail "an advanced rewrite should resolve to the remote"
[ -z "$NOTIFIED" ] || fail "a recoverable rewrite must not notify"

# --- 4. genuine divergence: merge both sides, keep everything ----------------
# The everyday case: the user commits locally while a pull request merges on the
# fork. Refusing here is what wedged the job and notified on every run.
setup diverged diverged
NOTIFIED=""
OUT=$(reconcile_diverged_base "$REMOTE_SHA" "$LOCAL_SHA") || fail "divergence should be merged"
[ "$OUT" != "$REMOTE_SHA" ] || fail "a merge must not discard the local side"
git merge-base --is-ancestor "$LOCAL_SHA" "$OUT" || fail "local work is not in the merge"
git merge-base --is-ancestor "$REMOTE_SHA" "$OUT" || fail "remote work is not in the merge"
for f in a.txt b.txt remote.txt; do
  git cat-file -e "$OUT:$f" 2>/dev/null || fail "$f is missing from the merge"
done
[ -z "$NOTIFIED" ] || fail "an ordinary divergence must not notify"
[ "$(git rev-parse master)" = "$LOCAL_SHA" ] || fail "reconciling must not move the branch"

# --- 5. conflicting content: refuse, notify, leave everything alone ----------
setup conflicting conflicting
NOTIFIED=""
if reconcile_diverged_base "$REMOTE_SHA" "$LOCAL_SHA" >/dev/null; then
  fail "a conflicting divergence must not be resolved automatically"
fi
[ "$(git rev-parse master)" = "$LOCAL_SHA" ] || fail "master must not move on a conflict"
[ -n "$NOTIFIED" ] || fail "a conflict the user must settle has to notify"
[ -f a.txt ] || fail "local work was destroyed"

# --- 6. uncommitted work never blocks the reconcile --------------------------
# Nothing here touches a working tree, so an active edit session is irrelevant.
setup dirty diverged
echo scratch > uncommitted.txt
NOTIFIED=""
OUT=$(reconcile_diverged_base "$REMOTE_SHA" "$LOCAL_SHA") \
  || fail "uncommitted work must not block reconciling"
git merge-base --is-ancestor "$LOCAL_SHA" "$OUT" || fail "local work is not in the merge"
[ -f uncommitted.txt ] || fail "uncommitted work was destroyed"
[ "$(cat uncommitted.txt)" = scratch ] || fail "uncommitted work was modified"

echo "PASS"
