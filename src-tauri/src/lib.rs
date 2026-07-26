mod codex;
mod gateway;
mod models;
mod sessions;
mod store;

use std::{
    collections::VecDeque,
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use chrono::Utc;
use models::{
    AppConfig, AppSettings, AppSnapshot, CodexApplyAndRestartResult, CodexApplyResult, CodexRestartResult,
    CodexSession, GatewayStatus, Provider, ProviderInput, ProviderStatus, RequestLog, SessionOverview,
    SessionRepairResult,
};
use tauri::{Manager, State};
use tokio::{
    sync::{oneshot, Mutex, Notify, RwLock},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    gateway::{GatewayContext, GatewayMetrics},
    store::Store,
};

/// Health probes run a few at a time so a long provider list still finishes quickly.
const HEALTH_CONCURRENCY: usize = 4;
/// Re-probe interval while any provider is cooling down, so recovery is not stuck behind a long cycle.
const RECOVERY_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("file operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid config: {0}")]
    Json(#[from] serde_json::Error),
    #[error("credential store failed: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("HTTP client failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Codex 数据库操作失败: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("{0}")]
    InvalidInput(String),
}

struct GatewayRuntime {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

#[derive(Clone)]
struct AppState {
    config: Arc<RwLock<AppConfig>>,
    store: Store,
    /// Short, bounded client used for `/v1/models` probes and key verification.
    probe_client: Arc<RwLock<reqwest::Client>>,
    /// Long-lived client used for proxied traffic; no total timeout so streams survive.
    proxy_client: Arc<RwLock<reqwest::Client>>,
    gateway: Arc<Mutex<Option<GatewayRuntime>>>,
    metrics: Arc<GatewayMetrics>,
    logs: Arc<RwLock<VecDeque<RequestLog>>>,
    cursor: Arc<AtomicUsize>,
    health_notify: Arc<Notify>,
}

fn command_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// Shared connection behaviour: pooled, keep-alive sockets so a switch between upstreams
/// does not pay a fresh TLS handshake every time.
fn base_client(settings: &AppSettings) -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(settings.connect_timeout_seconds.clamp(1, 120)))
        .tcp_nodelay(true)
        .user_agent(concat!("RelayDeck/", env!("CARGO_PKG_VERSION")));
    if settings.upstream_keep_alive {
        builder
            .tcp_keepalive(Duration::from_secs(30))
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(Duration::from_secs(300))
    } else {
        builder.pool_max_idle_per_host(0)
    }
}

fn build_probe_client(settings: &AppSettings) -> Result<reqwest::Client, reqwest::Error> {
    base_client(settings)
        .timeout(Duration::from_secs(settings.request_timeout_seconds.clamp(5, 600)))
        .build()
}

fn build_proxy_client(settings: &AppSettings) -> Result<reqwest::Client, reqwest::Error> {
    // Deliberately no total timeout: a long Codex turn must not be cut off mid-stream.
    // Dead upstreams are caught by the connect and per-read budgets instead.
    base_client(settings)
        .read_timeout(Duration::from_secs(settings.stream_idle_timeout_seconds.clamp(10, 3600)))
        .build()
}

#[tauri::command]
async fn get_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    let running = state.gateway.lock().await.is_some();
    let config = state.config.read().await.clone();
    let logs = state.logs.read().await.iter().cloned().collect();
    let gateway = status_from(&config, &state.metrics, running);
    Ok(AppSnapshot {
        providers: config.providers,
        gateway,
        logs,
        codex: codex::status(config.settings.gateway_port),
        settings: config.settings,
    })
}

