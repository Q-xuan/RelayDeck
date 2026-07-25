import {
  Activity, ArrowDownUp, Check, CircleGauge, Clipboard, Copy, Database, Eye, EyeOff, FileJson, Gauge,
  HardDrive, Import, KeyRound, LayoutDashboard, ListRestart, MoreHorizontal, Network, Pencil, Play,
  Plus, Power, RefreshCw, RotateCcw, Route, Save, Server, Settings, ShieldCheck, Square, Timer,
  Trash2, Upload, X, Zap,
  createIcons,
} from "lucide";
import "./styles.css";
import { call, isDesktop } from "./api";
import type { AppSettings, AppSnapshot, CodexApplyAndRestartResult, CodexApplyResult, Provider, ProviderInput, ProviderStatus, RequestLog } from "./types";

type View = "overview" | "providers" | "activity" | "settings";

const app = document.querySelector<HTMLDivElement>("#app")!;
let snapshot: AppSnapshot;
let activeView: View = "overview";
let busy = false;
let revealAccessKey = false;

const iconSet = {
  Activity, ArrowDownUp, Check, CircleGauge, Clipboard, Copy, Database, Eye, EyeOff, FileJson, Gauge,
  HardDrive, Import, KeyRound, LayoutDashboard, ListRestart, MoreHorizontal, Network, Pencil, Play,
  Plus, Power, RefreshCw, RotateCcw, Route, Save, Server, Settings, ShieldCheck, Square, Timer,
  Trash2, Upload, X, Zap,
};

const escapeHtml = (value: unknown) => String(value ?? "")
  .replaceAll("&", "&amp;")
  .replaceAll("<", "&lt;")
  .replaceAll(">", "&gt;")
  .replaceAll('"', "&quot;")
  .replaceAll("'", "&#039;");

const initials = (name: string) => name.trim().split(/\s+/).map((part) => part[0]).join("").slice(0, 2).toUpperCase();
const enabledProviders = () => snapshot.providers.filter((provider) => provider.enabled).sort((a, b) => a.priority - b.priority);
const healthyProviders = () => snapshot.providers.filter((provider) => provider.enabled && provider.status === "healthy");
const successRate = () => snapshot.gateway.requestCount ? (snapshot.gateway.successCount / snapshot.gateway.requestCount * 100).toFixed(1) : "0.0";
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

function refreshIcons(): void {
  createIcons({ icons: iconSet });
}

function statusLabel(status: ProviderStatus): string {
  return { healthy: "可用", unhealthy: "异常", checking: "检测中", unknown: "未检测" }[status];
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
  item.innerHTML = `<i data-lucide="${kind === "error" ? "x" : "check"}"></i><span>${escapeHtml(message)}</span>`;
  stack.append(item);
  refreshIcons();
  window.setTimeout(() => item.remove(), 2800);
}

