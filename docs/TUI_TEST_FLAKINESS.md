# jcode-tui test flakiness: root cause

**Status: the root cause below is fixed. See "What was fixed" for the current
shape of the code, and "Still open" for what remains.** The diagnosis is kept
because the same failure mode recurs whenever a test reaches for a process
global.

`cargo test -p jcode-tui --lib` failed 1-4 tests per run, with a varying set.
This was a parallelism race on process-global state, not a logic bug.

## Evidence

- `cargo test -p jcode-tui --lib -- --test-threads=1` passes **2006/2006** (16 ignored).
- The failing set changes between runs at the default thread count.
- Individually, each failing test passes when run alone.

Counts were taken on 2026-07-27 and will drift as tests are added. Reproduce
on an otherwise idle machine: under memory pressure (this host has 15 GiB and
was running concurrent workspace builds) `cargo` gets SIGTERMed mid-compile,
which is a different failure from the race described here.

## Root cause

`create_test_app()` (and its `create_named_provider_test_app` sibling) in
`crates/jcode-tui/src/tui/app/tests/support_failover/part_01.rs` calls:

```rust
crate::tui::ui::clear_test_render_state_for_tests();
```

That wipes **process-global** render state: the flicker frame history, layout
snapshots, status-area snapshots, copy targets, and scroll positions.

Rendering tests guard exactly that state with `render_state_test_lock()`. But
`create_test_app` cleared it *without* taking the lock, so any of its ~810 call
sites could reset a concurrently-running render test's state mid-assertion.

The mechanism for the most frequent victim
(`test_changelog_overlay_repeated_renders_are_stable`) is documented in
`clear_test_render_state_for_tests` itself: a recorded flicker event adds a
"⚠ flicker detected" notification line to later renders, shifting every
layout-sensitive assertion by a row.

### Bisected proof

Bisecting the 959 `tui::app::tests::` tests against the changelog test
identifies `test_tui_login_providers_have_real_tui_handlers`, which calls
`create_test_app()` in a loop (once per login provider). Running just those two
does not reproduce; the race needs enough concurrent load to interleave, which
is why it presents as order-dependent flakiness.

## What does not work

**Taking `render_state_test_lock` inside `create_test_app`.** This is correct
but serializes all ~810 call sites: suite runtime goes from ~12s to over 10
minutes. Measured, then reverted.

**Asserting a floor instead of an exact count** in the changelog test's
`buffered_samples` check, and **calling `clear_test_render_state_for_tests`**
at the top of that test. Both measured over 5 runs: the test still failed 5/5
with *and* without the change. Reverted rather than committed as churn.

## What was fixed

The state described above is historical: the shared state was removed rather
than serialized. As of `99b0a8adb`:

1. **The flicker history is thread-local under `cfg(test)`.**
   `flicker_frame_history()` in `crates/jcode-tui/src/tui/ui_frame_metrics.rs`
   returns a per-thread `Box::leak`ed `Mutex` when compiled for tests, and the
   process-global `OnceLock` otherwise. Production still has one render thread,
   so runtime behavior is unchanged. This removes the shared mutable state
   instead of adding coordination around it.

2. **The remaining render globals are guarded reentrantly.**
   `clear_test_render_state_for_tests` (`ui.rs`) routes through
   `with_render_state_lock`, which takes `render_state_test_lock()` only if the
   current thread does not already hold it. Ownership is tracked in the
   `RENDER_STATE_LOCK_HELD` thread-local set by `RenderStateTestGuard`, so
   nested calls become no-ops rather than blocking. That is what makes the
   clear safe from `create_test_app`'s ~900 call sites.

The deadlock hazard is real and is documented at both sites. `create_test_app`
must **not** take `render_state_test_lock()` explicitly: the mutex is not
reentrant, and tests that hold the lock and then build an app (the
pinned-todo-band render test, for one) hung the CI TUI step at its 35-minute
job timeout.

The sibling env-var problem got the same treatment:
`ensure_test_jcode_home_if_unset` (`tests/support_failover/part_01.rs`)
serializes the unset-to-set transition with a `try_lock` on the env lock,
degrading to unserialized rather than deadlocking when a caller already holds
it.

## Still open

Test isolation is better, not finished. `JCODE_HOME` is a process-global env
var, and the older per-PID home in `app/remote_tests.rs:61`
(`temp_dir()/jcode-test-home-<pid>`) is shared by every test in the process
while concurrent temp-home tests may delete it. Tests that mutate the
environment should hold `lock_test_env()` across the whole
save-mutate-restore window, not just part of it; fixes have landed along
exactly those lines (`a34162559`, `f0edf6294`, `6e961bf6b`, `6d8633503`).

Two caveats for anyone chasing a residual flake here:

- **Do not assume `JCODE_HOME` is the cause.** Pinning one stable `JCODE_HOME`
  for a whole run was measured and made failures *worse* (1-3 per run,
  spreading into unrelated suites): a shared home introduces its own cross-test
  contamination.
- **Confirm the reproduction first.** Some tests named in passing failure
  output do not reproduce over repeated full parallel runs, and wrapping those
  in a temp-home helper fixes nothing.

## Scope note

This is pre-existing and independent of the render-path performance work in
commits `0ba0154c6`, `2b8e78e34`, `8b44fc83b`, `8142f1a0b`. Verified by
stashing those changes and reproducing the same failure rate.