#[tauri::command]
async fn save_provider(input: ProviderInput, state: State<'_, AppState>) -> Result<Provider, String> {
    validate_provider(&input)?;
    let mut config = state.config.write().await;
    let id = input.id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
    let existing = config.providers.iter().find(|provider| provider.id == id).cloned();

    if let Some(secret) = input.api_key.as_deref().filter(|value| !value.trim().is_empty()) {
        store::save_secret(&id, secret).map_err(command_error)?;
    }
    let api_key = input.api_key.filter(|value| !value.trim().is_empty()).or_else(|| existing.as_ref().and_then(|provider| provider.api_key.clone()));
    if api_key.is_none() {
        return Err("API Key 不能为空".into());
    }
    let provider = Provider {
        id: id.clone(), name: input.name.trim().into(), base_url: input.base_url.trim_end_matches('/').into(), api_key,
        has_key: true, model: input.model.as_deref().filter(|value| !value.trim().is_empty()).map(str::to_owned).or_else(|| existing.as_ref().map(|value| value.model.clone())).unwrap_or_else(|| "自动获取中".into()),
        model_override: input.model_override.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty()),
        available_models: existing.as_ref().map(|value| value.available_models.clone()).unwrap_or_default(), enabled: input.enabled, priority: input.priority,
        status: existing.as_ref().map(|value| value.status).unwrap_or(ProviderStatus::Unknown),
        latency_ms: existing.as_ref().and_then(|value| value.latency_ms),
        last_checked_at: existing.as_ref().and_then(|value| value.last_checked_at),
        last_error: existing.as_ref().and_then(|value| value.last_error.clone()),
        consecutive_failures: 0,
        cooldown_until: None,
        served_count: existing.as_ref().map(|value| value.served_count).unwrap_or_default(),
        failed_count: existing.as_ref().map(|value| value.failed_count).unwrap_or_default(),
        health_history: existing.as_ref().map(|value| value.health_history.clone()).unwrap_or_default(),
    };
    if let Some(position) = config.providers.iter().position(|item| item.id == id) {
        config.providers[position] = provider.clone();
    } else {
        config.providers.push(provider.clone());
    }
    drop(config);
    state.store.save(&state.config).await.map_err(command_error)?;
    test_provider_inner(state.inner(), &id, true).await.or(Ok(provider))
}

#[tauri::command]
async fn delete_provider(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.config.write().await.providers.retain(|provider| provider.id != id);
    store::delete_secret(&id);
    state.store.save(&state.config).await.map_err(command_error)
}

#[tauri::command]
async fn toggle_provider(id: String, enabled: bool, state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        let provider = config.providers.iter_mut().find(|provider| provider.id == id).ok_or_else(|| "Provider 不存在".to_string())?;
        provider.enabled = enabled;
        if enabled {
            provider.clear_cooldown();
        }
    }
    state.store.save(&state.config).await.map_err(command_error)
}

/// Pins (or releases) the model RelayDeck sends to one upstream.
#[tauri::command]
async fn set_provider_model(id: String, model: Option<String>, state: State<'_, AppState>) -> Result<Provider, String> {
    let provider = {
        let mut config = state.config.write().await;
        let provider = config.providers.iter_mut().find(|provider| provider.id == id).ok_or_else(|| "Provider 不存在".to_string())?;
        provider.model_override = model.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
        provider.clone()
    };
    state.store.save(&state.config).await.map_err(command_error)?;
    Ok(provider)
}

/// Moves a provider up or down the routing order and renumbers priorities to stay gap-free.
#[tauri::command]
async fn move_provider(id: String, offset: i32, state: State<'_, AppState>) -> Result<Vec<Provider>, String> {
    let providers = {
        let mut config = state.config.write().await;
        config.providers.sort_by(|a, b| a.priority.cmp(&b.priority));
        let current = config.providers.iter().position(|provider| provider.id == id).ok_or_else(|| "Provider 不存在".to_string())?;
        let target = (current as i32 + offset).clamp(0, config.providers.len() as i32 - 1) as usize;
        if target != current {
            let provider = config.providers.remove(current);
            config.providers.insert(target, provider);
        }
        for (index, provider) in config.providers.iter_mut().enumerate() {
            provider.priority = index as u32 + 1;
        }
        config.providers.clone()
    };
    state.store.save(&state.config).await.map_err(command_error)?;
    Ok(providers)
}

/// Puts a cooling-down provider straight back into rotation and re-probes it.
#[tauri::command]
async fn clear_provider_cooldown(id: String, state: State<'_, AppState>) -> Result<Provider, String> {
    {
        let mut config = state.config.write().await;
        let provider = config.providers.iter_mut().find(|provider| provider.id == id).ok_or_else(|| "Provider 不存在".to_string())?;
        provider.clear_cooldown();
        provider.status = ProviderStatus::Unknown;
        provider.last_error = None;
    }
    test_provider_inner(state.inner(), &id, false).await
}

#[tauri::command]
async fn test_provider(id: String, state: State<'_, AppState>) -> Result<Provider, String> {
    test_provider_inner(&state, &id, true).await
}

/// Token-free keep-alive sweep across every enabled provider, on demand.
#[tauri::command]
async fn refresh_health(state: State<'_, AppState>) -> Result<Vec<Provider>, String> {
    run_health_sweep(state.inner()).await;
    Ok(state.config.read().await.providers.clone())
}

