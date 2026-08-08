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
    pub outdated: bool,
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

/// Parse `agent-browser 0.33.2` (or a bare `0.33.2`) into a comparable triple.
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let token = text
        .split_whitespace()
        .map(|part| part.trim_start_matches('v'))
        .find(|part| part.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0");
    let minor = minor
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let patch = parts
        .next()
        .unwrap_or("0")
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    Some((major, minor, patch))
}

/// agent-browser 0.30 replaced positional tab indices with stable `t<N>` handles.
pub const STABLE_TAB_HANDLE_VERSION: (u32, u32, u32) = (0, 30, 0);

/// Oldest agent-browser jcode considers healthy.
///
/// Releases below this have upstream defects that surface as long hangs rather
/// than errors: on 0.13, `wait --text` and `upload` block for ~150s and then
/// fail with a daemon read error. jcode installs its own newer copy instead of
/// driving a binary like that.
pub const MINIMUM_SUPPORTED_VERSION: (u32, u32, u32) = (0, 30, 0);

pub fn format_version(version: (u32, u32, u32)) -> String {
    format!("{}.{}.{}", version.0, version.1, version.2)
}

/// Capabilities that vary across agent-browser releases.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackendCaps {
    pub version: Option<(u32, u32, u32)>,
}

impl BackendCaps {
    pub fn from_version_text(text: Option<&str>) -> Self {
        Self {
            version: text.and_then(parse_version),
        }
    }

    /// Whether `tab t<N>` switches tabs.
    ///
    /// Older builds silently treat `t<N>` as "list tabs" and return success, so
    /// when the version is unknown we deliberately pick the positional form:
    /// newer binaries reject it loudly, which is far better than a silent no-op.
    pub fn uses_stable_tab_handles(&self) -> bool {
        match self.version {
            Some(version) => version >= STABLE_TAB_HANDLE_VERSION,
            None => false,
        }
    }
}

static CAPS_CACHE: std::sync::Mutex<Option<BackendCaps>> = std::sync::Mutex::new(None);

/// Cached capabilities for the resolved binary.
///
/// Cached because every browser action consults it, and invalidated by setup
/// so an upgrade takes effect without restarting jcode.
pub async fn backend_caps() -> BackendCaps {
    if let Ok(guard) = CAPS_CACHE.lock()
        && let Some(caps) = *guard
    {
        return caps;
    }

    let caps = match resolve_binary() {
        Some(bin) => BackendCaps::from_version_text(binary_version(&bin).await.as_deref()),
        None => BackendCaps::default(),
    };

    if let Ok(mut guard) = CAPS_CACHE.lock() {
        *guard = Some(caps);
    }
    caps
}

/// Drop cached capabilities after the binary may have changed.
pub fn invalidate_backend_caps() {
    if let Ok(mut guard) = CAPS_CACHE.lock() {
        *guard = None;
    }
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

    let parsed = version.as_deref().and_then(parse_version);
    let outdated = parsed.map(|v| v < MINIMUM_SUPPORTED_VERSION).unwrap_or(false);
    if outdated && let Some(found) = parsed {
        diagnostics.push(format!(
            "agent-browser {} is older than the supported minimum {}; `wait` on text and `upload` hang on these builds. Run browser setup to install a current copy.",
            format_version(found),
            format_version(MINIMUM_SUPPORTED_VERSION)
        ));
    }

    let ready = binary_installed && responding && chrome_installed && !outdated;

    AgentBrowserStatus {
        backend: "agent_browser",
        browser: "chrome",
        binary_installed,
        binary_path,
        version,
        chrome_installed,
        responding,
        outdated,
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

    invalidate_backend_caps();

    Ok(format!(
        "Installed agent-browser {} to {}",
        tag,
        dest.display()
    ))
}

/// Full setup: install the binary if missing, then ensure a Chrome exists.
pub async fn ensure_setup() -> Result<String> {
    let mut log = String::new();

    let existing = inspect_status().await;
    let bin = match (&existing.binary_path, existing.outdated) {
        (Some(path), false) => {
            log.push_str(&format!(
                "agent-browser already present at {}\n",
                path.display()
            ));
            path.clone()
        }
        (Some(path), true) => {
            // Too old to trust: install a current copy jcode manages itself
            // rather than leaving the user on a build that hangs.
            log.push_str(&format!(
                "agent-browser at {} is below the supported minimum {}; installing a current copy.\n",
                path.display(),
                format_version(MINIMUM_SUPPORTED_VERSION)
            ));
            let message = install_binary().await?;
            log.push_str(&message);
            log.push('\n');

            // An explicit override still wins over the copy we just installed,
            // so say so rather than reporting a confusing "still outdated".
            if let Ok(pinned) = std::env::var("JCODE_AGENT_BROWSER_BIN")
                && !pinned.is_empty()
            {
                log.push_str(&format!(
                    "Note: JCODE_AGENT_BROWSER_BIN pins {pinned}, so the newly installed copy will not be used until that variable is unset.\n"
                ));
            }

            managed_binary_path()
        }
        (None, _) => {
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
