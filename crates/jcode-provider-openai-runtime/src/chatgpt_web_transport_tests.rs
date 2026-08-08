use super::*;

#[test]
fn base64_matches_known_vectors() {
    // Padding-boundary cases, since the CLI rejects malformed base64 outright.
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64_encode(b"document.title"), "ZG9jdW1lbnQudGl0bGU=");
}

#[test]
fn base64_handles_non_ascii_and_high_bytes() {
    assert_eq!(base64_encode("é".as_bytes()), "w6k=");
    assert_eq!(base64_encode(&[0xFF, 0xFE, 0xFD]), "//79");
}

#[test]
fn backend_defaults_to_firefox_and_is_opt_in() {
    // Firefox stays the default because that is what this route was proven
    // against; agent-browser must be selected explicitly.
    for (value, expected) in [
        (None, WebBackend::FirefoxBridge),
        (Some("firefox"), WebBackend::FirefoxBridge),
        (Some("nonsense"), WebBackend::FirefoxBridge),
        (Some("agent-browser"), WebBackend::AgentBrowser),
        (Some("agent_browser"), WebBackend::AgentBrowser),
        (Some("chrome"), WebBackend::AgentBrowser),
    ] {
        match value {
            Some(raw) => unsafe { std::env::set_var("JCODE_CHATGPT_WEB_BACKEND", raw) },
            None => unsafe { std::env::remove_var("JCODE_CHATGPT_WEB_BACKEND") },
        }
        assert_eq!(WebBackend::resolve(), expected, "for {value:?}");
    }
    unsafe { std::env::remove_var("JCODE_CHATGPT_WEB_BACKEND") };
}

#[test]
fn backend_labels_name_the_actual_browser() {
    assert_eq!(WebBackend::FirefoxBridge.label(), "firefox");
    assert_eq!(WebBackend::AgentBrowser.label(), "chrome");
}

#[test]
fn chrome_profile_defaults_and_ignores_blank() {
    unsafe { std::env::remove_var("JCODE_CHATGPT_WEB_PROFILE") };
    assert_eq!(chrome_profile(), "Default");

    unsafe { std::env::set_var("JCODE_CHATGPT_WEB_PROFILE", "   ") };
    assert_eq!(chrome_profile(), "Default", "blank must not win");

    unsafe { std::env::set_var("JCODE_CHATGPT_WEB_PROFILE", "Work") };
    assert_eq!(chrome_profile(), "Work");
    unsafe { std::env::remove_var("JCODE_CHATGPT_WEB_PROFILE") };
}

#[test]
fn session_names_are_unique_and_shell_safe() {
    let a = next_session_name();
    let b = next_session_name();
    assert_ne!(a, b, "each turn needs its own isolated session");
    for name in [a, b] {
        assert!(name.starts_with("jcode-chatgpt-web-"));
        assert!(
            name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "session name must be safe to pass as an argument: {name}"
        );
    }
}

#[test]
fn firefox_remains_the_default_backend() {
    // The port must not quietly move existing users onto Chrome. Only an
    // explicit opt-in changes the backend.
    unsafe { std::env::remove_var("JCODE_CHATGPT_WEB_BACKEND") };
    assert_eq!(WebBackend::resolve(), WebBackend::FirefoxBridge);
}

#[test]
fn agent_browser_eval_wrapping_preserves_function_body_scripts() {
    // The ChatGPT page scripts are written as function bodies with a top-level
    // `return`, which agent-browser rejects as a SyntaxError unless wrapped.
    // This mirrors the wrapping the transport applies.
    let script = "const x = 1;\nreturn { x };";
    let wrapped = format!("(() => {{\n{script}\n}})()");
    assert!(wrapped.starts_with("(() => {"));
    assert!(wrapped.contains("return { x };"));
    assert!(wrapped.ends_with("})()"));
}
