//! The agent loop: user input -> streaming completion -> tool calls -> repeat.
//!
//! Includes conversation modes (chat / plan / agent) and an approval gate so
//! world-changing tools always pass through the user unless pre-allowed.

pub mod tools;

use crate::memory::compact;
use crate::memory::MemoryStore;
use crate::provider::{LlmProvider, Message, StreamEvent};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

/// Conversation modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Pure conversation - no tools at all.
    Chat,
    /// Read-only research, ends in a written plan.
    Plan,
    /// Full toolkit with permission gating.
    #[default]
    Agent,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Chat => "chat",
            Mode::Plan => "plan",
            Mode::Agent => "agent",
        }
    }
    pub fn parse(s: &str) -> Option<Mode> {
        match s.to_ascii_lowercase().as_str() {
            "chat" => Some(Mode::Chat),
            "plan" => Some(Mode::Plan),
            "agent" => Some(Mode::Agent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Delta(String),
    ToolStart { name: String, detail: String },
    ToolEnd { name: String },
    Compacted,
    Stopped,
    /// A gated tool wants to run. Answer via `Agent::respond(id, allowed)`.
    ApprovalRequest { id: u64, tool: String, detail: String },
    /// Live token accounting for the current turn.
    Usage { prompt: u64, completion: u64, total: u64 },
    /// The task board changed (task_write tool).
    Tasks(serde_json::Value),
    Error(String),
}

pub struct Agent {
    pub provider: Arc<dyn LlmProvider>,
    pub model: String,
    base_system: String,
    pub history: Vec<Message>,
    pub ctx: tools::ToolCtx,
    compaction_after: usize,
    pub tools_enabled: bool,
    pub mode: Mode,
    pub session_id: Option<String>,
    /// Patterns like "write_file" or "run_shell:npm" that skip the prompt.
    pub auto_approve: Vec<String>,
    pub thinking: crate::config::Thinking,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    approvals: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<bool>>>>,
    next_id: AtomicU64,
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: impl Into<String>,
        memory: Arc<Mutex<MemoryStore>>,
        allow_commands: bool,
        compaction_after: usize,
    ) -> Self {
        let base_system = build_base_system(Mode::default(), allow_commands);
        Self {
            provider,
            model: model.into(),
            base_system,
            history: Vec::new(),
            ctx: tools::ToolCtx { memory, allow_commands, session_id: None, graph: None },
            compaction_after,
            tools_enabled: true,
            mode: Mode::default(),
            session_id: None,
            auto_approve: Vec::new(),
            thinking: crate::config::Thinking::Off,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            approvals: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
        }
    }

    /// Request cancellation of the in-flight turn (also cancels pending asks).
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Attach the memory-graph engine (Some = graph mode active).
    pub fn set_engine(&mut self, graph: Option<Arc<Mutex<crate::memory::graph::GraphStore>>>) {
        self.ctx.graph = graph;
    }

    pub fn set_thinking(&mut self, t: crate::config::Thinking) {
        self.thinking = t;
    }

    pub fn set_model(&mut self, provider: Arc<dyn LlmProvider>, model: &str) {
        self.provider = provider;
        self.model = model.to_string();
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.base_system = build_base_system(mode, self.ctx.allow_commands);
    }

    pub fn set_session(&mut self, id: Option<&str>) {
        self.session_id = id.map(|s| s.to_string());
        self.ctx.session_id = self.session_id.clone();
    }

    pub fn set_auto_approve(&mut self, patterns: Vec<String>) {
        self.auto_approve = patterns;
    }

    /// Deliver a user decision for a pending approval. Returns false when the
    /// request no longer exists.
    pub fn respond(&self, id: u64, allowed: bool) -> bool {
        if let Some(tx) = self.approvals.lock().unwrap().remove(&id) {
            let _ = tx.send(allowed);
            true
        } else {
            false
        }
    }

    /// System prompt for this turn: persona + procedural memory + recalled facts.
    fn system_for_turn(&self, user_input: &str) -> String {
        let mut sys = self.base_system.clone();
        if let Some(proc_mem) = crate::memory::procedural_memory() {
            sys.push_str("\n\n");
            sys.push_str(&proc_mem);
        }
        if let Some(g) = &self.ctx.graph {
            // info-graph engine: whole compact outline instead of fuzzy recall
            if let Ok(g) = g.lock() {
                if let Some(block) = g.render(self.session_id.as_deref(), 220) {
                    sys.push_str("\n\n");
                    sys.push_str(&block);
                    sys.push_str("\nKeep the graph accurate: call graph_set_section after every significant milestone or change of direction.");
                }
            }
        } else if let Ok(mut m) = self.ctx.memory.lock() {
            if let Some(block) = m.recall_block_scoped(user_input, 6, self.session_id.as_deref()) {
                sys.push_str("\n\n");
                sys.push_str(&block);
            }
        }
        sys
    }

    /// Run one user turn to completion. Streams deltas through `tx` and
    /// executes any requested tools. Returns the final assistant text.
    pub async fn turn(
        &mut self,
        user_text: &str,
        tx: UnboundedSender<AgentEvent>,
    ) -> Result<String> {
        self.cancel.store(false, Ordering::Relaxed);
        self.history.push(Message::user(user_text));

        let max_rounds = match self.mode {
            Mode::Chat => 1,
            _ => 12,
        };
        for _round in 0..max_rounds {
            let system = self.system_for_turn(user_text);
            let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
            let provider = self.provider.clone();
            let model = self.model.clone();
            let msgs = self.history.clone();
            let tdefs = if self.tools_enabled && self.mode != Mode::Chat {
                tools::defs(&self.mode, self.ctx.allow_commands, self.ctx.graph.is_some())
            } else {
                vec![]
            };

            let thinking = self.thinking;
            let handle = tokio::spawn(async move {
                provider.stream_chat(&model, Some(&system), &msgs, &tdefs, thinking, etx).await
            });

            let mut text = String::new();
            let mut calls: Option<Vec<crate::provider::ToolCall>> = None;
            while let Some(ev) = erx.recv().await {
                if self.cancel.load(Ordering::Relaxed) {
                    handle.abort();
                    let _ = tx.send(AgentEvent::Stopped);
                    return Ok(String::new());
                }
                match ev {
                    StreamEvent::Delta(d) => {
                        text.push_str(&d);
                        let _ = tx.send(AgentEvent::Delta(d));
                    }
                    StreamEvent::ToolCalls(c) => calls = Some(c),
                    StreamEvent::Usage { prompt, completion, total } => {
                        let _ = tx.send(AgentEvent::Usage { prompt, completion, total });
                    }
                    StreamEvent::Done => {}
                }
            }
            handle.await??;

            if let Some(calls) = calls {
                self.history.push(Message {
                    role: crate::provider::Role::Assistant,
                    content: text,
                    tool_calls: calls.clone(),
                    ..Default::default()
                });
                for c in calls {
                    let detail: String = c.arguments.chars().take(140).collect();
                    let _ = tx.send(AgentEvent::ToolStart { name: c.name.clone(), detail });

                    // ---- approval gate -----------------------------------
                    let outcome = if tools::tier_of(&c.name) == tools::Tier::Gated
                        && !auto_approved(&self.auto_approve, &c.name, &c.arguments)
                    {
                        let (atx, arx) = tokio::sync::oneshot::channel::<bool>();
                        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                        self.approvals.lock().unwrap().insert(id, atx);
                        let summary = summarize_for_card(&c.name, &c.arguments);
                        let _ = tx.send(AgentEvent::ApprovalRequest {
                            id,
                            tool: c.name.clone(),
                            detail: summary,
                        });
                        let mut rx = arx;
                        let approved = loop {
                            tokio::select! {
                                r = &mut rx => break r.unwrap_or(false),
                                _ = tokio::time::sleep(std::time::Duration::from_millis(180)) => {
                                    if self.cancel.load(Ordering::Relaxed) {
                                        self.approvals.lock().unwrap().remove(&id);
                                        let _ = tx.send(AgentEvent::Stopped);
                                        return Ok(String::new());
                                    }
                                }
                            }
                        };
                        approved
                    } else {
                        true
                    };

                    let result = if outcome {
                        let ctx = self.ctx.clone_ctx();
                        tools::execute(&c.name, &c.arguments, &ctx)
                            .await
                            .unwrap_or_else(|e| format!("TOOL ERROR: {e:#}"))
                    } else {
                        "USER DENIED this action.".to_string()
                    };

                    if c.name == "task_write" {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&c.arguments) {
                            if let Some(list) = v.get("tasks") {
                                let _ = tx.send(AgentEvent::Tasks(list.clone()));
                            }
                        }
                    }
                    let _ = tx.send(AgentEvent::ToolEnd { name: c.name });
                    self.history.push(Message {
                        role: crate::provider::Role::Tool,
                        content: result,
                        tool_call_id: Some(c.id),
                        ..Default::default()
                    });
                }
                continue; // feed results back to the model
            }

            // plain answer - turn complete
            self.history.push(Message::assistant(text.clone()));

            if self.history.len() > self.compaction_after.max(10) && self.mode != Mode::Chat {
                if let Ok(new_hist) =
                    compact::compact(self.provider.clone(), &self.model, &self.history, 8).await
                {
                    self.history = new_hist;
                    let _ = tx.send(AgentEvent::Compacted);
                }
            }
            return Ok(text);
        }
        bail!("too many consecutive tool rounds (agent loop guard)")
    }

    pub fn reset(&mut self) {
        self.history.clear();
    }
}

