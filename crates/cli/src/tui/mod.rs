//! TUI core: state, keyboard+mouse events, sessions, modes, approvals.

pub mod ui;

use dragon_core::agent::{Agent, AgentEvent, Mode};
use dragon_core::config::Config;
use dragon_core::memory::MemoryStore;
use dragon_core::provider;
use dragon_core::session::{self, SessionLog};
use anyhow::Result;
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

#[derive(Clone)]
pub enum Entry {
    User(String),
    Assistant(String),
    Tool { name: String, detail: String },
    Approval { tool: String, detail: String },
    System(String),
}

enum TuiEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize,
    Paste(String),
    Agent(AgentEvent),
    TurnFinished(Result<String, String>),
    Update(Option<String>),
}

/// Frame geometry captured during render - used for mouse hit-testing.
#[derive(Default, Clone, Copy)]
pub struct Areas {
    pub body: Rect,
    pub wizard: Rect,
    pub input: Rect,
}

pub struct App {
    pub entries: Vec<Entry>,
    pub input: String,
    /// Cursor position as a *character* index into `input`.
    pub cursor: usize,
    pub streaming: Option<String>,
    pub busy: bool,
    pub status: String,
    pub spinner_frame: usize,
    /// 0 = pinned to the latest line.
    pub scroll_offset: usize,
    pub model_spec: String,
    pub session_id: String,
    pub mode: Mode,
    pub should_quit: bool,
    pub wizard: Option<Wizard>,
    /// Selected row inside the active wizard list.
    pub wizard_row: usize,
    /// First visible wizard row (windowing) - written by the renderer.
    pub wizard_top: usize,
    /// Frame geometry, written by the renderer each frame.
    pub areas: Areas,
    /// Pending permission request awaiting y/a/n/d.
    pub pending_approval: Option<(u64, String, String)>,
    pub update_note: Option<String>,
    /// Input history (most recent last).
    hist_stack: Vec<String>,
    hist_pos: Option<usize>,
    agent: Option<Arc<tokio::sync::Mutex<Agent>>>,
    session: Arc<Mutex<SessionLog>>,
    memory: Arc<Mutex<MemoryStore>>,
    config: Config,
}

// ------------------------------------------------------------------ helpers

fn char_to_byte(s: &str, char_pos: usize) -> usize {
    s.chars().take(char_pos).map(char::len_utf8).sum()
}

impl App {
    pub fn session_label(&self) -> String {
        self.session_id.chars().take(8).collect()
    }

    fn say<S: Into<Entry>>(&mut self, e: S) {
        self.entries.push(e.into());
        self.scroll_offset = 0;
    }

    fn sync_agent_extras(&self, ag: &mut Agent) {
        ag.set_mode(self.mode);
        ag.set_session(Some(&self.session_id));
        ag.set_auto_approve(self.config.settings.auto_approve.clone());
    }

    fn push_char(&mut self, c: char) {
        let b = char_to_byte(&self.input, self.cursor.min(self.input.chars().count()));
        self.input.insert(b, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let b = char_to_byte(&self.input, self.cursor);
            self.input.remove(b);
        }
    }

    fn delete_at(&mut self) {
        let len = self.input.chars().count();
        if self.cursor < len {
            let b = char_to_byte(&self.input, self.cursor);
            self.input.remove(b);
        }
    }

    // ------------------------------------------------------------- keys

