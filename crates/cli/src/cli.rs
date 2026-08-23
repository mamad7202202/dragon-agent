//! Command-line interface: subcommands, BYOK setup, one-shot mode.

use dragon_core::agent::{Agent, AgentEvent};
use dragon_core::config::{Config, ProviderCfg};
use dragon_core::memory::MemoryStore;
use dragon_core::provider;
use dragon_core::session;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "dragon",
    version = concat!("v", env!("CARGO_PKG_VERSION")),
    about = "Dragon Agent - a fast terminal AI agent with a long memory",
    after_help = "docs & source: https://github.com/mamad7202202/dragon-agent"
)]
pub struct Cli {
    /// Override the model for this run (format: provider/model-id)
    #[arg(short, long)]
    pub model: Option<String>,

    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// One-shot mode: answer a prompt in plain text and exit
    Run {
        prompt: String,
        /// Disable tool use (plain chat)
        #[arg(long)]
        no_tools: bool,
    },
    /// Quick setup from a built-in preset
    Setup {
        /// google | openrouter | openai | anthropic | groq | deepseek | ollama |
        /// lmstudio | custom (requires --url)
        #[arg(long)]
        preset: String,
        #[arg(long)]
        key: Option<String>,
        #[arg(long = "key-env")]
        key_env: Option<String>,
        /// Base URL when --preset custom
        #[arg(long)]
        url: Option<String>,
        /// Model id override (defaults to the preset's first model)
        #[arg(long = "model")]
        model: Option<String>,
        /// Provider name override (default: preset name)
        #[arg(long)]
        name: Option<String>,
    },
    /// List configured providers and their models
    Models,
    /// Register a provider + models (bring your own key)
    ModelAdd {
        name: String,
        base_url: String,
        #[arg(long)]
        key: Option<String>,
        /// Read the key from an environment variable instead
        #[arg(long = "key-env")]
        key_env: Option<String>,
        /// Protocol: openai | anthropic (auto-detected if omitted)
        #[arg(long)]
        kind: Option<String>,
        /// Model ids offered by this provider
        #[arg(long = "model", num_args = 1..)]
        models: Vec<String>,
        /// Set this as the default model (provider/model-id)
        #[arg(long)]
        default: Option<String>,
    },
    /// Remove a provider
    ModelRemove { name: String },
    /// List past sessions
    Sessions,
    /// Long-term memory operations
    Memory {
        #[command(subcommand)]
        cmd: MemoryCmd,
    },
    /// Print where config and data live
    Where,
    /// Change a setting (allow_commands, default_model, compaction_messages)
    Set {
        key: String,
        value: String,
    },
}

#[derive(Subcommand)]
pub enum MemoryCmd {
    List,
    Add { fact: String },
    Forget { id_prefix: String },
    Clear,
}

pub fn setup_instructions() -> String {
    format!(
        "quickest - use a preset:\n  \
         dragon setup --preset google --key AIza...      Google AI Studio (Gemini)\n  \
         dragon setup --preset openrouter --key sk-or-...\n  \
         dragon setup --preset anthropic --key sk-ant-...\n  \
         dragon setup --preset ollama                    local models, no cloud\n\n\
         presets: google | openrouter | openai | anthropic | groq | deepseek | ollama | lmstudio\n\
         custom endpoint:\n  \
         dragon setup --preset custom --url https://my.box/v1 --key k --model m1\n\n\
         or just run `dragon` and the interactive /setup wizard walks you through it.\n\
         config lives at {}",
        Config::path().display()
    )
}

