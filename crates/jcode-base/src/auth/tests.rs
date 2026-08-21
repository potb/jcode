use super::*;
use std::ffi::OsString;

fn restore_env_var(key: &str, previous: Option<OsString>) {
    if let Some(previous) = previous {
        crate::env::set_var(key, previous);
    } else {
        crate::env::remove_var(key);
    }
}

#[test]
fn command_candidates_adds_extension_on_windows() {
    crate::env::set_var("PATHEXT", ".EXE;.BAT");
    let candidates = command_candidates("testcmd");
    if cfg!(windows) {
        let normalized: Vec<String> = candidates
            .iter()
            .map(|c| c.to_string_lossy().to_ascii_lowercase())
            .collect();
        assert!(normalized.iter().any(|c| c == "testcmd"));
        assert!(normalized.iter().any(|c| c == "testcmd.exe"));
        assert!(normalized.iter().any(|c| c == "testcmd.bat"));
    } else {
        assert_eq!(candidates.len(), 1);
        assert!(candidates.iter().any(|c| c == "testcmd"));
    }
}

#[test]
fn auth_state_default_is_not_configured() {
    let state = AuthState::default();
    assert_eq!(state, AuthState::NotConfigured);
}

#[test]
fn auth_status_default_all_not_configured() {
    let status = AuthStatus::default();
    assert_eq!(status.anthropic.state, AuthState::NotConfigured);
    assert_eq!(status.openrouter, AuthState::NotConfigured);
    assert_eq!(status.openai, AuthState::NotConfigured);
    assert_eq!(status.copilot, AuthState::NotConfigured);
    assert_eq!(status.cursor, AuthState::NotConfigured);
    assert_eq!(status.antigravity, AuthState::NotConfigured);
    assert!(!status.openai_has_oauth);
    assert!(!status.openai_has_api_key);
    assert!(!status.copilot_has_api_token);
    assert!(!status.anthropic.has_oauth);
    assert!(!status.anthropic.has_api_key);
}

#[test]
fn auth_status_check_fast_includes_bedrock_probe() {
    let _lock = crate::storage::lock_test_env();
    let prev_bedrock_enable = std::env::var_os("JCODE_BEDROCK_ENABLE");

    crate::env::set_var("JCODE_BEDROCK_ENABLE", "1");
    AuthStatus::invalidate_cache();

    let status = AuthStatus::check_fast();
    assert_eq!(status.bedrock, AuthState::Available);

    restore_env_var("JCODE_BEDROCK_ENABLE", prev_bedrock_enable);
    AuthStatus::invalidate_cache();
}

#[test]
fn full_and_fast_auth_status_match_for_shared_probe_fields() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    std::fs::create_dir_all(&home).expect("create temp home");
    std::fs::create_dir_all(&xdg).expect("create temp xdg config");
    let saved = [
        "JCODE_HOME",
        "XDG_CONFIG_HOME",
        "HOME",
        crate::subscription_catalog::JCODE_API_KEY_ENV,
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "JCODE_OPENROUTER_API_BASE",
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENROUTER_ENV_FILE",
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "JCODE_OPENROUTER_PROVIDER_FEATURES",
        "JCODE_OPENROUTER_TRANSPORT_STATE",
        "JCODE_OPENROUTER_ALLOW_NO_AUTH",
        "JCODE_OPENROUTER_MODEL_CATALOG",
        "JCODE_OPENROUTER_STATIC_MODELS",
        "JCODE_OPENROUTER_MODEL",
        "JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        crate::auth::azure::ENDPOINT_ENV,
        crate::auth::azure::API_KEY_ENV,
        crate::auth::azure::MODEL_ENV,
        crate::auth::azure::USE_ENTRA_ENV,
        "JCODE_BEDROCK_ENABLE",
        "COPILOT_GITHUB_TOKEN",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "CURSOR_API_KEY",
        "CURSOR_ACCESS_TOKEN",
        "CURSOR_REFRESH_TOKEN",
    ]
    .into_iter()
    .map(|key| (key, std::env::var_os(key)))
    .collect::<Vec<_>>();

    crate::env::set_var("JCODE_HOME", temp.path().join("jcode-home"));
    crate::env::set_var("XDG_CONFIG_HOME", &xdg);
    crate::env::set_var("HOME", &home);
    crate::env::set_var(
        crate::subscription_catalog::JCODE_API_KEY_ENV,
        "jcode-test-key",
    );
    crate::env::set_var("ANTHROPIC_API_KEY", "anthropic-test-key");
    crate::env::set_var("OPENAI_API_KEY", "openai-test-key");
    crate::env::set_var("OPENROUTER_API_KEY", "openrouter-test-key");
    for key in [
        "JCODE_OPENROUTER_API_BASE",
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENROUTER_ENV_FILE",
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "JCODE_OPENROUTER_PROVIDER_FEATURES",
        "JCODE_OPENROUTER_TRANSPORT_STATE",
        "JCODE_OPENROUTER_ALLOW_NO_AUTH",
        "JCODE_OPENROUTER_MODEL_CATALOG",
        "JCODE_OPENROUTER_STATIC_MODELS",
        "JCODE_OPENROUTER_MODEL",
        "JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
    ] {
        crate::env::remove_var(key);
    }
    crate::env::set_var(
        crate::auth::azure::ENDPOINT_ENV,
        "https://example.openai.azure.com",
    );
    crate::env::set_var(crate::auth::azure::API_KEY_ENV, "azure-test-key");
    crate::env::set_var(crate::auth::azure::MODEL_ENV, "gpt-test-deployment");
    crate::env::remove_var(crate::auth::azure::USE_ENTRA_ENV);
    crate::env::set_var("JCODE_BEDROCK_ENABLE", "1");
    crate::env::set_var("COPILOT_GITHUB_TOKEN", "gho_test_token");
    crate::env::remove_var("GH_TOKEN");
    crate::env::remove_var("GITHUB_TOKEN");
    crate::env::set_var("CURSOR_API_KEY", "cursor-test-key");
    crate::env::remove_var("CURSOR_ACCESS_TOKEN");
    crate::env::remove_var("CURSOR_REFRESH_TOKEN");
    AuthStatus::invalidate_cache();

    let (full, _) = build_auth_status_uncached(AuthProbeMode::Full);
    let (fast, _) = build_auth_status_uncached(AuthProbeMode::Fast);

    assert_auth_status_shared_fields_match(&full, &fast);
    assert_eq!(full.jcode, AuthState::Available);
    assert_eq!(full.anthropic.state, AuthState::Available);
    assert_eq!(full.openai, AuthState::Available);
    assert_eq!(full.openrouter, AuthState::Available);
    assert_eq!(full.azure, AuthState::Available);
    assert_eq!(full.bedrock, AuthState::Available);
    assert_eq!(full.copilot, AuthState::Available);
    assert_eq!(full.cursor, AuthState::Available);

    for (key, value) in saved {
        restore_env_var(key, value);
    }
    AuthStatus::invalidate_cache();
}

