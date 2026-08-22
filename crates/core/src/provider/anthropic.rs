//! Anthropic Messages API streaming adapter.

use super::{sse, LlmProvider, Message, Role, StreamEvent, ToolCall, ToolDef};
use crate::config::ProviderCfg;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::mpsc::UnboundedSender;

pub struct Anthropic {
    cfg: ProviderCfg,
    http: reqwest::Client,
}

impl Anthropic {
    pub fn new(cfg: ProviderCfg) -> Self {
        Self { cfg, http: reqwest::Client::new() }
    }

    fn endpoint(&self) -> String {
        let base = self.cfg.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        }
    }
}

fn push_userish(wire: &mut Vec<Value>, role: &str, block: Value) {
    // Merge into the previous message of the same role (Anthropic requires
    // strict role alternation).
    if let Some(last) = wire.last_mut() {
        if last.get("role").and_then(|r| r.as_str()) == Some(role) {
            if let Some(arr) = last.get_mut("content").and_then(|c| c.as_array_mut()) {
                arr.push(block);
                return;
            }
        }
    }
    wire.push(json!({ "role": role, "content": [block] }));
}

#[async_trait]
impl LlmProvider for Anthropic {
    fn display_name(&self) -> &str {
        &self.cfg.name
    }

    async fn stream_chat(
        &self,
        model: &str,
        system: Option<&str>,
        messages: &[Message],
        tools: &[ToolDef],
        tx: UnboundedSender<StreamEvent>,
    ) -> Result<()> {
        let mut wire: Vec<Value> = Vec::new();
        for m in messages {
            match m.role {
                Role::System => continue,
                Role::User => {
                    push_userish(&mut wire, "user", json!({ "type": "text", "text": m.content }))
                }
                Role::Tool => push_userish(
                    &mut wire,
                    "user",
                    json!({
                        "type": "tool_result",
                        "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                        "content": [{ "type": "text", "text": m.content }],
                    }),
                ),
                Role::Assistant => {
                    if m.tool_calls.is_empty() {
                        push_userish(
                            &mut wire,
                            "assistant",
                            json!({ "type": "text", "text": m.content }),
                        );
                    } else {
                        // assistant blocks must not be merged with other roles;
                        // they follow their own turn.
                        let mut blocks: Vec<Value> = Vec::new();
                        if !m.content.is_empty() {
                            blocks.push(json!({ "type": "text", "text": m.content }));
                        }
                        for c in &m.tool_calls {
                            let input: Value =
                                serde_json::from_str(&c.arguments).unwrap_or_else(|_| json!({}));
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": c.id,
                                "name": c.name,
                                "input": input,
                            }));
                        }
                        wire.push(json!({ "role": "assistant", "content": blocks }));
                    }
                }
            }
        }

        let mut body = json!({
            "model": model,
            "max_tokens": 8192,
            "stream": true,
            "messages": wire,
        });
        if let Some(s) = system {
            if !s.is_empty() {
                body["system"] = json!(s);
            }
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools
                .iter()
                .map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                }))
                .collect::<Vec<_>>());
        }

        let resp = self
            .http
            .post(self.endpoint())
            .header("x-api-key", &self.cfg.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("{} returned {status}: {}", self.cfg.name, super::openai::truncate(&text, 500));
        }

        // index -> (tool_use_id, name, partial-json-input)
        let mut tool_blocks: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
        let mut wants_tools = false;
        let tx2 = tx.clone();

        super::sse::pump(resp.bytes_stream(), |data| {
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                return Ok(true);
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("error") => bail!("anthropic error: {}", v["error"]["message"]),
                Some("content_block_start") => {
                    if v["content_block"]["type"] == "tool_use" {
                        let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                        tool_blocks.insert(
                            idx,
                            (
                                v["content_block"]["id"].as_str().unwrap_or_default().into(),
                                v["content_block"]["name"].as_str().unwrap_or_default().into(),
                                String::new(),
                            ),
                        );
                    }
                }
                Some("content_block_delta") => {
                    let delta = &v["delta"];
                    match delta.get("type").and_then(|t| t.as_str()) {
                        Some("text_delta") => {
                            if let Some(t) = delta.get("text").and_then(|x| x.as_str()) {
                                if !t.is_empty() {
                                    let _ = tx2.send(StreamEvent::Delta(t.to_string()));
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                            if let Some(e) = tool_blocks.get_mut(&idx) {
                                if let Some(pj) =
                                    delta.get("partial_json").and_then(|x| x.as_str())
                                {
                                    e.2.push_str(pj);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some("message_delta") => {
                    if v["delta"]["stop_reason"] == "tool_use" {
                        wants_tools = true;
                    }
                }
                _ => {}
            }
            Ok(true)
        })
        .await
        .context("streaming anthropic response")?;

        if wants_tools && !tool_blocks.is_empty() {
            let calls: Vec<ToolCall> = tool_blocks
                .into_values()
                .map(|(id, name, args)| ToolCall { id, name, arguments: args })
                .collect();
            let _ = tx.send(StreamEvent::ToolCalls(calls));
        }
        let _ = tx.send(StreamEvent::Done);
        Ok(())
    }
}

// silence unused import warning for ToolDef used only in signature above
#[allow(unused)]
fn _assert(_: &[ToolDef]) {}
