# Claude Code 完整架构解析

## 一、全局定位

Claude Code 是一个运行在终端/SDK中的 **AI Agent**，核心是一个 **消息状态机**（message-state-machine）——不是传统意义上的"Agent 对象"在循环，而是 `query.ts` 里的 `while(true)` 不断积累消息，模型通过返回 `tool_use` 来驱动下一轮。

## 二、入口 → 执行 → 输出：三层架构

```
┌─────────────────────────────────────────────────────────────────┐
│  Layer 1: QueryEngine  (SDK 适配层 / 会话生命周期)               │
│  文件: QueryEngine.ts                                           │
│  职责: 拥有会话状态、构建 system prompt、处理 /命令、              │
│       调用 query()、翻译输出为 SDKMessage、判定最终结果            │
├─────────────────────────────────────────────────────────────────┤
│  Layer 2: queryLoop  (核心 Agent 循环)                           │
│  文件: query.ts                                                  │
│  职责: while(true) 消息积累循环、模型调用、工具执行、               │
│       上下文压缩、错误恢复、stop hooks                            │
├─────────────────────────────────────────────────────────────────┤
│  Layer 3: Tool Execution  (工具执行层)                            │
│  文件: StreamingToolExecutor.ts / toolOrchestration.ts           │
│  职责: 并发/串行执行工具、权限检查、结果收集                       │
└─────────────────────────────────────────────────────────────────┘
```

## 三、Layer 1: QueryEngine —— SDK 适配与会话管理

`restored-src/src/QueryEngine.ts` 大约 1300 行，核心是 `QueryEngine` 类。

### 生命周期

```
new QueryEngine(config)
  └─ submitMessage(prompt)  ← AsyncGenerator<SDKMessage>
       ├─ 1. 构建 system prompt（fetchSystemPromptParts + userContext + coordinatorContext）
       ├─ 2. processUserInput() 处理 /slash 命令
       ├─ 3. 持久化 transcript
       ├─ 4. yield systemInit 消息
       ├─ 5. for await (msg of query(...)) { switch(msg.type) → yield SDKMessage }
       ├─ 6. isResultSuccessful() 判定终态
       └─ 7. yield result 消息（success / error_during_execution / error_max_budget_usd）
```

### 关键设计

- **一个 QueryEngine = 一个会话**。`mutableMessages`、`readFileState`、`totalUsage`、`permissionDenials` 等跨 turn 保持
- **canUseTool 包装**：包装了 `canUseTool` 来追踪权限拒绝
- **结果判定**：通过 `isResultSuccessful(result, lastStopReason)` 判断。三种合法终态：
  1. 最后消息是 assistant 且包含 text/thinking 内容块
  2. 最后消息是 user 且所有内容块都是 `tool_result`
  3. `stopReason === 'end_turn'`（空输出但正常完成）
- **SDK 消息翻译**：`normalizeMessage()` 把内部 Message → SDKMessage。assistant 过滤空块；progress 按类型拆分（`agent_progress` 拆为 assistant+user，`bash_progress` 节流 30 秒）；user 直通

### 便利包装

`ask()` 是一个一次性便利函数，内部创建 `QueryEngine` + 调用一次 `submitMessage`。`snipReplay` 仅在 `feature('HISTORY_SNIP')` 启用时注入。

## 四、Layer 2: queryLoop —— 核心 Agent 循环

`restored-src/src/query.ts` 约 1740 行，`queryLoop()` 在 L241。

### State 状态机

```typescript
type State = {
  messages: Message[]                    // 全部累积消息
  toolUseContext: ToolUseContext          // 工具执行上下文（共享/隔离策略见 §八）
  autoCompactTracking                    // 自动压缩追踪
  turnCount: number                      // 当前 turn
  maxOutputTokensRecoveryCount: number   // max_output_tokens 恢复尝试（≤3）
  hasAttemptedReactiveCompact: boolean   // 是否已尝试响应式压缩
  maxOutputTokensOverride               // 输出 token 上限覆盖
  pendingToolUseSummary                  // 异步 tool 摘要（Haiku 生成）
  stopHookActive                        // stop hook 是否激活
  transition: Continue | undefined       // 上一次 continue 的原因
}
```

### 每次迭代的完整流程