function providerRows(providers: Provider[]): string {
  if (!providers.length) return `
    <div class="empty">
      <div><div class="empty-icon"><i data-lucide="server"></i></div>
      <div class="empty-title">还没有 Provider</div>
      <div class="empty-copy">导入或添加一个中转后即可开始测活和路由。</div>
      <button class="button primary" data-action="add-provider"><i data-lucide="plus"></i>添加 Provider</button></div>
    </div>`;

  return providers.map((provider, index) => `
    <div class="provider-row" data-provider-id="${escapeHtml(provider.id)}">
      <div class="provider-main">
        <div class="provider-avatar ${index % 3 === 1 ? "orange" : index % 3 === 2 ? "gray" : ""}">${escapeHtml(initials(provider.name))}</div>
        <div class="provider-copy">
          <div class="provider-name">${escapeHtml(provider.name)}</div>
          <div class="provider-url" title="${escapeHtml(provider.baseUrl)}">${escapeHtml(provider.baseUrl)}</div>
          <div class="provider-tags"><span>Responses</span><span>Bearer</span><span>${provider.availableModels.length || 0} 模型</span></div>
        </div>
      </div>
      <div class="model-name" title="${escapeHtml(provider.availableModels.length ? provider.availableModels.join(", ") : provider.model)}">${escapeHtml(provider.model)}</div>
      <div><span class="status-badge ${provider.status}" title="${escapeHtml(provider.lastError || relativeTime(provider.lastCheckedAt))}">${statusLabel(provider.status)}</span></div>
      <div class="latency">${provider.latencyMs ? `${provider.latencyMs} ms` : "--"}</div>
      <div><span class="priority">P${provider.priority}</span></div>
      <div class="row-actions">
        <button class="switch ${provider.enabled ? "on" : ""}" data-action="toggle-provider" data-id="${escapeHtml(provider.id)}" data-enabled="${!provider.enabled}" title="${provider.enabled ? "停用" : "启用"}" aria-label="${provider.enabled ? "停用" : "启用"}"></button>
        <button class="mini-button" data-action="test-provider" data-id="${escapeHtml(provider.id)}" title="测活" aria-label="测活"><i data-lucide="refresh-cw"></i></button>
        <button class="mini-button" data-action="provider-menu" data-id="${escapeHtml(provider.id)}" title="编辑" aria-label="编辑"><i data-lucide="more-horizontal"></i></button>
      </div>
    </div>`).join("");
}

function providerPanel(providers = snapshot.providers): string {
  return `<section class="panel provider-list">
    <div class="panel-header">
      <div class="panel-title">Provider</div>
      <div class="panel-subtitle">${providers.length} 个节点</div>
      <div class="panel-actions">
        <button class="button" data-action="test-all" ${busy || !providers.length ? "disabled" : ""}><i data-lucide="refresh-cw"></i>全部测活</button>
        <button class="button" data-action="open-import"><i data-lucide="import"></i>导入</button>
        <button class="button primary" data-action="add-provider"><i data-lucide="plus"></i>添加</button>
      </div>
    </div>
    ${providers.length ? `<div class="provider-head"><div>节点</div><div>模型</div><div>状态</div><div>延迟</div><div>路由</div><div></div></div>` : ""}
    ${providerRows(providers)}
  </section>`;
}

function statsPanel(): string {
  const gateway = snapshot.gateway;
  return `<section class="panel">
    <div class="panel-header"><div class="panel-title">总量统计</div><div class="panel-subtitle">本次运行</div><div class="panel-actions"><button class="button" data-action="clear-logs" ${gateway.requestCount ? "" : "disabled"}><i data-lucide="trash-2"></i>清除统计</button></div></div>
    <div class="stats stats-six">
      <div class="stat"><div class="stat-label"><i data-lucide="activity"></i>总请求</div><div class="stat-value">${gateway.requestCount}</div><div class="stat-foot">成功 ${gateway.successCount} / 失败 ${gateway.failedCount}</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="shield-check"></i>成功率</div><div class="stat-value">${successRate()}<span class="stat-unit">%</span></div><div class="stat-foot">${gateway.failedCount ? `${gateway.failedCount} 次失败` : "暂无失败"}</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="route"></i>故障切换</div><div class="stat-value">${gateway.failoverCount}</div><div class="stat-foot">按 Provider 优先级</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="gauge"></i>平均延迟</div><div class="stat-value">${gateway.averageLatencyMs || "--"}<span class="stat-unit">${gateway.averageLatencyMs ? "ms" : ""}</span></div><div class="stat-foot">请求首包响应</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="hard-drive"></i>代理流量</div><div class="stat-value compact">${formatBytes(gateway.outputBytes)}</div><div class="stat-foot">输入 ${formatBytes(gateway.inputBytes)}</div></div>
      <div class="stat"><div class="stat-label"><i data-lucide="timer"></i>运行时长</div><div class="stat-value compact">${formatDuration(gateway.uptimeSeconds)}</div><div class="stat-foot">${gateway.activeConnections} 个活动连接</div></div>
    </div>
  </section>`;
}

