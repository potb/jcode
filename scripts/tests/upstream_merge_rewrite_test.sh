#!/usr/bin/env bash
# Harness test for reconcile_rewritten_base: what happens when the fork's base
# branch is rewritten on GitHub (commits squashed, reordered, or dropped).
#
# Real git repos, no network. The three cases that matter:
#   1. squashed/reordered but content preserved -> adopt the rewrite
#   2. identical tree by any other route        -> adopt the rewrite
#   3. a local commit's work was dropped        -> refuse, notify, touch nothing
set -uo pipefail

# Resolve before any cd: the rest of the test runs inside temp repos.
SCRIPT="$(cd "$(dirname "$0")/.." && pwd)/upstream_merge_agent.sh"

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

BASE=master FORK_REMOTE=origin
NOTIFIED=""
log() { echo "[log] $*"; }
notify() { NOTIFIED="$1"; echo "[notify] $1"; }
eval "$(awk '/^reconcile_rewritten_base\(\) \{/,/^}/' "$SCRIPT")"

fail() { echo "FAIL: $*"; exit 1; }

# Build a repo whose local master has two commits and whose origin/master is a
# rewritten version of the same history, per the requested style.
setup() {
  local name="$1" style="$2"
  REPO="$TMP/$name"
  git init -q -b master "$REPO"
  cd "$REPO"
  git config user.email t@t; git config user.name t
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
    dropped)   # "add a" was thrown away by the rewrite
      echo b > b.txt; git add .; git commit -qm "add b" ;;
    squashed_plus) # squashed, then the remote moved on: trees differ, so this
                   # is the case that actually exercises the patch-id check
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
reconcile_rewritten_base "$REMOTE_SHA" "$LOCAL_SHA" || fail "squash rewrite should be adopted"
[ "$(git rev-parse master)" = "$REMOTE_SHA" ] || fail "master was not reset onto the squashed rewrite"
[ -z "$NOTIFIED" ] || fail "a recoverable rewrite must not notify"

# --- 2. reordered: same patches, different order -----------------------------
setup reordered reordered
NOTIFIED=""
reconcile_rewritten_base "$REMOTE_SHA" "$LOCAL_SHA" || fail "reordered rewrite should be adopted"
[ "$(git rev-parse master)" = "$REMOTE_SHA" ] || fail "master was not reset onto the reordered rewrite"

# --- 3. dropped work: refuse, notify, leave everything alone -----------------
setup dropped dropped
NOTIFIED=""
if reconcile_rewritten_base "$REMOTE_SHA" "$LOCAL_SHA"; then
  fail "a rewrite that drops local work must not be adopted"
fi
[ "$(git rev-parse master)" = "$LOCAL_SHA" ] || fail "master must not move when work would be lost"
[ -n "$NOTIFIED" ] || fail "losing local work must notify the user"
[ -f a.txt ] || fail "the dropped commit's file must still exist locally"

# --- 4. a dirty tree is never reset, even for a recoverable rewrite ----------
setup dirty squashed
echo scratch > uncommitted.txt
NOTIFIED=""
if reconcile_rewritten_base "$REMOTE_SHA" "$LOCAL_SHA"; then
  fail "a dirty working tree must block the reset"
fi
[ "$(git rev-parse master)" = "$LOCAL_SHA" ] || fail "master moved despite uncommitted changes"
[ -f uncommitted.txt ] || fail "uncommitted work was destroyed"

# --- 5. squashed and then advanced: trees differ, patch ids still all match --
# Without the patch-id check this case looks identical to case 3 and would be
# refused forever, which is the wedge this whole function exists to prevent.
setup squashed_plus squashed_plus
NOTIFIED=""
reconcile_rewritten_base "$REMOTE_SHA" "$LOCAL_SHA" \
  || fail "a squashed rewrite that also advanced should be adopted"
[ "$(git rev-parse master)" = "$REMOTE_SHA" ] || fail "master was not reset onto the advanced rewrite"
[ -z "$NOTIFIED" ] || fail "a recoverable rewrite must not notify"
[ -f c.txt ] || fail "the remote's newer commit should be present after the reset"

echo "PASS"