```
while(true) {
  ┌─── 1. 预处理管线 ────────────────────────────────────────────────┐
  │  messagesForQuery = getMessagesAfterCompactBoundary(messages)     │
  │  → applyToolResultBudget()     // 按预算裁剪大的 tool_result      │
  │  → snipCompactIfNeeded()       // 裁剪历史（HISTORY_SNIP）        │
  │  → microcompact()              // 微压缩（缓存编辑优化）          │
  │  → applyCollapsesIfNeeded()    // 上下文折叠（CONTEXT_COLLAPSE）  │
  │  → autocompact()               // 自动摘要压缩                    │
  └──────────────────────────────────────────────────────────────────┘
  
  ┌─── 2. 模型调用 ─────────────────────────────────────────────────┐
  │  for await (msg of callModel({                                   │
  │    messages: prependUserContext(messagesForQuery, userContext),   │
  │    systemPrompt, thinkingConfig, tools, model, ...               │
  │  })) {                                                           │
  │    if (msg.type === 'assistant') {                               │
  │      assistantMessages.push(msg)                                 │
  │      if (msg has tool_use blocks) → needsFollowUp = true        │
  │      if (streamingToolExecutor) → addTool() 立即排队执行          │
  │    }                                                             │
  │    if (!withheld) yield msg    // 可恢复错误先扣住               │
  │  }                                                               │
  └──────────────────────────────────────────────────────────────────┘
  
  ┌─── 3. 终止判定 (!needsFollowUp) ───────────────────────────────┐
  │  if (withheld 413) → collapse drain → reactive compact → 恢复   │
  │  if (withheld max_output_tokens) → escalate 64k → 多轮恢复(≤3) │
  │  if (API error) → return                                        │
  │  handleStopHooks() → 验证钩子 → blockingErrors 则 continue      │
  │  TOKEN_BUDGET → continue 或 return                              │
  │  → return { reason: 'completed' }                               │
  └──────────────────────────────────────────────────────────────────┘
  
  ┌─── 4. 工具执行（needsFollowUp = true）─────────────────────────┐
  │  StreamingToolExecutor / runTools → 执行所有 tool_use 块          │
  │  → 收集 toolResults                                              │
  │  → 生成 toolUseSummary（Haiku，异步不阻塞）                      │
  └──────────────────────────────────────────────────────────────────┘
  
  ┌─── 5. 后处理与续行 ────────────────────────────────────────────┐
  │  getAttachmentMessages() → 内存/技能/文件变更附件                │
  │  pendingMemoryPrefetch.consume() → 相关记忆注入                 │
  │  skillDiscoveryPrefetch.collect() → 技能发现                    │
  │  drainQueuedCommands() → 消费队列命令                            │
  │  refreshTools() → 刷新 MCP 工具                                  │
  │  maxTurns 检查                                                   │
  │  state = { messages: [...messagesForQuery, ...assistantMessages, │
  │            ...toolResults], turnCount+1, ... }                   │
  │  continue  ← 进入下一次 while(true) 迭代                        │
  └──────────────────────────────────────────────────────────────────┘
}
```

### 9 种 continue 原因（`state.transition.reason`）

| reason | 触发条件 |
|--------|---------|
| `next_turn` | 正常工具执行后继续 |
| `collapse_drain_retry` | 上下文折叠排空后重试 |
| `reactive_compact_retry` | 响应式压缩后重试 |
| `max_output_tokens_escalate` | 从默认 8k 升级到 64k |
| `max_output_tokens_recovery` | 多轮截断恢复（最多 3 次） |
| `stop_hook_blocking` | stop hook 返回阻塞错误 |
| `token_budget_continuation` | 预算未用完，继续执行 |
| `model_fallback` | 模型切换（FallbackTriggeredError） |
| _（隐式）_ | 流式回退（tombstone orphaned messages） |

### 6 种终止原因

| reason | 含义 |
|--------|------|
| `completed` | 正常完成 |
| `aborted_streaming` / `aborted_tools` | 用户中断 |
| `max_turns` | 达到最大轮次 |
| `hook_stopped` | 钩子阻止继续 |
| `prompt_too_long` / `image_error` / `model_error` / `blocking_limit` | 各种错误 |

## 五、Layer 3: 工具执行

### 两种执行器

**1. StreamingToolExecutor**（`restored-src/src/services/tools/StreamingToolExecutor.ts`）—— 流式并发

- 模型还在流式输出时，`tool_use` 块一到立即排队执行
- 并发策略：`isConcurrencySafe` 的工具可以并行；非并发安全的必须独占
- 按接收顺序发射结果（order-preserving）
- 兄弟出错时通过 `siblingAbortController` 取消其他并行工具
- `discard()` 用于流式回退时丢弃进行中的工具

**2. runTools**（`restored-src/src/services/tools/toolOrchestration.ts`）—— 传统顺序执行

