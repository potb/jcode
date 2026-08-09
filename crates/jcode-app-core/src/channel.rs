use crate::ambient_runner::AmbientRunnerHandle;
use crate::config::SafetyConfig;
use crate::logging;
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait MessageChannel: Send + Sync {
    fn name(&self) -> &str;

    fn is_send_enabled(&self) -> bool;

    fn is_reply_enabled(&self) -> bool;

    async fn send(&self, text: &str) -> anyhow::Result<()>;

    /// Send into a specific existing thread of this channel (for GitHub, an
    /// issue number). Channels with no notion of threads ignore the target and
    /// fall back to a plain send, so callers never have to special-case them.
    async fn send_to_thread(&self, _thread: &str, text: &str) -> anyhow::Result<()> {
        self.send(text).await
    }

    async fn reply_loop(&self, runner: AmbientRunnerHandle);
}

#[derive(Clone)]
pub struct ChannelRegistry {
    channels: Vec<Arc<dyn MessageChannel>>,
}

impl ChannelRegistry {
    pub fn from_config(config: &SafetyConfig) -> Self {
        let mut channels: Vec<Arc<dyn MessageChannel>> = Vec::new();

        if config.telegram_enabled
            && let (Some(token), Some(chat_id)) = (
                config.telegram_bot_token.clone(),
                config.telegram_chat_id.clone(),
            )
        {
            logging::info(&format!(
                "registering telegram notification channel reply_enabled={}",
                config.telegram_reply_enabled
            ));
            channels.push(Arc::new(TelegramChannel::new(
                token,
                chat_id,
                config.telegram_reply_enabled,
            )));
        }

        if config.discord_enabled
            && let (Some(token), Some(channel_id)) = (
                config.discord_bot_token.clone(),
                config.discord_channel_id.clone(),
            )
        {
            logging::info(&format!(
                "registering discord notification channel reply_enabled={}",
                config.discord_reply_enabled
            ));
            channels.push(Arc::new(DiscordChannel::new(
                token,
                channel_id,
                config.discord_reply_enabled,
                config.discord_bot_user_id.clone(),
            )));
        }

        if config.github_enabled {
            match (
                config.github_repo.clone(),
                GitHubChannel::resolve_token(config.github_token.as_deref()),
            ) {
                (Some(repo), Some(token)) => {
                    logging::info(&format!(
                        "registering github notification channel repo={} label={} reply_enabled={}",
                        repo, config.github_label, config.github_reply_enabled
                    ));
                    channels.push(Arc::new(GitHubChannel::new(
                        repo,
                        config.github_label.clone(),
                        token,
                        config.github_allowed_logins.clone(),
                        config.github_reply_enabled,
                        config.github_poll_seconds,
                    )));
                }
                (repo, token) => {
                    logging::warn(&format!(
                        "github_enabled but incomplete (repo={} token={}); skipping",
                        repo.is_some(),
                        token.is_some()
                    ));
                }
            }
        }

        if config.jade_relay_enabled {
            match (
                config.jade_relay_api_base.clone(),
                config.jade_relay_token.clone(),
                config.jade_relay_session_id.clone(),
            ) {
                (Some(api_base), Some(token), Some(session_id)) => {
                    // user_id defaults to the token id when not explicitly set.
                    let user_id = config
                        .jade_relay_user_id
                        .clone()
                        .or_else(|| config.jade_relay_token_id.clone())
                        .unwrap_or_else(|| "default".to_string());
                    logging::info(&format!(
                        "registering jade relay channel user={} session={} reply_enabled={}",
                        user_id, session_id, config.jade_relay_reply_enabled
                    ));
                    channels.push(Arc::new(JadeRelayChannel::new(
                        api_base,
                        token,
                        config.jade_relay_token_id.clone(),
                        user_id,
                        session_id,
                        config.jade_relay_reply_enabled,
                    )));
                }
                _ => {
                    logging::warn(
                        "jade_relay_enabled but api_base/token/session_id incomplete; skipping",
                    );
                }
            }
        }

        logging::debug(&format!(
            "channel registry initialized channel_count={}",
            channels.len()
        ));
        Self { channels }
    }

    pub fn send_all(&self, text: &str) {
        if tokio::runtime::Handle::try_current().is_err() {
            logging::warn("skipping channel send_all because no Tokio runtime is active");
            return;
        }
        for ch in self.channels.iter().filter(|c| c.is_send_enabled()) {
            let ch = Arc::clone(ch);
            let text = text.to_string();
            tokio::spawn(async move {
                logging::debug(&format!("sending notification via {}", ch.name()));
                if let Err(e) = ch.send(&text).await {
                    logging::error(&format!("{} notification failed: {}", ch.name(), e));
                }
            });
        }
    }

    pub fn spawn_reply_loops(&self, runner: &AmbientRunnerHandle) {
        for ch in self.channels.iter().filter(|c| c.is_reply_enabled()) {
            let ch = Arc::clone(ch);
            let runner = runner.clone();
            tokio::spawn(async move {
                logging::info(&format!("{} reply loop spawned", ch.name()));
                ch.reply_loop(runner).await;
            });
        }
    }

    pub fn channel_names(&self) -> Vec<String> {
        self.channels.iter().map(|c| c.name().to_string()).collect()
    }

    pub fn find_by_name(&self, name: &str) -> Option<Arc<dyn MessageChannel>> {
        let channel = self.channels.iter().find(|c| c.name() == name).cloned();
        if channel.is_none() {
            logging::debug(&format!("channel lookup missed name={name}"));
        }
        channel
    }

