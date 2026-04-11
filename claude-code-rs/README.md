# Claude Code RS

[![Rust](https://img.shields.io/badge/language-Rust-orange)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/claude--code--ts-v2.1.88-blue)](https://www.anthropic.com/)

> Claude Code 的 Rust 实现——终端 AI 编程助手，高性能、低内存占用。

---

## 目录

- [快速开始](#快速开始)
- [安装](#安装)
- [基本用法](#基本用法)
- [命令行选项](#命令行选项)
- [REPL 交互模式](#repl-交互模式)
- [斜杠命令参考](#斜杠命令参考)
- [Computer Use 桌面自动化](#computer-use-桌面自动化)
- [多 Agent 协调模式](#多-agent-协调模式)
- [MCP 工具扩展](#mcp-工具扩展)
- [CLAUDE.md 项目配置](#claudemd-项目配置)
- [Skills 技能系统](#skills-技能系统)
- [Hooks 钩子系统](#hooks-钩子系统)
- [会话管理](#会话管理)
- [多 Provider 支持](#多-provider-支持)
- [环境变量](#环境变量)
- [权限模式](#权限模式)
- [CI/CD 集成](#cicd-集成)

---

## 快速开始

```bash
# 设置 API Key
export ANTHROPIC_API_KEY="sk-ant-..."

# 启动交互式 REPL
claude

# 一次性提问（非交互式）
claude "帮我解释这段代码"

# 对当前目录进行代码审查
claude --print "review this codebase"
```

---

## 安装

### 从源码构建

```bash
git clone https://github.com/your-org/claude-code-rs
cd claude-code-rs
cargo build --release
# 二进制位于 target/release/claude
```

### 生成 Shell 补全

```bash
# Bash
claude --completions bash >> ~/.bashrc

# Zsh
claude --completions zsh > ~/.zsh/completions/_claude

# Fish
claude --completions fish > ~/.config/fish/completions/claude.fish

# PowerShell
claude --completions powershell >> $PROFILE
```

---

## 基本用法

### 交互式 REPL

```bash
claude                        # 使用默认模型启动
claude -m opus                # 使用 Claude Opus
claude --resume               # 恢复上次会话
claude --session-id <uuid>    # 恢复指定会话
```

### 非交互式（脚本/管道）

```bash
# 直接提问并输出结果
claude -p "用一句话解释量子纠缠"

# 管道输入
cat error.log | claude -p "分析这个错误"

# 指定工作目录
claude -d /path/to/project "review this code"

# JSON 输出（适合机器处理）
claude --output-format json "list all functions in main.rs"

# NDJSON 流式输出
claude --output-format stream-json "explain this file" | jq .
```

---

## 命令行选项

| 选项 | 简写 | 默认值 | 说明 |
|------|------|--------|------|
| `--api-key` | | `$ANTHROPIC_API_KEY` | API 密钥 |
| `--model` | `-m` | `claude-sonnet-4-20250514` | 模型名或别名 |
| `--permission-mode` | | `default` | 权限模式（见下文） |
| `--cwd` | `-d` | 当前目录 | 工作目录 |
| `--print` | `-p` | `false` | 仅输出最终回复（适合管道） |
| `--output-format` | | `text` | 输出格式：`text` / `json` / `stream-json` |
| `--resume` | | `false` | 恢复上次会话 |
| `--session-id` | | | 恢复指定会话 |
| `--max-turns` | | `100` | 最大对话轮数（非交互模式） |
| `--max-tokens` | | `16384` | 每次响应最大 token 数 |
| `--max-context-window` | | 模型默认 | 上下文窗口大小（token） |
| `--thinking` | | `false` | 开启扩展思考（链式推理） |
| `--thinking-budget` | | `10000` | 思考 token 预算 |
| `--system-prompt` | | | 替换整个系统提示词 |
| `--append-system-prompt` | | | 在默认提示词后追加内容 |
| `--no-claude-md` | | `false` | 跳过 CLAUDE.md 注入 |
| `--add-dir` | | | 追加上下文目录（可多次） |
| `--allowed-tools` | | 全部 | 限制可用工具（逗号分隔） |
| `--coordinator` | | `false` | 多 Agent 协调模式 |
| `--provider` | | `anthropic` | API 后端 |
| `--base-url` | | | 自定义 API 地址 |
| `--verbose` | `-v` | `false` | 详细日志输出 |
| `--timeout` | | `0`（无限制）| 全局超时（秒） |
| `--init` | | `false` | 初始化项目配置 |
| `--list-sessions` | | | 列出所有会话并退出 |
| `--search-sessions` | | | 搜索会话并退出 |
| `--completions` | | | 生成 Shell 补全并退出 |

### 模型别名

| 别名 | 实际模型 |
|------|---------|
| `sonnet` / `best` | claude-sonnet-4-20250514 |
| `opus` | claude-opus-4-20250514 |
| `haiku` | claude-haiku-4-20250514 |

---

## REPL 交互模式

### 基本操作

| 按键 | 功能 |
|------|------|
| `Enter` | 发送消息 |
| `Ctrl+J` / `Shift+Enter` | 插入换行（多行输入） |
| `Ctrl+C` | 中断当前操作 |
| `Ctrl+D` | 退出（空缓冲区时） |
| `/` | 输入斜杠触发命令补全 |
| `Tab` | 自动补全命令/路径 |
| `→`（右箭头）| 接受 ghost text 提示 |
| `↑` / `↓` | 浏览历史输入 |

### 多行输入

```
> 请帮我写一个函数，功能如下：[Ctrl+J]
  1. 读取文件[Ctrl+J]
  2. 解析 JSON[Ctrl+J]
  3. 返回结构体[Enter]
```

---

## 斜杠命令参考

在 REPL 中输入 `/` 后按 `Tab` 可浏览所有命令：

### 对话管理

| 命令 | 说明 |
|------|------|
| `/help` | 显示帮助信息 |
| `/clear` | 清空对话历史 |
| `/compact` | 压缩对话（保留摘要，节省 token） |
| `/undo` | 撤销上一轮 AI 回复 |
| `/retry` | 重试上一个失败的请求 |
| `/rewind [N]` | 回退 N 轮对话 |
| `/branch` | 创建对话分支 |
| `/history` | 浏览历史轮次 |

### 代码与 Git

| 命令 | 说明 |
|------|------|
| `/diff` | 显示 git diff |
| `/status` | 显示会话 + git 状态 |
| `/commit` | 暂存并提交变更 |
| `/commit-push-pr` | 提交 + 推送 + 创建 PR |
| `/pr` | 创建/审查 Pull Request |
| `/pr-comments` | 获取 PR 审查评论 |
| `/branch` | Fork 对话分支 |
| `/review` | AI 代码审查 |
| `/bug` | 调试问题 |

### 配置与环境

| 命令 | 说明 |
|------|------|
| `/model [name]` | 切换模型 |
| `/fast` | 切换快速/廉价模型 |
| `/think` | 切换扩展思考模式 |
| `/effort [low/med/high]` | 设置努力级别 |
| `/permissions` | 显示当前权限模式 |
| `/config` | 显示当前配置 |
| `/env` | 显示环境信息 |
| `/theme [name]` | 切换终端主题 |
| `/vim` | 切换 Vim 键位模式 |
| `/break-cache` | 跳过提示词缓存 |

### 会话与导出

| 命令 | 说明 |
|------|------|
| `/session [list/new/resume]` | 会话管理 |
| `/rename [title]` | 重命名当前会话 |
| `/tag [name]` | 标记会话 |
| `/export [path]` | 导出会话（JSON/Markdown） |
| `/share` | 生成可分享的会话摘要 |
| `/summary` | 生成对话摘要 |
| `/copy` | 复制最后回复到剪贴板 |
| `/image [path]` | 附加图片到下一条消息 |

### 上下文与 MCP

| 命令 | 说明 |
|------|------|
| `/context` | 显示已加载的上下文 |
| `/add-dir [path]` | 添加上下文目录 |
| `/reload-context` | 重新加载 CLAUDE.md 和配置 |
| `/memory [view/edit]` | 管理记忆文件 |
| `/mcp` | 显示 MCP 服务器列表 |
| `/plugin` | 显示已加载插件 |
| `/files` | 列出目录文件 |
| `/agents` | 管理 Agent 定义 |

### 杂项

| 命令 | 说明 |
|------|------|
| `/stats` / `/usage` / `/cost` | 显示 token 用量和费用 |
| `/plan` | 切换计划模式 |
| `/init` | 初始化 CLAUDE.md |
| `/search [keyword]` | 搜索对话历史 |
| `/doctor` | 检查环境健康 |
| `/version` | 显示版本信息 |
| `/login` | 设置 API Key |
| `/logout` | 清除 API Key |
| `/release-notes` | 显示更新日志 |
| `/feedback` | 提交反馈 |
| `/stickers` | 申请贴纸！ |
| `/exit` | 退出 |

---

## Computer Use 桌面自动化

**无需任何命令**，只要系统有可用的显示器，引擎启动时会自动注册桌面控制工具。

### 可用工具

| 工具 | 说明 |
|------|------|
| `screenshot` | 截取屏幕或指定窗口区域 |
| `click` | 点击指定坐标 |
| `double_click` | 双击指定坐标 |
| `type_text` | 键入文本字符串 |
| `key` | 按下键盘组合键 |
| `scroll` | 在指定坐标滚动 |
| `mouse_move` | 移动鼠标到指定坐标 |
| `cursor_position` | 获取当前鼠标位置 |
| `clipboard_read` | 读取剪贴板文本 |
| `clipboard_write` | 写入文本到剪贴板 |
| `platform_info` | 获取 OS、显示器信息 |

### 使用方式

直接用自然语言描述操作，Claude 会自动调用对应工具：

```
> 帮我截个屏看看当前桌面
> 点击屏幕左上角坐标 (100, 50)
> 在当前焦点窗口输入 "hello world" 然后按回车
> 打开浏览器，访问 github.com
> 把屏幕上的错误信息截图发给我看
```

### 排查

```bash
# 检查 computer-use 是否成功注册
claude --verbose 2>&1 | grep -i computer

# 成功：Computer Use: 11 tools registered
# 失败：Computer Use not available: <原因>
```

> **注意**：Windows 需要正确安装 `enigo` 和 `screenshots` 的原生依赖。Linux 需要 X11 或 Wayland 显示环境。

---

## 多 Agent 协调模式

使用 `--coordinator` 启用多 Agent 并行任务模式：

```bash
claude --coordinator "将整个代码库重构成模块化结构，同时更新文档"
```

协调者会自动将任务分解为子任务，并发派给多个 Agent 并行执行，最后汇总结果。

### Swarm 模式（实验性）

```bash
CLAUDE_CODE_SWARM=1 claude --coordinator "大规模并行任务"
```

---

## MCP 工具扩展

MCP（Model Context Protocol）允许通过外部服务扩展 Claude 的工具集。

### 配置 MCP 服务器

在 `.claude/settings.json` 中配置：

```json
{
  "mcpServers": {
    "my-server": {
      "command": "npx",
      "args": ["-y", "@my/mcp-server"],
      "env": {
        "MY_KEY": "value"
      }
    }
  }
}
```

### 查看 MCP 状态

```
> /mcp
```

---

## CLAUDE.md 项目配置

每个项目可以有自己的 `CLAUDE.md` 文件，为 Claude 提供项目级别的上下文和约束。

### 初始化

```bash
claude --init
# 或在 REPL 中：
# /init
```

### 文件位置

| 文件 | 作用范围 |
|------|---------|
| `~/.claude/CLAUDE.md` | 全局用户配置（所有项目生效） |
| `.claude/CLAUDE.md` | 项目级配置（优先级更高） |
| `.claudeignore` | 忽略特定文件/目录（类似 .gitignore） |

### 示例内容

```markdown
# 项目说明
这是一个 Rust Web 服务，使用 Axum 框架。

## 代码规范
- 使用 `anyhow` 处理错误
- 所有公共 API 必须有文档注释
- 测试文件放在 `tests/` 目录

## 禁止操作
- 不得修改 `vendor/` 目录
- 不得直接推送到 `main` 分支

## 构建命令
- 构建：`cargo build`
- 测试：`cargo test`
- 检查：`cargo clippy`
```

---

## Skills 技能系统

Skills 是可复用的提示词模板，存储在 `~/.claude/skills/` 或 `.claude/skills/`。

### 使用技能

```
> /skills               # 列出所有可用技能
> @skill-name 参数      # 调用指定技能
```

### 创建技能

在 `.claude/skills/review.md` 中：

```markdown
---
name: review
description: 专业代码审查
---

请对以下代码进行专业审查，重点关注：
1. 安全性问题
2. 性能瓶颈
3. 代码可读性
4. 测试覆盖率

代码：{{input}}
```

---

## Hooks 钩子系统

Hooks 允许在工具执行前后运行自定义 Shell 脚本。

### 配置

在 `.claude/settings.json` 中：

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "echo 'About to run bash command' >> ~/.claude/hooks.log"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "FileWrite",
        "hooks": [
          {
            "type": "command",
            "command": "prettier --write $TOOL_OUTPUT_FILE 2>/dev/null || true"
          }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "notify-send 'Claude 完成了任务'"
          }
        ]
      }
    ]
  }
}
```

### Hook 类型

| 类型 | 触发时机 |
|------|---------|
| `PreToolUse` | 工具调用前 |
| `PostToolUse` | 工具调用后 |
| `Stop` | Agent 停止时（退出码 2 = 注入反馈并继续） |

---

## 会话管理

```bash
# 列出所有会话
claude --list-sessions

# 搜索会话
claude --search-sessions "refactor"

# 恢复上次会话
claude --resume

# 恢复指定会话
claude --session-id <uuid>
```

### REPL 内会话操作

```
/session list           # 列出会话
/session new            # 新建会话
/rename 新名称          # 重命名
/tag feature/auth       # 打标签
/export ./my-session.md # 导出为 Markdown
```

---

## 多 Provider 支持

### Anthropic（默认）

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
claude -m claude-sonnet-4-20250514
```

### OpenAI 兼容

```bash
claude --provider openai --api-key sk-... -m gpt-4o
```

### DeepSeek

```bash
claude --provider deepseek --api-key sk-... -m deepseek-chat
```

### Ollama（本地）

```bash
# 先启动 Ollama 服务
ollama pull llama3.2
claude --provider ollama --base-url http://localhost:11434/v1 -m llama3.2
```

### DashScope（阿里云）

```bash
claude --provider dashscope --api-key <key> -m qwen-plus
```

### AWS Bedrock / Google Vertex

```bash
claude --provider bedrock -m anthropic.claude-3-5-sonnet-20241022-v2:0
claude --provider vertex -m claude-3-5-sonnet@20241022
```

---

## 环境变量

| 变量 | 说明 |
|------|------|
| `ANTHROPIC_API_KEY` | Anthropic API 密钥 |
| `CLAUDE_CODE_MAX_CONTEXT_TOKENS` | 覆盖上下文窗口大小 |
| `CLAUDE_CODE_AUTO_COMPACT_WINDOW` | 上下文窗口上限（只能缩小） |
| `CLAUDE_CODE_SWARM` | 设为 `1` 启用 Swarm 模式 |
| `RUST_LOG` | 日志级别（`debug`, `info`, `warn`, `error`） |

---

## 权限模式

| 模式 | 说明 |
|------|------|
| `default` | 危险操作前询问确认 |
| `bypass` | 跳过所有权限检查（⚠️ 危险） |
| `acceptEdits` | 自动批准文件编辑，Shell 命令仍需确认 |
| `plan` | 只读模式，不执行任何工具 |

```bash
# 自动化场景，信任 Claude 自主决策
claude --permission-mode bypass "fix all linting errors"

# 只想看计划，不执行
claude --permission-mode plan "how would you refactor this?"
```

---

## CI/CD 集成

```yaml
# GitHub Actions 示例
- name: AI Code Review
  run: |
    claude \
      --print \
      --permission-mode bypass \
      --output-format json \
      --max-turns 5 \
      "Review the changes in this PR and output any issues as JSON"
  env:
    ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
```

### 退出码

| 代码 | 含义 |
|------|------|
| `0` | 成功 |
| `1` | 通用错误 |
| `2` | 权限被拒绝 |
| `3` | 上下文窗口超限 |
| `4` | 超时 |

---

## 项目架构

```
claude-code-rs/
├── crates/
│   ├── clawed-core/        # 配置、消息、工具类型、会话、权限
│   ├── clawed-api/         # API 客户端、流式解析、多 Provider
│   ├── clawed-tools/       # 40+ 工具实现（文件/Shell/Web/MCP 等）
│   ├── clawed-agent/       # 推理引擎、Hooks、协调器、DispatchAgent
│   ├── clawed-computer-use/# 桌面自动化（截图/鼠标/键盘）
│   ├── clawed-mcp/         # MCP 客户端/服务端协议
│   ├── clawed-swarm/       # Swarm 多 Agent（kameo actors）
│   └── clawed-cli/         # CLI 入口、REPL、输入系统、渲染
└── docs/                   # 架构文档和审计报告
```

---

## 许可

本项目为学习/研究目的。Claude Code 原版权归 [Anthropic](https://www.anthropic.com) 所有。
