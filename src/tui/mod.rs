//! TUI core: state, event loop, slash commands, session wiring.

pub mod ui;

use crate::agent::{Agent, AgentEvent};
use crate::config::Config;
use crate::memory::MemoryStore;
use crate::provider;
use crate::session::SessionLog;
use anyhow::Result;
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

#[derive(Clone)]
pub enum Entry {
    User(String),
    Assistant(String),
    Tool { name: String, detail: String },
    System(String),
}

enum TuiEvent {
    Key(KeyEvent),
    Paste(String),
    Agent(AgentEvent),
    TurnFinished(Result<String, String>),
}

pub struct App {
    pub entries: Vec<Entry>,
    pub input: String,
    pub streaming: Option<String>,
    pub busy: bool,
    pub status: String,
    pub spinner_frame: usize,
    /// 0 = pinned to the latest line.
    pub scroll_offset: usize,
    pub model_spec: String,
    pub session_id: String,
    pub should_quit: bool,
    pub wizard: Option<Wizard>,
    agent: Option<Arc<tokio::sync::Mutex<Agent>>>,
    session: Arc<Mutex<SessionLog>>,
    memory: Arc<Mutex<MemoryStore>>,
    config: Config,
}

/// Interactive first-run setup: pick provider -> paste key -> pick model.
pub struct Wizard {
    step: &'static str, // "provider" | "url" | "key" | "model"
    name: String,
    base_url: String,
    kind: String,
    key: String,
    models: Vec<String>,
}

impl App {
    pub fn session_label(&self) -> String {
        self.session_id.chars().take(8).collect()
    }

    fn say<S: Into<Entry>>(&mut self, e: S) {
        self.entries.push(e.into());
        self.scroll_offset = 0;
    }

