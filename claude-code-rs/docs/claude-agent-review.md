# clawed-agent Crate 深度评审

> 评审日期：2026-04-09
> 评审范围：`crates/clawed-agent/` 全部源码

## 架构概览

该 crate 采用清晰的分层架构：

```
QueryEngine（编排器）
  ├── query_stream() ── 代理循环（async_stream）
  ├── ToolExecutor ──── 工具执行 + 权限 + 钩子
  ├── SharedState ──── 并发状态（RwLock<AppState>）
  ├── HookRegistry ─── 生命周期钩子
  └── Coordinator ──── 多代理编排
```

**依赖流向**：`engine → query → executor → {permissions, hooks, tools}`

### 模块结构

| 模块 | 文件数 | 大小 | 职责 |
|------|--------|------|------|
| `engine/` | 3 | ~55KB | QueryEngine 主类 + Builder |
| `query/` | 4 | ~50KB | 代理循环 + 流处理 |
| `executor.rs` | 1 | 22.9KB | 工具执行 + 权限检查 |
| `state.rs` | 1 | 14.0KB | 状态管理 + 会话快照 |
| `coordinator.rs` | 1 | 20.4KB | 多代理编排 |
| `dispatch_agent.rs` | 1 | 29.0KB | 子代理调度 |
| `bus_adapter.rs` | 1 | 28.5KB | 事件路由适配 |
| `memory_extractor.rs` | 1 | 33.0KB | 记忆提取 |
| `hooks/` | 4 | ~40KB | 钩子系统 |
| `permissions/` | 4 | ~28KB | 权限控制 |
| `compact/` | ? | ? | 上下文压缩 |
| `system_prompt/` | ? | ? | 系统提示词构建 |
| `plugin/` | ? | ? | 插件系统 |

---

## 优点

### 1. 清晰的 QueryEngineBuilder 模式（builder.rs）

- Fluent API，合理的默认值（max_turns=100, max_tokens=16384）
- 所有配置路径显式化 — coordinator 模式、allowed_tools、thinking 配置
- 上下文窗口处理带环境变量覆盖（`CLAUDE_CODE_MAX_CONTEXT_TOKENS`、`CLAUDE_CODE_AUTO_COMPACT_WINDOW`）设计良好

### 2. 健壮的代理循环（query/mod.rs）

- 多轮循环，正确执行 `max_turns` 限制
- `MaxTokens` 恢复策略：先升级到模型上限 + 多轮续传（最多 3 次）
- API 错误时自动压缩（截断 tool result、裁剪旧消息）
- 带抖动的指数退避重试
- 流空闲 watchdog 集成
- `consecutive_errors` 追踪 + 成功后重置

### 3. 智能工具并行化（executor.rs:252-293）

- `partition_tool_calls()` 将连续的安全/不安全工具分组
- 只读工具并行执行（上限 `MAX_TOOL_CONCURRENCY=10`）
- 写工具顺序执行，批次按顺序运行
- 这是一个非平凡的优化，实现正确

### 4. 钩子系统设计良好

- `HookDecision` 枚举（Block, ModifyInput, AppendContext, FeedbackAndContinue）覆盖所有需要的场景
- 钩子在正确的生命周期点触发：
  - `SessionStart` — 会话开始
  - `PreToolUse` / `PostToolUse` — 工具执行前后
  - `PostSampling` — 模型响应后、工具执行前
  - `Stop` — 停止时
  - `PreCompact` / `PostCompact` — 压缩前后
  - `PermissionDenied` / `PermissionRequest` — 权限相关
  - `PostToolUseFailure` — 工具执行失败

### 5. Coordinator 模式（coordinator.rs）

- 干净的 `AgentTracker` 带 `AgentStatus` 状态机（Running → Completed/Failed/Killed）
- 基于 XML 的通知协议，与 TS 实现对齐
- `SendMessageTool` 和 `TaskStopTool` 用于代理间通信
- `worker_tool_names()` 从 worker 中排除 coordinator 专用工具

### 6. 状态管理（state.rs）

- `AppState` 有全面的追踪：每个模型的用量、错误计数、行数变更、耗时指标
- `record_usage_auto_cost()` 根据模型定价自动计算费用
- 会话快照/恢复实现良好，带消息清洗

### 7. 良好的测试覆盖

- 128+ 测试，含单元测试和 e2e 测试
- 测试覆盖关键路径：工具执行、权限检查、消息配对、工具分组

---

## 问题与隐患

### P0 — 可能导致 Panic 或数据错误

#### 1. `pop_last_turn()` 逻辑错误（engine/mod.rs:524-553）

```rust
let mut removed_assistant = false;
while let Some(last) = s.messages.last() {
    match last {
        Message::Assistant(_) if !removed_assistant => { ... }
        Message::User(_) if removed_assistant => { break; }  // BUG
        _ if removed_assistant => { break; }
        _ => { s.messages.pop(); }
    }
}
```

**问题**：tool result 也是 `Message::User` 类型。当 `removed_assistant = true` 后，第一个遇到的 User 消息（实际是 tool result）会直接 `break`，导致真正的用户 prompt 没有被弹出。

**后果**：`/retry` 命令会发送错误的消息，可能导致重复执行工具。

**修复建议**：需要区分 tool result 类型的 User 消息和普通 User 消息。检查 User message 的内容块是否只包含 `ToolResult` 类型。

#### 2. `coordinator.rs:360` 的 unwrap（TOCTOU 竞态）

```rust
let task = self.tracker.get(&agent_id).await.unwrap();
```

