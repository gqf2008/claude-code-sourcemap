# claude-code-sourcemap

[![linux.do](https://img.shields.io/badge/linux.do-huo0-blue?logo=linux&logoColor=white)](https://linux.do)

> [!WARNING]
> This repository is **unofficial** and is reconstructed from the public npm package and source map analysis, **for research purposes only**.
> It does **not** represent the original internal development repository structure.
>
> 本仓库为**非官方**整理版，基于公开 npm 发布包与 source map 分析还原，**仅供研究使用**。
> **不代表**官方原始内部开发仓库结构。
> 一切基于L站"飘然与我同"的情报提供

## 概述

本仓库包含两部分：

1. **restored-src/** — 通过 npm 发布包（`@anthropic-ai/claude-code` v2.1.88）内附 source map 还原的 TypeScript 源码
2. **claude-code-rs/** — 基于还原源码的 Rust 完整移植（11 crate, 204 .rs, ~69.5K LoC, 2048 tests）

## Rust 移植 (claude-code-rs)

功能完整的 Claude Code Rust 实现，详见 [claude-code-rs/ARCHITECTURE.md](claude-code-rs/ARCHITECTURE.md)。

**核心特性：**
- 🔧 28+ 工具（文件/Shell/Web/Git/LSP/Notebook）
- 🤖 多 Agent 协调（coordinator + kameo swarm）
- 🔌 MCP 协议支持（stdio/SSE 传输）
- 🖥️ Computer Use（截屏/点击/键盘）
- 🌉 Bridge 网关（飞书/Telegram/Slack）
- 📡 RPC 接口（TCP/stdio JSON-RPC）
- 🎨 终端 UI（主题/Markdown 渲染/语法高亮 Diff）
- ⚡ 19.8 MB release binary, 38ms 启动

```bash
cd claude-code-rs && cargo build --release
```

## TypeScript 源码还原

- npm 包：[@anthropic-ai/claude-code](https://www.npmjs.com/package/@anthropic-ai/claude-code)
- 还原版本：`2.1.88`
- 还原文件数：**4756 个**（含 1884 个 `.ts`/`.tsx` 源文件）
- 还原脚本：`extract-sources.js`

## 目录结构

```
├── claude-code-rs/       # Rust 移植 (11 crates workspace)
├── restored-src/src/     # 还原的 TS 源码
├── package/              # 原始 npm 包内容
├── extract-sources.js    # source map 提取脚本
├── ARCHITECTURE.md       # TS 版架构分析
├── SWARM_ARCHITECTURE.md # Swarm 架构设计
├── CODE_REVIEW.md        # 代码审查笔记
└── .github/workflows/    # CI/CD
```

## 声明

- 源码版权归 [Anthropic](https://www.anthropic.com) 所有
- 本仓库仅用于技术研究与学习，请勿用于商业用途
- 如有侵权，请联系删除