#[allow(clippy::too_many_arguments)]
fn setup_preset(
    preset: &str,
    key: Option<String>,
    key_env: Option<String>,
    url: Option<String>,
    model: Option<String>,
    name: Option<String>,
) -> Result<()> {
    let mut cfg = Config::load()?;

    let (pname, base_url, kind, default_models, note): (
        String,
        String,
        &'static str,
        Vec<String>,
        &'static str,
    ) = if preset.eq_ignore_ascii_case("custom") {
        let u = url.clone().ok_or_else(|| {
            anyhow::anyhow!("--preset custom requires --url <base-url>")
        })?;
        let k = if u.contains("anthropic") { "anthropic" } else { "openai" };
        (
            name.clone().unwrap_or_else(|| "custom".into()),
            u.trim_end_matches('/').to_string(),
            k,
            vec![],
            "",
        )
    } else {
        let p = dragon_core::presets::find(preset)
            .with_context(|| format!("unknown preset '{preset}'"))?;
        (
            name.clone().unwrap_or_else(|| p.name.to_string()),
            p.base_url.to_string(),
            p.kind,
            p.models.iter().map(|m| m.to_string()).collect(),
            p.note,
        )
    };

    let model_id = match model {
        Some(m) => m,
        None => default_models.first().cloned().ok_or_else(|| {
            anyhow::anyhow!("no preset model available - pass --model <id>")
        })?,
    };

    let api_key = match (&key, &key_env) {
        (Some(k), _) => k.clone(),
        (None, Some(env)) => {
            std::env::var(env).with_context(|| format!("env var {env} is not set"))?
        }
        _ => String::new(),
    };

    if !note.is_empty() {
        println!("note: {note}");
    }

    let spec = format!("{pname}/{model_id}");
    // replace existing provider with same name
    cfg.providers.retain(|p| p.name != pname);
    cfg.providers.push(ProviderCfg {
        name: pname,
        base_url,
        api_key,
        kind: Some(kind.to_string()),
        models: vec![model_id],
    });
    cfg.default_model = Some(spec.clone());
    cfg.save()?;
    println!("saved '{spec}'.\nrun `dragon` to start chatting.");
    Ok(())
}

pub async fn dispatch(cmd: Cmd, model_override: Option<String>) -> Result<()> {
    match cmd {
        Cmd::Run { prompt, no_tools } => run_oneshot(&prompt, model_override, !no_tools).await?,
        Cmd::Setup {
            preset,
            key,
            key_env,
            url,
            model,
            name,
        } => setup_preset(&preset, key, key_env, url, model, name)?,
        Cmd::Models => models_list()?,
        Cmd::ModelAdd {
            name,
            base_url,
            key,
            key_env,
            kind,
            models,
            default,
        } => model_add(name, base_url, key, key_env, kind, models, default)?,
        Cmd::ModelRemove { name } => {
            let mut cfg = Config::load()?;
            let before = cfg.providers.len();
            cfg.providers.retain(|p| p.name != name);
            if cfg.providers.len() == before {
                bail!("provider '{name}' not found");
            }
            if let Some(d) = &cfg.default_model {
                if d.split_once('/').map(|(p, _)| p) == Some(name.as_str()) {
                    cfg.default_model = None;
                }
            }
            cfg.save()?;
            println!("removed '{name}'.");
        }
        Cmd::Sessions => sessions_list(),
        Cmd::Memory { cmd } => memory_cmd(cmd)?,
        Cmd::Where => {
            println!("config : {}", Config::path().display());
            println!("data   : {}", Config::data_dir().display());
        }
        Cmd::Set { key, value } => set_setting(&key, &value)?,
    }
    Ok(())
}

// ------------------------------------------------------------------ handlers

async fn run_oneshot(prompt: &str, model_override: Option<String>, tools: bool) -> Result<()> {
    let config = Config::load()?;
    let memory = std::sync::Arc::new(std::sync::Mutex::new(MemoryStore::open()?));
    let (pcfg, mid) = config.resolve_model(model_override.as_deref())?;
    let p = provider::build(pcfg)?;
    let mut agent =
        Agent::new(p, mid.to_string(), memory, config.settings.allow_commands, config.settings.compaction_messages);
    agent.tools_enabled = tools;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Delta(d) => {
                    print!("{d}");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
                AgentEvent::ToolStart { name, detail } => {
                    eprintln!("\x1b[2m» {name} {detail}\x1b[0m");
                }
                AgentEvent::Compacted => eprintln!("\x1b[2m· context compacted\x1b[0m"),
                AgentEvent::Stopped => eprintln!("\x1b[2m· stopped\x1b[0m"),
                AgentEvent::ApprovalRequest { tool, .. } => {
                    eprintln!("\x1b[33m» approval needed: {tool}\x1b[0m")
                }
                AgentEvent::Error(e) => eprintln!("\x1b[31merror: {e}\x1b[0m"),
                AgentEvent::ToolEnd { .. } => {}
            }
        }
    });

    let out = agent.turn(prompt, tx).await?;
    printer.abort();
    println!();
    Ok(())
}

