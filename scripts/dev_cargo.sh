#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

# A long-lived jcode daemon may have been started outside `nix develop`, so its
# selfdev subprocess does not necessarily have Cargo or this repository's native
# libraries on PATH. On NixOS, transparently re-enter a pinned dev environment.
#
# The environment is recorded in a persistent Nix profile keyed only by the
# flake files. Profile refreshes evaluate a tiny immutable copy of those files,
# not the dirty checkout. Warm builds enter the profile directly, and the exact
# active shell remains a GC root for Nix-store libraries in published selfdev
# binaries. Cargo itself still writes downloads to ~/.cargo and incremental
# artifacts to target/.
is_nixos_host() {
  [[ -r /etc/os-release ]] && grep -q '^ID=nixos$' /etc/os-release
}

nix_shell_for_args() {
  if [[ -n "${JCODE_NIX_DEVELOP_SHELL:-}" ]]; then
    printf '%s\n' "$JCODE_NIX_DEVELOP_SHELL"
    return
  fi
  local arg
  for arg in "$@"; do
    case "$arg" in
      *jcode-desktop2*|--workspace|--all)
        printf 'desktop\n'
        return
        ;;
    esac
  done
  printf 'selfdev\n'
}

nix_profile_root() {
  if [[ -n "${JCODE_NIX_PROFILE_DIR:-}" ]]; then
    printf '%s\n' "$JCODE_NIX_PROFILE_DIR"
  elif [[ -n "${JCODE_HOME:-}" ]]; then
    printf '%s/nix-profiles\n' "$JCODE_HOME"
  elif [[ -n "${HOME:-}" ]]; then
    printf '%s/.jcode/nix-profiles\n' "$HOME"
  else
    printf '%s/target/jcode-nix-profiles\n' "$repo_root"
  fi
}

nix_profile_fingerprint() {
  local shell_name="$1"
  {
    printf 'shell=%s\nsystem=%s-%s\n' "$shell_name" "$(uname -s)" "$(uname -m)"
    cksum "$repo_root/flake.nix"
    if [[ -f "$repo_root/flake.lock" ]]; then
      cksum "$repo_root/flake.lock"
    else
      printf 'flake.lock=missing\n'
    fi
  } | cksum | awk '{print $1 "-" $2}'
}

nix_profile_path="${JCODE_NIX_PROFILE_PATH:-}"
nix_profile_status="${JCODE_NIX_PROFILE_STATUS:-inactive}"

ensure_nix_profile() {
  local shell_name="$1"
  local root shell_key platform fingerprint fingerprint_file cached_fingerprint="" flake_source
  local profile_lock_file profile_lock_fd
  root="$(nix_profile_root)"
  shell_key="${shell_name//[^A-Za-z0-9_.-]/_}"
  platform="$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')"
  nix_profile_path="$root/${shell_key}-${platform}"
  fingerprint_file="$nix_profile_path.fingerprint"
  fingerprint="$(nix_profile_fingerprint "$shell_name")"

  mkdir -p "$root"
  profile_lock_file="$root/.${shell_key}-${platform}.lock"
  exec {profile_lock_fd}>"$profile_lock_file"
  flock "$profile_lock_fd"

  if [[ -f "$fingerprint_file" ]]; then
    cached_fingerprint="$(cat "$fingerprint_file")"
  fi
  if [[ -e "$nix_profile_path" && "$cached_fingerprint" == "$fingerprint" ]]; then
    nix_profile_status="cached"
  else
    flake_source="$("$repo_root/scripts/nix_flake_cache.sh")"
    printf 'dev_cargo: refreshing persistent Nix profile %s from cached flake %s#%s\n' \
      "$nix_profile_path" "$flake_source" "$shell_name" >&2
    JCODE_NIX_QUIET=1 nix develop --profile "$nix_profile_path" \
      "path:$flake_source#$shell_name" --command true
    local fingerprint_tmp="$fingerprint_file.tmp.$$"
    printf '%s\n' "$fingerprint" >"$fingerprint_tmp"
    mv -f "$fingerprint_tmp" "$fingerprint_file"
    nix_profile_status="refreshed"
  fi

  # Old generations intentionally remain GC roots by default because immutable
  # jcode binaries can still contain their Nix-store RUNPATHs. Contributors who
  # also prune old binaries can opt into a bounded history, e.g. 30 days.
  local history_days="${JCODE_NIX_PROFILE_HISTORY_DAYS:-keep}"
  case "$history_days" in
    keep|never|0|"") ;;
    *[!0-9]*)
      printf 'error: JCODE_NIX_PROFILE_HISTORY_DAYS must be keep or a positive integer\n' >&2
      exit 2
      ;;
    *)
      if ! nix profile wipe-history --profile "$nix_profile_path" \
        --older-than "${history_days}d" >/dev/null; then
        printf 'dev_cargo: warning: failed to prune Nix profile history for %s\n' \
          "$nix_profile_path" >&2
      fi
      ;;
  esac

  export JCODE_NIX_PROFILE_PATH="$nix_profile_path"
  export JCODE_NIX_PROFILE_STATUS="$nix_profile_status"
  flock -u "$profile_lock_fd"
  exec {profile_lock_fd}>&-
}

active_nix_shell_supports() {
  local requested="$1"
  local active="${JCODE_NIX_DEVSHELL_NAME:-}"
  [[ "$active" == "$requested" ]]
}

maybe_enter_nix_develop() {
  local mode="${JCODE_NIX_AUTO_DEVELOP:-auto}"
  local force="false"
  local disabled="false"
  case "$mode" in
    auto|"") ;;
    1|true|yes|on|force) force="true" ;;
    0|false|no|off|never) disabled="true" ;;
    *)
      printf 'error: unsupported JCODE_NIX_AUTO_DEVELOP=%s (expected auto|on|off)\n' "$mode" >&2
      exit 2
      ;;
  esac

  if [[ "${JCODE_NIX_DEVELOP_REEXEC:-0}" == "1" ]]; then
    if command -v cargo >/dev/null 2>&1; then
      return 0
    fi
    printf 'error: nix develop completed but cargo is still unavailable on PATH\n' >&2
    exit 127
  fi

  local shell_name
  shell_name="$(nix_shell_for_args "$@")"

  if command -v cargo >/dev/null 2>&1 \
    && [[ "${JCODE_NIX_DEVSHELL_ACTIVE:-0}" == "1" ]] \
    && active_nix_shell_supports "$shell_name"; then
    command -v nix >/dev/null 2>&1 || {
      printf 'error: active jcode Nix shell cannot create its runtime GC root because nix is unavailable\n' >&2
      exit 127
    }
    # Root the exact selected shell that will compile.
    ensure_nix_profile "${JCODE_NIX_DEVSHELL_NAME}"
    return 0
  fi

  [[ "$disabled" == "true" ]] && return 0

  # In auto mode, NixOS always uses the repository shell so native libraries
  # (not just rustc/cargo) are present. On other hosts, unmanaged Cargo setups
  # remain untouched, while an active jcode Nix shell still re-enters the exact
  # requested profile.
  if [[ "$force" != "true" ]] \
    && command -v cargo >/dev/null 2>&1 \
    && [[ "${JCODE_NIX_DEVSHELL_ACTIVE:-0}" != "1" ]] \
    && ! is_nixos_host; then
    return 0
  fi

  if [[ ! -f "$repo_root/flake.nix" ]] || ! command -v nix >/dev/null 2>&1; then
    printf 'error: the jcode Nix dev shell is required here but cannot be entered\n' >&2
    printf '%s\n' 'hint: install Nix with flakes enabled, run nix develop, or set up Rust on PATH' >&2
    exit 127
  fi

  ensure_nix_profile "$shell_name"
  printf 'dev_cargo: entering cached Nix profile %s\n' "$nix_profile_path" >&2
  exec nix develop "$nix_profile_path" --command \
    env JCODE_NIX_DEVELOP_REEXEC=1 "$repo_root/scripts/dev_cargo.sh" "$@"
}

