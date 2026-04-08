# CLAUDE.md

Non-official Rust port of `@anthropic-ai/claude-code` (v2.1.88).

## Build & Test

```bash
cargo check                # Type-check only
cargo build                # Full build
cargo test                 # Run all 128+ tests
cargo test -p claude-agent # Test specific crate
cargo clippy               # Lint
cargo fmt --check          # Format check
```

## API Setup

Required environment variables (one of):
- `ANTHROPIC_API_KEY` — Anthropic API key
- `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, etc. — For alternative providers

Or OAuth via `~/.claude/.credentials.json` (official CLI login).

## Run

```bash
cargo run -- --help
cargo run -- "your prompt here"
cargo run -- -m claude-opus-4-20250514 --thinking
```

## Providers

`--provider` supports: `anthropic`, `openai`, `deepseek`, `ollama`, `together`, `groq`, `bedrock`, `vertex`

Use `--base-url` to override API endpoint (e.g., LiteLLM, DashScope).

## Architecture

5-crate workspace with zero circular dependencies:

| Crate | Role |
|-------|------|
| `claude-cli` | Binary entry, REPL, CLI parsing |
| `claude-agent` | Agent loop, hooks, permissions, compression |
| `claude-api` | HTTP client, SSE streaming, OAuth PKCE |
| `claude-tools` | 28+ tools, MCP client, ToolRegistry |
| `claude-core` | Base types, Tool trait, config |

Dependency flow: `cli → agent → {api, tools} → core`

## Gotchas

- **Model validation**: Strict model/provider matching unless `--base-url` is set (for compatible APIs like DashScope).
- **OAuth tokens**: Stored with millisecond expiry in `~/.claude/.credentials.json`.
- **Session resume**: `--resume` restores latest; `--session-id <id>` restores specific.
- **MCP servers**: Auto-discovered from `.mcp.json` at project/user roots.
