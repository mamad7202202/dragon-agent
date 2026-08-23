#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Dragon Agent desktop app - Tauri shell around the shared core.

use dragon_core::agent::{Agent, AgentEvent};
use dragon_core::config::{Config, ProviderCfg};
use dragon_core::memory::MemoryStore;
use dragon_core::{presets, provider};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

struct Shared {
    cfg: Mutex<Config>,
    memory: Arc<Mutex<MemoryStore>>,
    agent: Mutex<Option<Arc<tokio::sync::Mutex<Agent>>>>,
    model_spec: Mutex<String>,
    rt: tokio::runtime::Runtime,
}

impl Shared {
    fn rebuild_agent(&self) -> Result<(), String> {
        let cfg = self.cfg.lock().unwrap();
        let (pcfg, mid) = cfg
            .resolve_model(None)
            .map_err(|e| format!("{e:#}"))?;
        let p = provider::build(pcfg).map_err(|e| format!("{e:#}"))?;
        let spec = format!("{}/{}", pcfg.name, mid);
        let agent = Agent::new(
            p,
            mid,
            self.memory.clone(),
            cfg.settings.allow_commands,
            cfg.settings.compaction_messages,
        );
        *self.agent.lock().unwrap() = Some(Arc::new(tokio::sync::Mutex::new(agent)));
        *self.model_spec.lock().unwrap() = spec;
        Ok(())
    }
}

// ------------------------------------------------------------------ views

#[derive(Serialize)]
struct AppInfo {
    version: String,
    model_spec: String,
    connected: bool,
    config_path: String,
    data_dir: String,
}

#[derive(Serialize)]
struct FactView {
    id: String,
    content: String,
    importance: f32,
    hits: u32,
}

#[derive(Serialize)]
struct ProvView {
    name: String,
    base_url: String,
    key_hint: String,
    models: Vec<String>,
    default_model: Option<String>,
}

#[derive(Serialize)]
struct SettingsView {
    allow_commands: bool,
    compaction_messages: usize,
    default_model: Option<String>,
}

fn state(s: &State<'_, Shared>) -> (&Mutex<Config>, &Mutex<Option<Arc<tokio::sync::Mutex<Agent>>>>, &Mutex<String>) {
    (&s.cfg, &s.agent, &s.model_spec)
}

// ---------------------------------------------------------------- commands

#[tauri::command]
fn app_info(s: State<'_, Shared>) -> AppInfo {
    let (_, _, model) = state(&s);
    AppInfo {
        version: dragon_core::VERSION.into(),
        model_spec: model.lock().unwrap().clone(),
        connected: s.agent.lock().unwrap().is_some(),
        config_path: Config::path().display().to_string(),
        data_dir: Config::data_dir().display().to_string(),
    }
}