maybe_enter_nix_develop "$@"

# `selfdev test` installs a shell-level `cargo` shim so raw `cargo test/check`
# commands receive this wrapper's memory, linker, feature, and toolchain policy.
# Exporting this recursion guard makes the final `cargo` invocation below bypass
# that shim and resolve the real Cargo binary.
export JCODE_IN_DEV_CARGO=1

# shellcheck source=scripts/remote_config.sh
source "$repo_root/scripts/remote_config.sh"
jcode_load_remote_config

log() {
  printf 'dev_cargo: %s\n' "$*" >&2
}

selected_linker_mode="not-configured"
selected_linker_desc=""
selected_linker_driver=""
sccache_status="disabled"
selfdev_low_memory_status="disabled"
feature_profile_status="default"
build_jobs_status="cargo-default"
git_meta_status="not-configured"
build_tmpdir_status="system-default"
if [[ "${JCODE_NIX_DEVELOP_REEXEC:-0}" == "1" ]]; then
  nix_develop_status="auto-reexec"
elif [[ "${JCODE_NIX_DEVSHELL_ACTIVE:-0}" == "1" ]]; then
  nix_develop_status="active"
else
  nix_develop_status="inactive"
fi

path_is_memory_backed() {
  local path="$1"
  local fs_type=""
  if command -v findmnt >/dev/null 2>&1; then
    fs_type=$(findmnt -n -o FSTYPE --target "$path" 2>/dev/null || true)
  elif [[ "$(uname -s)" == "Linux" ]]; then
    fs_type=$(stat -f -c '%T' "$path" 2>/dev/null || true)
  fi
  case "$fs_type" in
    tmpfs|ramfs) return 0 ;;
    *) return 1 ;;
  esac
}

configure_build_tmpdir() {
  local candidate="${JCODE_BUILD_TMPDIR:-}"
  if [[ -n "$candidate" ]]; then
    build_tmpdir_status="explicit"
  elif [[ -n "${TMPDIR:-}" ]]; then
    build_tmpdir_status="inherited"
    return
  elif ! path_is_memory_backed /tmp; then
    return
  elif [[ -n "${JCODE_SCRATCH_DIR:-}" ]]; then
    candidate="$JCODE_SCRATCH_DIR/cargo-tmp"
    build_tmpdir_status="jcode-scratch"
  elif [[ -n "${JCODE_HOME:-}" ]]; then
    candidate="$JCODE_HOME/scratch/cargo-tmp"
    build_tmpdir_status="jcode-home"
  elif [[ -n "${HOME:-}" ]]; then
    candidate="$HOME/.jcode/scratch/cargo-tmp"
    build_tmpdir_status="home-scratch"
  else
    candidate="$repo_root/target/jcode-scratch/cargo-tmp"
    build_tmpdir_status="target-scratch"
  fi

  if mkdir -p "$candidate" 2>/dev/null; then
    export TMPDIR="$candidate"
    log "using disk-backed build temp directory: $TMPDIR"
  else
    build_tmpdir_status="fallback-system-default"
    log "could not create build temp directory $candidate; using system default"
  fi
}

rust_host_triple() {
  local host=""
  if command -v rustc >/dev/null 2>&1; then
    host="$(rustc -vV 2>/dev/null | awk '/^host: / {print $2; exit}')"
  fi
  if [[ -z "$host" ]]; then
    case "$(uname -s)-$(uname -m)" in
      Linux-x86_64) host="x86_64-unknown-linux-gnu" ;;
      Linux-aarch64|Linux-arm64) host="aarch64-unknown-linux-gnu" ;;
      Darwin-x86_64) host="x86_64-apple-darwin" ;;
      Darwin-aarch64|Darwin-arm64) host="aarch64-apple-darwin" ;;
    esac
  fi
  printf '%s\n' "$host"
}

cargo_target_env_prefix() {
  local host
  host="$(rust_host_triple)"
  if [[ -z "$host" ]]; then
    return 1
  fi
  printf 'CARGO_TARGET_%s' "$(printf '%s' "$host" | tr '[:lower:]' '[:upper:]' | tr '-' '_')"
}

target_rustflags_var() {
  local prefix
  prefix="$(cargo_target_env_prefix)" || return 1
  printf '%s_RUSTFLAGS\n' "$prefix"
}

target_linker_var() {
  local prefix
  prefix="$(cargo_target_env_prefix)" || return 1
  printf '%s_LINKER\n' "$prefix"
}

append_rustflags() {
  local new_flag="$1"
  local var current
  var="$(target_rustflags_var)" || {
    var="RUSTFLAGS"
  }
  current="${!var-}"
  if [[ -z "$current" ]]; then
    printf -v "$var" '%s' "$new_flag"
  else
    printf -v "$var" '%s %s' "$current" "$new_flag"
  fi
  export "${var?}"
}

set_default_target_linker() {
  local linker="$1"
  local var
  var="$(target_linker_var)" || return 1
  if [[ -z "${!var-}" ]]; then
    printf -v "$var" '%s' "$linker"
    export "${var?}"
  fi
}

current_target_rustflags() {
  local var
  var="$(target_rustflags_var)" || {
    printf '%s\n' "${RUSTFLAGS:-<unset>}"
    return
  }
  printf '%s\n' "${!var-<unset>}"
}

current_target_linker() {
  local var
  var="$(target_linker_var)" || {
    printf '<unset>\n'
    return
  }
  printf '%s\n' "${!var-<unset>}"
}

