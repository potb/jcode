use super::*;

#[test]
fn platform_asset_name_matches_release_assets() {
    // Assets published by vercel-labs/agent-browser releases.
    let known = [
        "agent-browser-linux-x64",
        "agent-browser-linux-arm64",
        "agent-browser-darwin-x64",
        "agent-browser-darwin-arm64",
        "agent-browser-win32-x64.exe",
    ];
    if let Some(name) = platform_asset_name() {
        assert!(known.contains(&name), "unexpected asset name: {name}");
    }
}

#[test]
fn session_name_is_sanitized_and_prefixed() {
    let name = session_name("session_fox_1786202149372_c8f8ca6f");
    assert!(name.starts_with("jcode-"));
    assert!(
        name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "session name must be shell/daemon safe: {name}"
    );
}

#[test]
fn session_name_handles_empty_and_long_ids() {
    assert_eq!(session_name(""), "jcode");
    let long = "x".repeat(200);
    let name = session_name(&long);
    assert!(name.len() <= 46, "session name too long: {}", name.len());
}

#[test]
fn managed_binary_path_lives_under_jcode_dir() {
    let path = managed_binary_path();
    assert!(path.to_string_lossy().contains("agent-browser"));
}

#[tokio::test]
async fn inspect_status_reports_backend_identity() {
    let status = inspect_status().await;
    assert_eq!(status.backend, "agent_browser");
    assert_eq!(status.browser, "chrome");
    // ready implies all preconditions held.
    if status.ready {
        assert!(status.binary_installed);
        assert!(status.responding);
        assert!(status.chrome_installed);
    }
}

#[test]
fn outdated_versions_are_below_the_supported_minimum() {
    // 0.13 is the build where `wait --text` and `upload` hang for ~150s.
    assert!(parse_version("agent-browser 0.13.0").unwrap() < MINIMUM_SUPPORTED_VERSION);
    // The version verified working end-to-end.
    assert!(parse_version("agent-browser 0.33.2").unwrap() >= MINIMUM_SUPPORTED_VERSION);
}

#[test]
fn format_version_round_trips() {
    let text = format_version(MINIMUM_SUPPORTED_VERSION);
    assert_eq!(parse_version(&text), Some(MINIMUM_SUPPORTED_VERSION));
}

#[test]
fn caps_cache_can_be_invalidated_after_an_upgrade() {
    // Setup replaces the binary, so a stale cache would keep the old behavior.
    invalidate_backend_caps();
    let caps = BackendCaps::from_version_text(Some("agent-browser 0.33.2"));
    assert!(caps.uses_stable_tab_handles());
}
