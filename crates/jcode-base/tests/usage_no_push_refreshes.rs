//! A process with no server must still fetch usage for itself (issue #24).
//!
//! Its own integration binary, deliberately containing exactly one test. The
//! assertion is that this process has *not* been latched push-fed, and that latch
//! is process-global and one-way, so any test that adopts a snapshot destroys the
//! precondition for every test after it in the same binary. Keeping this alone
//! makes the guarantee independent of test order and of the harness's scheduling,
//! rather than relying on cargo happening to run tests in alphabetical order.
//!
//! Why it matters: `menubar`, one-shot CLI invocations, and any client before it
//! finishes `Subscribe` never receive a push. If a process started out push-fed,
//! its refresh would be suppressed and those surfaces would show a usage readout
//! that never populates and never recovers.

use jcode_base::usage;

#[test]
fn a_process_without_a_push_is_not_latched_out_of_refreshing() {
    assert!(
        !usage::is_push_fed(),
        "a process that has received no pushed snapshot must remain free to \
         refresh usage itself, otherwise menubar / one-shot CLI / pre-Subscribe \
         clients would never populate a usage readout at all"
    );

    // The render path must be callable and must not latch anything by itself.
    // Only an actual pushed snapshot may set the latch.
    let _ = usage::get_sync();
    assert!(
        !usage::is_push_fed(),
        "reading usage must not mark the process push-fed; only adopting a \
         pushed snapshot may do that"
    );
}
