import type {
  AppSnapshot, CodexSession, HealthSample, Provider, ProviderInput, SessionOverview, SessionRepairResult,
} from "./types";

const STORAGE_KEY = "relaydeck-local-state-v2";
const SESSION_KEY = "relaydeck-local-sessions-v1";
const DEMO_MODELS = ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.4"];
const SESSIONS_DIR = "C:\\Users\\demo\\.codex\\sessions";
const ARCHIVE_DIR = "C:\\Users\\demo\\.codex\\sessions-archive";

function accessKey(): string {
  return `rd_local_${crypto.randomUUID().replaceAll("-", "")}`;
}

function initialState(): AppSnapshot {
  return {
    providers: [],
    gateway: {
      running: false, host: "127.0.0.1", port: 1455, requestCount: 0, successCount: 0, failedCount: 0,
      failoverCount: 0, activeConnections: 0, averageLatencyMs: 0, inputBytes: 0, outputBytes: 0, uptimeSeconds: 0,
      strategy: "priority", routableCount: 0, cooldownCount: 0,
    },
    logs: [],
    settings: {
      gatewayPort: 1455, requestTimeoutSeconds: 30, connectTimeoutSeconds: 10, streamIdleTimeoutSeconds: 120,
      healthIntervalMinutes: 5, automaticHealthChecks: true, autoStartGateway: true, upstreamKeepAlive: true,
      routeStrategy: "priority", maxFailoverAttempts: 3, cooldownSeconds: 60, failoverOnClientErrors: true,
      remapModel: true, skipUnhealthy: true, localAccessKey: accessKey(),
    },
    codex: { configured: false, configPath: "~/.codex/config.toml", stale: false },
  };
}

function normalizeProvider(provider: Provider): Provider {
  return {
    ...provider,
    availableModels: provider.availableModels || (provider.model ? [provider.model] : []),
    consecutiveFailures: provider.consecutiveFailures ?? 0,
    servedCount: provider.servedCount ?? 0,
    failedCount: provider.failedCount ?? 0,
    healthHistory: provider.healthHistory ?? [],
  };
}

export function getMockState(): AppSnapshot {
  localStorage.removeItem("relaydeck-demo-state-v1");
  const saved = localStorage.getItem(STORAGE_KEY);
  const defaults = initialState();
  const state = saved ? JSON.parse(saved) as AppSnapshot : defaults;
  state.gateway = { ...defaults.gateway, ...state.gateway };
  state.settings = { ...defaults.settings, ...state.settings };
  state.codex = { ...defaults.codex, ...state.codex };
  state.providers = (state.providers ?? []).map(normalizeProvider);
  return state;
}

function persist(state: AppSnapshot): AppSnapshot {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  return state;
}

/** Mirrors the derived fields the Rust backend computes on every snapshot. */
function derive(state: AppSnapshot): AppSnapshot {
  const now = Date.now();
  const cooling = (provider: Provider) => Boolean(provider.cooldownUntil && new Date(provider.cooldownUntil).getTime() > now);
  state.gateway.strategy = state.settings.routeStrategy;
  state.gateway.port = state.settings.gatewayPort;
  state.gateway.routableCount = state.providers.filter((provider) => provider.enabled && provider.hasKey && !cooling(provider)).length;
  state.gateway.cooldownCount = state.providers.filter(cooling).length;
  state.codex.stale = state.codex.configured && state.codex.configuredPort !== state.gateway.port;
  return state;
}

function record(provider: Provider, ok: boolean, latencyMs?: number): Provider {
  const sample: HealthSample = { at: new Date().toISOString(), ok, latencyMs };
  const healthHistory = [...(provider.healthHistory ?? []), sample].slice(-20);
  const consecutiveFailures = ok ? 0 : provider.consecutiveFailures + 1;
  const cooldownSeconds = consecutiveFailures >= 2 ? Math.min(60 * 2 ** (consecutiveFailures - 2), 900) : 0;
  return {
    ...provider,
    status: ok ? "healthy" : "unhealthy",
    latencyMs: ok ? latencyMs : undefined,
    lastCheckedAt: sample.at,
    lastError: ok ? undefined : "连接超时（浏览器预览为模拟数据）",
    consecutiveFailures,
    cooldownUntil: cooldownSeconds ? new Date(Date.now() + cooldownSeconds * 1000).toISOString() : undefined,
    healthHistory,
  };
}

