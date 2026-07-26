use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, StatusCode},
    response::Response,
    Router,
};
use bytes::Bytes;
use chrono::Utc;
use futures_util::TryStreamExt;
use tokio::{net::TcpListener, sync::RwLock};
use uuid::Uuid;

use crate::{
    models::{AppConfig, AppSettings, Provider, ProviderStatus, RequestLog, RouteStrategy},
    RelayError,
};

const MAX_BODY_SIZE: usize = 64 * 1024 * 1024;
const MAX_LOGS: usize = 300;

#[derive(Default)]
pub struct GatewayMetrics {
    pub requests: AtomicU64,
    pub successes: AtomicU64,
    pub failures: AtomicU64,
    pub failovers: AtomicU64,
    pub active_connections: AtomicU64,
    pub total_latency_ms: AtomicU64,
    pub input_bytes: AtomicU64,
    pub output_bytes: AtomicU64,
    pub started_at: AtomicU64,
}

#[derive(Clone)]
pub struct GatewayContext {
    pub config: Arc<RwLock<AppConfig>>,
    /// Swapped whenever timeouts or keep-alive settings change, so live requests keep their client.
    pub client: Arc<RwLock<reqwest::Client>>,
    pub metrics: Arc<GatewayMetrics>,
    pub logs: Arc<RwLock<VecDeque<RequestLog>>>,
    pub cursor: Arc<AtomicUsize>,
}

struct ActiveRequest(Arc<GatewayMetrics>);

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.active_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Why an attempt against one upstream did not produce a usable response.
enum Failure {
    Transport(String),
    Status(StatusCode),
}

impl Failure {
    fn message(&self, provider: &str) -> String {
        match self {
            Self::Transport(error) => format!("{provider}: {error}"),
            Self::Status(status) => format!("{provider} 返回 HTTP {status}"),
        }
    }

    /// Rate limiting says "later", everything else says "this upstream is broken".
    fn marks_unhealthy(&self) -> bool {
        !matches!(self, Self::Status(status) if *status == StatusCode::TOO_MANY_REQUESTS)
    }
}

pub async fn serve(listener: TcpListener, context: GatewayContext, shutdown: tokio::sync::oneshot::Receiver<()>) {
    let app = Router::new().fallback(forward).with_state(context);
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async { let _ = shutdown.await; })
        .await;
    if let Err(error) = result {
        eprintln!("RelayDeck gateway stopped: {error}");
    }
}

