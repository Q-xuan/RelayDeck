import {
  Activity, Archive, ArchiveRestore, ArrowDown, ArrowDownUp, ArrowUp, Bot, Check, Copy, Database, Eye, EyeOff,
  FolderOpen, Gauge, HardDrive, HeartPulse, Import, LayoutDashboard, ListFilter, ListRestart, MessagesSquare,
  Moon, MoreHorizontal, Pencil, Pin, PinOff, Play, Plus, Power, Rabbit, RefreshCw, Repeat, RotateCcw, Route,
  Save, Scale, Search, Server, Settings, ShieldCheck, SlidersHorizontal, Snowflake, Square, Sun, Timer,
  TriangleAlert, Trash2, Upload, Wifi, Wrench, X, Zap,
  createIcons,
} from "lucide";
import "./styles.css";
import { call, isDesktop } from "./api";
import type {
  AppSettings, AppSnapshot, CodexApplyAndRestartResult, CodexApplyResult, CodexSession, HealthSample, Provider,
  ProviderInput, ProviderStatus, RequestLog, RouteStrategy, SessionOverview, SessionRepairResult,
} from "./types";

type View = "overview" | "providers" | "sessions" | "activity" | "settings";
type Theme = "light" | "dark";
type StatusFilter = "all" | "healthy" | "unhealthy" | "cooldown";
type ProviderSort = "priority" | "latency" | "name" | "traffic";
type SessionFilter = "all" | "resumable" | "orphan" | "archived" | "subagent";
type SessionSort = "recent" | "size" | "name";

const POLL_INTERVAL_MS = 2000;
const THEME_KEY = "relaydeck-theme";

const app = document.querySelector<HTMLDivElement>("#app")!;
let snapshot: AppSnapshot;
let activeView: View = "overview";
let busy = false;
let revealAccessKey = false;
let theme: Theme = localStorage.getItem(THEME_KEY) === "dark" ? "dark" : "light";
const filters = { query: "", status: "all" as StatusFilter, sort: "priority" as ProviderSort };
/** Scanning `~/.codex/sessions` is expensive, so it is fetched on demand instead of in the poll loop. */
let sessions: SessionOverview | null = null;
let sessionsLoading = false;
const sessionFilters = { query: "", kind: "all" as SessionFilter, sort: "recent" as SessionSort };

const iconSet = {
  Activity, Archive, ArchiveRestore, ArrowDown, ArrowDownUp, ArrowUp, Bot, Check, Copy, Database, Eye, EyeOff,
  FolderOpen, Gauge, HardDrive, HeartPulse, Import, LayoutDashboard, ListFilter, ListRestart, MessagesSquare,
  Moon, MoreHorizontal, Pencil, Pin, PinOff, Play, Plus, Power, Rabbit, RefreshCw, Repeat, RotateCcw, Route,
  Save, Scale, Search, Server, Settings, ShieldCheck, SlidersHorizontal, Snowflake, Square, Sun, Timer,
  TriangleAlert, Trash2, Upload, Wifi, Wrench, X, Zap,
};

const STRATEGIES: { value: RouteStrategy; label: string; icon: string; hint: string }[] = [
  { value: "priority", label: "优先级", icon: "route", hint: "按 P 序号从上到下，故障时向下切换" },
  { value: "fastest", label: "最快", icon: "rabbit", hint: "优先使用测活延迟最低的节点" },
  { value: "roundRobin", label: "轮询", icon: "repeat", hint: "在可用节点之间轮流分配请求" },
];

const escapeHtml = (value: unknown) => String(value ?? "")
  .replaceAll("&", "&amp;")
  .replaceAll("<", "&lt;")
  .replaceAll(">", "&gt;")
  .replaceAll('"', "&quot;")
  .replaceAll("'", "&#039;");

const initials = (name: string) => name.trim().split(/\s+/).map((part) => part[0]).join("").slice(0, 2).toUpperCase();
const byPriority = (a: Provider, b: Provider) => a.priority - b.priority;
const enabledProviders = () => snapshot.providers.filter((provider) => provider.enabled).sort(byPriority);
const healthyProviders = () => snapshot.providers.filter((provider) => provider.enabled && provider.status === "healthy");
const codexReady = () => snapshot.providers.some((provider) => provider.enabled && provider.model !== "自动获取中");
const successRate = () => snapshot.gateway.requestCount ? (snapshot.gateway.successCount / snapshot.gateway.requestCount * 100).toFixed(1) : "0.0";
const inCooldown = (provider: Provider) => Boolean(provider.cooldownUntil && new Date(provider.cooldownUntil).getTime() > Date.now());

