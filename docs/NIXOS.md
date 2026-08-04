# NixOS development and self-dev

The repository's Nix flake is a **development environment**, not a Nix build of
jcode. Nix supplies a pinned Rust toolchain, native libraries, and build tools;
Cargo still compiles in the checkout. That split gives reproducible tooling
without sacrificing Cargo's normal incremental caches.

## Fast start

```bash
scripts/nix_dev_shell.sh selfdev
scripts/dev_cargo.sh --print-setup
scripts/dev_cargo.sh build --profile selfdev -p jcode --bin jcode
```

The helper evaluates an immutable cache containing only `flake.nix` and
`flake.lock`, so entering the shell does not copy or hash the dirty checkout.
Plain `nix develop` also works when conventional flake entry is preferred.

To start a self-development session from the pinned environment:

```bash
scripts/nix_dev_shell.sh selfdev jcode self-dev --build
```

Inside a self-dev session, use `selfdev build-reload` for the normal edit,
build, reload loop. The repository wrapper also works when the long-lived daemon
was started outside the shell.

## Shells

The flake exposes separate environments so automatic builds do not pay for every
editor and desktop dependency:

- `nix develop` or `nix develop .#selfdev`: small default TUI build environment.
- `nix develop .#desktop`: smaller desktop build environment.
- `nix develop .#full`: explicit editor, lint, benchmark, TUI, and desktop
  environment.

`scripts/nix_dev_shell.sh selfdev|desktop|full` enters the same environments
through the small immutable flake cache and is the preferred low-overhead entry
point.

`scripts/dev_cargo.sh` selects `selfdev` normally and `desktop` for the desktop
package or workspace-wide builds. Set `JCODE_NIX_DEVELOP_SHELL=<name>` only to
override that selection deliberately. Set `JCODE_NIX_AUTO_DEVELOP=0` to disable
automatic shell entry when an externally managed environment should be used.

## What is cached

The fast path deliberately does not run Cargo in a Nix derivation or sandbox:

- Rust, mold, OpenSSL, and the other system dependencies live in the Nix store.
- Registry archives and Git dependencies use Cargo's normal persistent
  `CARGO_HOME`, which defaults to `~/.cargo`.
- Compiled dependencies and incremental state stay in this checkout's
  `target/`. Self-dev builds use `target/selfdev`.
- Entering or leaving `nix develop` does not clear either Cargo cache.

Do not point `CARGO_TARGET_DIR` at a temporary directory or the Nix store.
Self-dev also expects the binary at `target/selfdev/jcode`. Avoid `cargo clean`
unless invalidating the entire local cache is intentional.

The pinned nightly toolchain lets `scripts/dev_cargo.sh` enable rustc's parallel
front end. On Linux it also selects the host C compiler wrapper plus mold. The
wrapper intentionally skips sccache for the incremental selfdev profile because
sccache cannot cache incremental rustc units. The local `target/selfdev` cache
is the faster path for normal iteration.

Every shell exposes the same pinned `cargo` and `rustc` store path. The full
shell adds clippy, rustfmt, and rust-src as separate components, so the compiler
identity itself stays stable. Exact shell re-entry keeps the remaining build
environment stable for wrapper-driven Cargo builds.

## Persistent Nix profiles and garbage collection

A binary linked on NixOS can contain RUNPATH entries pointing into the Nix store.
For that reason, the wrapper records each automatic build environment under:

```text
~/.jcode/nix-profiles/<shell>-<architecture>-<os>
```

These profiles do two jobs:

1. They are durable Nix GC roots for the runtime libraries used by published
   selfdev binaries.
2. Warm builds enter the recorded environment directly instead of evaluating the
   flake and snapshotting the dirty checkout every time.

The wrapper always compiles inside the exact selected shell. For example, a TUI
build started from the full tooling shell re-enters the cached `selfdev` profile
instead of inheriting desktop `pkg-config` paths that would invalidate Cargo
artifacts. After a successful `jcode` selfdev link, it checks the ELF interpreter
and RUNPATH entries against that profile closure before the binary can be
published.

The profile is refreshed only when `flake.nix`, `flake.lock`, the selected shell,
or the host platform changes. Override its parent directory with
`JCODE_NIX_PROFILE_DIR` if needed. Do not delete the profiles while retaining
selfdev binaries that were linked against their Nix-store libraries.

Profile generations are retained indefinitely by default so older immutable
jcode builds continue to run. After pruning the corresponding old binaries, set
`JCODE_NIX_PROFILE_HISTORY_DAYS=30` (or another positive number) to remove
non-current profile generations older than that many days during profile use.
`keep`, `never`, and `0` preserve all generations.

Automatic refreshes use immutable copies under
`~/.jcode/nix-flakes/<sha256>/` containing only the two flake files. These tiny
copies can be deleted safely when no process is evaluating them; they contain no
Cargo or build artifacts. Keep `flake.nix` self-contained, or update
`scripts/nix_flake_cache.sh` if local Nix modules are introduced later.

Running the wrapper directly on NixOS is therefore enough:

```bash
scripts/dev_cargo.sh check --profile selfdev -p jcode --bin jcode
```

The first invocation materializes and records the profile. Later invocations use
the profile and preserve the Cargo registry and target caches.

Use `scripts/dev_cargo.sh` for binaries that must outlive the current shell.
Running raw `cargo` inside `nix develop` is fine for temporary checks, but it does
not create the durable profile or run the post-link closure validation.

## direnv

For the lowest interactive overhead, keep the checkout activated with direnv so
new jcode processes inherit the small selfdev environment directly. The checked
in `.envrc` uses the immutable flake cache and watches only the flake files and
cache helper. Enter `scripts/nix_dev_shell.sh full` explicitly when the larger
tooling environment is needed.

On NixOS, a declarative direnv setup is:

```nix
programs.direnv = {
  enable = true;
  nix-direnv.enable = true;
};
```

Then approve this repository's checked-in `.envrc` once:

```bash
direnv allow
```

## Measuring the cache

The full shell includes `hyperfine`. After one successful build, enter it with
`scripts/nix_dev_shell.sh full` and measure the warm edit-loop overhead without
deleting caches:

```bash
hyperfine --warmup 1 \
  'scripts/dev_cargo.sh build --profile selfdev -p jcode --bin jcode'

# Run inside the full shell if these tools are not installed globally.
du -sh "${CARGO_HOME:-$HOME/.cargo}" target/selfdev
```

An offline repeat build is a useful proof that Cargo already has its dependency
sources locally:

```bash
scripts/dev_cargo.sh build --offline --profile selfdev -p jcode --bin jcode
```

Changes to the Rust toolchain, enabled features, rustflags, or Cargo profile
correctly create new Cargo fingerprints. Ordinary source edits reuse the same
dependency and incremental artifacts.
