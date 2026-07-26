use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Health samples kept per provider so the UI can draw a short availability trend.
pub const HEALTH_HISTORY_LIMIT: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(skip, default)]
    pub api_key: Option<String>,
    pub has_key: bool,
    pub model: String,
    /// Model the user pinned by hand. Empty means "follow automatic selection".
    #[serde(default)]
    pub model_override: Option<String>,
    #[serde(default)]
    pub available_models: Vec<String>,
    pub enabled: bool,
    pub priority: u32,
    pub status: ProviderStatus,
    pub latency_ms: Option<u64>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub cooldown_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub served_count: u64,
    #[serde(default)]
    pub failed_count: u64,
    #[serde(default)]
    pub health_history: Vec<HealthSample>,
}

impl Provider {
    /// Model actually sent upstream: the manual pin when present, otherwise the discovered one.
    pub fn effective_model(&self) -> &str {
        self.model_override
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(self.model.as_str())
    }

    pub fn in_cooldown(&self, now: DateTime<Utc>) -> bool {
        self.cooldown_until.is_some_and(|until| until > now)
    }

    pub fn knows_model(&self, model: &str) -> bool {
        self.available_models.iter().any(|known| known.eq_ignore_ascii_case(model))
    }

    pub fn record_health(&mut self, ok: bool, latency_ms: Option<u64>, at: DateTime<Utc>) {
        self.health_history.push(HealthSample { at, ok, latency_ms });
        let overflow = self.health_history.len().saturating_sub(HEALTH_HISTORY_LIMIT);
        if overflow > 0 {
            self.health_history.drain(0..overflow);
        }
    }

    /// Escalating backoff so a flapping upstream is retried but does not stall every request.
    pub fn apply_cooldown(&mut self, base_seconds: u64, now: DateTime<Utc>) {
        if base_seconds == 0 || self.consecutive_failures < 2 {
            self.cooldown_until = None;
            return;
        }
        let steps = (self.consecutive_failures - 2).min(4);
        let seconds = base_seconds.saturating_mul(1u64 << steps).clamp(1, 900);
        self.cooldown_until = Some(now + Duration::seconds(seconds as i64));
    }