function probe(provider: Provider): Provider {
  const ok = !provider.baseUrl.includes("invalid");
  const updated = record(provider, ok, ok ? 420 + Math.floor(Math.random() * 760) : undefined);
  if (!ok) return updated;
  const availableModels = provider.availableModels.length ? provider.availableModels : DEMO_MODELS;
  return { ...updated, availableModels, model: availableModels.includes(provider.model) ? provider.model : availableModels[0] };
}

/* --------------------------------------------------------- codex sessions */

function seedSessions(): CodexSession[] {
  const hoursAgo = (hours: number) => new Date(Date.now() - hours * 3600_000).toISOString();
  const base = (index: number, extra: Partial<CodexSession>): CodexSession => ({
    id: `019f9ee0-0000-7000-8000-00000000000${index}`,
    title: "示例会话",
    path: `${SESSIONS_DIR}\\2026\\07\\26\\rollout-2026-07-26T09-0${index}-00-019f9ee0-0000-7000-8000-00000000000${index}.jsonl`,
    cwd: "E:\\code\\RelayDeck",
    sizeBytes: 240_000,
    indexed: true, archived: false, viaRelaydeck: false, subagent: false,
    createdAt: hoursAgo(index * 6 + 1), updatedAt: hoursAgo(index * 6),
    ...extra,
  });
  return [
    base(1, { title: "优化中转站保活与路由策略", viaRelaydeck: true, modelProvider: "relaydeck", cliVersion: "0.56.0", sizeBytes: 4_820_000 }),
    base(2, { title: "给项目加上 Codex 会话管理", viaRelaydeck: true, modelProvider: "relaydeck", cliVersion: "0.56.0", sizeBytes: 1_260_000 }),
    base(3, { title: "修复导入解析里的空行问题", indexed: false, modelProvider: "openai", cliVersion: "0.55.0", sizeBytes: 380_000 }),
    base(4, { title: "The following is the Codex agent history…", indexed: false, subagent: true, threadSource: "subagent", agentNickname: "reviewer", sizeBytes: 96_000 }),
    base(5, { title: "早期的打包脚本调试", indexed: false, archived: true, modelProvider: "openai", sizeBytes: 720_000,
      path: `${ARCHIVE_DIR}\\2026\\07\\20\\rollout-2026-07-20T08-00-00-019f9ee0-0000-7000-8000-000000000005.jsonl` }),
  ];
}

function getSessions(): CodexSession[] {
  const saved = localStorage.getItem(SESSION_KEY);
  if (saved) return JSON.parse(saved) as CodexSession[];
  const seeded = seedSessions();
  localStorage.setItem(SESSION_KEY, JSON.stringify(seeded));
  return seeded;
}

function persistSessions(list: CodexSession[]): CodexSession[] {
  localStorage.setItem(SESSION_KEY, JSON.stringify(list));
  return list;
}

function sessionOverview(list: CodexSession[]): SessionOverview {
  const sorted = [...list].sort((a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""));
  return {
    sessions: sorted,
    sessionsDir: SESSIONS_DIR,
    archiveDir: ARCHIVE_DIR,
    totalBytes: sorted.reduce((sum, session) => sum + session.sizeBytes, 0),
    archivedBytes: sorted.filter((session) => session.archived).reduce((sum, session) => sum + session.sizeBytes, 0),
    indexedCount: sorted.filter((session) => session.indexed && !session.archived).length,
    orphanCount: sorted.filter((session) => !session.indexed && !session.archived && !session.subagent).length,
    archivedCount: sorted.filter((session) => session.archived).length,
    // The browser preview has no real index file, so there is nothing to go stale.
    ghostCount: 0,
    activeProvider: "relaydeck",
    // The preview has no real thread database either, so nothing can be mis-attributed.
    providerMismatchCount: 0,
    scannedAt: new Date().toISOString(),
  };
}

