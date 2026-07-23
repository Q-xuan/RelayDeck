# RelayDeck

RelayDeck 是一个面向个人电脑的 Codex 中转管理器。它提供轻量 Tauri 桌面界面、本地 OpenAI 兼容代理、Provider 批量导入、健康检查以及按优先级故障转移。

## 当前能力

- Provider 添加、编辑、删除和启停
- JSON、文本和文件快速导入
- 保存后请求 `/v1/models` 获取模型，并向选中模型发送最小 `/v1/responses` 请求验证 API Key
- 仅监听 `127.0.0.1` 的本地 `/v1/*` 代理
- 连接错误、HTTP 429 和 5xx 自动切换备用节点
- 流式响应透传
- API Key 保存到操作系统凭据库
- 本地请求状态、成功失败、故障切换、延迟与流量统计
- 本地访问密钥、自动启动和仅本机访问控制
- 兼容标准 OpenAI Responses 中转与 Sub2API
- 自动备份并应用 Codex `config.toml` 和 `.codex/.env`
- 可确认后自动重启最新安装的 ChatGPT/Codex；上游切换无需重复重启

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

RelayDeck 设置页会根据当前代理端口和首选模型生成配置片段。当前 MVP 不会自动覆盖用户的 Codex 配置文件，避免破坏已有 MCP、沙箱或项目设置。