- `partitionToolCalls()` 把工具分为并发安全和非并发安全的批次
- 并发安全批次 → `runToolsConcurrently()`（用 `all()` 工具函数，最大并发 10）
- 非并发安全批次 → `runToolsSerially()`
- `contextModifier` 在串行执行后应用，在并发执行后按顺序应用

两种执行器最终都调用 `runToolUse()` → `canUseTool`（权限检查）→ `tool.call()`。

## 六、工具注册表

`restored-src/src/tools.ts` 集中注册 45+ 工具，分类如下：

| 分类 | 工具 | 门控 |
|------|------|------|
| 文件 | `FileReadTool`, `FileEditTool`, `FileWriteTool`, `GlobTool`, `GrepTool` | 无 |
| 执行 | `BashTool` | 无 |
| AI 编排 | `AgentTool`, `SkillTool` | 无 |
| 任务 | `TaskCreateTool`, `TaskGetTool`, `TaskUpdateTool`, `TaskListTool` | 无 |
| 交互 | `AskUserQuestionTool`, `WebFetchTool`, `WebSearchTool` | 无 |
| ANT 专属 | `REPLTool`, `SuggestBackgroundPRTool` | `USER_TYPE=ant` |
| 特性门控 | `SleepTool`(PROACTIVE), `MonitorTool`(MONITOR_TOOL), `WebBrowserTool`(WEB_BROWSER_TOOL), `CronTools`(AGENT_TRIGGERS), ... | `feature()` |

每个工具都实现 `Tool` 接口，核心字段：
- `name` / `aliases` / `description` — 工具标识
- `inputSchema`（Zod）— 输入验证
- `isConcurrencySafe(input)` — 能否并行
- `call(input, context)` — 执行逻辑，返回 `ToolResult<T>`
- `backfillObservableInput` — 流式输出时补全可观察字段

## 七、命令系统

`restored-src/src/commands.ts` 注册所有 `/命令`，三种类型（定义于 `types/command.ts`）：

| 类型 | 执行方式 | 示例 |
|------|---------|------|
| `local` | 同步 JS，不调 Claude | `/version`, `/cost`, `/clear` |
| `local-jsx` | React/Ink UI 组件 | `/config`, `/help`, `/mcp` |
| `prompt` | 通过 Claude 完成，声明 `allowedTools` | `/commit`, `/review` |

命令同样有大量特性门控（`feature('PROACTIVE')`, `feature('BRIDGE_MODE')`, `feature('VOICE_MODE')` 等）。

## 八、子代理/Worker 隔离策略

当 `AgentTool` 触发子代理时，流程是：

```
AgentTool.call()
  → runAgent() (runAgent.ts)
      → createSubagentContext() (forkedAgent.ts)  // 构建隔离的 ToolUseContext
      → query()  ← 和主线程用完全相同的 queryLoop！
      → sidechain transcript 记录
      → 清理（hooks, file cache, MCP, bash tasks）
```

**关键洞察**：子代理不是独立进程，也不是不同的执行引擎——它只是用不同的 `ToolUseContext` 调用同一个 `query()`。

### createSubagentContext 的隔离/共享策略

| 策略 | 字段 | 原因 |
|------|------|------|
| **克隆/隔离** | `readFileState`, `nestedMemoryAttachmentTriggers`, `loadedNestedMemoryPaths`, `dynamicSkillDirTriggers`, `discoveredSkillNames`, `toolDecisions`, `queryTracking`(新chainId) | 防止子代理的文件读取/技能发现状态污染父级 |
| **条件共享** | `setAppState`(共享 if shareSetAppState), `setResponseLength`(共享 if shareSetResponseLength), `abortController`(默认子控制器; 共享 if shareAbortController) | 不同类型的子代理需要不同的共享级别 |
| **始终共享** | `setAppStateForTasks`, `updateAttributionState` | 基础设施（bash 任务注册、归因追踪）必须全局可见 |
| **始终清空** | `addNotification`, `setToolJSX`, `setStreamMode`, `setSDKStatus`, `openMessageSelector` | UI 回调子代理不需要 |
| **克隆（带意图）** | `contentReplacementState` | 从父级克隆（非新建），保持 prompt cache 一致性 |
| **全新** | `localDenialTracking` | 异步代理的 `setAppState` 是 no-op，需要独立的拒绝计数 |

### Resume 机制

`resumeAgent.ts` 不是栈恢复，而是 **转录重建**：读取 sidechain transcript → 过滤未完成的 tool_use → 重建 `contentReplacementState` → 用原始参数重新调用 `runAgent()`。

## 九、上下文管理管线（压缩策略）