    async fn on_key(&mut self, key: KeyEvent, tx: UnboundedSender<TuiEvent>) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        // approval prompt swallows y/a/n/d first
        if self.pending_approval.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Ok(self.answer_approval(true, false)),
                KeyCode::Char('a') | KeyCode::Char('A') => return Ok(self.answer_approval(true, true)),
                KeyCode::Char('n') | KeyCode::Char('N') => return Ok(self.answer_approval(false, false)),
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    self.explain_pending().await;
                    return Ok(());
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => {
                if self.busy {
                    if let Some(ag) = &self.agent {
                        ag.lock().await.stop();
                        self.status = "stopping...".into();
                    }
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.list_sessions_cmd();
            }
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cycle_mode();
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.new_session()?;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::PageUp => self.scroll_offset += 12,
            KeyCode::PageDown => self.scroll_offset = self.scroll_offset.saturating_sub(12),
            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.word_left();
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.word_right();
            }
            KeyCode::Up if !self.wizard.is_some() && self.input.is_empty() => {
                self.history_prev();
            }
            KeyCode::Down if !self.wizard.is_some() && self.input.is_empty() && !self.hist_stack.is_empty() => {
                self.history_next();
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => {
                let len = self.input.chars().count();
                self.cursor = (self.cursor + 1).min(len);
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.chars().count(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_at(),
            KeyCode::Up if self.wizard.is_some() => {
                let n = self.wizard_rows().len();
                if n > 0 {
                    self.wizard_row =
                        if self.wizard_row == 0 { n - 1 } else { self.wizard_row - 1 };
                    self.scroll_offset = 0;
                }
            }
            KeyCode::Down if self.wizard.is_some() => {
                let n = self.wizard_rows().len();
                if n > 0 {
                    self.wizard_row = (self.wizard_row + 1) % n;
                    self.scroll_offset = 0;
                }
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.push_char('\n');
            }
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.input);
                self.cursor = 0;
                self.submit(&text, false, tx).await?;
            }
            // space selects highlighted wizard row
            KeyCode::Char(' ') if self.wizard.is_some() && self.input.is_empty() => {
                self.submit("", true, tx).await?;
            }
            KeyCode::Tab if self.wizard.is_some() => {
                self.submit("", true, tx).await?;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.push_char(c);
            }
            _ => {}
        }
        Ok(())
    }

    fn word_left(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.cursor = i;
    }

    fn word_right(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        self.cursor = i;
    }

    // ------------------------------------------------------------ mouse

    async fn on_mouse(&mut self, m: MouseEvent, tx: UnboundedSender<TuiEvent>) -> Result<()> {
        use crossterm::event::MouseButton::*;
        use crossterm::event::MouseEventKind::*;

        let (x, y) = (m.column, m.row);

        match m.kind {
            ScrollUp => self.scroll_offset += 3,
            ScrollDown => self.scroll_offset = self.scroll_offset.saturating_sub(3),
            Down(btn) if btn == Left => {
                // wizard list hit-test
                if self.wizard.is_some() && self.areas.wizard.height > 0 {
                    let w = self.areas.wizard;
                    if x >= w.x && x < w.x + w.width && y >= w.y && y < w.y + w.height {
                        // rows start 1px below border
                        let rel = (y - w.y).saturating_sub(1) as usize;
                        let idx = self.wizard_top + rel;
                        if idx < self.wizard_rows().len() {
                            self.wizard_row = idx;
                            self.submit("", true, tx).await?;
                            return Ok(());
                        }
                    }
                }
                // clicking the composer focuses it (cursor already there)
                let inp = self.areas.input;
                if inp.height > 0 && y >= inp.y && y < inp.y + inp.height {
                    let _ = x; // focus only
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ----------------------------------------------------------- submit

    async fn submit(
        &mut self,
        raw: &str,
        via_row: bool,
        tx: UnboundedSender<TuiEvent>,
    ) -> Result<()> {
        let text = raw.trim_end().to_string();
        if text.is_empty() && !via_row {
            return Ok(());
        }
        self.scroll_offset = 0;

        if self.wizard.is_some() {
            // empty text + Enter means "pick the highlighted row"
            let use_row = via_row || text.trim().is_empty();
            self.wizard_feed(&text, use_row);
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
            self.say(Entry::System(
                "dragon is still working - press esc to stop it.".to_string(),
            ));
            return Ok(());
        }

        self.remember_history(text.clone());

        {
            let mut sess = self.session.lock().unwrap();
            sess.append_message(&dragon_core::provider::Message::user(&text))?;
            sess.set_title_if_new(&text);
        }

        self.entries.push(Entry::User(text.clone()));
        self.streaming = Some(String::new());
        self.busy = true;
        self.status = format!("{} · thinking", self.mode.as_str());

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
            "providers" => self.list_providers(),
            "remove" => {
                if rest.is_empty() {
                    self.say(Entry::System("usage: /remove <provider-name>".into()));
                } else {
                    self.remove_provider(rest);
                }
            }
            "new" => {
                let _ = self.new_session();
            }
            "sessions" => self.list_sessions_cmd(),
            "resume" => {
                if rest.is_empty() {
                    self.list_sessions_cmd();
                } else {
                    self.resume_session(rest);
                }
            }
            "mode" => {
                if rest.is_empty() {
                    self.say(Entry::System(format!(
                        "current mode: {} (chat | plan | agent)",
                        self.mode.as_str()
                    )));
                } else if let Some(m) = Mode::parse(rest) {
                    self.mode = m;
                    if let Some(ag) = &self.agent {
                        ag.lock().await.set_mode(m);
                    }
                    self.say(Entry::System(format!("mode set to {}", m.as_str())));
                } else {
                    self.say(Entry::System("unknown mode - chat | plan | agent".into()));
                }
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
                    self.say(Entry::System("usage: /remember [global] <fact>".into()));
                } else if let Some(fact) = rest.strip_prefix("global ") {
                    let msg = {
                        let mut mem = self.memory.lock().unwrap();
                        let f = mem.add_scoped(fact, &["manual".into()], 0.85, None);
                        let _ = mem.save();
                        format!("saved global [{}] {}", f.id, f.content)
                    };
                    self.say(Entry::System(msg));
                } else {
                    let fact_text = rest.to_string();
                    let sid = Some(self.session_id.clone());
                    let msg = {
                        let mut mem = self.memory.lock().unwrap();
                        let f = mem.add_scoped(&fact_text, &[], 0.85, sid.as_deref());
                        let _ = mem.save();
                        format!("saved session [{}] {}", f.id, f.content)
                    };
                    self.say(Entry::System(msg));
                }
            }
            "memories" => {
                let facts = self.memory.lock().unwrap().all().to_vec();
                if facts.is_empty() {
                    self.say(Entry::System("memory is empty.".into()));
                } else {
                    let cur = self.session_id.clone();
                    let listing = facts
                        .iter()
                        .map(|f| {
                            let scope = match &f.session {
                                Some(s) if *s == cur => "s",
                                Some(_) => "x",
                                None => "g",
                            };
                            format!("[{}]({}) {}", f.id, scope, f.content)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.say(Entry::System(format!(
                        "(s)=this session (g)=global (x)=other session\n{listing}"
                    )));
                }
            }
            "forget" => {
                let msg = {
                    let removed = self.memory.lock().unwrap().remove(rest);
                    let _ = self.memory.lock().unwrap().save();
                    if removed {
                        "forgotten.".to_string()
                    } else {
                        "no matching id.".to_string()
                    }
                };
                self.say(Entry::System(msg));
            }
            other => {
                self.say(Entry::System(format!("unknown '/{other}' - try /help")))
            }
        }
    }

    // --------------------------------------------------------- sessions

    fn list_sessions_cmd(&mut self) {
        let all = session::list_sessions();
        if all.is_empty() {
            self.say(Entry::System("no saved sessions yet.".into()));
            return;
        }
        let mut s = String::from("sessions (newest first):\n");
        for (i, (_path, meta)) in all.iter().take(15).enumerate() {
            let mark = if meta.id == self.session_id { " <-" } else { "" };
            s.push_str(&format!(
                "\n{:>2}. {}  {}\n     {} · {}",
                i + 1,
                &meta.id[..8.min(meta.id.len())],
                meta.title,
                meta.model,
                meta.created_at
                    .chars()
                    .take(16)
                    .collect::<String>()
                    .replace('T', " "),
            ));
            if !mark.is_empty() {
                s.push_str(mark);
            }
        }
        s.push_str("\n\n/resume <number>");
        self.say(Entry::System(s));
    }

    fn resume_session(&mut self, which: &str) {
        let all = session::list_sessions();
        let picked = which.parse::<usize>().ok().and_then(|n| all.get(n - 1).cloned());
        let Some((_path, meta)) = picked else {
            self.say(Entry::System("no such session number - run /sessions".into()));
            return;
        };
        let dir = SessionLog::sessions_dir();
        let path = dir.join(format!("{}.jsonl", meta.id));
        match SessionLog::resume(&path) {
            Ok((log, msgs)) => {
                self.session = Arc::new(Mutex::new(log));
                self.session_id = meta.id.clone();
                if let Some(ag) = &self.agent {
                    if let Ok(mut guard) = ag.try_lock() {
                        guard.reset();
                        guard.history = msgs;
                        guard.set_session(Some(&self.session_id));
                    }
                }
                self.entries.clear();
                self.say(Entry::System(format!(
                    "resumed '{}' (session {})",
                    meta.title,
                    self.session_label()
                )));
            }
            Err(e) => self.say(Entry::System(format!("resume failed: {e:#}"))),
        }
    }

    fn cycle_mode(&mut self) {
        let next = match self.mode {
            Mode::Agent => Mode::Plan,
            Mode::Plan => Mode::Chat,
            Mode::Chat => Mode::Agent,
        };
        self.mode = next;
        if let Some(ag) = &self.agent {
            if let Ok(mut g) = ag.try_lock() {
                g.set_mode(next);
            }
        }
        self.say(Entry::System(format!(
            "mode: {} (ctrl+m cycles)",
            next.as_str()
        )));
    }

    // -------------------------------------------------------- approvals

    fn answer_approval(&mut self, allowed: bool, always: bool) {
        let Some((id, tool, _detail)) = self.pending_approval.take() else {
            return;
        };
        if let Some(ag) = &self.agent {
            if let Ok(guard) = ag.try_lock() {
                guard.respond(id, allowed);
            }
        }
        if allowed && always {
            self.config.settings.auto_approve.push(tool.clone());
            let _ = self.config.save();
            if let Some(ag) = &self.agent {
                if let Ok(mut g) = ag.try_lock() {
                    g.set_auto_approve(self.config.settings.auto_approve.clone());
                }
            }
            self.say(Entry::System(format!(
                "approved and will not ask again for '{tool}'."
            )));
        } else {
            self.say(Entry::System(if allowed {
                "approved.".to_string()
            } else {
                "denied - dragon will adapt.".to_string()
            }));
        }
        self.busy = !allowed; // still streaming results after denial too
        self.status = format!("{} · working", self.mode.as_str());
    }

    async fn explain_pending(&mut self) {
        let Some((_, tool, detail)) = self.pending_approval.clone() else {
            return;
        };
        let Some(ag) = self.agent.clone() else { return };
        self.say(Entry::System(format!(
            "asking dragon what `{tool}` does..."
        )));
        let (provider, model) = {
            let g = ag.lock().await;
            (g.provider.clone(), g.model.clone())
        };
        let sys = "You are a neutral security explainer. In at most 3 short sentences \
                   describe what the following action would do to the user's machine and its main \
                   risk level (low/medium/high). Do NOT reference any ongoing conversation.";
        let q = format!("Action: tool={tool}\narguments={detail}");
        let res = provider::complete(provider, &model, Some(sys), &[dragon_core::provider::Message::user(q)]).await;
        match res {
            Ok(txt) => self.say(Entry::Assistant(format!("what this does:\n{}", txt.trim()))),
            Err(e) => self.say(Entry::System(format!("explanation failed: {e:#}"))),
        }
    }

    // ---------------------------------------------------------- wizard

    pub fn start_wizard(&mut self) {
        self.wizard = Some(Wizard::new());
        self.wizard_row = 0;
        self.say(Entry::System(Wizard::intro()));
        self.status = "setup - pick a provider".into();
    }

    /// Options for the currently active wizard step (rendered + navigable).
    pub fn wizard_rows(&self) -> Vec<String> {
        let Some(w) = &self.wizard else { return vec![] };
        Self::rows_for(w.step, &w.models, &w.added_models)
    }

    fn rows_for(step: &str, models: &[String], added: &[String]) -> Vec<String> {
        match step {
            "provider" => {
                let mut v: Vec<String> = dragon_core::presets::PRESETS
                    .iter()
                    .map(|p| format!("{:<10} {}", p.name, p.label))
                    .collect();
                v.push(format!("{:<10} {}", "custom", "any other endpoint"));
                v
            }
            "model" => {
                let mut v: Vec<String> = models.to_vec();
                v.push("+ type a different model id".to_string());
                v
            }
            "more" => {
                let mut v: Vec<String> = models
                    .iter()
                    .filter(|m| !added.contains(m))
                    .cloned()
                    .collect();
                v.push("+ type another model id".to_string());
                v.push("finish - save & connect".to_string());
                v
            }
            _ => vec![],
        }
    }

    fn wizard_feed(&mut self, line: &str, via_row: bool) {
        let mut msgs: Vec<String> = Vec::new();
        let mut done: Option<(String, ProviderCfg)> = None;

        if let Some(w) = self.wizard.as_mut() {
            match w.step {
                "provider" => {
                    let choice = if via_row {
                        let idx = self.wizard_row;
                        let rows = Self::rows_for("provider", &[], &[]);
                        rows.get(idx).cloned().unwrap_or_default()
                    } else {
                        line.trim().to_ascii_lowercase()
                    };

                    let preset = if let Ok(n) = choice.parse::<usize>() {
                        dragon_core::presets::PRESETS.get(n.wrapping_sub(1))
                    } else {
                        dragon_core::presets::find(&choice)
                    };

                    if choice.split_whitespace().next() == Some("custom") {
                        w.step = "url";
                        msgs.push("base URL of the endpoint:".into());
                    } else if let Some(p) = preset {
                        w.name = p.name.to_string();
                        w.base_url = p.base_url.to_string();
                        w.kind = p.kind.to_string();
                        w.models = p.models.iter().map(|m| m.to_string()).collect();
                        msgs.push(format!("note: {}", p.note));
                        if p.key_required {
                            w.step = "key";
                            msgs.push("paste your API key:".into());
                        } else {
                            w.step = "more";
                            msgs.push("pick a model (up/down + space, or type an id):".into());
                        }
                        self.wizard_row = 0;
                    } else {
                        msgs.push(
                            "not on the list - pick a row (up/down/space), type a number, name, or 'custom'."
                                .into(),
                        );
                    }
                }
                "url" => {
                    let url = if via_row {
                        String::new()
                    } else {
                        line.trim().trim_end_matches('/').to_string()
                    };
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
                        msgs.push("API key (or type 'none' for local servers):".into());
                    }
                }
                "key" => {
                    let k = if via_row { String::new() } else { line.trim().to_string() };
                    if k.is_empty() {
                        msgs.push("empty key - paste the key, or 'none':".into());
                    } else {
                        w.key = if k.eq_ignore_ascii_case("none") { String::new() } else { k };
                        w.added_models.clear();
                        w.step = "more";
                        self.wizard_row = 0;
                        msgs.push("pick a model (up/down + space), or type any id:".into());
                    }
                }
                "more" => {
                    let rows = Self::rows_for("more", &w.models, &w.added_models);
                    if via_row {
                        let sel = rows.get(self.wizard_row).cloned().unwrap_or_default();
                        if sel.starts_with("finish") {
                            if w.added_models.is_empty() {
                                msgs.push("add at least one model first.".into());
                            } else {
                                let cfg = ProviderCfg {
                                    name: w.name.clone(),
                                    base_url: w.base_url.clone(),
                                    api_key: w.key.clone(),
                                    kind: Some(w.kind.clone()),
                                    models: w.added_models.clone(),
                                };
                                done = Some((w.added_models[0].clone(), cfg));
                            }
                        } else if let Some(m) = sel.strip_prefix("+ ") {
                            msgs.push(format!("type the model id (e.g. {m}):"));
                        } else if !sel.is_empty() {
                            w.added_models.push(sel.clone());
                            msgs.push(format!("added '{sel}' - add more or finish."));
                        }
                    } else {
                        let m = line.trim().to_string();
                        if !m.is_empty() && !w.added_models.contains(&m) {
                            w.added_models.push(m.clone());
                            msgs.push(format!("added '{m}' - add more or finish."));
                        } else if m.is_empty() && !w.added_models.is_empty() {
                            let cfg = ProviderCfg {
                                name: w.name.clone(),
                                base_url: w.base_url.clone(),
                                api_key: w.key.clone(),
                                kind: Some(w.kind.clone()),
                                models: w.added_models.clone(),
                            };
                            done = Some((w.added_models[0].clone(), cfg));
                        }
                    }
                }
                _ => {}
            }
        }

        for m in msgs {
            self.say(Entry::System(m));
        }

        if let Some((default_model, pcfg)) = done {
            self.finish_wizard(default_model, pcfg);
        }
        self.wizard_row = self.wizard_row.min(self.wizard_rows().len().saturating_sub(1));
    }

    fn finish_wizard(&mut self, _first: String, mut pcfg: ProviderCfg) {
        let mut newcfg = self.config.clone();

        let mut pname = pcfg.name.clone();
        if pname == "custom" || pname.is_empty() {
            pname = format!("custom-{}", &uuid::Uuid::new_v4().simple().to_string()[..4]);
        }
        if newcfg.find_provider(&pname).is_some() {
            pname = format!("{pname}-{}", &uuid::Uuid::new_v4().simple().to_string()[..4]);
        }
        pcfg.name = pname.clone();

        let spec = format!("{pname}/{}", pcfg.models[0]);
        newcfg.default_model = Some(spec.clone());
        newcfg.providers.push(pcfg.clone());

        if let Err(e) = newcfg.save() {
            self.say(Entry::System(format!("error saving config: {e:#}")));
            return;
        }

        match provider::build(&pcfg) {
            Ok(p) => {
                let allow = newcfg.settings.allow_commands;
                let compact_after = newcfg.settings.compaction_messages;
                let mut agent =
                    Agent::new(p, pcfg.models[0].clone(), self.memory.clone(), allow, compact_after);
                self.sync_agent_extras(&mut agent);
                self.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
                self.config = newcfg;
                self.model_spec = spec;
                self.wizard = None;
                self.wizard_row = 0;
                self.status = "ready".into();
                let count = pcfg.models.len();
                self.say(Entry::System(format!(
                    "saved to {}.\nconnected via {} ({count} models) - happy hacking!",
                    Config::path().display(),
                    self.model_spec,
                )));
            }
            Err(e) => {
                self.wizard = None;
                self.status = "setup failed".into();
                self.say(Entry::System(format!("error: {e:#}\nrun /setup to try again.")));
            }
        }
    }

    // ------------------------------------------------------------- misc

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
        let mid = model_id.clone();
        let allow = self.config.settings.allow_commands;
        let compact_after = self.config.settings.compaction_messages;
        match &self.agent {
            Some(ag) => {
                ag.lock().await.set_model(p, &mid);
            }
            None => {
                let mut agent =
                    Agent::new(p, mid, self.memory.clone(), allow, compact_after);
                self.sync_agent_extras(&mut agent);
                self.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
            }
        }
        self.model_spec = spec_str;
        self.say(Entry::System(format!("model set to {}", self.model_spec)));
    }

    fn new_session(&mut self) -> Result<()> {
        if let Some(ag) = &self.agent {
            if let Ok(mut g) = ag.try_lock() {
                g.reset();
            }
        }
        let model = self.model_spec.clone();
        let log = SessionLog::create(&model)?;
        self.session_id = log.meta().id.clone();
        self.session = Arc::new(Mutex::new(log));
        if let Some(ag) = &self.agent {
            if let Ok(mut g) = ag.try_lock() {
                g.set_session(Some(&self.session_id));
            }
        }
        self.entries.clear();
        self.streaming = None;
        self.pending_approval = None;
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
            AgentEvent::Stopped => {
                self.busy = false;
                self.streaming = None;
                self.pending_approval = None;
                self.status = "stopped".into();
                self.say(Entry::System("stopped.".to_string()));
            }
            AgentEvent::ApprovalRequest { id, tool, detail } => {
                self.pending_approval = Some((id, tool.clone(), detail.clone()));
                self.entries
                    .push(Entry::Approval { tool, detail });
                self.scroll_offset = 0;
                self.status = "approval needed".into();
            }
            AgentEvent::Error(e) => {
                self.entries.push(Entry::System(format!("error: {e}")));
            }
        }
    }

    fn finish_turn(&mut self, res: Result<String, String>) {
        self.busy = false;
        self.streaming = None;
        self.pending_approval = None;
        self.status = "ready".into();
        match res {
            Ok(text) => {
                if !text.is_empty() {
                    self.entries.push(Entry::Assistant(text.clone()));
                    let mut sess = self.session.lock().unwrap();
                    let _ =
                        sess.append_message(&dragon_core::provider::Message::assistant(&text));
                }
            }
            Err(e) => {
                self.entries.push(Entry::System(format!("error: {e}")));
            }
        }
        self.scroll_offset = 0;
    }

    fn config_setup_hint(&self) -> String {
        format!("no model configured.\n{}", crate::cli::setup_instructions())
    }
}

    // tiny input-history helpers ------------------------------------------

    fn remember_history(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        self.hist_stack.push(text);
        if self.hist_stack.len() > 100 {
            self.hist_stack.remove(0);
        }
        self.hist_pos = None;
    }

    fn history_prev(&mut self) {
        if self.hist_stack.is_empty() {
            return;
        }
        let pos = match self.hist_pos {
            None => self.hist_stack.len() - 1,
            Some(p) if p > 0 => p - 1,
            Some(p) => p,
        };
        self.hist_pos = Some(pos);
        self.input = self.hist_stack[pos].clone();
        self.cursor = self.input.chars().count();
    }

    fn history_next(&mut self) {
        let Some(pos) = self.hist_pos else { return };
        let next = pos + 1;
        if next >= self.hist_stack.len() {
            self.hist_pos = None;
            self.input.clear();
        } else {
            self.hist_pos = Some(next);
            self.input = self.hist_stack[next].clone();
        }
        self.cursor = self.input.chars().count();
    }

    // --------------------------------------------------------- providers

    fn list_providers(&mut self) {
        if self.config.providers.is_empty() {
            self.say(Entry::System("no providers configured. run /setup".into()));
            return;
        }
        let mut s = String::from("configured providers:");
        for p in &self.config.providers {
            let mark = if self
                .config
                .default_model
                .as_deref()
                .map(|d| d.starts_with(&format!("{}/", p.name)))
                .unwrap_or(false)
            {
                " *"
            } else {
                ""
            };
            s.push_str(&format!(
                "\n\n {}{}\n   {}\n",
                p.name,
                mark,
                p.base_url
            ));
            for m in &p.models {
                let dmark = if self.config.default_model.as_deref()
                    == Some(&format!("{}/{}", p.name, m))
                {
                    " <- default"
                } else {
                    ""
                };
                s.push_str(&format!("   - {m}{dmark}\n"));
            }
        }
        s.push_str("\n* default provider - /remove <name> deletes one");
        self.say(Entry::System(s));
    }

    fn remove_provider(&mut self, name: &str) {
        let mut cfg = self.config.clone();
        let before = cfg.providers.len();
        cfg.providers.retain(|p| p.name != name);
        if cfg.providers.len() == before {
            self.say(Entry::System(format!("provider '{name}' not found")));
            return;
        }
        if let Some(d) = &cfg.default_model {
            if d.split_once('/').map(|(p, _)| p) == Some(name) {
                cfg.default_model = None;
            }
        }
        if let Err(e) = cfg.save() {
            self.say(Entry::System(format!("error saving config: {e:#}")));
            return;
        }
        self.config = cfg;
        self.say(Entry::System(format!("removed '{name}'.")));
    }

    fn config_setup_hint(&self) -> String {
        format!("no model configured.\n{}", crate::cli::setup_instructions())
    }

