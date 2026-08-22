//! The agent loop: user input -> streaming completion -> tool calls -> repeat.

pub mod tools;

use crate::memory::MemoryStore;
use crate::provider::{LlmProvider, Message, StreamEvent};
use anyhow::{bail, Result};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Delta(String),
    ToolStart { name: String, detail: String },
    ToolEnd { name: String },
    Compacted,
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
}

impl Agent {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: impl Into<String>,
        memory: Arc<Mutex<MemoryStore>>,
        allow_commands: bool,
        compaction_after: usize,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            base_system: build_base_system(allow_commands),
            history: Vec::new(),
            ctx: tools::ToolCtx { memory, allow_commands },
            compaction_after,
            tools_enabled: true,
        }
    }

    pub fn set_model(&mut self, provider: Arc<dyn LlmProvider>, model: &str) {
        self.provider = provider;
        self.model = model.to_string();
    }

    /// System prompt for this turn: persona + procedural memory + recalled facts.
    fn system_for_turn(&self, user_input: &str) -> String {
        let mut sys = self.base_system.clone();
        if let Some(proc_mem) = crate::memory::procedural_memory() {
            sys.push_str("\n\n");
            sys.push_str(&proc_mem);
        }
        if let Ok(mut m) = self.ctx.memory.lock() {
            if let Some(block) = m.recall_block(user_input, 6) {
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
        self.history.push(Message::user(user_text));

        for _round in 0..10 {
            let system = self.system_for_turn(user_text);
            let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
            let provider = self.provider.clone();
            let model = self.model.clone();
            let msgs = self.history.clone();
            let tdefs = if self.tools_enabled { tools::defs() } else { vec![] };

            let handle = tokio::spawn(async move {
                provider.stream_chat(&model, Some(&system), &msgs, &tdefs, etx).await
            });

            let mut text = String::new();
            let mut calls: Option<Vec<crate::provider::ToolCall>> = None;
            while let Some(ev) = erx.recv().await {
                match ev {
                    StreamEvent::Delta(d) => {
                        text.push_str(&d);
                        let _ = tx.send(AgentEvent::Delta(d));
                    }
                    StreamEvent::ToolCalls(c) => calls = Some(c),
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
                    let result =
                        tools::execute(&c.name, &c.arguments, &self.ctx).await.unwrap_or_else(|e| {
                            format!("TOOL ERROR: {e:#}")
                        });
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

            if self.history.len() > self.compaction_after.max(10) {
                if let Ok(new_hist) = compact::compact(
                    self.provider.clone(),
                    &self.model,
                    &self.history,
                    8,
                )
                .await
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

fn build_base_system(allow_commands: bool) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let today = chrono::Local::now().format("%Y-%m-%d");
    format!(
        "You are Dragon Agent, a fast terminal AI agent created by mamad720220 (@mamad720220 on Telegram).
Today is {today}. Working directory: {cwd}.

Operating rules:
- Be concise and direct. Light markdown only (**bold**, `code`, - lists). No big headings unless asked.
- Tools available: read_file, write_file, list_files, grep, run_shell{shell_note}, save_memory, search_memory.
- Prefer tools over guessing: read files before editing them, list directories before assuming structure.
- When the user shares a durable preference or fact, call save_memory with it.
- If a request is ambiguous, ask ONE short clarifying question instead of guessing.
- Never claim a file/command succeeded without doing it.",
        shell_note = if allow_commands { "" } else { " (disabled by user)" },
    )
}