const formatCount = (value: number) => value.toLocaleString("zh-Hans-CN");
const formatBytes = (bytes: number) => {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
};
const formatDuration = (seconds: number) => {
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟`;
  return `${Math.floor(seconds / 3600)} 小时 ${Math.floor((seconds % 3600) / 60)} 分`;
};
const relativeTime = (iso?: string) => {
  if (!iso) return "尚未检测";
  const seconds = Math.max(0, Math.floor((Date.now() - new Date(iso).getTime()) / 1000));
  if (seconds < 60) return `${seconds} 秒前`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  return `${Math.floor(seconds / 3600)} 小时前`;
};
const cooldownCopy = (iso?: string) => {
  if (!iso) return "";
  const seconds = Math.max(0, Math.round((new Date(iso).getTime() - Date.now()) / 1000));
  if (!seconds) return "即将重试";
  return seconds < 60 ? `${seconds} 秒后重试` : `${Math.ceil(seconds / 60)} 分钟后重试`;
};

function refreshIcons(): void {
  createIcons({ icons: iconSet });
}

function statusLabel(status: ProviderStatus): string {
  return { healthy: "可用", unhealthy: "异常", checking: "检测中", unknown: "未检测" }[status];
}

function strategyLabel(strategy: RouteStrategy): string {
  return STRATEGIES.find((item) => item.value === strategy)?.label ?? "优先级";
}

function toast(message: string, kind: "success" | "error" = "success"): void {
  let stack = document.querySelector<HTMLDivElement>(".toast-stack");
  if (!stack) {
    stack = document.createElement("div");
    stack.className = "toast-stack";
    document.body.append(stack);
  }
  const item = document.createElement("div");
  item.className = `toast ${kind === "error" ? "error" : ""}`;
  item.innerHTML = `<i data-lucide="${kind === "error" ? "triangle-alert" : "check"}"></i><span>${escapeHtml(message)}</span>`;
  item.title = "点击关闭";
  item.addEventListener("click", () => item.remove());
  stack.append(item);
  refreshIcons();
  window.setTimeout(() => item.remove(), kind === "error" ? 6000 : 2800);
}

/* ---------------------------------------------------------------- providers */

function healthTrend(provider: Provider): string {
  const samples: HealthSample[] = (provider.healthHistory ?? []).slice(-12);
  if (!samples.length) return `<span class="spark-empty">--</span>`;
  const peak = Math.max(...samples.map((sample) => sample.latencyMs ?? 0), 1);
  const failures = samples.filter((sample) => !sample.ok).length;
  const bars = samples.map((sample) => {
    const height = sample.ok ? Math.max(3, Math.round((sample.latencyMs ?? 0) / peak * 14)) : 14;
    return `<i class="${sample.ok ? "" : "bad"}" style="height:${height}px"></i>`;
  }).join("");
  return `<span class="spark" title="最近 ${samples.length} 次保活检查，${failures} 次失败">${bars}</span>`;
}

function providerStateBadge(provider: Provider): string {
  if (inCooldown(provider)) {
    return `<span class="status-badge cooldown" title="${escapeHtml(provider.lastError || "连续失败后暂时移出路由")}">冷却中</span>
      <div class="status-note" data-live-cooldown="${escapeHtml(provider.cooldownUntil)}">${cooldownCopy(provider.cooldownUntil)}</div>`;
  }
  return `<span class="status-badge ${provider.status}" title="${escapeHtml(provider.lastError || relativeTime(provider.lastCheckedAt))}">${statusLabel(provider.status)}</span>
    <div class="status-note"><span data-live-rel="${escapeHtml(provider.lastCheckedAt ?? "")}">${relativeTime(provider.lastCheckedAt)}</span></div>`;
}

function providerRows(providers: Provider[]): string {
  if (!providers.length) {
    const filtered = snapshot.providers.length > 0;
    return `
    <div class="empty">
      <div><div class="empty-icon"><i data-lucide="${filtered ? "search" : "server"}"></i></div>
      <div class="empty-title">${filtered ? "没有匹配的节点" : "还没有 Provider"}</div>
      <div class="empty-copy">${filtered ? "换个关键词，或清空筛选条件。" : "导入或添加一个中转后即可开始测活和路由。"}</div>
      <button class="button primary" data-action="${filtered ? "reset-filters" : "add-provider"}"><i data-lucide="${filtered ? "list-filter" : "plus"}"></i>${filtered ? "清空筛选" : "添加 Provider"}</button></div>
    </div>`;
  }

  return providers.map((provider, index) => `
    <div class="provider-row ${inCooldown(provider) ? "cooling" : ""} ${provider.enabled ? "" : "muted"}" data-provider-id="${escapeHtml(provider.id)}">
      <div class="provider-main">
        <div class="provider-avatar ${index % 3 === 1 ? "orange" : index % 3 === 2 ? "gray" : ""}">${escapeHtml(initials(provider.name))}</div>
        <div class="provider-copy">
          <div class="provider-name">${escapeHtml(provider.name)}</div>
          <div class="provider-url" title="${escapeHtml(provider.baseUrl)}">${escapeHtml(provider.baseUrl)}</div>
          <div class="provider-tags">
            <span>Responses</span>
            <span>${provider.availableModels.length || 0} 模型</span>
            ${provider.servedCount || provider.failedCount ? `<span title="本次运行成功 / 失败次数">${provider.servedCount}/${provider.failedCount}</span>` : ""}
          </div>
        </div>
      </div>
      <div class="model-cell" title="${escapeHtml(provider.availableModels.length ? provider.availableModels.join(", ") : provider.model)}">
        <span class="model-name">${escapeHtml(provider.modelOverride || provider.model)}</span>
        ${provider.modelOverride ? `<span class="model-pin" title="已手动锁定模型"><i data-lucide="pin"></i>锁定</span>` : ""}
      </div>
      <div class="status-cell">${providerStateBadge(provider)}</div>
      <div class="latency">${provider.latencyMs ? `${provider.latencyMs} ms` : "--"}</div>
      <div class="trend-cell">${healthTrend(provider)}</div>
      <div><span class="priority">P${provider.priority}</span></div>
      <div class="row-actions">
        <button class="switch ${provider.enabled ? "on" : ""}" data-action="toggle-provider" data-id="${escapeHtml(provider.id)}" data-enabled="${!provider.enabled}" title="${provider.enabled ? "停用" : "启用"}" aria-label="${provider.enabled ? "停用" : "启用"}" role="switch" aria-checked="${provider.enabled}"></button>
        <button class="mini-button" data-action="test-provider" data-id="${escapeHtml(provider.id)}" title="深度测活（会消耗极少量 token）" aria-label="深度测活"><i data-lucide="refresh-cw"></i></button>
        <button class="mini-button" data-action="provider-menu" data-id="${escapeHtml(provider.id)}" title="更多操作" aria-label="更多操作"><i data-lucide="more-horizontal"></i></button>
      </div>
    </div>`).join("");
}

function visibleProviders(): Provider[] {
  const query = filters.query.trim().toLowerCase();
  const matched = snapshot.providers.filter((provider) => {
    const haystack = `${provider.name} ${provider.baseUrl} ${provider.model} ${provider.availableModels.join(" ")}`.toLowerCase();
    if (query && !haystack.includes(query)) return false;
    if (filters.status === "healthy") return provider.status === "healthy" && !inCooldown(provider);
    if (filters.status === "unhealthy") return provider.status === "unhealthy";
    if (filters.status === "cooldown") return inCooldown(provider);
    return true;
  });
  const sorters: Record<ProviderSort, (a: Provider, b: Provider) => number> = {
    priority: byPriority,
    latency: (a, b) => (a.latencyMs ?? Number.MAX_SAFE_INTEGER) - (b.latencyMs ?? Number.MAX_SAFE_INTEGER),
    name: (a, b) => a.name.localeCompare(b.name, "zh-Hans-CN"),
    traffic: (a, b) => b.servedCount - a.servedCount,
  };
  return matched.sort(sorters[filters.sort]);
}

function providerToolbar(): string {
  const chips: { value: StatusFilter; label: string }[] = [
    { value: "all", label: `全部 ${snapshot.providers.length}` },
    { value: "healthy", label: `可用 ${snapshot.providers.filter((provider) => provider.status === "healthy" && !inCooldown(provider)).length}` },
    { value: "cooldown", label: `冷却 ${snapshot.providers.filter(inCooldown).length}` },
    { value: "unhealthy", label: `异常 ${snapshot.providers.filter((provider) => provider.status === "unhealthy").length}` },
  ];
  return `<div class="toolbar">
    <div class="search"><i data-lucide="search"></i><input class="search-input" id="provider-search" type="search" placeholder="搜索名称、地址或模型" value="${escapeHtml(filters.query)}" aria-label="搜索 Provider" /></div>
    <div class="chips" role="group" aria-label="状态筛选">${chips.map((chip) => `<button class="chip ${filters.status === chip.value ? "active" : ""}" data-action="set-status-filter" data-status="${chip.value}">${escapeHtml(chip.label)}</button>`).join("")}</div>
    <label class="sort-field"><span>排序</span><select class="select compact" id="provider-sort" aria-label="排序方式">
      <option value="priority" ${filters.sort === "priority" ? "selected" : ""}>路由顺序</option>
      <option value="latency" ${filters.sort === "latency" ? "selected" : ""}>延迟</option>
      <option value="name" ${filters.sort === "name" ? "selected" : ""}>名称</option>
      <option value="traffic" ${filters.sort === "traffic" ? "selected" : ""}>请求量</option>
    </select></label>
  </div>`;
}

function providerPanel(providers: Provider[], withToolbar = false): string {
  return `<section class="panel provider-list">
    <div class="panel-header">
      <div class="panel-title">Provider</div>
      <div class="panel-subtitle">${snapshot.gateway.routableCount} 个节点参与路由${snapshot.gateway.cooldownCount ? ` · ${snapshot.gateway.cooldownCount} 个冷却中` : ""}</div>
      <div class="panel-actions">
        <button class="button" data-action="refresh-health" ${busy || !snapshot.providers.length ? "disabled" : ""} title="只校验 /v1/models，不消耗 token"><i data-lucide="heart-pulse"></i>保活检查</button>
        <button class="button" data-action="test-all" ${busy || !snapshot.providers.length ? "disabled" : ""} title="深度测活，会消耗极少量 token"><i data-lucide="refresh-cw"></i>全部测活</button>
        <button class="button" data-action="open-import"><i data-lucide="import"></i>导入</button>
        <button class="button primary" data-action="add-provider"><i data-lucide="plus"></i>添加</button>
      </div>
    </div>
    ${withToolbar ? providerToolbar() : ""}
    ${providers.length ? `<div class="provider-head"><div>节点</div><div>模型</div><div>状态</div><div>延迟</div><div>趋势</div><div>路由</div><div></div></div>` : ""}
    ${providerRows(providers)}
  </section>`;
}

/* ------------------------------------------------------------------- panels */

function statsPanel(): string {
  const gateway = snapshot.gateway;
  return `<section class="panel">
    <div class="panel-header"><div class="panel-title">总量统计</div><div class="panel-subtitle">本次运行</div><div class="panel-actions"><button class="button" data-action="clear-logs" ${gateway.requestCount ? "" : "disabled"}><i data-lucide="trash-2"></i>清除统计</button></div></div>
    <div class="stats stats-six">
      <div class="stat"><div class="stat-label"><i data-lucide="activity"></i>总请求</div><div class="stat-value">${formatCount(gateway.requestCount)}</div><div class="stat-foot">成功 ${formatCount(gateway.successCount)} / 失败 ${formatCount(gateway.failedCount)}</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="shield-check"></i>成功率</div><div class="stat-value">${successRate()}<span class="stat-unit">%</span></div><div class="stat-foot">${gateway.failedCount ? `${gateway.failedCount} 次失败` : "暂无失败"}</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="route"></i>故障切换</div><div class="stat-value">${formatCount(gateway.failoverCount)}</div><div class="stat-foot">当前策略：${strategyLabel(gateway.strategy)}</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="gauge"></i>平均延迟</div><div class="stat-value">${gateway.averageLatencyMs || "--"}<span class="stat-unit">${gateway.averageLatencyMs ? "ms" : ""}</span></div><div class="stat-foot">请求首包响应</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="hard-drive"></i>代理流量</div><div class="stat-value compact">${formatBytes(gateway.outputBytes)}</div><div class="stat-foot">输入 ${formatBytes(gateway.inputBytes)}</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="timer"></i>运行时长</div><div class="stat-value compact" data-live-uptime>${formatDuration(gateway.uptimeSeconds)}</div><div class="stat-foot">${gateway.activeConnections} 个活动连接</div></div>
    </div>
  </section>`;
}

function strategyControl(): string {
  return `<div class="segmented" role="group" aria-label="路由策略">${STRATEGIES.map((item) => `
    <button class="seg ${snapshot.gateway.strategy === item.value ? "active" : ""}" data-action="set-strategy" data-strategy="${item.value}" title="${escapeHtml(item.hint)}" aria-pressed="${snapshot.gateway.strategy === item.value}"><i data-lucide="${item.icon}"></i>${item.label}</button>`).join("")}</div>`;
}

function codexBanner(): string {
  if (!snapshot.codex.stale) return "";
  return `<div class="banner" role="status">
    <i data-lucide="triangle-alert"></i>
    <div><strong>Codex 指向的端口是 ${snapshot.codex.configuredPort ?? "未知"}</strong><span>代理现在监听 ${snapshot.gateway.port}，重新应用配置后 Codex 才能连上。</span></div>
    <button class="button primary" data-action="apply-codex"><i data-lucide="save"></i>重新写入</button>
  </div>`;
}

function servicePanel(): string {
  const gateway = snapshot.gateway;
  const address = `http://${gateway.host}:${gateway.port}/v1`;
  const key = snapshot.settings.localAccessKey;
  const maskedKey = key.length > 12 ? `${key.slice(0, 9)}${"•".repeat(12)}${key.slice(-5)}` : "••••••••••••";
  return `<section class="panel service-panel">
    <div class="panel-header service-header">
      <div class="service-title-icon"><i data-lucide="database"></i></div><div><div class="panel-title">本地 API 服务</div><div class="panel-subtitle inline">OpenAI Responses Gateway</div></div>
      <div class="service-badges"><span class="service-badge ${gateway.running ? "online" : "offline"}">${gateway.running ? "运行中" : "已停止"}</span><span class="service-badge">仅本机</span><span class="service-badge ${snapshot.codex.stale ? "warn" : snapshot.codex.configured ? "online" : ""}">${snapshot.codex.stale ? "Codex 需重新应用" : snapshot.codex.configured ? "Codex 已接入" : "Codex 未接入"}</span>${snapshot.settings.upstreamKeepAlive ? `<span class="service-badge" title="复用 TCP 连接并发送保活探测"><i data-lucide="wifi"></i>连接复用</span>` : ""}</div>
      <div class="panel-actions"><button class="button primary" data-action="apply-and-restart-codex" ${codexReady() ? "" : "disabled"}><i data-lucide="power"></i>应用并重启 Codex</button><button class="button ${gateway.running ? "danger" : ""}" data-action="toggle-gateway"><i data-lucide="${gateway.running ? "square" : "play"}"></i>${gateway.running ? "停止" : "启动"}</button></div>
    </div>
    <div class="service-config-grid">
      <div class="service-config-item"><div class="config-label">服务地址</div><div class="config-value"><code>${address}</code><button class="mini-button" data-action="copy" data-copy="${address}" title="复制地址" aria-label="复制地址"><i data-lucide="copy"></i></button></div></div>
      <div class="service-config-item key-item"><div class="config-label">本地访问密钥</div><div class="config-value"><code>${escapeHtml(revealAccessKey ? key : maskedKey)}</code><button class="mini-button" data-action="toggle-key-visibility" title="${revealAccessKey ? "隐藏" : "显示"}密钥" aria-label="${revealAccessKey ? "隐藏" : "显示"}密钥"><i data-lucide="${revealAccessKey ? "eye-off" : "eye"}"></i></button><button class="mini-button" data-action="copy" data-copy="${escapeHtml(key)}" title="复制密钥" aria-label="复制密钥"><i data-lucide="copy"></i></button><button class="mini-button" data-action="reset-access-key" title="重置密钥" aria-label="重置密钥"><i data-lucide="rotate-ccw"></i></button></div></div>
      <div class="service-config-item"><div class="config-label">服务端口</div><div class="config-value"><strong>${gateway.port}</strong><button class="mini-button" data-view="settings" title="修改端口" aria-label="修改端口"><i data-lucide="settings"></i></button></div></div>
      <div class="service-config-item span-2"><div class="config-label">路由策略</div><div class="config-value">${strategyControl()}<span class="config-note">${escapeHtml(STRATEGIES.find((item) => item.value === gateway.strategy)?.hint ?? "")}</span></div></div>
      <div class="service-config-item"><div class="config-label">Codex Provider</div><div class="config-value"><strong>${snapshot.codex.configured ? "relaydeck" : "未配置"}</strong><span class="config-note">${escapeHtml(snapshot.codex.activeModel || enabledProviders()[0]?.model || "等待导入")}</span></div></div>
    </div>
  </section>`;
}