function servicePanel(): string {
  const gateway = snapshot.gateway;
  const address = `http://${gateway.host}:${gateway.port}/v1`;
  const key = snapshot.settings.localAccessKey;
  const maskedKey = key.length > 12 ? `${key.slice(0, 9)}${"•".repeat(12)}${key.slice(-5)}` : "••••••••••••";
  return `<section class="panel service-panel">
    <div class="panel-header service-header">
      <div class="service-title-icon"><i data-lucide="database"></i></div><div><div class="panel-title">本地 API 服务</div><div class="panel-subtitle inline">OpenAI Responses Gateway</div></div>
      <div class="service-badges"><span class="service-badge ${gateway.running ? "online" : ""}">${gateway.running ? "运行中" : "已停止"}</span><span class="service-badge">仅本机</span><span class="service-badge ${snapshot.codex.configured ? "online" : ""}">${snapshot.codex.configured ? "Codex 已接入" : "Codex 未接入"}</span></div>
      <div class="panel-actions"><button class="button primary" data-action="apply-and-restart-codex" ${healthyProviders().length ? "" : "disabled"}><i data-lucide="power"></i>应用并重启 Codex</button><button class="button ${gateway.running ? "danger" : ""}" data-action="toggle-gateway"><i data-lucide="${gateway.running ? "square" : "play"}"></i>${gateway.running ? "停止" : "启动"}</button></div>
    </div>
    <div class="service-config-grid">
      <div class="service-config-item"><div class="config-label">服务地址</div><div class="config-value"><code>${address}</code><button class="mini-button" data-action="copy" data-copy="${address}" title="复制地址" aria-label="复制地址"><i data-lucide="copy"></i></button></div></div>
      <div class="service-config-item key-item"><div class="config-label">本地访问密钥</div><div class="config-value"><code>${escapeHtml(revealAccessKey ? key : maskedKey)}</code><button class="mini-button" data-action="toggle-key-visibility" title="${revealAccessKey ? "隐藏" : "显示"}密钥" aria-label="${revealAccessKey ? "隐藏" : "显示"}密钥"><i data-lucide="${revealAccessKey ? "eye-off" : "eye"}"></i></button><button class="mini-button" data-action="copy" data-copy="${escapeHtml(key)}" title="复制密钥" aria-label="复制密钥"><i data-lucide="copy"></i></button><button class="mini-button" data-action="reset-access-key" title="重置密钥" aria-label="重置密钥"><i data-lucide="rotate-ccw"></i></button></div></div>
      <div class="service-config-item"><div class="config-label">服务端口</div><div class="config-value"><strong>${gateway.port}</strong><button class="mini-button" data-view="settings" title="修改端口" aria-label="修改端口"><i data-lucide="settings"></i></button></div></div>
      <div class="service-config-item"><div class="config-label">访问范围</div><div class="config-value"><strong>127.0.0.1</strong><span class="config-note">不向局域网开放</span></div></div>
      <div class="service-config-item"><div class="config-label">路由模式</div><div class="config-value"><strong>优先级故障转移</strong><span class="config-note">${enabledProviders().length} 个节点</span></div></div>
      <div class="service-config-item"><div class="config-label">Codex Provider</div><div class="config-value"><strong>${snapshot.codex.configured ? "relaydeck" : "未配置"}</strong><span class="config-note">${escapeHtml(snapshot.codex.activeModel || enabledProviders()[0]?.model || "等待导入")}</span></div></div>
    </div>
  </section>`;
}

