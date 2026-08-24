//! Dragon Agent — desktop app, pure Rust (dgpui: winit + softbuffer + tiny-skia + cosmic-text).

mod ui;

use anyhow::Result;
use dragon_core::agent::{Agent, AgentEvent, Mode};
use dragon_core::config::Config;
use dragon_core::memory::graph::GraphStore;
use dragon_core::memory::MemoryStore;
use dragon_core::presets;
use dragon_core::provider;
use dragon_core::session::{self, SessionLog};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use ui::*;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::Window;

#[derive(Debug)]
enum Ev {
    Delta(String),
    Tool(String, String),
    Approval(u64, String, String),
    Usage(u64, u64, u64),
    Tasks(serde_json::Value),
    Compacted,
    Stopped,
    Error(String),
    Done(Result<String, String>),
    Explain(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Chat,
    Memory,
    Providers,
    Settings,
    About,
}

#[derive(Clone)]
enum Item {
    User(String),
    Assistant(String),
    Tool(String, String),
    Approval(u64, String, String),
    System(String),
    Tasks(Vec<(String, bool)>),
}

struct Field {
    value: String,
    caret: usize,
}
impl Field {
    fn new() -> Self {
        Self { value: String::new(), caret: 0 }
    }
    fn insert(&mut self, c: char) {
        let b: usize = self.value.chars().take(self.caret).map(char::len_utf8).sum();
        self.value.insert(b, c);
        self.caret += 1;
    }
    fn back(&mut self) {
        if self.caret > 0 {
            self.caret -= 1;
            let b: usize = self.value.chars().take(self.caret).map(char::len_utf8).sum();
            self.value.remove(b);
        }
    }
}

struct Model {
    cfg: Config,
    memory: Arc<Mutex<MemoryStore>>,
    graph: Arc<Mutex<GraphStore>>,
    agent: Option<Arc<tokio::sync::Mutex<Agent>>>,
    model_spec: String,
    mode: Mode,
    session_id: String,
    tab: Tab,
    items: Vec<Item>,
    draft: Field,
    streaming: Option<String>,
    busy: bool,
    status: String,
    usage_total: u64,
    pending: Option<(u64, String, String)>,
    scroll: i32,
    focus: Option<String>,
    form_open: bool,
    f_name: Field,
    f_url: Field,
    f_key: Field,
    f_models: Field,
    pv_idx: usize,
    toast: Option<(String, f32)>,
    update_note: Option<String>,
}

impl Model {
    fn new(cfg: Config, memory: Arc<Mutex<MemoryStore>>, graph: Arc<Mutex<GraphStore>>) -> Self {
        let mode = Mode::parse(&cfg.settings.default_mode).unwrap_or(Mode::Agent);
        let mut m = Self {
            cfg,
            memory,
            graph,
            agent: None,
            model_spec: "(none)".into(),
            mode,
            session_id: String::new(),
            tab: Tab::Chat,
            items: vec![],
            draft: Field::new(),
            streaming: None,
            busy: false,
            status: "ready".into(),
            usage_total: 0,
            pending: None,
            scroll: 0,
            focus: Some("draft".into()),
            form_open: false,
            f_name: Field::new(),
            f_url: Field::new(),
            f_key: Field::new(),
            f_models: Field::new(),
            pv_idx: 0,
            toast: None,
            update_note: None,
        };
        // resume newest session
        if let Some((_p, meta)) = session::list_sessions().first() {
            let path = SessionLog::sessions_dir().join(format!("{}.jsonl", meta.id));
            if let Ok((log, msgs)) = SessionLog::resume(&path) {
                m.session_id = log.meta().id.clone();
                for msg in msgs {
                    match msg.role {
                        dragon_core::provider::Role::User => m.items.push(Item::User(msg.content)),
                        dragon_core::provider::Role::Assistant => m.items.push(Item::Assistant(msg.content)),
                        _ => {}
                    }
                }
            }
        }
        if m.session_id.is_empty() {
            let log = SessionLog::create("(none)").ok();
            if let Some(log) = log {
                m.session_id = log.meta().id.clone();
            }
        }
        if let Err(e) = m.rebuild_agent() {
            m.items.push(Item::System(format!("welcome — add a provider to begin ({e})")));
            m.tab = Tab::Providers;
            m.form_open = true;
        } else {
            m.items.push(Item::Assistant(format!(
                "**DRAGON AGENT** v{}\nconnected via {}",
                dragon_core::VERSION,
                m.model_spec
            )));
        }
        m
    }

    fn rebuild_agent(&mut self) -> Result<()> {
        let (pcfg, mid) = self.cfg.resolve_model(None)?;
        let p = provider::build(pcfg)?;
        let spec = format!("{}/{}", pcfg.name, mid);
        let mut a = Agent::new(
            p,
            mid,
            self.memory.clone(),
            self.cfg.settings.allow_commands,
            self.cfg.settings.compaction_messages,
        );
        self.sync_extras(&mut a);
        self.agent = Some(Arc::new(tokio::sync::Mutex::new(a)));
        self.model_spec = spec;
        Ok(())
    }

    fn sync_extras(&self, a: &mut Agent) {
        a.set_mode(self.mode);
        a.set_session(if self.session_id.is_empty() { None } else { Some(&self.session_id) });
        a.set_auto_approve(self.cfg.settings.auto_approve.clone());
        a.set_thinking(self.cfg.settings.thinking_level());
        a.set_engine(if self.cfg.settings.graph_memory() { Some(self.graph.clone()) } else { None });
    }

    fn persist(&self) {
        let _ = self.cfg.save();
    }

