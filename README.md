# RelayDeck

RelayDeck 是一个面向个人电脑的 Codex 中转管理器。它提供轻量 Tauri 桌面界面、本地 OpenAI 兼容代理、Provider 批量导入、健康检查以及按优先级故障转移。

## 当前能力

### Provider 与保活

- Provider 添加、编辑、删除、启停和拖动排序（上移 / 下移）
- JSON、文本和文件快速导入，导入即并发测活
- 保存后请求 `/v1/models` 获取模型，并向选中模型发送最小 `/v1/responses` 请求验证 API Key
- 后台定时保活：只校验 `/v1/models`，4 个节点一批并发执行，不消耗 token
- 保活结果保留最近 20 次采样，界面上以趋势迷你图呈现
- 连续失败 2 次起进入指数退避冷却（基数 × 2ⁿ，最长 15 分钟），恢复后自动回归路由
- 有节点处于冷却时保活自动加密到 30 秒一次；修改设置会立即唤醒保活循环
- 上游连接复用：TCP keepalive、空闲连接池、`tcp_nodelay`
- 测活与代理使用两套 HTTP 客户端——代理只设置「读空闲超时」，长回答不会被整体超时切断

### 路由策略

- 三种策略：`优先级`（P 序号 + 延迟）、`最快`（保活延迟最低）、`轮询`（可用节点轮转）
- 界面上可一键切换策略，立即生效，无需重启代理或 Codex
- 冷却中和上次检测失败的节点降级为兜底层，只有全部正常节点都失败时才会尝试，网关不会因此直接 503
- 可配置单请求最大切换次数、冷却基数、是否把 401/402/403/404 也视为节点不可用
- 自动对齐模型：目标节点不支持请求里的模型时，按该节点的模型列表改写请求体，请求记录会标记「已对齐模型」
- 可为单个节点锁定模型（手动 pin），锁定优先于自动探测结果

### Codex 接入

- 自动备份并应用 Codex `config.toml`（保留最近 5 份备份）和 `.codex/.env`，保留已有 MCP、沙箱与项目设置
- 写入 `request_max_retries`、`stream_max_retries`、`stream_idle_timeout_ms`，与代理的流空闲预算保持一致
- 检测 Codex 当前指向的端口，端口漂移时在界面顶部提示重新写入
- 可确认后自动重启最新安装的 ChatGPT/Codex；之后切换上游、改动路由策略都不需要再重启

### 会话管理

- 扫描 `~/.codex/sessions` 下的 rollout 文件，按标题、工作目录、体积和更新时间列出全部 Codex 会话
- 对照 `~/.codex/session_index.jsonl` 标记「可继续 / 未索引 / 已归档 / 子代理」，并识别指向已删除文件的失效索引
- Codex Desktop 的会话列表按 `config.toml` 里的 `model_provider` 过滤，切换 Provider 会让全部历史会话从列表里消失；应用 RelayDeck 配置时会自动把 `state_5.sqlite` 里的历史会话归属到 relaydeck（写入前先经 SQLite backup API 备份，保留最近 3 份），历史照常可见
- 一键修复可见性：把历史会话归属到当前 Provider、把未索引的会话加回索引、清掉失效条目，写入前自动备份索引（保留最近 5 份）
- 归档而不是删除：把 rollout 移到 `~/.codex/sessions-archive`，Codex 不再列出，随时可以恢复；只有归档过的会话才允许永久删除
- 每条会话可复制 `codex resume <id>` 命令、复制文件路径或在资源管理器中定位
- 只读取每个文件的前 512 KiB，且不会打开 Codex 正在使用的 `state_5.sqlite` / `logs_2.sqlite`
- 修复、归档和删除会改动 Codex 的会话目录，建议先关闭 Codex 再操作

### 界面与其他

- 仅监听 `127.0.0.1` 的本地 `/v1/*` 代理，流式响应透传
- API Key 保存到操作系统凭据库
- 2 秒实时刷新的控制台：请求量、成功率、故障切换、平均延迟、流量与运行时长
- Provider 搜索、状态筛选与多维排序；深色 / 浅色主题
- 本地访问密钥、自动启动和仅本机访问控制
- 兼容标准 OpenAI Responses 中转与 Sub2API

## 开发

需要 Node.js、Rust 和 Tauri 2 的系统依赖。

```powershell
npm install
npm run tauri dev
```

只预览界面：

```powershell
npm run dev
```

浏览器预览只测试界面和导入解析，不会写 Codex 配置、调用真实 Provider 或重启应用。使用 `npm run tauri dev` 或打包后的 `RelayDeck.exe` 测试完整功能。

手动“测活”会对自动选中的模型发起一次最小请求，可能产生极少量 token 费用；后台定时检查只验证 `/v1/models`，不会周期性消耗 token。


## 导入格式

支持 JSON 数组：

```json
[
  {
    "name": "Primary",
    "baseUrl": "https://relay.example/v1",
    "apiKey": "sk-...",
    "priority": 1
  }
]
```

也支持每行一个 Provider，只需 API 地址和 API Key：

```text
https://relay.example/v1 sk-...
```

## Codex

设置页会按当前代理端口和首选模型生成配置片段，可以只复制，也可以直接写入。

写入时 RelayDeck 会：

1. 备份现有 `~/.codex/config.toml`（只保留最近 5 份备份）
2. 用 `toml_edit` 增量修改 `model`、`model_provider` 和 `[model_providers.relaydeck]`，不动已有的 MCP、沙箱和项目设置
3. 更新 `~/.codex/.env` 与用户环境变量 `RELAYDECK_API_KEY`

只有首次应用、改动监听端口或重置本地访问密钥后需要重启一次 Codex。日常切换上游中转、调整路由策略和模型对齐都在代理内部完成，Codex 侧无感。