    pub fn send_enabled(&self) -> Vec<Arc<dyn MessageChannel>> {
        self.channels
            .iter()
            .filter(|c| c.is_send_enabled())
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Telegram channel
// ---------------------------------------------------------------------------

pub struct TelegramChannel {
    token: String,
    chat_id: String,
    reply_enabled: bool,
    client: reqwest::Client,
}

impl TelegramChannel {
    pub fn new(token: String, chat_id: String, reply_enabled: bool) -> Self {
        Self {
            token,
            chat_id,
            reply_enabled,
            client: crate::provider::shared_http_client(),
        }
    }
}

#[async_trait]
impl MessageChannel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    fn is_send_enabled(&self) -> bool {
        true
    }

    fn is_reply_enabled(&self) -> bool {
        self.reply_enabled
    }

    async fn send(&self, text: &str) -> anyhow::Result<()> {
        logging::debug(&format!(
            "sending telegram notification bytes={}",
            text.len()
        ));
        crate::telegram::send_message(&self.client, &self.token, &self.chat_id, text).await
    }

    async fn reply_loop(&self, runner: AmbientRunnerHandle) {
        let mut offset: Option<i64> = None;

        loop {
            match crate::telegram::get_updates(&self.client, &self.token, offset, 30).await {
                Ok(updates) => {
                    if !updates.is_empty() {
                        logging::debug(&format!(
                            "telegram reply loop received update_count={}",
                            updates.len()
                        ));
                    }
                    for update in updates {
                        offset = Some(update.update_id + 1);

                        let msg = match update.message {
                            Some(m) => m,
                            None => continue,
                        };

                        if msg.chat.id.to_string() != self.chat_id {
                            continue;
                        }

                        let text = match msg.text {
                            Some(t) => t,
                            None => continue,
                        };

                        let trimmed = text.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        if let Some(req_id) = crate::notifications::extract_permission_id(trimmed) {
                            let (approved, message) =
                                crate::notifications::parse_permission_reply(trimmed);
                            if let Err(e) = crate::safety::record_permission_via_file(
                                &req_id,
                                approved,
                                "telegram_reply",
                                message,
                            ) {
                                logging::error(&format!(
                                    "Failed to record permission from Telegram for {}: {}",
                                    req_id, e
                                ));
                            } else {
                                logging::info(&format!(
                                    "Permission {} via Telegram: {}",
                                    if approved { "approved" } else { "denied" },
                                    req_id
                                ));
                                let _ = self
                                    .send(&format!(
                                        "✅ Permission {} for `{}`",
                                        if approved { "approved" } else { "denied" },
                                        req_id
                                    ))
                                    .await;
                            }
                        } else {
                            let injected = runner.inject_message(trimmed, "telegram").await;
                            logging::info(&format!(
                                "telegram reply injected into session injected={}",
                                injected
                            ));
                            let ack = if injected {
                                format!("💬 Message sent to active session: _{}_", trimmed)
                            } else {
                                format!("📋 Message queued, waking agent: _{}_", trimmed)
                            };
                            let _ = self.send(&ack).await;
                        }
                    }
                }
                Err(e) => {
                    logging::error(&format!("Telegram poll error: {}", e));
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Discord channel
// ---------------------------------------------------------------------------

pub struct DiscordChannel {
    token: String,
    channel_id: String,
    reply_enabled: bool,
    bot_user_id: Option<String>,
    client: reqwest::Client,
}

impl DiscordChannel {
    pub fn new(
        token: String,
        channel_id: String,
        reply_enabled: bool,
        bot_user_id: Option<String>,
    ) -> Self {
        Self {
            token,
            channel_id,
            reply_enabled,
            bot_user_id,
            client: crate::provider::shared_http_client(),
        }
    }

    async fn poll_messages(&self, after: Option<&str>) -> anyhow::Result<Vec<DiscordMessage>> {
        logging::debug(&format!(
            "polling discord messages after_present={}",
            after.is_some()
        ));
        let mut url = format!(
            "https://discord.com/api/v10/channels/{}/messages?limit=10",
            self.channel_id
        );
        if let Some(after_id) = after {
            url.push_str(&format!("&after={}", after_id));
        }

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bot {}", self.token))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            logging::warn(&format!("discord message poll returned status={status}"));
            anyhow::bail!("Discord messages error ({}): {}", status, body);
        }

        let messages: Vec<DiscordMessage> = resp.json().await?;
        logging::debug(&format!(
            "discord message poll returned count={}",
            messages.len()
        ));
        Ok(messages)
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DiscordMessage {
    pub id: String,
    pub content: String,
    pub author: DiscordAuthor,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DiscordAuthor {
    pub id: String,
    pub bot: Option<bool>,
}

#[async_trait]
impl MessageChannel for DiscordChannel {
    fn name(&self) -> &str {
        "discord"
    }

    fn is_send_enabled(&self) -> bool {
        true
    }

    fn is_reply_enabled(&self) -> bool {
        self.reply_enabled
    }

    async fn send(&self, text: &str) -> anyhow::Result<()> {
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            self.channel_id
        );
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.token))
            .json(&serde_json::json!({ "content": text }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Discord API error ({}): {}", status, body);
        }

        logging::info("Discord notification sent");
        Ok(())
    }

    async fn reply_loop(&self, runner: AmbientRunnerHandle) {
        let mut last_seen_id: Option<String> = None;

        // Get the latest message ID on startup so we don't replay old messages
        match self.poll_messages(None).await {
            Ok(msgs) => {
                if let Some(latest) = msgs.first() {
                    last_seen_id = Some(latest.id.clone());
                }
            }
            Err(e) => {
                logging::error(&format!("Discord initial poll error: {}", e));
            }
        }

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;

            match self.poll_messages(last_seen_id.as_deref()).await {
                Ok(msgs) => {
                    // Discord returns newest first, reverse for chronological order
                    let mut msgs = msgs;
                    msgs.reverse();

                    for msg in msgs {
                        last_seen_id = Some(msg.id.clone());

                        // Skip messages from bots (including ourselves)
                        if msg.author.bot.unwrap_or(false) {
                            continue;
                        }

                        // If we know our bot user ID, also skip our own messages
                        if let Some(ref bot_id) = self.bot_user_id
                            && msg.author.id == *bot_id
                        {
                            continue;
                        }

                        let trimmed = msg.content.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        if let Some(req_id) = crate::notifications::extract_permission_id(trimmed) {
                            let (approved, message) =
                                crate::notifications::parse_permission_reply(trimmed);
                            if let Err(e) = crate::safety::record_permission_via_file(
                                &req_id,
                                approved,
                                "discord_reply",
                                message,
                            ) {
                                logging::error(&format!(
                                    "Failed to record permission from Discord for {}: {}",
                                    req_id, e
                                ));
                            } else {
                                logging::info(&format!(
                                    "Permission {} via Discord: {}",
                                    if approved { "approved" } else { "denied" },
                                    req_id
                                ));
                                let _ = self
                                    .send(&format!(
                                        "✅ Permission {} for `{}`",
                                        if approved { "approved" } else { "denied" },
                                        req_id
                                    ))
                                    .await;
                            }
                        } else {
                            let injected = runner.inject_message(trimmed, "discord").await;
                            logging::info(&format!(
                                "discord reply injected into session injected={}",
                                injected
                            ));
                            let ack = if injected {
                                format!("💬 Message sent to active session: *{}*", trimmed)
                            } else {
                                format!("📋 Message queued, waking agent: *{}*", trimmed)
                            };
                            let _ = self.send(&ack).await;
                        }
                    }
                }
                Err(e) => {
                    logging::error(&format!("Discord poll error: {}", e));
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Jade cloud relay channel
// ---------------------------------------------------------------------------

/// Remote control via the Jade cloud relay (an append-only per-session event
/// log in AWS). Unlike the WebSocket gateway, nothing listens on this machine:
/// the laptop only makes outbound long-poll requests, so there is no inbound
/// port to attack. A cloud client posts `prompt` events; this channel injects
/// them into the live session and posts the agent's reply back as a `response`
/// event for the cloud client to read.
pub struct JadeRelayChannel {
    /// API base URL, normalized to end with a single '/'.
    api_base: String,
    token: String,
    token_id: Option<String>,
    user_id: String,
    session_id: String,
    reply_enabled: bool,
    client: reqwest::Client,
}

impl JadeRelayChannel {
    pub fn new(
        api_base: String,
        token: String,
        token_id: Option<String>,
        user_id: String,
        session_id: String,
        reply_enabled: bool,
    ) -> Self {
        let api_base = if api_base.ends_with('/') {
            api_base
        } else {
            format!("{}/", api_base)
        };
        Self {
            api_base,
            token,
            token_id,
            user_id,
            session_id,
            reply_enabled,
            client: crate::provider::shared_http_client(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.api_base, path.trim_start_matches('/'))
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = req.header("Authorization", format!("Bearer {}", self.token));
        if let Some(id) = &self.token_id {
            req = req.header("x-jade-token-id", id);
        }
        req
    }

    /// Register/heartbeat this device so the cloud can show it as online.
    async fn heartbeat(&self, device_id: &str) {
        let body = serde_json::json!({
            "user_id": self.user_id,
            "device_id": device_id,
            "label": device_id,
            "platform": std::env::consts::OS,
        });
        let req = self.auth(self.client.post(self.url("v1/devices")).json(&body));
        if let Err(e) = req.send().await {
            logging::debug(&format!("jade relay heartbeat failed: {}", e));
        }
    }

    /// Long-poll for new prompt events after `after`. Returns (events, next_after).
    /// `wait` is the server-side long-poll window in seconds (capped at 25 by the relay).
    async fn poll_prompts(&self, after: i64, wait: u32) -> anyhow::Result<(Vec<RelayEvent>, i64)> {
        let session = urlencoding_encode(&self.session_id);
        let url = self.url(&format!(
            "v1/sessions/{}/events?user_id={}&after={}&types=prompt&wait={}",
            session,
            urlencoding_encode(&self.user_id),
            after,
            wait
        ));
        let resp = self.auth(self.client.get(&url)).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("jade relay poll error ({}): {}", status, body);
        }
        let parsed: RelayEventsResponse = resp.json().await?;
        Ok((parsed.events, parsed.next_after))
    }

    /// Post a response event back to the relay for the cloud client to read.
    async fn post_response(&self, text: &str, request_seq: i64) -> anyhow::Result<()> {
        let session = urlencoding_encode(&self.session_id);
        let body = serde_json::json!({
            "user_id": self.user_id,
            "type": "response",
            "text": text,
            "request_seq": request_seq,
            "origin": "jcode",
        });
        let resp = self
            .auth(
                self.client
                    .post(self.url(&format!("v1/sessions/{}/events", session)))
                    .json(&body),
            )
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            anyhow::bail!("jade relay post error ({}): {}", status, detail);
        }
        Ok(())
    }
}

#[derive(Debug, serde::Deserialize)]
struct RelayEventsResponse {
    #[serde(default)]
    events: Vec<RelayEvent>,
    #[serde(default)]
    next_after: i64,
}

#[derive(Debug, serde::Deserialize)]
struct RelayEvent {
    #[serde(default)]
    seq: i64,
    #[serde(default)]
    text: Option<String>,
}

/// Minimal percent-encoding for path/query segments (alnum and -_.~ pass through).
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[async_trait]
impl MessageChannel for JadeRelayChannel {
    fn name(&self) -> &str {
        "jade_relay"
    }

    fn is_send_enabled(&self) -> bool {
        true
    }

    fn is_reply_enabled(&self) -> bool {
        // Inbound Jade relay prompts are delivered by server::jade_relay so they
        // work even when ambient mode is disabled and target the configured live
        // Jcode session directly. Keep this channel for outbound notifications
        // only; otherwise ambient mode would start a second poller.
        let _configured_for_server_listener = self.reply_enabled;
        false
    }

    async fn send(&self, text: &str) -> anyhow::Result<()> {
        // Cloud notifications (e.g. ambient cycle summaries) are posted as a
        // response event with request_seq=0 (not tied to a specific prompt).
        self.post_response(text, 0).await
    }

    async fn reply_loop(&self, runner: AmbientRunnerHandle) {
        let host = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "laptop".to_string());
        let device_id = format!("jcode-{}", host);
        logging::info(&format!(
            "jade relay reply loop started channel={}/{}",
            self.user_id, self.session_id
        ));
        // Start after the latest existing prompt so we don't replay history.
        let mut after: i64 = match self.poll_prompts(0, 0).await {
            Ok((_, next)) => next,
            Err(e) => {
                logging::error(&format!("jade relay init poll failed: {}", e));
                0
            }
        };
        let mut last_heartbeat = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(60))
            .unwrap_or_else(std::time::Instant::now);

        loop {
            if last_heartbeat.elapsed() >= std::time::Duration::from_secs(30) {
                self.heartbeat(&device_id).await;
                last_heartbeat = std::time::Instant::now();
            }
            match self.poll_prompts(after, 20).await {
                Ok((events, next_after)) => {
                    after = next_after;
                    for ev in events {
                        let text = ev.text.unwrap_or_default();
                        let trimmed = text.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some(req_id) = crate::notifications::extract_permission_id(trimmed) {
                            let (approved, message) =
                                crate::notifications::parse_permission_reply(trimmed);
                            if let Err(e) = crate::safety::record_permission_via_file(
                                &req_id,
                                approved,
                                "jade_relay",
                                message,
                            ) {
                                logging::error(&format!(
                                    "Failed to record permission from jade relay for {}: {}",
                                    req_id, e
                                ));
                            } else {
                                let _ = self
                                    .post_response(
                                        &format!(
                                            "Permission {} for {}",
                                            if approved { "approved" } else { "denied" },
                                            req_id
                                        ),
                                        ev.seq,
                                    )
                                    .await;
                            }
                            continue;
                        }
                        let injected = runner.inject_message(trimmed, "jade_relay").await;
                        logging::info(&format!(
                            "jade relay prompt injected seq={} injected={}",
                            ev.seq, injected
                        ));
                        let ack = if injected {
                            "Message delivered to active session."
                        } else {
                            "Message queued; waking agent."
                        };
                        if let Err(e) = self.post_response(ack, ev.seq).await {
                            logging::error(&format!("jade relay ack post failed: {}", e));
                        }
                    }
                }
                Err(e) => {
                    logging::error(&format!("jade relay poll error: {}", e));
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
            }
        }
    }
}


// ---------------------------------------------------------------------------
// GitHub issue channel
// ---------------------------------------------------------------------------

/// A notification channel backed by GitHub issues, one issue per topic.
///
/// Every other reply channel needs a bot and a hosted service to be reachable
/// from a phone. GitHub is already reachable, already authenticates the user,
/// and a private repo (or an allowlist of logins) limits who can answer.
///
/// Topics get their own issue rather than sharing one mailbox thread, because
/// a single thread collapses unrelated questions into one stream where the
/// reply "yes" is ambiguous and nothing is ever done. An issue per topic gives
/// each question its own reply thread, its own notification, and a close button
/// that means "this one is settled".
pub struct GitHubChannel {
    repo: String,
    label: String,
    token: String,
    /// Logins allowed to issue directives. Empty means "anyone except the
    /// account we post as", which on a private repo is the collaborator set.
    allowed_logins: Vec<String>,
    reply_enabled: bool,
    poll_seconds: u64,
    client: reqwest::Client,
}

impl GitHubChannel {
    pub fn new(
        repo: String,
        label: String,
        token: String,
        allowed_logins: Vec<String>,
        reply_enabled: bool,
        poll_seconds: u64,
    ) -> Self {
        Self {
            repo,
            label,
            token,
            allowed_logins,
            reply_enabled,
            poll_seconds: poll_seconds.max(5),
            client: crate::provider::shared_http_client(),
        }
    }

    /// Resolve a token from config, the usual env vars, or the `gh` CLI.
    ///
    /// Falling back to `gh auth token` matters because the user already has a
    /// working credential there; requiring a second PAT pasted into a config
    /// file is both friction and one more secret at rest.
    pub fn resolve_token(configured: Option<&str>) -> Option<String> {
        if let Some(t) = configured.map(str::trim).filter(|t| !t.is_empty()) {
            return Some(t.to_string());
        }
        for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
            if let Ok(v) = std::env::var(var)
                && !v.trim().is_empty()
            {
                return Some(v.trim().to_string());
            }
        }
        let out = std::process::Command::new("gh")
            .args(["auth", "token"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if token.is_empty() { None } else { Some(token) }
    }

    fn request(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "jcode-ambient")
    }

    /// The login of the account the token belongs to, used to skip our own
    /// comments without the user having to configure it.
    async fn viewer_login(&self) -> Option<String> {
        let resp = self
            .request(self.client.get("https://api.github.com/user"))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let user: GitHubUser = resp.json().await.ok()?;
        Some(user.login)
    }

    /// Open a new topic issue and return its number.
    pub async fn open_issue(&self, title: &str, body: &str) -> anyhow::Result<u64> {
        let url = format!("https://api.github.com/repos/{}/issues", self.repo);
        let resp = self
            .request(self.client.post(&url))
            .json(&serde_json::json!({
                "title": title,
                "body": body,
                "labels": [self.label],
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub issue create failed ({}): {}", status, body);
        }
        let issue: GitHubIssue = resp.json().await?;
        Ok(issue.number)
    }

    /// Comment on an existing topic issue.
    pub async fn comment(&self, issue: u64, body: &str) -> anyhow::Result<()> {
        let url = format!(
            "https://api.github.com/repos/{}/issues/{}/comments",
            self.repo, issue
        );
        let resp = self
            .request(self.client.post(&url))
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub comment post failed ({}): {}", status, body);
        }
        Ok(())
    }

    /// Close a topic issue once it is settled.
    pub async fn close_issue(&self, issue: u64) -> anyhow::Result<()> {
        let url = format!(
            "https://api.github.com/repos/{}/issues/{}",
            self.repo, issue
        );
        let resp = self
            .request(self.client.patch(&url))
            .json(&serde_json::json!({ "state": "closed" }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub issue close failed ({}): {}", status, body);
        }
        Ok(())
    }

    /// Numbers of the open issues that belong to this channel.
    ///
    /// Comments are polled repo-wide (one request instead of one per issue), so
    /// this is the filter that keeps unrelated issue traffic from being read as
    /// directives to the agent.
    async fn topic_issue_numbers(&self) -> anyhow::Result<std::collections::HashSet<u64>> {
        let url = format!(
            "https://api.github.com/repos/{}/issues?state=open&labels={}&per_page=100",
            self.repo,
            urlencoding_encode(&self.label)
        );
        let resp = self.request(self.client.get(&url)).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub issue list failed ({}): {}", status, body);
        }
        let issues: Vec<GitHubIssue> = resp.json().await?;
        Ok(issues.into_iter().map(|i| i.number).collect())
    }

    /// Open topic issues as (number, title) pairs, for the agent's backlog.
    pub async fn list_open_topics(&self) -> anyhow::Result<Vec<(u64, String)>> {
        let url = format!(
            "https://api.github.com/repos/{}/issues?state=open&labels={}&per_page=100",
            self.repo,
            urlencoding_encode(&self.label)
        );
        let resp = self.request(self.client.get(&url)).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub issue list failed ({}): {}", status, body);
        }
        let issues: Vec<GitHubIssue> = resp.json().await?;
        Ok(issues.into_iter().map(|i| (i.number, i.title)).collect())
    }

    /// All issue comments in the repo created at or after `since` (RFC3339).
    async fn poll_comments(&self, since: &str) -> anyhow::Result<Vec<GitHubComment>> {
        let url = format!(
            "https://api.github.com/repos/{}/issues/comments?since={}&per_page=100&sort=created&direction=asc",
            self.repo,
            urlencoding_encode(since)
        );
        let resp = self.request(self.client.get(&url)).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub comments error ({}): {}", status, body);
        }
        Ok(resp.json().await?)
    }

    fn is_allowed(&self, login: &str, viewer: Option<&str>) -> bool {
        if !self.allowed_logins.is_empty() {
            return self
                .allowed_logins
                .iter()
                .any(|l| l.eq_ignore_ascii_case(login));
        }
        viewer
            .map(|v| !v.eq_ignore_ascii_case(login))
            .unwrap_or(true)
    }
}

/// Split a message into an issue title and body.
///
/// The first line becomes the title so the issue list reads as a list of
/// topics rather than a wall of identical subjects. GitHub rejects very long
/// titles, so an overlong first line is truncated and kept in full in the body.
pub fn split_title_body(text: &str) -> (String, String) {
    let trimmed = text.trim();
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    let mut title: String = first_line
        .trim_start_matches(['#', '*', '-', ' '])
        .chars()
        .take(120)
        .collect();
    if title.trim().is_empty() {
        title = "Ambient update".to_string();
    }
    (title.trim().to_string(), trimmed.to_string())
}

/// Parse an issue number out of a comment's `issue_url`.
pub fn issue_number_from_url(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

#[derive(Debug, serde::Deserialize)]
struct GitHubUser {
    #[serde(default)]
    login: String,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubIssue {
    #[serde(default)]
    number: u64,
    #[serde(default)]
    title: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct GitHubComment {
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub issue_url: String,
    #[serde(default)]
    pub user: Option<GitHubCommentUser>,
}

#[derive(Debug, serde::Deserialize)]
pub struct GitHubCommentUser {
    #[serde(default)]
    pub login: String,
}

#[async_trait]
impl MessageChannel for GitHubChannel {
    fn name(&self) -> &str {
        "github"
    }

    fn is_send_enabled(&self) -> bool {
        true
    }

    fn is_reply_enabled(&self) -> bool {
        self.reply_enabled
    }

    /// A plain send opens a new topic issue: each notification is its own
    /// thread the user can answer or close independently.
    async fn send(&self, text: &str) -> anyhow::Result<()> {
        let (title, body) = split_title_body(text);
        self.open_issue(&title, &body).await?;
        Ok(())
    }

    async fn send_to_thread(&self, thread: &str, text: &str) -> anyhow::Result<()> {
        let issue: u64 = thread
            .trim()
            .trim_start_matches('#')
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid GitHub issue number: {}", thread))?;
        self.comment(issue, text).await
    }

    async fn reply_loop(&self, runner: AmbientRunnerHandle) {
        let viewer = self.viewer_login().await;
        logging::info(&format!(
            "github reply loop started repo={} label={} viewer={}",
            self.repo,
            self.label,
            viewer.as_deref().unwrap_or("unknown")
        ));

        // Start from now so switching this on does not replay an existing
        // backlog as a burst of stale directives.
        let mut since = chrono::Utc::now().to_rfc3339();

        loop {
            let topics = match self.topic_issue_numbers().await {
                Ok(t) => t,
                Err(e) => {
                    logging::error(&format!("GitHub issue list error: {}", e));
                    tokio::time::sleep(std::time::Duration::from_secs(self.poll_seconds)).await;
                    continue;
                }
            };

            match self.poll_comments(&since).await {
                Ok(comments) => {
                    for c in comments {
                        if c.created_at > since {
                            since = c.created_at.clone();
                        }
                        let issue = match issue_number_from_url(&c.issue_url) {
                            Some(n) if topics.contains(&n) => n,
                            _ => continue,
                        };
                        let login = c.user.as_ref().map(|u| u.login.clone()).unwrap_or_default();
                        if !self.is_allowed(&login, viewer.as_deref()) {
                            continue;
                        }
                        let trimmed = c.body.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        if let Some(req_id) = crate::notifications::extract_permission_id(trimmed) {
                            let (approved, message) =
                                crate::notifications::parse_permission_reply(trimmed);
                            if let Err(e) = crate::safety::record_permission_via_file(
                                &req_id,
                                approved,
                                "github_issue",
                                message,
                            ) {
                                logging::error(&format!(
                                    "Failed to record permission from GitHub for {}: {}",
                                    req_id, e
                                ));
                            } else {
                                let _ = self
                                    .comment(
                                        issue,
                                        &format!(
                                            "Permission {} for `{}`.",
                                            if approved { "approved" } else { "denied" },
                                            req_id
                                        ),
                                    )
                                    .await;
                            }
                            continue;
                        }

                        // Tag the source with the issue so the agent knows which
                        // topic is being answered and can reply in that thread.
                        let source = format!("github#{}", issue);
                        let injected = runner.inject_message(trimmed, &source).await;
                        logging::info(&format!(
                            "github comment injected issue={} from={} injected={}",
                            issue, login, injected
                        ));
                        let ack = if injected {
                            "Delivered to the running cycle."
                        } else {
                            "Queued; waking the agent now."
                        };
                        if let Err(e) = self.comment(issue, ack).await {
                            logging::error(&format!("github ack failed: {}", e));
                        }
                    }
                }
                Err(e) => {
                    logging::error(&format!("GitHub poll error: {}", e));
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(self.poll_seconds)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discord_message_parse() {
        let json = r#"{
            "id": "123456",
            "content": "hello agent",
            "author": {"id": "789", "bot": false}
        }"#;
        let msg: DiscordMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.id, "123456");
        assert_eq!(msg.content, "hello agent");
        assert!(!msg.author.bot.unwrap());
    }

    #[test]
    fn test_discord_bot_message_parse() {
        let json = r#"{
            "id": "999",
            "content": "bot response",
            "author": {"id": "111", "bot": true}
        }"#;
        let msg: DiscordMessage = serde_json::from_str(json).unwrap();
        assert!(msg.author.bot.unwrap());
    }

    #[test]
    fn test_relay_events_parse() {
        let json = r#"{
            "events": [
                {"seq": 5, "type": "prompt", "text": "run the tests"},
                {"seq": 6, "type": "prompt", "text": "now lint"}
            ],
            "next_after": 6
        }"#;
        let parsed: RelayEventsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.events[0].seq, 5);
        assert_eq!(parsed.events[0].text.as_deref(), Some("run the tests"));
        assert_eq!(parsed.next_after, 6);
    }

    #[test]
    fn test_relay_events_empty() {
        let json = r#"{"events": [], "next_after": 0}"#;
        let parsed: RelayEventsResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.events.is_empty());
        assert_eq!(parsed.next_after, 0);
    }

    #[test]
    fn test_relay_url_encoding() {
        assert_eq!(urlencoding_encode("sess-relay-test"), "sess-relay-test");
        assert_eq!(urlencoding_encode("a/b c"), "a%2Fb%20c");
        assert_eq!(urlencoding_encode("user.name~1_2"), "user.name~1_2");
    }

    #[test]
    fn test_relay_url_join() {
        let ch = JadeRelayChannel::new(
            "https://example.com/api".to_string(),
            "tok".to_string(),
            Some("jeremy".to_string()),
            "jeremy".to_string(),
            "sess-1".to_string(),
            true,
        );
        assert_eq!(ch.url("v1/devices"), "https://example.com/api/v1/devices");
        assert_eq!(ch.url("/v1/devices"), "https://example.com/api/v1/devices");
    }

    #[test]
    fn test_relay_registry_wiring() {
        // Disabled: not registered.
        let cfg = SafetyConfig::default();
        let reg = ChannelRegistry::from_config(&cfg);
        assert!(!reg.channel_names().iter().any(|n| n == "jade_relay"));

        // Enabled but incomplete: skipped with a warning.
        let mut cfg = SafetyConfig {
            jade_relay_enabled: true,
            ..SafetyConfig::default()
        };
        let reg = ChannelRegistry::from_config(&cfg);
        assert!(!reg.channel_names().iter().any(|n| n == "jade_relay"));

        // Enabled and complete: registered.
        cfg.jade_relay_api_base = Some("https://example.com/".to_string());
        cfg.jade_relay_token = Some("tok".to_string());
        cfg.jade_relay_session_id = Some("sess-1".to_string());
        let reg = ChannelRegistry::from_config(&cfg);
        assert!(reg.channel_names().iter().any(|n| n == "jade_relay"));
    }

    #[test]
    fn test_github_registry_wiring() {
        let mut cfg = SafetyConfig {
            github_enabled: true,
            github_token: Some("tok".to_string()),
            ..SafetyConfig::default()
        };
        // Incomplete: skipped rather than half-registered.
        let reg = ChannelRegistry::from_config(&cfg);
        assert!(!reg.channel_names().iter().any(|n| n == "github"));

        cfg.github_repo = Some("owner/repo".to_string());
        let reg = ChannelRegistry::from_config(&cfg);
        assert!(reg.channel_names().iter().any(|n| n == "github"));
    }

    #[test]
    fn test_github_split_title_body() {
        // First line becomes the title so the issue list reads as topics.
        let (t, b) = split_title_body("## Flaky test in bg_panel\nDetails here.");
        assert_eq!(t, "Flaky test in bg_panel");
        assert!(b.contains("Details here."));

        // An overlong first line is truncated for the title but kept in full
        // in the body, since GitHub rejects very long titles.
        let long = "x".repeat(400);
        let (t, b) = split_title_body(&long);
        assert_eq!(t.chars().count(), 120);
        assert_eq!(b.chars().count(), 400);

        // Leading blank lines are skipped rather than producing a blank title.
        let (t, _) = split_title_body("   \n\nbody only");
        assert_eq!(t, "body only");

        // Content with no usable first line still yields a title, because
        // GitHub rejects an empty one.
        let (t, _) = split_title_body("### \nrest");
        assert_eq!(t, "Ambient update");
    }

    #[test]
    fn test_github_issue_number_from_url() {
        assert_eq!(
            issue_number_from_url("https://api.github.com/repos/o/r/issues/42"),
            Some(42)
        );
        assert_eq!(issue_number_from_url("nonsense"), None);
    }

    #[test]
    fn test_github_allowed_logins() {
        // Empty allowlist: anyone except the account we post as, so the agent
        // never answers its own comment and loops.
        let ch = GitHubChannel::new(
            "owner/repo".to_string(),
            "ambient".to_string(),
            "tok".to_string(),
            Vec::new(),
            true,
            60,
        );
        assert!(ch.is_allowed("potb", Some("jcode-bot")));
        assert!(!ch.is_allowed("jcode-bot", Some("jcode-bot")));
        assert!(!ch.is_allowed("JCODE-BOT", Some("jcode-bot")));

        // Explicit allowlist wins and is case-insensitive.
        let ch = GitHubChannel::new(
            "owner/repo".to_string(),
            "ambient".to_string(),
            "tok".to_string(),
            vec!["PotB".to_string()],
            true,
            60,
        );
        assert!(ch.is_allowed("potb", Some("jcode-bot")));
        assert!(!ch.is_allowed("stranger", Some("jcode-bot")));
    }

    #[test]
    fn test_github_comment_parse() {
        let json = r#"[{
            "body": "ship it",
            "created_at": "2026-08-09T15:00:00Z",
            "user": {"login": "potb"}
        }]"#;
        let parsed: Vec<GitHubComment> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed[0].body, "ship it");
        assert_eq!(parsed[0].user.as_ref().unwrap().login, "potb");
    }

    #[test]
    fn test_github_poll_interval_floor() {
        // A zero or tiny interval would hammer the API into rate limiting.
        let ch = GitHubChannel::new(
            "o/r".to_string(),
            "ambient".to_string(),
            "t".to_string(),
            Vec::new(),
            true,
            0,
        );
        assert_eq!(ch.poll_seconds, 5);
    }

    /// Poll a condition for up to ~15s, for APIs that are eventually consistent.
    async fn wait_for<F, Fut>(mut f: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..15 {
            if f().await {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        false
    }

    /// Live end-to-end test against real GitHub: open a topic issue, comment
    /// on it, see it in the open-topic list, then close it. Ignored by default.
    ///   JCODE_GH_LIVE_REPO=owner/repo cargo test -p jcode-app-core \
    ///     github_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires a real GitHub token and repo"]
    async fn test_github_live_roundtrip() {
        let repo = match std::env::var("JCODE_GH_LIVE_REPO") {
            Ok(v) => v,
            Err(_) => {
                eprintln!("skipping: JCODE_GH_LIVE_REPO not set");
                return;
            }
        };
        let token = GitHubChannel::resolve_token(None).expect("a GitHub token");
        let ch = GitHubChannel::new(
            repo,
            "ambient".to_string(),
            token,
            vec!["potb".to_string()],
            true,
            60,
        );

        let title = format!("Live channel test {}", chrono::Utc::now().timestamp());
        let n = ch
            .open_issue(&title, "Opened by the GitHub channel live test.")
            .await
            .expect("open issue");
        eprintln!("opened #{}", n);

        ch.comment(n, "Live test comment.").await.expect("comment");

        // GitHub's issue list is eventually consistent: a just-created issue
        // can be missing from it for a second or two, so poll rather than
        // asserting on the first read.
        let appears = wait_for(|| async {
            ch.list_open_topics()
                .await
                .map(|t| t.iter().any(|(num, ti)| *num == n && ti == &title))
                .unwrap_or(false)
        })
        .await;
        assert!(appears, "new issue should appear in the open topic list");

        ch.close_issue(n).await.expect("close");
        let gone = wait_for(|| async {
            ch.list_open_topics()
                .await
                .map(|t| !t.iter().any(|(num, _)| *num == n))
                .unwrap_or(false)
        })
        .await;
        assert!(gone, "closed issue should leave the open topic list");
        eprintln!("LIVE GITHUB ROUNDTRIP OK: #{} opened, commented, closed", n);
    }

    /// Live end-to-end test against the real Jade relay. Ignored by default;
    /// run with the relay env vars set:
    ///   JADE_RELAY_API_BASE, JADE_RELAY_TOKEN, JADE_RELAY_TOKEN_ID,
    ///   JADE_RELAY_USER_ID, JADE_RELAY_SESSION_ID
    ///   cargo test -p jcode-app-core relay_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires live Jade relay credentials"]
    async fn test_relay_live_roundtrip() {
        let api_base = match std::env::var("JADE_RELAY_API_BASE") {
            Ok(v) => v,
            Err(_) => {
                eprintln!("skipping: JADE_RELAY_API_BASE not set");
                return;
            }
        };
        let token = std::env::var("JADE_RELAY_TOKEN").expect("JADE_RELAY_TOKEN");
        let token_id = std::env::var("JADE_RELAY_TOKEN_ID").ok();
        let user_id = std::env::var("JADE_RELAY_USER_ID").unwrap_or_else(|_| "jeremy".to_string());
        let session_id = std::env::var("JADE_RELAY_SESSION_ID")
            .unwrap_or_else(|_| format!("rust-live-{}", chrono::Utc::now().timestamp()));

        let ch = JadeRelayChannel::new(
            api_base,
            token,
            token_id.clone(),
            user_id.clone(),
            session_id.clone(),
            true,
        );

        // 1) heartbeat (device register)
        ch.heartbeat("jcode-test-device").await;

        // 2) baseline cursor: no prompts yet
        let (events, after) = ch.poll_prompts(0, 0).await.expect("baseline poll");
        eprintln!("baseline: {} events, next_after={}", events.len(), after);

        // 3) simulate a cloud client posting a prompt by POSTing a prompt event
        let prompt_text = format!(
            "hello from rust live test {}",
            chrono::Utc::now().timestamp()
        );
        let prompt_body = serde_json::json!({
            "user_id": user_id,
            "type": "prompt",
            "text": prompt_text,
            "origin": "rust-test-client",
        });
        let resp = ch
            .auth(
                ch.client
                    .post(ch.url(&format!(
                        "v1/sessions/{}/events",
                        urlencoding_encode(&session_id)
                    )))
                    .json(&prompt_body),
            )
            .send()
            .await
            .expect("post prompt");
        assert!(
            resp.status().is_success(),
            "post prompt status {}",
            resp.status()
        );

        // 4) the channel polls and sees the prompt
        let (events, after2) = ch.poll_prompts(after, 5).await.expect("poll after prompt");
        assert!(!events.is_empty(), "expected at least one prompt event");
        let prompt_ev = events
            .iter()
            .find(|e| e.text.as_deref() == Some(prompt_text.as_str()))
            .expect("our prompt event present");
        eprintln!("received prompt seq={} after2={}", prompt_ev.seq, after2);

        // 5) the channel posts a response tied to that prompt's seq
        let reply = format!("rust live reply to seq {}", prompt_ev.seq);
        ch.post_response(&reply, prompt_ev.seq)
            .await
            .expect("post response");

        // 6) verify the response is visible (poll all event types via raw GET)
        let verify_url = ch.url(&format!(
            "v1/sessions/{}/events?user_id={}&after=0&types=response&wait=5",
            urlencoding_encode(&session_id),
            urlencoding_encode(&user_id)
        ));
        let verify: RelayEventsResponse = ch
            .auth(ch.client.get(&verify_url))
            .send()
            .await
            .expect("verify get")
            .json()
            .await
            .expect("verify json");
        assert!(
            verify
                .events
                .iter()
                .any(|e| e.text.as_deref() == Some(reply.as_str())),
            "response event should be readable back from the relay"
        );
        eprintln!("LIVE ROUNDTRIP OK: prompt -> poll -> response verified");
    }
}
