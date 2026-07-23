use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
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
    models::{AppConfig, Provider, RequestLog},
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
    pub client: reqwest::Client,
    pub metrics: Arc<GatewayMetrics>,
    pub logs: Arc<RwLock<VecDeque<RequestLog>>>,
}

struct ActiveRequest(Arc<GatewayMetrics>);

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.active_connections.fetch_sub(1, Ordering::Relaxed);
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
    let authorized = {
        let config = context.config.read().await;
        let expected = config.settings.local_access_key.as_str();
        let supplied = request.headers().get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()).and_then(|value| value.strip_prefix("Bearer "));
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

    let mut providers = {
        let config = context.config.read().await;
        config.providers.iter().filter(|provider| provider.enabled && provider.api_key.is_some()).cloned().collect::<Vec<_>>()
    };
    providers.sort_by_key(|provider| provider.priority);
    if providers.is_empty() {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "no enabled provider with an API key".into());
    }

    let path = parts.uri.path_and_query().map(|value| value.as_str()).unwrap_or("/");
    let mut attempts = 0;
    let mut last_error = String::new();

    for provider in providers {
        attempts += 1;
        if attempts > 1 {
            context.metrics.failovers.fetch_add(1, Ordering::Relaxed);
        }
        match send_request(&context, &provider, &parts.method, path, &parts.headers, request_body.clone()).await {
            Ok(response) => {
                let status = response.status();
                if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    last_error = format!("{} returned {status}", provider.name);
                    continue;
                }

                let elapsed = started.elapsed().as_millis() as u64;
                context.metrics.total_latency_ms.fetch_add(elapsed, Ordering::Relaxed);
                if status.is_success() {
                    context.metrics.successes.fetch_add(1, Ordering::Relaxed);
                } else {
                    context.metrics.failures.fetch_add(1, Ordering::Relaxed);
                }
                push_log(&context.logs, RequestLog {
                    id: Uuid::new_v4().to_string(), timestamp: Utc::now(), method: parts.method.to_string(), path: path.to_string(),
                    provider_name: provider.name.clone(), status_code: status.as_u16(), latency_ms: elapsed, attempts,
                }).await;
                return proxy_response(response, context.metrics.clone());
            }
            Err(error) => {
                last_error = format!("{}: {error}", provider.name);
            }
        }
    }

    let elapsed = started.elapsed().as_millis() as u64;
    context.metrics.failures.fetch_add(1, Ordering::Relaxed);
    context.metrics.total_latency_ms.fetch_add(elapsed, Ordering::Relaxed);
    push_log(&context.logs, RequestLog {
        id: Uuid::new_v4().to_string(), timestamp: Utc::now(), method: parts.method.to_string(), path: path.to_string(),
        provider_name: "无可用节点".into(), status_code: 502, latency_ms: elapsed, attempts,
    }).await;
    error_response(StatusCode::BAD_GATEWAY, format!("all providers failed: {last_error}"))
}

async fn send_request(
    context: &GatewayContext,
    provider: &Provider,
    method: &axum::http::Method,
    path: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<reqwest::Response, reqwest::Error> {
    let target = join_url(&provider.base_url, path);
    let mut builder = context.client.request(method.clone(), target).body(body);
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
    use super::join_url;

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
}
