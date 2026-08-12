//! Staleness detection for the *encoded* default keybinding tables.
//!
//! Two of the terminals jcode scans cannot be asked what their keymap is:
//! Alacritty and kitty compile their defaults into the binary, ship a config
//! file that is entirely comments, and offer no "dump my effective keymap"
//! command. So their default tables are transcribed into [`super::terminal`]
//! from upstream documentation/source at a point in time.
//!
//! That transcription is a silent liability. If upstream adds, removes or moves
//! a default binding, jcode keeps reporting the old table with total
//! confidence: a chord that is now taken is reported free (a missed conflict),
//! and a chord that was freed is reported taken (a false conflict sending the
//! user hunting for something that does not exist). Nothing in the codebase
//! notices, because there is no signal to notice.
//!
//! The fix is to pin each table to the upstream version it was verified
//! against, detect the version actually installed, and say so when they differ.
//! The alternative the issue floated — a CI job diffing the table against the
//! upstream man page — needs network access from the test suite and a stable
//! machine-readable source for two different projects; the version pin gets the
//! same signal (*"a human should re-check this table"*) at a fraction of the
//! cost and fails safe when the probe does not work.
//!
//! ## Why only the major.minor version is compared
//!
//! Patch releases fix bugs; they do not reorganise a documented default keymap.
//! Comparing full versions would flag drift on every `0.17.0` → `0.17.1`, which
//! is the fastest way to teach the reader to ignore the warning. Comparing
//! `major.minor` keeps the signal aligned with when tables actually change.
//!
//! The division of labour matters: the snapshot records only the *fact* (which
//! version is installed) and this module computes the *verdict* at render time.
//! That way bumping a pin after re-verifying a table takes effect immediately,
//! rather than staying wrong until the machine happens to be re-scanned.

use serde::{Deserialize, Serialize};

/// The version of a scanned tool as found on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolVersion {
    /// Tool label, matching `DiscoveredBinding::tool` (e.g. "Alacritty").
    pub tool: String,
    /// Version as reported by the tool, e.g. "0.17.0".
    pub version: String,
}

/// An encoded default table and the upstream version it was last verified
/// against.
///
/// Bump `verified_version` **only** after actually re-reading the upstream
/// table and reconciling the transcription in [`super::terminal`]. Bumping it to
/// silence the warning without re-checking defeats the entire mechanism.
struct EncodedTable {
    tool: &'static str,
    verified_version: &'static str,
}

/// Tables transcribed by hand, with their verification points.
///
/// Ghostty and WezTerm are deliberately absent: jcode asks their binaries
/// (`ghostty +list-keybinds`, `wezterm show-keys`) for the effective keymap, so
/// there is no transcription to go stale and nothing a version could tell us.
const ENCODED_TABLES: &[EncodedTable] = &[
    // Transcribed from `alacritty-bindings(5)`.
    EncodedTable {
        tool: "Alacritty",
        verified_version: "0.17",
    },
    // Transcribed from upstream `kitty/options/definition.py`.
    EncodedTable {
        tool: "kitty",
        verified_version: "0.48",
    },
];

/// Which direction an installed version sits relative to the verified one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftDirection {
    /// Installed is newer: upstream may have changed bindings jcode still
    /// reports from the old table.
    Newer,
    /// Installed is older: jcode's table may contain bindings this version does
    /// not have yet, which shows up as conflicts that cannot fire.
    Older,
}

/// A detected mismatch between an encoded table and the installed tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDrift {
    pub tool: String,
    pub verified_version: String,
    pub installed_version: String,
    pub direction: DriftDirection,
}

