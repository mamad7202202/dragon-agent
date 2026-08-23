//! Provider abstraction + wire-format adapters (OpenAI-compatible & Anthropic).

pub mod anthropic;
pub mod openai;
mod sse;

use crate::config::ProviderCfg;
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON-encoded arguments object.
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into(), ..Default::default() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into(), ..Default::default() }
    }
}

impl Default for Message {
    fn default() -> Self {
        Self { role: Role::User, content: String::new(), tool_calls: Vec::new(), tool_call_id: None }
    }
}

/// Tool schema handed to the model.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Events emitted while a completion streams.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Delta(String),
    ToolCalls(Vec<ToolCall>),
    /// Token accounting reported by the provider (best effort).
    Usage {
        prompt: u64,
        completion: u64,
        total: u64,
    },
    Done,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn display_name(&self) -> &str;

    /// Stream a chat completion. Emits Delta events then either ToolCalls or Done.
    /// `thinking` requests extended reasoning from models that support it.
    async fn stream_chat(
        &self,
        model: &str,
        system: Option<&str>,
        messages: &[Message],
        tools: &[ToolDef],
        thinking: crate::config::Thinking,
        tx: UnboundedSender<StreamEvent>,
    ) -> Result<()>;
}

/// Convenience: run a non-interactive completion and return the full text.
pub async fn complete(
    provider: std::sync::Arc<dyn LlmProvider>,
    model: &str,
    system: Option<&str>,
    messages: &[Message],
) -> Result<String> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let p = provider;
    let m = model.to_string();
    let s = system.map(|x| x.to_string());
    let msgs = messages.to_vec();
    let handle = tokio::spawn(async move {
        p.stream_chat(&m, s.as_deref(), &msgs, &[], crate::config::Thinking::Off, tx).await
    });
    let mut text = String::new();
    while let Some(ev) = rx.recv().await {
        if let StreamEvent::Delta(d) = ev {
            text.push_str(&d);
        }
    }
    handle.await??;
    Ok(text)
}

pub fn build(cfg: &ProviderCfg) -> Result<std::sync::Arc<dyn LlmProvider>> {
    let kind = cfg
        .kind
        .clone()
        .unwrap_or_else(|| guess_kind(&cfg.base_url));
    match kind.as_str() {
        "anthropic" => Ok(std::sync::Arc::new(anthropic::Anthropic::new(cfg.clone()))),
        "openai" => Ok(std::sync::Arc::new(openai::OpenAiCompat::new(cfg.clone()))),
        other => bail!(
            "unknown protocol '{other}' for provider '{}' (expected openai|anthropic)",
            cfg.name
        ),
    }
}

fn guess_kind(url: &str) -> String {
    if url.contains("anthropic") {
        "anthropic".to_string()
    } else {
        "openai".to_string()
    }
}