function routePanel(): string {
  const providers = enabledProviders();
  const ready = providers.filter((provider) => !inCooldown(provider) && provider.status !== "unhealthy");
  const order = snapshot.gateway.strategy === "fastest"
    ? [...ready].sort((a, b) => (a.latencyMs ?? Number.MAX_SAFE_INTEGER) - (b.latencyMs ?? Number.MAX_SAFE_INTEGER))
    : ready;
  const sidelined = providers.filter((provider) => !order.includes(provider));
  const rows = [...order, ...sidelined];
  return `<section class="panel">
    <div class="panel-header"><div class="panel-title">路由顺序</div><div class="panel-subtitle">${strategyLabel(snapshot.gateway.strategy)}</div><div class="panel-actions"><button class="mini-button" data-view="providers" title="管理路由" aria-label="管理路由"><i data-lucide="arrow-down-up"></i></button></div></div>
    <div class="route-list">${rows.length ? rows.slice(0, 6).map((provider, index) => `
      <div class="route-item ${order.includes(provider) ? "" : "sidelined"}">
        <span class="route-index">${order.includes(provider) ? index + 1 : "—"}</span>
        <span class="route-name">${escapeHtml(provider.name)}${provider.modelOverride ? ` <i class="route-pin" data-lucide="pin"></i>` : ""}</span>
        <span class="route-state">${inCooldown(provider) ? `<span data-live-cooldown="${escapeHtml(provider.cooldownUntil)}">${cooldownCopy(provider.cooldownUntil)}</span>` : provider.status === "healthy" ? `${provider.latencyMs || "--"} ms` : statusLabel(provider.status)}</span>
      </div>`).join("") : `<div class="empty" style="min-height:120px;padding:20px">暂无启用节点</div>`}</div>
  </section>`;
}

function activityItems(logs: RequestLog[], limit?: number): string {
  const items = limit ? logs.slice(0, limit) : logs;
  if (!items.length) return `<div class="empty"><div><div class="empty-icon"><i data-lucide="activity"></i></div><div class="empty-title">暂无请求记录</div><div class="empty-copy">Codex 发出请求后会实时出现在这里。</div></div></div>`;
  return `<div class="activity-list">${items.map((log) => {
    const remapped = log.upstreamModel && log.requestedModel && log.upstreamModel !== log.requestedModel;
    return `
    <div class="activity-item">
      <div class="activity-icon ${log.statusCode >= 400 ? "error" : ""}"><i data-lucide="${log.statusCode >= 400 ? "triangle-alert" : "zap"}"></i></div>
      <div class="activity-main">
        <div class="activity-title">${escapeHtml(log.method)} ${escapeHtml(log.path)} · ${escapeHtml(log.providerName)}</div>
        <div class="activity-meta"><span data-live-rel="${escapeHtml(log.timestamp)}">${relativeTime(log.timestamp)}</span> · HTTP ${log.statusCode}${log.attempts > 1 ? ` · ${log.attempts} 次尝试` : ""}${log.upstreamModel ? ` · ${escapeHtml(log.upstreamModel)}` : ""}${log.error ? ` · ${escapeHtml(log.error)}` : ""}</div>
      </div>
      ${remapped ? `<span class="tag-remap" title="请求的 ${escapeHtml(log.requestedModel!)} 已改写为该节点支持的 ${escapeHtml(log.upstreamModel!)}">已对齐模型</span>` : ""}
      <div class="activity-latency">${log.latencyMs} ms</div>
    </div>`;
  }).join("")}</div>`;
}

function recentActivityPanel(): string {
  return `<section class="panel"><div class="panel-header"><div class="panel-title">最近活动</div><div class="panel-actions"><button class="mini-button" data-view="activity" title="全部记录" aria-label="全部记录"><i data-lucide="list-restart"></i></button></div></div>${activityItems(snapshot.logs, 4)}</section>`;
}

/* -------------------------------------------------------------------- views */

function overviewView(): string {
  return `<div class="full-page">${codexBanner()}${servicePanel()}${statsPanel()}<div class="dashboard-grid"><div class="main-column">${providerPanel(enabledProviders().concat(snapshot.providers.filter((provider) => !provider.enabled)))}</div><aside class="side-column">${routePanel()}${recentActivityPanel()}</aside></div></div>`;
}

function providersView(): string {
  return `<div class="full-page">${codexBanner()}${providerPanel(visibleProviders(), true)}</div>`;
}

function activityView(): string {
  return `<div class="full-page"><section class="panel"><div class="panel-header"><div class="panel-title">请求记录</div><div class="panel-subtitle">仅保存在本机 · 最近 ${snapshot.logs.length} 条</div><div class="panel-actions"><button class="button" data-action="clear-logs" ${snapshot.logs.length ? "" : "disabled"}><i data-lucide="trash-2"></i>清空</button></div></div>${activityItems(snapshot.logs)}</section></div>`;
}

function codexSnippet(): string {
  const model = snapshot.codex.activeModel || enabledProviders()[0]?.modelOverride || enabledProviders()[0]?.model || "YOUR_MODEL";
  return [
    `model = "${model}"`,
    `model_provider = "relaydeck"`,
    ``,
    `[model_providers.relaydeck]`,
    `name = "RelayDeck"`,
    `base_url = "http://127.0.0.1:${snapshot.settings.gatewayPort}/v1"`,
    `env_key = "RELAYDECK_API_KEY"`,
    `wire_api = "responses"`,
    `request_max_retries = 2`,
    `stream_max_retries = 3`,
    `stream_idle_timeout_ms = ${snapshot.settings.streamIdleTimeoutSeconds * 1000}`,
  ].join("\n");
}

function toggleField(id: keyof AppSettings, label: string, hint: string, on: boolean): string {
  return `<div class="field"><label for="${id}">${label}</label><select class="select" id="${id}" name="${id}"><option value="true" ${on ? "selected" : ""}>开启</option><option value="false" ${on ? "" : "selected"}>关闭</option></select><div class="field-hint">${hint}</div></div>`;
}

function settingsView(): string {
  const settings = snapshot.settings;
  return `<div class="settings-grid">
    <div class="settings-column">
      <section class="panel settings-section">
        <div class="settings-title"><i data-lucide="sliders-horizontal"></i>代理与保活</div>
        <form id="settings-form" class="form-grid">
          <div class="field"><label for="gatewayPort">监听端口</label><input class="input" id="gatewayPort" name="gatewayPort" type="number" min="1024" max="65535" value="${settings.gatewayPort}" required /><div class="field-hint">改动后会自动重启代理</div></div>
          <div class="field"><label for="connectTimeoutSeconds">连接超时</label><input class="input" id="connectTimeoutSeconds" name="connectTimeoutSeconds" type="number" min="1" max="120" value="${settings.connectTimeoutSeconds}" required /><div class="field-hint">秒 · 建立 TCP/TLS 的上限</div></div>
          <div class="field"><label for="streamIdleTimeoutSeconds">流空闲超时</label><input class="input" id="streamIdleTimeoutSeconds" name="streamIdleTimeoutSeconds" type="number" min="10" max="3600" value="${settings.streamIdleTimeoutSeconds}" required /><div class="field-hint">秒 · 两个数据块之间的最长间隔，长回答不会被整体超时切断</div></div>
          <div class="field"><label for="requestTimeoutSeconds">测活超时</label><input class="input" id="requestTimeoutSeconds" name="requestTimeoutSeconds" type="number" min="10" max="600" value="${settings.requestTimeoutSeconds}" required /><div class="field-hint">秒 · 只作用于测活与密钥校验</div></div>
          <div class="field"><label for="healthIntervalMinutes">保活间隔</label><select class="select" id="healthIntervalMinutes" name="healthIntervalMinutes">${[1, 2, 5, 10, 30, 60].map((minutes) => `<option value="${minutes}" ${settings.healthIntervalMinutes === minutes ? "selected" : ""}>${minutes} 分钟</option>`).join("")}</select><div class="field-hint">节点冷却时会自动加密到 30 秒一次</div></div>
          ${toggleField("automaticHealthChecks", "自动保活", "定时校验 /v1/models，不消耗 token", settings.automaticHealthChecks)}
          ${toggleField("upstreamKeepAlive", "连接复用", "复用 TCP 连接并发送保活探测，切换更快", settings.upstreamKeepAlive)}
          ${toggleField("autoStartGateway", "开机启动代理", "打开 RelayDeck 时自动监听本地端口", settings.autoStartGateway)}
          <div class="field"><label>访问范围</label><input class="input" value="127.0.0.1 · 仅本机" disabled /><div class="field-hint">代理只监听 127.0.0.1，不向局域网开放</div></div>
          <div class="divider"></div>
          <div class="field"><label for="routeStrategy">路由策略</label><select class="select" id="routeStrategy" name="routeStrategy">${STRATEGIES.map((item) => `<option value="${item.value}" ${settings.routeStrategy === item.value ? "selected" : ""}>${item.label} · ${item.hint}</option>`).join("")}</select></div>
          <div class="field"><label for="maxFailoverAttempts">最大切换次数</label><input class="input" id="maxFailoverAttempts" name="maxFailoverAttempts" type="number" min="0" max="20" value="${settings.maxFailoverAttempts}" required /><div class="field-hint">单个请求最多尝试几个节点，0 表示不限</div></div>
          <div class="field"><label for="cooldownSeconds">冷却基数</label><input class="input" id="cooldownSeconds" name="cooldownSeconds" type="number" min="0" max="900" value="${settings.cooldownSeconds}" required /><div class="field-hint">秒 · 连续失败后按 2 倍递增，最长 15 分钟；0 表示不冷却</div></div>
          ${toggleField("failoverOnClientErrors", "认证错误也切换", "401 / 402 / 403 / 404 也视为该节点不可用", settings.failoverOnClientErrors)}
          ${toggleField("remapModel", "自动对齐模型", "目标节点不支持请求的模型时，改写为它支持的模型", settings.remapModel)}
          ${toggleField("skipUnhealthy", "跳过异常节点", "上次检测失败的节点降级为兜底，仍会在全部失败时尝试", settings.skipUnhealthy)}
          <div class="field full"><button class="button primary" type="submit" style="width:fit-content"><i data-lucide="save"></i>保存设置</button></div>
        </form>
      </section>
    </div>
    <div class="settings-column">
      <section class="panel settings-section">
        <div class="settings-title"><i data-lucide="power"></i>Codex 接入</div>
        <div class="codex-status-row"><span class="status-badge ${snapshot.codex.stale ? "unhealthy" : snapshot.codex.configured ? "healthy" : "unknown"}">${snapshot.codex.stale ? "端口已变化" : snapshot.codex.configured ? "已应用" : "未应用"}</span><span>${escapeHtml(snapshot.codex.configPath)}</span></div>
        <div class="code-box"><button class="copy-dark code-copy" data-action="copy" data-copy="${escapeHtml(codexSnippet())}" title="复制配置" aria-label="复制配置"><i data-lucide="copy"></i></button>${escapeHtml(codexSnippet())}</div>
        <div class="settings-actions"><button class="button" data-action="apply-codex" ${codexReady() ? "" : "disabled"}><i data-lucide="save"></i>仅写入配置</button><button class="button primary" data-action="apply-and-restart-codex" ${codexReady() ? "" : "disabled"}><i data-lucide="power"></i>应用并重启 Codex</button></div>
        <div class="field-hint" style="margin-top:10px">会备份现有 config.toml（保留最近 5 份）并写入用户环境变量；首次应用后重启一次 Codex。之后切换上游、改动路由策略都不需要再重启。</div>
      </section>
      <section class="panel settings-section">
        <div class="settings-title"><i data-lucide="scale"></i>路由行为说明</div>
        <div class="explain">
          <div class="explain-item"><i data-lucide="route"></i><div><strong>优先级</strong><span>严格按 P 序号，主力可用时始终走主力。</span></div></div>
          <div class="explain-item"><i data-lucide="rabbit"></i><div><strong>最快</strong><span>按最近一次保活延迟排序，适合多条线路质量接近时。</span></div></div>
          <div class="explain-item"><i data-lucide="repeat"></i><div><strong>轮询</strong><span>忽略优先级轮流使用可用节点，分摊单节点限流。</span></div></div>
          <div class="explain-item"><i data-lucide="snowflake"></i><div><strong>冷却</strong><span>连续失败 2 次起进入冷却并暂时移出路由，恢复后自动回归。</span></div></div>
        </div>
      </section>
    </div>
  </div>`;
}