selected_profile() {
  # Print the Cargo profile selected by the args, defaulting to "dev" (cargo's
  # default for build/check) when no --profile/--release is present.
  local expect_profile_name="false"
  local profile="dev"
  for arg in "$@"; do
    if [[ "$expect_profile_name" == "true" ]]; then
      profile="$arg"
      expect_profile_name="false"
      continue
    fi
    case "$arg" in
      --release|-r) profile="release" ;;
      --profile=*) profile="${arg#--profile=}" ;;
      --profile) expect_profile_name="true" ;;
    esac
  done
  printf '%s\n' "$profile"
}

# Determine whether the effective build will use incremental compilation.
# sccache cannot cache incremental units, so this gates whether sccache is
# useful at all. CARGO_INCREMENTAL (if set) wins; otherwise infer from the
# profile's incremental setting in Cargo.toml.
build_is_incremental() {
  case "${CARGO_INCREMENTAL:-}" in
    0|false|no|off) return 1 ;;
    1|true|yes|on) return 0 ;;
  esac
  case "$(selected_profile "$@")" in
    # Non-incremental profiles (see Cargo.toml): sccache can produce hits here.
    release-lto) return 1 ;;
    # selfdev/dev/release/test and unknown profiles default to incremental.
    *) return 0 ;;
  esac
}

maybe_enable_sccache() {
  case "${SCCACHE_DISABLE:-}" in
    1|true|yes|on)
      if [[ -n "${RUSTC_WRAPPER:-}" && "${RUSTC_WRAPPER}" == *sccache* ]]; then
        unset RUSTC_WRAPPER
      fi
      sccache_status="disabled-by-env"
      log "sccache disabled by SCCACHE_DISABLE"
      return
      ;;
  esac

  # sccache cannot cache incremental compilations, so for our default
  # incremental profiles it produces 0% hits while adding wrapper overhead and
  # misleading "enabled" status. Skip it for incremental builds unless the
  # caller explicitly forces it via JCODE_SCCACHE=1/on/force.
  local force_sccache="${JCODE_SCCACHE:-auto}"
  case "$force_sccache" in
    1|true|yes|on|force) force_sccache="1" ;;
    0|false|no|off|never)
      sccache_status="disabled-by-jcode-sccache"
      log "sccache disabled by JCODE_SCCACHE"
      return
      ;;
    *) force_sccache="auto" ;;
  esac

  if [[ -n "${RUSTC_WRAPPER:-}" ]]; then
    if [[ "${RUSTC_WRAPPER}" == *sccache* ]] \
      && [[ "$force_sccache" != "1" ]] \
      && build_is_incremental "$@"; then
      unset RUSTC_WRAPPER
      sccache_status="removed-inherited-incremental"
      log "removed inherited sccache wrapper for incremental build (it cannot cache incremental units)"
      return
    fi
    sccache_status="external:${RUSTC_WRAPPER}"
    log "keeping existing RUSTC_WRAPPER=${RUSTC_WRAPPER}"
    return
  fi

  if [[ "$force_sccache" != "1" ]] && build_is_incremental "$@"; then
    sccache_status="skipped-incremental"
    log "sccache skipped for incremental build (it cannot cache incremental units; set JCODE_SCCACHE=on to force, or use a non-incremental profile)"
    return
  fi

  if command -v sccache >/dev/null 2>&1; then
    sccache --start-server >/dev/null 2>&1 || true
    export RUSTC_WRAPPER=sccache
    sccache_status="enabled"
    log "using sccache"
  else
    sccache_status="not-found"
    log "sccache not found; using direct rustc"
  fi
}

uses_selfdev_profile() {
  local expect_profile_name="false"
  for arg in "$@"; do
    if [[ "$expect_profile_name" == "true" ]]; then
      [[ "$arg" == "selfdev" ]] && return 0
      expect_profile_name="false"
      continue
    fi

    case "$arg" in
      --profile=selfdev)
        return 0
        ;;
      --profile)
        expect_profile_name="true"
        ;;
    esac
  done
  return 1
}

has_explicit_feature_args() {
  local expect_value="false"
  for arg in "$@"; do
    if [[ "$expect_value" == "true" ]]; then
      expect_value="false"
      continue
    fi
    case "$arg" in
      --)
        return 1
        ;;
      --features|--no-default-features)
        return 0
        ;;
      --features=*|--no-default-features=*)
        return 0
        ;;
    esac
  done
  return 1
}

feature_args_from_profile() {
  local profile="$1"
  case "$profile" in
    ""|default)
      return 0
      ;;
    minimal|none)
      printf '%s\0' --no-default-features
      ;;
    pdf)
      printf '%s\0' --no-default-features --features pdf
      ;;
    embeddings)
      printf '%s\0' --no-default-features --features embeddings
      ;;
    full)
      printf '%s\0' --features embeddings,pdf,bedrock
      ;;
    *)
      return 1
      ;;
  esac
}

validate_feature_profile() {
  local profile="${JCODE_DEV_FEATURE_PROFILE:-default}"
  case "$profile" in
    ""|default|minimal|none|pdf|embeddings|full)
      ;;
    *)
      printf 'error: unsupported JCODE_DEV_FEATURE_PROFILE=%s (expected default|minimal|pdf|embeddings|full)\n' "$profile" >&2
      exit 1
      ;;
  esac
}

build_cargo_argv() {
  local profile="${JCODE_DEV_FEATURE_PROFILE:-default}"
  if [[ "$profile" == "default" || -z "$profile" ]]; then
    feature_profile_status="default"
    printf '%s\0' "$@"
    return 0
  fi

  if has_explicit_feature_args "$@"; then
    feature_profile_status="ignored-explicit-cargo-args"
    printf '%s\0' "$@"
    return 0
  fi

  local -a feature_args=()
  while IFS= read -r -d '' arg; do
    feature_args+=("$arg")
  done < <(feature_args_from_profile "$profile")

  feature_profile_status="$profile"
  local inserted="false"
  for arg in "$@"; do
    if [[ "$arg" == "--" && "$inserted" == "false" ]]; then
      printf '%s\0' "${feature_args[@]}"
      inserted="true"
    fi
    printf '%s\0' "$arg"
  done
  if [[ "$inserted" == "false" ]]; then
    printf '%s\0' "${feature_args[@]}"
  fi
}

meminfo_kib() {
  local key="$1"
  awk -v key="$key" '$1 == key ":" { print $2; exit }' /proc/meminfo 2>/dev/null || true
}

selfdev_low_memory_default_needed() {
  [[ "$(uname -s)" == "Linux" ]] || return 1
  [[ -r /proc/meminfo ]] || return 1
  command -v pgrep >/dev/null 2>&1 || return 1
  pgrep -x earlyoom >/dev/null 2>&1 || return 1

  local mem_total_kib mem_available_kib swap_total_kib
  mem_total_kib=$(meminfo_kib MemTotal)
  mem_available_kib=$(meminfo_kib MemAvailable)
  swap_total_kib=$(meminfo_kib SwapTotal)
  [[ -n "$mem_total_kib" && -n "$mem_available_kib" && -n "$swap_total_kib" ]] || return 1

  # On small no-swap machines, earlyoom can terminate the root jcode rustc
  # around 1-3 GiB RSS before the kernel OOM killer would report anything.
  # Keep this adaptive so larger workstations, and currently-idle smaller
  # workstations with enough headroom, retain the faster inherited selfdev
  # profile by default.
  (( swap_total_kib == 0 && mem_total_kib < 24576 * 1024 && mem_available_kib < 8192 * 1024 ))
}