#[cfg(unix)]
#[test]
fn full_and_fast_auth_status_document_cursor_vscdb_exception() {
    let _lock = crate::storage::lock_test_env();
    if !crate::auth::cursor::tests::sqlite3_available() {
        eprintln!("skipping: sqlite3 is not installed");
        return;
    }
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    std::fs::create_dir_all(&home).expect("create temp home");
    std::fs::create_dir_all(&xdg).expect("create temp xdg config");
    let saved = [
        "JCODE_HOME",
        "XDG_CONFIG_HOME",
        "HOME",
        "CURSOR_API_KEY",
        "CURSOR_ACCESS_TOKEN",
        "CURSOR_REFRESH_TOKEN",
    ]
    .into_iter()
    .map(|key| (key, std::env::var_os(key)))
    .collect::<Vec<_>>();

    let jcode_home = temp.path().join("jcode-home");
    crate::env::set_var("JCODE_HOME", &jcode_home);
    let vscdb_dir = jcode_home.join("external/.config/Cursor/User/globalStorage");
    std::fs::create_dir_all(&vscdb_dir).expect("create cursor globalStorage");
    let vscdb = crate::auth::cursor::tests::create_mock_vscdb(
        &vscdb_dir,
        &[("cursorAuth/accessToken", "tok_vscdb_only")],
    );

    crate::env::set_var("XDG_CONFIG_HOME", &xdg);
    crate::env::set_var("HOME", &home);
    crate::env::remove_var("CURSOR_API_KEY");
    crate::env::remove_var("CURSOR_ACCESS_TOKEN");
    crate::env::remove_var("CURSOR_REFRESH_TOKEN");
    crate::config::Config::allow_external_auth_source_for_path(
        crate::auth::cursor::CURSOR_VSCDB_SOURCE_ID,
        &vscdb,
    )
    .expect("trust the temp vscdb");
    AuthStatus::invalidate_cache();

    let (full, _) = build_auth_status_uncached(AuthProbeMode::Full);
    let (fast, _) = build_auth_status_uncached(AuthProbeMode::Fast);

    assert_eq!(
        full.cursor,
        AuthState::Available,
        "Full auth reads Cursor's state.vscdb, the only credential present here"
    );
    assert_eq!(
        fast.cursor,
        AuthState::NotConfigured,
        "Fast auth intentionally skips the vscdb probe to keep UI paths responsive"
    );

    for (key, value) in saved {
        restore_env_var(key, value);
    }
    AuthStatus::invalidate_cache();
}

fn assert_auth_status_shared_fields_match(full: &AuthStatus, fast: &AuthStatus) {
    assert_eq!(full.jcode, fast.jcode, "jcode");
    assert_eq!(
        full.anthropic.state, fast.anthropic.state,
        "anthropic.state"
    );
    assert_eq!(
        full.anthropic.has_oauth, fast.anthropic.has_oauth,
        "anthropic.has_oauth"
    );
    assert_eq!(
        full.anthropic.has_api_key, fast.anthropic.has_api_key,
        "anthropic.has_api_key"
    );
    assert_eq!(full.openrouter, fast.openrouter, "openrouter");
    assert_eq!(full.azure, fast.azure, "azure");
    assert_eq!(
        full.azure_has_api_key, fast.azure_has_api_key,
        "azure api key"
    );
    assert_eq!(full.azure_uses_entra, fast.azure_uses_entra, "azure entra");
    assert_eq!(full.bedrock, fast.bedrock, "bedrock");
    assert_eq!(full.openai, fast.openai, "openai");
    assert_eq!(full.openai_has_oauth, fast.openai_has_oauth, "openai oauth");
    assert_eq!(
        full.openai_has_api_key, fast.openai_has_api_key,
        "openai api key"
    );
    assert_eq!(full.copilot, fast.copilot, "copilot");
    assert_eq!(
        full.copilot_has_api_token, fast.copilot_has_api_token,
        "copilot api token"
    );
    assert_eq!(full.antigravity, fast.antigravity, "antigravity");
    assert_eq!(full.gemini, fast.gemini, "gemini");
    assert_eq!(full.cursor, fast.cursor, "cursor");
    assert_eq!(full.google, fast.google, "google");
    assert_eq!(full.google_can_send, fast.google_can_send, "google send");
}