// expose ToolCtx cloner without deriving Clone on the memory mutex wrapper
impl tools::ToolCtx {
    fn clone_ctx(&self) -> tools::ToolCtx {
        tools::ToolCtx {
            memory: self.memory.clone(),
            allow_commands: self.allow_commands,
            session_id: self.session_id.clone(),
            graph: self.graph.clone(),
        }
    }
}

fn auto_approved(patterns: &[String], tool: &str, arguments: &str) -> bool {
    let val: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or(serde_json::json!({}));
    patterns.iter().any(|p| {
        let mut it = p.splitn(2, ':');
        let t = it.next().unwrap_or("").trim();
        if t != tool {
            return false;
        }
        match it.next() {
            None => true,
            Some(prefix) => {
                let cmd = val.get("command").and_then(|x| x.as_str()).unwrap_or("");
                cmd.trim_start().starts_with(prefix.trim())
            }
        }
    })
}

fn summarize_for_card(tool: &str, arguments: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(arguments).unwrap_or(serde_json::json!({}));
    let path = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("?").to_string();
    match tool {
        "write_file" | "edit_file" | "read_file" | "delete_file" => path("path"),
        "run_shell" => path("command").chars().take(120).collect(),
        _ => arguments.chars().take(120).collect(),
    }
}

fn build_base_system(mode: Mode, allow_commands: bool) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let today = chrono::Local::now().format("%Y-%m-%d");
    let mode_block = match mode {
        Mode::Chat => "\
MODE: CHAT - plain conversation. No tools are available; do not pretend to use any.",
        Mode::Plan => "\
MODE: PLAN - you may only RESEARCH (read files, list, grep, fetch_url). \
Produce a concrete numbered plan with file paths and steps. Do not modify anything.",
        Mode::Agent => "\
MODE: AGENT - full toolkit. Rules:
- Prefer edit_file over write_file for existing files.
- Destructive or irreversible actions (delete_file, risky shell) deserve a one-line warning first.
- When a user denies permission, adapt instead of retrying.",
    };
    format!(
        "You are Dragon Agent, a fast terminal AI agent.
Today is {today}. Working directory: {cwd}.

{mode_block}

Operating rules:
- Be concise and direct. Light markdown only (**bold**, `code`, - lists).
- Prefer tools over guessing: read files before editing them, list before assuming structure.
- save_memory: default scope=session (facts about this task/project); scope=global only for durable user preferences.
- If a request is ambiguous, ask ONE short clarifying question instead of guessing.
- Never claim a file/command succeeded without doing it.{shell_note}",
        shell_note = if allow_commands {
            ""
        } else {
            "\n- Shell access is disabled by user settings."
        },
    )
}
