//! agent-browser backend support: binary discovery, install, and status.
//!
//! agent-browser (https://github.com/vercel-labs/agent-browser) is a native Rust
//! CLI that drives Chrome over CDP through a background daemon. Unlike the
//! Firefox bridge it needs no browser extension and no native messaging host,
//! so "setup" is just "make sure the binary and a Chrome exist".

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::storage;

const GITHUB_API_LATEST: &str =
    "https://api.github.com/repos/vercel-labs/agent-browser/releases/latest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBrowserStatus {
    pub backend: &'static str,
    pub browser: &'static str,
    pub binary_installed: bool,
    pub binary_path: Option<PathBuf>,
    pub version: Option<String>,
    pub chrome_installed: bool,
    pub responding: bool,
    pub ready: bool,
    pub diagnostics: Vec<String>,
}

fn jcode_dir() -> PathBuf {
    storage::jcode_dir().unwrap_or_else(|_| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".jcode")
    })
}

fn agent_browser_dir() -> PathBuf {
    jcode_dir().join("agent-browser")
}

/// Path jcode installs its own managed copy to.
pub fn managed_binary_path() -> PathBuf {
    let dir = agent_browser_dir();
    #[cfg(windows)]
    {
        dir.join("agent-browser.exe")
    }
    #[cfg(not(windows))]
    {
        dir.join("agent-browser")
    }
}

/// Resolve the agent-browser binary: explicit override, jcode-managed copy, then PATH.
pub fn resolve_binary() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("JCODE_AGENT_BROWSER_BIN") {
        let path = PathBuf::from(explicit);
        if path.exists() {
            return Some(path);
        }
    }

    let managed = managed_binary_path();
    if managed.exists() {
        return Some(managed);
    }

    which_in_path("agent-browser")
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        #[cfg(windows)]
        let candidate = if candidate.extension().is_none() {
            candidate.with_extension("exe")
        } else {
            candidate
        };
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// GitHub release asset name for the current platform.
pub fn platform_asset_name() -> Option<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    Some(match (os, arch) {
        ("linux", "x86_64") => "agent-browser-linux-x64",
        ("linux", "aarch64") => "agent-browser-linux-arm64",
        ("macos", "x86_64") => "agent-browser-darwin-x64",
        ("macos", "aarch64") => "agent-browser-darwin-arm64",
        ("windows", "x86_64") => "agent-browser-win32-x64.exe",
        _ => return None,
    })
}

async fn binary_version(bin: &PathBuf) -> Option<String> {
    let output = tokio::process::Command::new(bin)
        .arg("--version")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Check whether some Chrome/Chromium is discoverable for agent-browser.
fn chrome_present() -> bool {
    if std::env::var("AGENT_BROWSER_EXECUTABLE_PATH").is_ok() {
        return true;
    }

    let managed_root = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agent-browser")
        .join("browsers");
    if managed_root.is_dir()
        && std::fs::read_dir(&managed_root)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false)
    {
        return true;
    }

    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome",
        "brave",
        "brave-browser",
        "microsoft-edge",
    ] {
        if which_in_path(name).is_some() {
            return true;
        }
    }

    #[cfg(target_os = "macos")]
    {
        for path in [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ] {
            if PathBuf::from(path).is_file() {
                return true;
            }
        }
    }

    false
}

pub async fn inspect_status() -> AgentBrowserStatus {
    let mut diagnostics = Vec::new();
    let binary_path = resolve_binary();
    let binary_installed = binary_path.is_some();

    let version = match &binary_path {
        Some(bin) => binary_version(bin).await,
        None => None,
    };

    if !binary_installed {
        diagnostics.push("agent-browser binary not found on PATH or in ~/.jcode/agent-browser".into());
    }

    let chrome_installed = chrome_present();
    if binary_installed && !chrome_installed {
        diagnostics.push("no Chrome/Chromium detected; run setup to download Chrome for Testing".into());
    }

    // The binary responding to --version is the cheap liveness probe. A real
    // page launch is deferred to the first action so status stays fast.
    let responding = version.is_some();
    let ready = binary_installed && responding && chrome_installed;

    AgentBrowserStatus {
        backend: "agent_browser",
        browser: "chrome",
        binary_installed,
        binary_path,
        version,
        chrome_installed,
        responding,
        ready,
        diagnostics,
    }
}

/// Download the platform binary from GitHub releases into ~/.jcode/agent-browser.
pub async fn install_binary() -> Result<String> {
    let asset = platform_asset_name()
        .ok_or_else(|| anyhow::anyhow!(
            "No agent-browser release asset for {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))?;

    let client = reqwest::Client::builder()
        .user_agent("jcode-agent-browser-installer")
        .build()?;

    let release: serde_json::Value = client
        .get(GITHUB_API_LATEST)
        .send()
        .await
        .context("Failed to query agent-browser releases")?
        .json()
        .await
        .context("Failed to parse agent-browser release metadata")?;

    let tag = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let url = release
        .get("assets")
        .and_then(|v| v.as_array())
        .and_then(|assets| {
            assets.iter().find(|a| {
                a.get("name").and_then(|n| n.as_str()) == Some(asset)
            })
        })
        .and_then(|a| a.get("browser_download_url"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Release {} has no asset named {}", tag, asset))?;

    let bytes = client
        .get(url)
        .send()
        .await
        .context("Failed to download agent-browser binary")?
        .bytes()
        .await?;

    let dest = managed_binary_path();
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&dest, &bytes).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(&dest).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&dest, perms).await?;
    }

    Ok(format!(
        "Installed agent-browser {} to {}",
        tag,
        dest.display()
    ))
}

/// Full setup: install the binary if missing, then ensure a Chrome exists.
pub async fn ensure_setup() -> Result<String> {
    let mut log = String::new();

    let bin = match resolve_binary() {
        Some(path) => {
            log.push_str(&format!("agent-browser already present at {}\n", path.display()));
            path
        }
        None => {
            let message = install_binary().await?;
            log.push_str(&message);
            log.push('\n');
            managed_binary_path()
        }
    };

    if chrome_present() {
        log.push_str("Chrome/Chromium already available.\n");
    } else {
        log.push_str("Downloading Chrome for Testing via `agent-browser install`...\n");
        let output = tokio::process::Command::new(&bin)
            .arg("install")
            .output()
            .await
            .context("Failed to run `agent-browser install`")?;
        log.push_str(&String::from_utf8_lossy(&output.stdout));
        if !output.status.success() {
            log.push_str(&String::from_utf8_lossy(&output.stderr));
            log.push_str(
                "\nChrome download failed. On Linux you may need `agent-browser install --with-deps`.\n",
            );
        }
    }

    Ok(log)
}

/// Stable per-jcode-session name so each agent session gets an isolated browser.
pub fn session_name(session_id: &str) -> String {
    let sanitized: String = session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = sanitized.trim_matches('-');
    let short = if trimmed.len() > 40 {
        &trimmed[trimmed.len() - 40..]
    } else {
        trimmed
    };
    if short.is_empty() {
        "jcode".to_string()
    } else {
        format!("jcode-{}", short)
    }
}

#[cfg(test)]
#[path = "agent_browser_tests.rs"]
mod agent_browser_tests;