/* ------------------------------------------------------------------ sessions */

type SessionKind = { label: string; className: string; hint: string };

function sessionKind(session: CodexSession): SessionKind {
  if (session.archived) return { label: "已归档", className: "archived", hint: "已移出 Codex 的会话目录，可随时恢复" };
  if (session.subagent) return { label: "子代理", className: "subagent", hint: "由其他会话派生，Codex 本来就不会列出" };
  if (!session.indexed) return { label: "未索引", className: "orphan", hint: "文件还在，但不在会话索引里，Codex 的 resume 列表看不到" };
  return { label: "可继续", className: "resumable", hint: "在会话索引里，可以直接 resume" };
}

function visibleSessions(): CodexSession[] {
  const all = sessions?.sessions ?? [];
  const query = sessionFilters.query.trim().toLowerCase();
  const matched = all.filter((session) => {
    const haystack = `${session.title} ${session.cwd ?? ""} ${session.id} ${session.modelProvider ?? ""} ${session.agentNickname ?? ""}`.toLowerCase();
    if (query && !haystack.includes(query)) return false;
    if (sessionFilters.kind === "resumable") return session.indexed && !session.archived;
    if (sessionFilters.kind === "orphan") return !session.indexed && !session.archived && !session.subagent;
    if (sessionFilters.kind === "archived") return session.archived;
    if (sessionFilters.kind === "subagent") return session.subagent;
    return true;
  });
  const sorters: Record<SessionSort, (a: CodexSession, b: CodexSession) => number> = {
    recent: (a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""),
    size: (a, b) => b.sizeBytes - a.sizeBytes,
    name: (a, b) => a.title.localeCompare(b.title, "zh-Hans-CN"),
  };
  return matched.sort(sorters[sessionFilters.sort]);
}

function sessionStats(): string {
  const overview = sessions!;
  return `<section class="panel">
    <div class="panel-header">
      <div class="panel-title">会话总览</div>
      <div class="panel-subtitle">${escapeHtml(overview.sessionsDir)}</div>
      <div class="panel-actions"><button class="button" data-action="load-sessions" ${sessionsLoading ? "disabled" : ""}>${sessionsLoading ? `<span class="spin-wrap"><i data-lucide="refresh-cw"></i></span>` : `<i data-lucide="refresh-cw"></i>`}重新扫描</button></div>
    </div>
    <div class="stats stats-six">
      <div class="stat"><div class="stat-label"><i data-lucide="messages-square"></i>会话总数</div><div class="stat-value">${overview.sessions.length}</div><div class="stat-foot">扫描于 <span data-live-rel="${escapeHtml(overview.scannedAt)}">${relativeTime(overview.scannedAt)}</span></div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="hard-drive"></i>占用空间</div><div class="stat-value compact">${formatBytes(overview.totalBytes)}</div><div class="stat-foot">归档 ${formatBytes(overview.archivedBytes)}</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="check"></i>可继续</div><div class="stat-value">${overview.indexedCount}</div><div class="stat-foot">在 Codex 的 resume 列表里</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="eye-off"></i>未索引</div><div class="stat-value">${overview.orphanCount}</div><div class="stat-foot">${overview.orphanCount ? "文件还在，但 resume 看不到" : "没有丢失的会话"}</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="archive"></i>已归档</div><div class="stat-value">${overview.archivedCount}</div><div class="stat-foot">${overview.archivedCount ? "已移出 Codex，可恢复" : "暂未归档任何会话"}</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="triangle-alert"></i>失效索引</div><div class="stat-value">${overview.ghostCount}</div><div class="stat-foot">${overview.ghostCount ? "指向已删除的文件" : "索引干净"}</div></div>
    </div>
  </section>`;
}

const needsRepair = () => Boolean(sessions && (sessions.orphanCount || sessions.ghostCount || sessions.providerMismatchCount));

function sessionBanner(): string {
  const overview = sessions!;
  if (!needsRepair()) return "";
  const parts = [
    overview.providerMismatchCount
      ? `${overview.providerMismatchCount} 个会话属于其他 Provider（当前 ${overview.activeProvider ?? "未知"}），Codex 列表看不到`
      : "",
    overview.orphanCount ? `${overview.orphanCount} 个会话不在索引里` : "",
    overview.ghostCount ? `${overview.ghostCount} 条索引指向已删除的文件` : "",
  ].filter(Boolean);
  return `<div class="banner" role="status">
    <i data-lucide="triangle-alert"></i>
    <div><strong>${escapeHtml(parts.join("，"))}</strong><span>修复会把这些会话归属到当前 Provider 并整理索引，Codex 重启后即可看到全部历史。修复前请先关闭 Codex。</span></div>
    <button class="button primary" data-action="repair-sessions" ${sessionsLoading ? "disabled" : ""}><i data-lucide="wrench"></i>修复可见性</button>
  </div>`;
}

function sessionToolbar(): string {
  const all = sessions!.sessions;
  const chips: { value: SessionFilter; label: string }[] = [
    { value: "all", label: `全部 ${all.length}` },
    { value: "resumable", label: `可继续 ${sessions!.indexedCount}` },
    { value: "orphan", label: `未索引 ${sessions!.orphanCount}` },
    { value: "archived", label: `已归档 ${sessions!.archivedCount}` },
    { value: "subagent", label: `子代理 ${all.filter((session) => session.subagent).length}` },
  ];
  return `<div class="toolbar">
    <div class="search"><i data-lucide="search"></i><input class="search-input" id="session-search" type="search" placeholder="搜索标题、目录或会话 ID" value="${escapeHtml(sessionFilters.query)}" aria-label="搜索会话" /></div>
    <div class="chips" role="group" aria-label="会话筛选">${chips.map((chip) => `<button class="chip ${sessionFilters.kind === chip.value ? "active" : ""}" data-action="set-session-filter" data-kind="${chip.value}">${escapeHtml(chip.label)}</button>`).join("")}</div>
    <label class="sort-field"><span>排序</span><select class="select compact" id="session-sort" aria-label="排序方式">
      <option value="recent" ${sessionFilters.sort === "recent" ? "selected" : ""}>最近更新</option>
      <option value="size" ${sessionFilters.sort === "size" ? "selected" : ""}>占用空间</option>
      <option value="name" ${sessionFilters.sort === "name" ? "selected" : ""}>标题</option>
    </select></label>
  </div>`;
}