async fn test_provider_inner(state: &AppState, id: &str, deep_probe: bool) -> Result<Provider, String> {
    let provider = check_provider(state, id, deep_probe).await?;
    state.store.save(&state.config).await.map_err(command_error)?;
    Ok(provider)
}

/// Probes one provider and folds the result into the in-memory config. Callers persist.
async fn check_provider(state: &AppState, id: &str, deep_probe: bool) -> Result<Provider, String> {
    let provider = {
        let mut config = state.config.write().await;
        let provider = config.providers.iter_mut().find(|provider| provider.id == id).ok_or_else(|| "Provider 不存在".to_string())?;
        provider.status = ProviderStatus::Checking;
        provider.clone()
    };
    let client = state.probe_client.read().await.clone();
    let started = Instant::now();
    let url = gateway::join_url(&provider.base_url, "/v1/models");
    let result = client.get(url).bearer_auth(provider.api_key.as_deref().unwrap_or_default()).send().await;
    let mut updated = provider;
    let checked_at = Utc::now();
    updated.last_checked_at = Some(checked_at);
    match result {
        Ok(response) if response.status().is_success() => match response.json::<serde_json::Value>().await {
            Ok(payload) => {
                let models = extract_model_ids(&payload);
                if models.is_empty() {
                    updated.status = ProviderStatus::Unhealthy;
                    updated.latency_ms = Some(started.elapsed().as_millis() as u64);
                    updated.last_error = Some("/models 没有返回可用模型".into());
                } else {
                    let model = select_codex_model(&models);
                    let probe = if deep_probe { probe_provider(&client, &updated, &model).await } else { Ok(()) };
                    updated.model = model;
                    updated.available_models = models;
                    updated.latency_ms = Some(started.elapsed().as_millis() as u64);
                    match probe {
                        Ok(()) => {
                            updated.status = ProviderStatus::Healthy;
                            updated.last_error = None;
                        }
                        Err(error) => {
                            updated.status = ProviderStatus::Unhealthy;
                            updated.last_error = Some(error);
                        }
                    }
                }
            }
            Err(error) => {
                updated.status = ProviderStatus::Unhealthy;
                updated.latency_ms = Some(started.elapsed().as_millis() as u64);
                updated.last_error = Some(format!("模型列表不是有效 JSON: {error}"));
            }
        },
        Ok(response) => {
            updated.status = ProviderStatus::Unhealthy;
            updated.latency_ms = Some(started.elapsed().as_millis() as u64);
            updated.last_error = Some(format!("HTTP {}", response.status()));
        }
        Err(error) => {
            updated.status = ProviderStatus::Unhealthy;
            updated.latency_ms = None;
            updated.last_error = Some(error.to_string());
        }
    }

    let healthy = updated.status == ProviderStatus::Healthy;
    updated.record_health(healthy, updated.latency_ms, checked_at);
    if healthy {
        updated.clear_cooldown();
    } else {
        updated.consecutive_failures = updated.consecutive_failures.saturating_add(1);
        let cooldown_seconds = state.config.read().await.settings.cooldown_seconds;
        updated.apply_cooldown(cooldown_seconds, checked_at);
    }

    let mut config = state.config.write().await;
    if let Some(position) = config.providers.iter().position(|provider| provider.id == id) {
        config.providers[position] = updated.clone();
    }
    Ok(updated)
}

async fn probe_provider(client: &reqwest::Client, provider: &Provider, model: &str) -> Result<(), String> {
    let url = gateway::join_url(&provider.base_url, "/v1/responses");
    let response = client.post(url)
        .bearer_auth(provider.api_key.as_deref().unwrap_or_default())
        .json(&serde_json::json!({
            "model": model,
            "input": "Reply with OK.",
            "max_output_tokens": 16,
            "stream": false,
            "store": false
        }))
        .send().await.map_err(|error| format!("模型请求失败: {error}"))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| format!("无法读取模型响应: {error}"))?;
    if !status.is_success() {
        let summary = body.chars().take(180).collect::<String>();
        return Err(format!("模型 {model} /responses 返回 HTTP {status}: {summary}"));
    }
    let payload = serde_json::from_str::<serde_json::Value>(&body).map_err(|error| format!("模型 {model} 返回的不是有效 JSON: {error}"))?;
    if !is_valid_response_payload(&payload) {
        return Err(format!("模型 {model} 未返回有效 Responses 结果"));
    }
    Ok(())
}

