use super::*;

#[cfg(unix)]
#[test]
fn harden_secret_file_permissions_sets_owner_only_modes() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().expect("create temp dir");
    let secret_dir = dir.path().join("jcode");
    std::fs::create_dir_all(&secret_dir).expect("create secret dir");

    let secret_file = secret_dir.join("openrouter.env");
    std::fs::write(&secret_file, "OPENROUTER_API_KEY=sk-or-v1-test\n").expect("write secret file");

    std::fs::set_permissions(&secret_dir, std::fs::Permissions::from_mode(0o755))
        .expect("set initial dir perms");
    std::fs::set_permissions(&secret_file, std::fs::Permissions::from_mode(0o644))
        .expect("set initial file perms");

    harden_secret_file_permissions(&secret_file);

    let dir_mode = std::fs::metadata(&secret_dir)
        .expect("stat dir")
        .permissions()
        .mode()
        & 0o777;
    let file_mode = std::fs::metadata(&secret_file)
        .expect("stat file")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(dir_mode, 0o700);
    assert_eq!(file_mode, 0o600);
}

#[test]
fn user_home_path_uses_external_dir_under_jcode_home() {
    let _guard = lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let temp = tempfile::TempDir::new().expect("create temp dir");
    crate::env::set_var("JCODE_HOME", temp.path());

    let resolved = user_home_path(".codex/auth.json").expect("resolve user home path");
    assert_eq!(
        resolved,
        temp.path()
            .join("external")
            .join(".codex")
            .join("auth.json")
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[cfg(unix)]
#[test]
fn validate_external_auth_file_rejects_symlink() {
    use std::os::unix::fs as unix_fs;

    let dir = tempfile::TempDir::new().expect("create temp dir");
    let target = dir.path().join("auth.json");
    let link = dir.path().join("auth-link.json");
    std::fs::write(&target, "{}\n").expect("write target");
    unix_fs::symlink(&target, &link).expect("create symlink");

    let err = validate_external_auth_file(&link).expect_err("symlink should be rejected");
    assert!(err.to_string().contains("symlink"));
}

#[test]
fn app_config_dir_uses_jcode_home_when_set() {
    let _guard = lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let temp = tempfile::TempDir::new().expect("create temp dir");
    crate::env::set_var("JCODE_HOME", temp.path());

    let resolved = app_config_dir().expect("resolve app config dir");
    assert_eq!(resolved, temp.path().join("config").join("jcode"));

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn upsert_env_file_value_writes_replaces_and_removes_entries() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let file = dir.path().join("test.env");

    upsert_env_file_value(&file, "API_KEY", Some("one")).expect("write initial env value");
    assert_eq!(
        std::fs::read_to_string(&file).expect("read env file"),
        "API_KEY=one\n"
    );

    upsert_env_file_value(&file, "OTHER", Some("two")).expect("append second value");
    upsert_env_file_value(&file, "API_KEY", Some("updated")).expect("replace existing value");
    assert_eq!(
        std::fs::read_to_string(&file).expect("read env file after replace"),
        "API_KEY=updated\nOTHER=two\n"
    );

    upsert_env_file_value(&file, "API_KEY", None).expect("remove env value");
    assert_eq!(
        std::fs::read_to_string(&file).expect("read env file after remove"),
        "OTHER=two\n"
    );
}

#[cfg(unix)]
#[test]
fn write_text_secret_sets_owner_only_modes() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().expect("create temp dir");
    let file = dir.path().join("secret.env");

    write_text_secret(&file, "SECRET=value\n").expect("write secret text");

    let dir_mode = std::fs::metadata(dir.path())
        .expect("stat dir")
        .permissions()
        .mode()
        & 0o777;
    let file_mode = std::fs::metadata(&file)
        .expect("stat file")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(dir_mode, 0o700);
    assert_eq!(file_mode, 0o600);
}

/// The test-env lock is reentrant per thread.
///
/// Before this, `lock_test_env` returned a raw `MutexGuard` over a plain
/// non-reentrant `Mutex`, so any helper that wanted the lock had to know
/// whether a caller already held it. Two call sites worked around that with
/// `try_lock` and with comments saying "do not take this lock here", and the
/// resulting env-vs-render lock order was left to each test to get right. That
/// is what deadlocked `cargo test -p jcode-tui --lib` (issue #27).
#[test]
fn test_env_lock_is_reentrant_on_one_thread() {
    let outer = lock_test_env();
    assert!(test_env_lock_held());
    {
        let _inner = lock_test_env();
        assert!(test_env_lock_held());
    }
    // Still held after the nested guard drops: only the outermost releases.
    assert!(test_env_lock_held());
    drop(outer);
    assert!(!test_env_lock_held());
}

/// The lock still excludes other threads. Reentrancy must not degrade into
/// "everyone gets in", or the env serialization it exists for is gone.
#[test]
fn test_env_lock_still_excludes_other_threads() {
    let guard = lock_test_env();
    let contended = std::thread::spawn(|| test_env_lock().try_lock().is_err())
        .join()
        .expect("thread");
    assert!(contended, "another thread acquired the env lock while held");
    drop(guard);
    assert!(!test_env_lock_held());
}
