# claude-bus Crate 深度评审

> 评审日期：2026-04-09
> 评审范围：`crates/claude-bus/` 全部源码（3 个文件）

## 架构概览

该 crate 是一个轻量级、零依赖的事件总线层，通过 typed channel 解耦 Agent Core 和 UI 层。

```
┌──────────────────────┐          tokio channels          ┌──────────────────────┐
│  Agent Core           │◄──── mpsc (AgentRequest) ──────│  UI Client            │
│  BusHandle            │──── broadcast (Notification) ──►│  ClientHandle         │
│                       │◄──── broadcast (PermissionReq) ─│                       │
│                       │──── mpsc (PermissionResp) ─────►│                       │
└──────────────────────┘                                  └──────────────────────┘
```

**依赖**：仅 `serde`, `serde_json`, `tokio`, `tracing`, `uuid`, `thiserror` — 无外部 HTTP/网络依赖

### 模块结构

| 模块 | 大小 | 职责 |
|------|------|------|
| `events.rs` | 17.6KB | 事件/请求/响应类型定义 |
| `bus.rs` | 21.3KB | 事件总线实现 |
| `lib.rs` | 1.9KB | 模块导出 |

---

## 优点

### 1. 信道拓扑设计清晰（bus.rs:7-14）

```text
AgentNotification:  Core ──broadcast──→ Client(s)  (1:N, lossy on slow receivers)
AgentRequest:       Client ──mpsc────→ Core        (N:1, backpressure via bounded)
PermissionRequest:  Core ──broadcast──→ Client(s)  (1:N, first responder wins)
PermissionResponse: Client ──mpsc────→ Core        (1:1, paired with request)
```

- 四种信道各有明确的语义：通知是广播（可丢失）、请求是 mpsc（保证送达）、权限请求是广播（多 UI 竞争响应）、权限响应是 mpsc（一对一配对）
- 使用 `broadcast::Sender` 作为 `_notify_tx` 和 `_perm_req_tx` 字段持有者，防止所有发送者丢失时接收端误报 `Closed`

### 2. 类型定义完整且一致（events.rs）

- `AgentNotification` 覆盖完整生命周期：
  - 流式内容：`TextDelta`, `ThinkingDelta`
  - 工具生命周期：`ToolUseStart`, `ToolUseReady`, `ToolUseComplete`
  - 回合生命周期：`TurnStart`, `TurnComplete`, `AssistantMessage`
  - 会话生命周期：`SessionStart`, `SessionEnd`, `SessionSaved`, `SessionStatus`
  - 上下文管理：`ContextWarning`, `CompactStart`, `CompactComplete`
  - 子代理生命周期：`AgentSpawned`, `AgentProgress`, `AgentComplete`
  - MCP 生命周期：`McpServerConnected`, `McpServerDisconnected`, `McpServerError`
  - 记忆系统：`MemoryExtracted`
- `AgentRequest` 涵盖所有用户操作：Submit, Abort, PermissionResponse, Compact, SetModel, SlashCommand, SendAgentMessage, StopAgent, McpConnect/Disconnect/List, Shutdown, SaveSession, GetStatus, ClearHistory, LoadSession, ListModels, ListTools

### 3. 序列化设计面向跨进程（events.rs）

- 所有类型 `#[derive(Serialize, Deserialize)]`，支持 JSON-RPC 序列化
- `#[serde(tag = "type")]` 用于 `AgentNotification`（discriminated union）
- `#[serde(tag = "method", content = "params")]` 用于 `AgentRequest`（JSON-RPC 风格）
- `#[serde(rename_all = "lowercase")]` 用于 `RiskLevel`
- `#[serde(rename_all = "snake_case")]` 用于 `ErrorCode`
- 完整的序列化往返测试覆盖所有变体

### 4. 权限请求超时保护（bus.rs:132-178）

```rust
pub async fn request_permission_with_timeout(
    &mut self,
    tool_name: &str,
    input: serde_json::Value,
    risk_level: RiskLevel,
    description: &str,
    timeout: std::time::Duration,
) -> Option<PermissionResponse>
```

- 默认 30 秒超时
- 超时自动视为拒绝（`None` = deny）
- 不匹配的 `request_id` 会 `tracing::warn!` 记录
- 支持非交互式客户端（RPC/Bridge）场景

### 5. 滞后容忍设计（bus.rs:245-256, 269-279）

```rust
pub async fn recv_notification(&mut self) -> Option<AgentNotification> {
    loop {
        match self.notify_rx.recv().await {
            Ok(event) => return Some(event),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Client lagged by {} notifications, catching up", n);
                continue; // 跳过中间消息，继续接收最新
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}
```

- 广播通道慢速接收者自动跳过中间消息，不会崩溃
- 有 `warn!` 日志帮助诊断性能问题

### 6. 多客户端支持（bus.rs:192-201）

```rust
pub fn new_client(&self) -> ClientHandle
```