cpu_count() {
  local n
  n=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)
  [[ "$n" =~ ^[0-9]+$ && "$n" -ge 1 ]] || n=1
  printf '%s\n' "$n"
}

# Choose a Cargo job count from *currently available* memory so concurrent
# builds (e.g. several self-dev agents on one machine) self-throttle instead of
# all assuming the full core count and tripping earlyoom/OOM.
#
# After the monolith was split into the jcode-base/app-core/tui/cli crate DAG,
# the largest single rustc unit is jcode-base, which has grown back to a
# measured ~1.6 GiB RSS peak (selfdev profile, sampled VmRSS while building the
# lib), down from the old 2.5-3 GiB monolith but above the original ~1.28 GiB
# post-split figure. We budget ~1.75 GiB of currently-available memory per job:
# a deliberate cushion above the measured peak (rustc's true VmHWM can exceed a
# coarse sample, and jcode-base keeps growing) so a fresh build under load backs
# off before earlyoom SIGTERMs it. Clamp into [1, cpus]. On an idle 15 GiB
# machine this still uses ~7 of 8 cores; under memory pressure a fresh build
# backs off further. An explicit CARGO_BUILD_JOBS / JCODE_BUILD_JOBS always
# wins, and non-Linux hosts fall back to the cargo/.cargo default.
select_build_jobs() {
  # Respect an explicit override from either env var.
  local override="${JCODE_BUILD_JOBS:-${CARGO_BUILD_JOBS:-}}"
  if [[ -n "$override" ]]; then
    if [[ "$override" =~ ^[0-9]+$ && "$override" -ge 1 ]]; then
      export CARGO_BUILD_JOBS="$override"
      build_jobs_status="override:$override"
      return
    fi
    # Invalid override: warn and fall through to adaptive sizing.
    log "ignoring invalid job override '$override' (expected a positive integer); using adaptive sizing"
    unset CARGO_BUILD_JOBS
  fi

  # Adaptive sizing only on Linux where /proc/meminfo is available; elsewhere we
  # leave cargo to honor the .cargo/config.toml default.
  if [[ "$(uname -s)" != "Linux" || ! -r /proc/meminfo ]]; then
    build_jobs_status="cargo-default"
    return
  fi

  local cpus mem_available_kib mib_per_job_default mib_per_job
  cpus=$(cpu_count)
  mem_available_kib=$(meminfo_kib MemAvailable)
  [[ -n "$mem_available_kib" && "$mem_available_kib" =~ ^[0-9]+$ ]] || mem_available_kib=0

  # Per-job memory budget (MiB). Sized with a cushion above the largest measured
  # rustc unit (jcode-base, ~1.6 GiB RSS sampled) so an idle machine uses nearly
  # every core while a memory-pressured one backs off before earlyoom kills a
  # build. Tunable per host via JCODE_BUILD_MIB_PER_JOB.
  mib_per_job_default=1792
  mib_per_job="${JCODE_BUILD_MIB_PER_JOB:-$mib_per_job_default}"
  [[ "$mib_per_job" =~ ^[0-9]+$ && "$mib_per_job" -ge 256 ]] || mib_per_job="$mib_per_job_default"

  local mem_available_mib jobs_by_mem jobs
  mem_available_mib=$(( mem_available_kib / 1024 ))
  jobs_by_mem=$(( mem_available_mib / mib_per_job ))
  (( jobs_by_mem < 1 )) && jobs_by_mem=1

  # Final job count is the smaller of "fits in memory" and core count.
  jobs="$jobs_by_mem"
  (( jobs > cpus )) && jobs="$cpus"
  (( jobs < 1 )) && jobs=1

  export CARGO_BUILD_JOBS="$jobs"
  build_jobs_status="adaptive:${jobs} (cpus=${cpus}, mem_avail=${mem_available_mib}MiB, budget=${mib_per_job}MiB/job)"
  if (( jobs < cpus )); then
    log "limiting cargo to ${jobs} job(s) under memory pressure (${mem_available_mib}MiB available, ~${mib_per_job}MiB/job); override with JCODE_BUILD_JOBS"
  fi
}

# Keep `jcode-build-meta`'s embedded git metadata in lockstep with HEAD without
# making it watch `.git` mtimes.
#
# `jcode-build-meta/build.rs` deliberately does NOT declare `.git/HEAD`/`.git/index`
# as `rerun-if-changed` inputs, because their mtimes change on every `git add`,
# `git status`, commit, and concurrent-agent git op -- which would force a
# full-tree recompile (base -> app-core -> tui -> cli) on every incremental
# build. The trade-off was that after a commit the binary kept embedding the
# previous short hash, so the self-dev publish guard rejected it with
# "binary was built from git hash X, but source state is Y" until someone
# manually touched Cargo.toml.
#
# Instead, export the *value* of the current git hash/date. build.rs declares
# `cargo:rerun-if-env-changed=JCODE_BUILD_GIT_HASH` (and _DATE), so cargo reruns
# the build script ONLY when these values change -- i.e. exactly when HEAD moves
# -- and never on a bare `git add`/`status` or repeated builds on the same
# commit. This keeps the embedded hash correct after every commit while keeping
# same-commit incremental builds fully incremental. We intentionally do NOT
# export JCODE_BUILD_GIT_DIRTY here: the dirty flag flips on every edit and would
# reintroduce per-build churn; the publish guard validates dirty builds via the
# source fingerprint / mtime path instead of the embedded flag.
export_git_build_metadata() {
  # Respect any value the caller already set (e.g. release/CI pipelines).
  if [[ -n "${JCODE_BUILD_GIT_HASH:-}" ]]; then
    git_meta_status="external:${JCODE_BUILD_GIT_HASH}"
    return
  fi
  if ! command -v git >/dev/null 2>&1; then
    git_meta_status="git-not-found"
    return
  fi
  local hash date
  hash="$(git -C "$repo_root" rev-parse --short HEAD 2>/dev/null || true)"
  if [[ -z "$hash" ]]; then
    git_meta_status="no-head"
    return
  fi
  export JCODE_BUILD_GIT_HASH="$hash"
  date="$(git -C "$repo_root" log -1 --format=%ci 2>/dev/null || true)"
  if [[ -n "$date" ]]; then
    export JCODE_BUILD_GIT_DATE="$date"
  fi
  git_meta_status="hash=${hash}"
}