在 `SendMessageTool::call()` 中，前面已经通过 `tracker.get()` 检查过 agent 存在，但这里再次调用 `get()` 并 `unwrap()` — 中间可能已被其他任务删除，存在 panic 风险。

**修复建议**：用 `ok_or_else` 替代 `unwrap()`。

### P1 — 可能导致功能异常或 hang

#### 3. 权限提示无超时（executor.rs:180-182）

```rust
let response = tokio::task::spawn_blocking(move || {
    PermissionChecker::prompt_user(&tn, &desc, &suggestions)
}).await.unwrap_or_else(|_| PermissionResponse::deny());
```

`PermissionChecker::prompt_user` 是阻塞 TUI — 如果用户永远不响应，整个 executor 会被阻塞，**没有超时机制**。

**修复建议**：添加超时或可取消机制。

#### 4. `build()` 是同步的，但执行了阻塞 I/O（builder.rs:180-401）

- `load_claude_md(&self.cwd)` 和 `load_memories_for_prompt(&self.cwd)` 是阻塞文件 I/O
- 直接返回 `QueryEngine` — 没有文件加载失败的错误路径

**修复建议**：改为 `async fn build(self) -> anyhow::Result<QueryEngine>` 或接受预加载的内容。

#### 5. 自动压缩熔断器未连线

`AutoCompactState` 存在（熔断器模式），但在 `query_stream` 的错误处理路径中看不到 `record_compact_failure()` 的调用。如果压缩失败，熔断器永远不会触发，每轮都会继续尝试压缩。

**修复建议**：在 query_stream 的压缩错误处理中调用 `engine.record_compact_failure()`。

#### 6. `should_auto_compact()` 双路径容易混淆（engine/mod.rs:449-466）

- `compact_threshold == 0` 时直接返回 `false`
- 但 `context_window` 模式仍通过 `AutoCompactState` 工作
- 阈值模式和百分比模式并存，容易出错

**修复建议**：统一为单一路径，优先使用百分比模式。

### P2 — 性能优化建议

#### 7. `execute()` 多次克隆输入（executor.rs:66, 91）

```rust
let mut actual_input = input.clone();  // 第66行
// ...钩子可能修改...
let result = self.execute_inner(..., actual_input.clone(), ...).await;  // 第91行
```

第 66 行的 `input.clone()` 可以避免 — 只有在钩子实际修改时才克隆。

#### 8. `AgentTracker.complete()` 字符串克隆两次（coordinator.rs:174-179）

```rust
let summary = if result.len() > 200 {
    let truncated: String = result.chars().take(200).collect();
    format!("{}...", truncated)  // 克隆 1
} else {
    result.clone()  // 克隆 2
};
```

#### 9. 热路径中频繁克隆字符串

模型名、工具名在热路径中被频繁克隆，考虑使用 `Arc<str>` 减少分配。

#### 10. 费用追踪使用 f64（state.rs:15）

```rust
pub cost_usd: f64,
```

浮点累加在多次 API 调用后会漂移。对于 CLI 工具可以接受，但应标注为近似值。

### P3 — 代码组织

#### 11. 大文件未拆分

| 文件 | 大小 | 建议 |
|------|------|------|
| `memory_extractor.rs` | 33.0KB | 拆分为提取逻辑、格式化、持久化 |
| `dispatch_agent.rs` | 29.0KB | 拆分为调度逻辑、子代理状态管理 |
| `bus_adapter.rs` | 28.5KB | 拆分为事件路由、协议适配 |

---

## 代码质量评估

| 维度 | 评分 | 说明 |
|------|------|------|
| 错误处理 | ⭐⭐⭐⭐ | 顶层用 `anyhow::Result`，工具错误用 `ContentBlock::ToolResult { is_error: true }` |
| 异步设计 | ⭐⭐⭐⭐ | 正确使用 `Arc<RwLock<>>`，热路径无 `block_on` |
| 测试覆盖 | ⭐⭐⭐⭐ | 128+ 测试，含单元测试和 e2e 测试 |
| 命名 | ⭐⭐⭐⭐⭐ | 清晰、描述性的命名 |
| 文档 | ⭐⭐⭐ | 多个公开 API 缺少文档注释 |
| 模块组织 | ⭐⭐⭐⭐ | 清晰分离，但部分文件过大 |
| 安全性 | ⭐⭐⭐ | 存在 TOCTOU 竞态、无超时等问题 |

---

## 修复建议汇总

| 优先级 | 问题 | 位置 | 建议 |
|--------|------|------|------|
| P0 | `pop_last_turn()` 逻辑错误 | engine/mod.rs:524 | 区分 tool result User 和普通 User |
| P0 | coordinator unwrap panic | coordinator.rs:360 | 改用 `ok_or_else` |
| P1 | 权限提示无超时 | executor.rs:180 | 添加超时机制 |
| P1 | build() 阻塞 I/O | builder.rs:180 | 改为 async 或接受预加载内容 |
| P1 | 压缩熔断器未连线 | query/mod.rs | 在错误路径调用 record_compact_failure() |
| P1 | should_auto_compact 双路径 | engine/mod.rs:449 | 统一为单一路径 |
| P2 | 不必要的 clone | executor.rs:66 | 延迟克隆 |
| P2 | 字符串克隆优化 | coordinator.rs:174 | 原地截断 |
| P3 | 大文件拆分 | 多个文件 | 按职责拆分 |
| P3 | 添加文档注释 | 多个文件 | 补充公开 API 文档 |
