# Claude Code Rust — Architecture

> 非官方研究移植，基于 `@anthropic-ai/claude-code` v2.1.88 TypeScript 源码。

## 项目统计

| 指标 | 数值 |
|------|------|
| Crate 数 | 9 |
| Rust 文件 | 157 |
| 代码行数 | ~50,500 LoC |
| 注册工具 | 28+ (含 MCP 动态代理) |
| 斜杠命令 | 30+ |
| 测试数 | 1,560+ |

## 分层架构

```
Layer 3  claude-cli      (21 files,  7,342 LoC)  二进制入口, REPL, cliclack UI
Layer 3  claude-rpc      ( 9 files,  1,683 LoC)  JSON-RPC 外部接口 (HTTP/WebSocket)
Layer 3  claude-bridge   (11 files,  1,976 LoC)  外部消息渠道网关 (Feishu/Telegram/Slack)
Layer 2  claude-agent    (34 files, 12,121 LoC)  引擎编排, 会话, Hooks, 权限, 压缩
Layer 2  claude-mcp      ( 8 files,  1,791 LoC)  MCP 服务器注册与生命周期管理
Layer 1  claude-bus      ( 3 files,    903 LoC)  事件总线, ClientHandle, 广播通知
Layer 1  claude-api      (15 files,  6,489 LoC)  HTTP 客户端, 流式 SSE, OAuth PKCE
Layer 1  claude-tools    (32 files,  7,613 LoC)  28+ 工具实现, ToolRegistry
Layer 0  claude-core     (24 files, 10,659 LoC)  基础类型, Tool trait, 权限模型, 配置
```

依赖方向: `{cli,rpc,bridge} → agent → {api,tools,mcp,bus} → core`（零循环依赖）

## 4-Client Event Bus 架构

```
                  ┌─────────┐
                  │  Agent   │ ← claude-agent (QueryEngine + ToolExecutor)
                  │   Core   │
                  └────┬─────┘
                       │ AgentCoreAdapter
                  ┌────┴─────┐
                  │ EventBus │ ← claude-bus (broadcast notifications, mpsc requests)
                  └────┬─────┘
        ┌──────────┬───┴───┬──────────┐
   ┌────┴───┐ ┌────┴───┐ ┌┴────┐ ┌───┴─────┐
   │  CLI   │ │  RPC   │ │ MCP │ │ Bridge  │
   │(REPL)  │ │(HTTP)  │ │     │ │(飞书等) │
   └────────┘ └────────┘ └─────┘ └─────────┘
```

每个客户端持有独立的 `ClientHandle`，通过 bus 发送 `AgentRequest`，接收 `AgentNotification` 广播。

## Crate 详解

### claude-core

基础类型和 trait 定义层，无外部 HTTP 依赖。

| 模块 | 职责 |
|------|------|
| `tool.rs` | `Tool` trait — 所有工具的统一接口 |
| `message.rs` | 会话消息类型 (User/Assistant/System, ContentBlock) |
| `permission.rs` | 权限规则引擎 (PermissionRule, PermissionMode) |
| `config.rs` | 全局配置 + CLAUDE.md 解析 |
| `memory.rs` | 用户记忆目录管理 |
| `model.rs` | 模型元信息 (display_name, knowledge_cutoff, pricing) |
| `text_util.rs` | 文本处理工具 (truncate, collapse_blank_lines) |

### claude-api

Anthropic Messages API 客户端。

| 模块 | 职责 |
|------|------|
| `client.rs` | `AnthropicClient` — 同步/流式 API 调用 |
| `streaming.rs` | SSE 流解析器 (content_block_delta, message_stop 等) |
| `types.rs` | API 请求/响应类型 (MessagesRequest, ApiMessage) |
| `oauth.rs` | OAuth PKCE 完整流程 (授权/令牌交换/刷新/本地回调) |

### claude-tools

28+ 工具实现和 MCP 客户端。

**工具分类：**

| 类别 | 工具 |
|------|------|
| 文件 I/O | `FileRead`, `FileEdit`, `FileWrite`, `MultiEdit`, `Glob`, `Grep`, `ListDir` |
| Shell | `Bash`, `PowerShell`, `REPL` |
| Web | `WebFetch`, `WebSearch` |
| 代码 | `LSP` (6 种操作 + ripgrep fallback), `Notebook` |
| Git | `Git` (status/diff/log/blame), `DiffUI`, `Worktree` |
| 交互 | `AskUser`, `SendMessage` |
| Agent | `Task` (子 Agent 派发), `Skill` |
| 管理 | `Todo`, `Config`, `PlanMode`, `Sleep` |
| MCP | `mcp/client.rs` (JSON-RPC stdio), `mcp/server.rs` (配置加载), `mcp/proxy.rs` (动态代理工具) |

**ToolRegistry** (`lib.rs`): 集中注册所有工具，支持按名称查找、类别过滤、MCP 动态注入。

### claude-agent

核心编排引擎 — 将工具、API、权限、压缩组合为完整的 Agent 循环。

