#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
flake_file="$repo_root/flake.nix"
lock_file="$repo_root/flake.lock"

# flake.nix is intentionally self-contained. If it starts importing local Nix
# modules, add them to both this cache and the fingerprint below.

if [[ ! -f "$flake_file" || ! -f "$lock_file" ]]; then
  printf 'error: expected pinned flake files at %s and %s\n' "$flake_file" "$lock_file" >&2
  exit 1
fi

if [[ -n "${JCODE_NIX_FLAKE_CACHE_DIR:-}" ]]; then
  cache_root="$JCODE_NIX_FLAKE_CACHE_DIR"
elif [[ -n "${JCODE_NIX_PROFILE_DIR:-}" ]]; then
  cache_root="$JCODE_NIX_PROFILE_DIR/.flakes"
elif [[ -n "${JCODE_HOME:-}" ]]; then
  cache_root="$JCODE_HOME/nix-flakes"
elif [[ -n "${HOME:-}" ]]; then
  cache_root="$HOME/.jcode/nix-flakes"
else
  cache_root="$repo_root/target/jcode-nix-flakes"
fi

fingerprint=$(
  {
    printf 'flake.nix\0'
    cat "$flake_file"
    printf '\0flake.lock\0'
    cat "$lock_file"
  } | sha256sum | awk '{print $1}'
)
cache_path="$cache_root/$fingerprint"

if [[ ! -f "$cache_path/flake.nix" || ! -f "$cache_path/flake.lock" ]]; then
  mkdir -p "$cache_root"
  temp_path="$cache_root/.${fingerprint}.tmp.$$"
  trap 'rm -f "$temp_path/flake.nix" "$temp_path/flake.lock"; rmdir "$temp_path" 2>/dev/null || true' EXIT
  mkdir "$temp_path"
  cp "$flake_file" "$temp_path/flake.nix"
  cp "$lock_file" "$temp_path/flake.lock"
  if ! mv -T "$temp_path" "$cache_path" 2>/dev/null; then
    # Another process can win the same immutable cache entry concurrently.
    [[ -f "$cache_path/flake.nix" && -f "$cache_path/flake.lock" ]]
    rm -f "$temp_path/flake.nix" "$temp_path/flake.lock"
    rmdir "$temp_path"
  fi
  trap - EXIT
fi

printf '%s\n' "$cache_path"