async fn forward(State(context): State<GatewayContext>, request: Request) -> Response {
    let started = Instant::now();
    let settings = context.config.read().await.settings.clone();
    let authorized = {
        let expected = settings.local_access_key.as_str();
        let supplied = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        expected.is_empty() || supplied == Some(expected)
    };
    if !authorized {
        return error_response(StatusCode::UNAUTHORIZED, "invalid RelayDeck local access key".into());
    }
    context.metrics.requests.fetch_add(1, Ordering::Relaxed);
    context.metrics.active_connections.fetch_add(1, Ordering::Relaxed);
    let _active = ActiveRequest(context.metrics.clone());

    let (parts, body) = request.into_parts();
    let request_body = match to_bytes(body, MAX_BODY_SIZE).await {
        Ok(body) => body,
        Err(error) => return error_response(StatusCode::PAYLOAD_TOO_LARGE, format!("request body rejected: {error}")),
    };
    context.metrics.input_bytes.fetch_add(request_body.len() as u64, Ordering::Relaxed);

    let candidates = {
        let config = context.config.read().await;
        select_candidates(&config, &settings, &context.cursor)
    };
    if candidates.is_empty() {
        context.metrics.failures.fetch_add(1, Ordering::Relaxed);
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "没有可用的中转节点，请先启用并测活 Provider".into());
    }

    let path = parts.uri.path_and_query().map(|value| value.as_str()).unwrap_or("/");
    let requested_model = requested_model(&request_body);
    let budget = attempt_budget(candidates.len(), settings.max_failover_attempts);
    let client = context.client.read().await.clone();
    let mut attempts = 0usize;
    let mut last_error = String::new();

    for provider in candidates.iter().take(budget) {
        attempts += 1;
        if attempts > 1 {
            context.metrics.failovers.fetch_add(1, Ordering::Relaxed);
        }
        let last_attempt = attempts >= budget;
        let (payload, upstream_model) = align_model(&request_body, provider, requested_model.as_deref(), &settings);

        let failure = match send_request(&client, provider, &parts.method, path, &parts.headers, payload).await {
            Ok(response) => {
                let status = response.status();
                if should_failover(status, &settings) && !last_attempt {
                    Failure::Status(status)
                } else {
                    let elapsed = started.elapsed().as_millis() as u64;
                    context.metrics.total_latency_ms.fetch_add(elapsed, Ordering::Relaxed);
                    if status.is_success() {
                        context.metrics.successes.fetch_add(1, Ordering::Relaxed);
                        record_success(&context.config, &provider.id).await;
                    } else {
                        context.metrics.failures.fetch_add(1, Ordering::Relaxed);
                        record_failure(&context.config, &provider.id, &Failure::Status(status), &settings).await;
                    }
                    push_log(&context.logs, RequestLog {
                        id: Uuid::new_v4().to_string(), timestamp: Utc::now(), method: parts.method.to_string(),
                        path: path.to_string(), provider_name: provider.name.clone(), status_code: status.as_u16(),
                        latency_ms: elapsed, attempts, requested_model: requested_model.clone(),
                        upstream_model, error: (!status.is_success()).then(|| format!("HTTP {status}")),
                    }).await;
                    return proxy_response(response, context.metrics.clone());
                }
            }
            Err(error) => Failure::Transport(error.to_string()),
        };

        last_error = failure.message(&provider.name);
        record_failure(&context.config, &provider.id, &failure, &settings).await;
    }

    let elapsed = started.elapsed().as_millis() as u64;
    context.metrics.failures.fetch_add(1, Ordering::Relaxed);
    context.metrics.total_latency_ms.fetch_add(elapsed, Ordering::Relaxed);
    push_log(&context.logs, RequestLog {
        id: Uuid::new_v4().to_string(), timestamp: Utc::now(), method: parts.method.to_string(), path: path.to_string(),
        provider_name: "无可用节点".into(), status_code: 502, latency_ms: elapsed, attempts,
        requested_model, upstream_model: None, error: Some(last_error.clone()),
    }).await;
    error_response(StatusCode::BAD_GATEWAY, format!("所有节点都失败了: {last_error}"))
}

fn attempt_budget(candidates: usize, max_attempts: u32) -> usize {
    if max_attempts == 0 { candidates } else { candidates.min(max_attempts as usize) }
}

/// Ordered upstreams for one request: ready nodes first, then everything else as a last resort.
fn select_candidates(config: &AppConfig, settings: &AppSettings, cursor: &AtomicUsize) -> Vec<Provider> {
    let now = Utc::now();
    let mut ready = Vec::new();
    let mut held_back = Vec::new();
    for provider in config.routable() {
        let sidelined = provider.in_cooldown(now)
            || (settings.skip_unhealthy && provider.status == ProviderStatus::Unhealthy);
        if sidelined { held_back.push(provider.clone()) } else { ready.push(provider.clone()) }
    }

    match settings.route_strategy {
        RouteStrategy::Priority => ready.sort_by(|a, b| a.priority.cmp(&b.priority).then(latency_key(a).cmp(&latency_key(b)))),
        RouteStrategy::Fastest => ready.sort_by(|a, b| latency_key(a).cmp(&latency_key(b)).then(a.priority.cmp(&b.priority))),
        RouteStrategy::RoundRobin => {
            ready.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
            let width = ready.len();
            if width > 1 {
                ready.rotate_left(cursor.fetch_add(1, Ordering::Relaxed) % width);
            }
        }
    }
    held_back.sort_by(|a, b| a.priority.cmp(&b.priority));
    ready.extend(held_back);
    ready
}

fn latency_key(provider: &Provider) -> u64 {
    provider.latency_ms.unwrap_or(u64::MAX)
}

fn should_failover(status: StatusCode, settings: &AppSettings) -> bool {
    if status.is_server_error() {
        return true;
    }
    if matches!(status, StatusCode::TOO_MANY_REQUESTS | StatusCode::REQUEST_TIMEOUT | StatusCode::CONFLICT | StatusCode::TOO_EARLY) {
        return true;
    }
    settings.failover_on_client_errors
        && matches!(status, StatusCode::UNAUTHORIZED | StatusCode::PAYMENT_REQUIRED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND)
}

