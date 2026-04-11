# clawed-mcp Crate 深度评审

> 评审日期：2026-04-09
> 评审范围：`crates/clawed-mcp/` 全部源码（8 个文件，68.2KB）

## 架构概览

该 crate 实现了 MCP（Model Context Protocol）协议，支持与外部 MCP 服务器的通信。提供两种传输方式（stdio 和 SSE）和多服务器管理。

```
McpManager（registry）
  ├── connect / disconnect / start_all / list_all_tools
  │
  ├── McpClient ─── McpClient ─── McpClient
  │   (protocol)      (protocol)     (protocol)
  │
  ├── StdioTransport          SseTransport
  │   (JSON-RPC/stdio)        (JSON-RPC/SSE/HTTP)
  │
  └── McpBusAdapter ── 桥接到 EventBus
```

### 模块结构

| 模块 | 大小 | 职责 |
|------|------|------|
| `registry.rs` | 14.0KB | 多服务器管理、配置发现、工具名映射 |
| `sse.rs` | 12.5KB | SSE 传输（HTTP + Server-Sent Events） |
| `bus.rs` | 11.4KB | EventBus 适配器 |
| `types.rs` | 10.4KB | MCP 领域类型（工具、资源、内容） |
| `protocol.rs` | 8.7KB | JSON-RPC 2.0 消息类型 |
| `client.rs` | 6.2KB | MCP 客户端（初始化 → 列表 → 调用 → 关闭） |
| `transport.rs` | 5.0KB | Stdio 传输（子进程 JSON-RPC） |
| `lib.rs` | 1.8KB | 模块导出 |

**依赖**：`serde`, `serde_json`, `tokio`, `reqwest`, `futures`, `clawed-bus`

---

## 优点

### 1. 完整的 JSON-RPC 2.0 协议实现（protocol.rs）

- 标准错误码常量（`-32700` 到 `-32603`）
- 自定义反序列化器正确区分 Request/Response/Notification（基于 `result`、`error`、`method`、`id` 字段存在性）
- `JsonRpcMessage` 使用 `#[serde(untagged)]` 序列化 + 手动 `Deserialize`
- 完整的序列化往返测试

### 2. Stdio 传输实现干净（transport.rs）

- 使用 `BufReader`/`BufWriter` 包装子进程 stdin/stdout
- 请求/响应通过 `AtomicU64` ID 匹配
- 通知消息自动跳过（`continue` 循环）
- `Drop` 实现中 `start_kill()` 防止子进程泄漏
- 环境变量传递给子进程

### 3. SSE 传输实现复杂但完整（sse.rs）

- 两步握手：GET SSE 端点 → 接收 `endpoint` 事件 → POST 请求
- 后台 listener 任务路由 SSE 消息到 `oneshot` 通道
- 请求/响应通过 `HashMap<u64, PendingRequest>` 匹配
- 通知消息通过 `mpsc::UnboundedReceiver` 传递
- 30 秒端点超时 + 300 秒请求超时
- URL 解析支持相对路径和绝对路径

### 4. 多服务器管理设计合理（registry.rs）

- 工具名使用 `mcp__<server>__<tool>` 前缀避免冲突
- `parse_mcp_tool_name` 正确处理工具名中的 `__`（如 `mcp__my_server__read__file`）
- 工具列表缓存（`tools_cache`），减少重复调用
- 健康检查清理死亡服务器
- 配置发现支持项目级（`.mcp.json`）和用户级（`~/.claude/.mcp.json`）

### 5. MCP 客户端协议层清晰（client.rs）

- 初始化握手：`initialize` → `notifications/initialized`
- 工具列表缓存 + 失效机制
- 资源列表和读取
- 错误日志记录

### 6. EventBus 集成良好（bus.rs）

- `McpBusAdapter` 桥接 `McpManager` 和 `EventBus`
- 连接/断开/列表操作返回 `AgentNotification`
- 健康检查自动清理死亡服务器
- 完整的异步测试覆盖

### 7. 大输出持久化（types.rs:134-172）

- 超过 100KB 的工具输出写入 `~/.claude/mcp-outputs/`
- UUID 命名避免冲突
- 写入失败优雅降级（`tracing::warn!`）

### 8. 测试覆盖良好

