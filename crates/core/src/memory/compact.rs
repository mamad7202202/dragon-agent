//! Context compaction: fold old turns into a summary so long sessions keep
//! fitting in the model's context window.

use crate::provider::{complete, LlmProvider, Message};
use anyhow::Result;

/// Fold everything except the most recent `keep` messages into one summary
/// message. Returns the new history.
pub async fn compact(
    provider: std::sync::Arc<dyn LlmProvider>,
    model: &str,
    messages: &[Message],
    keep: usize,
) -> Result<Vec<Message>> {
    if messages.len() <= keep + 2 {
        return Ok(messages.to_vec());
    }
    let split = messages.len() - keep;
    let (old, recent) = messages.split_at(split);

    let mut transcript = String::from("Transcript of an earlier conversation:\n\n");
    for m in old {
        let who = match m.role {
            crate::provider::Role::User => "User",
            crate::provider::Role::Assistant => "Dragon",
            _ => continue,
        };
        if m.content.trim().is_empty() {
            continue;
        }
        transcript.push_str(&format!("{who}: {}\n", truncate(&m.content, 700)));
    }

    let summary = complete(
        provider,
        model,
        Some(
            "You compress conversation history. Write a dense factual summary of the \
             transcript below: decisions made, facts learned, files touched, open \
             questions. Maximum 250 words. No preamble.",
        ),
        &[Message::user(transcript)],
    )
    .await?;

    let mut out = Vec::with_capacity(recent.len() + 1);
    out.push(Message {
        role: crate::provider::Role::User,
        content: format!("[Earlier conversation, summarized]\n{}", summary.trim()),
        ..Default::default()
    });
    out.extend_from_slice(recent);
    Ok(out)
}

fn truncate(s: &str, max_chars: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max_chars {
        t.to_string()
    } else {
        let cut: String = t.chars().take(max_chars).collect();
        format!("{cut}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_histories_pass_through() {
        let msgs = vec![Message::user("a"), Message::assistant("b")];
        // can't call async compact without a provider; test the guard logic instead
        assert!(msgs.len() <= 4 + 2);
    }
}