export async function mockInvoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  await new Promise((resolve) => setTimeout(resolve, command === "test_provider" ? 650 : 90));
  const state = getMockState();
  const find = () => state.providers.find((item) => item.id === args.id);
  const replace = (updated: Provider) => {
    state.providers = state.providers.map((item) => item.id === updated.id ? updated : item);
  };

  if (command === "get_snapshot") return derive(state) as T;
  if (command === "save_provider") {
    const input = args.input as ProviderInput;
    const current = state.providers.find((item) => item.id === input.id);
    const provider: Provider = {
      id: input.id || crypto.randomUUID(),
      name: input.name,
      baseUrl: input.baseUrl.replace(/\/$/, ""),
      apiKey: undefined,
      hasKey: Boolean(input.apiKey || current?.hasKey),
      model: input.model || current?.model || "自动获取中",
      modelOverride: input.modelOverride,
      availableModels: current?.availableModels || [],
      enabled: input.enabled,
      priority: input.priority,
      status: current?.status || "unknown",
      latencyMs: current?.latencyMs,
      lastCheckedAt: current?.lastCheckedAt,
      lastError: current?.lastError,
      consecutiveFailures: current?.consecutiveFailures ?? 0,
      cooldownUntil: current?.cooldownUntil,
      servedCount: current?.servedCount ?? 0,
      failedCount: current?.failedCount ?? 0,
      healthHistory: current?.healthHistory ?? [],
    };
    const probed = probe(provider);
    state.providers = current ? state.providers.map((item) => item.id === probed.id ? probed : item) : [...state.providers, probed];
    persist(derive(state));
    return probed as T;
  }
  if (command === "delete_provider") {
    state.providers = state.providers.filter((item) => item.id !== args.id);
    persist(derive(state));
    return undefined as T;
  }
  if (command === "toggle_provider") {
    state.providers = state.providers.map((item) => item.id === args.id ? { ...item, enabled: args.enabled as boolean } : item);
    persist(derive(state));
    return undefined as T;
  }
  if (command === "test_provider") {
    const updated = probe(find()!);
    replace(updated);
    persist(derive(state));
    return updated as T;
  }
  if (command === "refresh_health") {
    state.providers = state.providers.map((provider) => provider.enabled && provider.hasKey ? probe(provider) : provider);
    persist(derive(state));
    return state.providers as T;
  }
  if (command === "set_provider_model") {
    const provider = find()!;
    const updated: Provider = { ...provider, modelOverride: (args.model as string | null) ?? undefined };
    replace(updated);
    persist(derive(state));
    return updated as T;
  }
  if (command === "move_provider") {
    const offset = args.offset as number;
    const ordered = [...state.providers].sort((a, b) => a.priority - b.priority);
    const index = ordered.findIndex((item) => item.id === args.id);
    const target = index + offset;
    if (index >= 0 && target >= 0 && target < ordered.length) {
      const [moved] = ordered.splice(index, 1);
      ordered.splice(target, 0, moved);
    }
    state.providers = ordered.map((provider, position) => ({ ...provider, priority: position + 1 }));
    persist(derive(state));
    return state.providers as T;
  }
  if (command === "clear_provider_cooldown") {
    const provider = { ...find()!, consecutiveFailures: 0, cooldownUntil: undefined };
    const updated = probe(provider);
    replace(updated);
    persist(derive(state));
    return updated as T;
  }
  if (command === "start_gateway" || command === "stop_gateway") {
    state.gateway.running = command === "start_gateway";
    persist(derive(state));
    return state.gateway as T;
  }
  if (command === "save_settings") {
    state.settings = args.settings as AppSnapshot["settings"];
    persist(derive(state));
    return state.settings as T;
  }
  if (command === "reset_access_key") {
    state.settings.localAccessKey = accessKey();
    persist(derive(state));
    return state.settings as T;
  }
  if (command === "apply_codex_config") {
    const provider = state.providers.find((item) => item.enabled && item.hasKey);
    const model = provider?.modelOverride || provider?.model || "自动获取中";
    state.codex = {
      configured: true, configPath: "~/.codex/config.toml", activeModel: model, activeProvider: "relaydeck",
      configuredPort: state.settings.gatewayPort, stale: false,
    };
    persist(derive(state));
    return { configPath: state.codex.configPath, model, providerName: provider?.name || "relaydeck", alignedSessions: 0 } as T;
  }
  if (command === "restart_codex") return { appName: "ChatGPT", version: "preview" } as T;
  if (command === "apply_and_restart_codex") {
    const provider = state.providers.find((item) => item.enabled && item.hasKey);
    const model = provider?.modelOverride || provider?.model || "自动获取中";
    state.codex = {
      configured: true, configPath: "~/.codex/config.toml", activeModel: model, activeProvider: "relaydeck",
      configuredPort: state.settings.gatewayPort, stale: false,
    };
    persist(derive(state));
    return {
      apply: { configPath: state.codex.configPath, model, providerName: provider?.name || "relaydeck", alignedSessions: 0 },
      restart: { appName: "ChatGPT", version: "preview" },
    } as T;
  }
  if (command === "import_providers") {
    const inputs = args.providers as ProviderInput[];
    for (const input of inputs) {
      state.providers.push(probe({
        id: crypto.randomUUID(), name: input.name, baseUrl: input.baseUrl, hasKey: Boolean(input.apiKey),
        model: DEMO_MODELS[0], availableModels: [], enabled: input.enabled, priority: input.priority,
        status: "unknown", consecutiveFailures: 0, servedCount: 0, failedCount: 0, healthHistory: [],
      }));
    }
    persist(derive(state));
    return state.providers as T;
  }
  if (command === "clear_logs") {
    state.logs = [];
    state.gateway = { ...state.gateway, requestCount: 0, successCount: 0, failedCount: 0, failoverCount: 0, averageLatencyMs: 0, inputBytes: 0, outputBytes: 0 };
    state.providers = state.providers.map((provider) => ({ ...provider, servedCount: 0, failedCount: 0 }));
    persist(derive(state));
    return undefined as T;
  }
  if (command === "list_codex_sessions") return sessionOverview(getSessions()) as T;
  if (command === "repair_session_visibility") {
    const list = getSessions();
    const restored = list.filter((session) => !session.indexed && !session.archived && !session.subagent).length;
    persistSessions(list.map((session) => session.archived || session.subagent ? session : { ...session, indexed: true }));
    return { restored, removedGhosts: 0, alignedThreads: 0 } as SessionRepairResult as T;
  }
  if (command === "archive_codex_session" || command === "restore_codex_session") {
    const archived = command === "archive_codex_session";
    const list = getSessions();
    const session = list.find((item) => item.id === args.id);
    if (!session) throw new Error("找不到这个会话");
    if (session.archived === archived) throw new Error(archived ? "会话已经归档" : "会话没有归档，无需恢复");
    const updated: CodexSession = {
      ...session,
      archived, indexed: !archived,
      path: archived ? session.path.replace(SESSIONS_DIR, ARCHIVE_DIR) : session.path.replace(ARCHIVE_DIR, SESSIONS_DIR),
    };
    persistSessions(list.map((item) => item.id === updated.id ? updated : item));
    return updated as T;
  }
  if (command === "delete_codex_session") {
    const list = getSessions();
    const session = list.find((item) => item.id === args.id);
    if (!session) throw new Error("找不到这个会话");
    if (!session.archived) throw new Error("只能删除已归档的会话，请先归档");
    persistSessions(list.filter((item) => item.id !== session.id));
    return session.sizeBytes as T;
  }
  if (command === "reveal_codex_session") {
    throw new Error("浏览器预览不能打开本机文件夹");
  }
  throw new Error(`Unknown mock command: ${command}`);
}