fn models_list() -> Result<()> {
    let cfg = Config::load()?;
    if cfg.providers.is_empty() {
        println!("no providers configured yet.\n\n{}", setup_instructions());
        return Ok(());
    }
    for p in &cfg.providers {
        let default_mark = |m: &str| {
            if cfg.default_model.as_deref() == Some(&format!("{}/{}", p.name, m)) {
                " *"
            } else {
                ""
            }
        };
        println!("{}", p.name);
        println!("  url: {}", p.base_url);
        if p.models.is_empty() {
            println!("  (no models registered - edit config to add)");
        }
        for m in &p.models {
            println!("  - {}{}", m, default_mark(m));
        }
        println!();
    }
    if let Some(d) = &cfg.default_model {
        println!("default: {d}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn model_add(
    name: String,
    base_url: String,
    key: Option<String>,
    key_env: Option<String>,
    kind: Option<String>,
    models: Vec<String>,
    default: Option<String>,
) -> Result<()> {
    let mut cfg = Config::load()?;
    if name.is_empty() || name.contains('/') || name.contains(' ') {
        bail!("invalid provider name '{name}': no spaces or '/' allowed");
    }
    if cfg.find_provider(&name).is_some() {
        bail!("provider '{name}' already exists (edit {} to change it)", Config::path().display());
    }

    let api_key = match (&key, &key_env) {
        (Some(k), _) => k.clone(),
        (None, Some(env)) => std::env::var(env)
            .with_context(|| format!("env var {env} is not set"))?,
        _ => bail!("provide --key <KEY> or --key-env <VAR>"),
    };

    if let Some(k) = &kind {
        if k != "openai" && k != "anthropic" {
            bail!("kind must be 'openai' or 'anthropic'");
        }
    }

    cfg.providers.push(ProviderCfg {
        name: name.clone(),
        base_url,
        api_key,
        kind,
        models,
    });
    match default {
        Some(d) => {
            // accept bare model id -> prefix with provider
            cfg.default_model = Some(if d.contains('/') { d } else { format!("{name}/{d}") });
        }
        None if cfg.default_model.is_none() => {
            if let Some(first) = cfg.providers.last().and_then(|p| p.models.first()) {
                cfg.default_model = Some(format!("{}/{first}", name));
            }
        }
        None => {}
    }
    cfg.save()?;
    println!(
        "saved '{}'. default model: {}\nrun `dragon` to start.",
        name,
        cfg.default_model.as_deref().unwrap_or("(unset)")
    );
    Ok(())
}

fn sessions_list() {
    let all = session::list_sessions();
    if all.is_empty() {
        println!("no sessions yet.");
        return;
    }
    for (i, (_path, meta)) in all.iter().enumerate() {
        println!(
            "{:>3}. [{}] {} ({})",
            i,
            &meta.id[..8.min(meta.id.len())],
            meta.title,
            meta.created_at.chars().take(16).collect::<String>().replace('T', " ")
        );
    }
}

fn memory_cmd(cmd: MemoryCmd) -> Result<()> {
    let mut mem = MemoryStore::open()?;
    match cmd {
        MemoryCmd::List => {
            let facts = mem.all().to_vec();
            if facts.is_empty() {
                println!("memory is empty. add facts with /remember inside dragon,");
                println!("or: dragon memory add \"user prefers dark mode\"");
            }
            for f in facts {
                println!("[{}] ({:.1}) {}", f.id, f.importance, f.content);
            }
        }
        MemoryCmd::Add { fact } => {
            let f = mem.add(&fact, &[], 0.7);
            mem.save()?;
            println!("saved [{}]", f.id);
        }
        MemoryCmd::Forget { id_prefix } => {
            if mem.remove(&id_prefix) {
                mem.save()?;
                println!("forgotten.");
            } else {
                println!("no matching id.");
            }
        }
        MemoryCmd::Clear => {
            mem.clear();
            mem.save()?;
            println!("memory cleared.");
        }
    }
    Ok(())
}

fn set_setting(key: &str, value: &str) -> Result<()> {
    let mut cfg = Config::load()?;
    match key {
        "allow_commands" => {
            cfg.settings.allow_commands =
                value.parse().context("expected true or false")?;
        }
        "compaction_messages" => {
            cfg.settings.compaction_messages =
                value.parse().context("expected a number")?;
        }
        "default_model" => cfg.default_model = Some(value.to_string()),
        other => bail!(
            "unknown setting '{other}' (known: allow_commands, compaction_messages, default_model)"
        ),
    }
    cfg.save()?;
    println!("{key} = {value}");
    Ok(())
}
