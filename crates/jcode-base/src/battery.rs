//! Cross-platform, best-effort battery reading.
//!
//! Used by the power inhibitor to stand down on a draining laptop (#29) and by
//! the overnight host snapshot. A missing or unparsable reading must always be
//! reported as "unknown" so desktops, VMs and CI are never gated on it.
//!
//! macOS goes through `pmset -g batt` rather than IOKit. The reconciler ticks
//! every 5s, so the result is cached (see [`detect_cached`]) and the subprocess
//! runs at most once per [`CACHE_TTL`].

use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How long a reading stays fresh for [`detect_cached`].
pub const CACHE_TTL: Duration = Duration::from_secs(30);

/// A best-effort snapshot of the machine's power source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatteryStatus {
    /// Charge percentage, `None` when there is no battery or it cannot be read.
    pub percent: Option<u8>,
    /// Whether the machine is running on external (AC) power. `None` when unknown.
    ///
    /// This is deliberately separate from "charging": a plugged-in laptop that
    /// has finished charging is still on AC, and a plugged-in laptop at 15% is
    /// charging *upward* and must not be treated as draining.
    pub on_ac: Option<bool>,
    /// Raw platform status string (`Discharging`, `charging`, ...) when available.
    pub status: Option<String>,
}

impl BatteryStatus {
    /// Unknown reading: no battery information available.
    pub fn unknown() -> Self {
        Self::default()
    }

    /// Whether the machine is on battery power and at or below `threshold`.
    ///
    /// `threshold == 0` disables the check. An unknown percentage or unknown
    /// power source never counts as low, so machines we cannot read are
    /// unaffected.
    pub fn is_draining_below(&self, threshold: u8) -> bool {
        if threshold == 0 {
            return false;
        }
        let Some(percent) = self.percent else {
            return false;
        };
        // Unknown power source: assume AC, so we never release for a reading we
        // do not fully understand.
        if self.on_ac.unwrap_or(true) {
            return false;
        }
        percent <= threshold
    }
}

/// Read the current battery state, hitting the platform every call.
pub fn detect() -> BatteryStatus {
    #[cfg(target_os = "linux")]
    {
        detect_linux()
    }
    #[cfg(target_os = "macos")]
    {
        detect_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        BatteryStatus::unknown()
    }
}

/// Read the current battery state, reusing a reading younger than [`CACHE_TTL`].
///
/// Callers on a short reconcile interval should use this so macOS does not spawn
/// `pmset` several times a minute.
pub fn detect_cached() -> BatteryStatus {
    static CACHE: OnceLock<Mutex<Option<(Instant, BatteryStatus)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));

    if let Ok(guard) = cache.lock()
        && let Some((fetched_at, status)) = guard.as_ref()
        && fetched_at.elapsed() < CACHE_TTL
    {
        return status.clone();
    }

    let status = detect();
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((Instant::now(), status.clone()));
    }
    status
}

#[cfg(target_os = "linux")]
fn detect_linux() -> BatteryStatus {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return BatteryStatus::unknown();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("BAT") {
            continue;
        }
        let percent = std::fs::read_to_string(path.join("capacity"))
            .ok()
            .and_then(|value| value.trim().parse::<u8>().ok());
        let status = std::fs::read_to_string(path.join("status"))
            .ok()
            .map(|value| value.trim().to_string());
        let on_ac = status.as_deref().map(|status| {
            // `Discharging` is the only state that means "running off the cell".
            // `Full`, `Charging`, `Not charging` and `Unknown` all imply AC.
            !status.eq_ignore_ascii_case("discharging")
        });
        return BatteryStatus {
            percent,
            on_ac,
            status,
        };
    }
    BatteryStatus::unknown()
}

#[cfg(target_os = "macos")]
fn detect_macos() -> BatteryStatus {
    let output = std::process::Command::new("pmset")
        .args(["-g", "batt"])
        .output()
        .ok();
    let Some(output) = output else {
        return BatteryStatus::unknown();
    };
    if !output.status.success() {
        return BatteryStatus::unknown();
    }
    parse_pmset_batt(&String::from_utf8_lossy(&output.stdout))
}