52 个测试，覆盖：
- 协议序列化/反序列化（13 测试）
- 类型解析（12 测试）
- SSE URL 解析（7 测试）
- 注册表管理（8 测试）
- Bus 适配器（10 测试）
- 传输协议（2 测试）

---

## 问题与隐患

### P1 — 可能导致功能异常

#### 1. `McpClient::connect()` 的初始化握手缺少错误处理细节（client.rs:30-62）

```rust
let init_result = transport.request("initialize", Some(json!({ ... }))).await;
let capabilities: ServerCapabilities = serde_json::from_value(
    init_result.get("capabilities").cloned().unwrap_or(Value::Null),
).unwrap_or_default();
```

如果 `initialize` 返回非 JSON 对象，`unwrap_or_default()` 会静默返回空 capabilities，客户端仍认为连接成功。应该检查 `initialize` 响应是否有效。

#### 2. `StdioTransport::request()` 中无限循环等待响应（transport.rs:73-90）

```rust
loop {
    let msg = self.read_message().await?;
    match msg {
        JsonRpcMessage::Response(resp) if resp.id == Some(id) => { ... }
        JsonRpcMessage::Notification(_) => continue,
        _ => continue,
    }
}
```

**没有超时机制**。如果服务器不响应或响应丢失，这个请求会永远阻塞。应该添加 `tokio::time::timeout()` 包装。

#### 3. `SseTransport` 的 listener 任务在流结束时静默终止（sse.rs:115-139）

```rust
let listener_handle = tokio::spawn(async move {
    loop {
        match byte_stream.next().await {
            Some(Err(e)) => { warn!("..."); break; }
            None => { debug!("MCP SSE stream ended"); break; }
            // ...
        }
    }
});
```

listener 任务结束后，所有 pending requests 的 `oneshot::Sender` 会被 drop，但调用方只会收到 "MCP SSE response channel closed" 错误（第 211 行）。应该主动 cancel 所有 pending requests 并返回有意义的错误。

#### 4. `McpBusAdapter::connect()` 获取工具列表时重复调用 `list_all_tools()`（bus.rs:59-69, 111-123）

```rust
let tool_count = self.manager.list_all_tools().await
    .map(|tools| tools.iter()
        .filter(|(prefixed, _)| prefixed.starts_with(&format!("mcp__{name}__")))
        .count())
    .unwrap_or(0);
```

`list_all_tools()` 对所有服务器调用 `list_tools()`，效率低。应该只获取目标服务器的工具列表。

#### 5. `discover_mcp_configs()` 只发现两个固定路径（registry.rs:350-368）

```rust
pub fn discover_mcp_configs(cwd: &std::path::Path) -> Vec<std::path::PathBuf> {
    // Project-level: <cwd>/.mcp.json
    // User-level: ~/.claude/.mcp.json
}
```

TS 实现还搜索 `.claude/mcp.json`（无点前缀）和 `CLAUDE.md` 中的内联配置。缺少这些路径可能导致部分用户的 MCP 服务器无法发现。

### P2 — 设计/代码质量问题

#### 6. `SseTransport` 缺少 `close()` 方法

`SseTransport` 在 `Drop` 时没有显式关闭 HTTP 连接。`reqwest` 的 `Response` 和 `bytes_stream` 会在 drop 时关闭，但没有 graceful shutdown。

#### 7. `StdioTransport::request()` 中 `read_message()` 的 `bytes_read == 0` 判断（transport.rs:109）

```rust
if bytes_read == 0 {
    anyhow::bail!("MCP server closed stdout (EOF)");
}
```

这在所有 pending 请求的场景下会导致它们全部失败。更好的做法是：
1. 标记 transport 为已关闭
2. 取消所有 pending 请求
3. 返回 EOF 错误

#### 8. `McpManager::list_all_tools()` 获取写锁但只读（registry.rs:104）

```rust
pub async fn list_all_tools(&self) -> Result<Vec<(String, McpToolDef)>> {
    let mut servers = self.servers.write().await;  // 应该是 read lock
    // ...
}
```

`list_tools()` 需要 `&mut self`（因为 `McpClient::list_tools` 更新缓存），所以必须写锁。但 `McpClient::list_tools` 应该改为内部可变性（`RefCell` 或 `Mutex`），这样可以并发列出多个服务器的工具。

