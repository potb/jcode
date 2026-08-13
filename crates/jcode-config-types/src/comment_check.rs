//! Configuration for the advisory comment check.

use serde::{Deserialize, Serialize};

/// Comment check integration configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CommentCheckConfig {
    /// Master switch. Default true; advisory only, never blocks a tool call.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for CommentCheckConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_enabled() {
        assert!(CommentCheckConfig::default().enabled);
    }

    #[test]
    fn empty_table_keeps_the_default() {
        let parsed: CommentCheckConfig = toml::from_str("").expect("parse");
        assert_eq!(parsed, CommentCheckConfig::default());
    }

    #[test]
    fn explicit_false_round_trips() {
        let parsed: CommentCheckConfig = toml::from_str("enabled = false").expect("parse");
        assert!(!parsed.enabled);
        let text = toml::to_string(&parsed).expect("serialize");
        let reparsed: CommentCheckConfig = toml::from_str(&text).expect("reparse");
        assert_eq!(parsed, reparsed);
    }
}