fn is_valid_response_payload(payload: &serde_json::Value) -> bool {
    payload.get("id").and_then(serde_json::Value::as_str).is_some()
        || payload.get("object").and_then(serde_json::Value::as_str) == Some("response")
        || payload.get("output").and_then(serde_json::Value::as_array).is_some()
}

#[tauri::command]
async fn import_providers(providers: Vec<ProviderInput>, state: State<'_, AppState>) -> Result<Vec<Provider>, String> {
    if providers.is_empty() {
        return Err("没有可导入的 Provider".into());
    }
    let mut imported_ids = Vec::with_capacity(providers.len());
    let mut config = state.config.write().await;
    for input in providers {
        validate_provider(&input)?;
        let secret = input.api_key.as_deref().filter(|value| !value.trim().is_empty()).ok_or_else(|| format!("{} 缺少 API Key", input.name))?;
        let id = Uuid::new_v4().to_string();
        store::save_secret(&id, secret).map_err(command_error)?;
        let provider = Provider {
            id, name: input.name.trim().into(), base_url: input.base_url.trim_end_matches('/').into(), api_key: Some(secret.into()), has_key: true,
            model: "自动获取中".into(), model_override: None, available_models: Vec::new(), enabled: input.enabled, priority: input.priority,
            status: ProviderStatus::Unknown, latency_ms: None, last_checked_at: None, last_error: None,
            consecutive_failures: 0, cooldown_until: None, served_count: 0, failed_count: 0, health_history: Vec::new(),
        };
        imported_ids.push(provider.id.clone());
        config.providers.push(provider);
    }
    drop(config);
    state.store.save(&state.config).await.map_err(command_error)?;
    let mut imported = Vec::with_capacity(imported_ids.len());
    for chunk in imported_ids.chunks(HEALTH_CONCURRENCY) {
        let checks = chunk.iter().map(|id| check_provider(state.inner(), id, true));
        for result in futures_util::future::join_all(checks).await {
            imported.push(result?);
        }
    }
    state.store.save(&state.config).await.map_err(command_error)?;
    Ok(imported)
}

#[tauri::command]
async fn save_settings(settings: AppSettings, state: State<'_, AppState>) -> Result<AppSettings, String> {
    let settings = validate_settings(settings)?;
    // Read the two locks in sequence; the gateway mutex is always taken before the config lock elsewhere.
    let previous_port = state.config.read().await.settings.gateway_port;
    let running = state.gateway.lock().await.is_some();
    let applied = {
        let mut config = state.config.write().await;
        config.settings = settings;
        config.settings.clone()
    };
    state.store.save(&state.config).await.map_err(command_error)?;
    *state.probe_client.write().await = build_probe_client(&applied).map_err(command_error)?;
    *state.proxy_client.write().await = build_proxy_client(&applied).map_err(command_error)?;
    // A port change would otherwise leave the gateway listening on the old socket.
    if running && previous_port != applied.gateway_port {
        stop_gateway_inner(state.inner()).await;
        start_gateway_inner(state.inner()).await?;
    }
    state.health_notify.notify_one();
    Ok(applied)
}

#[tauri::command]
async fn start_gateway(state: State<'_, AppState>) -> Result<GatewayStatus, String> {
    start_gateway_inner(state.inner()).await
}

async fn start_gateway_inner(state: &AppState) -> Result<GatewayStatus, String> {
    {
        let mut runtime = state.gateway.lock().await;
        if runtime.is_none() {
            let port = state.config.read().await.settings.gateway_port;
            let listener = gateway::bind(port).await.map_err(|error| format!("端口 {port} 无法监听: {error}"))?;
            let (shutdown, receiver) = oneshot::channel();
            let context = GatewayContext {
                config: state.config.clone(),
                client: state.proxy_client.clone(),
                metrics: state.metrics.clone(),
                logs: state.logs.clone(),
                cursor: state.cursor.clone(),
            };
            let task = tokio::spawn(gateway::serve(listener, context, receiver));
            *runtime = Some(GatewayRuntime { shutdown, task });
            state.metrics.started_at.store(Utc::now().timestamp().max(0) as u64, Ordering::Relaxed);
        }
    }
    gateway_status(state).await
}

#[tauri::command]
async fn stop_gateway(state: State<'_, AppState>) -> Result<GatewayStatus, String> {
    stop_gateway_inner(state.inner()).await;
    gateway_status(state.inner()).await
}