#### 9. `parse_sse_endpoint_event()` 和 `extract_sse_data()` 的 SSE 解析不完整（sse.rs:256-276）

只处理 `event:` 和 `data:` 字段，不处理：
- `id:` 字段（事件 ID，用于重连）
- `retry:` 字段（重连间隔）
- 多行 `data:` 字段（用换行连接）

对于基本的 MCP SSE 连接可能够用，但不完全符合 SSE 规范。

#### 10. `persist_large_output()` 使用同步文件 I/O（types.rs:159）

```rust
match std::fs::write(&path, &text) { ... }
```

在异步上下文中执行阻塞 I/O。应该使用 `tokio::fs::write()`。

#### 11. `SseTransport::request()` 中 POST 和响应等待是两个独立的步骤（sse.rs:154-217）

POST 请求发送后，响应通过 SSE 流异步返回。如果 POST 成功但 SSE 流中断，调用方会等待 300 秒超时。应该将 POST 响应检查和 SSE 响应等待关联起来。

#### 12. 缺少 MCP 服务器重启机制

`McpManager` 没有自动重启死亡服务器的机制。如果服务器崩溃，需要手动重新连接。

#### 13. `McpClient` 没有实现 `Send` 约束

`StdioTransport` 持有 `Child`、`BufWriter<ChildStdin>`、`BufReader<ChildStdout>`，这些类型都不是 `Send` 的。这意味着 `McpClient` 不能被跨线程传递。

---

## 代码质量评估

| 维度 | 评分 | 说明 |
|------|------|------|
| 协议实现 | ⭐⭐⭐⭐ | JSON-RPC 2.0 完整，但 SSE 解析不完整 |
| 传输设计 | ⭐⭐⭐⭐ | Stdio 和 SSE 两种传输都实现了 |
| 服务器管理 | ⭐⭐⭐⭐ | 多服务器、工具缓存、健康检查 |
| 错误处理 | ⭐⭐⭐ | 缺少请求超时，EOF 处理不够优雅 |
| 测试覆盖 | ⭐⭐⭐⭐ | 52 个测试，覆盖主要路径 |
| 代码组织 | ⭐⭐⭐⭐ | 按职责清晰分离（protocol, types, client, transport, registry） |
| 文档 | ⭐⭐⭐⭐ | 模块级文档和架构图清晰 |
| 性能 | ⭐⭐⭐ | `list_all_tools` 获取写锁，大输出使用同步 I/O |

---

## 修复建议汇总

| 优先级 | 问题 | 位置 | 建议 |
|--------|------|------|------|
| P1 | StdioTransport 请求无超时 | transport.rs:73 | 添加 tokio::time::timeout |
| P1 | SseTransport listener 终止后 pending 请求泄漏 | sse.rs:115 | 主动 cancel 所有 pending requests |
| P1 | McpClient 初始化握手缺少有效性检查 | client.rs:47 | 检查 capabilities 是否有效 |
| P2 | list_all_tools 获取写锁但主要是读操作 | registry.rs:104 | 改用内部可变性或拆分读写 |
| P2 | StdioTransport 无 close 方法 | transport.rs | 添加 graceful shutdown |
| P2 | persist_large_output 使用同步 I/O | types.rs:159 | 改用 tokio::fs |
| P2 | SSE 解析不完整 | sse.rs:256 | 支持 id、retry、多行 data |
| P3 | 缺少 MCP 服务器自动重启 | registry.rs | 添加 restart 机制 |
| P3 | discover_mcp_configs 路径不完整 | registry.rs:350 | 添加更多标准路径 |

---

## 总体评价

这是一个**设计良好、实现完整的 MCP 协议库**。核心优势在于：

1. **两种传输方式**（stdio 和 SSE）都完整实现
2. **JSON-RPC 2.0 协议**正确实现，自定义反序列化器处理消息分发
3. **多服务器管理**设计合理，工具名映射避免冲突
4. **EventBus 集成**良好，生命周期事件正确通知

主要改进空间在于：
- **超时机制缺失** — `StdioTransport::request()` 没有超时，可能永久阻塞
- **SSE listener 终止后的清理** — pending 请求不会被主动 cancel
- **写锁使用不当** — `list_all_tools` 可以优化为读锁

总体而言，这是一个功能完整、代码质量良好的 MCP 实现，生产就绪度较高。