function routePanel(): string {
  const providers = enabledProviders();
  return `<section class="panel">
    <div class="panel-header"><div class="panel-title">路由顺序</div><div class="panel-actions"><button class="mini-button" data-view="providers" title="管理路由" aria-label="管理路由"><i data-lucide="arrow-down-up"></i></button></div></div>
    <div class="route-list">${providers.length ? providers.slice(0, 5).map((provider, index) => `
      <div class="route-item"><span class="route-index">${index + 1}</span><span class="route-name">${escapeHtml(provider.name)}</span><span class="route-state">${provider.status === "healthy" ? `${provider.latencyMs || "--"} ms` : statusLabel(provider.status)}</span></div>`).join("") : `<div class="empty" style="min-height:120px;padding:20px">暂无启用节点</div>`}</div>
  </section>`;
}

function activityItems(logs: RequestLog[], limit?: number): string {
  const items = limit ? logs.slice(0, limit) : logs;
  if (!items.length) return `<div class="empty"><div><div class="empty-icon"><i data-lucide="activity"></i></div><div class="empty-title">暂无请求记录</div></div></div>`;
  return `<div class="activity-list">${items.map((log) => `
    <div class="activity-item">
      <div class="activity-icon ${log.statusCode >= 400 ? "error" : ""}"><i data-lucide="${log.statusCode >= 400 ? "x" : "zap"}"></i></div>
      <div class="activity-main"><div class="activity-title">${escapeHtml(log.method)} ${escapeHtml(log.path)} · ${escapeHtml(log.providerName)}</div><div class="activity-meta">${relativeTime(log.timestamp)} · HTTP ${log.statusCode}${log.attempts > 1 ? ` · ${log.attempts} 次尝试` : ""}</div></div>
      <div class="activity-latency">${log.latencyMs} ms</div>
    </div>`).join("")}</div>`;
}

function recentActivityPanel(): string {
  return `<section class="panel"><div class="panel-header"><div class="panel-title">最近活动</div><div class="panel-actions"><button class="mini-button" data-view="activity" title="全部记录" aria-label="全部记录"><i data-lucide="list-restart"></i></button></div></div>${activityItems(snapshot.logs, 4)}</section>`;
}

function overviewView(): string {
  return `<div class="full-page">${servicePanel()}${statsPanel()}<div class="dashboard-grid"><div class="main-column">${providerPanel()}</div><aside class="side-column">${routePanel()}${recentActivityPanel()}</aside></div></div>`;
}

function providersView(): string {
  return `<div class="full-page">${providerPanel(snapshot.providers)}</div>`;
}

function activityView(): string {
  return `<div class="full-page"><section class="panel"><div class="panel-header"><div class="panel-title">请求记录</div><div class="panel-subtitle">仅保存在本机</div><div class="panel-actions"><button class="button" data-action="clear-logs" ${snapshot.logs.length ? "" : "disabled"}><i data-lucide="trash-2"></i>清空</button></div></div>${activityItems(snapshot.logs)}</section></div>`;
}

function codexSnippet(): string {
  const model = enabledProviders()[0]?.model || "YOUR_MODEL";
  return `model = "${model}"\nmodel_provider = "relaydeck"\n\n[model_providers.relaydeck]\nname = "RelayDeck"\nbase_url = "http://127.0.0.1:${snapshot.settings.gatewayPort}/v1"\nenv_key = "RELAYDECK_API_KEY"\nwire_api = "responses"`;
}