/// Compare two version strings on `major.minor` only.
///
/// Returns `None` when either side does not parse, which is treated as "no
/// drift": a version string we cannot understand is not evidence that the table
/// is wrong, and inventing a warning from a failed parse would be worse than
/// staying quiet.
fn compare_major_minor(installed: &str, verified: &str) -> Option<std::cmp::Ordering> {
    let parse = |s: &str| -> Option<(u32, u32)> {
        let mut parts = s.trim().trim_start_matches('v').split('.');
        let major = parts.next()?.parse::<u32>().ok()?;
        // A bare "1" is a legitimate major-only version; treat the minor as 0
        // rather than rejecting the whole string.
        let minor = match parts.next() {
            Some(m) => m.parse::<u32>().ok()?,
            None => 0,
        };
        Some((major, minor))
    };
    Some(parse(installed)?.cmp(&parse(verified)?))
}

/// Extract a version number from a tool's `--version` output.
///
/// Both tools print a line with extra material around the number
/// (`alacritty 0.17.0 (0e1b0e0)`, `kitty 0.48.2 created by Kovid Goyal`), so we
/// take the first whitespace-separated token that looks like a dotted number.
pub fn parse_version_output(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .map(|tok| tok.trim_start_matches('v'))
        .find(|tok| {
            let mut chars = tok.chars();
            chars.next().is_some_and(|c| c.is_ascii_digit())
                && tok.contains('.')
                && tok
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
        })
        .map(|tok| tok.trim_end_matches('.').to_string())
}

/// Ask a tool for its version, returning `None` if it cannot be run.
fn probe_version(bin: &str) -> Option<String> {
    let output = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_version_output(&String::from_utf8_lossy(&output.stdout))
}

/// Detect versions of the tools whose default tables jcode encodes.
///
/// Only the terminal actually in use is probed, matching the gating the
/// scanners themselves use: spawning `--version` for an installed-but-unused
/// terminal costs a process for a table whose bindings are not in the snapshot
/// anyway, and would attach a drift warning to a terminal the user is not
/// running.
pub fn detect_tool_versions() -> Vec<ToolVersion> {
    let mut found = Vec::new();
    if std::env::var_os("ALACRITTY_WINDOW_ID").is_some()
        && let Some(version) = probe_version("alacritty")
    {
        found.push(ToolVersion {
            tool: "Alacritty".to_string(),
            version,
        });
    }
    if std::env::var_os("KITTY_WINDOW_ID").is_some()
        && let Some(version) = probe_version("kitty")
    {
        found.push(ToolVersion {
            tool: "kitty".to_string(),
            version,
        });
    }
    found
}

/// Encoded tables whose pinned version disagrees with what is installed.
///
/// A tool with no detected version yields nothing: an unknown version is not
/// evidence of drift. Likewise a tool that contributed no bindings to the
/// snapshot is skipped, so a drift note never appears for a table that had no
/// influence on the report the user is reading.
pub fn detect_drift(versions: &[ToolVersion]) -> Vec<TableDrift> {
    let mut drifts = Vec::new();
    for table in ENCODED_TABLES {
        let Some(found) = versions.iter().find(|v| v.tool == table.tool) else {
            continue;
        };
        let Some(ordering) = compare_major_minor(&found.version, table.verified_version) else {
            continue;
        };
        let direction = match ordering {
            std::cmp::Ordering::Greater => DriftDirection::Newer,
            std::cmp::Ordering::Less => DriftDirection::Older,
            std::cmp::Ordering::Equal => continue,
        };
        drifts.push(TableDrift {
            tool: table.tool.to_string(),
            verified_version: table.verified_version.to_string(),
            installed_version: found.version.clone(),
            direction,
        });
    }
    drifts
}

