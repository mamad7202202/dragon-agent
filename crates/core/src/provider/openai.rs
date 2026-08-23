//! OpenAI-compatible chat-completions streaming (OpenAI, OpenRouter, Groq,
//! Together, DeepSeek, Ollama, LM Studio, vLLM, ...).

use super::{sse, LlmProvider, Message, Role, StreamEvent, ToolCall, ToolDef};
use crate::config::ProviderCfg;
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::mpsc::UnboundedSender;

pub struct OpenAiCompat {
    cfg: ProviderCfg,
    http: reqwest::Client,
}

impl OpenAiCompat {
    pub fn new(cfg: ProviderCfg) -> Self {
        Self { cfg, http: reqwest::Client::new() }
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompat {
    fn display_name(&self) -> &str {
        &self.cfg.name
    }

    async fn stream_chat(
        &self,
        model: &str,
        system: Option<&str>,
        messages: &[Message],
        tools: &[ToolDef],
        thinking: crate::config::Thinking,
        tx: UnboundedSender<StreamEvent>,
    ) -> Result<()> {
        let mut wire: Vec<Value> = Vec::new();
        if let Some(s) = system {
            if !s.is_empty() {
                wire.push(json!({ "role": "system", "content": s }));
            }
        }
        for m in messages {
            match m.role {
                Role::System => wire.push(json!({ "role": "system", "content": m.content })),
                Role::User => wire.push(json!({ "role": "user", "content": m.content })),
                Role::Assistant => {
                    let mut o = json!({ "role": "assistant" });
                    if !m.content.is_empty() {
                        o["content"] = json!(m.content);
                    }
                    if !m.tool_calls.is_empty() {
                        o["tool_calls"] = json!(m
                            .tool_calls
                            .iter()
                            .map(|c| json!({
                                "id": c.id,
                                "type": "function",
                                "function": { "name": c.name, "arguments": c.arguments },
                            }))
                            .collect::<Vec<_>>());
                    }
                    wire.push(o);
                }
                Role::Tool => wire.push(json!({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                })),
            }
        }

        let mut body = json!({ "model": model, "messages": wire, "stream": true });
        if !tools.is_empty() {
            body["tools"] = json!(tools
                .iter()
                .map(|t| json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    },
                }))
                .collect::<Vec<_>>());
        }
        // best-effort extras - dropped automatically if the provider rejects them
        body["stream_options"] = json!({ "include_usage": true });
        let effort = match thinking {
            crate::config::Thinking::Low => Some("low"),
            crate::config::Thinking::Medium => Some("medium"),
            crate::config::Thinking::High => Some("high"),
            _ => None,
        };
        if let Some(eff) = effort {
            body["reasoning_effort"] = json!(eff);
        }

        let url = format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'));
        let mut no_usage = false;
        let mut no_think = effort.is_none();
        let resp = loop {
            let mut b = body.clone();
            if no_usage {
                b.as_object_mut().unwrap().remove("stream_options");
            }
            if no_think {
                b.as_object_mut().unwrap().remove("reasoning_effort");
            }
            let r = self
                .http
                .post(&url)
                .bearer_auth(&self.cfg.api_key)
                .json(&b)
                .send()
                .await?;
            if r.status().is_client_error() {
                let status = r.status();
                let txt = r.text().await.unwrap_or_default();
                let lower = txt.to_lowercase();
                if !no_think && lower.contains("reasoning") {
                    no_think = true;
                    continue;
                }
                if !no_usage && (lower.contains("stream_options") || lower.contains("usage")) {
                    no_usage = true;
                    continue;
                }
                bail!("{} returned {}: {}", self.cfg.name, status, truncate(&txt, 500));
            }
            break r;
        };

        // index -> (id, name, arguments-so-far)
        let mut acc: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
        let mut wants_tools = false;
        let tx2 = tx.clone();

        super::sse::pump(resp.bytes_stream(), |data| {
            if data == "[DONE]" {
                return Ok(false);
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                return Ok(true);
            };
            if let Some(err) = v.get("error") {
                bail!("provider error: {err}");
            }
            let choice = &v["choices"][0];
            if let Some(delta) = choice.get("delta") {
                if let Some(txt) = delta.get("content").and_then(|x| x.as_str()) {
                    if !txt.is_empty() {
                        let _ = tx2.send(StreamEvent::Delta(txt.to_string()));
                    }
                }
                if let Some(calls) = delta.get("tool_calls").and_then(|x| x.as_array()) {
                    for tc in calls {
                        let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                        let entry = acc.entry(idx).or_default();
                        if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                            entry.0 = id.to_string();
                        }
                        if let Some(f) = tc.get("function") {
                            if let Some(n) = f.get("name").and_then(|x| x.as_str()) {
                                entry.1 = n.to_string();
                            }
                            if let Some(a) = f.get("arguments").and_then(|x| x.as_str()) {
                                entry.2.push_str(a);
                            }
                        }
                    }
                }
            }
            if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
                if fr == "tool_calls" || fr == "function_call" {
                    wants_tools = true;
                }
            }
            Ok(true)
        })
        .await?;

        if wants_tools && !acc.is_empty() {
            let calls: Vec<ToolCall> = acc
                .into_values()
                .map(|(id, name, arguments)| ToolCall { id, name, arguments })
                .collect();
            let _ = tx.send(StreamEvent::ToolCalls(calls));
        }
        let _ = tx.send(StreamEvent::Done);
        Ok(())
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}