function settingsView(): string {
  const settings = snapshot.settings;
  return `<div class="settings-grid">
    <section class="panel settings-section">
      <div class="settings-title">代理设置</div>
      <form id="settings-form" class="form-grid">
        <div class="field"><label for="gatewayPort">监听端口</label><input class="input" id="gatewayPort" name="gatewayPort" type="number" min="1024" max="65535" value="${settings.gatewayPort}" required /></div>
        <div class="field"><label for="requestTimeoutSeconds">请求超时</label><input class="input" id="requestTimeoutSeconds" name="requestTimeoutSeconds" type="number" min="10" max="600" value="${settings.requestTimeoutSeconds}" required /><div class="field-hint">单位：秒</div></div>
        <div class="field"><label for="healthIntervalMinutes">测活间隔</label><select class="select" id="healthIntervalMinutes" name="healthIntervalMinutes"><option value="2" ${settings.healthIntervalMinutes === 2 ? "selected" : ""}>2 分钟</option><option value="5" ${settings.healthIntervalMinutes === 5 ? "selected" : ""}>5 分钟</option><option value="10" ${settings.healthIntervalMinutes === 10 ? "selected" : ""}>10 分钟</option><option value="30" ${settings.healthIntervalMinutes === 30 ? "selected" : ""}>30 分钟</option></select></div>
        <div class="field"><label for="automaticHealthChecks">自动测活</label><select class="select" id="automaticHealthChecks" name="automaticHealthChecks"><option value="true" ${settings.automaticHealthChecks ? "selected" : ""}>开启</option><option value="false" ${!settings.automaticHealthChecks ? "selected" : ""}>关闭</option></select></div>
        <div class="field"><label for="autoStartGateway">开机启动代理</label><select class="select" id="autoStartGateway" name="autoStartGateway"><option value="true" ${settings.autoStartGateway ? "selected" : ""}>开启（推荐）</option><option value="false" ${!settings.autoStartGateway ? "selected" : ""}>关闭</option></select></div>
        <div class="field"><label>访问范围</label><input class="input" value="127.0.0.1 · 仅本机" disabled /></div>
        <div class="field full"><div class="field-hint">代理只监听 127.0.0.1，不向局域网开放。</div></div>
        <div class="field full"><button class="button primary" type="submit" style="width:fit-content"><i data-lucide="save"></i>保存设置</button></div>
      </form>
    </section>
    <section class="panel settings-section">
      <div class="settings-title">Codex 接入</div>
      <div class="codex-status-row"><span class="status-badge ${snapshot.codex.configured ? "healthy" : "unknown"}">${snapshot.codex.configured ? "已应用" : "未应用"}</span><span>${escapeHtml(snapshot.codex.configPath)}</span></div>
      <div class="code-box"><button class="copy-dark code-copy" data-action="copy" data-copy="${escapeHtml(codexSnippet())}" title="复制配置" aria-label="复制配置"><i data-lucide="copy"></i></button>${escapeHtml(codexSnippet())}</div>
      <div class="settings-actions"><button class="button" data-action="apply-codex" ${healthyProviders().length ? "" : "disabled"}><i data-lucide="save"></i>仅写入配置</button><button class="button primary" data-action="apply-and-restart-codex" ${healthyProviders().length ? "" : "disabled"}><i data-lucide="power"></i>应用并重启 Codex</button></div>
      <div class="field-hint" style="margin-top:10px">会备份现有 config.toml 并写入用户环境变量；首次应用后重启 Codex。之后切换上游不需要重启。</div>
    </section>
  </div>`;
}

const viewTitles: Record<View, [string, string]> = {
  overview: ["控制台", "路由与代理状态"], providers: ["Provider", "中转节点管理"], activity: ["活动", "本地请求记录"], settings: ["设置", "代理与 Codex 配置"],
};

