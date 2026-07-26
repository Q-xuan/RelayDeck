export type ProviderStatus = "unknown" | "checking" | "healthy" | "unhealthy";

export type RouteStrategy = "priority" | "fastest" | "roundRobin";

export interface HealthSample {
  at: string;
  ok: boolean;
  latencyMs?: number;
}

export interface Provider {
  id: string;
  name: string;
  baseUrl: string;
  apiKey?: string;
  hasKey: boolean;
  model: string;
  modelOverride?: string;
  availableModels: string[];
  enabled: boolean;
  priority: number;
  status: ProviderStatus;
  latencyMs?: number;
  lastCheckedAt?: string;
  lastError?: string;
  consecutiveFailures: number;
  cooldownUntil?: string;
  servedCount: number;
  failedCount: number;
  healthHistory: HealthSample[];
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
  strategy: RouteStrategy;
  routableCount: number;
  cooldownCount: number;
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
  requestedModel?: string;
  upstreamModel?: string;
  error?: string;
}

export interface AppSettings {
  gatewayPort: number;
  requestTimeoutSeconds: number;
  connectTimeoutSeconds: number;
  streamIdleTimeoutSeconds: number;
  healthIntervalMinutes: number;
  automaticHealthChecks: boolean;
  autoStartGateway: boolean;
  upstreamKeepAlive: boolean;
  routeStrategy: RouteStrategy;
  maxFailoverAttempts: number;
  cooldownSeconds: number;
  failoverOnClientErrors: boolean;
  remapModel: boolean;
  skipUnhealthy: boolean;
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
  configuredPort?: number;
  stale: boolean;
}

export interface CodexApplyResult {
  configPath: string;
  backupPath?: string;
  model: string;
  providerName: string;
  alignedSessions: number;
}

export interface CodexRestartResult {
  appName: string;
  version?: string;
  installedAt?: string;
}

export interface CodexApplyAndRestartResult {
  apply: CodexApplyResult;
  restart: CodexRestartResult;
}

export interface CodexSession {
  id: string;
  title: string;
  path: string;
  cwd?: string;
  originator?: string;
  cliVersion?: string;
  modelProvider?: string;
  threadSource?: string;
  agentNickname?: string;
  forkedFromId?: string;
  createdAt?: string;
  updatedAt?: string;
  sizeBytes: number;
  indexed: boolean;
  archived: boolean;
  viaRelaydeck: boolean;
  subagent: boolean;
}

export interface SessionOverview {
  sessions: CodexSession[];
  sessionsDir: string;
  archiveDir: string;
  totalBytes: number;
  archivedBytes: number;
  indexedCount: number;
  orphanCount: number;
  archivedCount: number;
  ghostCount: number;
  activeProvider?: string;
  providerMismatchCount: number;
  scannedAt: string;
}

export interface SessionRepairResult {
  restored: number;
  removedGhosts: number;
  backupPath?: string;
  alignedThreads: number;
  dbBackupPath?: string;
}

export interface ProviderInput {
  id?: string;
  name: string;
  baseUrl: string;
  apiKey?: string;
  model?: string;
  modelOverride?: string;
  enabled: boolean;
  priority: number;
}

export interface ImportPayload {
  providers: ProviderInput[];
}