/// Keeps the upstream model consistent with what that upstream actually serves.
fn align_model(body: &Bytes, provider: &Provider, requested: Option<&str>, settings: &AppSettings) -> (Bytes, Option<String>) {
    let Some(requested) = requested else { return (body.clone(), None) };
    if !settings.remap_model || provider.available_models.is_empty() || provider.knows_model(requested) {
        return (body.clone(), Some(requested.to_string()));
    }
    let target = provider.effective_model().to_string();
    if target.is_empty() || target.eq_ignore_ascii_case(requested) {
        return (body.clone(), Some(requested.to_string()));
    }
    match rewrite_model(body, &target) {
        Some(rewritten) => (rewritten, Some(target)),
        None => (body.clone(), Some(requested.to_string())),
    }
}

fn requested_model(body: &Bytes) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::to_owned)
}

fn rewrite_model(body: &Bytes, model: &str) -> Option<Bytes> {
    let mut payload = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    *payload.get_mut("model")? = serde_json::Value::String(model.to_string());
    serde_json::to_vec(&payload).ok().map(Bytes::from)
}

async fn record_success(config: &RwLock<AppConfig>, id: &str) {
    let mut config = config.write().await;
    if let Some(provider) = config.providers.iter_mut().find(|provider| provider.id == id) {
        provider.clear_cooldown();
        provider.served_count += 1;
        provider.last_error = None;
        if provider.status != ProviderStatus::Checking {
            provider.status = ProviderStatus::Healthy;
        }
    }
}

async fn record_failure(config: &RwLock<AppConfig>, id: &str, failure: &Failure, settings: &AppSettings) {
    let now = Utc::now();
    let mut config = config.write().await;
    let Some(provider) = config.providers.iter_mut().find(|provider| provider.id == id) else { return };
    provider.failed_count += 1;
    provider.consecutive_failures = provider.consecutive_failures.saturating_add(1);
    provider.last_error = Some(failure.message(&provider.name));
    provider.apply_cooldown(settings.cooldown_seconds, now);
    if failure.marks_unhealthy() && provider.status != ProviderStatus::Checking {
        provider.status = ProviderStatus::Unhealthy;
    }
}

async fn send_request(
    client: &reqwest::Client,
    provider: &Provider,
    method: &axum::http::Method,
    path: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<reqwest::Response, reqwest::Error> {
    let target = join_url(&provider.base_url, path);
    let mut builder = client.request(method.clone(), target).body(body);
    for (name, value) in headers {
        if is_hop_header(name) || *name == header::AUTHORIZATION || name.as_str().eq_ignore_ascii_case("x-api-key") {
            continue;
        }
        builder = builder.header(name, value);
    }
    if let Some(api_key) = &provider.api_key {
        builder = builder.bearer_auth(api_key);
    }
    builder.send().await
}

fn proxy_response(response: reqwest::Response, metrics: Arc<GatewayMetrics>) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let stream = response.bytes_stream().map_ok(move |chunk| {
        metrics.output_bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        chunk
    }).map_err(std::io::Error::other);
    let mut builder = Response::builder().status(status);
    if let Some(output_headers) = builder.headers_mut() {
        for (name, value) in &headers {
            if !is_hop_header(name) {
                output_headers.insert(name, value.clone());
            }
        }
    }
    builder.body(Body::from_stream(stream)).unwrap_or_else(|_| error_response(StatusCode::BAD_GATEWAY, "failed to build upstream response".into()))
}

fn error_response(status: StatusCode, message: String) -> Response {
    let payload = serde_json::json!({ "error": { "message": message, "type": "relaydeck_gateway_error" } });
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .expect("static error response is valid")
}

fn is_hop_header(name: &HeaderName) -> bool {
    matches!(name.as_str().to_ascii_lowercase().as_str(), "host" | "connection" | "content-length" | "transfer-encoding" | "upgrade" | "proxy-authorization" | "proxy-authenticate" | "te" | "trailer" | "keep-alive")
}