maybe_configure_low_memory_selfdev() {
  if ! uses_selfdev_profile "$@"; then
    selfdev_low_memory_status="not-selfdev"
    return
  fi

  local mode="${JCODE_SELFDEV_LOW_MEMORY:-auto}"
  case "$mode" in
    1|true|yes|on|force)
      ;;
    0|false|no|off|never)
      selfdev_low_memory_status="disabled-by-env"
      return
      ;;
    auto|"")
      if ! selfdev_low_memory_default_needed; then
        selfdev_low_memory_status="auto-not-needed"
        return
      fi
      ;;
    *)
      printf 'error: unsupported JCODE_SELFDEV_LOW_MEMORY=%s (expected auto|on|off)\n' "$mode" >&2
      exit 1
      ;;
  esac

  export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-1}"
  export CARGO_PROFILE_SELFDEV_INCREMENTAL="${CARGO_PROFILE_SELFDEV_INCREMENTAL:-true}"
  export CARGO_PROFILE_SELFDEV_CODEGEN_UNITS="${CARGO_PROFILE_SELFDEV_CODEGEN_UNITS:-256}"
  # The low-memory profile deliberately keeps incremental compilation enabled
  # to avoid repeatedly rebuilding large root crates. sccache rejects Cargo
  # incremental builds, so default to direct rustc unless the caller opts out
  # of low-memory mode entirely.
  export SCCACHE_DISABLE="${SCCACHE_DISABLE:-1}"
  selfdev_low_memory_status="enabled:incremental=${CARGO_PROFILE_SELFDEV_INCREMENTAL},codegen-units=${CARGO_PROFILE_SELFDEV_CODEGEN_UNITS}"
  log "using low-memory selfdev overrides (${selfdev_low_memory_status#enabled:})"
}

# Enable rustc's parallel front-end (`-Zthreads`) for iterative dev/selfdev/test
# builds. The jcode monoliths (jcode-base/app-core/tui) are ~80% single-threaded
# front-end (type-check + borrow-check + monomorphization collection); at
# opt-level 0 that front-end, not codegen, dominates wall time. The parallel
# front-end is a nightly-only `-Z` flag, so this is gated on a nightly toolchain
# being available and only applies to the unoptimized iteration profiles.
#
# Measured on this repo (Intel Ultra 7, 8 logical cores, selfdev profile):
#   jcode-base clean recompile  25.3s -> 12.7s   (-Zthreads=4)
#   base-edit full-chain rebuild  ~16s -> ~10s
# Cranelift was tried too and was *slower* here (16.5s) because the bottleneck is
# the front-end, not codegen, so we deliberately do not enable it.
#
# Controls:
#   JCODE_PARALLEL_FRONTEND=auto|0|1   (default auto)
#   JCODE_FRONTEND_THREADS=<n>         (default 4; diminishing returns past 4)
#   JCODE_DEV_TOOLCHAIN=<name>         (default: nightly when present)
parallel_frontend_status="disabled"
parallel_frontend_toolchain=""

dev_nightly_toolchain() {
  # Prefer an explicit override, else a `+toolchain` already on the argv, else
  # the first installed nightly toolchain.
  if [[ -n "${JCODE_DEV_TOOLCHAIN:-}" ]]; then
    printf '%s\n' "$JCODE_DEV_TOOLCHAIN"
    return 0
  fi
  local tc
  tc=$(rustup toolchain list 2>/dev/null | awk '/^nightly/ {print $1; exit}')
  tc=${tc%% *}
  [[ -n "$tc" ]] && printf '%s\n' "$tc"
  return 0
}

current_rustc_is_nightly() {
  command -v rustc >/dev/null 2>&1 \
    && rustc --version 2>/dev/null | grep -q -- '-nightly'
}

configure_parallel_frontend() {
  local requested="${JCODE_PARALLEL_FRONTEND:-auto}"
  local forced="false"
  case "$requested" in
    0|false|no|off)
      parallel_frontend_status="disabled-by-env"
      return 0
      ;;
    1|true|yes|on|force) forced="true" ;;
    auto) ;;
    *)
      parallel_frontend_status="disabled-bad-env:${requested}"
      return 0
      ;;
  esac

  # Only worth it for the unoptimized iteration profiles where the front-end is
  # the bottleneck; release/release-lto keep their own (codegen-bound) path.
  #
  # By default (`auto`) we restrict to the `selfdev` profile: it builds into the
  # isolated `target/selfdev` dir that only this script + `selfdev build` use, so
  # adding `-Zthreads` to RUSTFLAGS (which changes cargo's unit fingerprint)
  # cannot thrash rust-analyzer's `target/debug` cache. Forcing the flag on
  # (`JCODE_PARALLEL_FRONTEND=1`) opts dev/test in too, accepting that potential
  # cache contention.
  local profile
  profile=$(selected_profile "$@")
  case "$profile" in
    selfdev) ;;
    dev|test)
      if [[ "$forced" != "true" ]]; then
        parallel_frontend_status="skipped-profile-shared-target:${profile}"
        return 0
      fi
      ;;
    *)
      parallel_frontend_status="skipped-profile:${profile}"
      return 0
      ;;
  esac

  # If the caller already pinned a toolchain via `cargo +foo`, don't override it.
  for arg in "$@"; do
    case "$arg" in
      +*)
        parallel_frontend_status="skipped-explicit-toolchain:${arg}"
        return 0
        ;;
    esac
  done

  # Nix, mise, and other environment managers can place a pinned nightly rustc
  # directly on PATH without rustup. Use it in place, preserving the immutable
  # Nix toolchain and avoiding a second toolchain manager/cache.
  if [[ -z "${JCODE_DEV_TOOLCHAIN:-}" ]] && current_rustc_is_nightly; then
    local threads="${JCODE_FRONTEND_THREADS:-4}"
    [[ "$threads" =~ ^[0-9]+$ && "$threads" -ge 1 ]] || threads=4

    parallel_frontend_toolchain="path-nightly"
    append_rustflags "-Zthreads=${threads}"
    parallel_frontend_status="enabled:path-nightly:threads=${threads}"
    log "using parallel rustc front-end (nightly from PATH, -Zthreads=${threads})"
    return 0
  fi

  command -v rustup >/dev/null 2>&1 || {
    parallel_frontend_status="skipped-no-nightly-toolchain"
    return 0
  }
  local tc
  tc=$(dev_nightly_toolchain)
  if [[ -z "$tc" ]]; then
    parallel_frontend_status="skipped-no-nightly"
    return 0
  fi
  # Confirm the toolchain actually resolves (installed, not just configured).
  if ! rustup run "$tc" rustc --version >/dev/null 2>&1; then
    parallel_frontend_status="skipped-nightly-unavailable:${tc}"
    return 0
  fi

  local threads="${JCODE_FRONTEND_THREADS:-4}"
  [[ "$threads" =~ ^[0-9]+$ && "$threads" -ge 1 ]] || threads=4

  parallel_frontend_toolchain="$tc"
  export RUSTUP_TOOLCHAIN="$tc"
  append_rustflags "-Zthreads=${threads}"
  parallel_frontend_status="enabled:${tc}:threads=${threads}"
  log "using parallel rustc front-end (${tc}, -Zthreads=${threads})"
}