function sessionRows(list: CodexSession[]): string {
  if (!list.length) {
    const filtered = Boolean(sessions?.sessions.length);
    return `<div class="empty">
      <div><div class="empty-icon"><i data-lucide="${filtered ? "search" : "messages-square"}"></i></div>
      <div class="empty-title">${filtered ? "没有匹配的会话" : "还没有 Codex 会话"}</div>
      <div class="empty-copy">${filtered ? "换个关键词，或清空筛选条件。" : "Codex 每次对话都会在 ~/.codex/sessions 留下一个 rollout 文件。"}</div>
      ${filtered ? `<button class="button primary" data-action="reset-session-filters"><i data-lucide="list-filter"></i>清空筛选</button>` : ""}</div>
    </div>`;
  }
  return list.map((session) => {
    const kind = sessionKind(session);
    const location = session.cwd || session.path;
    return `
    <div class="session-row ${session.archived ? "archived" : ""}">
      <div class="session-main">
        <div class="session-icon ${kind.className}"><i data-lucide="${session.subagent ? "bot" : session.archived ? "archive" : "messages-square"}"></i></div>
        <div class="session-copy">
          <div class="session-title" title="${escapeHtml(session.title)}">${escapeHtml(session.title)}</div>
          <div class="session-path" title="${escapeHtml(session.path)}">${escapeHtml(location)}</div>
          <div class="session-tags">
            <span class="session-badge ${kind.className}" title="${escapeHtml(kind.hint)}">${kind.label}</span>
            ${session.viaRelaydeck ? `<span class="session-badge relay" title="这次会话走的是 RelayDeck 代理">RelayDeck</span>` : ""}
            ${session.modelProvider && !session.viaRelaydeck ? `<span>${escapeHtml(session.modelProvider)}</span>` : ""}
            ${session.cliVersion ? `<span>Codex ${escapeHtml(session.cliVersion)}</span>` : ""}
            ${session.forkedFromId ? `<span title="由另一个会话分叉而来">分叉</span>` : ""}
          </div>
        </div>
      </div>
      <div class="session-size">${formatBytes(session.sizeBytes)}</div>
      <div class="session-time">${session.updatedAt ? `<span data-live-rel="${escapeHtml(session.updatedAt)}">${relativeTime(session.updatedAt)}</span>` : "未知"}</div>
      <div class="row-actions">
        <button class="mini-button" data-action="copy" data-copy="codex resume ${escapeHtml(session.id)}" title="复制 codex resume 命令" aria-label="复制 resume 命令"><i data-lucide="copy"></i></button>
        <button class="mini-button" data-action="${session.archived ? "restore-session" : "archive-session"}" data-id="${escapeHtml(session.id)}" title="${session.archived ? "恢复到 Codex" : "归档（从 Codex 隐藏，不删除）"}" aria-label="${session.archived ? "恢复" : "归档"}"><i data-lucide="${session.archived ? "archive-restore" : "archive"}"></i></button>
        <button class="mini-button" data-action="session-menu" data-id="${escapeHtml(session.id)}" title="更多操作" aria-label="更多操作"><i data-lucide="more-horizontal"></i></button>
      </div>
    </div>`;
  }).join("");
}

function sessionsView(): string {
  if (!sessions) {
    return `<div class="full-page"><section class="panel"><div class="empty">
      <div><div class="empty-icon">${sessionsLoading ? `<span class="spin-wrap"><i data-lucide="refresh-cw"></i></span>` : `<i data-lucide="messages-square"></i>`}</div>
      <div class="empty-title">${sessionsLoading ? "正在扫描 Codex 会话" : "尚未扫描会话"}</div>
      <div class="empty-copy">读取 ~/.codex/sessions 下的 rollout 文件与会话索引，只读不改动。</div>
      ${sessionsLoading ? "" : `<button class="button primary" data-action="load-sessions"><i data-lucide="refresh-cw"></i>开始扫描</button>`}</div>
    </div></section></div>`;
  }
  const list = visibleSessions();
  return `<div class="full-page">
    ${sessionBanner()}
    ${sessionStats()}
    <section class="panel session-list">
      <div class="panel-header">
        <div class="panel-title">会话列表</div>
        <div class="panel-subtitle">显示 ${list.length} / ${sessions.sessions.length} 条</div>
        <div class="panel-actions">
          <button class="button" data-action="copy" data-copy="${escapeHtml(sessions.sessionsDir)}" title="复制会话目录"><i data-lucide="copy"></i>会话目录</button>
          <button class="button" data-action="repair-sessions" ${sessionsLoading || !needsRepair() ? "disabled" : ""}><i data-lucide="wrench"></i>修复可见性</button>
        </div>
      </div>
      ${sessionToolbar()}
      ${list.length ? `<div class="session-head"><div>会话</div><div>大小</div><div>更新</div><div></div></div>` : ""}
      ${sessionRows(list)}
    </section>
  </div>`;
}

const viewTitles: Record<View, [string, string]> = {
  overview: ["控制台", "路由与代理状态"], providers: ["Provider", "中转节点管理"], sessions: ["会话", "Codex 对话归档与恢复"],
  activity: ["活动", "本地请求记录"], settings: ["设置", "代理与 Codex 配置"],
};

let renderedView: View | null = null;

function render(): void {
  const [title, meta] = viewTitles[activeView];
  const workspace = document.querySelector<HTMLElement>(".workspace");
  const scrollTop = workspace?.scrollTop ?? 0;
  // Animate the content in only when the view actually changed, not on every poll re-render.
  const entering = renderedView !== activeView;
  renderedView = activeView;
  app.innerHTML = `<div class="app-shell">
    <aside class="sidebar">
      <div class="brand"><div class="brand-mark"></div><div class="brand-name">RelayDeck</div></div>
      <nav class="nav">
        <button class="nav-button ${activeView === "overview" ? "active" : ""}" data-view="overview"><i data-lucide="layout-dashboard"></i><span class="nav-label">控制台</span></button>
        <button class="nav-button ${activeView === "providers" ? "active" : ""}" data-view="providers"><i data-lucide="server"></i><span class="nav-label">Provider</span><span class="nav-count">${snapshot.providers.length}</span></button>
        <button class="nav-button ${activeView === "sessions" ? "active" : ""}" data-view="sessions"><i data-lucide="messages-square"></i><span class="nav-label">会话</span>${sessions?.sessions.length ? `<span class="nav-count">${sessions.sessions.length}</span>` : ""}</button>
        <button class="nav-button ${activeView === "activity" ? "active" : ""}" data-view="activity"><i data-lucide="activity"></i><span class="nav-label">活动</span>${snapshot.logs.length ? `<span class="nav-count">${snapshot.logs.length}</span>` : ""}</button>
        <button class="nav-button ${activeView === "settings" ? "active" : ""}" data-view="settings"><i data-lucide="settings"></i><span class="nav-label">设置</span></button>
      </nav>
      <div class="sidebar-spacer"></div>
      <div class="sidebar-status">
        <div class="sidebar-status-top"><span class="sidebar-status-title">Gateway</span><span class="live-dot ${snapshot.gateway.running ? "" : "off"}"></span></div>
        <div class="sidebar-status-copy">${snapshot.gateway.running
          ? `127.0.0.1:${snapshot.gateway.port}<br>${snapshot.gateway.routableCount} 个节点参与路由 · ${strategyLabel(snapshot.gateway.strategy)}<br>运行 <span data-live-uptime>${formatDuration(snapshot.gateway.uptimeSeconds)}</span>`
          : "代理服务已停止"}</div>
      </div>
      <div class="sidebar-foot">
        <button class="theme-toggle" data-action="toggle-theme" title="切换${theme === "dark" ? "浅色" : "深色"}主题" aria-label="切换主题"><i data-lucide="${theme === "dark" ? "sun" : "moon"}"></i></button>
        <span class="version">v0.1.0</span>
      </div>
    </aside>
    <main class="workspace">
      <header class="topbar">
        <div class="page-title">${title}</div><div class="page-meta">${meta}</div>
        ${isDesktop() ? "" : `<span class="preview-badge" title="本机操作仅在 RelayDeck 桌面版可用">浏览器预览</span>`}
        <div class="topbar-actions"><button class="button" data-action="open-import"><i data-lucide="import"></i>快速导入</button><button class="button primary" data-action="add-provider"><i data-lucide="plus"></i>添加 Provider</button></div>
        ${busy ? `<div class="busy-bar" role="progressbar" aria-label="处理中"></div>` : ""}
      </header>
      <div class="content ${entering ? "view-enter" : ""}">${activeView === "overview" ? overviewView() : activeView === "providers" ? providersView() : activeView === "sessions" ? sessionsView() : activeView === "activity" ? activityView() : settingsView()}</div>
    </main>
  </div>`;
  const nextWorkspace = document.querySelector<HTMLElement>(".workspace");
  // A new view starts at the top; re-renders of the same view keep the reading position.
  if (nextWorkspace) nextWorkspace.scrollTop = entering ? 0 : scrollTop;
  bindEvents();
  refreshIcons();
}

/** Re-renders while keeping the caret inside the field the user is typing in. */
function renderKeepingFocus(): void {
  const active = document.activeElement as HTMLInputElement | null;
  const id = active?.id;
  const caret = active?.selectionStart ?? null;
  render();
  if (!id) return;
  const restored = document.getElementById(id) as HTMLInputElement | null;
  if (!restored) return;
  restored.focus();
  if (caret !== null && restored.type !== "number") {
    try { restored.setSelectionRange(caret, caret); } catch { /* not all inputs support selection */ }
  }
}

function bindEvents(): void {
  document.querySelectorAll<HTMLElement>("[data-view]").forEach((element) => element.addEventListener("click", () => {
    activeView = element.dataset.view as View;
    render();
    // The first visit pays for the scan; afterwards the list is refreshed explicitly.
    if (activeView === "sessions" && !sessions && !sessionsLoading) void loadSessions();
  }));
  document.querySelectorAll<HTMLElement>("[data-action]").forEach((element) => element.addEventListener("click", () => void handleAction(element)));
  document.querySelector<HTMLFormElement>("#settings-form")?.addEventListener("submit", (event) => void saveSettings(event));
  const search = document.querySelector<HTMLInputElement>("#provider-search");
  search?.addEventListener("input", () => {
    filters.query = search.value;
    renderKeepingFocus();
  });
  const sort = document.querySelector<HTMLSelectElement>("#provider-sort");
  sort?.addEventListener("change", () => {
    filters.sort = sort.value as ProviderSort;
    render();
  });
  const sessionSearch = document.querySelector<HTMLInputElement>("#session-search");
  sessionSearch?.addEventListener("input", () => {
    sessionFilters.query = sessionSearch.value;
    renderKeepingFocus();
  });
  const sessionSort = document.querySelector<HTMLSelectElement>("#session-sort");
  sessionSort?.addEventListener("change", () => {
    sessionFilters.sort = sessionSort.value as SessionSort;
    render();
  });
}

