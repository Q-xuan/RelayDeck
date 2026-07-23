export type ProviderStatus = "unknown" | "checking" | "healthy" | "unhealthy";

export interface Provider {
  id: string;
  name: string;
  baseUrl: string;
  apiKey?: string;
  hasKey: boolean;
  model: string;
  availableModels: string[];
  enabled: boolean;
  priority: number;
  status: ProviderStatus;
  latencyMs?: number;
  lastCheckedAt?: string;
  lastError?: string;
}

export interface GatewayStatus {
  running: boolean;
  host: string;
  port: number;
  requestCount: number;
  successCount: number;
  failedCount: number;
  failoverCount: number;
  activeConnections: number;
  averageLatencyMs: number;
  inputBytes: number;
  outputBytes: number;
  uptimeSeconds: number;
}

export interface RequestLog {
  id: string;
  timestamp: string;
  method: string;
  path: string;
  providerName: string;
  statusCode: number;
  latencyMs: number;
  attempts: number;
}

export interface AppSettings {
  gatewayPort: number;
  requestTimeoutSeconds: number;
  healthIntervalMinutes: number;
  automaticHealthChecks: boolean;
  autoStartGateway: boolean;
  localAccessKey: string;
}

export interface AppSnapshot {
  providers: Provider[];
  gateway: GatewayStatus;
  logs: RequestLog[];
  settings: AppSettings;
  codex: CodexStatus;
}

export interface CodexStatus {
  configured: boolean;
  configPath: string;
  activeModel?: string;
  activeProvider?: string;
}

export interface CodexApplyResult {
  configPath: string;
  backupPath?: string;
  model: string;
}

export interface ProviderInput {
  id?: string;
  name: string;
  baseUrl: string;
  apiKey?: string;
  model?: string;
  enabled: boolean;
  priority: number;
}

export interface ImportPayload {
  providers: ProviderInput[];
}
