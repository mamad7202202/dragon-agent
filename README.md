<div align="center">

```
██████╗ ██████╗  █████╗  ██████╗  ██████╗ ███╗   ██╗
██╔══██╗██╔══██╗██╔══██╗██╔════╝ ██╔═══██╗████╗  ██║
██║  ██║██████╔╝███████║██║  ███╗██║   ██║██╔██╗ ██║
██║  ██║██╔══██╗██╔══██║██║   ██║██║   ██║██║╚██╗██║
██████╔╝██║  ██║██║  ██║╚██████╔╝╚██████╔╝██║ ╚████║
╚═════╝ ╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝  ╚═════╝ ╚═╝  ╚═══╝

 █████╗  ██████╗ ███████╗███╗   ██╗████████╗
██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝
███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║   
██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║   
██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║   
╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝
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

One line — Linux & macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/mamad7202202/dragon-agent/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/mamad7202202/dragon-agent/main/install.ps1 | iex
```

Both pull the raw executable straight from the rolling [`latest` release](https://github.com/mamad7202202/dragon-agent/releases/tag/latest), which is refreshed automatically on every build.

Or with cargo:

```bash
cargo install --git https://github.com/mamad7202202/dragon-agent
```

**Try the interactive demo in your browser first:** [mamad7202202.github.io/dragon-agent](https://mamad7202202.github.io/dragon-agent/) *(static page, runs entirely client-side)*

## Quick start

On first launch `dragon` opens an **interactive setup wizard**: pick a provider from the list, paste your key, choose a model — done. The key is stored only on your machine.

Prefer the terminal? Presets make it a single command:

```bash
# Google AI Studio (Gemini) - free tier available
dragon setup --preset google --key AIza...

# OpenRouter - 400+ models behind one key
dragon setup --preset openrouter --key sk-or-...

# local models - no cloud at all
dragon setup --preset ollama

# any other OpenAI-compatible endpoint
dragon setup --preset custom --url https://my.box/v1 --key k --model m1
```

Built-in presets: `google` · `openrouter` · `openai` · `anthropic` · `groq` · `deepseek` · `ollama` · `lmstudio`

Switch models any time inside the TUI (`/model`, `/setup`) or mix providers freely — each provider keeps its own key and model list.

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

GitHub Actions builds raw executables for `windows-x64`, `linux-x64` and `macos-arm64` on every push — attached directly (no archives) to the rolling [`latest` release](https://github.com/mamad7202202/dragon-agent/releases/tag/latest). Version tags starting with `v` get their own formal release.

## Roadmap

- [ ] Embedding-backed semantic recall (optional, provider-side)
- [ ] Session resume from the TUI
- [ ] MCP tool servers
- [ ] Multi-file diff review before writes

## Author

**mamad720220** · [Telegram @mamad720220](https://t.me/mamad720220)

Inspired by the ideas of [opencode](https://github.com/sst/opencode), [Letta/MemGPT](https://github.com/letta-ai/letta) and [mem0](https://github.com/mem0ai/mem0).

Licensed under [MIT](LICENSE).