#[tauri::command]
async fn check_update() -> Result<Option<dragon_core::update::UpdateInfo>, String> {
    dragon_core::update::check(dragon_core::VERSION)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn send(text: String, s: State<'_, Shared>, app: AppHandle) -> Result<(), String> {
    let agent = s
        .agent
        .lock()
        .unwrap()
        .clone()
        .ok_or("no provider configured - add one first")?;
    let tx = app.clone();
    s.rt.spawn(async move {
        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let job = {
            let ag = agent.clone();
            let t = text.clone();
            tokio::spawn(async move {
                let mut guard = ag.lock().await;
                guard.turn(&t, atx).await
            })
        };
        while let Some(ev) = arx.recv().await {
            let payload = match &ev {
                AgentEvent::Delta(d) => serde_json::json!({"kind":"delta","text":d}),
                AgentEvent::ToolStart { name, detail } => {
                    serde_json::json!({"kind":"tool","name":name,"detail":detail})
                }
                AgentEvent::ToolEnd { .. } => continue,
                AgentEvent::Compacted => serde_json::json!({"kind":"compacted"}),
                AgentEvent::Stopped => serde_json::json!({"kind":"stopped"}),
                AgentEvent::Error(e) => serde_json::json!({"kind":"error","text":e}),
            };
            let _ = tx.emit("dragon", payload);
        }
        let res = match job.await {
            Ok(Ok(t)) => serde_json::json!({"kind":"done","text":t}),
            Ok(Err(e)) => serde_json::json!({"kind":"done-error","text":format!("{e:#}")}),
            Err(e) => serde_json::json!({"kind":"done-error","text":e.to_string()}),
        };
        let _ = tx.emit("dragon", res);
    });
    Ok(())
}

#[tauri::command]
fn stop(s: State<'_, Shared>) {
    if let Some(ag) = &*s.agent.lock().unwrap() {
        if let Ok(a) = ag.try_lock() {
            a.stop();
        }
    }
}

#[tauri::command]
fn memories(s: State<'_, Shared>) -> Vec<FactView> {
    s.memory
        .lock()
        .unwrap()
        .all()
        .iter()
        .map(|f| FactView {
            id: f.id.clone(),
            content: f.content.clone(),
            importance: f.importance,
            hits: f.hits,
        })
        .collect()
}

#[tauri::command]
fn remember(fact: String, s: State<'_, Shared>) -> String {
    let mut m = s.memory.lock().unwrap();
    let f = m.add(&fact, &["manual".to_string()], 0.8);
    let _ = m.save();
    format!("[{}]", f.id)
}

#[tauri::command]
fn forget(id_prefix: String, s: State<'_, Shared>) -> bool {
    let removed = s.memory.lock().unwrap().remove(&id_prefix);
    let _ = s.memory.lock().unwrap().save();
    removed
}

#[tauri::command]
fn providers(s: State<'_, Shared>) -> Vec<ProvView> {
    let cfg = s.cfg.lock().unwrap();
    cfg.providers
        .iter()
        .map(|p| ProvView {
            name: p.name.clone(),
            base_url: p.base_url.clone(),
            key_hint: if p.api_key.is_empty() {
                "local".into()
            } else {
                format!("{}••••", &p.api_key[..4.min(p.api_key.len())])
            },
            models: p.models.clone(),
            default_model: cfg.default_model.clone(),
        })
        .collect()
}

#[tauri::command]
fn preset_names() -> Vec<String> {
    presets::PRESETS.iter().map(|p| p.name.to_string()).collect()
}

#[tauri::command]
fn preset_detail(name: String) -> Option<presets::Preset> {
    presets::find(&name).copied()
}

#[tauri::command]
fn save_provider(
    name: String,
    url: String,
    key: String,
    models: Vec<String>,
    set_default: bool,
    s: State<'_, Shared>,
) -> Result<(), String> {
    if name.is_empty() || url.is_empty() || models.is_empty() {
        return Err("name, url and at least one model are required".into());
    }
    let kind = if url.contains("anthropic") { "anthropic" } else { "openai" };
    let pcfg = ProviderCfg {
        name: name.trim().to_string(),
        base_url: url.trim().trim_end_matches('/').to_string(),
        api_key: key.trim().to_string(),
        kind: Some(kind.into()),
        models,
    };
    let spec = format!("{}/{}", pcfg.name, pcfg.models[0]);
    {
        let mut cfg = s.cfg.lock().unwrap();
        cfg.providers.retain(|p| p.name != pcfg.name);
        cfg.providers.push(pcfg);
        if set_default || cfg.default_model.is_none() {
            cfg.default_model = Some(spec);
        }
        cfg.save().map_err(|e| format!("{e:#}"))?;
    }
    s.rebuild_agent()?;
    Ok(())
}

#[tauri::command]
fn remove_provider(name: String, s: State<'_, Shared>) -> Result<(), String> {
    {
        let mut cfg = s.cfg.lock().unwrap();
        cfg.providers.retain(|p| p.name != name);
        if let Some(d) = &cfg.default_model {
            if d.split_once('/').map(|(p, _)| p) == Some(name.as_str()) {
                cfg.default_model = None;
            }
        }
        cfg.save().map_err(|e| format!("{e:#}"))?;
    }
    match s.cfg.lock().unwrap().resolve_model(None) {
        Ok(_) => s.rebuild_agent(),
        Err(_) => {
            *s.agent.lock().unwrap() = None;
            *s.model_spec.lock().unwrap() = "(none)".into();
            Ok(())
        }
    }
}

#[tauri::command]
fn set_default(spec: String, s: State<'_, Shared>) -> Result<(), String> {
    {
        let mut cfg = s.cfg.lock().unwrap();
        cfg.default_model = Some(spec);
        cfg.save().map_err(|e| format!("{e:#}"))?;
    }
    s.rebuild_agent()
}

#[tauri::command]
fn get_settings(s: State<'_, Shared>) -> SettingsView {
    let cfg = s.cfg.lock().unwrap();
    SettingsView {
        allow_commands: cfg.settings.allow_commands,
        compaction_messages: cfg.settings.compaction_messages,
        default_model: cfg.default_model.clone(),
    }
}

#[tauri::command]
fn set_settings(
    allow_commands: bool,
    compaction_messages: usize,
    s: State<'_, Shared>,
) -> Result<(), String> {
    {
        let mut cfg = s.cfg.lock().unwrap();
        cfg.settings.allow_commands = allow_commands;
        cfg.settings.compaction_messages = compaction_messages.clamp(12, 400);
        cfg.save().map_err(|e| format!("{e:#}"))?;
    }
    s.rebuild_agent()
}

#[tauri::command]
fn all_model_specs(s: State<'_, Shared>) -> Vec<String> {
    let cfg = s.cfg.lock().unwrap();
    let mut v = Vec::new();
    for p in &cfg.providers {
        for m in &p.models {
            v.push(format!("{}/{}", p.name, m));
        }
    }
    v
}

// ------------------------------------------------------------------- boot

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let cfg = Config::load().unwrap_or_default();
    let memory = Arc::new(Mutex::new(MemoryStore::open().unwrap()));
    let shared = Shared {
        cfg: Mutex::new(cfg),
        memory,
        agent: Mutex::new(None),
        model_spec: Mutex::new("(none)".into()),
        rt,
    };

    tauri::Builder::default()
        .manage(shared)
        .setup(|app| {
            let st: State<Shared> = app.state();
            let _ = st.rebuild_agent();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            check_update,
            send,
            stop,
            memories,
            remember,
            forget,
            providers,
            preset_names,
            preset_detail,
            save_provider,
            remove_provider,
            set_default,
            get_settings,
            set_settings,
            all_model_specs,
        ])
        .run(tauri::generate_context!())
        .expect("error while running dragon gui");
}
