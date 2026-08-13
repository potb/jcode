use super::provider_fetch::fetch_openai_usage_report;
use super::*;

static USAGE: tokio::sync::OnceCell<Arc<RwLock<UsageData>>> = tokio::sync::OnceCell::const_new();
static REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Set once this process has adopted a server-pushed snapshot.
///
/// This is the "client staleness must be neutralized" decision from issue #24.
/// Without it the push is pointless: a pushed snapshot ages past
/// `CACHE_DURATION` like any other, `get_sync` would notice from the render
/// loop and spawn a fetch, and every connected client would be back to polling
/// the endpoint on its own timer.
///
/// A latch rather than a config flag, because it is keyed on evidence rather
/// than intent: the only thing that can set it is an actual pushed snapshot
/// arriving, which is proof that a server is attached and polling on this
/// process's behalf. A process that never receives one (menubar, one-shot CLI,
/// a client before `Subscribe`) is unaffected and keeps its own refresh path.
///
/// Never cleared. If the server goes away the client keeps serving the last
/// pushed snapshot as stale, which is the same thing it already does for a
/// failed refresh, and is strictly better than having every orphaned client
/// resume polling at the same instant.
static PUSH_FED: AtomicBool = AtomicBool::new(false);

/// Whether this process is being fed usage snapshots by the server.
pub fn is_push_fed() -> bool {
    PUSH_FED.load(Ordering::SeqCst)
}

/// Adopt a usage snapshot pushed by the server, and stop self-refreshing.
///
/// Returns whether the snapshot was adopted. A snapshot describing a different
/// account than the one active here is ignored, so account A can never be shown
/// B's quota; that mirrors the on-disk snapshot's rule.
pub fn adopt_pushed_snapshot(snapshot: &jcode_protocol::UsageSnapshot) -> bool {
    let active =
        auth::claude::active_account_label().unwrap_or_else(auth::claude::primary_account_label);
    if !super::push::snapshot_matches_account(snapshot.account_label.as_deref(), &active) {
        return false;
    }

    let data = super::push::usage_from_snapshot(snapshot);

    // Mark push-fed before storing: a render on another thread that observes the
    // new data must not be able to see it as stale-and-unowned in between.
    PUSH_FED.store(true, Ordering::SeqCst);

    match USAGE.get() {
        Some(cell) => match cell.try_write() {
            Ok(mut guard) => *guard = data,
            // A refresh holds the lock right now. Dropping this push is safe:
            // the poller republishes every round, so the next one lands.
            Err(_) => return false,
        },
        None => {
            let _ = USAGE.set(Arc::new(RwLock::new(data)));
        }
    }

    true
}

pub(super) async fn get_usage() -> Arc<RwLock<UsageData>> {
    USAGE
        .get_or_init(|| async { Arc::new(RwLock::new(UsageData::default())) })
        .await
        .clone()
}

/// Fetch usage data from the API
async fn fetch_usage() -> Result<UsageData> {
    let creds = auth::claude::load_credentials().context("Failed to load Claude credentials")?;

    let now = chrono::Utc::now().timestamp_millis();
    let active_label =
        auth::claude::active_account_label().unwrap_or_else(auth::claude::primary_account_label);
    let access_token = if creds.expires_at < now + 300_000 && !creds.refresh_token.is_empty() {
        match auth::oauth::refresh_claude_tokens_for_account(&creds.refresh_token, &active_label)
            .await
        {
            Ok(refreshed) => refreshed.access_token,
            Err(_) => creds.access_token,
        }
    } else {
        creds.access_token
    };

    let cache_key = anthropic_usage_cache_key(&access_token, Some(&active_label));
    fetch_anthropic_usage_data(access_token, cache_key).await
}

/// The cached `UsageData` for the account the next fetch would use, including
/// any `retry_after` hint and the last-known windows recorded by a failed
/// refresh. `None` when credentials cannot be resolved or nothing is cached.
fn cached_anthropic_usage_entry_for_active_account() -> Option<UsageData> {
    let creds = auth::claude::load_credentials().ok()?;
    let active_label =
        auth::claude::active_account_label().unwrap_or_else(auth::claude::primary_account_label);
    let cache_key = anthropic_usage_cache_key(&creds.access_token, Some(&active_label));
    super::cache::peek_anthropic_usage(&cache_key)
}