#[test]
fn provider_auth_default() {
    let auth = ProviderAuth::default();
    assert_eq!(auth.state, AuthState::NotConfigured);
    assert!(!auth.has_oauth);
    assert!(!auth.has_api_key);
}

#[test]
fn provider_auth_assessment_predicates_reflect_state() {
    fn assessment_with_state(state: AuthState) -> ProviderAuthAssessment {
        ProviderAuthAssessment {
            state,
            readiness: AuthReadinessLevel::None,
            method_detail: "test".to_string(),
            credential_source: AuthCredentialSource::None,
            credential_source_detail: "not configured".to_string(),
            expiry_confidence: AuthExpiryConfidence::Unknown,
            refresh_support: AuthRefreshSupport::Unknown,
            validation_method: AuthValidationMethod::Unknown,
            last_validation: None,
            last_refresh: None,
        }
    }

    let not_configured = assessment_with_state(AuthState::NotConfigured);
    assert!(!not_configured.is_configured());
    assert!(!not_configured.is_available());

    let expired = assessment_with_state(AuthState::Expired);
    assert!(expired.is_configured());
    assert!(!expired.is_available());

    let available = assessment_with_state(AuthState::Available);
    assert!(available.is_configured());
    assert!(available.is_available());
}

#[test]
fn command_exists_for_known_binary() {
    if cfg!(windows) {
        assert!(command_exists("cmd") || command_exists("cmd.exe"));
    } else {
        assert!(command_exists("ls"));
    }
}

#[test]
fn command_exists_empty_string() {
    assert!(!command_exists(""));
    assert!(!command_exists("   "));
}

#[test]
fn command_exists_nonexistent() {
    assert!(!command_exists("surely_this_binary_does_not_exist_xyz"));
}

#[test]
fn command_exists_absolute_path() {
    if cfg!(windows) {
        assert!(command_exists(r"C:\Windows\System32\cmd.exe"));
    } else {
        // Distro layouts differ (NixOS has no /bin/ls), so use a path that is
        // guaranteed to exist and be executable: this test binary itself.
        let exe = std::env::current_exe().expect("test binary path");
        assert!(command_exists(&exe.to_string_lossy()));
    }
}

#[test]
fn command_exists_absolute_nonexistent() {
    assert!(!command_exists("/nonexistent/path/to/binary"));
}

#[test]
fn contains_path_separator_detection() {
    assert!(contains_path_separator("/usr/bin/test"));
    assert!(contains_path_separator("./test"));
    assert!(!contains_path_separator("test"));
}

#[test]
fn has_extension_detection() {
    assert!(has_extension(std::path::Path::new("test.exe")));
    assert!(!has_extension(std::path::Path::new("test")));
    assert!(has_extension(std::path::Path::new("test.sh")));
}

#[test]
fn dedup_preserves_order() {
    let input = vec![
        std::ffi::OsString::from("a"),
        std::ffi::OsString::from("b"),
        std::ffi::OsString::from("a"),
        std::ffi::OsString::from("c"),
    ];
    let result = dedup_preserve_order(input);
    assert_eq!(result.len(), 3);
    assert_eq!(result[0], "a");
    assert_eq!(result[1], "b");
    assert_eq!(result[2], "c");
}

#[test]
fn auth_state_equality() {
    assert_eq!(AuthState::Available, AuthState::Available);
    assert_eq!(AuthState::Expired, AuthState::Expired);
    assert_eq!(AuthState::NotConfigured, AuthState::NotConfigured);
    assert_ne!(AuthState::Available, AuthState::Expired);
    assert_ne!(AuthState::Available, AuthState::NotConfigured);
}

#[test]
fn is_wsl2_windows_path_matches_drive_mounts() {
    assert!(is_wsl2_windows_path(std::path::Path::new("/mnt/c")));
    assert!(is_wsl2_windows_path(std::path::Path::new("/mnt/d")));
    assert!(is_wsl2_windows_path(std::path::Path::new("/mnt/z")));
    assert!(is_wsl2_windows_path(std::path::Path::new(
        "/mnt/c/Windows/System32"
    )));
}

#[test]
fn is_wsl2_windows_path_rejects_non_drives() {
    // /mnt/wsl is a WSL-internal mount, not a Windows drive
    assert!(!is_wsl2_windows_path(std::path::Path::new("/mnt/wsl")));
    // /usr/bin is a plain Linux directory
    assert!(!is_wsl2_windows_path(std::path::Path::new("/usr/bin")));
    // /mnt alone is not a drive
    assert!(!is_wsl2_windows_path(std::path::Path::new("/mnt")));
    // empty
    assert!(!is_wsl2_windows_path(std::path::Path::new("")));
}

#[test]
fn command_exists_cached_on_second_call() {
    // Clear cache first to isolate this test
    if let Ok(mut cache) = COMMAND_EXISTS_CACHE.lock() {
        cache.remove("surely_this_binary_does_not_exist_xyz_cache_test");
    }
    // First call populates the cache
    let result1 = command_exists("surely_this_binary_does_not_exist_xyz_cache_test");
    assert!(!result1);
    // Second call must return same result (from cache)
    let result2 = command_exists("surely_this_binary_does_not_exist_xyz_cache_test");
    assert_eq!(result1, result2);
}

#[test]
fn auth_status_check_returns_valid_struct() {
    let status = AuthStatus::check_fast();
    // Just verify it runs without panicking and has coherent state
    match status.anthropic.state {
        AuthState::Available | AuthState::Expired | AuthState::NotConfigured => {}
    }
    match status.openai {
        AuthState::Available | AuthState::Expired | AuthState::NotConfigured => {}
    }
    // If copilot has api token, state should be Available
    if status.copilot_has_api_token {
        assert_eq!(status.copilot, AuthState::Available);
    }
}

