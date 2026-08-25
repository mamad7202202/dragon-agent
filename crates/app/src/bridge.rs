//! Bridge between the GPUI window and the async agent core.
//!
//! A dedicated worker thread owns the tokio runtime and the `Agent`. The UI
//! talks to it exclusively through two channels (`Cmd` in, `Ev` out), so no
//! locks are ever taken across the boundary — the class of bugs that made the
//! old winit app fragile simply cannot happen here.

use dragon_core::agent::{Agent, AgentEvent, Mode};
use dragon_core::config::Config;
use dragon_core::memory::graph::GraphStore;
use dragon_core::memory::MemoryStore;
use dragon_core::provider::{self, Message};
use dragon_core::session::{self, SessionLog};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// UI -> worker.
#[derive(Debug)]
pub enum Cmd {
    /// Run one user turn.
    Send { text: String },
    /// Cancel the in-flight turn.
    Stop,
    /// Answer an approval request.
    Respond { id: u64, allowed: bool },
    /// Switch conversation mode.
    SetMode(Mode),
    /// Start a fresh session file.
    NewSession,
    /// Ask the model what a gated action does.
    Explain { tool: String, detail: String },
    // ---- config mutations (the worker is the source of truth, echoes Ev::Cfg)
    SaveProvider {
        name: String,
        url: String,
        key: String,
        models: Vec<String>,
    },
    DeleteProvider(String),
    ToggleShell,
    CycleThinking,
    ToggleEngine,
    AddAutoApprove(String),
    ToggleTheme,
}

/// Worker -> UI.
#[derive(Debug)]
pub enum Ev {
    /// Restored conversation on startup.
    History(Vec<Message>),
    SessionStarted { id: String },
    /// Agent built OK; spec is "provider/model".
    AgentReady { spec: String },
    AgentError { message: String },
    /// Echoed after every successful config mutation.
    Cfg(Box<Config>),
    Delta(String),
    ToolStart { name: String, detail: String },
    Approval { id: u64, tool: String, detail: String },
    UsageTotal(u64),
    Tasks(serde_json::Value),
    Compacted,
    Stopped,
    Explanation(String),
    Error(String),
    /// Turn finished; carries the final assistant text.
    Done(Result<String, String>),
    UpdateAvailable { latest: String },
}

enum Internal {
    Ui(Cmd),
    /// The running turn finished; deferred rebuilds can proceed now.
    TurnFinished,
}

/// Handle held by the UI.
pub struct Bridge {
    pub rx: Receiver<Ev>,
    tx_cmd: Sender<Internal>,
}

impl Bridge {
    /// Spawn the worker thread. Returns the handle used by the UI.
    pub fn launch(
        cfg: Config,
        memory: Arc<Mutex<MemoryStore>>,
        graph: Arc<Mutex<GraphStore>>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = channel::<Internal>();
        let (ev_tx, ev_rx) = channel::<Ev>();

        {
            // Clone before the closure: a `move` closure would otherwise
            // capture the originals even though we only send clones in.
            let worker_cmds = cmd_tx.clone();
            let worker_evs = ev_tx.clone();
            std::thread::spawn(move || {
                let mut worker =
                    Worker::new(cmd_rx, worker_cmds, worker_evs, cfg, memory, graph);
                worker.run();
            });
        }

        // Pre-flight update check — fully async, no terminal, no blocking.
        spawn_update_check(ev_tx);

        Self { rx: ev_rx, tx_cmd: cmd_tx }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx_cmd.send(Internal::Ui(cmd));
    }
}

fn spawn_update_check(ev_tx: Sender<Ev>) {
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
            .and_then(|rt| {
                rt.block_on(dragon_core::update::check(dragon_core::VERSION))
                    .map_err(|e| format!("{e:#}"))
            });
        if let Ok(Some(info)) = result {
            let _ = ev_tx.send(Ev::UpdateAvailable { latest: info.latest });
        }
    });
}

struct Worker {
    cmd_rx: Receiver<Internal>,
    cmd_tx: Sender<Internal>,
    ev: Sender<Ev>,
    rt: tokio::runtime::Runtime,
    cfg: Config,
    memory: Arc<Mutex<MemoryStore>>,
    graph: Arc<Mutex<GraphStore>>,
    agent: Option<Arc<tokio::sync::Mutex<Agent>>>,
    spec: String,
    mode: Mode,
    session_log: Option<SessionLog>,
    busy: bool,
    needs_rebuild: bool,
}

impl Worker {
    fn new(
        cmd_rx: Receiver<Internal>,
        cmd_tx: Sender<Internal>,
        ev: Sender<Ev>,
        cfg: Config,
        memory: Arc<Mutex<MemoryStore>>,
        graph: Arc<Mutex<GraphStore>>,
    ) -> Self {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let mode = Mode::parse(&cfg.settings.default_mode).unwrap_or(Mode::Agent);
        Self {
            cmd_rx,
            cmd_tx,
            ev,
            rt,
            cfg,
            memory,
            graph,
            agent: None,
            spec: "(none)".into(),
            mode,
            session_log: None,
            busy: false,
            needs_rebuild: false,
        }
    }

