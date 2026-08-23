#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Dragon Agent desktop app - Tauri shell around the shared core.
//! Sessions, modes, approvals and the update checker all live here.

use dragon_core::agent::{Agent, AgentEvent, Mode};
use dragon_core::config::{Config, ProviderCfg};
use dragon_core::memory::MemoryStore;
use dragon_core::presets;
use dragon_core::provider;
use dragon_core::session::{self, SessionLog};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

struct Shared {
    cfg: Mutex<Config>,
    memory: Arc<Mutex<MemoryStore>>,
    agent: Mutex<Option<Arc<tokio::sync::Mutex<Agent>>>>,
    model_spec: Mutex<String>,
    mode: Mutex<Mode>,
    session_id: Mutex<String>,
    rt: tokio::runtime::Runtime,
}

impl Shared {
    fn rebuild_agent(&self) -> Result<(), String> {
        let cfg = self.cfg.lock().unwrap().clone();
        let (pcfg, mid) = cfg.resolve_model(None).map_err(|e| format!("{e:#}"))?;
        let p = provider::build(pcfg).map_err(|e| format!("{e:#}"))?;
        let spec = format!("{}/{}", pcfg.name, mid);
        let mut agent = Agent::new(
            p,
            mid,
            self.memory.clone(),
            cfg.settings.allow_commands,
            cfg.settings.compaction_messages,
        );
        let sid = self.session_id.lock().unwrap().clone();
        let mode = *self.mode.lock().unwrap();
        agent.set_session(if sid.is_empty() { None } else { Some(&sid) });
        agent.set_mode(mode);
        agent.set_auto_approve(cfg.settings.auto_approve.clone());
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
    mode: String,
    session_id: String,
    theme: String,
    config_path: String,
}

#[derive(Serialize)]
struct FactView {
    id: String,
    content: String,
    importance: f32,
    hits: u32,
    scope: String,
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
struct SessionView {
    id: String,
    title: String,
    model: String,
    created_at: String,
    current: bool,
}

fn fact_view(f: &dragon_core::memory::Fact, cur: &str) -> FactView {
    let scope = match &f.session {
        Some(s) if s == cur => "session".into(),
        Some(_) => "other".into(),
        None => "global".into(),
    };
    FactView {
        id: f.id.clone(),
        content: f.content.clone(),
        importance: f.importance,
        hits: f.hits,
        scope,
    }
}

// ---------------------------------------------------------------- commands

#[tauri::command]
fn app_info(s: State<'_, Shared>) -> AppInfo {
    let sid = s.session_id.lock().unwrap().clone();
    AppInfo {
        version: dragon_core::VERSION.into(),
        model_spec: s.model_spec.lock().unwrap().clone(),
        connected: s.agent.lock().unwrap().is_some(),
        mode: s.mode.lock().unwrap().as_str().into(),
        session_id: if sid.is_empty() { "(none)".into() } else { sid },
        theme: s.cfg.lock().unwrap().settings.theme.clone(),
        config_path: Config::path().display().to_string(),
    }
}

#[tauri::command]
async fn check_update() -> Result<Option<dragon_core::update::UpdateInfo>, String> {
    dragon_core::update::check(dragon_core::VERSION)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// One-shot independent explanation of a pending action.
#[tauri::command]
async fn explain_action(
    tool: String,
    detail: String,
    s: State<'_, Shared>,
) -> Result<String, String> {
    let (prov, model) = {
        let guard = s.agent.lock().unwrap().clone().ok_or("not connected")?;
        let g = guard.lock().await;
        (g.provider.clone(), g.model.clone())
    };
    let sys = "You are a neutral security explainer. In at most 3 short sentences describe \
               what this action would do to the user's machine and its risk level \
               (low/medium/high). Do not reference any conversation.";
    let q = format!("Action: tool={tool}\narguments={detail}");
    provider::complete(prov, &model, Some(sys), &[dragon_core::provider::Message::user(q)])
        .await
        .map(|t| t.trim().to_string())
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn send(text: String, s: State<'_, Shared>, app: AppHandle) -> Result<(), String> {
    // keep agent extras in sync with the active session/mode
    {
        let sid = s.session_id.lock().unwrap().clone();
        let mode = *s.mode.lock().unwrap();
        let auto = s.cfg.lock().unwrap().settings.auto_approve.clone();
        if let Some(ag) = &*s.agent.lock().unwrap() {
            if let Ok(mut g) = ag.try_lock() {
                g.set_session(if sid.is_empty() { None } else { Some(&sid) });
                g.set_mode(mode);
                g.set_auto_approve(auto);
            }
        }
    }
    let agent = s
        .agent
        .lock()
        .unwrap()
        .clone()
        .ok_or("no provider configured - add one first")?;

    // log user side of the transcript
    {
        let dir = SessionLog::sessions_dir();
        let _ = std::fs::create_dir_all(&dir);
        let sid = s.session_id.lock().unwrap().clone();
        if !sid.is_empty() {
            let p = dir.join(format!("{sid}.jsonl"));
            if p.exists() {
                if let Ok((mut log, _)) = SessionLog::resume(&p) {
                    let _ = log.append_message(&dragon_core::provider::Message::user(&text));
                    log.set_title_if_new(&text);
                }
            }
        }
    }

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
                AgentEvent::ApprovalRequest { id, tool, detail } => {
                    serde_json::json!({"kind":"approval","id":id,"tool":tool,"detail":detail})
                }
                AgentEvent::Compacted => serde_json::json!({"kind":"compacted"}),
                AgentEvent::Stopped => serde_json::json!({"kind":"stopped"}),
                AgentEvent::Error(e) => serde_json::json!({"kind":"error","text":e}),
                AgentEvent::ToolEnd { .. } => continue,
            };
            let _ = tx.emit("dragon", payload);
        }
        let res = match job.await {
            Ok(Ok(t)) => serde_json::json!({"kind":"done","text":t}),
            Ok(Err(e)) => serde_json::json!({"kind":"done-error","text":format!("{e:#}")}),
            Err(e) => serde_json::json!({"kind":"done-error","text":e.to_string()}),
        };
        // persist assistant side
        if let Some(text_out) = res.get("text").and_then(|v| v.as_str()) {
            let sid_guard = tx.state::<Shared>().session_id.lock().unwrap().clone();
            let dir = SessionLog::sessions_dir();
            let p = dir.join(format!("{sid_guard}.jsonl"));
            if !sid_guard.is_empty() && p.exists() {
                if let Ok((mut log, _)) = SessionLog::resume(&p) {
                    let _ = log.append_message(&dragon_core::provider::Message::assistant(text_out));
                }
            }
        }
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
fn respond_approval(
    id: u64,
    allowed: bool,
    always: bool,
    tool: String,
    s: State<'_, Shared>,
) -> Result<(), String> {
    if let Some(ag) = &*s.agent.lock().unwrap() {
        if let Ok(g) = ag.try_lock() {
            g.respond(id, allowed);
        }
    }
    if allowed && always {
        let mut cfg = s.cfg.lock().unwrap();
        if !cfg.settings.auto_approve.contains(&tool) {
            cfg.settings.auto_approve.push(tool);
        }
        cfg.save().map_err(|e| format!("{e:#}"))?;
        drop(cfg);
        s.rebuild_agent()?;
    }
    Ok(())
}

#[tauri::command]
fn memories(s: State<'_, Shared>) -> Vec<FactView> {
    let cur = s.session_id.lock().unwrap().clone();
    s.memory
        .lock()
        .unwrap()
        .all()
        .iter()
        .map(|f| fact_view(f, &cur))
        .collect()
}

#[tauri::command]
fn remember(fact: String, scope: String, s: State<'_, Shared>) -> String {
    let mut m = s.memory.lock().unwrap();
    let sid = s.session_id.lock().unwrap().clone();
    let scoped = if scope == "global" { None } else { Some(sid.as_str()) };
    let f = m.add_scoped(&fact, &["manual".to_string()], 0.8, scoped);
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
    presets::find(&name).cloned()
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
fn get_settings(s: State<'_, Shared>) -> serde_json::Value {
    let cfg = s.cfg.lock().unwrap();
    serde_json::json!({
        "allow_commands": cfg.settings.allow_commands,
        "compaction_messages": cfg.settings.compaction_messages,
        "default_model": cfg.default_model,
        "theme": cfg.settings.theme,
        "auto_approve": cfg.settings.auto_approve,
    })
}

#[tauri::command]
fn set_settings(
    allow_commands: bool,
    compaction_messages: usize,
    theme: Option<String>,
    s: State<'_, Shared>,
) -> Result<(), String> {
    {
        let mut cfg = s.cfg.lock().unwrap();
        cfg.settings.allow_commands = allow_commands;
        cfg.settings.compaction_messages = compaction_messages.clamp(12, 400);
        if let Some(t) = theme {
            cfg.settings.theme = t;
        }
        cfg.save().map_err(|e| format!("{e:#}"))?;
    }
    s.rebuild_agent()
}

// ------------------------------------------------------------- sessions

#[tauri::command]
fn sessions(s: State<'_, Shared>) -> Vec<SessionView> {
    let cur = s.session_id.lock().unwrap().clone();
    session::list_sessions()
        .into_iter()
        .take(30)
        .map(|(_p, meta)| {
            let current = meta.id == cur;
            SessionView {
                id: meta.id,
                title: meta.title,
                model: meta.model,
                created_at: meta.created_at.chars().take(16).collect::<String>().replace('T', " "),
                current,
            }
        })
        .collect()
}

#[tauri::command]
fn new_session(mode: String, s: State<'_, Shared>) -> Result<(), String> {
    let m = Mode::parse(&mode).unwrap_or(Mode::Agent);
    let model = s.model_spec.lock().unwrap().clone();
    let log = SessionLog::create(&model).map_err(|e| format!("{e:#}"))?;
    let id = log.meta().id.clone();
    *s.session_id.lock().unwrap() = id.clone();
    *s.mode.lock().unwrap() = m;
    if let Some(ag) = &*s.agent.lock().unwrap() {
        if let Ok(mut g) = ag.try_lock() {
            g.reset();
            g.set_session(Some(&id));
            g.set_mode(m);
        }
    }
    Ok(())
}

#[tauri::command]
fn load_session(id: String, s: State<'_, Shared>) -> Result<Vec<serde_json::Value>, String> {
    let dir = SessionLog::sessions_dir();
    let path = dir.join(format!("{id}.jsonl"));
    let (log, msgs) = SessionLog::resume(&path).map_err(|e| format!("{e:#}"))?;
    *s.session_id.lock().unwrap() = log.meta().id.clone();
    if let Some(ag) = &*s.agent.lock().unwrap() {
        if let Ok(mut g) = ag.try_lock() {
            g.reset();
            g.history = msgs.iter().cloned().collect();
            g.set_session(Some(&log.meta().id));
        }
    }
    Ok(msgs
        .into_iter()
        .filter(|m| {
            matches!(
                m.role,
                dragon_core::provider::Role::User | dragon_core::provider::Role::Assistant
            )
        })
        .map(|m| {
            serde_json::json!({
                "role": if m.role == dragon_core::provider::Role::User { "user" } else { "assistant" },
                "content": m.content,
            })
        })
        .collect())
}

#[tauri::command]
fn set_mode(mode: String, s: State<'_, Shared>) -> Result<(), String> {
    let m = Mode::parse(&mode).ok_or("unknown mode")?;
    *s.mode.lock().unwrap() = m;
    if let Some(ag) = &*s.agent.lock().unwrap() {
        if let Ok(mut g) = ag.try_lock() {
            g.set_mode(m);
        }
    }
    Ok(())
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

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let cfg = Config::load().unwrap_or_default();
    let memory = Arc::new(Mutex::new(MemoryStore::open().unwrap()));
    let mode = Mode::parse(&cfg.settings.default_mode).unwrap_or(Mode::Agent);
    let shared = Shared {
        cfg: Mutex::new(cfg),
        memory,
        agent: Mutex::new(None),
        model_spec: Mutex::new("(none)".into()),
        mode: Mutex::new(mode),
        session_id: Mutex::new(String::new()),
        rt,
    };

    tauri::Builder::default()
        .manage(shared)
        .setup(|app| {
            let st: State<Shared> = app.state();
            // open (or create) the most recent session at boot
            if let Some((_p, meta)) = session::list_sessions().first() {
                *st.session_id.lock().unwrap() = meta.id.clone();
            }
            let _ = st.rebuild_agent();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            check_update,
            explain_action,
            send,
            stop,
            respond_approval,
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
            sessions,
            new_session,
            load_session,
            set_mode,
            all_model_specs,
        ])
        .run(tauri::generate_context!())
        .expect("error while running dragon gui");
}