async fn refresh_usage(usage: Arc<RwLock<UsageData>>) {
    let active_label =
        auth::claude::active_account_label().unwrap_or_else(auth::claude::primary_account_label);

    // Another jcode process on this machine may have fetched moments ago.
    // Adopting its result is the whole point of the shared snapshot: without
    // it, every process runs its own five-minute poller against a single burst
    // limiter, and they all re-fetch at the same instant when a window resets.
    if let Some(shared) = super::snapshot::fresh_shared_snapshot(Some(&active_label)) {
        *usage.write().await = shared;
        return;
    }

    // No fresh snapshot to adopt, so a request is warranted - but not from
    // every process at once. `is_stale()` fires unconditionally the moment a
    // window's reset timestamp passes, which makes the shared snapshot stale
    // for every process on the machine at the *same instant*: without this
    // lease they all miss the adopt path above and burst together, which is
    // the 429 storm. Losing the race is a skip, not a wait: this process keeps
    // serving what it has and adopts the winner's snapshot on its next tick.
    let Some(lease) = super::lease::try_acquire_anthropic() else {
        return;
    };

    let result = fetch_usage().await;
    // Release before storing, so the next process to decide it needs a fetch
    // is not blocked behind bookkeeping this one still has to do.
    super::lease::release(&lease);

    match result {
        Ok(new_data) => {
            // Publish before storing so the next process to wake sees it even
            // if this one exits immediately afterwards.
            super::snapshot::publish(&new_data, Some(&active_label));
            *usage.write().await = new_data;
        }
        Err(e) => {
            let err_msg = e.to_string();
            // `fetch_anthropic_usage_data` already recorded the failure - along
            // with any server `Retry-After` hint - in the per-token cache. Adopt
            // that entry instead of hand-rolling the error state here, otherwise
            // the hint is dropped and this snapshot falls back to the blanket 15
            // minute rate-limit backoff even when the endpoint says to retry in
            // seconds.
            let cached = cached_anthropic_usage_entry_for_active_account();
            let mut data = usage.write().await;
            let is_new_error = data.last_error.as_deref() != Some(&err_msg);
            if let Some(cached) = cached {
                *data = cached;
            }
            data.last_error = Some(err_msg.clone());
            data.fetched_at = Some(Instant::now());
            if is_new_error {
                crate::logging::error(&format!("Usage fetch error: {}", err_msg));
            }
        }
    }
}

fn try_spawn_refresh(usage: Arc<RwLock<UsageData>>) {
    // A push-fed process must never fetch: the server polls on its behalf and
    // pushes the result, and the whole point of issue #24 is that N clients
    // refreshing on their own timers is what trips the burst limiter. Checked
    // here, at the single choke point every refresh path funnels through, rather
    // than at each caller, so a future call site cannot forget it.
    if !super::push::should_self_refresh(is_push_fed()) {
        return;
    }

    if REFRESH_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    tokio::spawn(async move {
        refresh_usage(usage).await;
        REFRESH_IN_FLIGHT.store(false, Ordering::SeqCst);
    });
}

/// Get current usage data, refreshing if stale
pub async fn get() -> UsageData {
    let usage = get_usage().await;

    // Check if we need to refresh
    let (should_refresh, current_data) = {
        let data = usage.read().await;
        (data.is_stale(), data.clone())
    };

    if should_refresh {
        try_spawn_refresh(usage.clone());
    }

    current_data.display_snapshot()
}

static OPENAI_USAGE: tokio::sync::OnceCell<Arc<RwLock<OpenAIUsageData>>> =
    tokio::sync::OnceCell::const_new();
static OPENAI_REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

pub(super) async fn get_openai_usage_cell() -> Arc<RwLock<OpenAIUsageData>> {
    OPENAI_USAGE
        .get_or_init(|| async { Arc::new(RwLock::new(OpenAIUsageData::default())) })
        .await
        .clone()
}

async fn fetch_openai_usage_data() -> OpenAIUsageData {
    match fetch_openai_usage_report().await {
        Some(report) => openai_usage_data_from_provider_report(&report),
        None => OpenAIUsageData {
            fetched_at: Some(Instant::now()),
            last_error: Some("No OpenAI/Codex OAuth credentials found".to_string()),
            ..Default::default()
        },
    }
}

async fn refresh_openai_usage(usage: Arc<RwLock<OpenAIUsageData>>) {
    let active_label = auth::codex::active_account_label();

    // Another jcode process on this machine may have fetched moments ago; see
    // the Anthropic path above. Without this, a server plus several clients
    // each poll the Codex usage endpoint on their own timer, and the
    // reset-timestamp staleness rule makes them all refetch at once.
    if let Some(shared) = super::snapshot::fresh_shared_openai_snapshot(active_label.as_deref()) {
        *usage.write().await = shared;
        return;
    }

    // One fetcher per machine per round; see the Anthropic path above. The
    // Codex endpoint has its own lease file because the two providers have
    // independent accounts and cache durations, so an Anthropic fetch must
    // never keep a Codex refresh from happening.
    let Some(lease) = super::lease::try_acquire_openai() else {
        return;
    };

    let new_data = fetch_openai_usage_data().await;
    super::lease::release(&lease);
    // Publish before storing, so the next process to wake sees it even if this
    // one exits immediately afterwards. `publish_openai` itself refuses error
    // and empty snapshots, so a failed fetch never silences other processes.
    super::snapshot::publish_openai(&new_data, active_label.as_deref());
    *usage.write().await = new_data;
}