configure_linux_linker() {
  local requested_mode="${JCODE_FAST_LINKER:-auto}"
  local mode="$requested_mode"
  local driver="${JCODE_LINKER_DRIVER:-}"

  if [[ -n "$driver" ]] && ! command -v "$driver" >/dev/null 2>&1; then
    printf 'error: JCODE_LINKER_DRIVER is unavailable: %s\n' "$driver" >&2
    exit 1
  fi
  if [[ -z "$driver" ]]; then
    # Prefer the stdenv `cc` wrapper so slim Nix profiles do not need a second
    # compiler closure. Users can still opt into clang explicitly.
    if command -v cc >/dev/null 2>&1; then
      driver="cc"
    elif command -v clang >/dev/null 2>&1; then
      driver="clang"
    fi
  fi

  case "$mode" in
    auto)
      # Prefer mold over lld: on this repo's large statically-linked binary
      # (~300 MB .text), mold links the jcode bin in ~2.0s vs lld's ~2.9s
      # (measured, warm, selfdev profile). The bin relinks on every build, so
      # that ~0.8s is a per-build win.
      if command -v mold >/dev/null 2>&1 && [[ -n "$driver" ]]; then
        mode="mold"
      elif command -v ld.lld >/dev/null 2>&1 && [[ -n "$driver" ]]; then
        mode="lld"
      else
        mode="system"
      fi
      ;;
    lld|mold|system)
      ;;
    *)
      printf 'error: unsupported JCODE_FAST_LINKER=%s (expected auto|lld|mold|system)\n' "$mode" >&2
      exit 1
      ;;
  esac

  selected_linker_mode="$mode"
  selected_linker_driver="$driver"

  case "$mode" in
    lld)
      if [[ -z "$driver" ]]; then
        printf 'error: lld requested but no C compiler driver is available\n' >&2
        exit 1
      fi
      set_default_target_linker "$driver"
      append_rustflags "-C link-arg=-fuse-ld=lld"
      selected_linker_desc="$driver + lld"
      log "using $driver + lld"
      ;;
    mold)
      if [[ -z "$driver" ]]; then
        printf 'error: mold requested but no C compiler driver is available\n' >&2
        exit 1
      fi
      set_default_target_linker "$driver"
      append_rustflags "-C link-arg=-fuse-ld=mold"
      selected_linker_desc="$driver + mold"
      log "using $driver + mold"
      ;;
    system)
      # Leave the linker driver to cargo's default (`cc`). Only mold/lld need
      # clang as the driver, and forcing it here breaks machines without clang.
      selected_linker_desc="system linker settings"
      if [[ "$requested_mode" == "auto" ]]; then
        log "no supported fast linker detected; using system linker settings"
      else
        log "using system linker settings"
      fi
      ;;
  esac
}

print_setup() {
  if [[ -n "${JCODE_DEV_FEATURE_PROFILE:-}" && "${JCODE_DEV_FEATURE_PROFILE}" != "default" ]]; then
    feature_profile_status="${JCODE_DEV_FEATURE_PROFILE}"
  fi
  cat <<EOF
repo_root=$repo_root
os=$(uname -s)
arch=$(uname -m)
nix_develop_status=$nix_develop_status
nix_devshell_name=${JCODE_NIX_DEVSHELL_NAME:-<unset>}
nix_toolchain_channel=${JCODE_NIX_TOOLCHAIN_CHANNEL:-<unset>}
nix_profile_status=${JCODE_NIX_PROFILE_STATUS:-$nix_profile_status}
nix_profile_path=${JCODE_NIX_PROFILE_PATH:-${nix_profile_path:-<unset>}}
cargo_path=$(command -v cargo 2>/dev/null || printf '<missing>')
cargo_home=${CARGO_HOME:-${HOME:-<unset>}/.cargo}
target_cache=$repo_root/target
rust_host=$(rust_host_triple)
sccache_status=$sccache_status
selfdev_low_memory_status=$selfdev_low_memory_status
parallel_frontend_status=$parallel_frontend_status
parallel_frontend_toolchain=${parallel_frontend_toolchain:-<unset>}
build_jobs_status=$build_jobs_status
cargo_build_jobs=${CARGO_BUILD_JOBS:-<unset>}
build_tmpdir_status=$build_tmpdir_status
tmpdir=${TMPDIR:-<unset>}
feature_profile_status=$feature_profile_status
git_meta_status=$git_meta_status
build_git_hash=${JCODE_BUILD_GIT_HASH:-<unset>}
rustc_wrapper=${RUSTC_WRAPPER:-<unset>}
linker_mode=$selected_linker_mode
linker_desc=${selected_linker_desc:-<none>}
linker_driver=${selected_linker_driver:-<unset>}
linker=$(current_target_linker)
rustflags=$(current_target_rustflags)
EOF
}