#[test]
fn auth_status_check_fast_ignores_expired_full_cache() {
    let _lock = crate::storage::lock_test_env();
    AuthStatus::invalidate_cache();

    let stale_status = AuthStatus {
        jcode: AuthState::Expired,
        ..Default::default()
    };
    let stale_when = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(
            AUTH_STATUS_CACHE_TTL_SECS + 1,
        ))
        .expect("stale cache timestamp");

    *AUTH_STATUS_CACHE.write().expect("auth cache lock") =
        Some((stale_status, stale_when, auth_cache_home_key()));
    *AUTH_STATUS_FAST_CACHE
        .write()
        .expect("fast auth cache lock") = None;

    let status = AuthStatus::check_fast();
    assert_ne!(
        status.jcode,
        AuthState::Expired,
        "check_fast must not reuse an expired full auth cache forever"
    );

    AuthStatus::invalidate_cache();
}

#[test]
fn copilot_recent_token_exchange_failure_is_not_auto_usable() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_copilot_token = std::env::var_os("COPILOT_GITHUB_TOKEN");
    let prev_gh_token = std::env::var_os("GH_TOKEN");
    let prev_github_token = std::env::var_os("GITHUB_TOKEN");

    crate::env::set_var("JCODE_HOME", temp.path());
    crate::env::remove_var("COPILOT_GITHUB_TOKEN");
    crate::env::remove_var("GH_TOKEN");
    crate::env::remove_var("GITHUB_TOKEN");
    AuthStatus::invalidate_cache();
    crate::auth::copilot::invalidate_github_token_cache();

    crate::auth::copilot::save_github_token("gho_saved_token", "tester")
        .expect("save copilot token");
    crate::auth::validation::save(
        "copilot",
        crate::auth::validation::ProviderValidationRecord {
            checked_at_ms: chrono::Utc::now().timestamp_millis(),
            success: false,
            provider_smoke_ok: None,
            tool_smoke_ok: None,
            summary:
                "refresh_probe: Copilot token exchange failed (HTTP 403 Forbidden): feature_flag_blocked"
                    .to_string(),
        },
    )
    .expect("save validation failure");

    AuthStatus::invalidate_cache();
    crate::auth::copilot::invalidate_github_token_cache();
    let status = AuthStatus::check_fast();
    assert_eq!(status.copilot, AuthState::Expired);
    assert!(!status.copilot_has_api_token);
    assert_eq!(
        copilot_auth_state_from_credentials(),
        (AuthState::Expired, false)
    );

    crate::env::set_var("GH_TOKEN", "gho_env_override");
    AuthStatus::invalidate_cache();
    crate::auth::copilot::invalidate_github_token_cache();
    let status = AuthStatus::check_fast();
    assert_eq!(status.copilot, AuthState::Available);
    assert!(status.copilot_has_api_token);

    restore_env_var("JCODE_HOME", prev_home);
    restore_env_var("COPILOT_GITHUB_TOKEN", prev_copilot_token);
    restore_env_var("GH_TOKEN", prev_gh_token);
    restore_env_var("GITHUB_TOKEN", prev_github_token);
    AuthStatus::invalidate_cache();
    crate::auth::copilot::invalidate_github_token_cache();
}

#[test]
fn openrouter_like_status_is_provider_specific() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_chutes = std::env::var_os("CHUTES_API_KEY");
    let prev_opencode = std::env::var_os("OPENCODE_API_KEY");

    crate::env::set_var("JCODE_HOME", temp.path());
    crate::env::set_var("CHUTES_API_KEY", "chutes-test-key");
    crate::env::remove_var("OPENCODE_API_KEY");
    AuthStatus::invalidate_cache();

    let status = AuthStatus::check_fast();
    let chutes_assessment =
        status.assessment_for_provider(crate::provider_catalog::CHUTES_LOGIN_PROVIDER);
    let opencode_assessment =
        status.assessment_for_provider(crate::provider_catalog::OPENCODE_LOGIN_PROVIDER);
    assert!(chutes_assessment.is_available());
    assert_eq!(opencode_assessment.state, AuthState::NotConfigured);
    assert_eq!(
        chutes_assessment.method_detail,
        "API key (`CHUTES_API_KEY`)".to_string()
    );

    restore_env_var("JCODE_HOME", prev_home);
    restore_env_var("CHUTES_API_KEY", prev_chutes);
    restore_env_var("OPENCODE_API_KEY", prev_opencode);
    AuthStatus::invalidate_cache();
}

#[test]
fn openrouter_status_excludes_shared_compatible_transport() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let keys = [
        "JCODE_HOME",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "JCODE_OPENROUTER_API_BASE",
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENROUTER_ENV_FILE",
        "JCODE_OPENROUTER_PROVIDER_FEATURES",
        "JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "JCODE_OPENROUTER_ALLOW_NO_AUTH",
        "JCODE_NAMED_PROVIDER_PROFILE",
        "JCODE_OPENROUTER_TRANSPORT_STATE",
    ];
    let saved = keys
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect::<Vec<_>>();

    for key in keys {
        crate::env::remove_var(key);
    }
    crate::env::set_var("JCODE_HOME", temp.path());

    crate::env::set_var("OPENAI_API_KEY", "openai-test-key");
    assert!(!crate::provider::openrouter::has_openrouter_credentials());
    AuthStatus::invalidate_cache();
    assert_eq!(
        AuthStatus::check_fast().openrouter,
        AuthState::NotConfigured
    );

    crate::env::set_var("JCODE_OPENROUTER_API_BASE", "https://example.test/v1");
    crate::env::set_var("JCODE_OPENROUTER_API_KEY_NAME", "OPENAI_API_KEY");
    assert!(crate::provider::openrouter::has_credentials());
    assert!(!crate::provider::openrouter::has_openrouter_credentials());

    crate::env::remove_var("JCODE_OPENROUTER_API_BASE");
    crate::env::remove_var("JCODE_OPENROUTER_API_KEY_NAME");
    crate::env::remove_var("OPENAI_API_KEY");
    crate::env::set_var("OPENROUTER_API_KEY", "openrouter-test-key");
    assert!(crate::provider::openrouter::has_openrouter_credentials());
    AuthStatus::invalidate_cache();
    assert_eq!(AuthStatus::check_fast().openrouter, AuthState::Available);

    for (key, value) in saved {
        restore_env_var(key, value);
    }
    AuthStatus::invalidate_cache();
}

