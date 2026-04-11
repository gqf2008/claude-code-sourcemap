# clawed-rpc Crate 深度评审

> 评审日期：2026-04-09
> 评审范围：`crates/clawed-rpc/` 全部源码（8 个文件，77.1KB）

## 架构概览

该 crate 实现了 JSON-RPC 2.0 服务器，将 Agent Core 能力暴露给外部客户端（IDE 扩展、Web UI）。支持 stdio 和 TCP 两种传输方式。

```
                          JSON-RPC 2.0
   ┌──────────┐    ┌─────────────────────┐    ┌───────────┐
   │  Client   │───▶│  Transport (stdio/  │───▶│ RpcSession│
   │(IDE/Web)  │◀───│   TCP)              │◀───│           │
   └──────────┘    └─────────────────────┘    └─────┬─────┘
                                                     │ ClientHandle
                                              ┌──────┴──────┐
                                              │  Event Bus   │
                                              └─────────────┘
```

### 模块结构

| 模块 | 大小 | 职责 |
|------|------|------|
| `methods.rs` | 21.9KB | 方法路由：JSON-RPC ↔ AgentRequest/Notification |
| `protocol.rs` | 17.9KB | JSON-RPC 2.0 消息类型 |
| `session.rs` | 12.0KB | 会话管理：传输 ↔ bus 绑定 |
| `server.rs` | 10.7KB | 多传输服务器（stdio/TCP） |
| `transport/tcp.rs` | 6.4KB | TCP 传输实现 |
| `transport/stdio.rs` | 5.1KB | Stdio 传输实现 |
| `transport/mod.rs` | 2.0KB | Transport trait |
| `error.rs` | 618B | 错误类型 |

**依赖**：`clawed-bus`, `clawed-agent`, `serde`, `serde_json`, `tokio`, `tracing`

---

## 优点

### 1. 完整的 JSON-RPC 2.0 协议实现（protocol.rs）

- `RequestId` 支持 Number/String/Null（符合规范）
- `RawMessage` 统一接收 + `classify()` 分发到 Request/Response/Notification
- 标准错误码（-32700 到 -32603）+ 应用级错误码（-32001 到 -32005）
- 双向 `From` 实现：`Request → RawMessage`、`Response → RawMessage`、`Notification → RawMessage`
- 版本检查（拒绝非 "2.0" 的消息）
- 完整的序列化往返测试

### 2. 方法命名约定清晰（methods.rs）

```
agent.submit          agent.setModel        agent.clearHistory
agent.abort           agent.listModels      session.save
agent.compact         agent.listTools       session.status
agent.permission      agent.sendMessage     session.shutdown
agent.stopAgent       session.load
mcp.connect           mcp.disconnect        mcp.listServers
```

17 种方法，命名一致，层次清晰（agent.* / session.* / mcp.*）

### 3. 完整的通知转换（methods.rs:217-360）

所有 31 种 `AgentNotification` 变体都有对应的 JSON-RPC 通知映射。这是最全面的 1:1 映射，确保 IDE 扩展/Web UI 能接收到所有 agent 事件。

### 4. MCP 命令安全验证（methods.rs:181-212）

```rust
const MCP_ALLOWED_COMMANDS: &[&str] = &[
    "npx", "node", "python", "python3", "uvx", "uv",
    "deno", "bun", "cargo", "go", "java",
    "docker", "podman", "mcp-server", "mcp-proxy",
];
// 也允许 mcp-* 前缀
```

防止通过 `mcp.connect` 执行任意命令。白名单 + 前缀匹配双重保护。

### 5. Session 单 select! 循环设计优雅（session.rs:65-137）

```rust
tokio::select! {
    // Inbound: transport → bus
    msg = self.transport.read_message() => { ... }
    // Outbound: bus notifications → transport
    notif = notif_rx.recv() => { ... }
    // Permission requests → transport
    perm = self.client.recv_permission_request() => { ... }
}
```

- 单个 `select!` 循环同时处理入站、出站和权限请求
- 无需 mutex（每个分支操作不同的资源）
- 使用 `subscribe_notifications()` 获取独立的 notification 接收器，避免与 `recv_permission_request()` 竞争 `&mut self`

### 6. TCP 服务器支持多会话 + 认证（server.rs）

- `MAX_TCP_SESSIONS = 64` 连接限制
- 可选 token 认证（`with_auth()`）
- `SessionGuard` RAII 模式确保 session count 正确递减（即使 panic）
- `tokio::select!` 监听 accept 和 shutdown 信号
- 5 秒认证超时

### 7. Transport trait 设计简洁（transport/mod.rs）

```rust
#[async_trait]
pub trait Transport: Send + 'static {
    async fn read_message(&mut self) -> Result<Option<RawMessage>, TransportError>;
    async fn write_message(&mut self, msg: &RawMessage) -> Result<(), TransportError>;
    async fn close(&mut self) -> Result<(), TransportError> { Ok(()) }
}
```