    fn send(&mut self, tx: &UnboundedSender<Ev>) {
        if self.draft.value.trim().is_empty() || self.busy {
            return;
        }
        let Some(agent) = self.agent.clone() else {
            self.items.push(Item::System("no provider configured.".into()));
            return;
        };
        let text = self.draft.value.trim().to_string();
        self.draft = Field::new();
        self.items.push(Item::User(text.clone()));

        let sid = self.session_id.clone();
        if !sid.is_empty() {
            let p = SessionLog::sessions_dir().join(format!("{sid}.jsonl"));
            if p.exists() {
                if let Ok((mut log, _)) = SessionLog::resume(&p) {
                    let _ = log.append_message(&dragon_core::provider::Message::user(&text));
                    log.set_title_if_new(&text);
                }
            }
        }
        {
            let auto = self.cfg.settings.auto_approve.clone();
            if let Ok(mut a) = agent.try_lock() {
                self.sync_extras(&mut a);
                let _ = auto;
            }
        }

        self.streaming = Some(String::new());
        self.busy = true;
        self.status = format!("{} · thinking", self.mode.as_str());

        let tx2 = tx.clone();
        tokio::spawn(async move {
            let (atx, mut arx): (_, UnboundedReceiver<AgentEvent>) = unbounded_channel();
            let job = {
                let ag = agent.clone();
                let t = text.clone();
                tokio::spawn(async move {
                    let mut g = ag.lock().await;
                    g.turn(&t, atx).await
                })
            };
            while let Some(ev) = arx.recv().await {
                let out = match ev {
                    AgentEvent::Delta(d) => Ev::Delta(d),
                    AgentEvent::ToolStart { name, detail } => Ev::Tool(name, detail),
                    AgentEvent::ApprovalRequest { id, tool, detail } => Ev::Approval(id, tool, detail),
                    AgentEvent::Usage { prompt: _, completion: _, total } => Ev::Usage(total, total, total),
                    AgentEvent::Tasks(v) => Ev::Tasks(v),
                    AgentEvent::Compacted => Ev::Compacted,
                    AgentEvent::Stopped => Ev::Stopped,
                    AgentEvent::Error(e) => Ev::Error(e),
                    AgentEvent::ToolEnd { .. } => continue,
                };
                let _ = tx2.send(out);
            }
            let done = match job.await {
                Ok(Ok(t)) => Ev::Done(Ok(t)),
                Ok(Err(e)) => Ev::Done(Err(format!("{e:#}"))),
                Err(e) => Ev::Done(Err(e.to_string())),
            };
            let _ = tx2.send(done);
        });
    }

    fn handle(&mut self, ev: Ev) {
        match ev {
            Ev::Delta(d) => {
                if let Some(s) = &mut self.streaming {
                    s.push_str(&d);
                }
            }
            Ev::Tool(n, d) => self.items.push(Item::Tool(n, d)),
            Ev::Approval(id, tool, detail) => {
                self.pending = Some((id, tool.clone(), detail.clone()));
                self.items.push(Item::Approval(id, tool, detail));
                self.status = "approval needed".into();
            }
            Ev::Usage(_, _, t) => self.usage_total = t,
            Ev::Tasks(board) => {
                let mut rows = vec![];
                if let Some(list) = board.as_array() {
                    for t in list {
                        rows.push((
                            t.get("text").and_then(|x| x.as_str()).unwrap_or("?").into(),
                            t.get("status").and_then(|x| x.as_str()) == Some("done"),
                        ));
                    }
                }
                self.items.push(Item::Tasks(rows));
            }
            Ev::Compacted => self.items.push(Item::System("context compacted.".into())),
            Ev::Stopped => {
                self.busy = false;
                self.streaming = None;
                self.pending = None;
                self.status = "stopped".into();
                self.items.push(Item::System("stopped.".into()));
            }
            Ev::Explain(t) => self.items.push(Item::System(format!("what this does:\n{t}"))),
            Ev::Error(e) => self.items.push(Item::System(format!("error: {e}"))),
            Ev::Done(res) => {
                self.busy = false;
                let text = match res {
                    Ok(t) if !t.trim().is_empty() => t,
                    _ => self.streaming.take().unwrap_or_default(),
                };
                self.streaming = None;
                self.pending = None;
                self.status = "ready".into();
                if !text.trim().is_empty() {
                    let sid = self.session_id.clone();
                    if !sid.is_empty() {
                        let p = SessionLog::sessions_dir().join(format!("{sid}.jsonl"));
                        if p.exists() {
                            if let Ok((mut log, _)) = SessionLog::resume(&p) {
                                let _ = log
                                    .append_message(&dragon_core::provider::Message::assistant(&text));
                            }
                        }
                    }
                    self.items.push(Item::Assistant(text));
                }
            }
        }
        self.scroll = 0;
    }

    fn answer(&mut self, allowed: bool, always_tool: Option<String>) {
        let Some((id, tool, _)) = self.pending.take() else { return };
        if let Some(ag) = &self.agent {
            if let Ok(g) = ag.try_lock() {
                g.respond(id, allowed);
            }
        }
        if allowed && always_tool.is_some() {
            let tool = always_tool.unwrap();
            if !self.cfg.settings.auto_approve.contains(&tool) {
                self.cfg.settings.auto_approve.push(tool.clone());
            }
            self.persist();
            if let Some(ag) = &self.agent {
                if let Ok(mut g) = ag.try_lock() {
                    g.set_auto_approve(self.cfg.settings.auto_approve.clone());
                }
            }
            self.items.push(Item::System(format!("always allow '{tool}' saved.")));
        } else if !allowed {
            self.items.push(Item::System("denied - dragon will adapt.".into()));
        }
    }

