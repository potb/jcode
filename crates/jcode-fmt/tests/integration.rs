//! Integration tests for `jcode-fmt` using tiny shell-script "fake
//! formatters" written to a tempdir per test.
//!
//! The registry + config are process-global, and every test in this binary
//! shares that process. Each test gets its own custom formatter id +
//! extension so registry/evidence-cache keys never collide, mirroring
//! `jcode-lsp`'s integration test isolation trick: `configure()` replaces
//! the whole process-global config, so a shared "configure everything"
//! helper avoids tests stepping on each other's formatter definitions.

use std::time::Instant;

use jcode_fmt::config_compat::{FormatterConfig, FormatterServerConfig};

/// The registry's config is process-global; serialize tests in this binary
/// so `configure()` calls from concurrent test threads don't race each
/// other (cargo runs `#[tokio::test]` fns in parallel by default).
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Write an executable shell script to `dir/name` and return its path.
fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    path
}

/// Uppercases the file content in place. `$1` is the file path (the last
/// arg, since our command template is `[script, "$FILE"]`).
const UPPERCASE_SCRIPT: &str = "#!/bin/sh\ntr a-z A-Z < \"$1\" > \"$1.tmp\" && mv \"$1.tmp\" \"$1\"\n";

/// Sleeps far longer than the 5s exec timeout.
const SLEEP_SCRIPT: &str = "#!/bin/sh\nsleep 10\n";

/// Always fails.
const FAIL_SCRIPT: &str = "#!/bin/sh\nexit 1\n";

/// All scenario formatters defined at once. `configure()` replaces the whole
/// process-global config, so every test must configure the full set.
fn full_test_config(scripts_dir: &std::path::Path) -> FormatterConfig {
    let mut servers = std::collections::HashMap::new();
    servers.insert(
        "fake".to_string(),
        FormatterServerConfig {
            command: Some(vec![
                scripts_dir.join("uppercase.sh").to_string_lossy().into_owned(),
                "$FILE".to_string(),
            ]),
            extensions: Some(vec!["fkfmt".to_string()]),
            ..Default::default()
        },
    );
    servers.insert(
        "fake-sleep".to_string(),
        FormatterServerConfig {
            command: Some(vec![
                scripts_dir.join("sleep.sh").to_string_lossy().into_owned(),
                "$FILE".to_string(),
            ]),
            extensions: Some(vec!["fksleep".to_string()]),
            ..Default::default()
        },
    );
    servers.insert(
        "fake-fail".to_string(),
        FormatterServerConfig {
            command: Some(vec![
                scripts_dir.join("fail.sh").to_string_lossy().into_owned(),
                "$FILE".to_string(),
            ]),
            extensions: Some(vec!["fkfail".to_string()]),
            ..Default::default()
        },
    );
    FormatterConfig {
        enabled: true,
        servers,
    }
}

fn setup_scripts() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write_script(dir.path(), "uppercase.sh", UPPERCASE_SCRIPT);
    write_script(dir.path(), "sleep.sh", SLEEP_SCRIPT);
    write_script(dir.path(), "fail.sh", FAIL_SCRIPT);
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn custom_formatter_uppercases_file_and_returns_notice() {
    let _guard = lock();
    let scripts = setup_scripts();
    jcode_fmt::configure(full_test_config(scripts.path()));

    let workdir = tempfile::tempdir().expect("tempdir");
    let file = workdir.path().join("foo.fkfmt");
    std::fs::write(&file, "hello world\n").expect("write file");

    let notice = jcode_fmt::format_file(&file).await;
    assert_eq!(notice.as_deref(), Some("formatted with fake"));

    let on_disk = std::fs::read_to_string(&file).expect("read formatted file");
    assert_eq!(on_disk, "HELLO WORLD\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn sleeping_formatter_times_out_and_returns_none() {
    let _guard = lock();
    let scripts = setup_scripts();
    jcode_fmt::configure(full_test_config(scripts.path()));

    let workdir = tempfile::tempdir().expect("tempdir");
    let file = workdir.path().join("foo.fksleep");
    std::fs::write(&file, "hello world\n").expect("write file");

    let start = Instant::now();
    let notice = jcode_fmt::format_file(&file).await;
    let elapsed = start.elapsed();

    assert!(notice.is_none(), "expected None, got {notice:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(7),
        "sleeping formatter took too long: {elapsed:?}"
    );
    // File is untouched since the script never got to run tr/mv before the
    // timeout killed it.
    let on_disk = std::fs::read_to_string(&file).expect("read file");
    assert_eq!(on_disk, "hello world\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_formatter_returns_none_and_leaves_file_untouched() {
    let _guard = lock();
    let scripts = setup_scripts();
    jcode_fmt::configure(full_test_config(scripts.path()));

    let workdir = tempfile::tempdir().expect("tempdir");
    let file = workdir.path().join("foo.fkfail");
    std::fs::write(&file, "hello world\n").expect("write file");

    let notice = jcode_fmt::format_file(&file).await;
    assert!(notice.is_none(), "expected None, got {notice:?}");

    let on_disk = std::fs::read_to_string(&file).expect("read file");
    assert_eq!(on_disk, "hello world\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_master_switch_skips_formatting() {
    let _guard = lock();
    let scripts = setup_scripts();
    let mut cfg = full_test_config(scripts.path());
    cfg.enabled = false;
    jcode_fmt::configure(cfg);

    let workdir = tempfile::tempdir().expect("tempdir");
    let file = workdir.path().join("foo.fkfmt");
    std::fs::write(&file, "hello world\n").expect("write file");

    assert!(jcode_fmt::format_file(&file).await.is_none());
    assert!(!jcode_fmt::is_enabled());
    let on_disk = std::fs::read_to_string(&file).expect("read file");
    assert_eq!(on_disk, "hello world\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_matching_extension_returns_none() {
    let _guard = lock();
    let scripts = setup_scripts();
    jcode_fmt::configure(full_test_config(scripts.path()));

    let workdir = tempfile::tempdir().expect("tempdir");
    let file = workdir.path().join("foo.unknownext");
    std::fs::write(&file, "hello world\n").expect("write file");

    assert!(jcode_fmt::format_file(&file).await.is_none());
}