    async fn on_key(&mut self, key: KeyEvent, tx: UnboundedSender<TuiEvent>) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }
        match key.code {
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.new_session().await?;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear();
            }
            KeyCode::PageUp => self.scroll_offset += 12,
            KeyCode::PageDown => self.scroll_offset = self.scroll_offset.saturating_sub(12),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input.push('\n');
            }
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.input);
                self.submit(&text, tx).await?;
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
        Ok(())
    }

    async fn submit(&mut self, raw: &str, tx: UnboundedSender<TuiEvent>) -> Result<()> {
        let text = raw.trim_end().to_string();
        if text.is_empty() {
            return Ok(());
        }
        self.scroll_offset = 0;

        if self.wizard.is_some() {
            self.wizard_feed(&text);
            return Ok(());
        }

        if let Some(cmdline) = text.strip_prefix('/') {
            self.command(cmdline).await;
            return Ok(());
        }

        let Some(agent) = self.agent.clone() else {
            self.say(Entry::System(self.config_setup_hint()));
            return Ok(());
        };
        if self.busy {
            self.say(Entry::System("dragon is still working - one thing at a time.".to_string()));
            return Ok(());
        }

        // log user side
        {
            let mut sess = self.session.lock().unwrap();
            sess.append_message(&crate::provider::Message::user(&text))?;
            sess.set_title_if_new(&text);
        }

        self.entries.push(Entry::User(text.clone()));
        self.streaming = Some(String::new());
        self.busy = true;
        self.status = "thinking".into();

        spawn_turn(agent, text, tx);
        Ok(())
    }

    async fn command(&mut self, cmdline: &str) {
        let (word, rest) = match cmdline.split_once(' ') {
            Some((w, r)) => (w, r.trim()),
            None => (cmdline, ""),
        };
        match word {
            "help" => self.say(Entry::System(HELP.into())),
            "quit" | "exit" => self.should_quit = true,
            "clear" => self.entries.clear(),
            "setup" => self.start_wizard(),
            "new" => {
                let _ = self.new_session().await;
            }
            "model" => {
                if rest.is_empty() {
                    self.say(Entry::System(format!("current model: {}", self.model_spec)));
                } else {
                    self.switch_model(rest).await;
                }
            }
            "remember" => {
                if rest.is_empty() {
                    self.say(Entry::System("usage: /remember <fact>".into()));
                } else {
                    let fact = {
                        let mut mem = self.memory.lock().unwrap();
                        let f = mem.add(rest, &["manual".to_string()], 0.85);
                        let _ = mem.save();
                        format!("saved [{}] {}", f.id, f.content)
                    };
                    self.say(Entry::System(fact));
                }
            }
            "memories" => {
                let facts = self.memory.lock().unwrap().all().to_vec();
                if facts.is_empty() {
                    self.say(Entry::System("memory is empty.".into()));
                } else {
                    let listing = facts
                        .iter()
                        .map(|f| format!("[{}] {}", f.id, f.content))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.say(Entry::System(listing));
                }
            }
            "forget" => {
                let msg = {
                    let removed = self.memory.lock().unwrap().remove(rest);
                    let _ = self.memory.lock().unwrap().save();
                    if removed { "forgotten.".to_string() } else { "no matching id.".to_string() }
                };
                self.say(Entry::System(msg));
            }
            other => self.say(Entry::System(format!(
                "unknown '/{other}' - try /help"
            ))),
        }
    }

    // ---------------------------------------------------------------- setup

    pub fn start_wizard(&mut self) {
        self.wizard = Some(Wizard {
            step: "provider",
            name: String::new(),
            base_url: String::new(),
            kind: String::new(),
            key: String::new(),
            models: Vec::new(),
        });
        self.say(Entry::System(crate::presets::menu()));
        self.status = "setup - pick a provider".into();
    }

    fn wizard_feed(&mut self, line: &str) {
        let mut msgs: Vec<String> = Vec::new();
        let mut done: Option<(String, crate::config::ProviderCfg)> = None;

        if let Some(w) = self.wizard.as_mut() {
            match w.step {
                "provider" => {
                    let choice = line.trim().to_ascii_lowercase();
                    if choice == "custom" || choice == "c" {
                        w.step = "url";
                        msgs.push(
                            "paste the base URL of the endpoint (OpenAI-compatible):".into(),
                        );
                    } else {
                        let preset = match choice.parse::<usize>() {
                            Ok(n) => crate::presets::PRESETS.get(n.wrapping_sub(1)),
                            Err(_) => crate::presets::find(&choice),
                        };
                        match preset {
                            Some(p) => {
                                w.name = p.name.to_string();
                                w.base_url = p.base_url.to_string();
                                w.kind = p.kind.to_string();
                                w.models = p.models.iter().map(|m| m.to_string()).collect();
                                msgs.push(format!("note: {}", p.note));
                                if p.key_required {
                                    w.step = "key";
                                    msgs.push("now paste your API key:".into());
                                } else {
                                    w.step = "model";
                                    let first =
                                        w.models.first().cloned().unwrap_or_default();
                                    msgs.push(format!(
                                        "model id? (press enter for '{first}')"
                                    ));
                                }
                            }
                            None => msgs.push(
                                "not on the list - type a number, a preset name, or 'custom'."
                                    .into(),
                            ),
                        }
                    }
                }
                "url" => {
                    let url = line.trim().trim_end_matches('/').to_string();
                    if !url.starts_with("http") {
                        msgs.push("that does not look like a URL - try again:".into());
                    } else {
                        w.base_url = url;
                        w.kind = if w.base_url.contains("anthropic") {
                            "anthropic".into()
                        } else {
                            "openai".into()
                        };
                        w.step = "key";
                        msgs.push("API key for it (type 'none' for local servers):".into());
                    }
                }
                "key" => {
                    let k = line.trim().to_string();
                    if k.is_empty() {
                        msgs.push("empty key - paste the key, or 'none':".into());
                    } else {
                        w.key = if k.eq_ignore_ascii_case("none") { String::new() } else { k };
                        w.step = "model";
                        let first = w.models.first().cloned().unwrap_or_default();
                        msgs.push(format!(
                            "model id? (press enter for '{first}', or type another)"
                        ));
                    }
                }
                "model" => {
                    let model = line.trim().to_string();
                    let model = if model.is_empty() {
                        w.models.first().cloned().unwrap_or_default()
                    } else {
                        model
                    };
                    if model.is_empty() {
                        msgs.push("type a model id (e.g. gpt-4o-mini):".into());
                    } else {
                        let cfg = crate::config::ProviderCfg {
                            name: w.name.clone(),
                            base_url: w.base_url.clone(),
                            api_key: w.key.clone(),
                            kind: Some(w.kind.clone()),
                            models: vec![model.clone()],
                        };
                        done = Some((model, cfg));
                    }
                }
                _ => {}
            }
        }

        for m in msgs {
            self.say(Entry::System(m));
        }

        if let Some((model_id, pcfg)) = done {
            self.finish_wizard(model_id, pcfg);
        }
    }

    fn finish_wizard(&mut self, model_id: String, mut pcfg: crate::config::ProviderCfg) {
        let mut newcfg = self.config.clone();

        let mut pname = pcfg.name.clone();
        if pname == "custom" {
            pname = format!("custom-{}", &uuid::Uuid::new_v4().simple().to_string()[..4]);
        }
        if newcfg.find_provider(&pname).is_some() {
            pname = format!("{pname}-{}", &uuid::Uuid::new_v4().simple().to_string()[..4]);
        }
        pcfg.name = pname.clone();

        let spec = format!("{pname}/{model_id}");
        newcfg.default_model = Some(spec.clone());
        newcfg.providers.push(pcfg.clone());

        if let Err(e) = newcfg.save() {
            self.say(Entry::System(format!("error saving config: {e:#}")));
            return;
        }

        match provider::build(&pcfg) {
            Ok(p) => {
                let agent = Agent::new(
                    p,
                    model_id,
                    self.memory.clone(),
                    newcfg.settings.allow_commands,
                    newcfg.settings.compaction_messages,
                );
                self.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
                self.config = newcfg;
                self.model_spec = spec;
                self.wizard = None;
                self.status = "ready".into();
                self.say(Entry::System(format!(
                    "saved to {}.\nconnected via {} - happy hacking!",
                    Config::path().display(),
                    self.model_spec
                )));
            }
            Err(e) => {
                self.wizard = None;
                self.status = "setup failed".into();
                self.say(Entry::System(format!("error: {e:#}\nrun /setup to try again.")));
            }
        }
    }

    async fn switch_model(&mut self, spec: &str) {
        let resolved = self.config.resolve_model(Some(spec));
        let (pcfg, model_id) = match resolved {
            Ok(pair) => pair,
            Err(e) => {
                self.say(Entry::System(format!("error: {e:#}")));
                return;
            }
        };
        let p = match provider::build(pcfg) {
            Ok(p) => p,
            Err(e) => {
                self.say(Entry::System(format!("error: {e:#}")));
                return;
            }
        };
        let spec_str = format!("{}/{}", pcfg.name, model_id);
        let allow = self.config.settings.allow_commands;
        let compact_after = self.config.settings.compaction_messages;
        match &self.agent {
            Some(ag) => {
                ag.lock().await.set_model(p, &model_id);
            }
            None => {
                self.agent = Some(Arc::new(tokio::sync::Mutex::new(Agent::new(
                    p,
                    model_id,
                    self.memory.clone(),
                    allow,
                    compact_after,
                ))));
            }
        }
        self.model_spec = spec_str;
        self.say(Entry::System(format!("model set to {}", self.model_spec)));
    }

    async fn new_session(&mut self) -> Result<()> {
        if let Some(ag) = &self.agent {
            ag.lock().await.reset();
        }
        let model = self.model_spec.clone();
        self.session = Arc::new(Mutex::new(SessionLog::create(&model)?));
        self.session_id = self.session.lock().unwrap().meta().id.clone();
        self.entries.clear();
        self.streaming = None;
        self.busy = false;
        self.say(Entry::System(format!("new session {}", self.session_label())));
        Ok(())
    }

    fn apply_agent_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::Delta(d) => {
                if let Some(s) = &mut self.streaming {
                    s.push_str(&d);
                }
                self.scroll_offset = 0;
            }
            AgentEvent::ToolStart { name, detail } => {
                self.entries.push(Entry::Tool { name, detail });
                self.scroll_offset = 0;
            }
            AgentEvent::ToolEnd { .. } => {}
            AgentEvent::Compacted => {
                self.entries
                    .push(Entry::System("context compacted to fit the window.".into()));
            }
            AgentEvent::Error(e) => {
                self.entries.push(Entry::System(format!("error: {e}")));
            }
        }
    }

    fn finish_turn(&mut self, res: Result<String, String>) {
        self.busy = false;
        self.streaming = None;
        self.status = "ready".into();
        match res {
            Ok(text) => {
                self.entries.push(Entry::Assistant(text.clone()));
                let mut sess = self.session.lock().unwrap();
                let _ =
                    sess.append_message(&crate::provider::Message::assistant(&text));
            }
            Err(e) => {
                self.entries.push(Entry::System(format!("error: {e}")));
            }
        }
        self.scroll_offset = 0;
    }

    fn config_setup_hint(&self) -> String {
        format!(
            "no model configured.\n{}",
            crate::cli::setup_instructions()
        )
    }
}

