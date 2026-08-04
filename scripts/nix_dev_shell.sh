#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
shell_name="${1:-selfdev}"
if [[ $# -gt 0 ]]; then
  shift
fi

case "$shell_name" in
  selfdev|desktop|full) ;;
  *)
    printf 'error: unknown jcode Nix shell %s (expected selfdev|desktop|full)\n' "$shell_name" >&2
    exit 2
    ;;
esac

flake_path=$("$repo_root/scripts/nix_flake_cache.sh")
if [[ $# -gt 0 ]]; then
  exec nix develop "path:$flake_path#$shell_name" --command "$@"
fi
exec nix develop "path:$flake_path#$shell_name"
