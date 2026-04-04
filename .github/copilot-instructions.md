# Claude Code Sourcemap — 工作区指引

## 项目性质

本仓库是 **非官方研究仓库**，通过 `@anthropic-ai/claude-code` npm 包内附的 source map（`cli.js.map`）还原的 TypeScript 源码（版本 `2.1.88`）。**不可构建、不可发布**，仅供逆向研究与学习。

- 还原脚本：[extract-sources.js](../extract-sources.js)
- 还原输出：[restored-src/src/](../restored-src/src/)
- 原始包：[package/cli.js](../package/cli.js)（WebPack 产物）

## 架构概览

Claude Code 是一个终端 AI Agent，核心由两个注册表驱动：

| 注册表 | 文件 | 作用 |
|--------|------|------|
| 命令注册表 | `restored-src/src/commands.ts` | 用户可调用的 `/命令` |
| 工具注册表 | `restored-src/src/tools.ts` | Claude 可调用的工具 |

### 命令系统（`commands/`）

命令有三种类型（定义于 `restored-src/src/types/command.ts`）：

- **`local`** — 同步 JS 执行，无 Claude 调用
- **`local-jsx`** — React + Ink 终端 UI 组件
- **`prompt`** — 通过 Claude 完成，可声明 `allowedTools`

```ts
// 标准 local 命令结构
export default { type: 'local', name: '...', description: '...' } satisfies Command
```

### 工具系统（`tools/`）

45+ 个工具，分类：

- **文件操作**：`FileReadTool`, `FileEditTool`, `FileWriteTool`, `GlobTool`, `GrepTool`
- **执行**：`BashTool`, `PowerShellTool`, `REPLTool`
- **AI 编排**：`AgentTool`（子 Agent）, `SkillTool`, `MCP*Tools`
- **任务管理**：`TaskCreateTool`, `TaskUpdateTool`, `TaskListTool`
- **交互**：`AskUserQuestionTool`, `WebFetchTool`, `WebSearchTool`

ANT 内部工具通过 `process.env.USER_TYPE === 'ant'` 门控。

## 代码约定

### TypeScript 风格

```ts
// 1. 模块扩展名必须写 .js（即使是 .ts 源文件）
import { X } from './path.js'

// 2. satisfies 替代类型注解（TypeScript 4.9+）
const cmd = { ... } satisfies Command

// 3. 惰性 require 打破循环依赖
const getUtils = () => require('./utils.js') as typeof import('./utils.js')

// 4. Biome linter 指令保留 ANT-ONLY 标记顺序
// biome-ignore-all assist/source/organizeImports: ANT-ONLY markers
```

### 特性开关

通过 `feature('FLAG_NAME')` 实现编译时死代码消除（Bun bundle 机制），勿直接修改门控逻辑。

## 关键目录

```
restored-src/src/
├── tools/        # 工具实现（参考 FileReadTool 了解标准结构）
├── commands/     # 命令实现（commit.ts=prompt 示例，version.ts=local 示例）
├── types/        # 核心类型定义（command.ts, tool.ts）
├── coordinator/  # 多 Agent 协调模式
├── bridge/       # 远程会话 / IDE 桥接
├── services/     # API、MCP、分析等服务
└── utils/        # git、model、auth、env 等工具函数
```

## 注意事项

- 源码版权归 [Anthropic](https://www.anthropic.com) 所有，勿用于商业用途
- 还原文件共 **4756 个**（含 1884 个 `.ts`/`.tsx`），目录结构映射 webpack bundle 路径
- `vendor/` 目录含预编译的原生模块（ripgrep、audio-capture），不含 TS 源码