// ------------------------------------------------------------------ wizard

pub struct Wizard {
    pub step: &'static str, // "provider" | "url" | "key" | "more"
    pub name: String,
    pub base_url: String,
    pub kind: String,
    key: String,
    pub models: Vec<String>,
    added_models: Vec<String>,
}

impl Wizard {
    fn new() -> Self {
        Self {
            step: "provider",
            name: String::new(),
            base_url: String::new(),
            kind: String::new(),
            key: String::new(),
            models: Vec::new(),
            added_models: Vec::new(),
        }
    }

    fn intro() -> String {
        format!(
            "setup - configure a provider (up/down + space to pick).\n{}",
            dragon_core::presets::menu()
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
  /setup                     (re)configure providers interactively
  /providers                 list every provider, its models and URLs
  /remove <provider>         delete a configured provider
  /sessions                  list saved sessions
  /resume <n>                continue a past session with its memory
  /mode [chat|plan|agent]    switch conversation mode
  /model <provider/model>    switch model mid-session
  /remember [global] <fact>  pin a fact (session or global scope)
  /memories                  list stored facts
  /forget <id-prefix>        delete a fact
  /clear · /new · /quit      view / fresh session / exit

editing: left/right caret - ctrl+left/right word - home/end
keys: enter send - shift+enter newline - pgup/pgdn scroll
      ctrl+n new session - ctrl+s sessions - ctrl+m cycle mode
      esc stop/quit - y/a/n/d answer permission prompts";

// ------------------------------------------------------------------- launch

pub async fn run(model_override: Option<String>) -> Result<()> {
    let config = Config::load()?;
    let memory = Arc::new(Mutex::new(MemoryStore::open()?));

    let mode = Mode::parse(&config.settings.default_mode).unwrap_or(Mode::Agent);

    let mut app = App {
        entries: Vec::new(),
        input: String::new(),
        cursor: 0,
        streaming: None,
        busy: false,
        status: "ready".into(),
        spinner_frame: 0,
        scroll_offset: 0,
        model_spec: "(none)".into(),
        session_id: String::new(),
        mode,
        should_quit: false,
        wizard: None,
        wizard_row: 0,
        wizard_top: 0,
        areas: Areas::default(),
        pending_approval: None,
        update_note: None,
        hist_stack: Vec::new(),
        hist_pos: None,
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
            let mut agent = Agent::new(
                p,
                model_id,
                memory.clone(),
                config.settings.allow_commands,
                config.settings.compaction_messages,
            );
            app.sync_agent_extras(&mut agent);
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
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        );
        default_panic(info);
    }));

