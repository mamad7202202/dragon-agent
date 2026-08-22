//! Dragon Agent - desktop app.
//! Same core as the CLI: one config, one memory, one session store.

use dragon_core::agent::{Agent, AgentEvent};
use dragon_core::config::{Config, ProviderCfg};
use dragon_core::memory::MemoryStore;
use dragon_core::presets;
use std::sync::{Arc, Mutex};

// palette - shared identity with the TUI and the website
const EMBER: egui::Color32 = egui::Color32::from_rgb(255, 99, 71);
const FLAME: egui::Color32 = egui::Color32::from_rgb(255, 152, 74);
const GOLD: egui::Color32 = egui::Color32::from_rgb(255, 205, 112);
const NIGHT: egui::Color32 = egui::Color32::from_rgb(18, 17, 20);
const SMOKE: egui::Color32 = egui::Color32::from_rgb(30, 28, 33);
const SCALE: egui::Color32 = egui::Color32::from_rgb(66, 62, 70);
const ASH: egui::Color32 = egui::Color32::from_rgb(124, 122, 129);
const BONE: egui::Color32 = egui::Color32::from_rgb(229, 226, 219);
const JADE: egui::Color32 = egui::Color32::from_rgb(105, 210, 150);
const SKY: egui::Color32 = egui::Color32::from_rgb(108, 170, 245);
const VIOLET: egui::Color32 = egui::Color32::from_rgb(172, 140, 250);

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Chat,
    Memory,
    Providers,
    Settings,
    About,
}

#[derive(Clone)]
enum ChatItem {
    User(String),
    Assistant(String),
    Tool { name: String, detail: String },
    System(String),
}

enum GuiEvent {
    Agent(AgentEvent),
    Done(Result<String, String>),
}

struct ProvForm {
    open: bool,
    name: String,
    preset_idx: usize,
    base_url: String,
    key: String,
    models_raw: String,
    set_default: bool,
}

impl Default for ProvForm {
    fn default() -> Self {
        Self {
            open: false,
            name: String::new(),
            preset_idx: 0,
            base_url: String::new(),
            key: String::new(),
            models_raw: String::new(),
            set_default: false,
        }
    }
}

struct DragonApp {
    tab: Tab,
    cfg: Config,
    memory: Arc<Mutex<MemoryStore>>,
    agent: Option<Arc<tokio::sync::Mutex<Agent>>>,
    model_spec: String,
    rt: tokio::runtime::Runtime,
    tx: std::sync::mpsc::Sender<GuiEvent>,
    rx: std::sync::mpsc::Receiver<GuiEvent>,

    chat: Vec<ChatItem>,
    draft: String,
    streaming: Option<String>,
    busy: bool,

    prov_form: ProvForm,
    mem_search: String,
    mem_draft: String,
    toast: Option<(String, f64)>,
}