    fn run(&mut self) {
        // 1. Resume the newest session, or open a fresh one.
        self.restore_session();
        // 2. Try to build an agent from the stored config.
        if let Err(e) = self.rebuild_agent() {
            let _ = self.ev.send(Ev::AgentError { message: format!("{e:#}") });
        }
        // 3. Serve the UI until it disconnects.
        while let Ok(cmd) = self.cmd_rx.recv() {
            match cmd {
                Internal::Ui(cmd) => self.handle(cmd),
                Internal::TurnFinished => {
                    self.busy = false;
                    if self.needs_rebuild {
                        self.needs_rebuild = false;
                        let _ = self.rebuild_agent();
                    }
                }
            }
        }
    }

    fn restore_session(&mut self) {
        if let Some((path, _meta)) = session::list_sessions().first() {
            if let Ok((log, msgs)) = SessionLog::resume(path) {
                let id = log.meta().id.clone();
                self.session_log = Some(log);
                // Order matters: the UI clears its view on SessionStarted,
                // then appends the restored history.
                let _ = self.ev.send(Ev::SessionStarted { id });
                let _ = self.ev.send(Ev::History(msgs));
                return;
            }
        }
        self.open_fresh_session();
    }

    fn open_fresh_session(&mut self) {
        match SessionLog::create(&self.spec) {
            Ok(log) => {
                let id = log.meta().id.clone();
                self.session_log = Some(log);
                if let Some(agent) = &self.agent {
                    if let Ok(mut a) = agent.try_lock() {
                        a.reset();
                        a.set_session(Some(&id));
                    }
                }
                let _ = self.ev.send(Ev::SessionStarted { id });
            }
            Err(e) => {
                let _ = self.ev.send(Ev::Error(format!("cannot start session: {e:#}")));
            }
        }
    }

    fn rebuild_agent(&mut self) -> anyhow::Result<()> {
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
        self.apply_extras(&mut a);
        self.agent = Some(Arc::new(tokio::sync::Mutex::new(a)));
        self.spec = spec.clone();
        let _ = self.ev.send(Ev::AgentReady { spec });
        Ok(())
    }

    fn apply_extras(&self, a: &mut Agent) {
        a.set_mode(self.mode);
        let sid = self.session_log.as_ref().map(|l| l.meta().id.clone());
        a.set_session(sid.as_deref());
        a.set_auto_approve(self.cfg.settings.auto_approve.clone());
        a.set_thinking(self.cfg.settings.thinking_level());
        a.set_engine(if self.cfg.settings.graph_memory() {
            Some(self.graph.clone())
        } else {
            None
        });
    }

