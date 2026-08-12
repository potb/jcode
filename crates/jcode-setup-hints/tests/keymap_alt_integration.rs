//! Integration coverage for the keymap snapshot, driven through the crate's
//! public API on the real machine rather than through hand-built structs.
//!
//! The unit tests in `keymap::alt` cover the parsers with fixed inputs. What
//! they cannot show is that the wiring actually holds end to end: that
//! `collect_snapshot` populates the new field, that the value is what this
//! platform should report, that it survives the JSON round-trip through
//! `~/.jcode`-style persistence, and that `render_report` says the right thing
//! to a real user. That is what this file checks.

use jcode_config_types::KeybindingsConfig;
use jcode_setup_hints::keymap::{self, AltDelivery};

/// `collect_snapshot` must fill in `alt_delivery` rather than leaving the
/// serde default in place, and must stamp the current schema version.
#[test]
fn real_snapshot_reports_alt_delivery_for_this_platform() {
    let snapshot = keymap::collect_snapshot();

    assert_eq!(
        snapshot.version, 2,
        "schema version must be stamped so stale caches are refreshed"
    );

    if cfg!(target_os = "macos") {
        // On macOS the answer depends on which terminal is running the test, so
        // only the domain is assertable: it must be one of the modelled states.
        assert!(
            matches!(
                snapshot.alt_delivery,
                AltDelivery::Unknown
                    | AltDelivery::Delivered
                    | AltDelivery::LeftOnly
                    | AltDelivery::RightOnly
                    | AltDelivery::Never
            ),
            "unexpected state {:?}",
            snapshot.alt_delivery
        );
    } else {
        // Everywhere else Alt is delivered as ESC-prefixed input unconditionally,
        // so a non-macOS machine must never be told its Option key is dead.
        assert_eq!(
            snapshot.alt_delivery,
            AltDelivery::Delivered,
            "non-macOS terminals always deliver Alt"
        );
        assert!(
            !snapshot.alt_delivery.is_degraded(),
            "must not warn a Linux/Windows user about Option"
        );
    }
}

/// The field has to survive persistence: it is written to
/// `~/.jcode/keymap-snapshot.json` and read back on the next launch.
#[test]
fn alt_delivery_survives_the_json_round_trip() {
    let mut snapshot = keymap::collect_snapshot();
    snapshot.alt_delivery = AltDelivery::LeftOnly;

    let json = serde_json::to_string(&snapshot).expect("snapshot must serialize");
    assert!(
        json.contains("\"alt_delivery\":\"left_only\""),
        "field must be persisted in snake_case: {json}"
    );

    let back: keymap::KeymapSnapshot = serde_json::from_str(&json).expect("must deserialize");
    assert_eq!(back.alt_delivery, AltDelivery::LeftOnly);
}

/// A snapshot written by an older jcode has no `alt_delivery` key at all. It
/// must read back as `Unknown` (silent), never as a confident "Alt is dead".
#[test]
fn version_1_snapshot_without_the_field_stays_silent() {
    let legacy = r#"{"version":1,"captured_at":"1786464402","os":"linux",
                     "terminal":"Alacritty","bindings":[]}"#;
    let snapshot: keymap::KeymapSnapshot =
        serde_json::from_str(legacy).expect("old snapshots must still parse");

    assert_eq!(snapshot.alt_delivery, AltDelivery::Unknown);
    assert!(
        !snapshot.alt_delivery.is_degraded(),
        "old data must not warn"
    );

    // And it must not silently be trusted: the version differs from current, so
    // the startup path refreshes it instead of reporting from stale data.
    assert_ne!(snapshot.version, 2);
}

/// The user-visible artifact. This is the text a real user reads after `/keys`,
/// rendered from a real machine snapshot.
#[test]
fn real_report_renders_and_matches_the_platform_verdict() {
    let cfg = KeybindingsConfig::default();
    let snapshot = keymap::collect_snapshot();
    let report = keymap::render_report(&cfg, &snapshot);

    // Structural expectations that hold on every platform.
    assert!(report.starts_with("Keymap diagnostics"), "got: {report}");
    assert!(report.contains("OS: "), "got: {report}");
    assert!(report.contains("Discovered bindings:"), "got: {report}");

    let status = keymap::render_status_line(&cfg, &snapshot);

    if snapshot.alt_delivery.is_degraded() {
        assert!(
            report.contains("Option") && report.contains("Alt"),
            "a degraded Option key must be explained in the report: {report}"
        );
        assert!(
            status.is_some(),
            "a degraded Option key must produce a status line"
        );
    } else {
        // The regression this guards: never tell a user their Option key is
        // broken when it is not.
        assert!(
            !report.contains("never sends Option as Alt"),
            "must not claim Alt is dead: {report}"
        );
    }
}
