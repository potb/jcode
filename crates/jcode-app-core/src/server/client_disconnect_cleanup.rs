use super::{
    ClientConnectionInfo, ClientDebugState, FileTouchService, SessionInterruptQueues, SwarmEvent,
    SwarmEventType, SwarmMember, VersionedPlan, record_swarm_event, remove_background_tool_signal,
    remove_session_channel_subscriptions, remove_session_from_swarm,
    remove_session_interrupt_queue, unregister_session_event_sender, update_member_status,
};
use crate::agent::Agent;
use anyhow::Result;
use jcode_agent_runtime::InterruptSignal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, broadcast};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;
type ChannelSubscriptions = Arc<RwLock<HashMap<String, HashMap<String, HashSet<String>>>>>;

const RELOAD_DISCONNECT_MARKER_MAX_AGE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisconnectDisposition {
    Closed,
    Crashed,
    Reloading,
    Detached,
}

fn disconnect_disposition(
    disconnected_while_processing: bool,
    client_detached: bool,
) -> DisconnectDisposition {
    if client_detached {
        return DisconnectDisposition::Detached;
    }

    if !disconnected_while_processing {
        return DisconnectDisposition::Closed;
    }

    if crate::server::reload_marker_active(RELOAD_DISCONNECT_MARKER_MAX_AGE) {
        DisconnectDisposition::Reloading
    } else {
        DisconnectDisposition::Crashed
    }
}

async fn session_has_live_successor(
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    session_id: &str,
) -> bool {
    client_connections
        .read()
        .await
        .values()
        .any(|info| info.owns_session(session_id))
}

