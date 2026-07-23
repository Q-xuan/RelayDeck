# RelayDeck

RelayDeck 是一个面向个人电脑的 Codex 中转管理器。它提供轻量 Tauri 桌面界面、本地 OpenAI 兼容代理、Provider 批量导入、健康检查以及按优先级故障转移。

## 当前能力

- Provider 添加、编辑、删除和启停
- JSON、文本和文件快速导入
- 保存后自动请求 `/v1/models`，获取模型并选择 Codex 默认模型
- 仅监听 `127.0.0.1` 的本地 `/v1/*` 代理
- 连接错误、HTTP 429 和 5xx 自动切换备用节点
- 流式响应透传
- API Key 保存到操作系统凭据库
- 本地请求状态、成功失败、故障切换、延迟与流量统计
- 本地访问密钥、自动启动和仅本机访问控制
- 兼容标准 OpenAI Responses 中转与 Sub2API
- 自动备份并应用 Codex `config.toml`，上游切换无需重复重启 Codex

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

浏览器预览使用空的本地状态；Tauri 桌面运行时使用 Rust 后端和真实代理。

## CI 打包

推送到 `master` 会通过 GitHub Actions 构建 Windows x64 的独立 EXE、NSIS 安装程序和 MSI，并上传为工作流构件。

创建 `v*` 标签会同时发布 GitHub Release：

```powershell
git tag v0.1.0
git push origin v0.1.0
```

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
