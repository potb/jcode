//! Wire types for server-pushed provider usage snapshots (issue #24).
//!
//! These live outside `wire.rs` because that file is over the code-size
//! ratchet's threshold and may not grow; they are self-contained, so a
//! separate module costs nothing in coupling.

use serde::{Deserialize, Serialize};

/// One model-scoped weekly usage window, as reported by the provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageSnapshotWindow {
    pub model_name: String,
    pub utilization: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

/// A provider usage snapshot in a form that can cross a socket.
///
/// The in-process type (`jcode_base::usage::UsageData`) stores its fetch time as
/// an `Instant`, which is process-local and cannot be serialized or compared
/// across processes. This carries the wall-clock fetch time instead and lets the
/// receiver rebuild its own `Instant` on arrival, the same way the on-disk
/// snapshot already does.
///
/// Only successful fetches are pushed, so there is no error field: a failure
/// must not be able to overwrite a client's still-useful stale values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageSnapshot {
    /// Account this snapshot describes, so a client whose active account differs
    /// can ignore it rather than display another account's quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    /// Wall-clock fetch time, milliseconds since the Unix epoch.
    pub fetched_at_ms: i64,
    pub five_hour: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour_resets_at: Option<String>,
    pub seven_day: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day_resets_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day_opus: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_scoped: Vec<UsageSnapshotWindow>,
    #[serde(default)]
    pub extra_usage_enabled: bool,
}