- `TransportError` 枚举覆盖 I/O、JSON、连接关闭、其他错误
- 默认 `close()` 实现简化了无需清理的传输

### 8. Stdio 和 TCP 传输实现一致（transport/stdio.rs, tcp.rs）

- 相同的行格式（`\n` 分隔 JSON）
- 相同的最大消息大小限制（4MB）
- 相同的空行跳过逻辑
- `ChannelTransport` 测试辅助工具实现完美

### 9. 测试覆盖全面

66 个测试，覆盖：
- 协议序列化（20 测试）
- 方法路由（20 测试）
- TCP 传输（4 测试）
- Stdio/Channel 传输（6 测试）
- Session（4 测试）
- Server（6 测试）
- Transport trait（3 测试）
- Error（3 测试）

---

## 问题与隐患

### P1 — 可能导致功能异常

#### 1. `handle_inbound()` 立即返回 `{"ok": true}` 而不等待结果（session.rs:160-166）

```rust
if let Err(e) = self.client.send_request(agent_req) {
    // error response
} else {
    let resp = Response::success(request_id, serde_json::json!({"ok": true}));
    // ...
}
```

**问题**：`send_request()` 只是将请求放入 mpsc 通道，不等待处理完成。客户端收到 `{"ok": true}` 后认为请求已成功处理，但 agent 可能还没开始处理。

**后果**：客户端无法知道请求的实际结果（如 `agent.submit` 的完成状态、错误等）。

**修复建议**：使用 request-response 模式，等待相应的通知或结果。

#### 2. Session 的权限请求处理不等待响应（session.rs:114-135）

```rust
perm = self.client.recv_permission_request() => {
    match perm {
        Some(req) => {
            let notif = Notification::new("agent.permissionRequest", ...);
            // 只发送通知，不等待客户端响应
            if let Err(e) = self.transport.write_message(&RawMessage::from(notif)).await { ... }
        }
        None => { /* 不 break */ }
    }
}
```

权限请求只是通过通知发送，但没有机制等待客户端的 `agent.permission` 响应并将结果传回给 agent core。这意味着权限请求/响应流在 RPC 层是断开的。

**修复建议**：收到权限通知后，等待客户端发送 `agent.permission` 请求，然后通过 bus 发送响应。

#### 3. `authenticate_connection()` 的认证协议设计有误（server.rs:187-226）

```rust
let is_auth = msg.method.as_deref() == Some("auth");
let token = msg.params.as_ref().and_then(|p| p.get("token")).and_then(|v| v.as_str());
```

认证消息使用 `method: "auth"` 而不是 JSON-RPC 标准方法。而且认证成功后，认证消息被消费掉了，不会转发给 session。如果客户端的第一个消息恰好是有效的 JSON-RPC 请求（如 `agent.submit`），认证会失败。

**修复建议**：使用标准的 JSON-RPC 方法名（如 `rpc.auth`），或者在认证完成后重新检查是否有排队消息。

#### 4. TCP `serve_tcp()` 使用随机端口时无法获取实际端口号（server.rs:93）

```rust
pub async fn serve_tcp(&self, addr: &str) -> Result<(), std::io::Error> {
    let listener = TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;  // 获取到了但只用于日志
    info!("TCP server listening on {}", local_addr);
    // ... 没有返回端口号给调用方
```

当使用随机端口（`:0`）时，调用方无法知道实际监听的端口。

**修复建议**：返回实际监听的地址，或提供 `local_addr()` 方法。

### P2 — 设计/代码质量问题

#### 5. `methods.rs` 的参数解析重复代码过多

```rust
// 每种方法都有类似的模式
let p = params.ok_or_else(|| RpcError::new(...))?;
let name = p.get("name").and_then(|v| v.as_str())
    .ok_or_else(|| RpcError::new(...))?.to_string();
```

200+ 行重复的参数提取代码。应该使用宏或 helper 函数。

#### 6. `StdioTransport` 不实现 `close()`（transport/stdio.rs）

`StdioTransport` 使用默认的 `close()` 实现（空操作），但 stdio 的 writer 应该调用 `shutdown()` 以刷新缓冲区。

#### 7. `RpcSession::run()` 中 `tokio::select!` 偏向第一个分支

```rust
tokio::select! {
    msg = self.transport.read_message() => { ... }     // bias: first
    notif = notif_rx.recv() => { ... }                  // checked second
    perm = self.client.recv_permission_request() => { ... } // checked third
}
```

`tokio::select!` 默认是 biased 的（按声明顺序检查）。如果 transport 一直有数据，通知和权限请求可能被延迟。

