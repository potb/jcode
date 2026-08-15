//! Socket-level detach tests: they drive `Request::Detach` over a real client
//! socket and read the server's own live-session count from the debug socket.

use crate::test_support::*;

async fn start_inprocess_server(
    label: &str,
) -> Result<(
    std::path::PathBuf,
    std::path::PathBuf,
    tokio::task::JoinHandle<Result<()>>,
)> {
    let runtime_dir = short_runtime_dir(format!(
        "jcode-detach-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir)?;
    let socket_path = runtime_dir.join("jcode.sock");
    let debug_socket_path = runtime_dir.join("jcode-debug.sock");

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new());
    let server_instance =
        server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });
    wait_for_socket(&socket_path).await?;
    Ok((socket_path, debug_socket_path, server_handle))
}

async fn subscribe_new_session(socket_path: &std::path::Path) -> Result<(server::Client, String)> {
    let mut client = server::Client::connect_with_path(socket_path.to_path_buf()).await?;
    let sub = client.subscribe().await?;
    let _ = collect_until_done_unix(&mut client, sub).await?;
    let history = client.get_history_event().await?;
    let session_id = match &history {
        ServerEvent::History { session_id, .. } => session_id.clone(),
        other => anyhow::bail!("expected history, got {other:?}"),
    };
    Ok((client, session_id))
}

/// Returns the server's own (live_sessions, connected_clients) counts.
async fn live_session_population(debug_socket_path: &std::path::Path) -> Result<(u64, u64)> {
    let mut debug =
        server::Client::connect_debug_with_path(debug_socket_path.to_path_buf()).await?;
    let id = debug.debug_command("memory-incident", None).await?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match timeout(Duration::from_millis(500), debug.read_event()).await {
            Ok(Ok(ServerEvent::DebugResponse {
                id: response_id,
                output,
                ..
            })) if response_id == id => {
                let payload: serde_json::Value = serde_json::from_str(&output)?;
                let population = &payload["population"];
                let live = population["live_sessions"]
                    .as_u64()
                    .context("live_sessions missing from debug memory payload")?;
                let connected = population["connected_clients"]
                    .as_u64()
                    .context("connected_clients missing from debug memory payload")?;
                return Ok((live, connected));
            }
            Ok(Ok(_)) => continue,
            Ok(Err(err)) => return Err(err),
            Err(_) => continue,
        }
    }
    anyhow::bail!("timed out waiting for debug memory response")
}

/// Polls until the counts match, so assertions do not race disconnect cleanup.
async fn wait_for_population(
    debug_socket_path: &std::path::Path,
    expected_live: u64,
    expected_connected: u64,
) -> Result<(u64, u64)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = (u64::MAX, u64::MAX);
    while Instant::now() < deadline {
        last = live_session_population(debug_socket_path).await?;
        if last == (expected_live, expected_connected) {
            return Ok(last);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(last)
}

/// The behaviour the feature exists for: detach, disconnect, session stays live.
/// `dropping_the_socket_ends_the_session` is the same flow without the request.
#[tokio::test]
async fn detach_then_disconnect_leaves_the_session_live_on_the_server() -> Result<()> {
    let _env = setup_test_env()?;
    let (socket_path, debug_socket_path, server_handle) = start_inprocess_server("live").await?;

    let result = async {
        let (mut client, session_id) = subscribe_new_session(&socket_path).await?;
        let (live_before, connected_before) = wait_for_population(&debug_socket_path, 1, 1).await?;
        assert_eq!(
            (live_before, connected_before),
            (1, 1),
            "expected exactly one live session with one attached client before detaching"
        );

        let ack_id = client.detach(&session_id, None).await?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut acked = false;
        while Instant::now() < deadline {
            match timeout(Duration::from_millis(500), client.read_event()).await {
                Ok(Ok(ServerEvent::Ack { id })) if id == ack_id => {
                    acked = true;
                    break;
                }
                Ok(Ok(ServerEvent::Error { id, message, .. })) if id == ack_id => {
                    anyhow::bail!("detach rejected: {message}")
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
        assert!(acked, "server never acknowledged the detach request");

        drop(client);

        let (live_after, connected_after) = wait_for_population(&debug_socket_path, 1, 0).await?;
        assert_eq!(
            connected_after, 0,
            "the detaching client must no longer be counted as attached"
        );
        assert_eq!(
            live_after, 1,
            "the session must stay live after a detach so it can be attached again"
        );
        Ok(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    result
}

/// Negative control: without a `Detach`, the very same disconnect must keep
/// removing the live session, which is what makes the test above meaningful.
#[tokio::test]
async fn dropping_the_socket_ends_the_session() -> Result<()> {
    let _env = setup_test_env()?;
    let (socket_path, debug_socket_path, server_handle) = start_inprocess_server("control").await?;

    let result = async {
        let (client, _session_id) = subscribe_new_session(&socket_path).await?;
        let (live_before, connected_before) = wait_for_population(&debug_socket_path, 1, 1).await?;
        assert_eq!((live_before, connected_before), (1, 1));

        drop(client);

        let (live_after, connected_after) = wait_for_population(&debug_socket_path, 0, 0).await?;
        assert_eq!(connected_after, 0);
        assert_eq!(
            live_after, 0,
            "an ordinary disconnect must still remove the live session"
        );
        Ok(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    result
}

/// A detach naming a session this connection does not own must be refused, so a
/// stale client cannot detach a session it lost to a resume takeover.
#[tokio::test]
async fn detach_for_another_session_is_rejected_and_keeps_the_client_attached() -> Result<()> {
    let _env = setup_test_env()?;
    let (socket_path, debug_socket_path, server_handle) =
        start_inprocess_server("wrongsid").await?;

    let result = async {
        let (mut client, _session_id) = subscribe_new_session(&socket_path).await?;
        let bogus_id = client.detach("not-this-session", None).await?;

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut rejected = false;
        while Instant::now() < deadline {
            match timeout(Duration::from_millis(500), client.read_event()).await {
                Ok(Ok(ServerEvent::Error { id, .. })) if id == bogus_id => {
                    rejected = true;
                    break;
                }
                Ok(Ok(ServerEvent::Ack { id })) if id == bogus_id => {
                    anyhow::bail!("server acknowledged a detach for a session it does not own")
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
        assert!(rejected, "expected an Error response for the bogus detach");

        let (live, connected) = wait_for_population(&debug_socket_path, 1, 1).await?;
        assert_eq!(
            (live, connected),
            (1, 1),
            "a rejected detach must leave the client attached"
        );
        Ok(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    result
}