- 通过 `subscribe()` 创建额外客户端
- 所有客户端接收通知（广播）
- 所有客户端共享同一个请求通道（mpsc）
- 权限请求广播给所有客户端，第一个响应者获胜

### 7. 测试覆盖全面

| 测试 | 覆盖场景 |
|------|----------|
| `basic_notification_flow` | 基础通知收发 |
| `request_flow` | 请求流转 |
| `permission_request_response` | 权限请求/响应配对 |
| `abort_signal` | 中止信号 |
| `shutdown_signal` | 关闭信号 |
| `multiple_subscribers` | 多订阅者 |
| `disconnected_send_error` | 断开连接错误处理 |
| `submit_convenience` | 便捷方法 |
| `try_recv_empty` / `try_recv_with_pending` | 非阻塞接收 |
| `high_throughput_notifications` | 500 条消息高吞吐 |
| `agent_lifecycle_events` | 子代理生命周期 |
| `permission_request_timeout_auto_denies` | 超时自动拒绝 |
| `permission_response_within_timeout` | 超时内响应 |
| `notification_serialization_roundtrip` | 所有通知变体序列化 |
| `request_serialization_roundtrip` | 所有请求变体序列化 |

### 8. 极简依赖，无外部系统

- 仅依赖 `tokio::sync`（broadcast, mpsc），不需要网络、数据库或文件系统
- 这个 crate 是纯内存中的事件分发层，测试快速、确定性高

---

## 问题与隐患

### P1 — 可能导致消息丢失或 hang

#### 1. `request_permission_with_timeout()` 消费了不匹配的响应（bus.rs:157-167）

```rust
while let Some(resp) = self.perm_resp_rx.recv().await {
    if resp.request_id == request_id {
        return Some(resp);
    }
    tracing::warn!(
        "Received permission response for unknown request: {}",
        resp.request_id
    );
}
```

**问题**：如果有多个并发的权限请求，`perm_resp_rx` 是 mpsc 通道，**这个循环会消费掉所有不匹配的响应**，导致其他权限请求永远收不到响应。

**示例场景**：
1. 并发请求权限 A（request_id = "a"）和权限 B（request_id = "b"）
2. 等待 A 的循环先运行
3. B 的响应先到达，被 A 的循环消费掉并 warn
4. A 等待超时，B 的响应永久丢失

**修复建议**：不应该在循环中消费不匹配的响应。应该使用 `tokio::select!` 或使用独立的 per-request 通道（用 `oneshot::Sender` 配对），而非单通道轮询。

#### 2. `EventBus::new()` 返回 `ClientHandle` 持有 `_notify_tx`，阻止 `Closed` 传播（bus.rs:55）

```rust
let client = ClientHandle {
    notify_rx,
    _notify_tx: notify_tx.clone(),  // 保持发送者存活
    ...
};
```

`ClientHandle` 持有 `_notify_tx` 是为了防止所有发送者丢失时接收端误报 `Closed`。但这意味着即使 `BusHandle` 被 drop，通知通道永远不会 `Closed` —— 接收者会永远等待。

这在 `disconnected_send_error` 测试中暴露了问题（bus.rs:453-462）——测试注释承认了 channel 语义的微妙性。

**影响**：UI 客户端无法可靠检测 Agent Core 断开。

#### 3. `subscribe_requests()` 返回空的 dummy 接收器（bus.rs:209-220）

```rust
pub fn subscribe_requests(&self) -> mpsc::UnboundedReceiver<AgentRequest> {
    let (_tx, rx) = mpsc::unbounded_channel();
    rx  // 永远不会收到任何东西
}
```

这是一个占位实现，**永远无法工作**。注释解释了原因（mpsc 不支持 subscribe），但暴露了一个空函数给调用方是危险的。

**修复建议**：标记为 `#[doc(hidden)]` 或直接删除，或返回 `Option::None` / `Result::Err`。

### P2 — 设计/代码质量问题

#### 4. `AgentNotification` 枚举过大（events.rs:17-166）

50+ 变体的单枚举，违反单一职责原则。应该按领域拆分为：

```rust
pub enum AgentNotification {
    Content(ContentNotification),   // TextDelta, ThinkingDelta
    Tool(ToolNotification),         // ToolUseStart, ToolUseReady, ToolUseComplete
    Turn(TurnNotification),         // TurnStart, TurnComplete, AssistantMessage
    Session(SessionNotification),   // SessionStart, SessionEnd, ...
    Context(ContextNotification),   // ContextWarning, CompactStart, ...
    Agent(AgentNotification_),      // AgentSpawned, AgentProgress, ...
    Mcp(McpNotification),           // McpServerConnected, ...
    Error(ErrorCode, String),
}
```