pub fn join_url(base_url: &str, request_path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = if base.ends_with("/v1") && request_path.starts_with("/v1/") { &request_path[3..] } else { request_path };
    format!("{base}/{}", path.trim_start_matches('/'))
}

async fn push_log(logs: &RwLock<VecDeque<RequestLog>>, log: RequestLog) {
    let mut logs = logs.write().await;
    logs.push_front(log);
    logs.truncate(MAX_LOGS);
}

pub async fn bind(port: u16) -> Result<TcpListener, RelayError> {
    Ok(TcpListener::bind(("127.0.0.1", port)).await?)
}

#[cfg(test)]
mod tests {
    use super::{align_model, attempt_budget, join_url, requested_model, select_candidates, should_failover};
    use crate::models::{AppConfig, AppSettings, Provider, ProviderStatus, RouteStrategy};
    use axum::http::StatusCode;
    use bytes::Bytes;
    use chrono::{Duration, Utc};
    use std::sync::atomic::AtomicUsize;

    fn provider(id: &str, priority: u32, latency: Option<u64>, models: &[&str]) -> Provider {
        Provider {
            id: id.into(), name: id.into(), base_url: "https://relay.example/v1".into(), api_key: Some("sk".into()),
            has_key: true, model: models.first().copied().unwrap_or("gpt-5.6-sol").into(), model_override: None,
            available_models: models.iter().map(|value| (*value).to_string()).collect(), enabled: true, priority,
            status: ProviderStatus::Healthy, latency_ms: latency, last_checked_at: None, last_error: None,
            consecutive_failures: 0, cooldown_until: None, served_count: 0, failed_count: 0, health_history: Vec::new(),
        }
    }

    fn config(providers: Vec<Provider>, strategy: RouteStrategy) -> AppConfig {
        AppConfig { providers, settings: AppSettings { route_strategy: strategy, ..AppSettings::default() } }
    }

    #[test]
    fn avoids_duplicate_v1_prefix() {
        assert_eq!(join_url("https://relay.example/v1", "/v1/responses"), "https://relay.example/v1/responses");
    }

    #[test]
    fn keeps_provider_subpath() {
        assert_eq!(
            join_url("https://relay.example/openai/v1/", "/v1/models?limit=10"),
            "https://relay.example/openai/v1/models?limit=10"
        );
    }

    #[test]
    fn appends_full_path_to_origin() {
        assert_eq!(join_url("http://127.0.0.1:9000", "/v1/responses"), "http://127.0.0.1:9000/v1/responses");
    }

    #[test]
    fn priority_strategy_orders_by_priority_then_latency() {
        let config = config(vec![provider("c", 2, Some(90), &[]), provider("a", 1, Some(500), &[]), provider("b", 1, Some(120), &[])], RouteStrategy::Priority);
        let order = select_candidates(&config, &config.settings, &AtomicUsize::new(0));
        assert_eq!(order.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["b", "a", "c"]);
    }

    #[test]
    fn fastest_strategy_ignores_priority() {
        let config = config(vec![provider("slow", 1, Some(900), &[]), provider("quick", 9, Some(80), &[])], RouteStrategy::Fastest);
        let order = select_candidates(&config, &config.settings, &AtomicUsize::new(0));
        assert_eq!(order.first().unwrap().id, "quick");
    }

    #[test]
    fn round_robin_advances_each_request() {
        let config = config(vec![provider("a", 1, None, &[]), provider("b", 2, None, &[]), provider("c", 3, None, &[])], RouteStrategy::RoundRobin);
        let cursor = AtomicUsize::new(0);
        let first = select_candidates(&config, &config.settings, &cursor);
        let second = select_candidates(&config, &config.settings, &cursor);
        let third = select_candidates(&config, &config.settings, &cursor);
        assert_eq!(first.first().unwrap().id, "a");
        assert_eq!(second.first().unwrap().id, "b");
        assert_eq!(third.first().unwrap().id, "c");
        // Every node stays reachable, only the head rotates.
        assert_eq!(second.len(), 3);
    }

