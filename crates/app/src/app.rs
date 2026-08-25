//! Application state: the retained `DragonApp` entity that owns chat history,
//! UI state, and the channel to the agent worker thread.

use crate::bridge::{Bridge, Cmd, Ev};
use crate::input::TextField;
use crate::theme::{self, Theme, ThemeName};
use dragon_core::agent::Mode;
use dragon_core::config::Config;
use dragon_core::memory::graph::GraphStore;
use dragon_core::memory::MemoryStore;
use dragon_core::presets;
use gpui::{
    actions, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, KeyBinding,
    ListAlignment, ListState, Render, SharedString, WeakEntity, Window, prelude::*,
};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex};
use std::time::Duration;

actions!(
    dragon,
    [Cancel, NewSessionAction, ToggleThemeAction, OpenSettings, CycleMode]
);

/// Register app-level shortcuts. Call once at startup.
pub fn init_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, None),
        KeyBinding::new("secondary-n", NewSessionAction, None),
        KeyBinding::new("secondary-d", ToggleThemeAction, None),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("alt-m", CycleMode, None),
    ]);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Chat,
    Memory,
    Providers,
    Settings,
    About,
}

impl Tab {
    pub fn nav_items() -> [(Tab, &'static str); 5] {
        [
            (Tab::Chat, "Chat"),
            (Tab::Memory, "Memory"),
            (Tab::Providers, "Providers"),
            (Tab::Settings, "Settings"),
            (Tab::About, "About"),
        ]
    }
}

/// One rendered row in the conversation.
#[derive(Clone)]
pub enum Item {
    User(String),
    Assistant(String),
    Tool(String, String),
    Approval(u64, String, String),
    System(String),
    Tasks(Vec<(String, bool)>),
}

pub struct DragonApp {
    pub(crate) bridge: Bridge,
    pub(crate) cfg: Config,
    pub(crate) graph: Arc<Mutex<GraphStore>>,
    pub(crate) session_id: String,
    pub(crate) model_spec: String,
    pub(crate) agent_ok: bool,

    pub(crate) items: Vec<Item>,
    pub(crate) streaming: Option<String>,
    pub(crate) busy: bool,
    pub(crate) usage_total: u64,
    pub(crate) pending: Option<(u64, String)>,

    pub(crate) tab: Tab,
    pub(crate) mode: Mode,
    pub(crate) update_latest: Option<String>,
    pub(crate) toast: Option<SharedString>,
    pub(crate) toast_gen: u32,
    pub(crate) animating: bool,
    pub(crate) tick: u8,
    pub(crate) first_agent_error_seen: bool,

    pub(crate) list: ListState,
    pub(crate) focus: FocusHandle,
    pub(crate) composer: Entity<TextField>,
    pub(crate) form_open: bool,
    pub(crate) pv_idx: usize,
    pub(crate) f_name: Entity<TextField>,
    pub(crate) f_url: Entity<TextField>,
    pub(crate) f_key: Entity<TextField>,
    pub(crate) f_models: Entity<TextField>,
}

impl Focusable for DragonApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl DragonApp {
    pub fn new(
        cfg: Config,
        memory: Arc<Mutex<MemoryStore>>,
        graph: Arc<Mutex<GraphStore>>,
        cx: &mut Context<Self>,
    ) -> Self {
        theme::set_global_theme(
            cx,
            Theme::new(if cfg.settings.theme == "light" {
                ThemeName::Light
            } else {
                ThemeName::Dark
            }),
        );

        let composer = cx.new(|cx| TextField::new("message dragon…   enter to send", false, cx));
        let f_name = cx.new(|cx| TextField::new("provider name (custom only)", false, cx));
        let f_url = cx.new(|cx| TextField::new("base url  https://…", false, cx));
        let f_key = cx.new(|cx| TextField::new("api key", true, cx));
        let f_models = cx.new(|cx| TextField::new("models, comma separated", false, cx));

        let bridge = Bridge::launch(cfg.clone(), memory, graph.clone());
        let start_mode = Mode::parse(&cfg.settings.default_mode).unwrap_or(Mode::Agent);
        let this = Self {
            bridge,
            cfg,
            graph,
            session_id: String::new(),
            model_spec: "(none)".into(),
            agent_ok: false,
            items: vec![],
            streaming: None,
            busy: false,
            usage_total: 0,
            pending: None,
            tab: Tab::Chat,
            mode: start_mode,
            update_latest: None,
            toast: None,
            toast_gen: 0,
            animating: false,
            tick: 0,
            first_agent_error_seen: false,
            list: ListState::new(0, ListAlignment::Bottom, gpui::px(400.)),
            focus: cx.focus_handle(),
            composer,
            form_open: false,
            pv_idx: 0,
            f_name,
            f_url,
            f_key,
            f_models,
        };
        this.spawn_event_loop(cx);
        this
    }