fn spawn_turn(
    agent: Arc<tokio::sync::Mutex<Agent>>,
    text: String,
    tx: UnboundedSender<TuiEvent>,
) {
    tokio::spawn(async move {
        let (atx, mut arx) = unbounded_channel::<AgentEvent>();
        let job = {
            let ag = agent.clone();
            let t = text.clone();
            tokio::spawn(async move {
                let mut guard = ag.lock().await;
                guard.turn(&t, atx).await
            })
        };
        while let Some(ae) = arx.recv().await {
            let _ = tx.send(TuiEvent::Agent(ae));
        }
        match job.await {
            Ok(Ok(final_text)) => {
                let _ = tx.send(TuiEvent::TurnFinished(Ok(final_text)));
            }
            Ok(Err(e)) => {
                let _ = tx.send(TuiEvent::TurnFinished(Err(format!("{e:#}"))));
            }
            Err(e) => {
                let _ = tx.send(TuiEvent::TurnFinished(Err(e.to_string())));
            }
        }
    });
}

const HELP: &str = "commands:
  /setup                    (re)configure a provider interactively
  /model <provider/model>   switch model mid-session
  /model                    show current model
  /remember <fact>          pin a fact to long-term memory
  /memories                 list stored facts
  /forget <id-prefix>       delete a fact
  /clear                    clear this view
  /new                      fresh session
