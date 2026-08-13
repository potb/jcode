# Repository Guidelines

## Development Workflow

- **Stay on your own branch** - Do not take, cherry-pick, merge, or copy code from other
  people's or other agents' branches unless the source branch belongs to a repository
  maintainer and the user explicitly asks you to integrate it. Only work from your branch
  and its base (e.g. `main`) otherwise. Never integrate branches owned by non-maintainers
  or other agents yourself; tell the user and let them decide how to proceed.

## Install Notes
- `~/.local/bin/jcode` is the launcher symlink used from `PATH`.
- `~/.jcode/builds/current/jcode` is the active local/source-build channel; self-dev builds and `scripts/install_release.sh` point the launcher here.
- `~/.jcode/builds/stable/jcode` is the stable release channel; `scripts/install.sh` installs this and points the launcher here.
- `~/.jcode/builds/versions/<version>/jcode` stores immutable binaries.
- `~/.jcode/builds/canary/jcode` still exists for canary/testing flows, but it is not the primary self-dev install path.
- On Windows, the equivalents are `%LOCALAPPDATA%\\jcode\\bin\\jcode.exe` for the launcher, `%LOCALAPPDATA%\\jcode\\builds\\stable\\jcode.exe` for stable, and `%LOCALAPPDATA%\\jcode\\builds\\versions\\<version>\\jcode.exe` for immutable installs; `scripts/install.ps1` currently installs the stable channel.
- Ensure `~/.local/bin` is **before** `~/.cargo/bin` in `PATH`.

## Verifying a change at runtime

`cargo build` alone proves nothing about behavior. `jcode run` and interactive
sessions are served by the long-lived daemon at
`~/.jcode/builds/shared-server/jcode`, which is a symlink into
`~/.jcode/builds/versions/<version>/`. Until that symlink is repointed and the
daemon restarted (`jcode self-dev --build`), a freshly built binary is inert and
every runtime check silently measures the old code.

To test a change without disturbing the shared daemon or the caller's session,
run your build against its own socket:

```bash
cargo build --profile selfdev
./target/selfdev/jcode run --no-update --socket /run/user/1000/jcode-mytest.sock '<prompt>'
```

Two things that waste time otherwise:

- `crate::logging::info` writes to a log file, not stderr, so instrumenting a
  code path with it produces no visible output under `--trace`. Use `eprintln!`
  for throwaway diagnostics and delete it before committing.
- Confirm which binary you are actually inspecting. `strings` on
  `builds/shared-server/jcode` reads a 70-byte symlink, not a program; resolve it
  with `readlink -f` first.

## Running clippy

Run clippy inside the `full` dev shell, not the default `selfdev` one:

```bash
nix develop .#full --command cargo clippy -p <crate> --lib
```

In `selfdev`, `cargo clippy` produces a flood of errors that look alarming and
have nothing to do with your change:

```
error[E0514]: found crate `std` compiled by an incompatible version of rustc
error: cannot find macro `format` in this scope
```

That is a toolchain mismatch, not a code problem. `selfdev` deliberately excludes
`developerTools` (where the flake puts nightly clippy), so `clippy-driver` falls
through to the system one on `/run/current-system/sw/bin` — currently 1.97.1 —
while `rustc` and every artifact in `target/` come from the flake's nightly,
currently 1.99.0. Clippy then refuses to read metadata another compiler wrote.

`nix develop .#full` puts the matching `clippy-preview` on `PATH`, so versions
line up and the same crate lints cleanly. Check with `cargo clippy --version`: it
must match `rustc --version`.

Do not "fix" this with `cargo clean`. The mismatch is in which driver is on
`PATH`, not in `target/`, so a clean costs a full rebuild and changes nothing.

`scripts/check_guardrails.sh` checks this before it runs clippy and skips the
lint with an explanation rather than printing the flood, so the usual way to
meet this is a one-line gate failure rather than several hundred errors.
