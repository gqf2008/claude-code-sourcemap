# Claude Code Rust — Architecture

> 非官方研究移植，基于 `@anthropic-ai/claude-code` v2.1.88 TypeScript 源码。

## 项目统计

| 指标 | 数值 |
|------|------|
| Crate 数 | 11 |
| Rust 文件 | 204 |
| 代码行数 | ~69,500 LoC |
| 注册工具 | 28+ (含 MCP 动态代理 + Computer Use) |
| 斜杠命令 | 30+ |
| 测试数 | 2,048 |
| Clippy 警告 | 0 |
| unsafe 块 | 0 |
| Release 二进制 | 19.8 MB, 38ms 启动 |

## 分层架构

```
Layer 3  claude-cli           (30 files, 11,570 LoC,  297 tests)  二进制入口, REPL, 主题, NDJSON 输出
Layer 3  claude-rpc           ( 9 files,  2,251 LoC,   84 tests)  JSON-RPC 外部接口 (TCP/stdio)
Layer 3  claude-bridge        (11 files,  2,087 LoC,   52 tests)  外部消息渠道网关 (飞书/Telegram/Slack)
Layer 2  claude-agent         (38 files, 15,144 LoC,  483 tests)  引擎编排, 会话, Hooks, 权限, 压缩
Layer 2  claude-mcp           ( 8 files,  2,546 LoC,   73 tests)  MCP 注册, 健康监控, 自动重连
Layer 2  claude-swarm         (14 files,  3,134 LoC,   65 tests)  kameo Actor 多 Agent 网络
Layer 2  claude-computer-use  ( 5 files,  1,237 LoC,   16 tests)  Computer Use (截屏/键鼠/内置 MCP)
Layer 1  claude-bus           ( 3 files,  1,198 LoC,   23 tests)  事件总线, ClientHandle, 广播通知
Layer 1  claude-api           (15 files,  6,693 LoC,  180 tests)  HTTP 客户端, 流式 SSE, OAuth PKCE
Layer 1  claude-tools         (41 files, 10,225 LoC,  323 tests)  28+ 工具实现, ToolRegistry, LSP
Layer 0  claude-core          (30 files, 13,431 LoC,  452 tests)  基础类型, Tool trait, 权限, 配置, 文件监听
```

依赖方向: `{cli,rpc,bridge} → agent → {swarm,mcp,computer-use,api,tools,bus} → core`（零循环依赖）

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
        ┌──────────┬───┴───┬──────────┬──────────┐
   ┌────┴───┐ ┌────┴───┐ ┌┴────┐ ┌───┴─────┐ ┌──┴─────┐
   │  CLI   │ │  RPC   │ │ MCP │ │ Bridge  │ │ Swarm  │
   │(REPL)  │ │(TCP)   │ │     │ │(飞书等) │ │(kameo) │
   └────────┘ └────────┘ └─────┘ └─────────┘ └────────┘