    fn stop(&self) {
        if let Some(ag) = &self.agent {
            if let Ok(a) = ag.try_lock() {
                a.stop();
            }
        }
    }
}

// ------------------------------------------------------------------- window

struct WinState {
    window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    width: u32,
    height: u32,
}

struct Handler {
    win: Option<WinState>,
    model: Model,
    font: cosmic_text::FontSystem,
    swash: cosmic_text::SwashCache,
    tx: UnboundedSender<Ev>,
    rx: std::sync::mpsc::Receiver<Ev>,
    rt: tokio::runtime::Runtime,
    mouse: (i32, i32),
    ctrl_held: bool,
    hits: Vec<Hit>,
}

fn inside(r: (i32, i32, u32, u32), x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 as i32 && y >= r.1 && y < r.1 + r.3 as i32
}

impl ApplicationHandler for Handler {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.win.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Dragon Agent")
            .with_inner_size(winit::dpi::LogicalSize::new(1140.0f64, 760.0f64))
            .with_min_inner_size(winit::dpi::LogicalSize::new(860.0f64, 580.0f64));
        let window = Arc::new(el.create_window(attrs).expect("window"));
        let context = softbuffer::Context::new(window.clone()).expect("sb context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("sb surface");
        let s = window.inner_size();
        self.win = Some(WinState {
            window,
            surface,
            width: s.width.max(1),
            height: s.height.max(1),
        });
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        if let Some(w) = &self.win {
            if self.model.busy || self.rx.try_recv().is_ok() {
                w.window.request_redraw();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: winit::window::WindowId, ev: WindowEvent) {
        // drain events
        loop {
            match self.rx.try_recv() {
                Ok(ev) => self.model.handle(ev),
                Err(_) => break,
            }
        }
        let warc = self.win.as_ref().map(|w| w.window.clone());

        match ev {
            WindowEvent::Resized(s) => {
                if let Some(w) = &mut self.win {
                    w.width = s.width.max(1);
                    w.height = s.height.max(1);
                }
                if let Some(w) = &warc { w.request_redraw(); }
            }
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = (position.x as i32, position.y as i32);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (-y * 44.0) as i32,
                    MouseScrollDelta::PixelDelta(d) => -(d.y as i32),
                };
                if self.model.tab == Tab::Chat {
                    self.model.scroll = (self.model.scroll + dy).max(0);
                    if let Some(w) = &warc { w.request_redraw(); }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let hit = self.hits.iter().rev().find(|h| inside(h.rect, self.mouse.0, self.mouse.1)).cloned();
                if let Some(h) = hit {
                    self.dispatch(&h.action);
                    if let Some(w) = &warc { w.request_redraw(); }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let winit::keyboard::PhysicalKey::Code(code) = event.physical_key {
                    use winit::keyboard::KeyCode::*;
                    if matches!(code, ControlLeft | ControlRight) {
                        self.ctrl_held = event.state == ElementState::Pressed;
                    }
                }
                if event.state == ElementState::Pressed {
                    let ctrl = self.ctrl_held;
                    self.key(event, ctrl);
                    if let Some(w) = &warc { w.request_redraw(); }
                }
            }
            WindowEvent::RedrawRequested => {
                let mut w = self.win.take();
                if let Some(w) = &mut w {
                    self.paint(w);
                }
                self.win = w;
            }
            _ => {}
        }
    }
}

static EXPLAIN: Mutex<Option<String>> = Mutex::new(None);

impl Handler {
    fn dispatch(&mut self, action: &str) {
        let mut it = action.split(':');
        let cmd = it.next().unwrap_or("").to_string();
        let arg = it.collect::<Vec<&str>>().join(":");
        match cmd.as_str() {
            "quit" => std::process::exit(0),
            "tab" => {
                self.model.tab = match arg.as_str() {
                    "chat" => Tab::Chat,
                    "memory" => Tab::Memory,
                    "providers" => Tab::Providers,
                    "settings" => Tab::Settings,
                    _ => Tab::About,
                };
            }
            "mode" => {
                if let Some(m) = Mode::parse(&arg) {
                    self.model.mode = m;
                    if let Some(ag) = &self.model.agent {
                        if let Ok(mut g) = ag.try_lock() {
                            g.set_mode(m);
                        }
                    }
                }
            }
            "send" => self.model.send(&self.tx),
            "stop" => self.model.stop(),
            "focus" => self.model.focus = Some(arg.to_string()),
            "approve-y" => self.model.answer(true, None),
            "approve-a" => {
                let tool = self.model.pending.as_ref().map(|p| p.1.clone()).unwrap_or_default();
                self.model.answer(true, Some(tool));
            }
            "approve-n" => self.model.answer(false, None),
            "approval-why" => {
                let Some((_, tool, detail)) = self.model.pending.clone() else { return };
                let Some(ag) = self.model.agent.clone() else { return };
                let tx = self.tx.clone();
                self.rt.spawn(async move {
                    let (prov, model) = {
                        let g = ag.lock().await;
                        (g.provider.clone(), g.model.clone())
                    };
                    let sys = "You are a neutral security explainer. In at most 3 short sentences describe what this action would do to the user's machine and its risk level (low/medium/high). Do not reference any conversation.";
                    let q = format!("Action: tool={tool}\narguments={detail}");
                    match provider::complete(prov, &model, Some(sys), &[dragon_core::provider::Message::user(q)]).await {
                        Ok(t) => {
                            let _ = tx.send(Ev::Explain(t.trim().to_string()));
                        }
                        Err(e) => {
                            let _ = tx.send(Ev::Explain(format!("(failed: {e:#})")));
                        }
                    }
                });
            }
            "update" => {
                let url = dragon_core::update::latest_download_url(true);
                let _ = dragon_core::update::open_browser(&url);
                self.model.toast = Some(("opening download…".into(), 2.5));
            }
            "toggle-shell" => {
                self.model.cfg.settings.allow_commands = !self.model.cfg.settings.allow_commands;
                self.model.persist();
                let _ = self.model.rebuild_agent();
            }
            "toggle-graph" => {
                self.model.cfg.settings.memory_engine =
                    if self.model.cfg.settings.graph_memory() { "hybrid".into() } else { "graph".into() };
                self.model.persist();
                let _ = self.model.rebuild_agent();
            }
            "cycle-thinking" => {
                let order = ["off", "low", "medium", "high"];
                let cur = self.model.cfg.settings.thinking.clone();
                let i = order.iter().position(|o| *o == cur).unwrap_or(0);
                self.model.cfg.settings.thinking = order[(i + 1) % 4].into();
                self.model.persist();
                let _ = self.model.rebuild_agent();
            }
            "theme" => {
                self.model.cfg.settings.theme =
                    if self.model.cfg.settings.theme == "dark" { "light".into() } else { "dark".into() };
                self.model.persist();
            }
            "pv-preset" => {
                let n = presets::PRESETS.len() + 1;
                self.model.pv_idx = (self.model.pv_idx + 1) % n;
                // prefill url/models from preset
                if let Some(p) = presets::PRESETS.get(self.model.pv_idx) {
                    self.model.f_url.value = p.base_url.to_string();
                    self.model.f_models.value = p.models.join(", ");
                    self.model.f_url.caret = self.model.f_url.value.len();
                    self.model.f_models.caret = self.model.f_models.value.len();
                }
                if custom_idx(self.model.pv_idx) {
                    self.model.f_url.value.clear();
                    self.model.f_models.value.clear();
                }
            }
            "form-open" => self.model.form_open = !self.model.form_open,
            "form-save" => {
                let custom = custom_idx(self.model.pv_idx);
                let (name, url) = if custom {
                    (
                        self.model.f_name.value.trim().to_string(),
                        self.model.f_url.value.trim().trim_end_matches('/').to_string(),
                    )
                } else if let Some(p) = presets::PRESETS.get(self.model.pv_idx) {
                    (p.name.to_string(), self.model.f_url.value.trim().trim_end_matches('/').to_string())
                } else {
                    Default::default()
                };
                let models: Vec<String> = self
                    .model
                    .f_models
                    .value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if name.is_empty() || url.is_empty() || models.is_empty() {
                    self.model.toast = Some(("name, url and models are required".into(), 3.0));
                } else {
                    let kind = if url.contains("anthropic") { "anthropic" } else { "openai" };
                    self.model.cfg.providers.retain(|p| p.name != name);
                    self.model.cfg.providers.push(dragon_core::config::ProviderCfg {
                        name: name.clone(),
                        base_url: url,
                        api_key: self.model.f_key.value.trim().to_string(),
                        kind: Some(kind.into()),
                        models,
                    });
                    self.model.cfg.default_model = Some(format!(
                        "{name}/{}",
                        self.model.f_models.value.split(',').next().unwrap_or("").trim()
                    ));
                    self.model.persist();
                    let _ = self.model.rebuild_agent();
                    self.model.form_open = false;
                    self.model.toast = Some((format!("saved '{name}'"), 2.2));
                }
            }
            "sess-new" => {
                let model = self.model.model_spec.clone();
                if let Ok(log) = SessionLog::create(&model) {
                    self.model.session_id = log.meta().id.clone();
                    if let Some(ag) = &self.model.agent {
                        if let Ok(mut g) = ag.try_lock() {
                            g.reset();
                            g.set_session(Some(&self.model.session_id));
                        }
                    }
                    self.model.items.clear();
                    self.model.toast = Some(("new session".into(), 1.6));
                }
            }
            "sess-list" => {
                let all = session::list_sessions();
                let mut s = String::from("sessions:");
                for (i, (_p, meta)) in all.iter().take(10).enumerate() {
                    s.push_str(&format!("\n{}. {}", i + 1, meta.title));
                }
                s.push_str("\n\n(Ctrl+N starts a fresh one; resume lands next release)");
                self.model.items.push(Item::System(s));
                self.model.tab = Tab::Chat;
            }
            "noop" => {}
            _ => {}
        }
        if let Some(w) = &self.win {
            w.window.request_redraw();
        }
    }

    fn key(&mut self, event: winit::event::KeyEvent, ctrl: bool) {
        use winit::keyboard::{Key as LKey, NamedKey};
        if event.state != ElementState::Pressed {
            return;
        }

        if self.model.pending.is_some() {
            match &event.logical_key {
                LKey::Character(c) => match c.as_str() {
                    "y" | "Y" => return self.dispatch("approve-y"),
                    "a" | "A" => return self.dispatch("approve-a"),
                    "n" | "N" => return self.dispatch("approve-n"),
                    "d" | "D" => return self.dispatch("approval-why"),
                    _ => {}
                },
                _ => {}
            }
        }
        let named = |k: NamedKey| matches!(&event.logical_key, LKey::Named(n) if *n == k);
        if named(NamedKey::Escape) {
            if self.model.busy {
                self.model.stop();
            }
            return;
        }

        if ctrl {
            match &event.logical_key {
                LKey::Character(c) => match c.as_str() {
                    "n" => return self.dispatch("sess-new"),
                    "s" => return self.dispatch("sess-list"),
                    "d" => return self.dispatch("theme"),
                    "," => {
                        self.model.tab = Tab::Settings;
                        return;
                    }
                    "m" => {
                        let next = match self.model.mode {
                            Mode::Agent => Mode::Plan,
                            Mode::Plan => Mode::Chat,
                            Mode::Chat => Mode::Agent,
                        };
                        self.model.mode = next;
                        if let Some(ag) = &self.model.agent {
                            if let Ok(mut g) = ag.try_lock() {
                                g.set_mode(next);
                            }
                        }
                        return;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        let focus_is_draft = self.model.focus.as_deref() == Some("draft");
        if named(NamedKey::Enter) {
            if focus_is_draft {
                return self.dispatch("send");
            }
            return;
        }

        let target = self.model.focus.clone().unwrap_or_default();
        // special keys first
        match &event.logical_key {
            LKey::Named(k) => {
                let field = match target.as_str() {
                    "draft" => &mut self.model.draft,
                    "f_name" => &mut self.model.f_name,
                    "f_url" => &mut self.model.f_url,
                    "f_key" => &mut self.model.f_key,
                    "f_models" => &mut self.model.f_models,
                    _ => return,
                };
                match k {
                    NamedKey::Backspace => field.back(),
                    NamedKey::ArrowLeft => field.caret = field.caret.saturating_sub(1),
                    NamedKey::ArrowRight => field.caret = (field.caret + 1).min(field.value.chars().count()),
                    NamedKey::Home => field.caret = 0,
                    NamedKey::End => field.caret = field.value.chars().count(),
                    NamedKey::Space => field.insert(' '),
                    _ => {}
                }
                return;
            }
            LKey::Character(c) => {
                if ctrl {
                    return;
                }
                let field = match target.as_str() {
                    "draft" => &mut self.model.draft,
                    "f_name" => &mut self.model.f_name,
                    "f_url" => &mut self.model.f_url,
                    "f_key" => &mut self.model.f_key,
                    "f_models" => &mut self.model.f_models,
                    _ => return,
                };
                for ch in c.chars() {
                    field.insert(ch);
                }
            }
            _ => {}
        }
    }

    fn paint(&mut self, win: &mut WinState) {
        // drain explains queued cross-thread
        if let Some(t) = EXPLAIN.lock().unwrap().take() {
            self.model.items.push(Item::System(format!("what this does:\n{t}")));
        }
        let theme = Theme::new(if self.model.cfg.settings.theme == "light" {
            ThemeName::Light
        } else {
            ThemeName::Dark
        });
        let mut pix = tiny_skia::Pixmap::new(win.width, win.height).expect("pix");
        let mut fr = Frame {
            pix: &mut pix,
            font: &mut self.font,
            swash: &mut self.swash,
            theme,
            hits: Vec::new(),
        };
        let W = win.width as i32;
        let H = win.height as i32;

        fr.fill_all(theme.bg);

        // titlebar brand
        fr.text(16, 14, 300, 12.5, EMBER, "DRAGON", true);
        fr.text(70, 14, 200, 12.5, GOLD, "AGENT", false);

        // right chips row
        let mut hx = W - 16;
        if let Some(note) = self.model.update_note.clone() {
            let wpx = (note.chars().count() as u32 * 9 + 40).min(340);
            hx -= wpx as i32 + 8;
            fr.rounded(hx, 9, wpx, 27, 999.0, Frame::rgba(GOLD, 0.14));
            fr.outline(hx, 9, wpx, 27, 999.0, 1.2, Frame::rgb(GOLD));
            fr.text(hx + 14, 16, wpx - 24, 12.0, GOLD, &format!("⭡ {note} — click"), false);
            fr.hits.push(Hit { rect: (hx, 9, wpx, 28), action: "update".into() });
        }
        hx -= 46;
        let dark = theme.name == ThemeName::Dark;
        hx -= fr.chip(hx, 9, if dark { "☾" } else { "☀" }, "theme", false) + 6;
        hx -= fr.chip(hx, 9, "?", "noop", false) + 6;
        let usage_txt = if self.model.usage_total > 0 {
            format!("↯ {} tok", fmt_tok(self.model.usage_total))
        } else {
            "↯ —".into()
        };
        hx -= fr.chip(hx, 9, &usage_txt, "noop", false) + 6;

        // rail
        let rx = 12;
        let ry = 48;
        let rh = H - ry - 12;
        fr.rounded(rx, ry, 58, rh as i32, 19.0, Frame::rgba(theme.panel, 0.85));
        fr.outline(rx, ry, 58, rh as i32, 19.0, 1.2, Frame::rgb(theme.line));
        let tabs = [("chat", "CHAT"), ("memory", "MEM"), ("providers", "PROV"), ("settings", "SET"), ("about", "INFO")];
        let mut ty = ry + 42;
        for (id, label) in tabs {
            let active = match self.model.tab {
                Tab::Chat => id == "chat",
                Tab::Memory => id == "memory",
                Tab::Providers => id == "providers",
                Tab::Settings => id == "settings",
                Tab::About => id == "about",
            };
            if active {
                fr.gradient_rounded(rx + 7, ty, 44, 44, 13.0, EMBER, FLAME);
            } else {
                fr.rounded(rx + 7, ty, 44, 44, 13.0, Frame::rgba(theme.panel2, 0.6));
            }
            fr.text(rx + 11, ty + 15, 40, 10.5, if active { [20, 18, 22] } else { theme.ash }, label, true);
            fr.hits.push(Hit { rect: (rx + 4, ty, 50, 48), action: format!("tab:{id}") });
            ty += 54;
        }

        // header chips
        let hy = ry + 6;
        let mut hxx = rx + 74;
        hxx += fr.chip(hxx, hy, &format!("☰ {}", short8(&self.model.session_id)), "sess-new", false) + 8;
        hxx += fr.chip(hxx, hy, &self.model.model_spec, "noop", false) + 10;
        for m in ["chat", "plan", "agent"] {
            let active = self.model.mode.as_str() == m;
            hxx += fr.chip(hxx, hy, m, &format!("mode:{m}"), active) + 6;
        }
        fr.rect(W - 30, hy + 11, 8, 8, Frame::rgb(if self.model.agent.is_some() { JADE } else { BLOOD }));

        let cx = rx + 74;
        let cw = (W - cx - 16) as u32;

        match self.model.tab {
            Tab::Chat => self.paint_chat(&mut fr, cx, hy + 44, cw, H),
            Tab::Memory => self.paint_memory(&mut fr, cx, hy + 44, cw, H),
            Tab::Providers => self.paint_providers(&mut fr, cx, hy + 44, cw, H),
            Tab::Settings => self.paint_settings(&mut fr, cx, hy + 44, cw, H),
            Tab::About => {
                let cy = H / 3;
                let tw = "DRAGON AGENT".len() as u32 * 20;
                fr.text((W - tw as i32) / 2, cy, u32::MAX, 36.0, EMBER, "DRAGON AGENT", true);
                fr.text((W - tw as i32) / 2, cy + 50, u32::MAX, 14.0, theme.ash, "a fast AI agent with a long memory", false);
                fr.text(
                    (W - tw as i32) / 2,
                    cy + 96,
                    u32::MAX,
                    12.0,
                    theme.line,
                    &format!("v{} · MIT · dgpui (pure rust)", dragon_core::VERSION),
                    false,
                );
            }
        }

        if let Some((msg, ttl)) = &mut self.model.toast {
            let wpx = (msg.chars().count() as u32 * 9 + 40).min(W as u32 - 80);
            let x = (W - wpx as i32) / 2;
            fr.rounded(x, H - 58, wpx, 34, 10.0, Frame::rgb(theme.panel2));
            fr.outline(x, H - 58, wpx, 34, 10.0, 1.2, Frame::rgb(GOLD));
            fr.text(x + 20, H - 49, wpx - 30, 13.0, GOLD, msg, false);
            *ttl -= 0.02;
            if *ttl <= 0.0 {
                self.model.toast = None;
            }
        }

        self.hits = fr.hits.clone();
        drop(fr);

        win.surface
            .resize(NonZeroU32::new(win.width).unwrap(), NonZeroU32::new(win.height).unwrap())
            .ok();
        let mut buf = match win.surface.buffer_mut() {
            Ok(b) => b,
            Err(_) => return,
        };
        let data = pix.data();
        for (dst, src) in buf.as_mut_slice().iter_mut().zip(data.chunks_exact(4)) {
            *dst = ((src[0] as u32) << 16) | ((src[1] as u32) << 8) | src[2] as u32;
        }
        let _ = buf.present();
        win.window.request_redraw();
    }

    fn paint_chat(&mut self, fr: &mut Frame, x: i32, y: i32, w: u32, H: i32) {
        let comp_h = 100;
        let clip_bottom = H - comp_h - 26;
        fr.bottom_shadow(x, w, H - comp_h - 20, 130, fr.theme.name == ThemeName::Dark);

        let pad = 18;
        let iw = w.saturating_sub(pad * 2);
        let mut yy = y + 6 - self.model.scroll;
        for item in self.model.items.clone() {
            if yy > clip_bottom {
                break;
            }
            let hh = self.paint_item(fr, &item, x + pad, iw as i32, yy, clip_bottom);
            yy += hh + 14;
        }
        if let Some(s) = &self.model.streaming {
            let bh = fr.measure(iw - 40, 13.8, s, false).max(30) + 30;
            if yy < clip_bottom {
                fr.rounded(x + pad, yy, iw, bh, 14.0, Frame::rgba(fr.theme.panel, 0.95));
                fr.outline(x + pad, yy, iw, bh, 14.0, 1.0, Frame::rgb(fr.theme.line));
                fr.text(x + pad + 14, yy + 13, iw - 30, 13.8, fr.theme.bone, &format!("{s}▌"), false);
            }
        }

        // status
        let status_txt = if self.model.pending.is_some() {
            "PERMISSION REQUESTED — y allow · a always · n deny · d explain".to_string()
        } else {
            self.model.status.clone()
        };
        fr.text(x + pad, H - comp_h - 20, w, 11.5, if self.model.pending.is_some() { GOLD } else { fr.theme.ash }, &status_txt, false);

        // glass composer
        let gy = H - comp_h;
        fr.rounded(x, gy, w, comp_h - 14, 20.0, Frame::rgba(fr.theme.panel2, 0.55));
        fr.outline(x, gy, w, comp_h - 14, 20.0, 1.5, Frame::rgb(if self.model.focus.as_deref() == Some("draft") { EMBER } else { fr.theme.line }));
        let fw = w - 140;
        fr.field(x + 14, gy + 14, fw, focus_is_draft(&self.model), &self.model.draft.value, "message...", "focus:draft", self.model.draft.caret);
        let bx = x + w as i32 - 106;
        if self.model.busy {
            fr.button(bx, gy + 14, "stop", "stop", false);
        } else {
            fr.button(bx, gy + 14, "send", "send", true);
        }
    }

    fn paint_item(&mut self, fr: &mut Frame, item: &Item, x: i32, w: i32, y: i32, _clip: i32) -> i32 {
        match item {
            Item::User(t) => {
                let half = w / 2;
                let bh = fr.measure(half - 40, 13.8, t, false) + 30;
                fr.text(x + half, y - 17, half, 10.0, SKY, "YOU", true);
                fr.rounded(x + half, y, half, bh, 14.0, if fr.theme.name == ThemeName::Dark {
                    Frame::rgb([36, 58, 82])
                } else {
                    Frame::rgb([214, 230, 250])
                });
                fr.outline(x + half, y, half, bh, 14.0, 1.0, Frame::rgb(fr.theme.line));
                fr.text(x + half + 14, y + 12, half - 30, 13.8, fr.theme.bone, t, false);
                bh + 20
            }
            Item::Assistant(t) => {
                let clean = md_lite(t);
                let bh = fr.measure(w - 40, 13.8, &clean, false) + 30;
                fr.text(x, y - 17, w, 10.0, EMBER, "DRAGON", true);
                fr.rounded(x, y, w.min(760), bh, 14.0, Frame::rgba(fr.theme.panel, 0.96));
                fr.outline(x, y, w.min(760), bh, 14.0, 1.0, Frame::rgb(fr.theme.line));
                fr.text(x + 14, y + 12, w.min(760) - 28, 13.8, fr.theme.bone, &clean, false);
                bh + 20
            }
            Item::Tool(n, d) => {
                let s = format!("» {n} {}", truncate(d, 80));
                let bh = fr.measure(w.min(560), 12.3, &s, false) + 10;
                fr.rounded(x, y, w.min(560), bh, 9.0, Frame::rgba(VIOLET, 0.08));
                fr.text(x + 10, y + 5, w.min(550), 12.3, VIOLET, &s, false);
                bh
            }
            Item::Approval(id, tool, detail) => {
                let _ = id;
                let cw = (w as f32 * 0.9) as u32;
                let ch = 152;
                fr.rounded(x, y, cw, ch, 14.0, Frame::rgba(GOLD, 0.07));
                fr.outline(x, y, cw, ch, 14.0, 1.4, Frame::rgb(GOLD));
                fr.text(x + 16, y + 13, cw - 30, 12.8, GOLD, "⚠ PERMISSION REQUESTED", true);
                fr.text(x + 16, y + 37, cw - 30, 12.6, fr.theme.bone, &format!("{tool}: {}", truncate(detail, 90)), false);
                let mut bx = x + 16;
                bx += fr.button(bx, y + 76, "allow", "approve-y", true) + 8;
                bx += fr.button(bx, y + 76, &format!("always {}", truncate(tool, 14)), "approve-a", false) + 8;
                bx += fr.button(bx, y + 76, "deny", "approve-n", false) + 8;
                fr.button(bx, y + 76, "what does this do?", "approval-why", false);
                fr.text(x + 16, y + 126, cw - 30, 10.5, fr.theme.scale_hint(),
                    "or press y / a / n / d — d asks dragon itself what this action does", false);
                ch
            }
            Item::System(t) => {
                let bh = fr.measure(w.min(640), 12.5, t, false) + 12;
                fr.rounded(x, y, w.min(640), bh, 999.0, Frame::rgba(fr.theme.panel2, 0.7));
                fr.text(x + 14, y + 7, w.min(640) - 24, 12.5, fr.theme.ash, t, false);
                bh
            }
            Item::Tasks(rows) => {
                let mut hh = 22;
                fr.text(x, y, w, 11.0, GOLD, "TASK BOARD", true);
                for (text, done) in rows {
                    if *done {
                        fr.rounded(x + 2, y + hh, 15, 15, 4.0, Frame::rgb(JADE));
                        fr.text(x + 5, y + hh - 1, 14, 11.0, [16, 36, 26], "✓", true);
                    } else {
                        fr.outline(x + 2, y + hh, 15, 15, 4.0, 1.5, Frame::rgb(fr.theme.line));
                    }
                    fr.text(x + 26, y + hh - 1, w - 40, 12.6,
                        if *done { fr.theme.ash } else { fr.theme.bone }, text, false);
                    hh += 24;
                }
                hh + 6
            }
        }
    }

    fn paint_memory(&mut self, fr: &mut Frame, x: i32, y: i32, w: u32, _H: i32) {
        fr.text(x, y, w, 20.0, fr.theme.bone, "Memory graph", true);
        fr.text(x, y + 30, w, 12.3, fr.theme.ash,
            &format!("engine: {} · confidence decays with disuse; archival fades away", self.model.cfg.settings.memory_engine), false);
        let snap = self.model.graph.lock().unwrap().snapshot(Some(&self.model.session_id));
        let mut yy = y + 62;
        for (label, bullets) in snap {
            fr.text(x, yy, w, 12.0, GOLD, &label.to_uppercase(), true);
            yy += 22;
            for (b, _mine) in bullets {
                let tag = match b.kind {
                    dragon_core::memory::graph::Kind::Decision => "!",
                    dragon_core::memory::graph::Kind::Lesson => "L",
                    dragon_core::memory::graph::Kind::Task => "~",
                    dragon_core::memory::graph::Kind::Context => "?",
                    dragon_core::memory::graph::Kind::Fact => "·",
                };
                let color = match b.tier() {
                    dragon_core::memory::graph::Tier::Active => fr.theme.bone,
                    dragon_core::memory::graph::Tier::Cooling => fr.theme.ash,
                    dragon_core::memory::graph::Tier::Archival => fr.theme.line,
                };
                let line = format!("{tag} {}", b.text);
                let h = fr.measure(w - 70, 13.0, &line, false);
                fr.text(x + 14, yy, w - 76, 13.0, color, &line, false);
                fr.text(x + w as i32 - 52, yy, 50, 10.5, fr.theme.line,
                    &format!("{:.0}", b.confidence * 100.0), false);
                yy += h + 6;
            }
            yy += 10;
        }
        if yy == y + 62 {
            fr.text(x, yy, w, 13.0, fr.theme.ash,
                "(empty — the agent maintains it via graph_set_section)", false);
        }
    }

    fn paint_providers(&mut self, fr: &mut Frame, x: i32, y: i32, w: u32, _H: i32) {
        fr.text(x, y, w, 20.0, fr.theme.bone, "Providers", true);
        fr.text(x, y + 30, w, 12.3, fr.theme.ash, "bring your own key — stored locally only", false);
        fr.button(x + w as i32 - 160, y - 4,
                  if self.model.form_open { "close form" } else { "+ add provider" },
                  "form-open", false);

        let mut yy = y + 60;
        for p in self.model.cfg.providers.clone() {
            let is_def = self.model.cfg.default_model.as_deref()
                .map(|d| d.starts_with(&format!("{}/", p.name))).unwrap_or(false);
            fr.rounded(x, yy, w, 62, 13.0, Frame::rgba(fr.theme.panel, 0.92));
            fr.outline(x, yy, w, 62, 13.0, 1.0, Frame::rgb(fr.theme.line));
            fr.text(x + 16, yy + 9, 400, 14.5, if is_def { GOLD } else { fr.theme.bone }, &p.name, true);
            if is_def {
                fr.text(x + w as i32 - 108, yy + 9, 96, 11.0, GOLD, "default", false);
            }
            fr.text(x + 16, yy + 34, w - 220, 11.3, fr.theme.ash, &p.base_url, false);
            let joined = p.models.iter().take(2).cloned().collect::<Vec<_>>().join(", ");
            fr.text(x + w as i32 - 210, yy + 34, 196, 11.0, FLAME, &joined, false);
            yy += 74;
        }

        if self.model.form_open {
            yy += 8;
            fr.rounded(x, yy, w, 250, 15.0, Frame::rgba(fr.theme.panel, 0.96));
            fr.outline(x, yy, w, 250, 15.0, 1.2, Frame::rgb(fr.theme.line));
            let custom = custom_idx(self.model.pv_idx);
            let pname = if custom {
                "custom"
            } else {
                presets::PRESETS.get(self.model.pv_idx).map(|p| p.name).unwrap_or("?")
            };
            let mut fx = x + 18;
            fx += fr.chip(fx, yy + 14, &format!("preset: {pname}"), "pv-preset", true) + 14;
            if !custom {
                if let Some(p) = presets::PRESETS.get(self.model.pv_idx) {
                    fr.text(fx, yy + 22, w - (fx - x) - 20, 11.5, GOLD, p.note, false);
                }
            } else {
                fr.field(fx, yy + 8, 220, self.model.focus.as_deref() == Some("f_name"),
                         &self.model.f_name.value, "provider name", "focus:f_name", self.model.f_name.caret);
            }
            fr.field(x + 18, yy + 58, w - 36, self.model.focus.as_deref() == Some("f_url"),
                     &self.model.f_url.value, "base url https://…", "focus:f_url", self.model.f_url.caret);
            fr.field(x + 18, yy + 110, w - 36, self.model.focus.as_deref() == Some("f_key"),
                     &self.model.f_key.value, "api key", "focus:f_key", self.model.f_key.caret);
            fr.field(x + 18, yy + 162, w - 260, self.model.focus.as_deref() == Some("f_models"),
                     &self.model.f_models.value, "models, comma separated", "focus:f_models", self.model.f_models.caret);
            fr.button(x + w as i32 - 230, yy + 164, "save provider", "form-save", true);
        }
    }

    fn paint_settings(&mut self, fr: &mut Frame, x: i32, y: i32, w: u32, _H: i32) {
        fr.text(x, y, w, 20.0, fr.theme.bone, "Settings", true);
        let mut yy = y + 54;

        let rows: [(String, String, bool, &str); 3] = [
            ("Allow shell commands".into(),
             "run_shell can execute commands here (still asks per action)".into(),
             self.model.cfg.settings.allow_commands,
             "toggle-shell"),
            (format!("Memory engine: {}", self.model.cfg.settings.memory_engine),
             "graph = info-graph maintained by the model · hybrid = scored facts".into(),
             self.model.cfg.settings.graph_memory(),
             "toggle-graph"),
            (format!("Deep thinking: {}", self.model.cfg.settings.thinking),
             "off / low / medium / high reasoning effort".into(),
             self.model.cfg.settings.thinking != "off",
             "cycle-thinking"),
        ];
        for (title, sub, on, action) in rows {
            fr.rounded(x, yy, w, 58, 13.0, Frame::rgba(fr.theme.panel, 0.92));
            fr.text(x + 18, yy + 10, w - 120, 14.2, fr.theme.bone, &title, false);
            fr.text(x + 18, yy + 32, w - 120, 11.5, fr.theme.ash, &sub, false);
            fr.rounded(x + w as i32 - 72, yy + 15, 48, 26, 999.0,
                       Frame::rgb(if on { EMBER } else { fr.theme.line }));
            fr.rect(x + w as i32 - 70 + if on { 24 } else { 3 }, yy + 17, 21, 21, Frame::rgb([255, 255, 255]));
            fr.hits.push(Hit { rect: (x + w as i32 - 76, yy + 8, 66, 40), action: action.into() });
            yy += 68;
        }
        fr.text(x, yy + 8, w, 11.5, fr.theme.line,
            &format!("config: {}\ndata:   {}", Config::path().display(), Config::data_dir().display()), false);
    }
}

fn custom_idx(i: usize) -> bool {
    i >= presets::PRESETS.len()
}

fn focus_is_draft(_m: &Model) -> bool {
    true
}

fn md_lite(t: &str) -> String {
    t.replace("**", "").replace('`', "")
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

fn short8(s: &str) -> String {
    s.chars().take(8).collect()
}

fn fmt_tok(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}



fn main() -> Result<()> {
    let cfg = Config::load()?;
    let memory = Arc::new(Mutex::new(MemoryStore::open()?));
    let graph = Arc::new(Mutex::new(GraphStore::open()?));
    let rt = tokio::runtime::Runtime::new()?;
    let (tx, mut trx) = unbounded_channel::<Ev>();
    let (stx, srx) = std::sync::mpsc::channel::<Ev>();

    // bridge tokio events -> std queue consumed by the UI thread
    rt.spawn(async move {
        while let Some(ev) = trx.recv().await {
            if stx.send(ev).is_err() {
                break;
            }
        }
    });

    // pre-flight update gate in terminal before opening any window
    let gate = rt.block_on(async { dragon_core::update::check(dragon_core::VERSION).await });
    let update_note = match gate {
        Ok(Some(u)) => {
            println!(
                "\n\x1b[38;2;255;205;112m⭡ update available\x1b[0m  v{} → {}\nrelease: {}\n[o] open download & exit   [enter] continue\n> ",
                dragon_core::VERSION, u.latest, u.url
            );
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            let l = line.trim().to_ascii_lowercase();
            if l.starts_with('o') || l.starts_with('u') || l.starts_with('y') {
                let url = dragon_core::update::latest_download_url(true);
                let _ = dragon_core::update::open_browser(&url);
                println!("opening browser: {url}");
                return Ok(());
            }
            Some(u.latest)
        }
        _ => None,
    };

    let model = Model::new(cfg, memory, graph);
    let mut handler = Handler {
        win: None,
        model,
        font: cosmic_text::FontSystem::new(),
        swash: cosmic_text::SwashCache::new(),
        tx,
        rx: srx,
        rt,
        mouse: (0, 0),
        ctrl_held: false,
        hits: Vec::new(),
    };
    handler.model.update_note = update_note.map(|latest| format!("{latest}"));

    let event_loop = EventLoop::builder().build()?;
    event_loop.run_app(&mut handler)?;
    Ok(())
}