async fn stop_gateway_inner(state: &AppState) {
    let current = state.gateway.lock().await.take();
    if let Some(runtime) = current {
        let _ = runtime.shutdown.send(());
        let _ = runtime.task.await;
    }
    state.metrics.started_at.store(0, Ordering::Relaxed);
}

#[tauri::command]
async fn clear_logs(state: State<'_, AppState>) -> Result<(), String> {
    state.logs.write().await.clear();
    state.metrics.requests.store(0, Ordering::Relaxed);
    state.metrics.successes.store(0, Ordering::Relaxed);
    state.metrics.failures.store(0, Ordering::Relaxed);
    state.metrics.failovers.store(0, Ordering::Relaxed);
    state.metrics.total_latency_ms.store(0, Ordering::Relaxed);
    state.metrics.input_bytes.store(0, Ordering::Relaxed);
    state.metrics.output_bytes.store(0, Ordering::Relaxed);
    let mut config = state.config.write().await;
    for provider in &mut config.providers {
        provider.served_count = 0;
        provider.failed_count = 0;
    }
    Ok(())
}

#[tauri::command]
async fn reset_access_key(state: State<'_, AppState>) -> Result<AppSettings, String> {
    let settings = {
        let mut config = state.config.write().await;
        config.settings.local_access_key = format!("rd_local_{}", Uuid::new_v4().simple());
        config.settings.clone()
    };
    state.store.save(&state.config).await.map_err(command_error)?;
    Ok(settings)
}

/// Codex Desktop only lists threads belonging to the configured provider, so switching to
/// relaydeck would empty its picker; re-pointing historical threads keeps them visible.
fn align_sessions_to_relaydeck() -> usize {
    codex::codex_home()
        .and_then(|home| sessions::align_thread_providers(&home, "relaydeck"))
        .map(|(aligned, _)| aligned)
        .unwrap_or(0)
}

#[tauri::command]
async fn apply_codex_config(state: State<'_, AppState>) -> Result<CodexApplyResult, String> {
    let config = state.config.read().await;
    let (name, model) = codex_target(&config)?;
    let mut apply = codex::apply(&config.settings, &model, &name).map_err(command_error)?;
    apply.aligned_sessions = align_sessions_to_relaydeck();
    Ok(apply)
}

#[tauri::command]
async fn restart_codex() -> Result<CodexRestartResult, String> {
    codex::restart().map_err(command_error)
}

#[tauri::command]
async fn apply_and_restart_codex(state: State<'_, AppState>) -> Result<CodexApplyAndRestartResult, String> {
    let (settings, name, model) = {
        let config = state.config.read().await;
        let (name, model) = codex_target(&config)?;
        (config.settings.clone(), name, model)
    };
    let target = codex::discover_restart_target().map_err(command_error)?;
    codex::stop_running().map_err(command_error)?;
    let mut apply = codex::apply(&settings, &model, &name).map_err(command_error)?;
    // Codex is down at this point, so the thread database can be updated without a writer race.
    apply.aligned_sessions = align_sessions_to_relaydeck();
    let restart = codex::launch(target).map_err(command_error)?;
    Ok(CodexApplyAndRestartResult { apply, restart })
}

/// Scanning `~/.codex/sessions` touches hundreds of files, so it never runs on the async runtime
/// that also serves the proxy.
async fn with_codex_home<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&std::path::Path) -> Result<T, RelayError> + Send + 'static,
{
    let home = codex::codex_home().map_err(command_error)?;
    tokio::task::spawn_blocking(move || work(&home))
        .await
        .map_err(|error| format!("会话扫描失败: {error}"))?
        .map_err(command_error)
}

#[tauri::command]
async fn list_codex_sessions() -> Result<SessionOverview, String> {
    with_codex_home(|home| sessions::overview(home)).await
}

#[tauri::command]
async fn repair_session_visibility() -> Result<SessionRepairResult, String> {
    with_codex_home(|home| sessions::repair_visibility(home)).await
}

#[tauri::command]
async fn archive_codex_session(id: String) -> Result<CodexSession, String> {
    with_codex_home(move |home| sessions::archive(home, &id)).await
}

#[tauri::command]
async fn restore_codex_session(id: String) -> Result<CodexSession, String> {
    with_codex_home(move |home| sessions::restore(home, &id)).await
}

#[tauri::command]
async fn delete_codex_session(id: String) -> Result<u64, String> {
    with_codex_home(move |home| sessions::delete(home, &id)).await
}