    #[test]
    fn cooling_and_unhealthy_nodes_drop_to_the_back_but_stay_reachable() {
        let mut cooling = provider("cooling", 1, Some(10), &[]);
        cooling.cooldown_until = Some(Utc::now() + Duration::seconds(120));
        let mut broken = provider("broken", 2, Some(20), &[]);
        broken.status = ProviderStatus::Unhealthy;
        let config = config(vec![cooling, broken, provider("good", 8, Some(300), &[])], RouteStrategy::Priority);
        let order = select_candidates(&config, &config.settings, &AtomicUsize::new(0));
        assert_eq!(order.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["good", "cooling", "broken"]);
    }

    #[test]
    fn disabled_and_keyless_providers_never_route() {
        let mut disabled = provider("disabled", 1, None, &[]);
        disabled.enabled = false;
        let mut keyless = provider("keyless", 1, None, &[]);
        keyless.api_key = None;
        let config = config(vec![disabled, keyless], RouteStrategy::Priority);
        assert!(select_candidates(&config, &config.settings, &AtomicUsize::new(0)).is_empty());
    }

    #[test]
    fn failover_covers_overload_and_optionally_auth() {
        let mut settings = AppSettings::default();
        assert!(should_failover(StatusCode::TOO_MANY_REQUESTS, &settings));
        assert!(should_failover(StatusCode::BAD_GATEWAY, &settings));
        assert!(should_failover(StatusCode::UNAUTHORIZED, &settings));
        assert!(!should_failover(StatusCode::BAD_REQUEST, &settings));
        assert!(!should_failover(StatusCode::OK, &settings));
        settings.failover_on_client_errors = false;
        assert!(!should_failover(StatusCode::UNAUTHORIZED, &settings));
    }

    #[test]
    fn model_is_rewritten_only_when_the_target_cannot_serve_it() {
        let settings = AppSettings::default();
        let body = Bytes::from(br#"{"model":"gpt-5.6-sol","input":"hi"}"#.to_vec());
        assert_eq!(requested_model(&body).as_deref(), Some("gpt-5.6-sol"));

        let knows = provider("knows", 1, None, &["gpt-5.6-sol", "gpt-5.4"]);
        let (payload, model) = align_model(&body, &knows, Some("gpt-5.6-sol"), &settings);
        assert_eq!(payload, body);
        assert_eq!(model.as_deref(), Some("gpt-5.6-sol"));

        let other = provider("other", 1, None, &["glm-5", "kimi-k3"]);
        let (payload, model) = align_model(&body, &other, Some("gpt-5.6-sol"), &settings);
        assert_eq!(model.as_deref(), Some("glm-5"));
        assert_eq!(requested_model(&payload).as_deref(), Some("glm-5"));
        assert!(String::from_utf8_lossy(&payload).contains("\"input\":\"hi\""));
    }

    #[test]
    fn manual_pin_is_used_when_remapping() {
        let settings = AppSettings::default();
        let body = Bytes::from(br#"{"model":"gpt-5.6-sol"}"#.to_vec());
        let mut pinned = provider("pinned", 1, None, &["glm-5", "kimi-k3"]);
        pinned.model_override = Some("kimi-k3".into());
        let (_, model) = align_model(&body, &pinned, Some("gpt-5.6-sol"), &settings);
        assert_eq!(model.as_deref(), Some("kimi-k3"));
    }

    #[test]
    fn remapping_can_be_switched_off_and_bodyless_requests_pass_through() {
        let settings = AppSettings { remap_model: false, ..AppSettings::default() };
        let body = Bytes::from(br#"{"model":"gpt-5.6-sol"}"#.to_vec());
        let other = provider("other", 1, None, &["glm-5"]);
        let (payload, model) = align_model(&body, &other, Some("gpt-5.6-sol"), &settings);
        assert_eq!(payload, body);
        assert_eq!(model.as_deref(), Some("gpt-5.6-sol"));

        let empty = Bytes::new();
        assert_eq!(requested_model(&empty), None);
        let (payload, model) = align_model(&empty, &other, None, &AppSettings::default());
        assert!(payload.is_empty());
        assert_eq!(model, None);
    }

    #[test]
    fn attempt_budget_respects_the_configured_ceiling() {
        assert_eq!(attempt_budget(5, 3), 3);
        assert_eq!(attempt_budget(2, 3), 2);
        assert_eq!(attempt_budget(5, 0), 5);
    }
}