keys: enter send · shift+enter newline · pgup/pgdn scroll · ctrl+n new · ctrl+u clear line · esc quit";

// ------------------------------------------------------------------- launch

pub async fn run(model_override: Option<String>) -> Result<()> {
    let config = Config::load()?;
    let memory = Arc::new(Mutex::new(MemoryStore::open()?));

    let mut app = App {
        entries: Vec::new(),
        input: String::new(),
        streaming: None,
        busy: false,
        status: "ready".into(),
        spinner_frame: 0,
        scroll_offset: 0,
        model_spec: "(none)".into(),
        session_id: String::new(),
        should_quit: false,
        wizard: None,
        agent: None,
        session: Arc::new(Mutex::new(SessionLog::create("(none)")?)),
        memory: memory.clone(),
        config: config.clone(),
    };
    app.session_id = app.session.lock().unwrap().meta().id.clone();

    match config.resolve_model(model_override.as_deref()) {
        Ok((pcfg, model_id)) => {
            let p = provider::build(pcfg)?;
            let spec = format!("{}/{}", pcfg.name, model_id);
            let agent = Agent::new(
                p,
                model_id,
                memory.clone(),
                config.settings.allow_commands,
                config.settings.compaction_messages,
            );
            app.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
            app.model_spec = spec;
        }
        Err(_) => {
            app.status = "no model configured".into();
            app.start_wizard();
        }
    }

    // terminal setup ---------------------------------------------------------
    let mut stdout = std::io::stdout();
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen
        );
        default_panic(info);
    }));

    // keyboard thread --------------------------------------------------------
    let (tx, mut rx) = unbounded_channel::<TuiEvent>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            match crossterm::event::read() {
                Ok(TermEvent::Key(k)) => {
                    if tx.send(TuiEvent::Key(k)).is_err() {
                        break;
                    }
                }
                Ok(TermEvent::Paste(s)) => {
                    if tx.send(TuiEvent::Paste(s)).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        });
    }

    // main loop ----------------------------------------------------------------
    let result = loop {
        if app.busy {
            app.spinner_frame += 1;
        }
        terminal.draw(|f| ui::draw(f, &mut app))?;
        let Some(ev) = rx.recv().await else { break Ok(()) };
        let step = match ev {
            TuiEvent::Key(k) => app.on_key(k, tx.clone()).await.map(|_| ()),
            TuiEvent::Paste(s) => {
                app.input.push_str(&s.replace(['\r', '\n'], " "));
                Ok(())
            }
            TuiEvent::Agent(ae) => {
                app.apply_agent_event(ae);
                Ok(())
            }
            TuiEvent::TurnFinished(res) => {
                app.finish_turn(res);
                Ok(())
            }
        };
        if let Err(e) = step {
            break Err(e);
        }
        if app.should_quit {
            break Ok(());
        }
    };

    // teardown -----------------------------------------------------------------
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::LeaveAlternateScreen
    );
    result
}