```

每个客户端持有独立的 `ClientHandle`，通过 bus 发送 `AgentRequest`（18 种），接收 `AgentNotification` 广播（26 种）。

### 事件总线类型

**AgentRequest（UI → Core，18 种）：**

| 类别 | 请求 |
|------|------|
| 会话控制 | `Submit`, `Abort`, `Compact`, `SetModel`, `ClearHistory`, `Shutdown` |
| 持久化 | `SaveSession`, `LoadSession`, `GetStatus` |
| 权限 | `PermissionResponse` |
| 子 Agent | `SendAgentMessage`, `StopAgent` |
| MCP | `McpConnect`, `McpDisconnect`, `McpListServers` |
| 查询 | `ListModels`, `ListTools`, `SlashCommand` |

**AgentNotification（Core → UI，26 种）：**

| 类别 | 通知 |
|------|------|
| 流式内容 | `TextDelta`, `ThinkingDelta` |
| 工具生命周期 | `ToolUseStart`, `ToolUseReady`, `ToolUseComplete` |
| Turn 生命周期 | `TurnStart`, `TurnComplete`, `AssistantMessage` |
| 会话生命周期 | `SessionStart`, `SessionEnd`, `SessionSaved`, `SessionStatus`, `HistoryCleared`, `ModelChanged` |
| 上下文管理 | `ContextWarning`, `CompactStart`, `CompactComplete` |
| 子 Agent | `AgentSpawned`, `AgentProgress`, `AgentComplete` |
| MCP | `McpServerConnected`, `McpServerDisconnected`, `McpServerError`, `McpServerList` |
| 其他 | `MemoryExtracted`, `ModelList`, `ToolList`, `Error` |

### RPC 方法（17 个）

```
agent.submit       agent.abort        agent.compact      agent.setModel
agent.clearHistory  agent.permission   agent.sendMessage  agent.stopAgent
agent.listModels   agent.listTools    session.save       session.status
session.shutdown   session.load       mcp.connect        mcp.disconnect
mcp.listServers
```

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
| `session.rs` | SessionSnapshot 序列化/反序列化 |
| `skills.rs` | Skill 加载与缓存（带 mutex 毒化恢复） |
| `agents.rs` | Agent 定义加载与缓存（带 mutex 毒化恢复） |
| `text_util.rs` | 文本处理工具 (truncate, collapse_blank_lines) |
| `write_queue.rs` | 异步写入队列（磁盘 I/O 去抖动） |

### claude-api

Anthropic Messages API 客户端。

| 模块 | 职责 |
|------|------|
| `client.rs` | `AnthropicClient` — 同步/流式 API 调用 |
| `streaming.rs` | SSE 流解析器 (content_block_delta, message_stop 等) |
| `types.rs` | API 请求/响应类型 (MessagesRequest, ApiMessage) |
| `oauth.rs` | OAuth PKCE 完整流程 (授权/令牌交换/刷新/本地回调) |
| `cache_detect.rs` | 缓存命中检测与统计（带 mutex 毒化恢复） |
| `usage.rs` | Token 用量累计 |

### claude-tools

28+ 工具实现和 MCP 客户端。

**工具分类：**

| 类别 | 工具 |
|------|------|
| 文件 I/O | `FileRead`, `FileEdit`, `FileWrite`, `MultiEdit`, `Glob`, `Grep`, `ListDir` |
| Shell | `Bash`, `PowerShell`, `REPL` |
| Web | `WebFetch`, `WebSearch` |
| 代码 | `LSP` (6 种操作 + ripgrep fallback), `Notebook` |
| Git | `Git` (status/diff/log/blame), `DiffUI` (syntect 高亮), `Worktree` |
| 交互 | `AskUser`, `SendMessage` |
| Agent | `Task` (子 Agent 派发), `Skill` |
| 管理 | `Todo`, `Config`, `PlanMode`, `Sleep` |

**ToolRegistry** (`lib.rs`): 集中注册所有工具，支持按名称查找、类别过滤、MCP 动态注入。

### claude-agent

核心编排引擎 — 将工具、API、权限、压缩组合为完整的 Agent 循环。

| 模块 | 职责 |
|------|------|
| `engine/mod.rs` | `QueryEngine` — Agent 主循环 (query→dispatch→tool→loop) |
| `engine/builder.rs` | `EngineBuilder` — 构建 engine 并组装 coordinator 管道 |
| `query.rs` | 流式响应处理、token 计数、上下文警告 |
| `executor.rs` | 工具执行器 (权限检查→Hook→执行→结果格式化), 并发 join_all |
| `state.rs` | `SessionState` — 消息历史、会话 I/O、简历恢复 |
| `bus_adapter.rs` | `AgentCoreAdapter` — QueryEngine ↔ EventBus 桥接（含 tool_name 追踪） |
| `traits.rs` | `AgentEngine` trait — 统一接口供 bus/swarm/rpc 调用 |
| `hooks/` | 25 种事件类型、Hook 匹配 (glob/regex 缓存)、shell 执行 |
| `permissions/` | `PermissionChecker` — 规则匹配 + 建议 + cliclack 交互 |
| `compact/` | 会话压缩模块 (全量/微/记忆提取) |
| `system_prompt/` | 系统提示词组装 (18 个 section + 动态边界) |
| `coordinator.rs` | 多 Agent 协调模式 (AgentTracker, dispatch) |
| `dispatch_agent.rs` | 子 Agent 派发 (explore/task/general-purpose)，含 CancelTokenMap/AgentChannelMap |
| `cost.rs` | `CostTracker` — 按模型累计 token/费用 |
| `task_runner.rs` | 后台任务执行器 (NDJSON 流式输出支持) |
| `audit.rs` | 操作审计日志 |

**compact/ 子模块：**
- `mod.rs` — 核心压缩: `compact_conversation()` (调用 Claude 生成摘要), `AutoCompactState` (熔断器)
- `micro.rs` — 微压缩策略: `clear_old_tool_results`, `truncate_large_tool_results`, `snip_old_messages`
- `memory.rs` — 记忆提取: `ExtractedMemory`, `parse_extracted_memories`, `save_extracted_memories`

**system_prompt/ 子模块：**
- `sections.rs` — 18 个提示词 section (identity, guidelines, tasks, actions, tools, tone, ...)
- `mod.rs` — `SystemPrompt` 类型, `DynamicSections` builder, 动态边界分割, 优先级覆盖

### claude-cli

用户入口 — 命令行解析 + REPL 交互循环 + 主题系统。

| 模块 | 职责 |
|------|------|
| `main.rs` | CLI 参数解析 (clap), 模式分发, EventBus 启动, 超时/退出码 |
| `auth.rs` | API key 解析（多 provider）、OAuth 凭据、会话恢复 |
| `init.rs` | `--init` 项目初始化、CLAUDE.md 模板生成、MCP 发现 |
| `repl.rs` | REPL 主循环 — crossterm, 多行输入, Tab 补全, 实时文件监听 |
| `input.rs` | `InputReader` — crossterm 按键处理, Ctrl+R 搜索, Alt+V 粘贴图片 |
| `repl_commands/` | 30+ 斜杠命令处理 (model, compact, diff, review, PR ...) |
| `output/` | 流式渲染: `helpers`(Spinner/格式化), `renderer`(OutputRenderer), `stream`(print_stream) |
| `session.rs` | `SessionManager` (bus 代理) + 权限 handler (cliclack 弹窗) |
| `ui.rs` | cliclack 交互组件 (permission_confirm, model_select, init_wizard) |
| `theme.rs` | 6 主题 (Dark/Light/Daltonized/ANSI), 终端色彩检测 |
| `diff_display.rs` | Diff 可视化 (syntect 语法高亮 + word-level diff) |
| `markdown.rs` | Markdown 渲染 (终端适配, 代码块高亮) |

### claude-bus

进程内事件总线 — 解耦 Agent Core 与 5 个客户端。

| 模块 | 职责 |
|------|------|
| `bus.rs` | `EventBus` + `ClientHandle` — broadcast 通知, mpsc 请求, 权限握手 |
| `events.rs` | `AgentRequest` (18 种) / `AgentNotification` (26 种) 全部事件类型定义 |

### claude-mcp

MCP (Model Context Protocol) 服务器注册与生命周期管理。

| 模块 | 职责 |
|------|------|
| `registry.rs` | `McpManager` — 服务器发现、启停、工具代理 |
| `config.rs` | MCP 服务器配置解析 (stdio/SSE) |
| `bus.rs` | `McpBusAdapter` — MCP ↔ EventBus 桥接 |
| `protocol.rs` | JSON-RPC 2.0 消息类型 |
| `sse.rs` | SSE 传输客户端 |
| `types.rs` | MCP 工具/资源类型定义 |

### claude-rpc

外部 JSON-RPC 接口 — TCP/stdio 供 IDE/脚本调用。

| 模块 | 职责 |
|------|------|
| `server.rs` | TCP/stdio 服务器, 会话管理 |
| `session.rs` | RPC 会话: JSON-RPC ↔ EventBus 双向桥接 |
| `methods.rs` | 17 个方法解析 + 26 种通知序列化 |
| `protocol.rs` | JSON-RPC 2.0 请求/响应/通知类型 |
| `transport/` | `TcpTransport`, `StdioTransport` (可扩展 WebSocket) |
| `error.rs` | RPC 错误码定义 |

### claude-bridge

外部消息渠道网关 — 让 Agent 通过飞书/Telegram/Slack 等平台交互。

| 模块 | 职责 |
|------|------|
| `gateway.rs` | `ChannelGateway` — 适配器生命周期 + Entry::Vacant 消费者管理 |
| `session.rs` | `SessionRouter` — channel→session 映射 |
| `formatter.rs` | `MessageFormatter` — AgentNotification → 用户友好文本 |
| `message.rs` | 渠道消息标准化类型 |
| `config.rs` | 渠道配置 (API token, webhook URL) |
| `adapter.rs` | `ChannelAdapter` trait 定义 |
| `adapters/` | `FeishuAdapter`, `TelegramAdapter`, `SlackAdapter` |
| `webhook.rs` | Webhook 接收骨架（待完善） |

### claude-swarm

多 Agent 协作网络 — 基于 kameo Actor 模型。

| 模块 | 职责 |
|------|------|
| `actor.rs` | `AgentActor` — kameo Actor, 持有 QueryEngine, 处理 `AgentMessage` |
| `swarm.rs` | `SwarmManager` — Actor 网络编排: 注册/启动/停止/路由 |
| `topology.rs` | 拓扑定义: `SwarmTopology`, `AgentRole`, `Link`; 从 YAML 加载 |
| `bus.rs` | `SwarmBusAdapter` — Swarm ↔ EventBus 桥接 |
| `config.rs` | Swarm 配置解析 (agent 定义、路由规则) |
| `types.rs` | `AgentMessage`, `SwarmEvent`, `AgentStatus` 等类型 |

### claude-computer-use

Computer Use 工具 — 屏幕截图 + 鼠标/键盘控制。

| 模块 | 职责 |
|------|------|
| `tool.rs` | `ComputerUseTool` — screenshot/click/type/scroll/key 5 种操作 |
| `bus.rs` | `ComputerUseBusAdapter` — CU ↔ EventBus 桥接 |
| `types.rs` | 操作类型定义 (Action enum, Coordinate, ScreenSize) |

## 核心数据流

```
用户输入 → REPL → ClientHandle.submit()
                       ↓ AgentRequest::Submit (mpsc)
              AgentCoreAdapter.run() 事件循环
                       ↓
              QueryEngine.submit() → AnthropicClient.messages_stream()
                       ↓
              SSE Parser → ContentBlock events (AgentEvent stream)
                       ↓
              ToolUse detected? → PermissionChecker.check()
                       ↓                    ↓
                   Approved          Denied → PermissionRequest via bus → cliclack UI
                       ↓
              Hooks.pre_tool_use() → Executor.run() → Hooks.post_tool_use()
                       ↓
              ToolResult → append to messages → loop (auto-compact if needed)
                       ↓
              StopReason::EndTurn → AgentNotification::TurnComplete (broadcast)
                       ↓
              OutputRenderer 渲染到终端 ← 同时 RPC/Bridge 客户端也收到