fn try_spawn_openai_refresh(usage: Arc<RwLock<OpenAIUsageData>>) {
    if OPENAI_REFRESH_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    tokio::spawn(async move {
        refresh_openai_usage(usage).await;
        OPENAI_REFRESH_IN_FLIGHT.store(false, Ordering::SeqCst);
    });
}

pub async fn get_openai_usage() -> OpenAIUsageData {
    let usage = get_openai_usage_cell().await;

    let (should_refresh, current_data) = {
        let data = usage.read().await;
        (data.is_stale(), data.clone())
    };

    if should_refresh {
        try_spawn_openai_refresh(usage.clone());
    }

    current_data.display_snapshot()
}

pub fn get_openai_usage_sync() -> OpenAIUsageData {
    if let Some(usage) = OPENAI_USAGE.get()
        && let Ok(data) = usage.try_read()
    {
        if data.is_stale() {
            try_spawn_openai_refresh(usage.clone());
        }
        return data.display_snapshot();
    }

    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(async {
            let _ = get_openai_usage().await;
        });
    }

    OpenAIUsageData::default()
}

/// Check if extra usage (1M context, etc.) is enabled for the account.
/// Returns false if unknown/not yet fetched.
pub fn has_extra_usage() -> bool {
    if let Some(usage) = USAGE.get()
        && let Ok(data) = usage.try_read()
    {
        return data.extra_usage_enabled;
    }
    false
}

/// Fetch usage data for a specific Anthropic account token (blocking).
/// Used for account rotation - checks if a particular account is exhausted.
/// Returns an error if the fetch fails (network, auth, etc.).
/// Results are cached per-account to avoid hammering the API.
pub fn fetch_usage_for_account_sync(
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
) -> Result<UsageData> {
    let cache_key = anthropic_usage_cache_key(access_token, None);

    if let Some(cached) = cached_anthropic_usage(&cache_key) {
        return Ok(cached);
    }

    if tokio::runtime::Handle::try_current().is_err() {
        anyhow::bail!("Anthropic usage refresh requires a Tokio runtime")
    }

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(fetch_usage_for_account(
            access_token.to_string(),
            refresh_token.to_string(),
            expires_at,
        ))
    });

    if let Ok(ref data) = result {
        store_anthropic_usage(cache_key, data.clone());
    }

    result
}

pub fn fetch_openai_usage_for_account_sync(
    label: &str,
    email: Option<String>,
    creds: auth::codex::CodexCredentials,
) -> Result<AccountUsageSnapshot> {
    let cache_key = openai_usage_cache_key(&creds.access_token, Some(label));
    if let Some(cached) = cached_openai_usage(&cache_key) {
        return Ok(openai_snapshot_from_usage(
            label.to_string(),
            email,
            &cached,
        ));
    }

    if tokio::runtime::Handle::try_current().is_err() {
        anyhow::bail!("OpenAI usage refresh requires a Tokio runtime")
    }

    let report = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(fetch_openai_usage_for_account(
            openai_provider_display_name(label, email.as_deref(), 2, false),
            creds,
            Some(label),
        ))
    });
    let data = openai_usage_data_from_provider_report(&report);
    store_openai_usage(cache_key, data.clone());
    Ok(openai_snapshot_from_usage(label.to_string(), email, &data))
}

pub fn account_usage_probe_sync(provider: MultiAccountProviderKind) -> Option<AccountUsageProbe> {
    match provider {
        MultiAccountProviderKind::Anthropic => anthropic_account_usage_probe_sync(),
        MultiAccountProviderKind::OpenAI => openai_account_usage_probe_sync(),
    }
}