#[test]
fn azure_readiness_distinguishes_credentials_from_deployment_validation() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let saved = [
        "JCODE_HOME",
        crate::auth::azure::ENDPOINT_ENV,
        crate::auth::azure::API_KEY_ENV,
        crate::auth::azure::MODEL_ENV,
        crate::auth::azure::USE_ENTRA_ENV,
    ]
    .into_iter()
    .map(|key| (key, std::env::var_os(key)))
    .collect::<Vec<_>>();

    crate::env::set_var("JCODE_HOME", temp.path());
    crate::env::set_var(
        crate::auth::azure::ENDPOINT_ENV,
        "https://example.openai.azure.com",
    );
    crate::env::set_var(crate::auth::azure::API_KEY_ENV, "azure-test-key");
    crate::env::set_var(crate::auth::azure::MODEL_ENV, "gpt-test-deployment");
    crate::env::remove_var(crate::auth::azure::USE_ENTRA_ENV);
    AuthStatus::invalidate_cache();

    let status = AuthStatus::check_fast();
    let assessment = status.assessment_for_provider(crate::provider_catalog::AZURE_LOGIN_PROVIDER);
    assert_eq!(assessment.state, AuthState::Available);
    assert_eq!(assessment.readiness, AuthReadinessLevel::CredentialPresent);
    assert!(
        assessment
            .health_summary()
            .contains("readiness: credential present")
    );

    crate::auth::validation::save(
        "azure",
        crate::auth::validation::ProviderValidationRecord {
            checked_at_ms: chrono::Utc::now().timestamp_millis(),
            success: false,
            provider_smoke_ok: Some(false),
            tool_smoke_ok: None,
            summary: "provider_smoke: deployment not found".to_string(),
        },
    )
    .expect("save failed validation");
    let assessment = status.assessment_for_provider(crate::provider_catalog::AZURE_LOGIN_PROVIDER);
    assert_eq!(assessment.readiness, AuthReadinessLevel::CredentialPresent);

    crate::auth::validation::save(
        "azure",
        crate::auth::validation::ProviderValidationRecord {
            checked_at_ms: chrono::Utc::now().timestamp_millis(),
            success: true,
            provider_smoke_ok: Some(true),
            tool_smoke_ok: None,
            summary: "provider_smoke: ok".to_string(),
        },
    )
    .expect("save successful validation");
    let assessment = status.assessment_for_provider(crate::provider_catalog::AZURE_LOGIN_PROVIDER);
    assert_eq!(assessment.readiness, AuthReadinessLevel::DeploymentValid);
    assert!(
        assessment
            .health_summary()
            .contains("readiness: deployment valid")
    );

    for (key, value) in saved {
        restore_env_var(key, value);
    }
    AuthStatus::invalidate_cache();
}

#[cfg(unix)]
#[test]
fn cursor_status_is_available_when_api_key_exists_without_cli() {
    let _lock = crate::storage::lock_test_env();
    let prev_access_token = std::env::var_os("CURSOR_ACCESS_TOKEN");
    let prev_refresh_token = std::env::var_os("CURSOR_REFRESH_TOKEN");
    let prev_api_key = std::env::var_os("CURSOR_API_KEY");

    crate::env::remove_var("CURSOR_ACCESS_TOKEN");
    crate::env::remove_var("CURSOR_REFRESH_TOKEN");
    crate::env::set_var("CURSOR_API_KEY", "cursor-test-key");
    AuthStatus::invalidate_cache();

    let status = AuthStatus::check();
    assert_eq!(status.cursor, AuthState::Available);

    restore_env_var("CURSOR_ACCESS_TOKEN", prev_access_token);
    restore_env_var("CURSOR_REFRESH_TOKEN", prev_refresh_token);
    restore_env_var("CURSOR_API_KEY", prev_api_key);
    AuthStatus::invalidate_cache();
}

#[cfg(unix)]
#[test]
fn cursor_status_is_available_for_native_auth_without_cli() {
    let _lock = crate::storage::lock_test_env();
    let prev_access_token = std::env::var_os("CURSOR_ACCESS_TOKEN");
    let prev_refresh_token = std::env::var_os("CURSOR_REFRESH_TOKEN");
    let prev_api_key = std::env::var_os("CURSOR_API_KEY");

    crate::env::set_var(
        "CURSOR_ACCESS_TOKEN",
        "eyJhbGciOiJub25lIn0.eyJleHAiIjo0MTAyNDQ0ODAwfQ.",
    );
    crate::env::remove_var("CURSOR_REFRESH_TOKEN");
    crate::env::remove_var("CURSOR_API_KEY");
    AuthStatus::invalidate_cache();

    let status = AuthStatus::check();
    assert_eq!(status.cursor, AuthState::Available);

    restore_env_var("CURSOR_ACCESS_TOKEN", prev_access_token);
    restore_env_var("CURSOR_REFRESH_TOKEN", prev_refresh_token);
    restore_env_var("CURSOR_API_KEY", prev_api_key);
    AuthStatus::invalidate_cache();
}

