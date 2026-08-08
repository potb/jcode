//! Live check that the agent-browser backend can drive a real ChatGPT session.
//!
//! Ignored by default: it needs a Chrome profile that is already signed in to
//! chatgpt.com, and it opens a visible browser window. Run explicitly with:
//!
//! ```bash
//! cargo test -p jcode-provider-openai-runtime --test chatgpt_web_live -- --ignored --nocapture
//! ```

use std::time::Duration;

/// The composer is the element the ChatGPT route types into, so reaching it is
/// the meaningful signal that the transport works end to end: the browser
/// launched, the bot check passed, and the session is logged in.
const EDITOR_SELECTOR: &str = "[contenteditable=true]";

#[tokio::test]
#[ignore = "needs a signed-in Chrome profile and opens a browser window"]
async fn agent_browser_backend_reaches_the_chatgpt_composer() {
    let binary = match jcode_base::agent_browser::resolve_binary() {
        Some(path) => path,
        None => {
            eprintln!("skipping: agent-browser is not installed");
            return;
        }
    };

    let profile = std::env::var("JCODE_CHATGPT_WEB_PROFILE").unwrap_or_else(|_| "Default".into());
    let session = format!("jcode-live-test-{}", std::process::id());

    // Mirror exactly what the transport does: headed, with the real profile,
    // and with both flags repeated on every invocation.
    let base = |args: &[&str]| {
        let mut command = std::process::Command::new(&binary);
        command
            .arg("--json")
            .arg("--session")
            .arg(&session)
            .arg("--profile")
            .arg(&profile)
            .arg("--headed");
        for arg in args {
            command.arg(arg);
        }
        command
    };

    let open = base(&["open", "https://chatgpt.com"])
        .output()
        .expect("failed to run agent-browser");
    assert!(
        open.status.success(),
        "open failed: {}",
        String::from_utf8_lossy(&open.stderr)
    );

    // The page performs a bot check before rendering the app.
    tokio::time::sleep(Duration::from_secs(10)).await;

    let script = format!(
        "(() => {{ const c = document.querySelector('{EDITOR_SELECTOR}'); \
         return {{ url: location.href, title: document.title, hasComposer: !!c }}; }})()"
    );
    let encoded = base64_for_test(script.as_bytes());
    let probe = base(&["eval", "-b", &encoded])
        .output()
        .expect("failed to run agent-browser eval");
    let stdout = String::from_utf8_lossy(&probe.stdout).to_string();

    let _ = base(&["close"]).output();

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("non-JSON eval output: {stdout}"));
    let result = &parsed["data"]["result"];

    assert_eq!(
        result["hasComposer"], true,
        "did not reach the ChatGPT composer (title {:?}, url {:?}). \
         A 'Just a moment...' title means the bot check blocked the session; \
         confirm the '{profile}' Chrome profile is signed in at chatgpt.com.",
        result["title"], result["url"]
    );
}

fn base64_for_test(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}