#[tauri::command]
async fn reveal_codex_session(id: String) -> Result<String, String> {
    let path = with_codex_home(move |home| sessions::session_path(home, &id)).await?;
    codex::reveal(&path).map_err(command_error)?;
    Ok(path)
}

/// Codex only needs one concrete model name; the gateway realigns it per upstream on failover.
/// Prefer a healthy node, but a not-yet-probed one still beats refusing to write the config.
fn codex_target(config: &AppConfig) -> Result<(String, String), String> {
    let mut candidates = config.routable().collect::<Vec<_>>();
    candidates.sort_by_key(|provider| (health_rank(provider.status), provider.priority));
    candidates
        .into_iter()
        .find(|provider| {
            let model = provider.effective_model();
            !model.is_empty() && model != "自动获取中"
        })
        .map(|provider| (provider.name.clone(), provider.effective_model().to_string()))
        .ok_or_else(|| "请先导入并测活至少一个 Provider".to_string())
}

fn health_rank(status: ProviderStatus) -> u8 {
    match status {
        ProviderStatus::Healthy => 0,
        ProviderStatus::Checking | ProviderStatus::Unknown => 1,
        ProviderStatus::Unhealthy => 2,
    }
}

async fn gateway_status(state: &AppState) -> Result<GatewayStatus, String> {
    let running = state.gateway.lock().await.is_some();
    let config = state.config.read().await;
    Ok(status_from(&config, &state.metrics, running))
}

fn status_from(config: &AppConfig, metrics: &GatewayMetrics, running: bool) -> GatewayStatus {
    let now = Utc::now();
    GatewayStatus {
        running,
        host: "127.0.0.1".into(),
        port: config.settings.gateway_port,
        request_count: metrics.requests.load(Ordering::Relaxed),
        success_count: metrics.successes.load(Ordering::Relaxed),
        failed_count: metrics.failures.load(Ordering::Relaxed),
        failover_count: metrics.failovers.load(Ordering::Relaxed),
        active_connections: metrics.active_connections.load(Ordering::Relaxed),
        average_latency_ms: average_latency(metrics),
        input_bytes: metrics.input_bytes.load(Ordering::Relaxed),
        output_bytes: metrics.output_bytes.load(Ordering::Relaxed),
        uptime_seconds: uptime_seconds(metrics, running),
        strategy: config.settings.route_strategy,
        routable_count: config.routable().filter(|provider| !provider.in_cooldown(now)).count(),
        cooldown_count: config.routable().filter(|provider| provider.in_cooldown(now)).count(),
    }
}

fn average_latency(metrics: &GatewayMetrics) -> u64 {
    let requests = metrics.requests.load(Ordering::Relaxed);
    if requests == 0 { 0 } else { metrics.total_latency_ms.load(Ordering::Relaxed) / requests }
}

fn uptime_seconds(metrics: &GatewayMetrics, running: bool) -> u64 {
    let started = metrics.started_at.load(Ordering::Relaxed);
    if !running || started == 0 { return 0; }
    (Utc::now().timestamp().max(0) as u64).saturating_sub(started)
}

fn validate_provider(input: &ProviderInput) -> Result<(), String> {
    if input.name.trim().is_empty() {
        return Err("名称不能为空".into());
    }
    if !(input.base_url.starts_with("https://") || input.base_url.starts_with("http://")) {
        return Err("API 地址必须以 http:// 或 https:// 开头".into());
    }
    if input.priority == 0 || input.priority > 99 {
        return Err("路由优先级必须在 1 到 99 之间".into());
    }
    Ok(())
}

fn validate_settings(mut settings: AppSettings) -> Result<AppSettings, String> {
    if !(1024..=65535).contains(&settings.gateway_port) {
        return Err("端口必须在 1024 到 65535 之间".into());
    }
    if !(10..=600).contains(&settings.request_timeout_seconds) {
        return Err("测活超时必须在 10 到 600 秒之间".into());
    }
    if !(1..=120).contains(&settings.connect_timeout_seconds) {
        return Err("连接超时必须在 1 到 120 秒之间".into());
    }
    if !(10..=3600).contains(&settings.stream_idle_timeout_seconds) {
        return Err("流空闲超时必须在 10 到 3600 秒之间".into());
    }
    if !(1..=120).contains(&settings.health_interval_minutes) {
        return Err("保活间隔必须在 1 到 120 分钟之间".into());
    }
    if settings.max_failover_attempts > 20 {
        return Err("最大切换次数不能超过 20".into());
    }
    if settings.cooldown_seconds > 900 {
        return Err("冷却时间不能超过 900 秒".into());
    }
    settings.local_access_key = settings.local_access_key.trim().to_owned();
    if settings.local_access_key.is_empty() {
        return Err("本地访问密钥不能为空".into());
    }
    Ok(settings)
}