async function handleAction(element: HTMLElement): Promise<void> {
  const action = element.dataset.action;
  if (action === "add-provider") openProviderModal();
  if (action === "open-import") openImportModal();
  if (action === "copy") await copyText(element.dataset.copy || "");
  if (action === "toggle-provider") await toggleProvider(element.dataset.id!, element.dataset.enabled === "true");
  if (action === "test-provider") await testProvider(element.dataset.id!);
  if (action === "test-all") await testAllProviders();
  if (action === "refresh-health") await refreshHealth();
  if (action === "provider-menu") openProviderActions(element.dataset.id!);
  if (action === "toggle-gateway") await toggleGateway();
  if (action === "clear-logs") await clearLogs();
  if (action === "toggle-key-visibility") { revealAccessKey = !revealAccessKey; render(); }
  if (action === "reset-access-key") await resetAccessKey();
  if (action === "apply-codex") await applyCodexConfig();
  if (action === "apply-and-restart-codex") await applyAndRestartCodex();
  if (action === "set-strategy") await setStrategy(element.dataset.strategy as RouteStrategy);
  if (action === "set-status-filter") { filters.status = element.dataset.status as StatusFilter; render(); }
  if (action === "reset-filters") { filters.query = ""; filters.status = "all"; render(); }
  if (action === "toggle-theme") toggleTheme();
  if (action === "load-sessions") await loadSessions();
  if (action === "repair-sessions") await repairSessions();
  if (action === "archive-session") await archiveSession(element.dataset.id!);
  if (action === "restore-session") await restoreSession(element.dataset.id!);
  if (action === "reveal-session") await revealSession(element.dataset.id!);
  if (action === "session-menu") openSessionActions(element.dataset.id!);
  if (action === "set-session-filter") { sessionFilters.kind = element.dataset.kind as SessionFilter; render(); }
  if (action === "reset-session-filters") { sessionFilters.query = ""; sessionFilters.kind = "all"; render(); }
}

/* ------------------------------------------------------------------ actions */

async function reload(): Promise<void> {
  snapshot = await call<AppSnapshot>("get_snapshot");
  render();
}

async function withBusy(work: () => Promise<void>): Promise<void> {
  busy = true;
  render();
  try {
    await work();
  } finally {
    busy = false;
  }
}

function toggleTheme(): void {
  theme = theme === "dark" ? "light" : "dark";
  localStorage.setItem(THEME_KEY, theme);
  document.documentElement.dataset.theme = theme;
  render();
}

async function setStrategy(strategy: RouteStrategy): Promise<void> {
  if (!strategy || strategy === snapshot.settings.routeStrategy) return;
  try {
    await call("save_settings", { settings: { ...snapshot.settings, routeStrategy: strategy } });
    await reload();
    toast(`路由策略已切换为${strategyLabel(strategy)}`);
  } catch (error) { toast(String(error), "error"); }
}

async function toggleProvider(id: string, enabled: boolean): Promise<void> {
  try {
    await call("toggle_provider", { id, enabled });
    await reload();
    toast(enabled ? "Provider 已加入路由" : "Provider 已停用");
  } catch (error) { toast(String(error), "error"); }
}

async function testProvider(id: string): Promise<void> {
  const current = snapshot.providers.find((provider) => provider.id === id);
  if (!current) return;
  current.status = "checking";
  render();
  try {
    const updated = await call<Provider>("test_provider", { id });
    await reload();
    toast(updated.status === "healthy" ? `${updated.name} 可用 · ${updated.latencyMs} ms` : `${updated.name} 检测失败：${updated.lastError ?? "未知原因"}`, updated.status === "healthy" ? "success" : "error");
  } catch (error) {
    await reload();
    toast(String(error), "error");
  }
}

async function testAllProviders(): Promise<void> {
  await withBusy(async () => {
    snapshot.providers.filter((provider) => provider.enabled).forEach((provider) => { provider.status = "checking"; });
    render();
    await Promise.allSettled(snapshot.providers.filter((provider) => provider.enabled).map((provider) => call("test_provider", { id: provider.id })));
  });
  await reload();
  const healthy = healthyProviders().length;
  toast(`全部测活完成 · ${healthy} / ${enabledProviders().length} 个节点可用`, healthy ? "success" : "error");
}

async function refreshHealth(): Promise<void> {
  await withBusy(async () => {
    await call<Provider[]>("refresh_health");
  });
  await reload();
  toast(`保活检查完成 · ${snapshot.gateway.routableCount} 个节点参与路由`);
}

async function toggleGateway(): Promise<void> {
  const wasRunning = snapshot.gateway.running;
  try {
    await call(wasRunning ? "stop_gateway" : "start_gateway");
    await reload();
    toast(wasRunning ? "本地代理已停止" : `本地代理已启动 · 127.0.0.1:${snapshot.gateway.port}`);
  } catch (error) { toast(String(error), "error"); }
}

async function clearLogs(): Promise<void> {
  await call("clear_logs");
  await reload();
  toast("统计与请求记录已清空");
}

async function resetAccessKey(): Promise<void> {
  if (!(await confirmDialog("重置本地访问密钥", "重置后需要重新应用 Codex 配置，Codex 才能继续通过本地代理访问。", "重置密钥", true))) return;
  try {
    await call("reset_access_key");
    revealAccessKey = false;
    await reload();
    toast("本地访问密钥已重置，请重新写入 Codex 配置");
  } catch (error) { toast(String(error), "error"); }
}

async function applyCodexConfig(): Promise<void> {
  if (!isDesktop()) {
    toast("浏览器预览不能修改本机 Codex，请使用 RelayDeck.exe", "error");
    return;
  }
  try {
    const result = await call<CodexApplyResult>("apply_codex_config");
    await reload();
    const aligned = result.alignedSessions ? `，已同步 ${result.alignedSessions} 条历史会话` : "";
    toast(`配置已写入 · ${result.model}（来自 ${result.providerName}）${aligned}`);
  } catch (error) { toast(String(error), "error"); }
}

async function applyAndRestartCodex(): Promise<void> {
  if (!isDesktop()) {
    toast("浏览器预览不能重启 Codex，请使用 RelayDeck.exe", "error");
    return;
  }
  if (!(await confirmDialog("应用并重启 Codex", "将备份并应用配置，同步历史会话归属，然后关闭所有 Codex 窗口并重新启动。未保存的 Codex 任务会中断。", "应用并重启"))) return;
  await withBusy(async () => {
    try {
      const result = await call<CodexApplyAndRestartResult>("apply_and_restart_codex");
      await reload();
      const version = result.restart.version ? ` ${result.restart.version}` : "";
      const aligned = result.apply.alignedSessions ? `，已同步 ${result.apply.alignedSessions} 条历史会话` : "";
      toast(`已应用 ${result.apply.model}${aligned}，正在启动 ${result.restart.appName}${version}`);
    } catch (error) { toast(String(error), "error"); }
  });
  render();
}

async function loadSessions(): Promise<void> {
  sessionsLoading = true;
  render();
  try {
    sessions = await call<SessionOverview>("list_codex_sessions");
  } catch (error) {
    toast(String(error), "error");
  } finally {
    sessionsLoading = false;
    render();
  }
}

async function repairSessions(): Promise<void> {
  if (!(await confirmDialog("修复会话可见性", "将备份后把历史会话归属到当前 Provider、补全索引并清掉失效条目。建议先关闭 Codex 再执行。", "开始修复"))) return;
  try {
    const result = await call<SessionRepairResult>("repair_session_visibility");
    await loadSessions();
    const parts = [
      result.alignedThreads ? `归属 ${result.alignedThreads} 个会话到当前 Provider` : "",
      result.restored ? `恢复 ${result.restored} 条索引` : "",
      result.removedGhosts ? `清理 ${result.removedGhosts} 条失效索引` : "",
    ].filter(Boolean);
    toast(parts.length ? `修复完成：${parts.join("，")}，重启 Codex 后生效` : "会话可见性正常，无需修复");
  } catch (error) { toast(String(error), "error"); }
}

async function archiveSession(id: string): Promise<void> {
  try {
    const session = await call<CodexSession>("archive_codex_session", { id });
    await loadSessions();
    toast(`已归档「${session.title}」，Codex 不再列出，可随时恢复`);
  } catch (error) { toast(String(error), "error"); }
}

async function restoreSession(id: string): Promise<void> {
  try {
    const session = await call<CodexSession>("restore_codex_session", { id });
    await loadSessions();
    toast(`已恢复「${session.title}」，重启 Codex 后可以 resume`);
  } catch (error) { toast(String(error), "error"); }
}

async function deleteSession(id: string, title: string): Promise<void> {
  if (!(await confirmDialog("永久删除会话", `将删除「${title}」的 rollout 文件，这条对话记录无法恢复。`, "永久删除", true))) return;
  try {
    const freed = await call<number>("delete_codex_session", { id });
    await loadSessions();
    toast(`会话已删除，释放 ${formatBytes(freed)}`);
  } catch (error) { toast(String(error), "error"); }
}

async function revealSession(id: string): Promise<void> {
  if (!isDesktop()) {
    toast("浏览器预览不能打开本机文件夹，请使用 RelayDeck.exe", "error");
    return;
  }
  try {
    await call<string>("reveal_codex_session", { id });
  } catch (error) { toast(String(error), "error"); }
}

function openSessionActions(id: string): void {
  const session = sessions?.sessions.find((item) => item.id === id);
  if (!session) return;
  const kind = sessionKind(session);
  const resumeCommand = `codex resume ${session.id}`;
  const body = `
    <div class="proxy-metric"><span>状态</span><strong>${kind.label} · ${escapeHtml(kind.hint)}</strong></div>
    <div class="proxy-metric"><span>会话 ID</span><strong>${escapeHtml(session.id)}</strong></div>
    <div class="proxy-metric"><span>工作目录</span><strong title="${escapeHtml(session.cwd ?? "")}">${escapeHtml(session.cwd || "未记录")}</strong></div>
    <div class="proxy-metric"><span>上游 Provider</span><strong>${escapeHtml(session.modelProvider || "未记录")}${session.viaRelaydeck ? "（经 RelayDeck）" : ""}</strong></div>
    <div class="proxy-metric"><span>创建 / 更新</span><strong>${session.createdAt ? relativeTime(session.createdAt) : "未知"} · ${session.updatedAt ? relativeTime(session.updatedAt) : "未知"}</strong></div>
    <div class="proxy-metric"><span>体积</span><strong>${formatBytes(session.sizeBytes)}</strong></div>
    ${session.forkedFromId ? `<div class="proxy-metric"><span>分叉自</span><strong>${escapeHtml(session.forkedFromId)}</strong></div>` : ""}
    <div class="code-box"><button class="copy-dark code-copy" data-action="copy" data-copy="${escapeHtml(resumeCommand)}" title="复制命令" aria-label="复制命令"><i data-lucide="copy"></i></button>${escapeHtml(resumeCommand)}</div>
    <div class="modal-toolbar">
      <button class="button" data-reveal><i data-lucide="folder-open"></i>打开所在文件夹</button>
      <button class="button" data-copy-path><i data-lucide="copy"></i>复制文件路径</button>
    </div>`;
  const footer = `<button class="button danger" data-delete ${session.archived ? "" : "disabled"} title="${session.archived ? "永久删除 rollout 文件" : "请先归档，再删除"}"><i data-lucide="trash-2"></i>删除</button>
    <button class="button" data-close>关闭</button>
    <button class="button primary" data-toggle><i data-lucide="${session.archived ? "archive-restore" : "archive"}"></i>${session.archived ? "恢复到 Codex" : "归档"}</button>`;
  const modal = modalShell(session.title, body, footer);
  modal.querySelector<HTMLElement>("[data-reveal]")!.addEventListener("click", () => { modal.remove(); void revealSession(id); });
  modal.querySelector<HTMLElement>("[data-copy-path]")!.addEventListener("click", () => { modal.remove(); void copyText(session.path); });
  modal.querySelector<HTMLElement>("[data-toggle]")!.addEventListener("click", () => {
    modal.remove();
    void (session.archived ? restoreSession(id) : archiveSession(id));
  });
  modal.querySelector<HTMLElement>("[data-delete]")!.addEventListener("click", () => { modal.remove(); void deleteSession(id, session.title); });
  modal.querySelectorAll<HTMLElement>("[data-action='copy']").forEach((button) => button.addEventListener("click", () => void copyText(button.dataset.copy || "")));
}