/// Parse `pmset -g batt` output.
///
/// Real shapes handled:
///
/// ```text
/// Now drawing from 'AC Power'
///  -InternalBattery-0 (id=22544483)\t52%; charging; 1:35 remaining present: true
/// ```
///
/// ```text
/// Now drawing from 'Battery Power'
///  -InternalBattery-0 (id=22544483)\t18%; discharging; 0:41 remaining present: true
/// ```
///
/// A desktop prints only the `Now drawing from 'AC Power'` line, which maps to
/// an unknown percentage rather than 0%.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_pmset_batt(output: &str) -> BatteryStatus {
    let mut result = BatteryStatus::unknown();

    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Now drawing from") {
            let source = rest.trim().trim_matches(['\'', '"']).to_ascii_lowercase();
            if source.contains("ac power") {
                result.on_ac = Some(true);
            } else if source.contains("battery power") {
                result.on_ac = Some(false);
            }
            continue;
        }

        if !line.contains("InternalBattery") {
            continue;
        }

        // Fields after the id are tab/semicolon separated: `52%; charging; ...`.
        let mut fields = line
            .split(['\t', ';'])
            .map(str::trim)
            .filter(|field| !field.is_empty());
        // First field still carries `-InternalBattery-0 (id=...)`, possibly with
        // the percentage appended when no tab was emitted.
        for field in fields.by_ref() {
            if let Some(percent) = field
                .split_whitespace()
                .last()
                .and_then(|token| token.strip_suffix('%'))
                .and_then(|value| value.parse::<u8>().ok())
            {
                result.percent = Some(percent);
                break;
            }
        }
        for field in fields {
            let lowered = field.to_ascii_lowercase();
            if lowered == "charging"
                || lowered == "discharging"
                || lowered == "charged"
                || lowered == "finishing charge"
                || lowered == "ac attached"
            {
                result.status = Some(field.to_string());
                break;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ac_power_while_charging() {
        let status = parse_pmset_batt(
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=22544483)\t52%; charging; 1:35 remaining present: true\n",
        );
        assert_eq!(status.percent, Some(52));
        assert_eq!(status.on_ac, Some(true));
        assert_eq!(status.status.as_deref(), Some("charging"));
        assert!(!status.is_draining_below(20));
    }

    #[test]
    fn parses_battery_power_while_discharging() {
        let status = parse_pmset_batt(
            "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=22544483)\t18%; discharging; 0:41 remaining present: true\n",
        );
        assert_eq!(status.percent, Some(18));
        assert_eq!(status.on_ac, Some(false));
        assert_eq!(status.status.as_deref(), Some("discharging"));
        assert!(status.is_draining_below(20));
        assert!(!status.is_draining_below(10));
        assert!(!status.is_draining_below(0), "0 disables the check");
    }

    #[test]
    fn desktop_without_a_battery_reads_as_unknown() {
        let status = parse_pmset_batt("Now drawing from 'AC Power'\n");
        assert_eq!(status.percent, None);
        assert_eq!(status.on_ac, Some(true));
        assert!(!status.is_draining_below(20));
    }

    #[test]
    fn plugged_in_and_charged_is_not_draining() {
        let status = parse_pmset_batt(
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00 remaining present: true\n",
        );
        assert_eq!(status.percent, Some(100));
        assert_eq!(status.on_ac, Some(true));
        assert!(!status.is_draining_below(20));
    }

    #[test]
    fn plugged_in_at_low_charge_keeps_the_inhibitor() {
        // A Mac at 15% on AC is charging upward, so the threshold must not fire.
        let status = parse_pmset_batt(
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t15%; charging; 1:02 remaining present: true\n",
        );
        assert!(!status.is_draining_below(20));
    }

    /// Real output from a MacBook plugged in and holding charge: the status
    /// field reads `AC attached; not charging`, not `charging`.
    #[test]
    fn parses_ac_attached_but_not_charging() {
        let status = parse_pmset_batt(
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=22610019)\t80%; AC attached; not charging present: true\n",
        );
        assert_eq!(status.percent, Some(80));
        assert_eq!(status.on_ac, Some(true));
        assert_eq!(status.status.as_deref(), Some("AC attached"));
        assert!(!status.is_draining_below(20));
    }

    #[test]
    fn unknown_power_source_is_treated_as_ac() {
        let status = BatteryStatus {
            percent: Some(5),
            on_ac: None,
            status: None,
        };
        assert!(!status.is_draining_below(20));
    }

    #[test]
    fn missing_percentage_never_counts_as_low() {
        let status = BatteryStatus {
            percent: None,
            on_ac: Some(false),
            status: Some("discharging".to_string()),
        };
        assert!(!status.is_draining_below(20));
    }
}