impl DragonApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let cfg = Config::load().unwrap_or_default();
        let memory = Arc::new(Mutex::new(MemoryStore::open().unwrap()));
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let (tx, rx) = std::sync::mpsc::channel();

        let mut app = Self {
            tab: Tab::Chat,
            agent: None,
            model_spec: "(none)".into(),
            cfg,
            memory,
            rt,
            tx,
            rx,
            chat: vec![],
            draft: String::new(),
            streaming: None,
            busy: false,
            prov_form: ProvForm::default(),
            mem_search: String::new(),
            mem_draft: String::new(),
            toast: None,
        };

        match app.cfg.resolve_model(None) {
            Ok(_) => {
                if let Err(e) = app.rebuild_agent() {
                    app.chat.push(ChatItem::System(format!("error: {e:#}")));
                }
            }
            Err(e) => {
                app.tab = Tab::Providers;
                app.prov_form.open = true;
                app.chat
                    .push(ChatItem::System(format!("welcome!\n{e}")));
            }
        }
        app
    }

    fn rebuild_agent(&mut self) -> anyhow::Result<()> {
        let (pcfg, mid) = self.cfg.resolve_model(None)?;
        let p = dragon_core::provider::build(pcfg)?;
        let spec = format!("{}/{}", pcfg.name, mid);
        let agent = Agent::new(
            p,
            mid,
            self.memory.clone(),
            self.cfg.settings.allow_commands,
            self.cfg.settings.compaction_messages,
        );
        self.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
        self.model_spec = spec;
        Ok(())
    }

    fn persist(&self) {
        if let Err(e) = self.cfg.save() {
            eprintln!("config save failed: {e:#}");
        }
    }

    fn send_message(&mut self) {
        if self.draft.trim().is_empty() || self.busy {
            return;
        }
        let Some(agent) = self.agent.clone() else {
            self.toast = Some(("no model configured - add one in Providers".into(), 3.0));
            return;
        };
        let text = self.draft.trim().to_string();
        self.draft.clear();
        self.chat.push(ChatItem::User(text.clone()));
        self.streaming = Some(String::new());
        self.busy = true;

        let tx = self.tx.clone();
        self.rt.spawn(async move {
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
                let _ = tx.send(GuiEvent::Agent(ev));
            }
            let res = match job.await {
                Ok(Ok(t)) => Ok(t),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(GuiEvent::Done(res));
        });
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        let mut done_res: Option<Result<String, String>> = None;
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                GuiEvent::Agent(AgentEvent::Delta(d)) => {
                    if let Some(s) = &mut self.streaming {
                        s.push_str(&d);
                    }
                }
                GuiEvent::Agent(AgentEvent::ToolStart { name, detail }) => {
                    self.chat.push(ChatItem::Tool { name, detail });
                }
                GuiEvent::Agent(AgentEvent::Stopped) => {
                    self.busy = false;
                    self.streaming = None;
                    self.chat.push(ChatItem::System("stopped.".into()));
                }
                GuiEvent::Agent(AgentEvent::Compacted) => {
                    self.chat
                        .push(ChatItem::System("context compacted.".into()));
                }
                GuiEvent::Agent(AgentEvent::Error(e)) => {
                    self.chat.push(ChatItem::System(format!("error: {e}")));
                }
                GuiEvent::Agent(AgentEvent::ToolEnd { .. }) => {}
                GuiEvent::Done(res) => done_res = Some(res),
            }
        }
        if let Some(res) = done_res {
            self.busy = false;
            self.streaming = None;
            match res {
                Ok(text)
                    if !text.is_empty()
                        || !matches!(self.chat.last(), Some(ChatItem::System(_))) =>
                {
                    self.chat.push(ChatItem::Assistant(text));
                }
                _ => {}
            }
        }
        if self.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    // ------------------------------------------------------------ providers

    fn save_provider_form(&mut self) {
        let f = &self.prov_form;
        let name = f.name.trim().to_string();
        let url = f.base_url.trim().trim_end_matches('/').to_string();
        let models: Vec<String> = f
            .models_raw
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        if name.is_empty() || url.is_empty() || models.is_empty() {
            self.toast = Some(("name, base url and at least one model are required".into(), 3.5));
            return;
        }

        let kind = if url.contains("anthropic") {
            "anthropic"
        } else {
            "openai"
        };
        let pcfg = ProviderCfg {
            name: name.clone(),
            base_url: url,
            api_key: f.key.trim().to_string(),
            kind: Some(kind.into()),
            models,
        };

        self.cfg.providers.retain(|p| p.name != name);
        self.cfg.providers.push(pcfg);
        if self.prov_form.set_default || self.cfg.default_model.is_none() {
            self.cfg.default_model =
                Some(format!("{}/{}", name, f.models_raw.lines().next().unwrap_or("").trim()));
        }
        self.persist();
        if let Err(e) = self.rebuild_agent() {
            self.toast = Some((format!("saved, but agent failed: {e:#}"), 4.0));
        } else {
            self.toast = Some((format!("saved '{name}'"), 2.0));
        }
        self.prov_form = ProvForm::default();
    }

    fn remove_provider(&mut self, name: &str) {
        self.cfg.providers.retain(|p| p.name != name);
        if let Some(d) = &self.cfg.default_model {
            if d.split_once('/').map(|(p, _)| p) == Some(name) {
                self.cfg.default_model = None;
            }
        }
        self.persist();
        if self.cfg.resolve_model(None).is_ok() {
            let _ = self.rebuild_agent();
        } else {
            self.agent = None;
            self.model_spec = "(none)".into();
        }
        self.toast = Some((format!("removed '{name}'"), 2.0));
    }
}