function render(): void {
  const [title, meta] = viewTitles[activeView];
  app.innerHTML = `<div class="app-shell">
    <aside class="sidebar">
      <div class="brand"><div class="brand-mark"></div><div class="brand-name">RelayDeck</div></div>
      <nav class="nav">
        <button class="nav-button ${activeView === "overview" ? "active" : ""}" data-view="overview"><i data-lucide="layout-dashboard"></i><span class="nav-label">控制台</span></button>
        <button class="nav-button ${activeView === "providers" ? "active" : ""}" data-view="providers"><i data-lucide="server"></i><span class="nav-label">Provider</span><span class="nav-count">${snapshot.providers.length}</span></button>
        <button class="nav-button ${activeView === "activity" ? "active" : ""}" data-view="activity"><i data-lucide="activity"></i><span class="nav-label">活动</span></button>
        <button class="nav-button ${activeView === "settings" ? "active" : ""}" data-view="settings"><i data-lucide="settings"></i><span class="nav-label">设置</span></button>
      </nav>
      <div class="sidebar-spacer"></div>
      <div class="sidebar-status"><div class="sidebar-status-top"><span class="sidebar-status-title">Gateway</span><span class="live-dot ${snapshot.gateway.running ? "" : "off"}"></span></div><div class="sidebar-status-copy">${snapshot.gateway.running ? `127.0.0.1:${snapshot.gateway.port}<br>${enabledProviders().length} 个节点参与路由` : "代理服务已停止"}</div></div>
      <div class="version">RelayDeck v0.1.0</div>
    </aside>
    <main class="workspace">
      <header class="topbar"><div class="page-title">${title}</div><div class="page-meta">${meta}</div>${isDesktop() ? "" : `<span class="preview-badge" title="本机操作仅在 RelayDeck 桌面版可用">浏览器预览</span>`}<div class="topbar-actions"><button class="button" data-action="open-import"><i data-lucide="import"></i>快速导入</button><button class="button primary" data-action="add-provider"><i data-lucide="plus"></i>添加 Provider</button></div></header>
      <div class="content">${activeView === "overview" ? overviewView() : activeView === "providers" ? providersView() : activeView === "activity" ? activityView() : settingsView()}</div>
    </main>
  </div>`;
  bindEvents();
  refreshIcons();
}

function bindEvents(): void {
  document.querySelectorAll<HTMLElement>("[data-view]").forEach((element) => element.addEventListener("click", () => {
    activeView = element.dataset.view as View;
    render();
  }));
  document.querySelectorAll<HTMLElement>("[data-action]").forEach((element) => element.addEventListener("click", () => void handleAction(element)));
  document.querySelector<HTMLFormElement>("#settings-form")?.addEventListener("submit", (event) => void saveSettings(event));
}

async function handleAction(element: HTMLElement): Promise<void> {
  const action = element.dataset.action;
  if (action === "add-provider") openProviderModal();
  if (action === "open-import") openImportModal();
  if (action === "copy") await copyText(element.dataset.copy || "");
  if (action === "toggle-provider") await toggleProvider(element.dataset.id!, element.dataset.enabled === "true");
  if (action === "test-provider") await testProvider(element.dataset.id!);
  if (action === "test-all") await testAllProviders();
  if (action === "provider-menu") openProviderActions(element.dataset.id!);
  if (action === "toggle-gateway") await toggleGateway();
  if (action === "clear-logs") await clearLogs();
  if (action === "toggle-key-visibility") { revealAccessKey = !revealAccessKey; render(); }
  if (action === "reset-access-key") await resetAccessKey();
  if (action === "apply-codex") await applyCodexConfig();
  if (action === "apply-and-restart-codex") await applyAndRestartCodex();
}

async function reload(): Promise<void> {
  snapshot = await call<AppSnapshot>("get_snapshot");
  render();
}

async function toggleProvider(id: string, enabled: boolean): Promise<void> {
  await call("toggle_provider", { id, enabled });
  await reload();
  toast(enabled ? "Provider 已加入路由" : "Provider 已停用");
}

async function testProvider(id: string): Promise<void> {
  const current = snapshot.providers.find((provider) => provider.id === id);
  if (!current) return;
  current.status = "checking";
  render();
  try {
    const updated = await call<Provider>("test_provider", { id });
    await reload();
    toast(updated.status === "healthy" ? `${updated.name} 可用 · ${updated.latencyMs} ms` : `${updated.name} 检测失败`, updated.status === "healthy" ? "success" : "error");
  } catch (error) {
    await reload();
    toast(String(error), "error");
  }
}

