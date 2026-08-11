#!/usr/bin/env bash
# Harness test for publish_fork_if_ahead: real git repos, stubbed `gh`.
#
# Verifies the pull-request publish path end to end without touching GitHub:
# the PR branch is pushed, a PR is opened and merged with the `merge` method
# (never squash), and the local base branch is fast-forwarded onto the result.
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
    for a in "$@"; do
      case "$a" in --squash) echo "SQUASH USED" >&2; exit 1 ;; esac
    done
    # A bare repo cannot `git merge`, so build the merge commit directly. This
    # is exactly what GitHub's "merge" method produces: a two-parent commit
    # keeping both sides' original SHAs.
    base=$(git --git-dir="$REMOTE" rev-parse master)
    head=$(git --git-dir="$REMOTE" rev-parse auto/upstream-merge-pr)
    tree=$(git --git-dir="$REMOTE" rev-parse "$head^{tree}")
    merge=$(git --git-dir="$REMOTE" commit-tree "$tree" -p "$base" -p "$head" \
      -m "Merge pull request #7") || { echo "merge failed" >&2; exit 1; }
    git --git-dir="$REMOTE" update-ref refs/heads/master "$merge"
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
echo one > f; git add f; git commit -qm one
git push -q origin master
git remote add upstream "$TMP/upstream-placeholder"
echo two >> f; git commit -qam two
AHEAD=$(git rev-parse master)

# --- load the script's functions without running it --------------------------
FORK_REMOTE=origin UPSTREAM_REMOTE=upstream BASE=master
PUSH_FORK=1 PR_MERGE_METHOD=merge PR_BRANCH=auto/upstream-merge-pr
UPSTREAM_REF=upstream/master UP_SHA=deadbeef ENFORCE_ACTIONS_OFF=0
log() { echo "[log] $*"; }
ensure_fork_actions_disabled() { :; }
eval "$(awk '/^fork_slug\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^publish_fork_if_ahead\(\) \{/,/^}/' "$SCRIPT")"
eval "$(awk '/^sync_local_base_to_fork\(\) \{/,/^}/' "$SCRIPT")"

fail() { echo "FAIL: $*"; exit 1; }

publish_fork_if_ahead || fail "publish returned nonzero"

grep -q "pr create" "$GH_CALLS" || fail "no pull request was created"
grep -q -- "--merge" "$GH_CALLS" || fail "pull request was not merged with --merge"
grep -q -- "--squash" "$GH_CALLS" && fail "squash must never be used"

git --git-dir="$REMOTE" rev-parse auto/upstream-merge-pr >/dev/null 2>&1 \
  || fail "PR branch was not pushed to the fork"
git --git-dir="$REMOTE" merge-base --is-ancestor "$AHEAD" master \
  || fail "the fork's base does not contain the published commits"
[ "$(git -C "$REPO" rev-parse master)" = "$(git --git-dir="$REMOTE" rev-parse master)" ] \
  || fail "local base was not fast-forwarded onto the fork's merge commit"

# A second run has nothing to publish and must stay silent.
: > "$GH_CALLS"
publish_fork_if_ahead || fail "idempotent run returned nonzero"
[ -s "$GH_CALLS" ] && fail "an up-to-date fork must not open a pull request"

echo "PASS"
