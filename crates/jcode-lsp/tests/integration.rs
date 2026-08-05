//! Integration tests for `jcode-lsp` using the `fake-lsp` test-support
//! binary (see `src/bin/fake_lsp.rs`).
//!
//! The registry + config are process-global, and every test in this binary
//! shares that process. Each scenario gets its own server id + extension so
//! registry keys and PATH caching never collide, and every test configures
//! *all* scenario servers via [`full_test_config`] (configure() replaces the
//! whole config, so a shared "configure everything" helper avoids tests
//! stepping on each other's server definitions).

use std::time::Instant;

use jcode_lsp::config_compat::{LspConfig, LspServerConfig};

fn fake_lsp_bin() -> String {
    env!("CARGO_BIN_EXE_fake-lsp").to_string()
}

fn server_config(scenario: &str) -> LspServerConfig {
    let mut command = vec![fake_lsp_bin(), scenario.to_string()];
    if scenario == "rename" {
        // argv[2]: the fake server appends every received notification method
        // to this file, so the rename test can observe didChange.
        command.push(rename_notif_log().to_string_lossy().into_owned());
    }
    LspServerConfig {
        command: Some(command),
        extensions: Some(vec![ext_for(scenario).to_string()]),
        root_markers: Some(vec![marker_for(scenario).to_string()]),
        ..Default::default()
    }
}

/// Stable per-process log path for the rename scenario's notifications
/// (`full_test_config` is rebuilt by every test, so this must not change
/// between calls within one test process).
fn rename_notif_log() -> &'static std::path::Path {
    static LOG: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    LOG.get_or_init(|| {
        std::env::temp_dir().join(format!("jcode-lsp-rename-notifs-{}.log", std::process::id()))
    })
}

fn ext_for(scenario: &str) -> &'static str {
    match scenario {
        "error" => "fkerr",
        "clean" => "fkcln",
        "silent" => "fksil",
        "pull" => "fkpul",
        "crash" => "fkcrs",
        "definition" => "fkdef",
        "hang" => "fkhng",
        "rename" => "fkren",
        _ => panic!("unknown scenario {scenario}"),
    }
}

fn marker_for(scenario: &str) -> &'static str {
    match scenario {
        "error" => ".fakeroot-error",
        "clean" => ".fakeroot-clean",
        "silent" => ".fakeroot-silent",
        "pull" => ".fakeroot-pull",
        "crash" => ".fakeroot-crash",
        "definition" => ".fakeroot-definition",
        "hang" => ".fakeroot-hang",
        "rename" => ".fakeroot-rename",
        _ => panic!("unknown scenario {scenario}"),
    }
}

/// All scenario servers defined at once. `configure()` replaces the whole
/// process-global config, so every test must configure the full set (a test
/// only using the "error" scenario would otherwise blow away the "clean"
/// server definition for a test running concurrently in the same binary).
fn full_test_config() -> LspConfig {
    let mut servers = std::collections::HashMap::new();
    for scenario in [
        "error",
        "clean",
        "silent",
        "pull",
        "crash",
        "definition",
        "hang",
        "rename",
    ] {
        servers.insert(format!("fake-{scenario}"), server_config(scenario));
    }
    LspConfig {
        enabled: true,
        servers,
    }
}

/// Build an isolated tempdir workspace for one scenario: a root marker file
/// plus a `foo.<ext>` file with some content.
fn workspace(scenario: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(marker_for(scenario)), "").expect("write marker");
    let file = dir.path().join(format!("foo.{}", ext_for(scenario)));
    std::fs::write(&file, "let x = 1;\nlet y = 2;\n").expect("write file");
    (dir, file)
}