async function testAllProviders(): Promise<void> {
  busy = true;
  snapshot.providers.filter((provider) => provider.enabled).forEach((provider) => { provider.status = "checking"; });
  render();
  await Promise.allSettled(snapshot.providers.filter((provider) => provider.enabled).map((provider) => call("test_provider", { id: provider.id })));
  busy = false;
  await reload();
  toast("全部节点测活完成");
}

async function toggleGateway(): Promise<void> {
  try {
    await call(snapshot.gateway.running ? "stop_gateway" : "start_gateway");
    await reload();
    toast(snapshot.gateway.running ? "本地代理已启动" : "本地代理已停止");
  } catch (error) { toast(String(error), "error"); }
}

async function clearLogs(): Promise<void> {
  await call("clear_logs");
  await reload();
  toast("请求记录已清空");
}

async function resetAccessKey(): Promise<void> {
  if (!window.confirm("重置后需要重新应用 Codex 配置，确定继续？")) return;
  try {
    await call("reset_access_key");
    revealAccessKey = false;
    await reload();
    toast("本地访问密钥已重置");
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
    toast(`配置已写入，重启 Codex 后生效 · ${result.model}`);
  } catch (error) { toast(String(error), "error"); }
}

async function applyAndRestartCodex(): Promise<void> {
  if (!isDesktop()) {
    toast("浏览器预览不能重启 Codex，请使用 RelayDeck.exe", "error");
    return;
  }
  if (!window.confirm("将备份并应用配置，然后关闭所有 Codex 窗口并重新启动。未保存的 Codex 任务会中断，确定继续？")) return;
  try {
    const result = await call<CodexApplyAndRestartResult>("apply_and_restart_codex");
    await reload();
    const version = result.restart.version ? ` ${result.restart.version}` : "";
    toast(`已应用 ${result.apply.model}，正在启动 ${result.restart.appName}${version}`);
  } catch (error) { toast(String(error), "error"); }
}

async function copyText(value: string): Promise<void> {
  await navigator.clipboard.writeText(value);
  toast("已复制到剪贴板");
}

function modalShell(title: string, body: string, footer: string, wide = false): HTMLDivElement {
  const backdrop = document.createElement("div");
  backdrop.className = "modal-backdrop";
  backdrop.innerHTML = `<div class="modal ${wide ? "wide" : ""}"><div class="modal-header"><div class="modal-title">${escapeHtml(title)}</div><button class="mini-button modal-close" data-close title="关闭" aria-label="关闭"><i data-lucide="x"></i></button></div><div class="modal-body">${body}</div><div class="modal-footer">${footer}</div></div>`;
  backdrop.addEventListener("mousedown", (event) => { if (event.target === backdrop) backdrop.remove(); });
  backdrop.querySelectorAll<HTMLElement>("[data-close]").forEach((item) => item.addEventListener("click", () => backdrop.remove()));
  document.body.append(backdrop);
  refreshIcons();
  return backdrop;
}

function openProviderModal(provider?: Provider): void {
  const form = `<form id="provider-form" class="form-grid">
    <div class="field full"><label for="name">名称</label><input class="input" id="name" name="name" value="${escapeHtml(provider?.name || "")}" placeholder="例如：主力线路" required /></div>
    <div class="field full"><label for="baseUrl">API 地址</label><input class="input" id="baseUrl" name="baseUrl" type="url" value="${escapeHtml(provider?.baseUrl || "")}" placeholder="https://example.com/v1" required /></div>
    <div class="field full"><label for="apiKey">API Key</label><input class="input" id="apiKey" name="apiKey" type="password" autocomplete="off" placeholder="${provider?.hasKey ? "留空以保留当前密钥" : "sk-..."}" ${provider?.hasKey ? "" : "required"} /><div class="field-hint">保存后会获取模型，并向选中模型发送一次最小请求验证 API Key；可能产生极少量 token 费用</div></div>
    <div class="field"><label for="priority">路由优先级</label><input class="input" id="priority" name="priority" type="number" min="1" max="99" value="${provider?.priority || snapshot.providers.length + 1}" required /></div>
    <div class="field"><label for="enabled">加入路由</label><select class="select" id="enabled" name="enabled"><option value="true" ${provider?.enabled !== false ? "selected" : ""}>启用</option><option value="false" ${provider?.enabled === false ? "selected" : ""}>停用</option></select></div>
  </form>`;
  const modal = modalShell(provider ? "编辑 Provider" : "添加 Provider", form, `<button class="button" data-close>取消</button><button class="button primary" data-save><i data-lucide="save"></i>保存</button>`);
  modal.querySelector<HTMLElement>("[data-save]")!.addEventListener("click", () => void saveProvider(modal, provider));
}