#[expect(
    clippy::too_many_arguments,
    reason = "disconnect cleanup updates sessions, swarms, files, channels, debug state, and shutdown signals together"
)]
pub(super) async fn cleanup_client_connection(
    sessions: &SessionAgents,
    client_session_id: &str,
    client_is_processing: bool,
    client_detached: bool,
    processing_task: &mut Option<tokio::task::JoinHandle<()>>,
    event_handle: tokio::task::JoinHandle<()>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    file_touch: &FileTouchService,
    channel_subscriptions: &ChannelSubscriptions,
    channel_subscriptions_by_session: &ChannelSubscriptions,
    client_debug_state: &Arc<RwLock<ClientDebugState>>,
    client_debug_id: &str,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    client_connection_id: &str,
    shutdown_signals: &Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: &SessionInterruptQueues,
    event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
) -> Result<()> {
    let disconnected_while_processing = client_is_processing
        || processing_task
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false);
    let disposition = disconnect_disposition(disconnected_while_processing, client_detached);

    {
        let mut debug_state = client_debug_state.write().await;
        debug_state.unregister(client_debug_id);
    }
    {
        let mut connections = client_connections.write().await;
        connections.remove(client_connection_id);
    }
    unregister_session_event_sender(swarm_members, client_session_id, client_connection_id).await;

    // Release stale live ownership before slower cleanup so a reconnecting TUI can
    // reclaim the same session without tripping duplicate-attach guards.
    tokio::task::yield_now().await;

    // A deliberate detach surrenders this connection only, so return before the
    // destructive branch below: the session agent, its swarm membership,
    // subscriptions and interrupt signals must survive for a later attach.
    if disposition == DisconnectDisposition::Detached {
        crate::logging::info(&format!(
            "Client detached from {}; preserving live session state",
            client_session_id
        ));
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "session_detached",
                "client_detach_request",
            )
            .with_session_id(client_session_id.to_string())
            .force_attribution(),
        );
        event_handle.abort();
        return Ok(());
    }

    let successor_connected =
        session_has_live_successor(client_connections, client_session_id).await;
    if successor_connected {
        crate::logging::info(&format!(
            "Skipping destructive disconnect cleanup for {} because another client is still attached",
            client_session_id
        ));
        event_handle.abort();
        return Ok(());
    }

    {
        if let Some(agent_arc) = super::remove_session_entry(sessions, client_session_id).await {
            let lock_result =
                tokio::time::timeout(std::time::Duration::from_secs(2), agent_arc.lock()).await;

            match lock_result {
                Ok(mut agent) => {
                    match disposition {
                        DisconnectDisposition::Closed => {
                            agent.mark_closed();
                        }
                        DisconnectDisposition::Reloading => {
                            agent.mark_crashed(Some(
                                "Server reload interrupted processing".to_string(),
                            ));
                        }
                        DisconnectDisposition::Crashed => {
                            agent.mark_crashed(Some(
                                "Client disconnected while processing".to_string(),
                            ));
                        }
                        DisconnectDisposition::Detached => {
                            unreachable!("a detached connection returns before destructive cleanup")
                        }
                    }

                    let memory_enabled = agent.memory_enabled();
                    let transcript = if memory_enabled {
                        Some(agent.build_transcript_for_extraction())
                    } else {
                        None
                    };
                    let sid = client_session_id.to_string();
                    let working_dir = agent.working_dir().map(|dir| dir.to_string());
                    drop(agent);
                    let event = match disposition {
                        DisconnectDisposition::Closed => {
                            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                                "session_closed",
                                "client_disconnected",
                            )
                        }
                        DisconnectDisposition::Crashed => {
                            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                                "session_crashed",
                                "client_disconnected_while_processing",
                            )
                        }
                        DisconnectDisposition::Reloading => {
                            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                                "session_reloading",
                                "server_reload_disconnect",
                            )
                        }
                        DisconnectDisposition::Detached => {
                            unreachable!("a detached connection returns before destructive cleanup")
                        }
                    }
                    .with_session_id(sid.clone())
                    .force_attribution();
                    crate::runtime_memory_log::emit_event(event);
                    if let Some(transcript) = transcript {
                        crate::memory_agent::trigger_final_extraction_with_dir(
                            transcript,
                            sid,
                            working_dir,
                        );
                    }
                }
                Err(_) => {
                    crate::logging::warn(&format!(
                        "Session {} cleanup timed out waiting for agent lock (stuck task); skipping graceful shutdown",
                        client_session_id
                    ));
                }
            }
        }
    }

    {
        let (status, detail) = match disposition {
            DisconnectDisposition::Closed => ("stopped", Some("disconnected".to_string())),
            DisconnectDisposition::Crashed => {
                ("crashed", Some("disconnect while running".to_string()))
            }
            DisconnectDisposition::Reloading => {
                ("stopped", Some("server reload in progress".to_string()))
            }
            DisconnectDisposition::Detached => {
                unreachable!("a detached connection returns before destructive cleanup")
            }
        };
        update_member_status(
            client_session_id,
            status,
            detail,
            swarm_members,
            swarms_by_id,
            Some(event_history),
            Some(event_counter),
            Some(swarm_event_tx),
        )
        .await;

        let (swarm_id, removed_name) = {
            let mut members = swarm_members.write().await;
            if let Some(member) = members.remove(client_session_id) {
                (member.swarm_id, member.friendly_name)
            } else {
                (None, None)
            }
        };
        crate::session_metrics::forget(client_session_id);
        crate::session_effort::forget_session_effort(client_session_id);

        if let Some(ref swarm_id) = swarm_id {
            record_swarm_event(
                event_history,
                event_counter,
                swarm_event_tx,
                client_session_id.to_string(),
                removed_name.clone(),
                Some(swarm_id.clone()),
                SwarmEventType::MemberChange {
                    action: "left".to_string(),
                },
            )
            .await;
            remove_session_from_swarm(
                client_session_id,
                swarm_id,
                swarm_members,
                swarms_by_id,
                swarm_coordinators,
                swarm_plans,
            )
            .await;
        }
        remove_session_channel_subscriptions(
            client_session_id,
            channel_subscriptions,
            channel_subscriptions_by_session,
        )
        .await;
        file_touch.clear_session(client_session_id).await;
    }

    {
        let mut signals = shutdown_signals.write().await;
        signals.remove(client_session_id);
    }
    remove_background_tool_signal(client_session_id);
    remove_session_interrupt_queue(soft_interrupt_queues, client_session_id).await;

    if let Some(handle) = processing_task.take() {
        handle.abort();
    }

    event_handle.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ClientConnectionInfo, DisconnectDisposition, cleanup_client_connection,
        disconnect_disposition,
    };
    use crate::agent::Agent;
    use crate::message::{Message, ToolDefinition};
    use crate::provider::{EventStream, Provider};
    use crate::tool::Registry;
    use async_trait::async_trait;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use tokio::sync::{Mutex, RwLock, broadcast};

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<EventStream> {
            Err(anyhow::anyhow!("mock provider should not be called"))
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(MockProvider)
        }
    }

    #[test]
    fn idle_disconnect_is_closed() {
        assert_eq!(
            disconnect_disposition(false, false),
            DisconnectDisposition::Closed
        );
    }

    #[test]
    fn running_disconnect_without_reload_is_crash() {
        let _guard = crate::storage::lock_test_env();
        crate::server::clear_reload_marker();
        assert_eq!(
            disconnect_disposition(true, false),
            DisconnectDisposition::Crashed
        );
    }

    #[test]
    fn running_disconnect_during_reload_is_expected() {
        let _guard = crate::storage::lock_test_env();
        let runtime = tempfile::TempDir::new().expect("create runtime dir");
        crate::env::set_var("JCODE_RUNTIME_DIR", runtime.path());
        crate::server::clear_reload_marker();
        crate::server::write_reload_state(
            "test-request",
            "test-hash",
            crate::server::ReloadPhase::Starting,
            None,
        );
        assert_eq!(
            disconnect_disposition(true, false),
            DisconnectDisposition::Reloading
        );
        crate::server::clear_reload_marker();
        crate::env::remove_var("JCODE_RUNTIME_DIR");
    }

    #[test]
    fn running_disconnect_during_recent_socket_ready_reload_is_expected() {
        let _guard = crate::storage::lock_test_env();
        let runtime = tempfile::TempDir::new().expect("create runtime dir");
        crate::env::set_var("JCODE_RUNTIME_DIR", runtime.path());
        crate::server::clear_reload_marker();
        crate::server::write_reload_state(
            "test-request",
            "test-hash",
            crate::server::ReloadPhase::SocketReady,
            None,
        );
        assert_eq!(
            disconnect_disposition(true, false),
            DisconnectDisposition::Reloading
        );
        crate::server::clear_reload_marker();
        crate::env::remove_var("JCODE_RUNTIME_DIR");
    }

    #[test]
    fn explicit_detach_is_not_a_close_or_crash() {
        assert_eq!(
            disconnect_disposition(false, true),
            DisconnectDisposition::Detached
        );
    }

    #[test]
    fn explicit_detach_while_processing_is_still_detach() {
        assert_eq!(
            disconnect_disposition(true, true),
            DisconnectDisposition::Detached
        );
    }

    struct CleanupFixture {
        sessions: super::SessionAgents,
        client_connections: Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
        swarm_members: Arc<RwLock<HashMap<String, super::SwarmMember>>>,
        shutdown_signals: Arc<RwLock<HashMap<String, jcode_agent_runtime::InterruptSignal>>>,
    }

    async fn run_cleanup(session_id: &str, client_detached: bool) -> CleanupFixture {
        run_cleanup_with_extra_connection(session_id, client_detached, None).await
    }

    /// `extra` is a second connection left in the map for the same session,
    /// with its `is_detaching` flag as given.
    async fn run_cleanup_with_extra_connection(
        session_id: &str,
        client_detached: bool,
        extra: Option<bool>,
    ) -> CleanupFixture {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider);
        let registry = Registry::new(provider.clone()).await;
        let session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
        let agent = Agent::new_with_session(provider, registry, session, None);
        let sessions: super::SessionAgents = Arc::new(RwLock::new(HashMap::new()));
        sessions
            .write()
            .await
            .insert(session_id.to_string(), Arc::new(Mutex::new(agent)));

        let (disconnect_tx, _disconnect_rx) = tokio::sync::mpsc::unbounded_channel();
        let connection_id = "conn-1".to_string();
        let client_connections: Arc<RwLock<HashMap<String, ClientConnectionInfo>>> =
            Arc::new(RwLock::new(HashMap::new()));
        client_connections.write().await.insert(
            connection_id.clone(),
            ClientConnectionInfo {
                client_id: "client-1".to_string(),
                session_id: session_id.to_string(),
                client_instance_id: Some("inst-1".to_string()),
                debug_client_id: None,
                connected_at: std::time::Instant::now(),
                last_seen: std::time::Instant::now(),
                is_processing: false,
                current_tool_name: None,
                terminal_env: Vec::new(),
                is_detaching: false,
                disconnect_tx,
            },
        );

        if let Some(extra_is_detaching) = extra {
            let mut connections = client_connections.write().await;
            connections.insert(
                "conn-2".to_string(),
                ClientConnectionInfo {
                    client_id: "client-2".to_string(),
                    session_id: session_id.to_string(),
                    client_instance_id: Some("inst-2".to_string()),
                    debug_client_id: None,
                    connected_at: std::time::Instant::now(),
                    last_seen: std::time::Instant::now(),
                    is_processing: false,
                    current_tool_name: None,
                    terminal_env: Vec::new(),
                    is_detaching: extra_is_detaching,
                    disconnect_tx: tokio::sync::mpsc::unbounded_channel().0,
                },
            );
        }

        let shutdown_signals: Arc<RwLock<HashMap<String, jcode_agent_runtime::InterruptSignal>>> =
            Arc::new(RwLock::new(HashMap::new()));
        shutdown_signals.write().await.insert(
            session_id.to_string(),
            jcode_agent_runtime::InterruptSignal::new(),
        );

        let swarm_members: Arc<RwLock<HashMap<String, super::SwarmMember>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
        let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
        let file_touch = super::FileTouchService::new();
        let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
        let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
        let client_debug_state = Arc::new(RwLock::new(super::ClientDebugState::default()));
        let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(VecDeque::new()));
        let event_counter = Arc::new(AtomicU64::new(0));
        let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(16);
        let event_handle = tokio::spawn(async { std::future::pending::<()>().await });
        let mut processing_task = None;

        cleanup_client_connection(
            &sessions,
            session_id,
            false,
            client_detached,
            &mut processing_task,
            event_handle,
            &swarm_members,
            &swarms_by_id,
            &swarm_coordinators,
            &swarm_plans,
            &file_touch,
            &channel_subscriptions,
            &channel_subscriptions_by_session,
            &client_debug_state,
            "debug-1",
            &client_connections,
            &connection_id,
            &shutdown_signals,
            &soft_interrupt_queues,
            &event_history,
            &event_counter,
            &swarm_event_tx,
        )
        .await
        .expect("cleanup succeeds");

        CleanupFixture {
            sessions,
            client_connections,
            swarm_members,
            shutdown_signals,
        }
    }

    #[tokio::test]
    async fn detach_keeps_the_live_session_but_drops_the_connection() {
        let fixture = run_cleanup("detach-session", true).await;
        assert!(
            fixture.sessions.read().await.contains_key("detach-session"),
            "detach must leave the live agent attachable"
        );
        assert!(
            fixture
                .shutdown_signals
                .read()
                .await
                .contains_key("detach-session"),
            "detach must leave the session's interrupt signal registered"
        );
        assert!(
            fixture.client_connections.read().await.is_empty(),
            "detach must surrender connection ownership"
        );
        assert!(fixture.swarm_members.read().await.is_empty());
    }

    #[tokio::test]
    async fn ordinary_disconnect_still_removes_the_live_session() {
        let fixture = run_cleanup("closed-session", false).await;
        assert!(
            !fixture.sessions.read().await.contains_key("closed-session"),
            "a non-detach disconnect must keep removing the live agent"
        );
        assert!(
            !fixture
                .shutdown_signals
                .read()
                .await
                .contains_key("closed-session"),
            "a non-detach disconnect must keep clearing the interrupt signal"
        );
        assert!(fixture.client_connections.read().await.is_empty());
    }
    /// Issue #133, the attach/detach race in its destructive direction.
    ///
    /// A detach is accepted before the detaching connection's record leaves the
    /// map: the Ack write and the request-loop teardown both happen in between. If
    /// a *different* client's ordinary disconnect is cleaned up inside that window,
    /// the successor check must not mistake the detaching record for a client that
    /// is still attached. Getting this wrong strands the session live with nobody
    /// on it and no detach ever requested for it, which the idle-exit monitor then
    /// keeps alive because it counts connections rather than sessions.
    #[tokio::test]
    async fn a_connection_mid_detach_is_not_a_live_successor() {
        let fixture =
            run_cleanup_with_extra_connection("stranded-session", false, Some(true)).await;
        assert!(
            !fixture
                .sessions
                .read()
                .await
                .contains_key("stranded-session"),
            "a connection whose detach was accepted must not keep the session alive"
        );
    }

    /// Positive control for the test above: an ordinary second client in exactly
    /// the same position *must* still keep the session alive, so the assertion
    /// there is about the detach flag and not about successor checks in general.
    #[tokio::test]
    async fn an_ordinary_second_connection_is_still_a_live_successor() {
        let fixture = run_cleanup_with_extra_connection("shared-session", false, Some(false)).await;
        assert!(
            fixture.sessions.read().await.contains_key("shared-session"),
            "a genuinely attached second client must keep the live session"
        );
    }
}