fn anthropic_account_usage_probe_sync() -> Option<AccountUsageProbe> {
    let accounts = auth::claude::list_accounts().ok()?;
    if accounts.is_empty() {
        return None;
    }

    let current_label = auth::claude::active_account_label()
        .or_else(|| accounts.first().map(|account| account.label.clone()))?;
    let active_cached = get_sync();

    let mut snapshots = Vec::with_capacity(accounts.len());
    for account in &accounts {
        let usage = if account.label == current_label && active_cached.fetched_at.is_some() {
            Ok(active_cached.clone())
        } else {
            fetch_usage_for_account_sync(&account.access, &account.refresh, account.expires)
        };

        match usage {
            Ok(usage) => snapshots.push(anthropic_snapshot_from_usage(
                account.label.clone(),
                account.email.clone(),
                &usage,
            )),
            Err(err) => snapshots.push(AccountUsageSnapshot {
                label: account.label.clone(),
                email: account.email.clone(),
                exhausted: false,
                primary_label: None,
                five_hour_ratio: None,
                secondary_label: None,
                seven_day_ratio: None,
                resets_at: None,
                error: Some(err.to_string()),
            }),
        }
    }

    Some(AccountUsageProbe {
        provider: MultiAccountProviderKind::Anthropic,
        current_label,
        accounts: snapshots,
    })
}

fn openai_account_usage_probe_sync() -> Option<AccountUsageProbe> {
    let accounts = auth::codex::list_accounts().ok()?;
    if accounts.is_empty() {
        return None;
    }

    let current_label = auth::codex::active_account_label()
        .or_else(|| accounts.first().map(|account| account.label.clone()))?;
    let active_cached = get_openai_usage_sync();

    let mut snapshots = Vec::with_capacity(accounts.len());
    for account in &accounts {
        let usage = if account.label == current_label && active_cached.fetched_at.is_some() {
            Ok(openai_snapshot_from_usage(
                account.label.clone(),
                account.email.clone(),
                &active_cached,
            ))
        } else {
            fetch_openai_usage_for_account_sync(
                &account.label,
                account.email.clone(),
                auth::codex::CodexCredentials {
                    access_token: account.access_token.clone(),
                    refresh_token: account.refresh_token.clone(),
                    id_token: account.id_token.clone(),
                    account_id: account.account_id.clone(),
                    expires_at: account.expires_at,
                },
            )
        };

        match usage {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(err) => snapshots.push(AccountUsageSnapshot {
                label: account.label.clone(),
                email: account.email.clone(),
                exhausted: false,
                primary_label: None,
                five_hour_ratio: None,
                secondary_label: None,
                seven_day_ratio: None,
                resets_at: None,
                error: Some(err.to_string()),
            }),
        }
    }

    Some(AccountUsageProbe {
        provider: MultiAccountProviderKind::OpenAI,
        current_label,
        accounts: snapshots,
    })
}

async fn fetch_usage_for_account(
    access_token: String,
    _refresh_token: String,
    expires_at: i64,
) -> Result<UsageData> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    if expires_at < now_ms {
        anyhow::bail!("OAuth token expired");
    }

    let cache_key = anthropic_usage_cache_key(&access_token, None);
    fetch_anthropic_usage_data(access_token, cache_key).await
}

/// Fetch the current Anthropic OAuth usage for an already-resolved access
/// token. This is used on the request path when model-scoped quota affects
/// routing. Unlike [`get`], it waits for the first fetch instead of returning
/// an empty snapshot while a background refresh starts.
pub async fn fetch_usage_for_access_token(access_token: &str) -> Result<UsageData> {
    let cache_key = anthropic_usage_cache_key(access_token, None);
    fetch_anthropic_usage_data(access_token.to_string(), cache_key).await
}

/// Seed the in-process usage snapshot so tests can drive UI surfaces that read
/// [`get_sync`] without performing a network fetch. Test-only: the live path
/// always populates this through `refresh_usage`.
#[cfg(feature = "test-support")]
pub fn seed_for_test(data: UsageData) {
    // OnceCell may already be initialized by an earlier call; overwrite the
    // inner value in that case so repeated seeding works.
    if let Some(existing) = USAGE.get() {
        if let Ok(mut guard) = existing.try_write() {
            *guard = data;
        }
        return;
    }
    let _ = USAGE.set(Arc::new(RwLock::new(data)));
}

/// Get usage data synchronously (returns cached data, triggers refresh if stale)
pub fn get_sync() -> UsageData {
    // Try to get cached data
    if let Some(usage) = USAGE.get() {
        // Return current cached value (blocking read)
        if let Ok(data) = usage.try_read() {
            if data.is_stale() {
                try_spawn_refresh(usage.clone());
            }
            return data.display_snapshot();
        }
    }

    // Not initialized yet - trigger initialization
    // `get()` reaches the network through its own path rather than
    // `try_spawn_refresh`, so the push-fed guard has to be repeated here. A
    // push-fed process with a not-yet-initialized cell simply waits for the next
    // pushed snapshot, which is at most one server poll interval away.
    if super::push::should_self_refresh(is_push_fed())
        && tokio::runtime::Handle::try_current().is_ok()
    {
        tokio::spawn(async {
            let _ = get().await;
        });
    }

    UsageData::default()
}