| 模块 | 职责 |
|------|------|
| `engine.rs` | `QueryEngine` — Agent 主循环 (query→dispatch→tool→loop) |
| `query.rs` | 流式响应处理、token 计数、上下文警告 |
| `executor.rs` | 工具执行器 (权限检查→Hook→执行→结果格式化) |
| `state.rs` | `SessionState` — 消息历史、会话 I/O、简历恢复 |
| `hooks.rs` | 25 种事件类型、Hook 匹配 (glob/regex 缓存)、shell 执行 |
| `permissions.rs` | `PermissionChecker` — 规则匹配 + 建议 + 终端提示 |
| `compact/` | 会话压缩模块 (见下) |
| `system_prompt/` | 系统提示词组装 (见下) |
| `coordinator.rs` | 多 Agent 协调模式 (AgentTracker, dispatch) |
| `dispatch_agent.rs` | 子 Agent 派发 (explore/task/general-purpose) |
| `cost.rs` | `CostTracker` — 按模型累计 token/费用 |
| `task_runner.rs` | 后台任务执行器 |

**compact/ 子模块：**
- `mod.rs` — 核心压缩: `compact_conversation()` (调用 Claude 生成摘要), `AutoCompactState` (熔断器)
- `micro.rs` — 微压缩策略: `clear_old_tool_results`, `truncate_large_tool_results`, `snip_old_messages`
- `memory.rs` — 记忆提取: `ExtractedMemory`, `parse_extracted_memories`, `save_extracted_memories`

**system_prompt/ 子模块：**
- `sections.rs` — 18 个提示词 section (identity, guidelines, tasks, actions, tools, tone, ...)
- `mod.rs` — `SystemPrompt` 类型, `DynamicSections` builder, 动态边界分割, 优先级覆盖

### claude-cli

用户入口 — 命令行解析 + REPL 交互循环 + cliclack UI。

| 模块 | 职责 |
|------|------|
| `main.rs` | CLI 参数解析 (clap), 初始化, 模式分发, EventBus 启动 |
| `repl.rs` | REPL 主循环 — rustyline, 多行输入, Tab 补全, 自动压缩检查 |
| `repl_commands/` | 30+ 斜杠命令处理 (model, compact, diff, review, PR ...) |
| `output.rs` | 终端输出格式化 (Markdown 渲染, 颜色, 进度) |
| `session.rs` | SessionManager (bus 代理) + 权限 handler (cliclack 弹窗) |
| `ui.rs` | cliclack 交互组件 (permission_confirm, model_select, init_wizard) |

### claude-bus

进程内事件总线 — 解耦 Agent Core 与 4 个客户端。

| 模块 | 职责 |
|------|------|
| `bus.rs` | `EventBus` + `ClientHandle` — broadcast 通知, mpsc 请求, 权限握手 |
| `events.rs` | `AgentRequest` / `AgentNotification` 全部事件类型定义 |

### claude-mcp

MCP (Model Context Protocol) 服务器注册与生命周期管理。

| 模块 | 职责 |
|------|------|
| `registry.rs` | `McpManager` — 服务器发现、启停、工具代理 |
| `config.rs` | MCP 服务器配置解析 (stdio/SSE) |
| `bus_adapter.rs` | `McpBusAdapter` — MCP ↔ EventBus 桥接 |
| `transport.rs` | JSON-RPC stdio/SSE 传输层 |

### claude-rpc

外部 JSON-RPC 接口 — HTTP/WebSocket 供 IDE/脚本调用。

| 模块 | 职责 |
|------|------|
| `server.rs` | Axum HTTP/WS 服务器, 路由定义 |
| `handler.rs` | JSON-RPC 方法处理 (submit, abort, status, history ...) |
| `protocol.rs` | RPC 请求/响应/通知类型 |
| `bus_adapter.rs` | RPC ↔ EventBus 桥接 |

### claude-bridge

外部消息渠道网关 — 让 Agent 通过飞书/Telegram/Slack 等平台交互。

| 模块 | 职责 |
|------|------|
| `gateway.rs` | `ChannelGateway` — 适配器生命周期 + 通知消费循环 |
| `session.rs` | `SessionRouter` — channel→session 映射 |
| `formatter.rs` | `MessageFormatter` — AgentNotification → 用户友好文本 |
| `adapters/` | `FeishuAdapter`, `TelegramAdapter`, `SlackAdapter` (trait `ChannelAdapter`) |

## 核心数据流

```
用户输入 → REPL → QueryEngine.query_stream()
                      ↓
              AnthropicClient.messages_stream()
                      ↓
              SSE Parser → ContentBlock events
                      ↓
              ToolUse detected? → PermissionChecker.check()
                      ↓                    ↓
                  Approved          Denied → ask user
                      ↓
              Hooks.pre_tool_use() → Executor.run() → Hooks.post_tool_use()
                      ↓
              ToolResult → append to messages → loop
                      ↓
              StopReason::EndTurn → display response → REPL
```

## 权限系统

```
PermissionMode: Default | AcceptEdits | BypassAll | Plan | DontAsk

Permission check flow:
1. PermissionChecker.check(tool, input)
2. Match against PermissionRule list (allow/deny patterns)
3. If no match → consult mode default
4. Plan mode → allow read-only, deny writes
5. Default → prompt user in terminal
```

## Hook 系统

25 种事件类型，通过 CLAUDE.md 中 `hooks:` 配置或 `~/.claude/hooks.json` 加载:

```
PreToolUse(tool_name)  → 工具执行前
PostToolUse(tool_name) → 工具执行后
UserPromptSubmit       → 用户提交输入
SessionStart/End       → 会话生命周期
Compact                → 压缩事件
...
```

匹配器支持 glob 模式 + regex 缓存, shell 命令执行返回 stdout 作为反馈。

## 构建与测试

```bash
cd claude-code-rs

# 编译检查
cargo check

# 运行所有测试
cargo test

# 运行特定 crate 测试
cargo test -p claude-agent
cargo test -p claude-tools

# 运行特定测试
cargo test test_auto_compact
```