每次模型调用前，消息历史经过 5 层处理：

```
原始消息 messages
  │
  ├─ 1. applyToolResultBudget()     —— 按预算裁剪大的 tool_result
  ├─ 2. snipCompactIfNeeded()       —— 裁剪旧历史（HISTORY_SNIP）
  ├─ 3. microcompact()              —— 微压缩（缓存编辑优化，CACHED_MICROCOMPACT）
  ├─ 4. applyCollapsesIfNeeded()    —— 上下文折叠（CONTEXT_COLLAPSE，读时投影）
  └─ 5. autocompact()               —— 全量摘要压缩（超过 token 阈值时触发）
        │
        └→ messagesForQuery（喂给模型的最终消息）
```

当模型返回 prompt-too-long (413) 时，有两级恢复：
1. **Collapse drain**：排空所有已暂存的上下文折叠 → 重试
2. **Reactive compact**：紧急全量压缩 → 重试
3. 都失败 → 表面化错误，终止

## 十、ToolUseContext —— 全局上下文袋

`restored-src/src/Tool.ts` 定义的 `ToolUseContext` 是贯穿整个系统的上下文容器：

```typescript
type ToolUseContext = {
  // 配置
  options: { commands, tools, mainLoopModel, thinkingConfig, mcpClients, ... }
  
  // 状态
  messages: Message[]
  readFileState: FileStateCache
  abortController: AbortController
  contentReplacementState?: ContentReplacementState
  
  // 能力
  getAppState / setAppState / setAppStateForTasks
  handleElicitation?                    // MCP URL 确认
  setToolJSX? / addNotification?       // UI（仅 REPL）
  updateFileHistoryState / updateAttributionState
  
  // 追踪
  agentId?: AgentId                     // 子代理标识
  queryTracking?: QueryChainTracking    // 查询链追踪
  localDenialTracking?                  // 权限拒绝追踪
  discoveredSkillNames?                 // 技能发现追踪
  
  // 限制
  fileReadingLimits? / globLimits?
  renderedSystemPrompt?                 // fork 子代理共享父级 prompt cache
}
```

## 十一、Stop Hooks —— 停止前校验

`restored-src/src/query/stopHooks.ts` 中 `handleStopHooks()` 在模型说"完成"（无 `tool_use`）后执行：

```
模型完成（needsFollowUp = false）
  │
  ├─ 1. saveCacheSafeParams()         —— 保存缓存安全参数以供 /btw 等使用
  ├─ 2. 模板作业分类（TEMPLATES）       —— 更新 job state
  ├─ 3. 非 bare 模式后台任务：
  │     ├─ promptSuggestion             —— 提示建议
  │     ├─ extractMemories              —— 记忆提取（EXTRACT_MEMORIES）
  │     └─ autoDream                    —— 自动梦境
  ├─ 4. computerUse 清理（CHICAGO_MCP）
  ├─ 5. executeStopHooks()            —— 运行注册的停止钩子
  │     ├─ 如果有 blockingErrors → 返回，queryLoop 将 continue
  │     └─ 如果 preventContinuation → 返回，queryLoop 将终止
  └─ 6. 团队协作钩子（teammateIdle, taskCompleted）
```

**关键**：如果 stop hook 返回 `blockingErrors`，它们被注入消息历史，循环 **继续** 执行（`state.transition.reason = 'stop_hook_blocking'`），让模型修正问题。

## 十二、完整数据流总结

```
用户输入 "fix the bug in auth.ts"
        │
        ▼
  QueryEngine.submitMessage()
        │
        ├─ processUserInput() → 判断是否 /命令
        │   └─ 是命令 → 直接执行，yield 结果，return
        │   └─ 非命令 → shouldQuery = true
        │
        ├─ 构建 SystemPrompt + UserContext
        │
        └─ query() ──────────────────────────────────────────────────
            │                                                        │
            ▼                                                        │
        queryLoop() [while(true)]                                    │
            │                                                        │
            ├─ 预处理管线（budget/snip/microcompact/collapse/auto）     │
            │                                                        │
            ├─ callModel() → 流式收到 assistant 消息                  │
            │   │ "I'll read the file first"                         │
            │   │ tool_use: FileReadTool({file_path: "auth.ts"})     │
            │   └─ needsFollowUp = true                              │
            │                                                        │
            ├─ StreamingToolExecutor.addTool() → 立即执行              │
            │   └─ FileReadTool.call() → { data: "file content..." }  │
            │   └─ yield tool_result 消息                             │
            │                                                        │
            ├─ getAttachmentMessages() → 内存/技能附件                │
            │                                                        │
            ├─ state = {messages: [...old, assistant, tool_result]}   │
            │  continue ←──────────────────── 下一次迭代 ─────────────┘
            │
            ├─ callModel() → "I see the issue, let me fix it"
            │   │ tool_use: FileEditTool({...})
            │   └─ needsFollowUp = true
            │
            ├─ FileEditTool.call() → 修改文件
            │  continue ─── 下一次迭代
            │
            ├─ callModel() → "Done! I've fixed the authentication bug."
            │   └─ needsFollowUp = false (无 tool_use)
            │
            ├─ handleStopHooks() → 无阻塞错误
            │
            └─ return { reason: 'completed' }
                    │
                    ▼
        QueryEngine 收到 return
            ├─ isResultSuccessful() → true
            └─ yield { type: 'result', subtype: 'success', result: "Done! ..." }
```

