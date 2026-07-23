mod codex;
mod gateway;
mod models;
mod store;

use std::{
    collections::VecDeque,
    io,
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use chrono::Utc;
use models::{AppConfig, AppSettings, AppSnapshot, CodexApplyResult, GatewayStatus, Provider, ProviderInput, ProviderStatus, RequestLog};
use tauri::{Manager, State};
use tokio::{sync::{oneshot, Mutex, RwLock}, task::JoinHandle};
use uuid::Uuid;

use crate::{gateway::{GatewayContext, GatewayMetrics}, store::Store};

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
    client: reqwest::Client,
    gateway: Arc<Mutex<Option<GatewayRuntime>>>,
    metrics: Arc<GatewayMetrics>,
    logs: Arc<RwLock<VecDeque<RequestLog>>>,
}

fn command_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[tauri::command]
async fn get_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    let config = state.config.read().await.clone();
    let running = state.gateway.lock().await.is_some();
    let logs = state.logs.read().await.iter().cloned().collect();
    Ok(AppSnapshot {
        providers: config.providers,
        gateway: GatewayStatus {
            running,
            host: "127.0.0.1".into(),
            port: config.settings.gateway_port,
            request_count: state.metrics.requests.load(Ordering::Relaxed),
            success_count: state.metrics.successes.load(Ordering::Relaxed),
            failed_count: state.metrics.failures.load(Ordering::Relaxed),
            failover_count: state.metrics.failovers.load(Ordering::Relaxed),
            active_connections: state.metrics.active_connections.load(Ordering::Relaxed),
            average_latency_ms: average_latency(&state.metrics),
            input_bytes: state.metrics.input_bytes.load(Ordering::Relaxed),
            output_bytes: state.metrics.output_bytes.load(Ordering::Relaxed),
            uptime_seconds: uptime_seconds(&state.metrics, running),
        },
        logs,
        settings: config.settings,
        codex: codex::status(),
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
        available_models: existing.as_ref().map(|value| value.available_models.clone()).unwrap_or_default(), enabled: input.enabled, priority: input.priority,
        status: existing.as_ref().map(|value| value.status.clone()).unwrap_or(ProviderStatus::Unknown),
        latency_ms: existing.as_ref().and_then(|value| value.latency_ms),
        last_checked_at: existing.as_ref().and_then(|value| value.last_checked_at),
        last_error: existing.as_ref().and_then(|value| value.last_error.clone()),
    };
    if let Some(position) = config.providers.iter().position(|item| item.id == id) {
        config.providers[position] = provider.clone();
    } else {
        config.providers.push(provider.clone());
    }
    drop(config);
    state.store.save(&state.config).await.map_err(command_error)?;
    test_provider_inner(state.inner(), &id).await.or(Ok(provider))
}

#[tauri::command]
async fn delete_provider(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.config.write().await.providers.retain(|provider| provider.id != id);
    store::delete_secret(&id);
    state.store.save(&state.config).await.map_err(command_error)
}

#[tauri::command]
async fn toggle_provider(id: String, enabled: bool, state: State<'_, AppState>) -> Result<(), String> {
    let mut config = state.config.write().await;
    let provider = config.providers.iter_mut().find(|provider| provider.id == id).ok_or_else(|| "Provider 不存在".to_string())?;
    provider.enabled = enabled;
    drop(config);
    state.store.save(&state.config).await.map_err(command_error)
}

#[tauri::command]
async fn test_provider(id: String, state: State<'_, AppState>) -> Result<Provider, String> {
    test_provider_inner(&state, &id).await
}