```

## 权限系统

```
PermissionMode: Default | AcceptEdits | BypassAll | Plan | DontAsk

Permission check flow:
1. PermissionChecker.check(tool, input)
2. Match against PermissionRule list (allow/deny patterns)
3. If no match → consult mode default
4. Plan mode → allow read-only, deny writes
5. Default → prompt user via cliclack (session.rs PermissionHandler)
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

## 并发与安全

| 特性 | 状态 |
|------|------|
| unsafe 代码 | 0 块 |
| panic! (生产代码) | 0 处 |
| .lock().unwrap() (生产代码) | 0 处 (全部使用 `lock_or_recover` 毒化恢复) |
| TODO/FIXME | 0 处 |
| Clippy 警告 | 0 |
| 死锁风险 | 无 (单任务顺序循环 + 一致锁顺序) |

## 构建与测试

```bash
cd claude-code-rs

# 编译检查
cargo check

# 运行所有测试 (2,048 tests)
cargo test

# 运行特定 crate 测试
cargo test -p claude-agent    # 483 tests
cargo test -p claude-core     # 452 tests
cargo test -p claude-tools    # 323 tests
cargo test -p claude-cli      # 297 tests
cargo test -p claude-api      # 180 tests
cargo test -p claude-rpc      # 84 tests
cargo test -p claude-mcp      # 73 tests
cargo test -p claude-swarm    # 65 tests
cargo test -p claude-bridge   # 52 tests
cargo test -p claude-bus      # 23 tests
cargo test -p claude-computer-use  # 16 tests

# Lint 检查
cargo clippy --workspace

# CI (GitHub Actions)
# .github/workflows/ci.yml — check + test (Linux/Mac/Win) + clippy + fmt

# Release 构建
cargo build --release  # ~19.8 MB, ~38ms 启动
```