## 十三、关键模式总结

| 模式 | 实现 |
|------|------|
| **Agent 循环** | 不是递归/递归函数，而是 `while(true)` + 可变 `State` 对象。消息积累-切换-继续。 |
| **工具协议** | Anthropic tool_use/tool_result 协议。assistant 返回 `tool_use` 块 → 系统执行 → 把 `tool_result` 作为 user 消息写回 → 下一轮。 |
| **流式并发** | `StreamingToolExecutor` 在模型还在流式输出时就开始执行工具，不等模型完成。 |
| **子代理复用** | worker/子代理不是新进程，而是用隔离的 `ToolUseContext` 调用同一个 `query()`。 |
| **上下文窗口管理** | 5 层压缩管线 + 2 级错误恢复（collapse drain → reactive compact）。 |
| **特性门控** | `feature('FLAG')` 通过 Bun bundle 实现编译时死代码消除。`process.env.USER_TYPE === 'ant'` 门控内部工具。 |
| **惰性加载** | 大量 `const X = require(...)` 惰性导入，打破循环依赖且延迟加载。 |
| **prompt cache 一致性** | `contentReplacementState` 从父级克隆（非新建），`renderedSystemPrompt` 传递给 fork 子代理，避免系统提示字节不匹配导致缓存失效。 |

## 十四、关键文件索引

| 文件 | 职责 |
|------|------|
| `restored-src/src/QueryEngine.ts` | SDK 适配层，会话生命周期 |
| `restored-src/src/query.ts` | 核心 Agent 循环（queryLoop） |
| `restored-src/src/Tool.ts` | Tool 接口 + ToolUseContext 类型定义 |
| `restored-src/src/tools.ts` | 工具注册表 |
| `restored-src/src/commands.ts` | 命令注册表 |
| `restored-src/src/services/tools/StreamingToolExecutor.ts` | 流式并发工具执行器 |
| `restored-src/src/services/tools/toolOrchestration.ts` | 传统工具编排（串行/并发） |
| `restored-src/src/services/tools/toolExecution.ts` | 单个工具执行（runToolUse） |
| `restored-src/src/tools/AgentTool/runAgent.ts` | 子代理/Worker 生命周期 |
| `restored-src/src/tools/AgentTool/resumeAgent.ts` | 子代理恢复（转录重建） |
| `restored-src/src/utils/forkedAgent.ts` | createSubagentContext 隔离策略 |
| `restored-src/src/utils/queryHelpers.ts` | normalizeMessage + isResultSuccessful |
| `restored-src/src/utils/messages/mappers.ts` | SDK 消息映射工具 |
| `restored-src/src/query/stopHooks.ts` | 停止钩子 |
| `restored-src/src/query/config.ts` | 每轮不变的环境/配置快照 |
| `restored-src/src/query/tokenBudget.ts` | Token 预算跟踪 |
| `restored-src/src/services/compact/autoCompact.ts` | 自动压缩 |
| `restored-src/src/services/compact/reactiveCompact.ts` | 响应式压缩（413 恢复） |
| `restored-src/src/services/compact/snipCompact.ts` | 裁剪压缩 |
| `restored-src/src/services/contextCollapse/index.ts` | 上下文折叠 |
| `restored-src/src/entrypoints/agentSdkTypes.ts` | SDK 类型定义与公共 API |
| `restored-src/src/state/AppState.ts` | 全局应用状态 |
| `restored-src/src/types/message.ts` | 消息类型定义 |
| `restored-src/src/types/permissions.ts` | 权限类型定义 |

---

> 基于 `@anthropic-ai/claude-code` v2.1.88 source map 还原的源码分析。源码版权归 [Anthropic](https://www.anthropic.com) 所有。
