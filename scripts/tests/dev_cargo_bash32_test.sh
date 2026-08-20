#!/usr/bin/env bash
# Guard scripts/dev_cargo.sh against bash 4+ syntax.
#
# macOS ships bash 3.2 as /bin/bash, where `declare -A` fails at runtime with
# `declare: -A: invalid option`, so the wrapper's whole job (Nix profile, linker
# selection, sccache) is silently lost there. See issue #219.
set -uo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
script="$repo_root/scripts/dev_cargo.sh"
status=0

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  status=1
}

# Only inspect executable lines: prose in comments may legitimately name the
# constructs this test forbids.
code_only=$(sed 's/[[:space:]]*#.*$//' "$script")
tab=$(printf '\t')

while IFS=$'\t' read -r pattern description; do
  [[ -z "$pattern" ]] && continue
  if printf '%s\n' "$code_only" | grep -qE "$pattern"; then
    fail "dev_cargo.sh uses $description, which bash 3.2 does not support"
    printf '%s\n' "$code_only" | grep -nE "$pattern" | head -3 >&2
  fi
done <<PATTERNS
declare[[:space:]]+-[a-zA-Z]*A${tab}declare -A (associative array)
local[[:space:]]+-[a-zA-Z]*A${tab}local -A (associative array)
^[[:space:]]*(readarray|mapfile)[[:space:]]${tab}readarray or mapfile
\\\$\\{[A-Za-z_][A-Za-z_0-9]*,,${tab}\${var,,} (lowercase expansion)
\\\$\\{[A-Za-z_][A-Za-z_0-9]*\\^\\^${tab}\${var^^} (uppercase expansion)
PATTERNS

# `declare -A` is a runtime error under bash 3.2, not a parse error, so `-n`
# would pass on the very script this test exists to reject. Run the memo for
# real instead, under an old bash supplied by the caller as $BASH32.
run_memo_under() {
  "$1" -c "
    log() { :; }
    source '$2'
    printf 'empty=[%s] ' \"\$(remembered_linker_verdict cc:mold)\"
    remember_linker_verdict cc:mold ok
    remember_linker_verdict cc:lld bad
    printf 'mold=[%s] lld=[%s] ' \"\$(remembered_linker_verdict cc:mold)\" \"\$(remembered_linker_verdict cc:lld)\"
    printf 'unrelated=[%s] prefix=[%s]' \"\$(remembered_linker_verdict clang:mold)\" \"\$(remembered_linker_verdict c)\"
  " 2>&1
}

probe_harness=$(mktemp)
trap 'rm -f "$probe_harness"' EXIT
awk '
  /^__jcode_linker_probe_cache=/ { print; next }
  /^remembered_linker_verdict\(\)/ , /^}/ { print; next }
  /^remember_linker_verdict\(\)/ , /^}/ { print }
' "$script" >"$probe_harness"

expected='empty=[] mold=[ok] lld=[bad] unrelated=[] prefix=[]'

verdict_check=$(run_memo_under bash "$probe_harness")
if [[ "$verdict_check" != "$expected" ]]; then
  fail "linker verdict memo misbehaves under $BASH_VERSION
  expected: $expected
  actual:   $verdict_check"
fi

if [[ -n "${BASH32:-}" && -x "${BASH32:-}" ]]; then
  verdict_check=$(run_memo_under "$BASH32" "$probe_harness")
  if [[ "$verdict_check" != "$expected" ]]; then
    fail "linker verdict memo misbehaves under $("$BASH32" --version | head -1)
  expected: $expected
  actual:   $verdict_check"
  fi
fi

if [[ "$status" -eq 0 ]]; then
  if [[ -n "${BASH32:-}" && -x "${BASH32:-}" ]]; then
    echo "dev_cargo.sh: memo exact under $("$BASH32" --version | head -1)"
  else
    echo "dev_cargo.sh: no bash 4+ syntax, memo exact (set BASH32 to also run it under a real bash 3.2)"
  fi
fi
exit "$status"