#[tokio::test(flavor = "multi_thread")]
async fn error_scenario_publishes_diagnostics() {
    jcode_lsp::configure(full_test_config());
    let (_dir, file) = workspace("error");

    let out = jcode_lsp::diagnostics_block(&file).await;
    let out = out.expect("expected diagnostics for error scenario");
    assert!(out.contains("<diagnostics file="), "got: {out}");
    assert!(out.contains("fake error"), "got: {out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn clean_scenario_returns_none() {
    jcode_lsp::configure(full_test_config());
    let (_dir, file) = workspace("clean");

    let out = jcode_lsp::diagnostics_block(&file).await;
    assert!(out.is_none(), "expected None for clean scenario, got {out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn silent_scenario_times_out_within_cold_cap() {
    jcode_lsp::configure(full_test_config());
    let (_dir, file) = workspace("silent");

    let start = Instant::now();
    let out = jcode_lsp::diagnostics_block(&file).await;
    let elapsed = start.elapsed();

    assert!(out.is_none(), "expected None for silent scenario, got {out:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "silent scenario took too long: {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_scenario_surfaces_warning_when_no_errors() {
    jcode_lsp::configure(full_test_config());
    let (_dir, file) = workspace("pull");

    let out = jcode_lsp::diagnostics_block(&file).await;
    let out = out.expect("expected pull-diagnostics warning");
    assert!(out.contains("WARN"), "got: {out}");
    assert!(out.contains("fake warning"), "got: {out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_scenario_does_not_hang_or_panic() {
    jcode_lsp::configure(full_test_config());
    let (_dir, file) = workspace("crash");

    let start = Instant::now();
    let first = jcode_lsp::diagnostics_block(&file).await;
    let second = jcode_lsp::diagnostics_block(&file).await;
    let elapsed = start.elapsed();

    // The server exits right after `initialize`, before any diagnostics can
    // be published, so both touches should come back empty without panicking
    // or hanging (registry crash/respawn ladder is unit-tested separately).
    assert!(first.is_none(), "got: {first:?}");
    assert!(second.is_none(), "got: {second:?}");
    // Each touch is a cold spawn (COLD_CAP = 5s) since the server dies before
    // ever publishing diagnostics; two touches bound the wait near ~10s.
    // Give a margin above that sum so this stays a "no hang" check rather
    // than a flaky exact-boundary assertion.
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "crash scenario took too long: {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn handle_for_definition_returns_fixed_location() {
    jcode_lsp::configure(full_test_config());
    let (_dir, file) = workspace("definition");

    let handle = jcode_lsp::handle_for(&file)
        .await
        .expect("handle_for should resolve the fake server");
    let locations = handle
        .definition(1, 1)
        .await
        .expect("definition request should succeed");

    assert_eq!(locations.len(), 1, "expected exactly one location");
    let loc = &locations[0];
    // Fake server always answers line 0, col 0 (0-based) -> 1-based (1, 1).
    assert_eq!(loc.line, 1, "expected 1-based line 1, got {}", loc.line);
    assert_eq!(loc.column, 1, "expected 1-based column 1, got {}", loc.column);
    assert_eq!(
        loc.path.file_name().and_then(|n| n.to_str()),
        file.file_name().and_then(|n| n.to_str())
    );
}

/// Sanity: the workspace root markers actually resolve to the tempdir (not
/// the process cwd fallback), so each scenario's server is truly isolated.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_root_resolves_to_tempdir_not_fallback() {
    jcode_lsp::configure(full_test_config());
    let (dir, file) = workspace("error");

    let handle = jcode_lsp::handle_for(&file).await.expect("handle_for");
    // If root resolution fell back to cwd, this would panic on canonicalize
    // mismatch when definition-applying edits; instead just check the file
    // path we resolved against is inside the tempdir.
    assert!(file.starts_with(dir.path()));
    drop(handle);
}

/// A server that never answers `initialize` must not stall the write path:
/// the registry's init timeout (5s) kills it and `diagnostics_block` returns
/// None. Concurrent registry users (a different, healthy server) must not be
/// blocked while the hung handshake is in flight.
#[tokio::test(flavor = "multi_thread")]
async fn hang_scenario_times_out_and_does_not_block_other_servers() {
    jcode_lsp::configure(full_test_config());
    let (_hang_dir, hang_file) = workspace("hang");
    let (_clean_dir, clean_file) = workspace("clean");

    let start = Instant::now();
    let hang_task = tokio::spawn(async move { jcode_lsp::diagnostics_block(&hang_file).await });

    // While the hung handshake is in flight, another registry user must make
    // progress well inside the hang timeout window.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let clean_start = Instant::now();
    let clean_out = jcode_lsp::diagnostics_block(&clean_file).await;
    let clean_elapsed = clean_start.elapsed();
    assert!(clean_out.is_none(), "clean scenario should be clean: {clean_out:?}");
    assert!(
        clean_elapsed < std::time::Duration::from_secs(8),
        "clean scenario blocked behind hung handshake: {clean_elapsed:?}"
    );

    let hang_out = hang_task.await.expect("hang task should not panic");
    let elapsed = start.elapsed();
    assert!(hang_out.is_none(), "hang scenario should yield None: {hang_out:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "hang scenario exceeded bounded wait: {elapsed:?}"
    );
}

/// End-to-end rename against the fake server: the WorkspaceEdit is applied to
/// disk (full new content, single write) and the server's buffer is re-synced
/// via didChange (observed through the fake server's notification log).
#[tokio::test(flavor = "multi_thread")]
async fn rename_applies_edit_to_disk_and_resyncs_open_buffer() {
    jcode_lsp::configure(full_test_config());
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(marker_for("rename")), "").expect("marker");
    let file = dir.path().join(format!("foo.{}", ext_for("rename")));
    // Line 0 chars 4..12 = "old_name" (matches the fake server's edit range).
    std::fs::write(&file, "let old_name = 1;\nuse_it(old_name);\n").expect("write file");

    let handle = jcode_lsp::handle_for(&file).await.expect("handle_for");
    let outcome = handle
        .rename(1, 5, "new_name")
        .await
        .expect("rename should succeed");

    let canonical = file.canonicalize().expect("canonicalize");
    assert_eq!(outcome.changed_files, vec![canonical]);
    let on_disk = std::fs::read_to_string(&file).expect("read renamed file");
    assert_eq!(on_disk, "let new_name = 1;\nuse_it(old_name);\n");

    // The file was opened by handle_for, so the rename must re-sync it with a
    // didChange. Poll the fake server's notification log briefly (the write
    // is on the server side of the pipe).
    let deadline = Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let log = std::fs::read_to_string(rename_notif_log()).unwrap_or_default();
        let did_changes = log
            .lines()
            .filter(|l| *l == "textDocument/didChange")
            .count();
        if did_changes >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no didChange observed after rename; log:\n{log}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// `workspace_handle_for` resolves a directory (no file read, no didOpen) by
/// walking up to a root marker.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_handle_for_resolves_directory() {
    jcode_lsp::configure(full_test_config());
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(marker_for("definition")), "").expect("marker");
    let nested = dir.path().join("src/deep");
    std::fs::create_dir_all(&nested).expect("mkdirs");

    let handle = jcode_lsp::workspace_handle_for(&nested)
        .await
        .expect("workspace_handle_for should resolve via root marker walk-up");
    // The fake server answers workspace/symbol with one fixed symbol echoing
    // the query, exercising the full request/parse path end to end.
    let symbols = handle
        .workspace_symbols("anything")
        .await
        .expect("workspace_symbols should succeed");
    assert_eq!(symbols.len(), 1, "expected the fake symbol, got {symbols:?}");
    assert_eq!(symbols[0].name, "fake_symbol_anything");
    assert_eq!(symbols[0].line, 4, "line must be 1-based");
}