#[test]
fn configured_api_key_source_uses_valid_overrides() {
    let _lock = crate::storage::lock_test_env();
    let key_var = "JCODE_OPENAI_COMPAT_API_KEY_NAME";
    let file_var = "JCODE_OPENAI_COMPAT_ENV_FILE";
    let prev_key = std::env::var(key_var).ok();
    let prev_file = std::env::var(file_var).ok();

    crate::env::set_var(key_var, "GROQ_API_KEY");
    crate::env::set_var(file_var, "groq.env");

    let source = crate::provider_catalog::configured_api_key_source(
        key_var,
        file_var,
        "OPENAI_COMPAT_API_KEY",
        "compat.env",
    );
    assert_eq!(
        source,
        Some(("GROQ_API_KEY".to_string(), "groq.env".to_string()))
    );

    if let Some(v) = prev_key {
        crate::env::set_var(key_var, v);
    } else {
        crate::env::remove_var(key_var);
    }
    if let Some(v) = prev_file {
        crate::env::set_var(file_var, v);
    } else {
        crate::env::remove_var(file_var);
    }
}

#[test]
fn configured_api_key_source_rejects_invalid_values() {
    let _lock = crate::storage::lock_test_env();
    let key_var = "JCODE_OPENAI_COMPAT_API_KEY_NAME";
    let file_var = "JCODE_OPENAI_COMPAT_ENV_FILE";
    let prev_key = std::env::var(key_var).ok();
    let prev_file = std::env::var(file_var).ok();

    crate::env::set_var(key_var, "bad-key");
    crate::env::set_var(file_var, "../bad.env");

    let source = crate::provider_catalog::configured_api_key_source(
        key_var,
        file_var,
        "OPENAI_COMPAT_API_KEY",
        "compat.env",
    );
    assert!(source.is_none());

    if let Some(v) = prev_key {
        crate::env::set_var(key_var, v);
    } else {
        crate::env::remove_var(key_var);
    }
    if let Some(v) = prev_file {
        crate::env::set_var(file_var, v);
    } else {
        crate::env::remove_var(file_var);
    }
}

#[test]
fn anthropic_api_provider_reports_api_key_independently_of_oauth() {
    // Regression: the `anthropic-api` (API-key) login provider used to share the
    // OAuth/subscription credential's availability via `auth_state_key::Anthropic`.
    // That made it claim "available / OAuth + API key" even with zero API key
    // configured, then fail at request time (API-key mode never falls back to
    // OAuth). It must report purely on the presence of an Anthropic API key.
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    std::fs::create_dir_all(&home).expect("create temp home");
    std::fs::create_dir_all(&xdg).expect("create temp xdg config");
    let saved = ["JCODE_HOME", "XDG_CONFIG_HOME", "HOME", "ANTHROPIC_API_KEY"]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect::<Vec<_>>();

    crate::env::set_var("JCODE_HOME", temp.path().join("jcode-home"));
    crate::env::set_var("XDG_CONFIG_HOME", &xdg);
    crate::env::set_var("HOME", &home);
    crate::env::remove_var("ANTHROPIC_API_KEY");
    AuthStatus::invalidate_cache();

    // No API key anywhere: the API-key provider must be NotConfigured, even if
    // OAuth credentials happen to exist for the separate `claude` provider.
    let status = AuthStatus::check_fast();
    let api = status.assessment_for_provider(crate::provider_catalog::ANTHROPIC_API_LOGIN_PROVIDER);
    assert_eq!(
        api.state,
        AuthState::NotConfigured,
        "anthropic-api must not borrow OAuth availability"
    );
    assert_eq!(api.method_detail, "not configured");

    // With an API key present (env here; config-file path is covered separately),
    // the API-key provider becomes available and names ANTHROPIC_API_KEY honestly.
    crate::env::set_var("ANTHROPIC_API_KEY", "sk-ant-api-test-key");
    AuthStatus::invalidate_cache();
    let status = AuthStatus::check_fast();
    let api = status.assessment_for_provider(crate::provider_catalog::ANTHROPIC_API_LOGIN_PROVIDER);
    assert_eq!(api.state, AuthState::Available);
    assert!(
        api.method_detail.contains("ANTHROPIC_API_KEY"),
        "method detail should name the API key env: {}",
        api.method_detail
    );

    for (key, value) in saved {
        restore_env_var(key, value);
    }
    AuthStatus::invalidate_cache();
}

