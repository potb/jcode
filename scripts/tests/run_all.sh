#!/usr/bin/env bash
# Run every shell harness test under scripts/tests.
#
# These cover scripts/upstream_merge_agent.sh, which no cargo target reaches:
# it is a scheduled shell job, so its regressions are only ever caught here.
set -uo pipefail

cd "$(dirname "$0")" || exit 1

status=0
for test in ./*_test.sh; do
  printf '%s: ' "$(basename "$test")"
  if output=$(bash "$test" 2>&1); then
    echo "PASS"
  else
    echo "FAIL"
    printf '%s\n' "$output" | tail -20
    status=1
  fi
done
exit "$status"
