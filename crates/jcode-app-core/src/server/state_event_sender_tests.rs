//! Tests for the live-attachment bookkeeping in `register_session_event_sender`
//! / `unregister_session_event_sender` and the delivery accounting of
//! `fanout_session_event`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc};

use super::{
    SwarmMember, fanout_session_event, register_session_event_sender,
    unregister_session_event_sender,
};
use crate::protocol::ServerEvent;
use jcode_swarm_core::{SwarmLifecycleStatus, SwarmMemberRecord, SwarmRole};

type Members = Arc<RwLock<HashMap<String, SwarmMember>>>;

fn record(session_id: &str) -> SwarmMemberRecord {
    SwarmMemberRecord {
        session_id: session_id.to_string(),
        working_dir: None,
        swarm_id: None,
        swarm_enabled: false,
        status: SwarmLifecycleStatus::from("ready".to_string()),
        detail: None,
        task_label: None,
        friendly_name: None,
        report_back_to_session_id: None,
        latest_completion_report: None,
        role: SwarmRole::from("agent".to_string()),
        is_headless: false,
    }
}

/// A member whose primary `event_tx` belongs to nobody yet, mirroring a
/// restored-from-record member with no live attachment.
fn members_with(session_id: &str) -> (Members, mpsc::UnboundedReceiver<ServerEvent>) {
    let (tx, rx) = mpsc::unbounded_channel::<ServerEvent>();
    let member = SwarmMember::from_record(record(session_id), tx);
    (
        Arc::new(RwLock::new(HashMap::from([(
            session_id.to_string(),
            member,
        )]))),
        rx,
    )
}

fn delta(text: &str) -> ServerEvent {
    ServerEvent::TextDelta {
        text: text.to_string(),
    }
}

/// Regression: after the only live attachment detaches, fanout must report
/// zero deliveries.
///
/// Before the fix, `unregister_session_event_sender` left `member.event_tx`
/// pointing at the departed connection's still-open channel, so
/// `fanout_session_event` took its "no attachments" branch, the send succeeded
/// into a channel nobody drains, and the delivered count was 1. Callers that
/// gate a user-visible notification on that count then decided a human had
/// seen the event.
#[tokio::test]
async fn fanout_reports_zero_after_the_last_attachment_detaches() {
    let session = "sess-detach";
    let (members, _bootstrap_rx) = members_with(session);

    let (conn_tx, mut conn_rx) = mpsc::unbounded_channel::<ServerEvent>();
    register_session_event_sender(&members, session, "conn-a", conn_tx).await;

    assert_eq!(
        fanout_session_event(&members, session, delta("attached")).await,
        1,
        "an attached client must receive the event"
    );
    assert!(conn_rx.try_recv().is_ok());

    unregister_session_event_sender(&members, session, "conn-a").await;

    // The departed connection's receiver is deliberately still alive here:
    // that is exactly the window in which the stale primary looked deliverable.
    assert_eq!(
        fanout_session_event(&members, session, delta("orphan")).await,
        0,
        "with nobody attached, fanout must not report a delivery"
    );
    assert!(
        conn_rx.try_recv().is_err(),
        "the detached connection must not keep receiving session events"
    );
}

/// The sentinel must not be installed while another connection is still
/// attached: the surviving attachment becomes the primary and keeps receiving.
#[tokio::test]
async fn detaching_one_of_two_attachments_keeps_delivering_to_the_other() {
    let session = "sess-two";
    let (members, _bootstrap_rx) = members_with(session);

    let (tx_a, mut rx_a) = mpsc::unbounded_channel::<ServerEvent>();
    let (tx_b, mut rx_b) = mpsc::unbounded_channel::<ServerEvent>();
    register_session_event_sender(&members, session, "conn-a", tx_a).await;
    register_session_event_sender(&members, session, "conn-b", tx_b).await;

    assert_eq!(
        fanout_session_event(&members, session, delta("both")).await,
        2
    );
    let _ = rx_a.try_recv();
    let _ = rx_b.try_recv();

    unregister_session_event_sender(&members, session, "conn-a").await;

    assert_eq!(
        fanout_session_event(&members, session, delta("only b")).await,
        1,
        "the surviving attachment must still receive events"
    );
    assert!(rx_b.try_recv().is_ok());
    assert!(rx_a.try_recv().is_err());

    let member_tx_is_closed = {
        let members = members.read().await;
        members[session].event_tx.is_closed()
    };
    assert!(
        !member_tx_is_closed,
        "primary must re-point at the live attachment, not the closed sentinel"
    );
}

/// A headless member that never registered an attachment keeps its original
/// primary channel: detaching an unknown connection id must not disable it.
#[tokio::test]
async fn unregistering_an_unknown_connection_does_not_close_the_primary() {
    let session = "sess-headless";
    let (members, mut bootstrap_rx) = members_with(session);

    unregister_session_event_sender(&members, session, "conn-never-registered").await;

    assert_eq!(
        fanout_session_event(&members, session, delta("headless")).await,
        1,
        "the untouched primary channel must still be used"
    );
    assert!(bootstrap_rx.try_recv().is_ok());
}

/// Re-attaching after a full detach must restore delivery.
#[tokio::test]
async fn reattaching_after_detach_restores_delivery() {
    let session = "sess-reattach";
    let (members, _bootstrap_rx) = members_with(session);

    let (tx_a, _rx_a) = mpsc::unbounded_channel::<ServerEvent>();
    register_session_event_sender(&members, session, "conn-a", tx_a).await;
    unregister_session_event_sender(&members, session, "conn-a").await;

    let (tx_b, mut rx_b) = mpsc::unbounded_channel::<ServerEvent>();
    register_session_event_sender(&members, session, "conn-b", tx_b).await;

    assert_eq!(
        fanout_session_event(&members, session, delta("back")).await,
        1
    );
    assert!(rx_b.try_recv().is_ok());
}
