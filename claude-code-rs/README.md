# Clawed Code

[![Rust](https://img.shields.io/badge/language-Rust-orange)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Based on](https://img.shields.io/badge/based%20on-claude--code%20v2.1.88-blue)](https://www.anthropic.com/)

> A Rust port of [Claude Code](https://www.anthropic.com/claude-code) — an agentic AI coding assistant for the terminal.  
> High performance, low memory footprint, and fully open source.

---

## Table of Contents

- [Quick Start](#quick-start)
- [Installation](#installation)
- [Usage](#usage)
- [CLI Reference](#cli-reference)
- [REPL Keybindings](#repl-keybindings)
- [Slash Commands](#slash-commands)
- [Computer Use](#computer-use)
- [Multi-Agent Coordinator](#multi-agent-coordinator)
- [MCP Extensions](#mcp-extensions)
- [CLAUDE.md Project Config](#claudemd-project-config)
- [Skills](#skills)
- [Hooks](#hooks)
- [Session Management](#session-management)
- [Providers](#providers)
- [Environment Variables](#environment-variables)
- [Permission Modes](#permission-modes)
- [CI/CD Integration](#cicd-integration)
- [Architecture](#architecture)

---

## Quick Start

```bash
# Set your API key
export ANTHROPIC_API_KEY="sk-ant-..."

# Start an interactive REPL session
clawed

# One-shot query (non-interactive)
clawed "explain this codebase"

# AI code review for the current directory
clawed --print "review this codebase and list any issues"
```

---

## Installation

### Build from Source

```bash
git clone https://github.com/gqf2008/clawed-code
cd clawed-code
cargo build --release
# Binary is at: target/release/clawed
```

Move to PATH:

```bash
# Linux / macOS
cp target/release/clawed ~/.local/bin/

# Windows
copy target\release\clawed.exe %USERPROFILE%\.cargo\bin\
```

### Shell Completions

```bash
# Bash
clawed --completions bash >> ~/.bashrc

# Zsh
clawed --completions zsh > ~/.zsh/completions/_clawed

# Fish
clawed --completions fish > ~/.config/fish/completions/clawed.fish

# PowerShell
clawed --completions powershell >> $PROFILE
```

---

## Usage

### Interactive REPL

```bash
clawed                         # Start with default model
clawed -m opus                 # Use Claude Opus
clawed --resume                # Resume last session
clawed --session-id <uuid>     # Resume a specific session
clawed --coordinator "..."     # Multi-agent orchestration mode
```

### Non-Interactive (Scripts & Pipes)

```bash
# Direct query with output
clawed -p "explain quantum entanglement in one sentence"

# Pipe input
cat error.log | clawed -p "diagnose this error"

# Specify working directory
clawed -d /path/to/project "review this code"

# JSON output (machine-readable)
clawed --output-format json "list all public functions in main.rs"

# NDJSON streaming
clawed --output-format stream-json "explain this file" | jq .
```

---

## CLI Reference

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--api-key` | | `$ANTHROPIC_API_KEY` | API key for authentication |
| `--model` | `-m` | `claude-sonnet-4-20250514` | Model name or alias |
| `--permission-mode` | | `default` | Permission mode (see below) |
| `--cwd` | `-d` | current dir | Working directory for the session |
| `--print` | `-p` | `false` | Print final response only (pipe-friendly) |
| `--output-format` | | `text` | Output format: `text` / `json` / `stream-json` |
| `--resume` | | `false` | Resume the most recent session |
| `--session-id` | | | Resume a specific session by UUID |
| `--max-turns` | | `100` | Max conversation turns (non-interactive) |
| `--max-tokens` | | `16384` | Max tokens per model response |
| `--max-context-window` | | model default | Override context window size (tokens) |
| `--thinking` | | `false` | Enable extended thinking (chain-of-thought) |
| `--thinking-budget` | | `10000` | Token budget for extended thinking |
| `--system-prompt` | | | Replace entire system prompt |
| `--append-system-prompt` | | | Append text after the default system prompt |
| `--no-claude-md` | | `false` | Skip CLAUDE.md injection |
| `--add-dir` | | | Add context directory (repeatable) |
| `--allowed-tools` | | all | Restrict available tools (comma-separated) |
| `--coordinator` | | `false` | Enable multi-agent coordinator mode |
| `--provider` | | `anthropic` | API provider backend |
| `--base-url` | | | Custom API endpoint URL |
| `--verbose` | `-v` | `false` | Enable debug logging |
| `--timeout` | | `0` (none) | Global session timeout in seconds |
| `--init` | | `false` | Initialize project configuration |
| `--list-sessions` | | | List all sessions and exit |
| `--search-sessions` | | | Search sessions by keyword and exit |
| `--completions` | | | Generate shell completions and exit |

### Model Aliases

| Alias | Resolves to |
|-------|-------------|
| `sonnet` / `best` | `claude-sonnet-4-20250514` |
| `opus` | `claude-opus-4-20250514` |
| `haiku` | `claude-haiku-4-20250514` |

---

## REPL Keybindings

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Ctrl+J` | Insert newline (multiline input) |
| `Ctrl+C` | Interrupt current operation |
| `Ctrl+D` | Exit (on empty buffer) |
| `/` + `Tab` | Browse slash commands |
| `Tab` | Complete command / file path |
| `→` (right arrow) | Accept ghost-text hint |
| `↑` / `↓` | Navigate input history |
| `Ctrl+R` | Reverse history search |
| `Ctrl+A` / `Ctrl+E` | Jump to line start / end |
| `Ctrl+U` / `Ctrl+K` | Delete to start / end of line |
| `Alt+B` / `Alt+F` | Move backward / forward by word |

### Multiline Input

```
> Please write a function that:[Ctrl+J]
  1. Reads a file[Ctrl+J]
  2. Parses JSON[Ctrl+J]
  3. Returns a struct[Enter]
```

---

## Slash Commands

Type `/` in the REPL and press `Tab` to browse all 57 commands:

### Conversation

| Command | Description |
|---------|-------------|
| `/help` | Show help |
| `/clear` | Clear conversation history |
| `/compact` | Compact conversation (summarize, save tokens) |
| `/undo` | Undo last assistant turn |
| `/retry` | Retry last failed prompt |
| `/rewind [N]` | Rewind N turns |
| `/branch` | Fork conversation into a new branch |
| `/history` | Browse conversation turns |
| `/search <query>` | Search conversation history |
| `/summary` | Generate a conversation summary |

### Code & Git

| Command | Description |
|---------|-------------|
| `/diff` | Show git diff |
| `/status` | Show session + git status |
| `/commit` | Stage and commit changes |
| `/commit-push-pr` | Commit, push, and open a PR |
| `/pr` | Create or review a pull request |
| `/pr-comments` | Fetch PR review comments |
| `/review` | AI code review |
| `/bug` | Debug a problem |

### Configuration

| Command | Description |
|---------|-------------|
| `/model [name]` | Switch model |
| `/fast` | Toggle fast/cheap model |
| `/think` | Toggle extended thinking |
| `/effort [low\|med\|high]` | Set effort level |
| `/permissions` | Show current permission mode |
| `/config` | Show current configuration |
| `/env` | Show environment info |
| `/theme [name]` | Switch terminal color theme |
| `/vim` | Toggle Vim keybindings |
| `/break-cache` | Skip prompt cache for this turn |

### Sessions & Export

| Command | Description |
|---------|-------------|
| `/session [list\|new\|resume]` | Manage sessions |
| `/rename [title]` | Rename the current session |
| `/tag [name]` | Tag or untag the session |
| `/export [path]` | Export session to JSON or Markdown |
| `/share` | Generate a shareable session snapshot |
| `/copy` | Copy last response to clipboard |
| `/image [path]` | Attach an image to the next message |

### Context & MCP

| Command | Description |
|---------|-------------|
| `/context` | Show loaded context files |
| `/add-dir [path]` | Add a context directory |
| `/reload-context` | Reload CLAUDE.md and settings |
| `/memory [view\|edit]` | Manage memory files |
| `/mcp` | List MCP servers and their tools |
| `/plugin` | List loaded plugins |
| `/files` | List files in current directory |
| `/agents` | Manage agent definitions |

### Misc

| Command | Description |
|---------|-------------|
| `/stats` / `/usage` / `/cost` | Show token usage and cost |
| `/plan` | Toggle plan mode (read-only) |
| `/init` | Initialize CLAUDE.md interactively |
| `/doctor` | Check environment health |
| `/version` | Show version info |
| `/login` | Set API key |
| `/logout` | Clear API key |
| `/release-notes` | Show changelog |
| `/feedback` | Submit feedback |
| `/stickers` | Order stickers! |
| `/exit` | Exit the CLI |

---

## Computer Use

Computer Use lets the AI control your desktop — no extra command needed. When a display is available, the engine auto-registers 11 desktop control tools at startup.

### Available Tools

| Tool | Description |
|------|-------------|
| `screenshot` | Capture screen or a region |
| `click` | Click at coordinates |
| `double_click` | Double-click at coordinates |
| `type_text` | Type a text string |
| `key` | Press a keyboard shortcut |
| `scroll` | Scroll at coordinates |
| `mouse_move` | Move cursor to coordinates |
| `cursor_position` | Get current cursor position |
| `clipboard_read` | Read clipboard text |
| `clipboard_write` | Write text to clipboard |
| `platform_info` | Get OS and display info |

### Usage

Just describe what you want in plain language:

```
> Take a screenshot of the current desktop
> Click the button at coordinates (200, 400)
> Type "hello world" and press Enter
> Open the browser and navigate to github.com
```

### Diagnostics

```bash
# Check if Computer Use registered successfully
clawed --verbose 2>&1 | grep -i computer
# OK:   Computer Use: 11 tools registered
# FAIL: Computer Use not available: <reason>
```

> **Platform notes:** On Linux, an X11 or Wayland display is required. On Windows, `enigo` and `screenshots` native dependencies must be installed correctly.

---

## Multi-Agent Coordinator

Enable parallel task orchestration with `--coordinator`:

```bash
clawed --coordinator "Refactor the entire codebase into a modular structure and update all docs"
```

The coordinator decomposes the task into sub-tasks and dispatches them to parallel sub-agents, then aggregates results.

### Swarm Mode (Experimental)

Swarm mode uses [kameo](https://github.com/tqwewe/kameo) actors for even more granular parallel execution:

```bash
CLAUDE_CODE_SWARM=1 clawed --coordinator "large-scale parallel task"
```

---

## MCP Extensions

[Model Context Protocol](https://modelcontextprotocol.io/) lets you extend the tool set via external services.

### Configure MCP Servers

In `.claude/settings.json`:

```json
{
  "mcpServers": {
    "my-tools": {
      "command": "npx",
      "args": ["-y", "@my-org/mcp-server"],
      "env": {
        "MY_API_KEY": "value"
      }
    }
  }
}
```

Then check the status in REPL:

```
/mcp
```

---

## CLAUDE.md Project Config

Place a `CLAUDE.md` file in your project to inject persistent context, constraints, and instructions into every session.

### Initialize

```bash
clawed --init
# or inside the REPL:
/init
```

### File Locations

| File | Scope |
|------|-------|
| `~/.claude/CLAUDE.md` | Global (all projects) |
| `.claude/CLAUDE.md` | Project-level (higher priority) |
| `.claudeignore` | Exclude files (like `.gitignore`) |

### Example

```markdown
# Project: Rust Web Service (Axum)

## Rules
- Use `anyhow` for error handling.
- All public APIs must have doc comments.
- Tests go in `tests/`.
- Never push directly to `main`.

## Build Commands
- Build: `cargo build`
- Test:  `cargo test`
- Lint:  `cargo clippy`
```

---

## Skills

Skills are reusable prompt templates stored in `~/.claude/skills/` or `.claude/skills/`.

### Use a Skill

```
/skills             # list available skills
@skill-name args    # invoke a skill
```

### Create a Skill

`.claude/skills/review.md`:

```markdown
---
name: review
description: Professional code review
---

Review the following code for:
1. Security vulnerabilities
2. Performance bottlenecks
3. Readability and maintainability
4. Test coverage

Code: {{input}}
```

---

## Hooks

Hooks run shell commands before or after tool execution, letting you integrate linters, formatters, notifications, and more.

### Configure

In `.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "echo 'Running: $TOOL_INPUT' >> ~/clawed-hooks.log"
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
            "command": "rustfmt $TOOL_OUTPUT_FILE 2>/dev/null || true"
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
            "command": "notify-send 'Clawed finished the task'"
          }
        ]
      }
    ]
  }
}
```

### Hook Types

| Type | When it fires |
|------|---------------|
| `PreToolUse` | Before a tool call |
| `PostToolUse` | After a tool call completes |
| `Stop` | When the agent finishes (exit code `2` = inject feedback and continue) |

---

## Session Management

```bash
# List all saved sessions
clawed --list-sessions

# Search sessions by keyword
clawed --search-sessions "auth refactor"

# Resume the most recent session
clawed --resume

# Resume a specific session
clawed --session-id <uuid>
```

### In-REPL Session Operations

```
/session list             # list sessions
/session new              # start a new session
/rename My Feature        # rename current session
/tag feature/auth         # tag the session
/export ./session.md      # export to Markdown
```

---

## Providers

Clawed Code supports 8 API backends:

### Anthropic (default)

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
clawed -m claude-sonnet-4-20250514
```

### OpenAI / OpenAI-compatible

```bash
clawed --provider openai --api-key sk-... -m gpt-4o
```

### DeepSeek

```bash
clawed --provider deepseek --api-key sk-... -m deepseek-chat
```

### Ollama (local)

```bash
ollama pull llama3.2
clawed --provider ollama --base-url http://localhost:11434/v1 -m llama3.2
```

### DashScope (Alibaba Cloud)

```bash
clawed --provider dashscope --api-key <key> -m qwen-plus
```

### Together AI / Groq

```bash
clawed --provider together --api-key <key> -m meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo
clawed --provider groq    --api-key <key> -m llama-3.1-70b-versatile
```

### AWS Bedrock / Google Vertex

```bash
clawed --provider bedrock -m anthropic.claude-3-5-sonnet-20241022-v2:0
clawed --provider vertex  -m claude-3-5-sonnet@20241022
```

---

## Environment Variables

| Variable | Description |
|----------|-------------|
| `ANTHROPIC_API_KEY` | Default API key (Anthropic) |
| `CLAUDE_CODE_MAX_CONTEXT_TOKENS` | Override context window size |
| `CLAUDE_CODE_AUTO_COMPACT_WINDOW` | Auto-compact threshold (tokens) |
| `CLAUDE_CODE_SWARM` | Set to `1` to enable Swarm mode |
| `RUST_LOG` | Log level: `error`, `warn`, `info`, `debug`, `trace` |

---

## Permission Modes

| Mode | Behavior |
|------|----------|
| `default` | Ask for confirmation before risky operations |
| `bypass` | Skip all permission checks ⚠️ |
| `acceptEdits` | Auto-approve file edits; still ask for shell commands |
| `plan` | Read-only — no tools are executed |

```bash
# Fully automated (trust the agent)
clawed --permission-mode bypass "fix all clippy warnings"

# Inspect the plan without executing anything
clawed --permission-mode plan "how would you refactor this module?"
```

---

## CI/CD Integration

```yaml
# GitHub Actions example
- name: AI Code Review
  run: |
    clawed \
      --print \
      --permission-mode bypass \
      --output-format json \
      --max-turns 5 \
      "Review the PR changes and report any bugs or security issues as JSON"
  env:
    ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
```

### Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | General error |
| `2` | Permission denied |
| `3` | Context window exceeded |
| `4` | Timeout |

---

## Architecture

Clawed Code is organized as a Cargo workspace with 11 crates:

```
clawed-code/
├── crates/
│   ├── clawed-core/          # Config, messages, tool types, sessions, permissions
│   ├── clawed-api/           # API client, SSE streaming, multi-provider support
│   ├── clawed-tools/         # 40+ built-in tools (file I/O, shell, web, MCP, …)
│   ├── clawed-agent/         # Inference engine, hooks, coordinator, DispatchAgent
│   ├── clawed-computer-use/  # Desktop automation (screenshot, mouse, keyboard)
│   ├── clawed-mcp/           # MCP client + server protocol implementation
│   ├── clawed-swarm/         # Swarm multi-agent (kameo actors)
│   ├── clawed-bus/           # Internal event bus
│   ├── clawed-rpc/           # RPC layer for remote agents
│   ├── clawed-bridge/        # IDE / remote session bridge
│   └── clawed-cli/           # CLI entry point, REPL, input system, renderer
└── docs/                     # Architecture docs and audit reports
```

### Built-in Tools

| Category | Tools |
|----------|-------|
| File I/O | `FileRead`, `FileWrite`, `FileEdit`, `MultiEdit`, `LS`, `Glob`, `Grep` |
| Execution | `Bash`, `PowerShell`, `REPL` |
| Web | `WebFetch`, `WebSearch` |
| AI Orchestration | `Agent` (sub-agent), `Skill`, `Task` |
| Todo / Tasks | `TodoRead`, `TodoWrite` |
| Interaction | `AskUser`, `NotebookRead`, `NotebookEdit` |
| MCP | `McpTool` (dynamic, from configured servers) |
| Computer Use | `Screenshot`, `Click`, `TypeText`, `Key`, `Scroll`, + 6 more |

---

## License

This project is for educational and research purposes.  
Claude Code is a product of [Anthropic](https://www.anthropic.com) — all original code and IP belongs to them.