#[test]
fn claude_oauth_provider_reports_oauth_independently_of_api_key() {
    // Mirror of the regression above: the `claude` (OAuth/subscription) login
    // provider must report on OAuth credentials alone. An ANTHROPIC_API_KEY
    // used to leak into `auth_state_key::Anthropic`, making the OAuth row claim
    // "available / OAuth + API key" with zero OAuth accounts -- contradicting
    // the separate `anthropic-api` row and the header's active-route tag.
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    std::fs::create_dir_all(&home).expect("create temp home");
    std::fs::create_dir_all(&xdg).expect("create temp xdg config");
    let saved = ["JCODE_HOME", "XDG_CONFIG_HOME", "HOME", "ANTHROPIC_API_KEY"]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect::<Vec<_>>();

    crate::env::set_var("JCODE_HOME", temp.path().join("jcode-home"));
    crate::env::set_var("XDG_CONFIG_HOME", &xdg);
    crate::env::set_var("HOME", &home);
    // API key present, no OAuth anywhere: the OAuth provider must stay
    // NotConfigured and must not describe the API key as its method.
    crate::env::set_var("ANTHROPIC_API_KEY", "sk-ant-api-test-key");
    AuthStatus::invalidate_cache();

    let status = AuthStatus::check_fast();
    let oauth = status.assessment_for_provider(crate::provider_catalog::CLAUDE_LOGIN_PROVIDER);
    assert_eq!(
        oauth.state,
        AuthState::NotConfigured,
        "claude (OAuth) must not borrow API-key availability"
    );
    assert_eq!(oauth.method_detail, "not configured");
    assert!(
        !oauth.credential_source_detail.contains("ANTHROPIC_API_KEY"),
        "OAuth row must not attribute the API key as its source: {}",
        oauth.credential_source_detail
    );

    // The API-key row still owns that credential.
    let api = status.assessment_for_provider(crate::provider_catalog::ANTHROPIC_API_LOGIN_PROVIDER);
    assert_eq!(api.state, AuthState::Available);

    for (key, value) in saved {
        restore_env_var(key, value);
    }
    AuthStatus::invalidate_cache();
}

/// Test binaries must never open real browser windows: login/onboarding flows
/// are exercised heavily by unit tests, and each ungated `open::that` pops an
/// OAuth page on the developer's desktop. `running_in_test_harness` detects
/// the `target/**/deps/` test-binary path, and `browser_suppressed` must honor
/// it even without --no-browser or NO_BROWSER/JCODE_NO_BROWSER.
#[test]
fn test_harness_detection_covers_both_cargo_output_layouts() {
    // Classic layout.
    assert!(super::exe_path_is_test_harness(
        "/home/dev/jcode/target/selfdev/deps/jcode_base-abc123"
    ));
    // Split build-directory layout.
    assert!(super::exe_path_is_test_harness(
        "/home/dev/jcode/target/selfdev/build/jcode-base/abc123/out/jcode_base-abc123"
    ));
    // Windows separators.
    assert!(super::exe_path_is_test_harness(
        r"C:\src\jcode\target\debug\deps\jcode_base-abc123.exe"
    ));
    // A real binary built into the checkout is not a test harness.
    assert!(!super::exe_path_is_test_harness(
        "/home/dev/jcode/target/selfdev/jcode"
    ));
    // Nor is an installed binary.
    assert!(!super::exe_path_is_test_harness(
        "/home/dev/.jcode/builds/current/jcode"
    ));
}

#[test]
fn browser_suppressed_inside_test_harness_without_env_overrides() {
    assert!(
        super::running_in_test_harness(),
        "test binary should be detected as a test harness (exe under target/**/deps/)"
    );
    assert!(
        super::browser_suppressed(false),
        "browser opens must be suppressed in test binaries even without --no-browser/env vars"
    );
}

/// Antigravity/Gemini access tokens live about an hour and are refreshed
/// transparently on the next request. Reporting `Expired` just because the
/// cached access token aged out made a fully working provider render as broken
/// in `/login`, the header, onboarding, and `jcode auth status`, which is what
/// the "antigravity is not working" reports actually were. Only a missing or
/// permanently rejected refresh token means the user must log in again.
#[test]
fn refreshable_token_state_covers_the_full_expiry_state_space() {
    let never_rejected = |_: &str| false;
    let always_rejected = |_: &str| true;

    // (case, expired access token, refresh token, refresh token rejected) -> state
    let cases: [(&str, bool, &str, bool, AuthState); 6] = [
        (
            "hourly access token expired but refresh works",
            true,
            "1//live-refresh-token",
            false,
            AuthState::Available,
        ),
        (
            "fresh access token",
            false,
            "1//live-refresh-token",
            false,
            AuthState::Available,
        ),
        (
            "fresh access token, no refresh token",
            false,
            "",
            false,
            AuthState::Available,
        ),
        (
            "expired with no refresh token needs re-login",
            true,
            "   ",
            false,
            AuthState::Expired,
        ),
        (
            "expired with revoked refresh token needs re-login",
            true,
            "1//revoked",
            true,
            AuthState::Expired,
        ),
        (
            "fresh access token is trusted even if an old refresh token was rejected",
            false,
            "1//revoked",
            true,
            AuthState::Available,
        ),
    ];

    for (case, expired, refresh_token, rejected, expected) in cases {
        let observed = if rejected {
            super::refreshable_token_state_with(
                Ok((expired, refresh_token.to_string())),
                always_rejected,
            )
        } else {
            super::refreshable_token_state_with(
                Ok((expired, refresh_token.to_string())),
                never_rejected,
            )
        };
        assert_eq!(observed, expected, "{case}");
    }
}

#[test]
fn missing_refreshable_credentials_are_not_configured() {
    assert_eq!(
        super::refreshable_token_state_with(
            Err(anyhow::anyhow!("No Antigravity tokens found.")),
            |_| false
        ),
        AuthState::NotConfigured
    );
}

#[test]
fn openai_compatible_credentials_make_any_provider_available() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_cerebras = std::env::var_os("CEREBRAS_API_KEY");

    crate::env::set_var("JCODE_HOME", temp.path());
    crate::env::remove_var("CEREBRAS_API_KEY");
    AuthStatus::invalidate_cache();

    let empty = AuthStatus::check_fast();
    assert!(
        !empty.has_any_available(),
        "a sandbox with no credentials should report nothing available"
    );

    crate::env::set_var("CEREBRAS_API_KEY", "test-cerebras-key");
    AuthStatus::invalidate_cache();

    let provider = crate::provider_catalog::resolve_login_provider("cerebras")
        .expect("cerebras is a login provider");
    let configured = AuthStatus::check_fast();

    assert_eq!(
        configured.state_for_provider(provider),
        AuthState::Available,
        "per-provider status should see the configured key"
    );
    assert!(
        configured.has_any_available(),
        "the aggregate must agree with the per-provider status (#155)"
    );

    restore_env_var("JCODE_HOME", prev_home);
    restore_env_var("CEREBRAS_API_KEY", prev_cerebras);
    AuthStatus::invalidate_cache();
}