    // ------------------------------------------------------------- plumbing

    fn spawn_event_loop(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            loop {
                let mut had_events = false;
                let mut connected = true;
                this.update(cx, |app, cx| {
                    loop {
                        match app.bridge.rx.try_recv() {
                            Ok(ev) => {
                                had_events = true;
                                app.handle_ev(ev, cx);
                            }
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => {
                                connected = false;
                                break;
                            }
                        }
                    }
                })
                .ok();
                if !connected {
                    return;
                }
                let pause = if had_events {
                    Duration::from_millis(8)
                } else {
                    Duration::from_millis(90)
                };
                cx.background_executor().timer(pause).await;
            }
        })
        .detach();
    }

    /// Kick the busy-dot animation while a turn runs.
    fn ensure_animation(&mut self, cx: &mut Context<Self>) {
        if self.animating {
            return;
        }
        self.animating = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(320))
                    .await;
                let mut running = false;
                let alive = this.update(cx, |app, cx| {
                    running = app.busy;
                    if running {
                        app.tick = app.tick.wrapping_add(1);
                        cx.notify();
                    } else {
                        app.animating = false;
                    }
                });
                if alive.is_err() || !running {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn show_toast(
        &mut self,
        msg: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.toast_gen += 1;
        let gen = self.toast_gen;
        self.toast = Some(msg.into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(2600))
                .await;
            this.update(cx, |app, cx| {
                if app.toast_gen == gen {
                    app.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Keep the virtualized list in sync with the rendered rows.
    fn sync_list(&mut self) {
        let target = self.row_count();
        let current = self.list.item_count();
        if target > current {
            self.list.splice(current..current, target - current);
        } else if target < current {
            self.list.reset(target);
        } else if target > 0 {
            // Same count, but the last row (streaming bubble) changed height.
            self.list.splice(target - 1..target, 1);
        }
    }

    /// Rows rendered in the chat list: items + optional streaming bubble.
    pub(crate) fn row_count(&self) -> usize {
        self.items.len() + usize::from(self.streaming.is_some())
    }

    pub(crate) fn pending_approval(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn streaming_text(&self) -> String {
        self.streaming.clone().unwrap_or_default()
    }

    /// Fold buffered streaming text into a finished assistant bubble.
    fn flush_streaming(&mut self) {
        if let Some(s) = self.streaming.take() {
            if !s.trim().is_empty() {
                self.items.push(Item::Assistant(s));
            }
        }
    }

    fn handle_ev(&mut self, ev: Ev, cx: &mut Context<Self>) {
        match ev {
            Ev::History(msgs) => {
                for m in msgs {
                    match m.role {
                        dragon_core::provider::Role::User if !m.content.trim().is_empty() => {
                            self.items.push(Item::User(m.content))
                        }
                        dragon_core::provider::Role::Assistant
                            if !m.content.trim().is_empty() =>
                        {
                            self.items.push(Item::Assistant(m.content))
                        }
                        _ => {}
                    }
                }
                self.sync_list();
            }
            Ev::SessionStarted { id } => {
                self.session_id = id;
                self.items.clear();
                self.streaming = None;
                self.pending = None;
                self.busy = false;
                self.items.push(Item::System(format!("session {}", self.session_id)));
                self.sync_list();
            }
            Ev::AgentReady { spec } => {
                self.model_spec = spec;
                self.agent_ok = true;
                self.first_agent_error_seen = true;
            }
            Ev::AgentError { message } => {
                self.agent_ok = false;
                self.items.push(Item::System(message));
                self.sync_list();
                if !self.first_agent_error_seen {
                    self.first_agent_error_seen = true;
                    self.tab = Tab::Providers;
                    self.form_open = true;
                }
            }
            Ev::Cfg(cfg) => {
                self.cfg = *cfg;
                theme::set_global_theme(
                    cx,
                    Theme::new(if self.cfg.settings.theme == "light" {
                        ThemeName::Light
                    } else {
                        ThemeName::Dark
                    }),
                );
            }
            Ev::Delta(d) => {
                self.streaming.get_or_insert_with(String::new).push_str(&d);
                self.sync_list();
            }
            Ev::ToolStart { name, detail } => {
                self.flush_streaming();
                self.items.push(Item::Tool(name, detail));
                self.sync_list();
            }
            Ev::Approval { id, tool, detail } => {
                self.flush_streaming();
                self.pending = Some((id, tool.clone()));
                self.items.push(Item::Approval(id, tool, detail));
                self.sync_list();
            }
            Ev::UsageTotal(total) => self.usage_total = total,
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
                self.sync_list();
            }
            Ev::Compacted => {
                self.items.push(Item::System("context compacted".into()));
                self.sync_list();
            }
            Ev::Stopped => {
                self.busy = false;
                self.streaming = None;
                self.pending = None;
                self.items.push(Item::System("stopped".into()));
                self.sync_list();
            }
            Ev::Explanation(t) => {
                self.items
                    .push(Item::System(format!("what this does:\n{t}")));
                self.sync_list();
            }
            Ev::Error(e) => {
                self.busy = false;
                self.streaming = None;
                self.items.push(Item::System(format!("error: {e}")));
                self.sync_list();
            }
            Ev::Done(res) => {
                self.busy = false;
                self.pending = None;
                let streamed = self.streaming.take();
                let text = match res {
                    Ok(t) if !t.trim().is_empty() => Some(t),
                    _ => streamed,
                };
                match text {
                    Some(t) if !t.trim().is_empty() => self.items.push(Item::Assistant(t)),
                    _ => {}
                }
                self.sync_list();
            }
            Ev::UpdateAvailable { latest } => self.update_latest = Some(latest),
        }
        if self.busy {
            self.ensure_animation(cx);
        }
        cx.notify();
    }

    // ------------------------------------------------------------ commands

    pub fn send_message(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text().trim().to_string();
        if text.is_empty() || self.busy {
            return;
        }
        if !self.agent_ok {
            self.show_toast("configure a provider first", cx);
            self.tab = Tab::Providers;
            return;
        }
        self.composer.update(cx, |f, cx| f.clear(cx));
        self.items.push(Item::User(text.clone()));
        self.streaming = Some(String::new());
        self.busy = true;
        self.bridge.send(Cmd::Send { text });
        self.ensure_animation(cx);
        self.sync_list();
        cx.notify();
    }

    pub fn answer(&mut self, allowed: bool, always: bool, cx: &mut Context<Self>) {
        let Some((id, tool)) = self.pending.take() else {
            return;
        };
        self.bridge.send(Cmd::Respond { id, allowed });
        if allowed && always {
            self.bridge.send(Cmd::AddAutoApprove(tool.clone()));
            self.items
                .push(Item::System(format!("always allow '{tool}' saved")));
        } else if !allowed {
            self.items
                .push(Item::System("denied - dragon will adapt".into()));
        }
        self.sync_list();
        cx.notify();
    }

    pub fn explain_pending(&mut self, cx: &mut Context<Self>) {
        if let Some((_, tool)) = &self.pending {
            let detail = self
                .items
                .iter()
                .rev()
                .find_map(|i| match i {
                    Item::Approval(_, t, d) if t == tool => Some(d.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            self.bridge.send(Cmd::Explain {
                tool: tool.clone(),
                detail,
            });
        }
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            self.bridge.send(Cmd::Stop);
        }
        cx.notify();
    }

    pub fn new_session(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            self.show_toast("wait for the current turn to finish", cx);
            return;
        }
        self.bridge.send(Cmd::NewSession);
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    /// Config mutations that only need an echo + persist (no agent rebuild).
    pub(crate) fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.bridge.send(Cmd::ToggleTheme);
        cx.notify();
    }

    pub(crate) fn toggle_shell(&mut self, cx: &mut Context<Self>) {
        self.bridge.send(Cmd::ToggleShell);
        cx.notify();
    }

    pub(crate) fn toggle_engine(&mut self, cx: &mut Context<Self>) {
        self.bridge.send(Cmd::ToggleEngine);
        cx.notify();
    }

    pub(crate) fn cycle_thinking(&mut self, cx: &mut Context<Self>) {
        self.bridge.send(Cmd::CycleThinking);
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.bridge.send(Cmd::SetMode(mode));
        cx.notify();
    }

    pub fn cycle_mode(&mut self, cx: &mut Context<Self>) {
        let next = match self.mode {
            Mode::Agent => Mode::Plan,
            Mode::Plan => Mode::Chat,
            Mode::Chat => Mode::Agent,
        };
        self.set_mode(next, cx);
    }

    pub fn open_update_page(&mut self, cx: &mut Context<Self>) {
        let url = dragon_core::update::latest_download_url(true);
        let _ = dragon_core::update::open_browser(&url);
        self.show_toast("opening download…", cx);
    }

    pub fn open_homepage(&mut self, _cx: &mut Context<Self>) {
        let _ = dragon_core::update::open_browser(dragon_core::HOMEPAGE);
    }

    pub fn copy_session_id(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.session_id.clone()));
        self.show_toast("session id copied", cx);
    }

    /// True when no text field owns the keyboard focus right now.
    pub fn typing_in_a_field(&self, window: &Window, cx: &App) -> bool {
        [
            &self.composer,
            &self.f_name,
            &self.f_url,
            &self.f_key,
            &self.f_models,
        ]
        .iter()
        .any(|f| f.read(cx).focus_handle.is_focused(window))
    }

    // ---- provider form ----

    pub fn toggle_form(&mut self, cx: &mut Context<Self>) {
        self.form_open = !self.form_open;
        cx.notify();
    }

    pub fn select_preset(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.pv_idx = idx;
        if let Some(p) = presets::PRESETS.get(idx) {
            let url = p.base_url.to_string();
            let models = p.models.join(", ");
            self.f_url.update(cx, |f, cx| f.set_text(url, cx));
            self.f_models.update(cx, |f, cx| f.set_text(models, cx));
        }
        cx.notify();
    }

    pub fn save_provider(&mut self, cx: &mut Context<Self>) {
        let custom = self.pv_idx >= presets::PRESETS.len();
        let name = if custom {
            self.f_name.read(cx).text().trim().to_string()
        } else {
            presets::PRESETS
                .get(self.pv_idx)
                .map(|p| p.name.to_string())
                .unwrap_or_default()
        };
        let url = self.f_url.read(cx).text().trim().to_string();
        let url = url.trim_end_matches('/').to_string();
        let key = self.f_key.read(cx).text().trim().to_string();
        let models: Vec<String> = self
            .f_models
            .read(cx)
            .text()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let key_required = presets::PRESETS
            .get(self.pv_idx)
            .map(|p| p.key_required)
            .unwrap_or(true);
        if name.is_empty() || url.is_empty() || models.is_empty() || (key_required && key.is_empty())
        {
            self.show_toast("name, url, models (and a key) are required", cx);
            return;
        }
        self.bridge.send(Cmd::SaveProvider {
            name: name.clone(),
            url,
            key,
            models,
        });
        self.form_open = false;
        self.f_key.update(cx, |f, cx| f.clear(cx));
        self.show_toast(format!("saved '{name}'"), cx);
        cx.notify();
    }

    pub fn delete_provider(&mut self, name: String, cx: &mut Context<Self>) {
        self.bridge.send(Cmd::DeleteProvider(name));
        self.show_toast("provider removed", cx);
    }
}

impl Render for DragonApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::views::render_root(self, window, cx)
    }
}