#### 5. `AgentRequest` 使用 `#[serde(tag = "method", content = "params")]` 但某些变体不需要参数

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum AgentRequest {
    Abort,              // 无参 → 序列化为 {"method":"Abort","params":null}
    Shutdown,           // 同上
    McpListServers,     // 同上
    Submit { text, images },  // 有参 → {"method":"Submit","params":{"text":"...","images":[]}}
    ...
}
```

无参变体的 `params` 会是 `null`。对于 JSON-RPC 来说可以接受，但不如 `#[serde(tag = "method")]` + `#[serde(skip_serializing_if = "Option::is_none")]` 干净。

#### 6. 所有请求/响应都使用 `Clone`

`AgentRequest` 和 `AgentNotification` 都 `#[derive(Clone)]`，在大 payload 场景下（如包含大文件的 `Submit` 或大结果的 `ToolUseComplete`）复制成本高。

**修复建议**：考虑使用 `Arc` 包装大字段，或在不需要 clone 的场景下使用 `Arc<Self>`。

#### 7. `RiskLevel` 只有三个级别，粒度不够

```rust
pub enum RiskLevel {
    Low,
    Medium,
    High,
}
```

对于实际权限检查来说，`rm -rf /` 和 `ls` 都是 `High`，无法区分。应该允许更细粒度的风险评估或与 `PermissionChecker` 的规则系统联动。

#### 8. 缺少背压控制

通知通道使用 `broadcast::channel(capacity)`，慢速接收者会收到 `Lagged` 错误并丢弃消息。对于关键事件（如 `ToolUseComplete`、`TurnComplete`），消息丢失可能导致 UI 状态不一致。

**修复建议**：区分关键事件（保证送达）和非关键事件（可丢弃），或使用两个不同 channel。

#### 9. `SendError` 没有携带原因

```rust
#[derive(Debug, Clone, thiserror::Error)]
#[error("Bus disconnected: the other end has been dropped")]
pub struct SendError;
```

错误类型是单元结构，无法区分是 receiver 端还是 sender 端断开，也无法区分是 request 通道还是 notification 通道。

---

## 代码质量评估

| 维度 | 评分 | 说明 |
|------|------|------|
| API 设计 | ⭐⭐⭐⭐⭐ | 简洁、类型安全、语义清晰 |
| 类型设计 | ⭐⭐⭐⭐ | 事件枚举覆盖全面，但 `AgentNotification` 过大 |
| 测试覆盖 | ⭐⭐⭐⭐⭐ | 所有场景都有测试，包括边界情况 |
| 序列化 | ⭐⭐⭐⭐⭐ | JSON-RPC 兼容，双向测试完整 |
| 代码组织 | ⭐⭐⭐⭐ | 3 个文件，结构清晰 |
| 错误处理 | ⭐⭐⭐ | `SendError` 缺乏上下文，权限请求消费 bug |
| 文档 | ⭐⭐⭐⭐⭐ | 模块级文档 + 架构图 + 代码注释优秀 |
| 性能 | ⭐⭐⭐ | 大 payload Clone 成本高，无背压控制 |

---

## 修复建议汇总

| 优先级 | 问题 | 位置 | 建议 |
|--------|------|------|------|
| P1 | 并发权限请求响应互相消费 | bus.rs:157 | 使用 `oneshot::Sender` 配对或 `tokio::select!` |
| P1 | `ClientHandle` 持有 `_notify_tx` 阻止 Closed 传播 | bus.rs:55 | 考虑在 bus 层添加心跳或显式 ping 检测 |
| P1 | `subscribe_requests()` 返回空 dummy | bus.rs:209 | 标记 `#[doc(hidden)]` 或删除 |
| P2 | `AgentNotification` 枚举过大（50+ 变体） | events.rs:17 | 按领域拆分为子枚举 |
| P2 | `SendError` 缺乏错误上下文 | bus.rs:321 | 添加原因枚举字段 |
| P2 | 关键事件可能被 Lagged 丢弃 | bus.rs:249 | 区分关键/非关键通道 |
| P2 | 大 payload Clone 成本高 | events.rs | 考虑 `Arc` 包装大字段 |
| P3 | `RiskLevel` 粒度过粗 | events.rs:330 | 增加更多级别或允许自定义 |

---

## 总体评价

这是一个**设计精巧、简洁高效的事件总线**，是整个项目中最干净的 crate。它的核心优势在于：

1. **信道拓扑设计正确**：四种信道（广播通知、mpsc 请求、广播权限请求、mpsc 权限响应）各司其职，语义明确
2. **序列化面向 JSON-RPC**：所有类型可直接跨进程序列化，为未来的 RPC 扩展做好准备
3. **测试质量高**：覆盖了正常路径、边界情况、超时、断开连接等场景
4. **依赖极少**：纯 tokio channel 抽象，无外部系统依赖，测试快速且确定性高

主要改进空间在于：
- **并发权限请求的响应消费 bug** 是最严重的缺陷，多任务并发时会导致响应丢失
- `AgentNotification` 枚举可以按领域拆分以提高可维护性
- 缺少关键事件的保证送达机制

总体而言，这是一个生产就绪的高质量 crate，唯一的 P1 bug 需要尽快修复。
