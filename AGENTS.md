# Repository Guidelines

## Development Workflow

- **Stay on your own branch** - Do not take, cherry-pick, merge, or copy code from other
  people's or other agents' branches unless the source branch belongs to a repository
  maintainer and the user explicitly asks you to integrate it. Only work from your branch
  and its base (e.g. `main`) otherwise. Never integrate branches owned by non-maintainers
  or other agents yourself; tell the user and let them decide how to proceed.

- **Comments explain why, not what.** Doc comments on `pub` items are expected. Do not
  leave memo comments describing what you changed; git history covers that. The write and
  edit tools report a file's non-doc comments back to you as advice, never as a block.

## Install Notes
- `~/.local/bin/jcode` is the launcher symlink used from `PATH`.
- `~/.jcode/builds/current/jcode` is the active local/source-build channel; self-dev builds and `scripts/install_release.sh` point the launcher here.
- `~/.jcode/builds/stable/jcode` is the stable release channel; `scripts/install.sh` installs this and points the launcher here.
- `~/.jcode/builds/versions/<version>/jcode` stores immutable binaries.
- `~/.jcode/builds/canary/jcode` still exists for canary/testing flows, but it is not the primary self-dev install path.
- On Windows, the equivalents are `%LOCALAPPDATA%\\jcode\\bin\\jcode.exe` for the launcher, `%LOCALAPPDATA%\\jcode\\builds\\stable\\jcode.exe` for stable, and `%LOCALAPPDATA%\\jcode\\builds\\versions\\<version>\\jcode.exe` for immutable installs; `scripts/install.ps1` currently installs the stable channel.
- Ensure `~/.local/bin` is **before** `~/.cargo/bin` in `PATH`.

## `--lib` is not a suite check

`cargo test --workspace --lib` is the fast health scan, and it is easy to
mistake for "the tests pass". It is not: `--lib` builds only the library target
of each crate. It never builds the `tests/` directory, so an integration binary
that does not even **compile** is invisible to it, and reports green.

That is not hypothetical. `cef23e116` added a required field to
`ScheduledItem` and updated the 12 struct literals in the crate's own unit
tests, but missed three in `tests/e2e/ambient.rs`. The `e2e` target failed with
`E0063` for four days, its 64 tests never ran, and every `--lib` scan in that
window passed.

So before claiming a change is clean, compile the other target kinds:

```bash
cargo test --workspace --tests --no-run   # integration binaries compile
cargo test --test e2e                     # and the e2e target actually runs
```

`cargo check --all-targets` (what `ci.yml` runs) catches the compile half too.
The rule of thumb: **adding or removing a struct field, an enum variant, or a
trait method can break a target kind nobody in the loop compiles.** After any
such change, run `--tests` at least once rather than trusting `--lib`.

When you report suite health, state the scope you measured. "`-p jcode-tui
--lib` is green" is a claim about 1 of 86 crates, not about the suite. It is
also, as of `56745ff7a`, not reliably true, for two unrelated reasons that are
easy to conflate:

- **Order dependence** (#208). Some tests pass alone and fail in a full run.
  `test_background_task_markdown_is_suppressed_even_if_role_was_lost` is one,
  and it is worth knowing how it resolved: the code under test was never
  broken. The assertion `!text.contains("╭")` was written to mean "no message
  card rendered" but matched *any* rounded border in the buffer, including the
  info side cards other tests cause to be drawn. Before concluding a global
  leaked, print what the test actually saw.
- **Nondeterminism** (#210). Under parallelism the failing set varies between
  runs on an untouched tree (6, then 5, then 6), so a green result there may
  only mean you got a lucky interleaving.

Re-run a failure serially with `-- --test-threads=1` before believing either
outcome, and do not treat that crate as a clean health signal today.

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