**修复建议**：添加 `biased;` 标记明确意图，或使用公平模式。

#### 8. `notification_to_jsonrpc()` 中的 `serde_json::json!` 宏调用过多

31 个 match arm 每个都调用 `serde_json::json!` 宏，在热路径上产生大量动态分配。

**修复建议**：使用 `serde::Serialize` 结构体替代 `json!` 宏。

#### 9. `ChannelTransport::pair()` 的通道方向容易混淆（transport/stdio.rs:87-94）

```rust
pub fn pair(capacity: usize) -> (Self, Self) {
    let (tx_a, rx_b) = mpsc::channel(capacity);
    let (tx_b, rx_a) = mpsc::channel(capacity);
    (Self { rx: rx_a, tx: tx_a }, Self { rx: rx_b, tx: tx_b })
}
```

`tx_a` 发给 `rx_b`，`tx_b` 发给 `rx_a`。交叉连接是正确的，但变量命名容易让人混淆。

#### 10. 缺少 WebSocket 传输

lib.rs 文档提到了 WebSocket（"stdio, TCP, WebSocket"），但实际只实现了 stdio 和 TCP。

#### 11. `parse_request()` 不支持 `session.listModels` 和 `session.listTools`（methods.rs:129-130）

```rust
"agent.listModels" => Ok(AgentRequest::ListModels),
"agent.listTools" => Ok(AgentRequest::ListTools),
```

这些方法使用 `agent.*` 前缀而不是 `session.*`。应该统一为 `session.*` 前缀以保持一致性。

#### 12. 缺少批量请求支持

JSON-RPC 2.0 规范支持批量请求（数组），但 `RawMessage` 只处理单个对象。批量请求会被解析失败。

---

## 代码质量评估

| 维度 | 评分 | 说明 |
|------|------|------|
| 协议实现 | ⭐⭐⭐⭐⭐ | 完整 JSON-RPC 2.0，标准错误码 |
| 方法路由 | ⭐⭐⭐⭐ | 17 种方法，但参数解析代码重复 |
| 传输设计 | ⭐⭐⭐⭐ | stdio + TCP，trait 抽象好 |
| Session 设计 | ⭐⭐⭐⭐ | 单 select! 循环优雅，但立即返回 ok 有问题 |
| 安全 | ⭐⭐⭐⭐ | MCP 命令白名单 + TCP token 认证 |
| 测试覆盖 | ⭐⭐⭐⭐⭐ | 66 个测试，覆盖所有路径 |
| 代码组织 | ⭐⭐⭐⭐ | 按职责清晰分离 |
| 文档 | ⭐⭐⭐⭐⭐ | 架构图 + 示例 + wire format 文档 |

---

## 修复建议汇总

| 优先级 | 问题 | 位置 | 建议 |
|--------|------|------|------|
| P1 | handle_inbound 立即返回 ok 不等待结果 | session.rs:160 | 使用 request-response 模式等待实际结果 |
| P1 | 权限请求不等待客户端响应 | session.rs:114 | 实现完整的权限请求/响应流 |
| P1 | 认证协议使用非标准方法名 | server.rs:187 | 使用标准 JSON-RPC 方法名 |
| P2 | 参数解析重复代码过多 | methods.rs | 使用宏或 helper 函数 |
| P2 | select! 偏向第一个分支 | session.rs:65 | 添加 biased 标记或使用公平模式 |
| P2 | notification_to_jsonrpc 过多 json! 调用 | methods.rs:217 | 使用 Serialize 结构体 |
| P2 | StdioTransport 缺少 close 实现 | transport/stdio.rs | 添加 flush/shutdown |
| P3 | 缺少 WebSocket 传输 | lib.rs | 实现或更新文档 |
| P3 | 缺少批量请求支持 | protocol.rs | 支持 JSON-RPC 批量请求 |

---

## 总体评价

这是一个**设计良好的 JSON-RPC 2.0 服务器实现**，核心优势在于：

1. **完整的协议实现** — JSON-RPC 2.0 规范全覆盖，标准错误码 + 应用级错误码
2. **优雅的单 select! 循环** — 无需 mutex 同时处理入站、出站、权限请求
3. **两种传输方式** — stdio（IDE 扩展）和 TCP（守护进程模式）
4. **MCP 安全验证** — 命令白名单防止任意代码执行
5. **66 个测试** — 覆盖协议、方法、传输、会话、服务器

主要改进空间在于：
- **P1**: `handle_inbound()` 立即返回 `{"ok": true}` 而不等待 agent 处理结果 — 这破坏了 request-response 语义
- **P1**: 权限请求/响应流在 RPC 层是断开的
- **P2**: 参数解析有大量重复代码

总体而言，这是一个架构清晰、代码质量良好的 RPC 层，但核心的 request-response 语义需要修复才能达到生产就绪。