validate_nix_runtime_binary() {
  local binary="$1"
  local profile="${JCODE_NIX_PROFILE_PATH:-}"

  case "${JCODE_NIX_VALIDATE_RUNTIME:-1}" in
    0|false|no|off)
      log "Nix runtime closure validation disabled by JCODE_NIX_VALIDATE_RUNTIME"
      return 0
      ;;
  esac

  if [[ -z "$profile" ]]; then
    printf 'error: cannot validate Nix runtime paths without JCODE_NIX_PROFILE_PATH\n' >&2
    return 1
  fi
  if [[ ! -x "$binary" ]]; then
    printf 'error: expected linked binary for Nix runtime validation: %s\n' "$binary" >&2
    return 1
  fi
  command -v patchelf >/dev/null 2>&1 || {
    printf 'error: patchelf is required for Nix runtime closure validation\n' >&2
    return 1
  }
  command -v nix-store >/dev/null 2>&1 || {
    printf 'error: nix-store is required for Nix runtime closure validation\n' >&2
    return 1
  }
  command -v ldd >/dev/null 2>&1 || {
    printf 'error: ldd is required for Nix runtime closure validation\n' >&2
    return 1
  }

  local profile_store closure rpath interpreter ldd_output resolved_paths runtime_paths entry rest store_root
  local missing="false"
  profile_store="$(readlink -f "$profile")"
  closure="$(nix-store --query --requisites "$profile_store")"
  rpath="$(patchelf --print-rpath "$binary")"
  interpreter="$(patchelf --print-interpreter "$binary" 2>/dev/null || true)"
  ldd_output="$(ldd "$binary" 2>&1 || true)"
  if grep -q 'not found' <<<"$ldd_output"; then
    printf 'error: linked library could not be resolved for %s\n%s\n' "$binary" "$ldd_output" >&2
    return 1
  fi
  resolved_paths="$(grep -o '/nix/store/[^[:space:]]*' <<<"$ldd_output" || true)"
  runtime_paths="$rpath:$interpreter:${resolved_paths//$'\n'/:}"

  IFS=: read -r -a runtime_entries <<<"$runtime_paths"
  for entry in "${runtime_entries[@]}"; do
    [[ "$entry" == /nix/store/* ]] || continue
    rest="${entry#/nix/store/}"
    store_root="/nix/store/${rest%%/*}"
    if ! grep -Fxq "$store_root" <<<"$closure"; then
      printf 'error: linked runtime path is not retained by Nix profile %s: %s\n' \
        "$profile" "$store_root" >&2
      missing="true"
    fi
  done

  if [[ "$missing" == "true" ]]; then
    printf '%s\n' 'hint: add the missing runtime package to the selected flake shell before publishing' >&2
    return 1
  fi
  log "validated Nix runtime closure for $binary against $profile"
}

is_jcode_selfdev_build() {
  local saw_build="false" saw_package="false" saw_binary="false" expect=""
  local arg
  [[ -z "${CARGO_BUILD_TARGET:-}" ]] || return 1
  for arg in "$@"; do
    if [[ "$expect" == "package" ]]; then
      [[ "$arg" == "jcode" ]] && saw_package="true"
      expect=""
      continue
    elif [[ "$expect" == "binary" ]]; then
      [[ "$arg" == "jcode" ]] && saw_binary="true"
      expect=""
      continue
    fi
    case "$arg" in
      build) saw_build="true" ;;
      -p|--package) expect="package" ;;
      -p=jcode|--package=jcode) saw_package="true" ;;
      --bin) expect="binary" ;;
      --bin=jcode) saw_binary="true" ;;
      --target|--target=*|--target-dir|--target-dir=*) return 1 ;;
    esac
  done
  [[ "$saw_build" == "true" && "$saw_package" == "true" && "$saw_binary" == "true" ]] \
    && uses_selfdev_profile "$@"
}

selfdev_jcode_binary_path() {
  local target_root="${CARGO_TARGET_DIR:-$repo_root/target}"
  if [[ "$target_root" != /* ]]; then
    target_root="$repo_root/$target_root"
  fi
  printf '%s/selfdev/jcode\n' "$target_root"
}

validate_requested_cargo_output() {
  if [[ -n "${JCODE_NIX_PROFILE_PATH:-}" ]] \
    && is_jcode_selfdev_build "${cargo_argv[@]}"; then
    validate_nix_runtime_binary "$(selfdev_jcode_binary_path)"
  fi
}

remote_connect_timeout() {
  local value="${JCODE_REMOTE_CONNECT_TIMEOUT:-5}"
  if [[ ! "$value" =~ ^[0-9]+$ || "$value" -lt 1 ]]; then
    value=5
  fi
  printf '%s\n' "$value"
}

remote_tcp_timeout() {
  # Bounded probe used the first time we contact a host (or after the recovery
  # window expires) so an unreachable host fails fast instead of waiting for
  # the full SSH ConnectTimeout. Accepts fractional seconds (GNU timeout).
  local value="${JCODE_REMOTE_TCP_TIMEOUT:-1}"
  if [[ ! "$value" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    value=1
  fi
  printf '%s\n' "$value"
}

remote_recovery_tcp_timeout() {
  # Shorter probe used while a host was recently seen down. An up host always
  # answers in a few ms, so this still detects recovery on the next build while
  # keeping per-build cost low during an outage.
  local value="${JCODE_REMOTE_RECOVERY_TCP_TIMEOUT:-0.3}"
  if [[ ! "$value" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    value=0.3
  fi
  printf '%s\n' "$value"
}

remote_down_cache_ttl() {
  # How long (seconds) a recent failure keeps using the shorter recovery probe
  # timeout. Recovery is still detected on the very next build; this only
  # controls how long downtime builds stay cheap before reverting to the full
  # probe timeout. Set to 0 to always use the full timeout.
  local value="${JCODE_REMOTE_DOWN_TTL:-300}"
  if [[ ! "$value" =~ ^[0-9]+$ ]]; then
    value=300
  fi
  printf '%s\n' "$value"
}

remote_down_cache_path() {
  local remote="${JCODE_REMOTE_HOST:-}"
  local key
  if command -v cksum >/dev/null 2>&1; then
    key="$(printf '%s' "$remote" | cksum | awk '{print $1}')"
  else
    key="${remote//[^A-Za-z0-9_-]/_}"
  fi
  printf '%s/jcode-remote-down-%s\n' "${TMPDIR:-/tmp}" "$key"
}

remote_down_cache_fresh() {
  local ttl
  ttl="$(remote_down_cache_ttl)"
  [[ "$ttl" -gt 0 ]] || return 1
  local cache
  cache="$(remote_down_cache_path)"
  [[ -f "$cache" ]] || return 1
  local recorded now age
  recorded="$(cat "$cache" 2>/dev/null || printf '0')"
  [[ "$recorded" =~ ^[0-9]+$ ]] || return 1
  now="$(date +%s)"
  age=$((now - recorded))
  (( age >= 0 && age < ttl ))
}

record_remote_down() {
  local ttl
  ttl="$(remote_down_cache_ttl)"
  [[ "$ttl" -gt 0 ]] || return 0
  local cache
  cache="$(remote_down_cache_path)"
  date +%s >"$cache" 2>/dev/null || true
}

clear_remote_down() {
  local cache
  cache="$(remote_down_cache_path)"
  rm -f "$cache" 2>/dev/null || true
}

# Resolve the effective hostname/port for the remote alias and report whether
# the connection goes through a ProxyJump/ProxyCommand (where a direct TCP
# probe to the final hostname would be misleading).
remote_resolve_endpoint() {
  local ssh_bin="$1" remote="$2"
  local hostname="$remote" port=22 uses_proxy="false"
  local config key value
  if config="$("$ssh_bin" -G "$remote" 2>/dev/null)"; then
    while IFS=' ' read -r key value; do
      case "$key" in
        hostname) [[ -n "$value" ]] && hostname="$value" ;;
        port) [[ "$value" =~ ^[0-9]+$ ]] && port="$value" ;;
        proxyjump|proxycommand)
          [[ -n "$value" && "$value" != "none" ]] && uses_proxy="true"
          ;;
      esac
    done <<<"$config"
  fi
  printf '%s\t%s\t%s\n' "$hostname" "$port" "$uses_proxy"
}

# Fast TCP reachability check using bash /dev/tcp with a hard timeout.
remote_tcp_reachable() {
  local hostname="$1" port="$2"
  local tcp_timeout="${3:-}"
  if [[ -z "$tcp_timeout" ]]; then
    tcp_timeout="$(remote_tcp_timeout)"
  fi
  timeout "$tcp_timeout" bash -c "exec 3<>/dev/tcp/$hostname/$port" 2>/dev/null
}

remote_cargo_preflight() {
  local remote="${JCODE_REMOTE_HOST:-}"
  if [[ -z "$remote" ]]; then
    log "remote cargo requested but JCODE_REMOTE_HOST is not configured"
    return 1
  fi

  local ssh_bin="${JCODE_REMOTE_SSH_BIN:-ssh}"
  if ! command -v "$ssh_bin" >/dev/null 2>&1; then
    log "remote cargo requested but ssh binary is unavailable: $ssh_bin"
    return 1
  fi

  # Choose probe timeout: while a host was recently seen down, use a short
  # recovery timeout so downtime builds stay cheap. An up host answers in a few
  # ms regardless, so recovery is still detected on the very next build.
  local tcp_timeout
  if remote_down_cache_fresh; then
    tcp_timeout="$(remote_recovery_tcp_timeout)"
  else
    tcp_timeout="$(remote_tcp_timeout)"
  fi

  # Fast TCP pre-probe to fail fast when the host is offline, unless the
  # connection is proxied (where a direct probe would be wrong) or explicitly
  # disabled via JCODE_REMOTE_TCP_PROBE=0.
  local tcp_probe="${JCODE_REMOTE_TCP_PROBE:-1}"
  case "$tcp_probe" in
    0|false|no|off) tcp_probe="0" ;;
    *) tcp_probe="1" ;;
  esac
  if [[ "$tcp_probe" == "1" ]]; then
    local endpoint hostname port uses_proxy
    endpoint="$(remote_resolve_endpoint "$ssh_bin" "$remote")"
    IFS=$'\t' read -r hostname port uses_proxy <<<"$endpoint"
    if [[ "$uses_proxy" != "true" ]]; then
      if ! remote_tcp_reachable "$hostname" "$port" "$tcp_timeout"; then
        log "remote host $remote ($hostname:$port) unreachable within ${tcp_timeout}s TCP probe; using local cargo"
        record_remote_down
        return 1
      fi
    fi
  fi

  local connect_timeout
  connect_timeout="$(remote_connect_timeout)"
  local server_alive_interval="${JCODE_REMOTE_SERVER_ALIVE_INTERVAL:-10}"
  local server_alive_count="${JCODE_REMOTE_SERVER_ALIVE_COUNT_MAX:-1}"
  local output
  if ! output=$("$ssh_bin" \
    -o BatchMode=yes \
    -o ConnectTimeout="$connect_timeout" \
    -o ServerAliveInterval="$server_alive_interval" \
    -o ServerAliveCountMax="$server_alive_count" \
    "$remote" "printf 'jcode-remote-ok\\n'" 2>&1); then
    log "remote cargo preflight failed for $remote after ~${connect_timeout}s: $output"
    record_remote_down
    return 1
  fi
  clear_remote_down
  return 0
}

remote_cargo_fallback_mode() {
  local mode="${JCODE_REMOTE_CARGO_FALLBACK:-local}"
  case "$mode" in
    local|1|true|yes|on)
      printf 'local\n'
      ;;
    error|fail|0|false|no|off|never)
      printf 'error\n'
      ;;
    *)
      printf 'error: unsupported JCODE_REMOTE_CARGO_FALLBACK=%s (expected local|error)\n' "$mode" >&2
      exit 2
      ;;
  esac
}

cargo_test_has_explicit_filter() {
  [[ "${1:-}" == "test" ]] || return 1

  local expect_value=""
  shift
  for arg in "$@"; do
    if [[ -n "$expect_value" ]]; then
      expect_value=""
      continue
    fi

    case "$arg" in
      --)
        return 1
        ;;
      --bench|--bin|--example|--features|--manifest-path|--message-format|--package|-p|--profile|--target|--target-dir)
        expect_value="$arg"
        ;;
      --bench=*|--bin=*|--example=*|--features=*|--manifest-path=*|--message-format=*|--package=*|-p=*|--profile=*|--target=*|--target-dir=*)
        ;;
      --*)
        ;;
      -*)
        ;;
      *)
        return 0
        ;;
    esac
  done
  return 1
}

run_local_cargo() {
  if cargo_test_has_explicit_filter "${cargo_argv[@]}" && [[ "${JCODE_DEV_CARGO_ALLOW_ZERO_TESTS:-0}" != "1" ]]; then
    local output_file
    output_file=$(mktemp "${TMPDIR:-/tmp}/jcode-dev-cargo.XXXXXX")
    local status=0
    cargo "${cargo_argv[@]}" 2>&1 | tee "$output_file" || status=${PIPESTATUS[0]}
    if [[ "$status" -eq 0 ]] \
      && grep -qE '^running 0 tests$' "$output_file" \
      && ! grep -qE '^running [1-9][0-9]* tests$' "$output_file"; then
      printf 'dev_cargo: explicit cargo test filter matched zero tests; check the test path/name or set JCODE_DEV_CARGO_ALLOW_ZERO_TESTS=1 to allow this intentionally\n' >&2
      rm -f "$output_file"
      return 97
    fi
    rm -f "$output_file"
    return "$status"
  fi

  if is_jcode_selfdev_build "${cargo_argv[@]}"; then
    cargo "${cargo_argv[@]}"
    validate_requested_cargo_output
    return 0
  fi

  exec cargo "${cargo_argv[@]}"
}

validate_feature_profile
configure_build_tmpdir
export_git_build_metadata
maybe_configure_low_memory_selfdev "$@"
maybe_enable_sccache "$@"
configure_parallel_frontend "$@"
select_build_jobs

if [[ "$(uname -s)" == "Linux" ]]; then
  configure_linux_linker
fi

if [[ "${1:-}" == "--print-setup" ]]; then
  print_setup
  exit 0
fi

if [[ "${1:-}" == "--validate-nix-runtime" ]]; then
  if [[ $# -ne 2 ]]; then
    printf 'usage: scripts/dev_cargo.sh --validate-nix-runtime <binary>\n' >&2
    exit 2
  fi
  validate_nix_runtime_binary "$2"
  exit 0
fi

cargo_argv=()
while IFS= read -r -d '' arg; do
  cargo_argv+=("$arg")
done < <(build_cargo_argv "$@")

if [[ "${JCODE_REMOTE_CARGO:-0}" == "1" ]]; then
  if remote_cargo_preflight; then
    log "using remote cargo via scripts/remote_build.sh"
    "$repo_root/scripts/remote_build.sh" "${cargo_argv[@]}"
    validate_requested_cargo_output
    exit 0
  fi
  if [[ "$(remote_cargo_fallback_mode)" == "local" ]]; then
    log "remote cargo unavailable; falling back to local cargo (set JCODE_REMOTE_CARGO_FALLBACK=error to fail instead)"
  else
    log "remote cargo unavailable and fallback disabled"
    exit 75
  fi
fi

run_local_cargo
