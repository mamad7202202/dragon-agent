<div align="center">

```
╔═╗ ╦═╗ ┌─┐ ┌─┐ ┌─┐ ╔╦╗   ┌─┐ ┌─┐ ┌─┐ ╔╦╗ ┌┬┐
║ ║ ╠═╝ ├─┤ │ ┬ │ │ ║║║   ├─┤ │ ┬ ├─┤ ║║║  │
╚═╝ ╩   ┴ ┴ ┴─┘ └─┘ ╩ ╩   └─┘ └─┘ ┴ ┴ ╩ ╩  ┴
```

**A fast, open-source AI agent for your terminal — with a memory that actually lasts.**

*Bring your own key. Own your data. Run anywhere.*

[![Build](https://github.com/mamad7202202/dragon-agent/actions/workflows/build.yml/badge.svg)](https://github.com/mamad7202202/dragon-agent/actions/workflows/build.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-ff6347.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-ff984a.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-windows%20%7C%20linux%20%7C%20macos-66d496.svg)]()

</div>

---

## Why Dragon Agent

Most terminal agents forget everything the moment a session ends. Dragon Agent is built around its **hybrid memory system** from day one, ships as a single native binary (no runtime, no Node), and works with *your* API keys against *any* provider.

## Highlights

| | |
|---|---|
| **Hybrid memory** | Semantic fact store + procedural `MEMORY.md` + episodic sessions + automatic context compaction |
| **Bring your own model** | OpenAI-compatible (OpenRouter, Groq, DeepSeek, Ollama, LM Studio, vLLM...) and Anthropic-native |
| **Real tool use** | Read/write files, grep with regex, list trees, run shell commands |
| **Native speed** | Pure Rust. Single binary per OS. Cold start in milliseconds |
| **Beautiful TUI** | Ember-lit terminal UI built on ratatui — streaming responses, live tool activity |
| **Private by default** | Everything (config, memory, transcripts) lives in your user directory |

## Install

Grab a prebuilt binary from [Actions artifacts](../../actions) or build yourself:

```bash
cargo install --git https://github.com/mamad7202202/dragon-agent
```

## Quick start

```bash
# 1. register any OpenAI-compatible provider
dragon model add openrouter https://openrouter.ai/api/v1 \
  --key sk-or-... --model anthropic/claude-sonnet-4 --default anthropic/claude-sonnet-4

# local models work too - no cloud needed
dragon model add ollama http://localhost:11434/v1 --key ollama --model llama3.1

# or Anthropic's native protocol
dragon model add anthropic https://api.anthropic.com \
  --key sk-ant-... --kind anthropic --model claude-sonnet-4

# 2. launch
dragon

# one-shot mode
dragon run "explain this repo in 5 bullet points"
```

Inside the TUI:

| Key / command | Action |
|---|---|
| `enter` / `shift+enter` | send / newline |
| `pgup` `pgdn` | scroll transcript |
| `ctrl+n` | new session |
| `/model <provider/model>` | switch model mid-session |
| `/remember <fact>` | pin a long-term fact |
| `/memories`, `/forget <id>` | inspect / delete facts |
| `esc` | quit |

## The memory system

```
                ┌──────────────────────────────────────────────┐
 user input ──► │ SYSTEM PROMPT                                │
                │  ├─ persona + tool rules                     │
                │  ├─ PROCEDURAL  MEMORY.md (persistent rules) │
                │  └─ SEMANTIC    top-k recalled facts         │
                │                 cosine(tf) × importance      │
                │                 × recency                    │
                └──────────────────────────────────────────────┘
                        ▲                     ▲
        save_memory ◄───┘                     └─── search_memory
        (the agent decides what matters)

 long sessions ──► COMPACTION: old turns folded into an LLM summary,
                   recent turns kept verbatim

 every session ──► EPISODIC: append-only JSONL transcript, resumable
```

Three layers, one loop:

1. **Semantic** — discrete facts (`facts.json`). The agent calls `save_memory` when it notices something durable; every turn, relevant shards are scored by term-frequency cosine similarity blended with importance and a two-week recency decay, then injected into the prompt.
2. **Procedural** — plain `MEMORY.md`. Standing instructions, always loaded.
3. **Episodic** — full session logs as JSONL under `data/sessions/`.

No vector database required — recall runs locally in microseconds and works fully offline. (Embedding-backed recall is on the roadmap.)

## Configuration

Everything lives in:

| Platform | Path |
|---|---|
| Windows | `%APPDATA%\dragon\config.toml` + `%LOCALAPPDATA%\dragon\` |
| Linux | `~/.config/dragon/` + `~/.local/share/dragon/` |
| macOS | `~/Library/Application Support/dragon/` |

```toml
default_model = "openrouter/anthropic/claude-sonnet-4"

[settings]
allow_commands = false       # gate for the run_shell tool
compaction_messages = 36     # fold history after this many entries

[[providers]]
name = "openrouter"
base_url = "https://openrouter.ai/api/v1"
api_key = "sk-or-..."
models = ["anthropic/claude-sonnet-4", "openai/gpt-4o"]
```

## CLI reference

```
dragon                          interactive TUI
dragon run "<prompt>"           one-shot answer
dragon models                   list providers & models
dragon model add <name> <url> --key ... --model ...
dragon model remove <name>
dragon sessions                 list past sessions
dragon memory list|add|forget|clear
dragon set allow_commands true  enable shell tool
dragon where                    show config/data paths
```

## Building

Requires Rust 1.75+:

```bash
cargo build --release
```

GitHub Actions builds `windows-x64`, `linux-x64` and `macos-arm64` on every push; tags starting with `v` are published as releases automatically.

## Roadmap

- [ ] Embedding-backed semantic recall (optional, provider-side)
- [ ] Session resume from the TUI
- [ ] MCP tool servers
- [ ] Multi-file diff review before writes

## Author

**mamad720220** · [Telegram @mamad720220](https://t.me/mamad720220)

Inspired by the ideas of [opencode](https://github.com/sst/opencode), [Letta/MemGPT](https://github.com/letta-ai/letta) and [mem0](https://github.com/mem0ai/mem0).

Licensed under [MIT](LICENSE).
