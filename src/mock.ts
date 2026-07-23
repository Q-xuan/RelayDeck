import type { AppSnapshot, Provider, ProviderInput } from "./types";

const STORAGE_KEY = "relaydeck-local-state-v2";

function initialState(): AppSnapshot {
  return {
    providers: [],
    gateway: {
      running: false, host: "127.0.0.1", port: 1455, requestCount: 0, successCount: 0, failedCount: 0,
      failoverCount: 0, activeConnections: 0, averageLatencyMs: 0, inputBytes: 0, outputBytes: 0, uptimeSeconds: 0,
    },
    logs: [],
    settings: {
      gatewayPort: 1455, requestTimeoutSeconds: 90, healthIntervalMinutes: 5, automaticHealthChecks: true,
      autoStartGateway: true, localAccessKey: `rd_local_${crypto.randomUUID().replaceAll("-", "")}`,
    },
    codex: { configured: false, configPath: "~/.codex/config.toml" },
  };
}

export function getMockState(): AppSnapshot {
  localStorage.removeItem("relaydeck-demo-state-v1");
  const saved = localStorage.getItem(STORAGE_KEY);
  const defaults = initialState();
  const state = saved ? JSON.parse(saved) as AppSnapshot : defaults;
  state.gateway = { ...defaults.gateway, ...state.gateway };
  state.settings = { ...defaults.settings, ...state.settings };
  state.codex = state.codex || defaults.codex;
  state.providers = state.providers.map((provider) => ({
    ...provider,
    availableModels: provider.availableModels || (provider.model ? [provider.model] : []),
  }));
  return state;
}

function persist(state: AppSnapshot): AppSnapshot {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  return state;
}

export async function mockInvoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  await new Promise((resolve) => setTimeout(resolve, command === "test_provider" ? 650 : 90));
  const state = getMockState();

  if (command === "get_snapshot") return state as T;
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
      availableModels: current?.availableModels || [],
      enabled: input.enabled,
      priority: input.priority,
      status: current?.status || "unknown",
      latencyMs: current?.latencyMs,
      lastCheckedAt: current?.lastCheckedAt,
      lastError: current?.lastError,
    };
    state.providers = current ? state.providers.map((item) => item.id === provider.id ? provider : item) : [...state.providers, provider];
    persist(state);
    return provider as T;
  }
  if (command === "delete_provider") {
    state.providers = state.providers.filter((item) => item.id !== args.id);
    persist(state);
    return undefined as T;
  }
  if (command === "toggle_provider") {
    state.providers = state.providers.map((item) => item.id === args.id ? { ...item, enabled: args.enabled as boolean } : item);
    persist(state);
    return undefined as T;
  }
  if (command === "test_provider") {
    const provider = state.providers.find((item) => item.id === args.id)!;
    const healthy = !provider.baseUrl.includes("invalid");
    const availableModels = healthy ? ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.4"] : [];
    const updated: Provider = {
      ...provider,
      status: healthy ? "healthy" : "unhealthy",
      latencyMs: healthy ? 420 + Math.floor(Math.random() * 760) : undefined,
      lastCheckedAt: new Date().toISOString(),
      lastError: healthy ? undefined : "连接超时",
      model: healthy ? availableModels[0] : provider.model,
      availableModels,
    };
    state.providers = state.providers.map((item) => item.id === updated.id ? updated : item);
    persist(state);
    return updated as T;
  }
  if (command === "start_gateway" || command === "stop_gateway") {
    state.gateway.running = command === "start_gateway";
    persist(state);
    return state.gateway as T;
  }
  if (command === "save_settings") {
    state.settings = args.settings as AppSnapshot["settings"];
    state.gateway.port = state.settings.gatewayPort;
    persist(state);
    return undefined as T;
  }
  if (command === "reset_access_key") {
    state.settings.localAccessKey = `rd_local_${crypto.randomUUID().replaceAll("-", "")}`;
    persist(state);
    return state.settings as T;
  }
  if (command === "apply_codex_config") {
    state.codex = { configured: true, configPath: "~/.codex/config.toml", activeModel: state.providers.find((provider) => provider.enabled)?.model || "自动获取中", activeProvider: "relaydeck" };
    persist(state);
    return { configPath: state.codex.configPath, model: state.codex.activeModel } as T;
  }
  if (command === "restart_codex") return undefined as T;
  if (command === "import_providers") {
    const inputs = args.providers as ProviderInput[];
    for (const input of inputs) {
      state.providers.push({
        id: crypto.randomUUID(), name: input.name, baseUrl: input.baseUrl, hasKey: Boolean(input.apiKey), model: "gpt-5.6-sol",
        availableModels: ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.4"],
        enabled: input.enabled, priority: input.priority, status: "healthy", latencyMs: 520 + Math.floor(Math.random() * 420), lastCheckedAt: new Date().toISOString(),
      });
    }
    persist(state);
    return state.providers as T;
  }
  if (command === "clear_logs") {
    state.logs = [];
    persist(state);
    return undefined as T;
  }
  throw new Error(`Unknown mock command: ${command}`);
}
