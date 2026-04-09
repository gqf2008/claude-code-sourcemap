# Agent 迭代执行机制分析

## 核心循环位置

`crates/claude-agent/src/query/mod.rs` — `query_stream_with_injection` 函数（第 100-615 行）

## 1. ReAct 主循环

核心是一个 `loop`，每一轮执行以下步骤：

```
┌─────────────────────────────────────┐
│ 1. 检查 abort / max_turns / budget   │
│ 2. 构建 MessagesRequest，调用 API    │
│ 3. 流式接收响应 (SSE)                │
│    - 解析 TextDelta / ToolUse 等事件  │
│ 4. 根据 stop_reason 分支：           │
│    - ToolUse  → 执行工具 → 结果追加为 │
│      UserMessage → continue          │
│    - EndTurn  → break（任务完成）     │
│    - MaxTokens → 自动恢复重试          │
│    - 其他     → break                │
└─────────────────────────────────────┘
```

### 关键代码（第 461-511 行）

```rust
match actual_stop {
    StopReason::ToolUse if !tool_uses.is_empty() => {
        // 执行所有工具调用
        tool_context.messages = messages.clone();
        let results = executor.execute_many(tool_uses, &tool_context).await;
        // 工具结果作为 UserMessage 回传给模型
        messages.push(Message::User(tool_result_msg));
        turn_count += 1;
        state.write().await.turn_count = turn_count;

        // ← 没有 break！循环继续，再次调用 API
        // 模型看到工具结果后决定下一步
    }
    StopReason::MaxTokens => { /* 自动恢复 */ }
    other => { /* break */ }
}
```

## 2. 终止条件（5 种）

| 条件 | 代码行 | 说明 |
|------|--------|------|
| `turn_count >= max_turns` | 155 | 默认上限 100 轮，可配置 |
| `abort_signal.is_aborted()` | 149 | 用户手动 Ctrl-C 中止 |
| `StopReason::EndTurn` | 569-610 | 模型认为任务完成，主动结束 |
| `StopReason::StopSequence` | 569-610 | 触发停止序列 |
| `token_budget` 超限 | 444-457 | 达到 token 预算上限 |
| 连续 API 错误 | 216-220 | 错误恢复全部失败后终止 |

## 3. 为什么能"自动完成"任务

不是魔法，是 **提示工程 + 工具循环** 的组合：

1. **系统提示词** 告诉模型：你需要规划、执行工具、观察结果、继续下一步直到任务完成
2. 模型每次返回 `tool_use` 时，系统自动执行并把结果作为 `UserMessage` 回传
3. 模型看到工具结果后，决定下一步操作（继续调工具 或 返回 `end_turn`）
4. 当模型判断任务完成，返回 `end_turn`，循环退出

## 4. 自我修复机制

### 4.1 MaxTokens 恢复（533-567 行）

```rust
StopReason::MaxTokens => {
    // 策略1：escalate max_tokens 到模型上限
    if effective_max_tokens < escalated_max_tokens {
        effective_max_tokens = escalated_max_tokens;
        continue; // 重试
    }
    // 策略2：多轮 continuation 重试，最多 3 次
    if max_tokens_recovery_count < MAX_TOKENS_RECOVERY_LIMIT {
        max_tokens_recovery_count += 1;
        messages.push(Message::User(make_continuation_message(...)));
        continue;
    }
    // 耗尽 → break
}
```

### 4.2 Reactive Compact（184-202 行）

API 报 prompt 过长时：
- 截断大型 tool result（减半）
- 裁剪旧消息，只保留最近 5 轮
- `continue` 重试

### 4.3 Proactive Auto-Compact（487-530 行）

每轮工具执行后：
- 计算当前 token 数占 context window 的百分比
- 接近上限时主动压缩对话为摘要
- 重置 token 计数器，避免触发 context limit

### 4.4 API 错误重试（204-214 行）

```rust
ApiErrorAction::Retry { wait_ms } => {
    // 指数退避：1s → 2s → 4s → 8s → ... → 32s 上限
    let jittered = with_jitter(wait_ms, consecutive_errors);
    tokio::time::sleep(Duration::from_millis(jittered)).await;
    continue;
}
```

### 4.5 Stream 超时重试（341-354 行）

- 流超时（idle timeout / stall）自动重试最多 3 次
- 重试时跳过不完整的 assistant 消息，重新调用 API

### 4.6 Stop Hook 反馈循环（571-604 行）

```rust
HookDecision::FeedbackAndContinue { feedback }
    if stop_hook_retries < MAX_STOP_HOOK_RETRIES => {
    // 将 hook 的 feedback 作为新 UserMessage 注入
    messages.push(Message::User(feedback_msg));
    continue; // 让模型根据反馈重新思考
}
```

- Hook 脚本返回非零退出码 → 反馈给模型，最多重试 3 次
- 模型根据反馈调整方案后继续执行

## 5. 协调者模式（Coordinator Mode）

`crates/claude-agent/src/coordinator.rs`

协调者可以：
- 通过 `Agent` 工具启动后台 worker agent
- 通过 `SendMessage` 工具向运行中的 agent 发送消息
- 通过 `TaskStop` 工具中止 agent
- Worker 完成后通过 `TaskNotification` 注入为 UserMessage

Worker 的完成结果以 `<task-notification>` XML 注入协调者的消息流：

```xml
<task-notification>
  <task-id>agent-123</task-id>
  <status>completed</status>
  <summary>...</summary>
  <result>...</result>
  <usage>
    <total_tokens>1500</total_tokens>
    <tool_uses>5</tool_uses>
    <duration_ms>3200</duration_ms>
  </usage>
</task-notification>
```

## 6. 相关 crate 职责

| 文件 | 职责 |
|------|------|
| `query/mod.rs` | 核心 agent loop：API 调用 → 解析 → 工具执行 → 循环 |
| `engine/mod.rs` | QueryEngine：封装 loop，提供 submit / save / compact 等入口 |
| `executor.rs` | ToolExecutor：执行工具调用 |
| `coordinator.rs` | 多 agent 协调：注册/通知/消息路由 |
| `compact/mod.rs` | 自动压缩：对话摘要 + 动态阈值 |
| `hooks/` | 生命周期钩子：SessionStart / PostSampling / Stop 等 |
| `state.rs` | 共享状态：messages / token 计数 / model |