fn extract_model_ids(payload: &serde_json::Value) -> Vec<String> {
    let entries = payload.get("data").and_then(serde_json::Value::as_array).or_else(|| payload.as_array());
    let mut models = entries.into_iter().flatten().filter_map(|entry| {
        entry.get("id").and_then(serde_json::Value::as_str).or_else(|| entry.as_str()).map(str::to_owned)
    }).collect::<Vec<_>>();
    models.sort();
    models.dedup();
    models
}

fn select_codex_model(models: &[String]) -> String {
    const PREFERRED: &[&str] = &["gpt-5.6-sol", "gpt-5.6", "gpt-5.6-terra", "gpt-5.3-codex", "gpt-5.5", "gpt-5.4"];
    for preferred in PREFERRED {
        if let Some(model) = models.iter().find(|model| model.eq_ignore_ascii_case(preferred)) {
            return model.clone();
        }
    }
    if let Some(model) = models.iter().find(|model| model.to_ascii_lowercase().contains("codex") && !model.to_ascii_lowercase().contains("review")) {
        return model.clone();
    }
    models.first().cloned().unwrap_or_else(|| "自动获取中".into())
}

/// Token-free probe of every enabled provider, persisted once at the end.
async fn run_health_sweep(state: &AppState) {
    let ids = state.config.read().await.providers.iter().filter(|provider| provider.enabled).map(|provider| provider.id.clone()).collect::<Vec<_>>();
    if ids.is_empty() {
        return;
    }
    for chunk in ids.chunks(HEALTH_CONCURRENCY) {
        // Periodic checks only validate authentication and model discovery to avoid recurring token charges.
        futures_util::future::join_all(chunk.iter().map(|id| check_provider(state, id, false))).await;
    }
    let _ = state.store.save(&state.config).await;
}

async fn background_health_checks(state: AppState) {
    // One sweep shortly after launch so the dashboard is accurate before the first interval elapses.
    tokio::time::sleep(Duration::from_secs(2)).await;
    loop {
        let settings = state.config.read().await.settings.clone();
        if settings.automatic_health_checks {
            run_health_sweep(&state).await;
        }
        let cooling = {
            let now = Utc::now();
            state.config.read().await.providers.iter().any(|provider| provider.in_cooldown(now))
        };
        let interval = Duration::from_secs(settings.health_interval_minutes.clamp(1, 120) * 60);
        let wait = if cooling { interval.min(RECOVERY_INTERVAL) } else { interval };
        // Saving settings wakes the loop so a new interval or toggle applies immediately.
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = state.health_notify.notified() => {}
        }
    }
}

fn load_config(path: &PathBuf) -> Result<AppConfig, RelayError> {
    let mut config = Store::load(path)?;
    for provider in &mut config.providers {
        provider.api_key = store::load_secret(&provider.id);
        provider.has_key = provider.api_key.is_some();
        provider.status = ProviderStatus::Unknown;
        // Circuit-breaker state is runtime-only; a restart deserves a clean slate.
        provider.clear_cooldown();
    }
    Ok(config)
}

#[cfg(test)]
mod model_tests {
    use super::{codex_target, extract_model_ids, is_valid_response_payload, select_codex_model, validate_settings};
    use crate::models::{AppConfig, AppSettings, Provider, ProviderStatus};

    fn provider(name: &str, priority: u32, status: ProviderStatus, model: &str) -> Provider {
        Provider {
            id: name.into(), name: name.into(), base_url: "https://relay.example/v1".into(), api_key: Some("sk".into()),
            has_key: true, model: model.into(), model_override: None, available_models: vec![model.into()], enabled: true,
            priority, status, latency_ms: None, last_checked_at: None, last_error: None, consecutive_failures: 0,
            cooldown_until: None, served_count: 0, failed_count: 0, health_history: Vec::new(),
        }
    }

    #[test]
    fn extracts_models_from_openai_list_shape() {
        let payload = serde_json::json!({"data": [{"id": "gpt-5.4"}, {"id": "gpt-5.6-sol"}]});
        assert_eq!(extract_model_ids(&payload), vec!["gpt-5.4", "gpt-5.6-sol"]);
    }