// ------------------------------------------------------------------ theme

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.dark_mode = true;
    style.visuals.panel_fill = NIGHT;
    style.visuals.window_fill = SMOKE;
    style.visuals.extreme_bg_color = SMOKE;
    style.visuals.faint_bg_color = egui::Color32::from_rgb(24, 23, 27);
    style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, SCALE);
    style.visuals.widgets.inactive.bg_fill = SMOKE;
    style.visuals.widgets.inactive.weak_bg_fill = SMOKE;
    style.visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(40, 37, 43);
    style.visuals.selection.bg_fill = EMBER.gamma_multiply(0.35);
    style.visuals.selection.stroke = egui::Stroke::new(1.0, EMBER);
    style.visuals.override_text_color = Some(BONE);
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    ctx.set_style(style);
}

// ------------------------------------------------------------------- impl

impl eframe::App for DragonApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_theme(ctx);
        self.poll_events(ctx);

        egui::SidePanel::left("nav")
            .exact_width(190.0)
            .resizable(false)
            .show(ctx, |ui| self.nav(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(NIGHT).inner_margin(14.0))
            .show(ctx, |ui| match self.tab {
                Tab::Chat => self.tab_chat(ui),
                Tab::Memory => self.tab_memory(ui),
                Tab::Providers => self.tab_providers(ui),
                Tab::Settings => self.tab_settings(ui),
                Tab::About => self.tab_about(ui),
            });

        if let Some((msg, ttl)) = &mut self.toast {
            let rect = ctx.screen_rect();
            let painter =
                ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("toast")));
            painter.rect_filled(
                egui::Rect::from_center_size(
                    egui::pos2(rect.center().x, rect.bottom() - 42.0),
                    egui::vec2(rect.width() * 0.6, 34.0),
                ),
                8.0,
                SMOKE,
            );
            painter.text(
                egui::pos2(rect.center().x, rect.bottom() - 42.0),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(14.0),
                GOLD,
            );
            *ttl -= ctx.input(|i| i.stable_dt) as f64;
            if *ttl <= 0.0 {
                self.toast = None;
            }
            ctx.request_repaint();
        }
    }
}