async function copyText(value: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(value);
    toast("已复制到剪贴板");
  } catch { toast("复制失败，请手动选择内容", "error"); }
}

/* ------------------------------------------------------------------- modals */

function modalShell(title: string, body: string, footer: string, wide = false): HTMLDivElement {
  const opener = document.activeElement as HTMLElement | null;
  const backdrop = document.createElement("div");
  backdrop.className = "modal-backdrop";
  backdrop.innerHTML = `<div class="modal ${wide ? "wide" : ""}" role="dialog" aria-modal="true" aria-label="${escapeHtml(title)}"><div class="modal-header"><div class="modal-title">${escapeHtml(title)}</div><button class="mini-button modal-close" data-close title="关闭" aria-label="关闭"><i data-lucide="x"></i></button></div><div class="modal-body">${body}</div><div class="modal-footer">${footer}</div></div>`;
  const close = () => backdrop.remove();
  // Cleanup keys off DOM removal, so `modal.remove()` from any caller also detaches the
  // keydown listener and returns focus to the opener.
  const cleanup = new MutationObserver(() => {
    if (backdrop.isConnected) return;
    cleanup.disconnect();
    document.removeEventListener("keydown", onKeyDown);
    opener?.focus?.();
  });
  function onKeyDown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      close();
    }
    if (event.key === "Enter" && !(event.target instanceof HTMLTextAreaElement)) {
      const primary = backdrop.querySelector<HTMLElement>(".modal-footer .button.primary");
      if (primary && !primary.hasAttribute("disabled")) {
        event.preventDefault();
        primary.click();
      }
    }
  }
  backdrop.addEventListener("mousedown", (event) => { if (event.target === backdrop) close(); });
  backdrop.querySelectorAll<HTMLElement>("[data-close]").forEach((item) => item.addEventListener("click", close));
  document.addEventListener("keydown", onKeyDown);
  document.body.append(backdrop);
  cleanup.observe(document.body, { childList: true });
  refreshIcons();
  backdrop.querySelector<HTMLElement>("input, select, textarea, .button.primary")?.focus();
  return backdrop;
}

/** In-app replacement for window.confirm: themed, Chinese buttons, Esc = cancel, Enter = confirm. */
function confirmDialog(title: string, body: string, confirmLabel = "确定", danger = false): Promise<boolean> {
  return new Promise((resolve) => {
    const modal = modalShell(
      title,
      `<div class="confirm-copy">${escapeHtml(body)}</div>`,
      `<button class="button" data-close>取消</button><button class="button primary ${danger ? "danger" : ""}" data-confirm>${escapeHtml(confirmLabel)}</button>`,
    );
    let confirmed = false;
    modal.querySelector<HTMLElement>("[data-confirm]")!.addEventListener("click", () => { confirmed = true; modal.remove(); });
    const watch = new MutationObserver(() => {
      if (modal.isConnected) return;
      watch.disconnect();
      resolve(confirmed);
    });
    watch.observe(document.body, { childList: true });
  });
}

function modelOptions(provider?: Provider): string {
  if (!provider?.availableModels.length) return "";
  const pinned = provider.modelOverride ?? "";
  return `<div class="field full"><label for="modelOverride">锁定模型</label>
    <select class="select" id="modelOverride" name="modelOverride">
      <option value="" ${pinned ? "" : "selected"}>自动选择（当前 ${escapeHtml(provider.model)}）</option>
      ${provider.availableModels.map((model) => `<option value="${escapeHtml(model)}" ${pinned === model ? "selected" : ""}>${escapeHtml(model)}</option>`).join("")}
    </select>
    <div class="field-hint">锁定后该节点只会使用这个模型；其他节点不支持时会自动对齐到各自可用的模型</div></div>`;
}

function openProviderModal(provider?: Provider): void {
  const form = `<form id="provider-form" class="form-grid">
    <div class="field full"><label for="name">名称</label><input class="input" id="name" name="name" value="${escapeHtml(provider?.name || "")}" placeholder="例如：主力线路" required /></div>
    <div class="field full"><label for="baseUrl">API 地址</label><input class="input" id="baseUrl" name="baseUrl" type="url" value="${escapeHtml(provider?.baseUrl || "")}" placeholder="https://example.com/v1" required /></div>
    <div class="field full"><label for="apiKey">API Key</label><input class="input" id="apiKey" name="apiKey" type="password" autocomplete="off" placeholder="${provider?.hasKey ? "留空以保留当前密钥" : "sk-..."}" ${provider?.hasKey ? "" : "required"} /><div class="field-hint">保存后会获取模型，并向选中模型发送一次最小请求验证 API Key；可能产生极少量 token 费用</div></div>
    <div class="field"><label for="priority">路由优先级</label><input class="input" id="priority" name="priority" type="number" min="1" max="99" value="${provider?.priority || snapshot.providers.length + 1}" required /><div class="field-hint">数字越小越优先</div></div>
    <div class="field"><label for="enabled">加入路由</label><select class="select" id="enabled" name="enabled"><option value="true" ${provider?.enabled !== false ? "selected" : ""}>启用</option><option value="false" ${provider?.enabled === false ? "selected" : ""}>停用</option></select></div>
    ${modelOptions(provider)}
  </form>`;
  const modal = modalShell(provider ? "编辑 Provider" : "添加 Provider", form, `<button class="button" data-close>取消</button><button class="button primary" data-save><i data-lucide="save"></i>保存</button>`);
  modal.querySelector<HTMLElement>("[data-save]")!.addEventListener("click", () => void saveProvider(modal, provider));
}

async function saveProvider(modal: HTMLElement, provider?: Provider): Promise<void> {
  const form = modal.querySelector<HTMLFormElement>("#provider-form")!;
  if (!form.reportValidity()) return;
  const data = new FormData(form);
  // The model picker only exists once a provider has reported its model list; keep the pin untouched otherwise.
  const pinned = data.get("modelOverride");
  const input: ProviderInput = {
    id: provider?.id,
    name: String(data.get("name")), baseUrl: String(data.get("baseUrl")), apiKey: String(data.get("apiKey") ?? "") || undefined,
    model: provider?.model, modelOverride: pinned === null ? provider?.modelOverride : String(pinned) || undefined,
    enabled: data.get("enabled") === "true", priority: Number(data.get("priority")),
  };
  try {
    await call("save_provider", { input });
    modal.remove();
    await reload();
    toast(provider ? "Provider 已更新" : "Provider 已添加");
  } catch (error) { toast(String(error), "error"); }
}

function openProviderActions(id: string): void {
  const provider = snapshot.providers.find((item) => item.id === id);
  if (!provider) return;
  const ordered = enabledProviders().concat(snapshot.providers.filter((item) => !item.enabled));
  const position = ordered.findIndex((item) => item.id === id);
  const body = `
    <div class="proxy-metric"><span>API 地址</span><strong>${escapeHtml(provider.baseUrl)}</strong></div>
    <div class="proxy-metric"><span>当前模型</span><strong>${escapeHtml(provider.modelOverride || provider.model)}${provider.modelOverride ? "（已锁定）" : ""}</strong></div>
    <div class="proxy-metric"><span>可用模型</span><strong>${provider.availableModels.length || "--"}</strong></div>
    <div class="proxy-metric"><span>路由优先级</span><strong>P${provider.priority}</strong></div>
    <div class="proxy-metric"><span>本次运行</span><strong>成功 ${provider.servedCount} · 失败 ${provider.failedCount}</strong></div>
    <div class="proxy-metric"><span>状态</span><strong>${inCooldown(provider) ? `冷却中 · ${cooldownCopy(provider.cooldownUntil)}` : statusLabel(provider.status)}</strong></div>
    ${provider.lastError ? `<div class="error-box">${escapeHtml(provider.lastError)}</div>` : ""}
    <div class="modal-toolbar">
      <button class="button" data-move="-1" ${position <= 0 ? "disabled" : ""}><i data-lucide="arrow-up"></i>上移</button>
      <button class="button" data-move="1" ${position < 0 || position >= ordered.length - 1 ? "disabled" : ""}><i data-lucide="arrow-down"></i>下移</button>
      ${inCooldown(provider) ? `<button class="button" data-thaw><i data-lucide="snowflake"></i>立即恢复</button>` : ""}
      ${provider.modelOverride ? `<button class="button" data-unpin><i data-lucide="pin-off"></i>解除锁定</button>` : ""}
    </div>`;
  const modal = modalShell(provider.name, body, `<button class="button danger" data-delete><i data-lucide="trash-2"></i>删除</button><button class="button" data-edit><i data-lucide="pencil"></i>编辑</button><button class="button primary" data-test><i data-lucide="refresh-cw"></i>重新测活</button>`);
  modal.querySelector<HTMLElement>("[data-edit]")!.addEventListener("click", () => { modal.remove(); openProviderModal(provider); });
  modal.querySelector<HTMLElement>("[data-test]")!.addEventListener("click", () => { modal.remove(); void testProvider(id); });
  modal.querySelector<HTMLElement>("[data-delete]")!.addEventListener("click", () => { modal.remove(); void deleteProvider(id); });
  modal.querySelectorAll<HTMLElement>("[data-move]").forEach((button) => button.addEventListener("click", () => {
    modal.remove();
    void moveProvider(id, Number(button.dataset.move));
  }));
  modal.querySelector<HTMLElement>("[data-thaw]")?.addEventListener("click", () => { modal.remove(); void thawProvider(id); });
  modal.querySelector<HTMLElement>("[data-unpin]")?.addEventListener("click", () => { modal.remove(); void pinModel(id, null); });
}

