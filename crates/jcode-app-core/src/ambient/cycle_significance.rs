//! Deciding whether a completed ambient cycle is worth a notification.
//!
//! Most cycles are garden maintenance: the queue is empty, memories are
//! healthy, nothing needs the user. Pushing those to a phone trains the user
//! to ignore the channel, which destroys the value of the notifications that
//! DO matter. So the default is silence and a cycle must earn its push.
//!
//! The counts cannot make this call. Real transcripts show a garden-only cycle
//! with `memories_modified = 2` and a genuinely newsworthy one ("#763 and #764
//! are both MERGED") with `memories_modified = 1`: gardening IS memory work,
//! so the number says nothing about whether a human cares. The agent is the
//! only party that knows, so it declares it, and structure is only a fallback
//! for when it does not.

/// What a finished cycle claims about its own newsworthiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleSignificance {
    /// Routine maintenance. No notification.
    Routine,
    /// Something the user would want to know.
    Notable,
    /// The agent did not say.
    Unspecified,
}

impl CycleSignificance {
    /// Parse the agent's declared value.
    ///
    /// Unknown strings are `Unspecified` rather than an error: a model typo
    /// must fall back to structure, never silently suppress a real alert.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_lowercase()).as_deref() {
            Some("routine") | Some("garden") | Some("maintenance") => Self::Routine,
            Some("notable") | Some("significant") => Self::Notable,
            _ => Self::Unspecified,
        }
    }
}

/// The facts a notification decision is made from.
#[derive(Debug, Clone, Copy)]
pub struct CycleOutcome {
    pub significance: CycleSignificance,
    /// Permission requests awaiting a human. Always notifies.
    pub pending_permissions: usize,
    /// The agent changed code, which is never routine.
    pub did_proactive_work: bool,
    /// The cycle did not finish cleanly.
    pub failed: bool,
}

/// Whether to send a phone/desktop notification for this cycle.
///
/// Ordered so the arms that protect the user come first: anything needing a
/// decision, or anything that went wrong, notifies regardless of what the
/// cycle called itself. A cycle cannot silence a permission request or a
/// crash by declaring itself routine.
pub fn should_notify(outcome: &CycleOutcome) -> bool {
    // Blocked on a human. This is the whole point of the channel.
    if outcome.pending_permissions > 0 {
        return true;
    }
    // A failure is news even when the cycle thought it was routine, since a
    // cycle that died may not have reached its own reporting code.
    if outcome.failed {
        return true;
    }
    // Code changed. Never routine, whatever the label says.
    if outcome.did_proactive_work {
        return true;
    }

    match outcome.significance {
        CycleSignificance::Routine => false,
        CycleSignificance::Notable => true,
        // Silence by default when the agent did not say.
        //
        // The alternative (notify on unspecified) reproduces exactly the noise
        // being removed, because garden-only cycles are the overwhelming
        // majority and none of them declare anything. The arms above already
        // cover every case where silence could cost the user something: work
        // needing approval, failures, and code changes all notify on structure
        // alone, without the agent's cooperation.
        CycleSignificance::Unspecified => false,
    }
}