impl DragonApp {
    fn nav(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(14.0);
            ui.label(
                egui::RichText::new("DRAGON")
                    .color(EMBER)
                    .strong()
                    .size(20.0),
            );
            ui.label(
                egui::RichText::new("A G E N T")
                    .color(GOLD)
                    .small(),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("v{}", dragon_core::VERSION))
                    .color(SCALE)
                    .small(),
            );
        });
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);

        for (tab, icon_label) in [
            (Tab::Chat, "chat"),
            (Tab::Memory, "memory"),
            (Tab::Providers, "providers"),
            (Tab::Settings, "settings"),
            (Tab::About, "about"),
        ] {
            let selected = self.tab == tab;
            if ui
                .add(egui::SelectableLabel::new(
                    selected,
                    egui::RichText::new(icon_label).color(if selected { EMBER } else { ASH }),
                ))
                .clicked()
            {
                self.tab = tab;
            }
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("by ").small().color(SCALE));
                ui.label(egui::RichText::new(dragon_core::TELEGRAM).small().color(ASH));
            });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(match &self.model_spec.is_empty() {
                    true => "no model",
                    false => self.model_spec.as_str(),
                })
                .small()
                .color(if self.busy { FLAME } else { ASH }),
            );
        });
    }

    // ------------------------------------------------------------------ chat

    fn tab_chat(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&self.model_spec).color(FLAME));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.busy {
                    ui.add(egui::Spinner::new().size(14.0));
                }
                ui.label(egui::RichText::new(if self.busy { "working" } else { "ready" }).small().color(ASH));
            });
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.chat.is_empty() && self.streaming.is_none() {
                    ui.add_space(ui.available_height() / 3.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("DRAGON AGENT").strong().size(26.0).color(EMBER));
                        ui.label(egui::RichText::new("a fast AI agent with a long memory").italics().color(ASH));
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("say something below - your keys stay on this machine.").small().color(SCALE));
                    });
                    return;
                }
                let items = self.chat.clone();
                for item in items {
                    match item {
                        ChatItem::User(t) => bubble(ui, "you", SKY, &t),
                        ChatItem::Assistant(t) => bubble(ui, "dragon", EMBER, &t),
                        ChatItem::Tool { name, detail } => {
                            ui.label(
                                egui::RichText::new(format!("» {} {}", name, truncate(&detail, 90)))
                                    .color(VIOLET)
                                    .small(),
                            );
                        }
                        ChatItem::System(t) => {
                            ui.label(egui::RichText::new(t).small().color(ASH));
                        }
                    }
                    ui.add_space(6.0);
                }
                if let Some(s) = &self.streaming {
                    bubble(ui, "dragon", EMBER, s);
                }
            });

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let send = ui.add_sized(
                [74.0, 30.0],
                egui::Button::new(egui::RichText::new(if self.busy { "..." } else { "send" }).color(BONE)),
            );
            if self.busy {
                if ui.button("stop").clicked() {
                    if let Some(ag) = &self.agent {
                        if let Ok(mut a) = ag.try_lock() {
                            a.stop();
                        }
                    }
                }
            }
            let editable = !self.busy;
            let resp = ui.add_enabled(
                editable,
                egui::TextEdit::singleline(&mut self.draft)
                    .hint_text("message...")
                    .desired_width(f32::INFINITY),
            );
            if resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && !ui.input(|i| i.modifiers.shift)
            {
                self.send_message();
            }
            let _ = send;
        });
    }

    // ---------------------------------------------------------------- memory

    fn tab_memory(&mut self, ui: &mut egui::Ui) {
        ui.heading("long-term memory");
        ui.label(egui::RichText::new("facts the agent recalls by relevance x importance x recency.").small().color(ASH));
        ui.separator();

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.mem_draft)
                    .hint_text("remember a new fact...")
                    .desired_width(ui.available_width() - 90.0),
            );
            if ui.button("save fact").clicked() && !self.mem_draft.trim().is_empty() {
                let fact = {
                    let mut m = self.memory.lock().unwrap();
                    let f = m.add(self.mem_draft.trim(), &["manual".to_string()], 0.8);
                    let _ = m.save();
                    format!("[{}]", f.id)
                };
                self.mem_draft.clear();
                self.toast = Some((format!("saved {}", fact), 2.0));
            }
        });

        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.mem_search)
                .hint_text("search...")
                .desired_width(ui.available_width()),
        );

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let facts: Vec<_> = self.memory.lock().unwrap().all().to_vec();
            let q = self.mem_search.to_lowercase();
            let mut removed: Option<String> = None;
            for f in facts {
                if !q.is_empty() && !f.content.to_lowercase().contains(&q) {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("[{}]", f.id)).small().color(SCALE));
                    ui.label(
                        egui::RichText::new(format!("{:.1}", f.importance))
                            .small()
                            .color(GOLD),
                    );
                    ui.label(&f.content);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("forget").clicked() {
                            removed = Some(f.id.clone());
                        }
                        ui.label(egui::RichText::new(format!("hits {}", f.hits)).small().color(SCALE));
                    });
                });
                ui.separator();
            }
            if let Some(id) = removed {
                self.memory.lock().unwrap().remove(&id);
                let _ = self.memory.lock().unwrap().save();
                self.toast = Some(("forgotten".into(), 1.5));
            }
        });
    }

    // ------------------------------------------------------------- providers

    fn tab_providers(&mut self, ui: &mut egui::Ui) {
        ui.heading("providers");
        ui.label(
            egui::RichText::new("bring your own key - everything stays in your config file.")
                .small()
                .color(ASH),
        );
        ui.separator();

        let providers = self.cfg.providers.clone();
        let default_model = self.cfg.default_model.clone();
        let mut remove: Option<String> = None;

        for p in &providers {
            let is_def = default_model
                .as_deref()
                .map(|d| d.starts_with(&format!("{}/", p.name)))
                .unwrap_or(false);
            let head = format!(
                "{}  {}",
                if is_def { "★" } else { "" },
                p.name
            );
            egui::CollapsingHeader::new(
                egui::RichText::new(head).color(if is_def { GOLD } else { BONE }).strong(),
            )
            .id_salt(format!("prov-{}", p.name))
            .show(ui, |ui| {
                ui.label(egui::RichText::new(&p.base_url).small().color(ASH));
                ui.label(
                    egui::RichText::new(if p.api_key.is_empty() {
                        "key: none (local)".to_string()
                    } else {
                        format!("key: {}••••", &p.api_key[..4.min(p.api_key.len())])
                    })
                    .small()
                    .color(SCALE),
                );
                for m in &p.models {
                    ui.horizontal(|ui| {
                        let is_current = default_model.as_deref()
                            == Some(&format!("{}/{}", p.name, m));
                        if ui
                            .selectable_label(is_current, egui::RichText::new(m).color(if is_current { FLAME } else { BONE }))
                            .clicked()
                        {
                            self.cfg.default_model = Some(format!("{}/{}", p.name, m));
                            self.persist();
                            let _ = self.rebuild_agent();
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if !is_def && ui.small_button("make default provider").clicked() {
                        if let Some(m) = p.models.first() {
                            self.cfg.default_model = Some(format!("{}/{}", p.name, m));
                            self.persist();
                            let _ = self.rebuild_agent();
                        }
                    }
                    if ui.small_button("remove").clicked() {
                        remove = Some(p.name.clone());
                    }
                });
            });
            ui.add_space(2.0);
        }

        if let Some(name) = remove {
            self.remove_provider(&name);
        }

        ui.add_space(8.0);
        let label = if self.prov_form.open { "close add-form" } else { "+ add provider" };
        if ui.button(label).clicked() {
            self.prov_form.open = !self.prov_form.open;
        }

        if self.prov_form.open {
            egui::Frame::group(ui.style())
                .fill(SMOKE)
                .stroke(egui::Stroke::new(1.0, SCALE))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add_space(4.0);

                    let preset_names: Vec<&str> = presets::PRESETS.iter().map(|p| p.name).collect();
                    ui.horizontal(|ui| {
                        ui.label("preset:");
                        egui::ComboBox::from_id_salt("preset-combo")
                            .selected_text(preset_names[self.prov_form.preset_idx.min(preset_names.len()-1)])
                            .width(160.0)
                            .show_ui(ui, |ui| {
                                for (i, n) in preset_names.iter().enumerate() {
                                    ui.selectable_value(&mut self.prov_form.preset_idx, i, *n);
                                }
                                ui.selectable_value(
                                    &mut self.prov_form.preset_idx,
                                    preset_names.len(),
                                    "custom",
                                );
                            });
                        let is_custom =
                            self.prov_form.preset_idx == preset_names.len();
                        if is_custom {
                            ui.label("name:");
                            ui.add(egui::TextEdit::singleline(&mut self.prov_form.name)
                                .hint_text("my-provider"));
                        }
                    });

                    let preset = presets::PRESETS.get(self.prov_form.preset_idx);
                    if let Some(p) = preset {
                        ui.label(
                            egui::RichText::new(p.note).small().color(ASH),
                        );
                    }

                    ui.horizontal(|ui| {
                        ui.label("base url:");
                        let shown_url = if let Some(p) = preset {
                            p.base_url.to_string()
                        } else {
                            self.prov_form.base_url.clone()
                        };
                        let mut edit = shown_url;
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut edit)
                                    .hint_text("https://...")
                                    .desired_width(ui.available_width()),
                            )
                            .changed()
                        {
                            self.prov_form.base_url = edit;
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("api key:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.prov_form.key)
                                .password(true)
                                .hint_text(if preset.map(|p| p.key_required).unwrap_or(false) {
                                    "paste your key"
                                } else {
                                    "not required"
                                })
                                .desired_width(ui.available_width()),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("models:");
                        let suggested = preset
                            .map(|p| p.models.join("\n"))
                            .filter(|_| self.prov_form.models_raw.is_empty());
                        if let Some(s) = suggested {
                            self.prov_form.models_raw = s;
                        }
                        ui.add(
                            egui::TextEdit::multiline(&mut self.prov_form.models_raw)
                                .hint_text("one model id per line")
                                .desired_width(ui.available_width()),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.prov_form.set_default, "set as default");
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("save provider").color(NIGHT),
                                )
                                .fill(EMBER),
                            )
                            .clicked()
                        {
                            self.save_provider_form();
                        }
                    });
                    ui.add_space(4.0);
                });
        }
    }

    // -------------------------------------------------------------- settings

    fn tab_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("settings");
        ui.separator();

        let mut specs: Vec<String> = Vec::new();
        for p in &self.cfg.providers {
            for m in &p.models {
                specs.push(format!("{}/{}", p.name, m));
            }
        }
        ui.horizontal(|ui| {
            ui.label("default model:");
            egui::ComboBox::from_id_salt("default-model")
                .selected_text(self.cfg.default_model.as_deref().unwrap_or("(none)"))
                .width(280.0)
                .show_ui(ui, |ui| {
                    for s in &specs {
                        ui.selectable_value(
                            &mut self.cfg.default_model,
                            Some(s.clone()),
                            s,
                        );
                    }
                });
            if ui.button("apply").clicked() {
                self.persist();
                let _ = self.rebuild_agent();
            }
        });

        ui.add_space(6.0);
        if ui
            .checkbox(&mut self.cfg.settings.allow_commands, "allow shell commands (run_shell tool)")
            .changed()
        {
            self.persist();
            let _ = self.rebuild_agent();
        }
        ui.label(
            egui::RichText::new("when off, the agent cannot execute commands on this machine.")
                .small()
                .color(ASH),
        );

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("compact history after");
            if ui
                .add(
                    egui::DragValue::new(&mut self.cfg.settings.compaction_messages)
                        .range(12..=400)
                        .suffix(" messages"),
                )
                .changed()
            {
                self.persist();
            }
        });

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!(
                "config : {}\ndata   : {}",
                Config::path().display(),
                Config::data_dir().display()
            ))
            .small()
            .color(SCALE),
        );
    }

    fn tab_about(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 4.0);
            ui.label(
                egui::RichText::new("DRAGON")
                    .strong()
                    .size(30.0)
                    .color(EMBER),
            );
            ui.label(egui::RichText::new("A G E N T").size(14.0).color(GOLD));
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("a fast AI agent with a long memory")
                    .italics()
                    .color(ASH),
            );
            ui.add_space(14.0);
            ui.label(egui::RichText::new(format!("version {}", dragon_core::VERSION)).color(ASH));
            ui.label(
                egui::RichText::new(format!(
                    "created by {} ({})",
                    dragon_core::AUTHOR,
                    dragon_core::TELEGRAM
                ))
                .color(ASH),
            );
            ui.label(egui::RichText::new(dragon_core::HOMEPAGE).color(SKY).small());
            ui.label(egui::RichText::new("MIT license").small().color(SCALE));
            ui.add_space(14.0);
            ui.label(
                egui::RichText::new(
                    "this desktop app shares its config, memory and sessions\nwith the dragon terminal client.",
                )
                .small()
                .color(SCALE),
            );
        });
    }
}

fn bubble(ui: &mut egui::Ui, role: &str, color: egui::Color32, text: &str) {
    ui.label(egui::RichText::new(role).strong().color(color).small());
    egui::Frame::none()
        .fill(egui::Color32::from_rgb(24, 23, 27))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(10.0, 7.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new(text).color(BONE));
        });
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1020.0, 680.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("Dragon Agent"),
        ..Default::default()
    };
    eframe::run_native(
        "Dragon Agent",
        options,
        Box::new(|cc| {
            apply_theme(&cc.egui_ctx);
            Ok(Box::new(DragonApp::new(cc)))
        }),
    )
}