    pub fn clear_cooldown(&mut self) {
        self.consecutive_failures = 0;
        self.cooldown_until = None;
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSample {
    pub at: DateTime<Utc>,
    pub ok: bool,
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderStatus {
    Unknown,
    Checking,
    Healthy,
    Unhealthy,
}

/// How the gateway orders candidates for each incoming request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub enum RouteStrategy {
    /// Strict priority order, lowest number first.
    #[default]
    Priority,
    /// Lowest measured probe latency first.
    Fastest,
    /// Spread requests across candidates of the same priority tier.
    RoundRobin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInput {
    pub id: Option<String>,
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_override: Option<String>,
    pub enabled: bool,
    pub priority: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub gateway_port: u16,
    /// Total budget for health probes and key verification.
    pub request_timeout_seconds: u64,
    /// TCP connect budget for every upstream call.
    pub connect_timeout_seconds: u64,
    /// Maximum gap between two chunks of a proxied (possibly streaming) response.
    pub stream_idle_timeout_seconds: u64,
    pub health_interval_minutes: u64,
    pub automatic_health_checks: bool,
    pub auto_start_gateway: bool,
    /// Reuse pooled TCP connections and send TCP keepalive probes to upstreams.
    pub upstream_keep_alive: bool,
    pub route_strategy: RouteStrategy,
    /// Upper bound on providers tried per request. 0 means "try every candidate".
    pub max_failover_attempts: u32,
    /// Base cooldown applied after repeated upstream failures, doubling per failure.
    pub cooldown_seconds: u64,
    /// Treat 401/402/403/404 as a reason to switch upstream, not just 429/5xx.
    pub failover_on_client_errors: bool,
    /// Rewrite the request `model` when the target upstream does not expose it.
    pub remap_model: bool,
    /// Keep providers that failed their last probe out of the primary rotation.
    pub skip_unhealthy: bool,
    pub local_access_key: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            gateway_port: 1455,
            request_timeout_seconds: 30,
            connect_timeout_seconds: 10,
            stream_idle_timeout_seconds: 120,
            health_interval_minutes: 5,
            automatic_health_checks: true,
            auto_start_gateway: true,
            upstream_keep_alive: true,
            route_strategy: RouteStrategy::Priority,
            max_failover_attempts: 3,
            cooldown_seconds: 60,
            failover_on_client_errors: true,
            remap_model: true,
            skip_unhealthy: true,
            local_access_key: format!("rd_local_{}", uuid::Uuid::new_v4().simple()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub providers: Vec<Provider>,
    pub settings: AppSettings,
}

impl AppConfig {
    /// Providers that could serve traffic at all, cheapest checks first.
    pub fn routable(&self) -> impl Iterator<Item = &Provider> {
        self.providers.iter().filter(|provider| provider.enabled && provider.api_key.is_some())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub host: String,
    pub port: u16,
    pub request_count: u64,
    pub success_count: u64,
    pub failed_count: u64,
    pub failover_count: u64,
    pub active_connections: u64,
    pub average_latency_ms: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub uptime_seconds: u64,
    pub strategy: RouteStrategy,
    pub routable_count: usize,
    pub cooldown_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestLog {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub method: String,
    pub path: String,
    pub provider_name: String,
    pub status_code: u16,
    pub latency_ms: u64,
    pub attempts: usize,
    #[serde(default)]
    pub requested_model: Option<String>,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub providers: Vec<Provider>,
    pub gateway: GatewayStatus,
    pub logs: Vec<RequestLog>,
    pub settings: AppSettings,
    pub codex: CodexStatus,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexStatus {
    pub configured: bool,
    pub config_path: String,
    pub active_model: Option<String>,
    pub active_provider: Option<String>,
    /// Port currently written into the Codex provider block.
    pub configured_port: Option<u16>,
    /// True when Codex points at RelayDeck but with a stale port, so a re-apply is needed.
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexApplyResult {
    pub config_path: String,
    pub backup_path: Option<String>,
    pub model: String,
    pub provider_name: String,
    /// Historical threads re-pointed at the relaydeck provider so Codex keeps listing them.
    pub aligned_sessions: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRestartResult {
    pub app_name: String,
    pub version: Option<String>,
    pub installed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexApplyAndRestartResult {
    pub apply: CodexApplyResult,
    pub restart: CodexRestartResult,
}

/// One Codex conversation, reconstructed from its rollout file plus `session_index.jsonl`.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexSession {
    pub id: String,
    pub title: String,
    pub path: String,
    pub cwd: Option<String>,
    pub originator: Option<String>,
    pub cli_version: Option<String>,
    pub model_provider: Option<String>,
    pub thread_source: Option<String>,
    pub agent_nickname: Option<String>,
    pub forked_from_id: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub size_bytes: u64,
    /// Listed in `session_index.jsonl`, which is what makes it resumable from Codex.
    pub indexed: bool,
    /// Moved into RelayDeck's archive folder, hidden from Codex but recoverable.
    pub archived: bool,
    /// Routed through the RelayDeck gateway.
    pub via_relaydeck: bool,
    /// Spawned by another thread rather than started by the user.
    pub subagent: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionOverview {
    pub sessions: Vec<CodexSession>,
    pub sessions_dir: String,
    pub archive_dir: String,
    pub total_bytes: u64,
    pub archived_bytes: u64,
    pub indexed_count: usize,
    /// Rollout files Codex cannot resume because they are missing from the index.
    pub orphan_count: usize,
    pub archived_count: usize,
    /// Index entries whose rollout file no longer exists.
    pub ghost_count: usize,
    /// The `model_provider` Codex is currently configured to use.
    pub active_provider: Option<String>,
    /// User threads whose provider differs from the active one; Codex Desktop hides them.
    pub provider_mismatch_count: usize,
    pub scanned_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionRepairResult {
    pub restored: usize,
    pub removed_ghosts: usize,
    pub backup_path: Option<String>,
    /// Threads whose `model_provider` was re-pointed at the active provider.
    pub aligned_threads: usize,
    pub db_backup_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{AppSettings, Provider, ProviderStatus, HEALTH_HISTORY_LIMIT};
    use chrono::Utc;

    fn provider() -> Provider {
        Provider {
            id: "id".into(), name: "N".into(), base_url: "https://x/v1".into(), api_key: Some("k".into()), has_key: true,
            model: "gpt-5.6-sol".into(), model_override: None, available_models: vec!["gpt-5.6-sol".into()], enabled: true,
            priority: 1, status: ProviderStatus::Healthy, latency_ms: Some(120), last_checked_at: None, last_error: None,
            consecutive_failures: 0, cooldown_until: None, served_count: 0, failed_count: 0, health_history: Vec::new(),
        }
    }

    #[test]
    fn manual_pin_wins_over_discovered_model() {
        let mut provider = provider();
        assert_eq!(provider.effective_model(), "gpt-5.6-sol");
        provider.model_override = Some("  ".into());
        assert_eq!(provider.effective_model(), "gpt-5.6-sol");
        provider.model_override = Some("gpt-5.4".into());
        assert_eq!(provider.effective_model(), "gpt-5.4");
    }

    #[test]
    fn first_failure_does_not_trigger_cooldown() {
        let mut provider = provider();
        let now = Utc::now();
        provider.consecutive_failures = 1;
        provider.apply_cooldown(60, now);
        assert!(!provider.in_cooldown(now));
    }

    #[test]
    fn cooldown_backs_off_and_is_capped() {
        let now = Utc::now();
        let mut provider = provider();
        provider.consecutive_failures = 2;
        provider.apply_cooldown(60, now);
        let first = provider.cooldown_until.unwrap() - now;
        assert_eq!(first.num_seconds(), 60);

        provider.consecutive_failures = 4;
        provider.apply_cooldown(60, now);
        assert_eq!((provider.cooldown_until.unwrap() - now).num_seconds(), 240);

        provider.consecutive_failures = 99;
        provider.apply_cooldown(60, now);
        assert_eq!((provider.cooldown_until.unwrap() - now).num_seconds(), 900);

        provider.clear_cooldown();
        assert!(!provider.in_cooldown(now));
        assert_eq!(provider.consecutive_failures, 0);
    }

    #[test]
    fn health_history_stays_bounded() {
        let mut provider = provider();
        for _ in 0..HEALTH_HISTORY_LIMIT + 8 {
            provider.record_health(true, Some(10), Utc::now());
        }
        assert_eq!(provider.health_history.len(), HEALTH_HISTORY_LIMIT);
    }

    #[test]
    fn defaults_keep_streams_alive_longer_than_probes() {
        let settings = AppSettings::default();
        assert!(settings.stream_idle_timeout_seconds > settings.request_timeout_seconds);
        assert!(settings.upstream_keep_alive);
    }
}