    // input thread -----------------------------------------------------------
    let (tx, mut rx) = unbounded_channel::<TuiEvent>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            match crossterm::event::read() {
                Ok(TermEvent::Key(k)) => {
                    if tx.send(TuiEvent::Key(k)).is_err() { break; }
                }
                Ok(TermEvent::Mouse(m)) => {
                    if tx.send(TuiEvent::Mouse(m)).is_err() { break; }
                }
                Ok(TermEvent::Resize(_, _)) => {
                    if tx.send(TuiEvent::Resize).is_err() { break; }
                }
                Ok(TermEvent::Paste(s)) => {
                    if tx.send(TuiEvent::Paste(s)).is_err() { break; }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        });
    }

    // update check -----------------------------------------------------------
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let info = dragon_core::update::check(dragon_core::VERSION)
                .await
                .ok()
                .flatten()
                .map(|u| format!("{} available (you have v{})", u.latest, u.current));
            let _ = tx.send(TuiEvent::Update(info));
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
            TuiEvent::Mouse(m) => app.on_mouse(m, tx.clone()).await,
            TuiEvent::Resize => Ok(()),
            TuiEvent::Paste(s) => {
                for c in s.replace(['\r'], "").chars() {
                    if c != '\n' {
                        app.push_char(c);
                    } else {
                        app.push_char(' ');
                    }
                }
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
            TuiEvent::Update(note) => {
                app.update_note = note.clone();
                if let Some(n) = note {
                    app.say(Entry::System(format!(
                        "update available: {n}\ndownload: https://github.com/mamad7202202/dragon-agent/releases"
                    )));
                }
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
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    );
    result
}