/// User-facing explanation of a drift, phrased so the consequence is clear
/// rather than just the version mismatch.
pub fn explain(drift: &TableDrift) -> String {
    let consequence = match drift.direction {
        DriftDirection::Newer => {
            "so bindings added or moved since then are not known here: a conflict\n\
             may be missed"
        }
        DriftDirection::Older => {
            "which is newer than what you run, so a listed conflict may involve a\n\
             binding your version does not have"
        }
    };
    format!(
        "{tool} {installed} is installed, but jcode's copy of its default keymap was\n\
         verified against {tool} {verified}, {consequence}.",
        tool = drift.tool,
        installed = drift.installed_version,
        verified = drift.verified_version,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(tool: &str, version: &str) -> ToolVersion {
        ToolVersion {
            tool: tool.to_string(),
            version: version.to_string(),
        }
    }

    #[test]
    fn matching_major_minor_reports_no_drift() {
        assert!(detect_drift(&[v("Alacritty", "0.17")]).is_empty());
    }

    #[test]
    fn patch_release_is_not_drift() {
        // The whole point of comparing major.minor: a patch bump must stay
        // silent, or the warning becomes noise and gets ignored.
        assert!(
            detect_drift(&[v("Alacritty", "0.17.9")]).is_empty(),
            "a patch release must not be reported as drift"
        );
    }

    #[test]
    fn newer_minor_reports_drift_as_newer() {
        let drifts = detect_drift(&[v("Alacritty", "0.18.0")]);
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].direction, DriftDirection::Newer);
        assert_eq!(drifts[0].installed_version, "0.18.0");
        assert_eq!(drifts[0].verified_version, "0.17");
    }

    #[test]
    fn older_minor_reports_drift_as_older() {
        let drifts = detect_drift(&[v("kitty", "0.40.1")]);
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].direction, DriftDirection::Older);
        assert_eq!(drifts[0].tool, "kitty");
    }

    #[test]
    fn unknown_version_is_not_drift() {
        // A version string we cannot parse is not evidence that the table is
        // stale; warning from a failed parse would be a fabricated conflict.
        assert!(detect_drift(&[v("Alacritty", "git-main")]).is_empty());
    }

    #[test]
    fn untracked_tool_is_ignored() {
        assert!(detect_drift(&[v("Ghostty", "99.0")]).is_empty());
    }

    #[test]
    fn absent_tool_reports_nothing() {
        // No detected version at all (probe failed, or terminal not in use)
        // must stay silent rather than assume the worst.
        assert!(detect_drift(&[]).is_empty());
    }

    #[test]
    fn ask_the_binary_terminals_are_not_pinned() {
        // Ghostty and WezTerm report their effective keymap, so pinning a
        // version for them would be meaningless. Guard against someone adding
        // one out of symmetry.
        for table in ENCODED_TABLES {
            assert!(
                !matches!(table.tool, "Ghostty" | "WezTerm"),
                "{} is queried at runtime and needs no version pin",
                table.tool
            );
        }
    }

    #[test]
    fn parses_alacritty_version_output() {
        assert_eq!(
            parse_version_output("alacritty 0.17.0 (0e1b0e0)").as_deref(),
            Some("0.17.0")
        );
    }

    #[test]
    fn parses_kitty_version_output() {
        assert_eq!(
            parse_version_output("kitty 0.48.2 created by Kovid Goyal").as_deref(),
            Some("0.48.2")
        );
    }

    #[test]
    fn parses_leading_v_prefix() {
        assert_eq!(
            parse_version_output("tool v1.2.3").as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn version_output_without_a_number_is_none() {
        assert_eq!(parse_version_output("command not found"), None);
        assert_eq!(parse_version_output(""), None);
    }

    #[test]
    fn major_only_version_compares_as_minor_zero() {
        assert_eq!(
            compare_major_minor("1", "1.0"),
            Some(std::cmp::Ordering::Equal)
        );
    }

    #[test]
    fn explanation_names_both_versions_and_the_consequence() {
        let drifts = detect_drift(&[v("Alacritty", "0.18.0")]);
        let text = explain(&drifts[0]);
        assert!(text.contains("0.18.0"), "should name the installed version");
        assert!(text.contains("0.17"), "should name the verified version");
        assert!(
            text.contains("conflict"),
            "should state the consequence, not just the mismatch"
        );
    }
}