async fn test_provider_inner(state: &AppState, id: &str) -> Result<Provider, String> {
    let provider = {
        let mut config = state.config.write().await;
        let provider = config.providers.iter_mut().find(|provider| provider.id == id).ok_or_else(|| "Provider 不存在".to_string())?;
        provider.status = ProviderStatus::Checking;
        provider.clone()
    };
    let started = Instant::now();
    let url = gateway::join_url(&provider.base_url, "/v1/models");
    let result = state.client.get(url).bearer_auth(provider.api_key.as_deref().unwrap_or_default()).send().await;
    let mut updated = provider;
    updated.last_checked_at = Some(Utc::now());
    match result {
        Ok(response) if response.status().is_success() => match response.json::<serde_json::Value>().await {
            Ok(payload) => {
                let models = extract_model_ids(&payload);
                if models.is_empty() {
                    updated.status = ProviderStatus::Unhealthy;
                    updated.latency_ms = Some(started.elapsed().as_millis() as u64);
                    updated.last_error = Some("/models 没有返回可用模型".into());
                } else {
                    updated.model = select_codex_model(&models);
                    updated.available_models = models;
                    updated.status = ProviderStatus::Healthy;
                    updated.latency_ms = Some(started.elapsed().as_millis() as u64);
                    updated.last_error = None;
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
    let mut config = state.config.write().await;
    if let Some(position) = config.providers.iter().position(|provider| provider.id == id) {
        config.providers[position] = updated.clone();
    }
    drop(config);
    state.store.save(&state.config).await.map_err(command_error)?;
    Ok(updated)
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
            model: "自动获取中".into(), available_models: Vec::new(), enabled: input.enabled, priority: input.priority, status: ProviderStatus::Unknown,
            latency_ms: None, last_checked_at: None, last_error: None,
        };
        imported_ids.push(provider.id.clone());
        config.providers.push(provider);
    }
    drop(config);
    state.store.save(&state.config).await.map_err(command_error)?;
    let mut imported = Vec::with_capacity(imported_ids.len());
    for id in imported_ids {
        imported.push(test_provider_inner(state.inner(), &id).await?);
    }
    Ok(imported)
}

#[tauri::command]
async fn save_settings(settings: AppSettings, state: State<'_, AppState>) -> Result<(), String> {
    if !(1024..=65535).contains(&settings.gateway_port) {
        return Err("端口必须在 1024 到 65535 之间".into());
    }
    if !(10..=600).contains(&settings.request_timeout_seconds) {
        return Err("请求超时必须在 10 到 600 秒之间".into());
    }
    if settings.local_access_key.trim().is_empty() {
        return Err("本地访问密钥不能为空".into());
    }
    state.config.write().await.settings = settings;
    state.store.save(&state.config).await.map_err(command_error)
}

#[tauri::command]
async fn start_gateway(state: State<'_, AppState>) -> Result<GatewayStatus, String> {
    start_gateway_inner(state.inner()).await
}

async fn start_gateway_inner(state: &AppState) -> Result<GatewayStatus, String> {
    let mut runtime = state.gateway.lock().await;
    if runtime.is_none() {
        let port = state.config.read().await.settings.gateway_port;
        let listener = gateway::bind(port).await.map_err(command_error)?;
        let (shutdown, receiver) = oneshot::channel();
        let context = GatewayContext { config: state.config.clone(), client: state.client.clone(), metrics: state.metrics.clone(), logs: state.logs.clone() };
        let task = tokio::spawn(gateway::serve(listener, context, receiver));
        *runtime = Some(GatewayRuntime { shutdown, task });
        state.metrics.started_at.store(Utc::now().timestamp().max(0) as u64, Ordering::Relaxed);
    }
    drop(runtime);
    gateway_status(&state).await
}

#[tauri::command]
async fn stop_gateway(state: State<'_, AppState>) -> Result<GatewayStatus, String> {
    let current = state.gateway.lock().await.take();
    if let Some(runtime) = current {
        let _ = runtime.shutdown.send(());
        let _ = runtime.task.await;
    }
    state.metrics.started_at.store(0, Ordering::Relaxed);
    gateway_status(&state).await
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

#[tauri::command]
async fn apply_codex_config(state: State<'_, AppState>) -> Result<CodexApplyResult, String> {
    let config = state.config.read().await;
    let mut providers = config.providers.iter().filter(|provider| provider.enabled && provider.status == ProviderStatus::Healthy).collect::<Vec<_>>();
    providers.sort_by_key(|provider| provider.priority);
    let provider = providers.first().ok_or_else(|| "请先导入并测活至少一个 Provider".to_string())?;
    codex::apply(&config.settings, &provider.model).map_err(command_error)
}

async fn gateway_status(state: &AppState) -> Result<GatewayStatus, String> {
    let running = state.gateway.lock().await.is_some();
    let config = state.config.read().await;
    Ok(GatewayStatus {
        running, host: "127.0.0.1".into(), port: config.settings.gateway_port,
        request_count: state.metrics.requests.load(Ordering::Relaxed), success_count: state.metrics.successes.load(Ordering::Relaxed),
        failed_count: state.metrics.failures.load(Ordering::Relaxed), failover_count: state.metrics.failovers.load(Ordering::Relaxed),
        active_connections: state.metrics.active_connections.load(Ordering::Relaxed),
        average_latency_ms: average_latency(&state.metrics), input_bytes: state.metrics.input_bytes.load(Ordering::Relaxed),
        output_bytes: state.metrics.output_bytes.load(Ordering::Relaxed), uptime_seconds: uptime_seconds(&state.metrics, running),
    })
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

async fn background_health_checks(state: AppState) {
    loop {
        let settings = state.config.read().await.settings.clone();
        tokio::time::sleep(Duration::from_secs(settings.health_interval_minutes.max(1) * 60)).await;
        if !settings.automatic_health_checks {
            continue;
        }
        let ids = state.config.read().await.providers.iter().filter(|provider| provider.enabled).map(|provider| provider.id.clone()).collect::<Vec<_>>();
        for id in ids {
            let _ = test_provider_inner(&state, &id).await;
        }
    }
}

fn load_config(path: &PathBuf) -> Result<AppConfig, RelayError> {
    let mut config = Store::load(path)?;
    for provider in &mut config.providers {
        provider.api_key = store::load_secret(&provider.id);
        provider.has_key = provider.api_key.is_some();
        provider.status = ProviderStatus::Unknown;
    }
    Ok(config)
}

#[cfg(test)]
mod model_tests {
    use super::{extract_model_ids, select_codex_model};

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
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let config_path = app.path().app_config_dir()?.join("relaydeck.json");
            let config = load_config(&config_path)?;
            let timeout = config.settings.request_timeout_seconds;
            let auto_start = config.settings.auto_start_gateway;
            let state = AppState {
                config: Arc::new(RwLock::new(config)),
                store: Store::new(config_path),
                client: reqwest::Client::builder().timeout(Duration::from_secs(timeout)).build()?,
                gateway: Arc::new(Mutex::new(None)),
                metrics: Arc::new(GatewayMetrics::default()),
                logs: Arc::new(RwLock::new(VecDeque::new())),
            };
            let gateway_state = state.clone();
            let health_state = state.clone();
            app.manage(state);
            tauri::async_runtime::spawn(async move {
                if auto_start {
                    let _ = start_gateway_inner(&gateway_state).await;
                }
            });
            tauri::async_runtime::spawn(background_health_checks(health_state));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot, save_provider, delete_provider, toggle_provider, test_provider,
            import_providers, save_settings, start_gateway, stop_gateway, clear_logs, reset_access_key, apply_codex_config,
        ])
        .run(tauri::generate_context!())
        .expect("error while running RelayDeck");
}