#[test]
fn has_any_available_reads_the_snapshot_without_touching_disk() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    AuthStatus::invalidate_cache();

    let status = AuthStatus::check_fast();

    // The snapshot is already built, so answering from it must be pure field
    // reads. Resolving openai-compatible profiles on demand instead costs a
    // config-file read per profile (~180us/call measured), which is charged to
    // every frame that renders the header.
    let start = std::time::Instant::now();
    for _ in 0..200 {
        std::hint::black_box(status.has_any_available());
    }
    let per_call = start.elapsed() / 200;

    assert!(
        per_call < std::time::Duration::from_micros(10),
        "has_any_available must answer from the snapshot, not the filesystem; \
         measured {per_call:?} per call"
    );

    restore_env_var("JCODE_HOME", prev_home);
    AuthStatus::invalidate_cache();
}
fn issue_211_saved_env() -> Vec<(&'static str, Option<OsString>)> {
    [
        "JCODE_HOME",
        "HOME",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "JCODE_ANTHROPIC_AUTH",
        "JCODE_ANTHROPIC_API_KEY_NAME",
        "JCODE_ANTHROPIC_ENV_FILE",
    ]
    .into_iter()
    .map(|key| (key, std::env::var_os(key)))
    .collect()
}

fn issue_211_point_at_empty_home_and_clear_anthropic_env(temp_home: &std::path::Path) {
    crate::env::set_var("JCODE_HOME", temp_home);
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "JCODE_ANTHROPIC_AUTH",
        "JCODE_ANTHROPIC_API_KEY_NAME",
        "JCODE_ANTHROPIC_ENV_FILE",
    ] {
        crate::env::remove_var(key);
    }
    AuthStatus::invalidate_cache();
}

fn issue_211_switch_home_leaving_cache_populated(temp_home: &std::path::Path) {
    crate::env::set_var("JCODE_HOME", temp_home);
    crate::env::remove_var("ANTHROPIC_API_KEY");
}

#[test]
fn issue_211_temp_home_isolates_oauth_credentials() {
    let _env_lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("temp dir");
    let saved = issue_211_saved_env();

    issue_211_point_at_empty_home_and_clear_anthropic_env(&temp.path().join("jcode-home"));
    let isolated = AuthStatus::check_fast();

    for (key, previous) in saved {
        restore_env_var(key, previous);
    }
    AuthStatus::invalidate_cache();

    assert!(
        !isolated.anthropic.has_oauth,
        "a temp JCODE_HOME must hide real OAuth credentials, got has_oauth=true"
    );
}

#[test]
fn issue_211_temp_home_does_not_isolate_api_key_env() {
    let _env_lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("temp dir");
    let saved = issue_211_saved_env();

    issue_211_point_at_empty_home_and_clear_anthropic_env(&temp.path().join("jcode-home"));
    let without_key = AuthStatus::check_fast();

    crate::env::set_var("ANTHROPIC_API_KEY", "issue-211-probe-key");
    AuthStatus::invalidate_cache();
    let with_key = AuthStatus::check_fast();

    for (key, previous) in saved {
        restore_env_var(key, previous);
    }
    AuthStatus::invalidate_cache();

    assert!(
        !without_key.anthropic.has_api_key,
        "control: with the env cleared there must be no API key"
    );
    assert!(
        with_key.anthropic.has_api_key,
        "ANTHROPIC_API_KEY must still be visible under a temp JCODE_HOME, so \
         home isolation alone cannot fix the env half of #211"
    );
}

#[test]
fn issue_211_auth_token_env_is_a_second_leak_path() {
    let _env_lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("temp dir");
    let saved = issue_211_saved_env();

    issue_211_point_at_empty_home_and_clear_anthropic_env(&temp.path().join("jcode-home"));
    crate::env::set_var("ANTHROPIC_AUTH_TOKEN", "issue-211-probe-token");
    AuthStatus::invalidate_cache();
    let with_token = AuthStatus::check_fast();

    for (key, previous) in saved {
        restore_env_var(key, previous);
    }
    AuthStatus::invalidate_cache();

    assert!(
        with_token.anthropic.has_api_key,
        "ANTHROPIC_AUTH_TOKEN must also surface as has_api_key, so scrubbing \
         only ANTHROPIC_API_KEY would still leak"
    );
}

#[test]
fn issue_211_fast_cache_does_not_bleed_across_homes() {
    let _env_lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("temp dir");
    let saved = issue_211_saved_env();

    let home_with_key = temp.path().join("home-a");
    issue_211_point_at_empty_home_and_clear_anthropic_env(&home_with_key);
    crate::env::set_var("ANTHROPIC_API_KEY", "issue-211-cache-key");
    let primed = AuthStatus::check_fast();

    issue_211_switch_home_leaving_cache_populated(&temp.path().join("home-b"));
    let after_switch = AuthStatus::check_fast();

    for (key, previous) in saved {
        restore_env_var(key, previous);
    }
    AuthStatus::invalidate_cache();

    assert!(
        primed.anthropic.has_api_key,
        "control: first home had a key"
    );
    assert!(
        !after_switch.anthropic.has_api_key,
        "a cached snapshot from another JCODE_HOME must not be reused"
    );
}