async function saveProvider(modal: HTMLElement, provider?: Provider): Promise<void> {
  const form = modal.querySelector<HTMLFormElement>("#provider-form")!;
  if (!form.reportValidity()) return;
  const data = new FormData(form);
  const input: ProviderInput = {
    id: provider?.id,
    name: String(data.get("name")), baseUrl: String(data.get("baseUrl")), apiKey: String(data.get("apiKey")) || undefined,
    model: provider?.model, enabled: data.get("enabled") === "true", priority: Number(data.get("priority")),
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
  const modal = modalShell(provider.name, `<div class="proxy-metric"><span>API 地址</span><strong>${escapeHtml(provider.baseUrl)}</strong></div><div class="proxy-metric"><span>自动模型</span><strong>${escapeHtml(provider.model)}</strong></div><div class="proxy-metric"><span>可用模型</span><strong>${provider.availableModels.length || "--"}</strong></div><div class="proxy-metric"><span>路由优先级</span><strong>P${provider.priority}</strong></div>`, `<button class="button danger" data-delete><i data-lucide="trash-2"></i>删除</button><button class="button" data-edit><i data-lucide="pencil"></i>编辑</button><button class="button primary" data-test><i data-lucide="refresh-cw"></i>重新获取模型</button>`);
  modal.querySelector<HTMLElement>("[data-edit]")!.addEventListener("click", () => { modal.remove(); openProviderModal(provider); });
  modal.querySelector<HTMLElement>("[data-test]")!.addEventListener("click", () => { modal.remove(); void testProvider(id); });
  modal.querySelector<HTMLElement>("[data-delete]")!.addEventListener("click", () => void deleteProvider(modal, id));
}

async function deleteProvider(modal: HTMLElement, id: string): Promise<void> {
  if (!window.confirm("确定删除这个 Provider？")) return;
  await call("delete_provider", { id });
  modal.remove();
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
      await call("import_providers", { providers });
      modal.remove();
      await reload();
      toast(`已导入 ${providers.length} 个 Provider`);
    } catch (error) { toast(error instanceof Error ? error.message : String(error), "error"); }
  });
}

async function saveSettings(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  const form = event.currentTarget as HTMLFormElement;
  const data = new FormData(form);
  const settings: AppSettings = {
    gatewayPort: Number(data.get("gatewayPort")), requestTimeoutSeconds: Number(data.get("requestTimeoutSeconds")),
    healthIntervalMinutes: Number(data.get("healthIntervalMinutes")), automaticHealthChecks: data.get("automaticHealthChecks") === "true",
    autoStartGateway: data.get("autoStartGateway") === "true", localAccessKey: snapshot.settings.localAccessKey,
  };
  try {
    await call("save_settings", { settings });
    await reload();
    toast("设置已保存，重启代理后生效");
  } catch (error) { toast(String(error), "error"); }
}

async function boot(): Promise<void> {
  try {
    snapshot = await call<AppSnapshot>("get_snapshot");
    render();
  } catch (error) {
    app.innerHTML = `<div class="empty" style="height:100vh"><div><div class="empty-title">RelayDeck 启动失败</div><div class="empty-copy">${escapeHtml(error)}</div></div></div>`;
  }
}

void boot();