    /// Mutate the config, persist it, echo it back, and refresh the agent
    /// unless a turn is streaming right now (then defer via `needs_rebuild`).
    fn mutate_config(&mut self, needs_rebuild: bool, f: impl FnOnce(&mut Config)) {
        f(&mut self.cfg);
        let _ = self.cfg.save();
        if self.busy && needs_rebuild {
            self.needs_rebuild = true;
        } else if needs_rebuild {
            let _ = self.rebuild_agent();
        }
        let _ = self.ev.send(Ev::Cfg(Box::new(self.cfg.clone())));
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Send { text } => self.start_turn(text),
            Cmd::Stop => {
                if let Some(agent) = &self.agent {
                    if let Ok(a) = agent.try_lock() {
                        a.stop();
                    }
                }
            }
            Cmd::Respond { id, allowed } => {
                if let Some(agent) = &self.agent {
                    if let Ok(a) = agent.try_lock() {
                        a.respond(id, allowed);
                    }
                }
            }
            Cmd::SetMode(mode) => {
                self.mode = mode;
                if let Some(agent) = &self.agent {
                    if let Ok(mut a) = agent.try_lock() {
                        a.set_mode(mode);
                    }
                }
            }
            Cmd::NewSession => self.open_fresh_session(),
            Cmd::Explain { tool, detail } => self.explain(tool, detail),
            Cmd::SaveProvider { name, url, key, models } => {
                self.mutate_config(true, |cfg| {
                    cfg.providers.retain(|p| p.name != name);
                    let kind = if url.contains("anthropic") { "anthropic" } else { "openai" };
                    cfg.providers.push(dragon_core::config::ProviderCfg {
                        name: name.clone(),
                        base_url: url.trim_end_matches('/').to_string(),
                        api_key: key,
                        kind: Some(kind.into()),
                        models: models.clone(),
                    });
                    cfg.default_model =
                        Some(format!("{name}/{}", models.first().cloned().unwrap_or_default()));
                });
            }
            Cmd::DeleteProvider(name) => {
                self.mutate_config(true, |cfg| {
                    cfg.providers.retain(|p| p.name != name);
                    let is_default = cfg
                        .default_model
                        .as_deref()
                        .and_then(|d| d.split_once('/'))
                        .map(|(p, _)| p == name)
                        .unwrap_or(false);
                    if is_default {
                        cfg.default_model = None;
                    }
                });
            }
            Cmd::ToggleShell => self.mutate_config(true, |cfg| {
                cfg.settings.allow_commands = !cfg.settings.allow_commands;
            }),
            Cmd::CycleThinking => self.mutate_config(true, |cfg| {
                let order = ["off", "low", "medium", "high"];
                let i = order
                    .iter()
                    .position(|o| *o == cfg.settings.thinking)
                    .unwrap_or(0);
                cfg.settings.thinking = order[(i + 1) % 4].into();
            }),
            Cmd::ToggleEngine => self.mutate_config(true, |cfg| {
                cfg.settings.memory_engine = if cfg.settings.graph_memory() {
                    "hybrid".into()
                } else {
                    "graph".into()
                };
            }),
            Cmd::AddAutoApprove(tool) => {
                self.mutate_config(false, |cfg| {
                    if !cfg.settings.auto_approve.contains(&tool) {
                        cfg.settings.auto_approve.push(tool.clone());
                    }
                });
                // Apply immediately even mid-turn: approvals are answered live.
                if let Some(agent) = &self.agent {
                    if let Ok(mut a) = agent.try_lock() {
                        a.set_auto_approve(self.cfg.settings.auto_approve.clone());
                    }
                }
            }
            Cmd::ToggleTheme => self.mutate_config(false, |cfg| {
                cfg.settings.theme =
                    if cfg.settings.theme == "dark" { "light" } else { "dark" }.into();
            }),
        }
    }

    fn start_turn(&mut self, text: String) {
        if self.busy {
            let _ = self.ev.send(Ev::Error("a turn is already running".into()));
            return;
        }
        let Some(agent) = self.agent.clone() else {
            let _ = self.ev.send(Ev::Error("no provider configured yet".into()));
            return;
        };

        // Persist the user message + title.
        if let Some(log) = &mut self.session_log {
            let _ = log.append_message(&Message::user(&text));
            log.set_title_if_new(&text);
        }

        self.busy = true;
        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let ev_tx = self.ev.clone();
        let finished = self.cmd_tx.clone();

        self.rt.spawn(async move {
            let job = {
                let t = text.clone();
                let ag = agent.clone();
                tokio::spawn(async move {
                    let mut g = ag.lock().await;
                    g.turn(&t, atx).await
                })
            };
            while let Some(e) = arx.recv().await {
                let out = match e {
                    AgentEvent::Delta(d) => Ev::Delta(d),
                    AgentEvent::ToolStart { name, detail } => Ev::ToolStart { name, detail },
                    AgentEvent::ApprovalRequest { id, tool, detail } => {
                        Ev::Approval { id, tool, detail }
                    }
                    AgentEvent::Usage { total, .. } => Ev::UsageTotal(total),
                    AgentEvent::Tasks(v) => Ev::Tasks(v),
                    AgentEvent::Compacted => Ev::Compacted,
                    AgentEvent::Stopped => Ev::Stopped,
                    AgentEvent::Error(e) => Ev::Error(e),
                    AgentEvent::ToolEnd { .. } => continue,
                };
                if ev_tx.send(out).is_err() {
                    break;
                }
            }
            let done = match job.await {
                Ok(Ok(t)) => Ev::Done(Ok(t)),
                Ok(Err(e)) => Ev::Done(Err(format!("{e:#}"))),
                Err(e) => Ev::Done(Err(e.to_string())),
            };
            let _ = ev_tx.send(done);
            let _ = finished.send(Internal::TurnFinished);
        });
    }

    fn explain(&mut self, tool: String, detail: String) {
        let Some(agent) = self.agent.clone() else { return };
        let ev_tx = self.ev.clone();
        self.rt.spawn(async move {
            let (prov, model) = {
                let g = agent.lock().await;
                (g.provider.clone(), g.model.clone())
            };
            let sys = "You are a neutral security explainer. In at most 3 short sentences \
                       describe what this action would do to the user's machine and its risk \
                       level (low/medium/high). Do not reference any conversation.";
            let q = format!("Action: tool={tool}\narguments={detail}");
            match provider::complete(prov, &model, Some(sys), &[Message::user(q)]).await {
                Ok(t) => {
                    let _ = ev_tx.send(Ev::Explanation(t.trim().to_string()));
                }
                Err(e) => {
                    let _ = ev_tx.send(Ev::Explanation(format!("(failed: {e:#})")));
                }
            }
        });
    }
}
