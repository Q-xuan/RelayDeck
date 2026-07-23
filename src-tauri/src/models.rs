use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
    #[serde(default)]
    pub available_models: Vec<String>,
    pub enabled: bool,
    pub priority: u32,
    pub status: ProviderStatus,
    pub latency_ms: Option<u64>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderStatus {
    Unknown,
    Checking,
    Healthy,
    Unhealthy,
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
    pub enabled: bool,
    pub priority: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub gateway_port: u16,
    pub request_timeout_seconds: u64,
    pub health_interval_minutes: u64,
    pub automatic_health_checks: bool,
    pub auto_start_gateway: bool,
    pub local_access_key: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            gateway_port: 1455,
            request_timeout_seconds: 90,
            health_interval_minutes: 5,
            automatic_health_checks: true,
            auto_start_gateway: true,
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
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexApplyResult {
    pub config_path: String,
    pub backup_path: Option<String>,
    pub model: String,
}