async function moveProvider(id: string, offset: number): Promise<void> {
  try {
    await call("move_provider", { id, offset });
    await reload();
    toast("路由顺序已更新");
  } catch (error) { toast(String(error), "error"); }
}

async function thawProvider(id: string): Promise<void> {
  try {
    const provider = await call<Provider>("clear_provider_cooldown", { id });
    await reload();
    toast(provider.status === "healthy" ? `${provider.name} 已恢复路由` : `${provider.name} 仍然不可用：${provider.lastError ?? "未知原因"}`, provider.status === "healthy" ? "success" : "error");
  } catch (error) { toast(String(error), "error"); }
}

async function pinModel(id: string, model: string | null): Promise<void> {
  try {
    await call("set_provider_model", { id, model });
    await reload();
    toast(model ? `已锁定模型 ${model}` : "已恢复自动选择模型");
  } catch (error) { toast(String(error), "error"); }
}

async function deleteProvider(id: string): Promise<void> {
  const provider = snapshot.providers.find((item) => item.id === id);
  if (!(await confirmDialog("删除 Provider", `将删除「${provider?.name ?? "该节点"}」及其密钥，请求记录保留。`, "删除", true))) return;
  await call("delete_provider", { id });
  await reload();
  toast("Provider 已删除");
}

function parseProviders(raw: string): ProviderInput[] {
  const trimmed = raw.trim();
  if (!trimmed) return [];
  if (trimmed.startsWith("[") || trimmed.startsWith("{")) {
    const normalized = trimmed.replace(/\]\s*\[/g, ",");
    const parsed = JSON.parse(normalized) as unknown;
    const list = Array.isArray(parsed) ? parsed : [parsed];
    return list.map((item, index) => {
      const value = item as Record<string, unknown>;
      return {
        name: String(value.name || value.label || value.api_provider_name || `Provider ${index + 1}`),
        baseUrl: String(value.baseUrl || value.base_url || value.api_base_url || value.url || ""),
        apiKey: String(value.apiKey || value.api_key || value.OPENAI_API_KEY || value.key || ""),
        enabled: value.enabled !== false, priority: Number(value.priority || snapshot.providers.length + index + 1),
      };
    }).filter((item) => item.baseUrl);
  }
  return trimmed.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).map((line, index) => {
    const parts = line.split(/\s+/);
    const baseUrl = parts[0] || "";
    let name = `Provider ${snapshot.providers.length + index + 1}`;
    try { name = new URL(baseUrl).hostname.replace(/^www\./, "").split(".")[0] || name; } catch { /* validation reports invalid URLs */ }
    return {
      name, baseUrl, apiKey: parts[1] || "", enabled: true, priority: snapshot.providers.length + index + 1,
    };
  }).filter((item) => /^https?:\/\//i.test(item.baseUrl) && item.apiKey);
}

function openImportModal(): void {
  const body = `<div class="tabs"><button class="tab active" type="button" data-import-tab="paste">粘贴导入</button><button class="tab" type="button" data-import-tab="file">JSON 文件</button></div>
    <div data-import-panel="paste"><div class="import-format">每行一个中转：API 地址 空格 API Key。兼容 OpenAI Responses 与 Sub2API，模型自动获取。</div><textarea class="textarea" id="import-text" placeholder="https://relay.example/v1 sk-..."></textarea></div>
    <div data-import-panel="file" hidden><div class="import-format">支持 .json 与 .txt 文件。</div><label class="drop-zone" for="import-file"><div><i data-lucide="upload"></i><strong>选择配置文件</strong><span id="file-name">JSON 或文本格式</span></div></label><input id="import-file" type="file" accept=".json,.txt,application/json,text/plain" hidden /></div>`;
  const modal = modalShell("快速导入", body, `<button class="button" data-close>取消</button><button class="button primary" data-import><i data-lucide="import"></i>导入 Provider</button>`, true);
  let fileContent = "";
  modal.querySelectorAll<HTMLElement>("[data-import-tab]").forEach((tab) => tab.addEventListener("click", () => {
    modal.querySelectorAll("[data-import-tab]").forEach((item) => item.classList.toggle("active", item === tab));
    modal.querySelectorAll<HTMLElement>("[data-import-panel]").forEach((panel) => { panel.hidden = panel.dataset.importPanel !== tab.dataset.importTab; });
  }));
  modal.querySelector<HTMLInputElement>("#import-file")!.addEventListener("change", async (event) => {
    const file = (event.target as HTMLInputElement).files?.[0];
    if (!file) return;
    fileContent = await file.text();
    modal.querySelector("#file-name")!.textContent = file.name;
  });
  modal.querySelector<HTMLElement>("[data-import]")!.addEventListener("click", async () => {
    try {
      const raw = fileContent || modal.querySelector<HTMLTextAreaElement>("#import-text")!.value;
      const providers = parseProviders(raw);
      if (!providers.length) throw new Error("没有识别到可导入的 Provider");
      modal.remove();
      await withBusy(async () => { await call("import_providers", { providers }); });
      await reload();
      toast(`已导入 ${providers.length} 个 Provider`);
    } catch (error) {
      busy = false;
      await reload();
      toast(error instanceof Error ? error.message : String(error), "error");
    }
  });
}

async function saveSettings(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  const form = event.currentTarget as HTMLFormElement;
  if (!form.reportValidity()) return;
  const data = new FormData(form);
  const flag = (name: string) => data.get(name) === "true";
  const settings: AppSettings = {
    gatewayPort: Number(data.get("gatewayPort")),
    requestTimeoutSeconds: Number(data.get("requestTimeoutSeconds")),
    connectTimeoutSeconds: Number(data.get("connectTimeoutSeconds")),
    streamIdleTimeoutSeconds: Number(data.get("streamIdleTimeoutSeconds")),
    healthIntervalMinutes: Number(data.get("healthIntervalMinutes")),
    automaticHealthChecks: flag("automaticHealthChecks"),
    autoStartGateway: flag("autoStartGateway"),
    upstreamKeepAlive: flag("upstreamKeepAlive"),
    routeStrategy: String(data.get("routeStrategy")) as RouteStrategy,
    maxFailoverAttempts: Number(data.get("maxFailoverAttempts")),
    cooldownSeconds: Number(data.get("cooldownSeconds")),
    failoverOnClientErrors: flag("failoverOnClientErrors"),
    remapModel: flag("remapModel"),
    skipUnhealthy: flag("skipUnhealthy"),
    localAccessKey: snapshot.settings.localAccessKey,
  };
  const portChanged = settings.gatewayPort !== snapshot.settings.gatewayPort;
  try {
    await call("save_settings", { settings });
    await reload();
    toast(portChanged ? `设置已保存，代理已切换到端口 ${settings.gatewayPort}，记得重新写入 Codex 配置` : "设置已保存并立即生效");
  } catch (error) { toast(String(error), "error"); }
}

/* --------------------------------------------------------------- live state */

/** Everything that changes the rendered markup; excluded fields are patched in place instead. */
function signature(next: AppSnapshot): string {
  return JSON.stringify([
    next.providers.map((provider) => [
      provider.id, provider.name, provider.baseUrl, provider.model, provider.modelOverride, provider.enabled,
      provider.priority, provider.status, provider.latencyMs, provider.lastError, provider.servedCount,
      provider.failedCount, provider.availableModels.length, provider.healthHistory.length, Boolean(provider.cooldownUntil),
    ]),
    [next.gateway.running, next.gateway.port, next.gateway.requestCount, next.gateway.successCount, next.gateway.failedCount,
      next.gateway.failoverCount, next.gateway.activeConnections, next.gateway.averageLatencyMs, next.gateway.inputBytes,
      next.gateway.outputBytes, next.gateway.strategy, next.gateway.routableCount, next.gateway.cooldownCount],
    next.logs.map((log) => log.id),
    next.settings,
    next.codex,
  ]);
}

/** Keeps counters and countdowns ticking without replacing the DOM under the cursor. */
function patchLiveFields(): void {
  document.querySelectorAll<HTMLElement>("[data-live-uptime]").forEach((element) => {
    element.textContent = formatDuration(snapshot.gateway.uptimeSeconds);
  });
  document.querySelectorAll<HTMLElement>("[data-live-rel]").forEach((element) => {
    element.textContent = relativeTime(element.dataset.liveRel || undefined);
  });
  document.querySelectorAll<HTMLElement>("[data-live-cooldown]").forEach((element) => {
    element.textContent = cooldownCopy(element.dataset.liveCooldown);
  });
}

function modalOpen(): boolean {
  return Boolean(document.querySelector(".modal-backdrop"));
}

function isEditing(): boolean {
  const active = document.activeElement;
  return active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement || active instanceof HTMLSelectElement;
}

function startPolling(): void {
  window.setInterval(async () => {
    if (document.hidden || busy || modalOpen() || isEditing()) return;
    try {
      const next = await call<AppSnapshot>("get_snapshot");
      const unchanged = signature(next) === signature(snapshot);
      snapshot = next;
      if (unchanged) patchLiveFields(); else render();
    } catch { /* a transient invoke failure should not stop the loop */ }
  }, POLL_INTERVAL_MS);
}

async function boot(): Promise<void> {
  document.documentElement.dataset.theme = theme;
  // "/" jumps to the search box of the current view, like most dashboards.
  document.addEventListener("keydown", (event) => {
    if (event.key !== "/" || modalOpen() || isEditing()) return;
    const search = document.querySelector<HTMLInputElement>("#provider-search, #session-search");
    if (search) {
      event.preventDefault();
      search.focus();
    }
  });
  try {
    snapshot = await call<AppSnapshot>("get_snapshot");
    render();
    startPolling();
  } catch (error) {
    app.innerHTML = `<div class="empty" style="height:100vh"><div><div class="empty-title">RelayDeck 启动失败</div><div class="empty-copy">${escapeHtml(error)}</div></div></div>`;
  }
}

void boot();