    #[test]
    fn prefers_sol_for_codex_when_available() {
        let models = vec!["codex-auto-review".into(), "gpt-5.4".into(), "gpt-5.6-sol".into()];
        assert_eq!(select_codex_model(&models), "gpt-5.6-sol");
    }

    #[test]
    fn accepts_openai_responses_payload() {
        assert!(is_valid_response_payload(&serde_json::json!({"id": "resp_123", "object": "response", "output": []})));
        assert!(!is_valid_response_payload(&serde_json::json!({"message": "login required"})));
    }

    #[test]
    fn codex_target_prefers_healthy_then_priority() {
        let config = AppConfig {
            providers: vec![
                provider("backup", 1, ProviderStatus::Unhealthy, "glm-5"),
                provider("main", 3, ProviderStatus::Healthy, "gpt-5.6-sol"),
                provider("spare", 2, ProviderStatus::Healthy, "gpt-5.4"),
            ],
            settings: AppSettings::default(),
        };
        assert_eq!(codex_target(&config).unwrap(), ("spare".into(), "gpt-5.4".into()));
    }

    #[test]
    fn codex_target_falls_back_to_an_unprobed_node() {
        let config = AppConfig { providers: vec![provider("fresh", 1, ProviderStatus::Unknown, "glm-5")], settings: AppSettings::default() };
        assert_eq!(codex_target(&config).unwrap().1, "glm-5");
    }

    #[test]
    fn codex_target_rejects_providers_without_a_discovered_model() {
        let config = AppConfig { providers: vec![provider("pending", 1, ProviderStatus::Unknown, "自动获取中")], settings: AppSettings::default() };
        assert!(codex_target(&config).is_err());
    }

    #[test]
    fn codex_target_uses_the_manual_pin() {
        let mut pinned = provider("pinned", 1, ProviderStatus::Healthy, "glm-5");
        pinned.model_override = Some("kimi-k3".into());
        let config = AppConfig { providers: vec![pinned], settings: AppSettings::default() };
        assert_eq!(codex_target(&config).unwrap().1, "kimi-k3");
    }

    #[test]
    fn settings_validation_guards_every_timeout() {
        let base = AppSettings::default();
        assert!(validate_settings(base.clone()).is_ok());
        assert!(validate_settings(AppSettings { gateway_port: 80, ..base.clone() }).is_err());
        assert!(validate_settings(AppSettings { connect_timeout_seconds: 0, ..base.clone() }).is_err());
        assert!(validate_settings(AppSettings { stream_idle_timeout_seconds: 5, ..base.clone() }).is_err());
        assert!(validate_settings(AppSettings { health_interval_minutes: 0, ..base.clone() }).is_err());
        assert!(validate_settings(AppSettings { max_failover_attempts: 50, ..base.clone() }).is_err());
        assert!(validate_settings(AppSettings { local_access_key: "   ".into(), ..base }).is_err());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let config_path = app.path().app_config_dir()?.join("relaydeck.json");
            let config = load_config(&config_path)?;
            let auto_start = config.settings.auto_start_gateway;
            let state = AppState {
                probe_client: Arc::new(RwLock::new(build_probe_client(&config.settings)?)),
                proxy_client: Arc::new(RwLock::new(build_proxy_client(&config.settings)?)),
                config: Arc::new(RwLock::new(config)),
                store: Store::new(config_path),
                gateway: Arc::new(Mutex::new(None)),
                metrics: Arc::new(GatewayMetrics::default()),
                logs: Arc::new(RwLock::new(VecDeque::new())),
                cursor: Arc::new(AtomicUsize::new(0)),
                health_notify: Arc::new(Notify::new()),
            };
            let gateway_state = state.clone();
            let health_state = state.clone();
            app.manage(state);
            tauri::async_runtime::spawn(async move {
                if auto_start {
                    if let Err(error) = start_gateway_inner(&gateway_state).await {
                        eprintln!("RelayDeck gateway autostart failed: {error}");
                    }
                }
            });
            tauri::async_runtime::spawn(background_health_checks(health_state));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot, save_provider, delete_provider, toggle_provider, test_provider, refresh_health,
            set_provider_model, move_provider, clear_provider_cooldown, import_providers, save_settings,
            start_gateway, stop_gateway, clear_logs, reset_access_key, apply_codex_config, restart_codex,
            apply_and_restart_codex, list_codex_sessions, repair_session_visibility, archive_codex_session,
            restore_codex_session, delete_codex_session, reveal_codex_session,
        ])
        .run(tauri::generate_context!())
        .expect("error while running RelayDeck");
}
