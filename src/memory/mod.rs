// context compaction lives in its own module
pub mod compact;

//! Hybrid memory system.
//!
//! Three cooperating layers:
//!
//! 1. Semantic memory  — discrete facts ("memory shards") persisted as JSON,
//!                        recalled per-turn by lexical cosine scoring with
//!                        importance + recency boosts.
//! 2. Procedural memory — a plain MEMORY.md the user (or the agent) maintains;
//!                        always injected into the system prompt.
//! 3. Episodic memory   — full session transcripts (see `crate::session`)
//!                        that can be resumed verbatim.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 0.0 .. 1.0 — how much this matters (agent- or user-assigned).
    #[serde(default = "default_importance")]
    pub importance: f32,
    pub created_at: String,
    #[serde(default)]
    pub hits: u32,
}

fn default_importance() -> f32 {
    0.5
}

pub struct MemoryStore {
    path: PathBuf,
    facts: Vec<Fact>,
}

impl MemoryStore {
    pub fn open() -> Result<Self> {
        let dir = crate::config::Config::data_dir().join("memory");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("facts.json");
        let facts = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(Self { path, facts })
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(&self.facts)?)?;
        Ok(())
    }

    pub fn add(&mut self, content: &str, tags: &[String], importance: f32) -> Fact {
        // de-dup: near-identical content refreshes instead of piling up
        let norm = normalize(content);
        if let Some(existing) = self
            .facts
            .iter_mut()
            .find(|f| normalize(&f.content) == norm)
        {
            existing.importance = existing.importance.max(importance);
            for t in tags {
                if !existing.tags.contains(t) {
                    existing.tags.push(t.clone());
                }
            }
            return existing.clone();
        }
        let fact = Fact {
            id: uuid::Uuid::new_v4().simple().to_string()[..8].to_string(),
            content: content.trim().to_string(),
            tags: tags.to_vec(),
            importance: importance.clamp(0.0, 1.0),
            created_at: chrono::Local::now().to_rfc3339(),
            hits: 0,
        };
        self.facts.push(fact.clone());
        fact
    }

    pub fn remove(&mut self, id_prefix: &str) -> bool {
        let before = self.facts.len();
        self.facts.retain(|f| !f.id.starts_with(id_prefix));
        before != self.facts.len()
    }

    pub fn clear(&mut self) {
        self.facts.clear();
    }

    pub fn all(&self) -> &[Fact] {
        &self.facts
    }

    /// Recall the most relevant facts for a query, blending lexical relevance
    /// with importance and recency. Bumps hit counters on the winners.
    pub fn recall(&mut self, query: &str, k: usize) -> Vec<Fact> {
        let q_tokens = tokenize(query);
        if q_tokens.is_empty() || self.facts.is_empty() {
            return Vec::new();
        }
        let now = chrono::Local::now();

        let mut scored: Vec<(f32, usize)> = Vec::with_capacity(self.facts.len());
        for (i, fact) in self.facts.iter().enumerate() {
            let mut text = format!("{} {}", fact.content, fact.tags.join(" "));
            text.make_ascii_lowercase();
            let f_tokens = tokenize(&text);
            let rel = cosine(&q_tokens, &f_tokens);
            if rel <= 0.0 {
                continue;
            }
            let age_days = chrono::DateTime::parse_from_rfc3339(&fact.created_at)
                .map(|t| (now - t.with_timezone(&chrono::Local)).num_days().max(0) as f32)
                .unwrap_or(0.0);
            let recency = 1.0 / (1.0 + age_days / 14.0); // two-week half-life-ish
            let score = rel * (0.55 + 0.45 * fact.importance) * (0.7 + 0.3 * recency);
            scored.push((score, i));
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut out = Vec::new();
        for (_, i) in scored.into_iter().take(k) {
            let mut fact = self.facts[i].clone();
            fact.hits += 1;
            self.facts[i].hits = fact.hits;
            out.push(fact);
        }
        out
    }

    /// Render recalled facts as a prompt block.
    pub fn recall_block(&mut self, query: &str, k: usize) -> Option<String> {
        let facts = self.recall(query, k);
        if facts.is_empty() {
            return None;
        }
        let mut s = String::from("[REMEMBERED FACTS]\n");
        for f in facts {
            s.push_str(&format!("- {}\n", f.content));
        }
        Some(s)
    }
}

// ---------------------------------------------------------------- procedural

pub fn procedural_path() -> PathBuf {
    crate::config::Config::data_dir().join("memory").join("MEMORY.md")
}

/// The always-on instructions file. Truncated to keep prompts sane.
pub fn procedural_memory() -> Option<String> {
    let raw = std::fs::read_to_string(procedural_path()).ok()?;
    let trimmed: String = raw.chars().take(6000).collect();
    if trimmed.trim().is_empty() {
        return None;
    }
    Some(format!("[PROCEDURAL MEMORY - persistent user instructions]\n{trimmed}"))
}

// ------------------------------------------------------------------ scoring

fn normalize(s: &str) -> String {
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1 && t.len() < 24)
        .map(|t| t.to_string())
        .collect()
}

/// Cosine similarity over term-frequency vectors.
fn cosine(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut ca: HashMap<&str, f32> = HashMap::new();
    for t in a {
        *ca.entry(t.as_str()).or_insert(0.0) += 1.0;
    }
    let mut cb: HashMap<&str, f32> = HashMap::new();
    for t in b {
        *cb.entry(t.as_str()).or_insert(0.0) += 1.0;
    }
    let dot: f32 = ca
        .iter()
        .filter_map(|(t, va)| cb.get(t).map(|vb| va * vb))
        .sum();
    let na = ca.values().map(|v| v * v).sum::<f32>().sqrt();
    let nb = cb.values().map(|v| v * v).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(facts: &[&str]) -> MemoryStore {
        let mut m = MemoryStore {
            path: PathBuf::from("/tmp/does-not-matter.json"),
            facts: Vec::new(),
        };
        for (i, f) in facts.iter().enumerate() {
            m.facts.push(Fact {
                id: format!("id{i}"),
                content: f.to_string(),
                tags: vec![],
                importance: 0.5,
                created_at: chrono::Local::now().to_rfc3339(),
                hits: 0,
            });
        }
        m
    }

    #[test]
    fn recall_prefers_relevant_fact() {
        let mut m = store_with(&[
            "User prefers dark mode everywhere",
            "Project Dragon uses Rust and tokio",
            "User's sister is called Sara",
        ]);
        let top = m.recall("what language is the dragon project written in", 2);
        assert!(!top.is_empty());
        assert!(top[0].content.contains("Rust"));
    }

    #[test]
    fn add_dedupes_identical_content() {
        let mut m = store_with(&[]);
        m.add("likes tea", &[], 0.4);
        m.add("likes tea", &[], 0.9);
        assert_eq!(m.all().len(), 1);
        assert!((m.all()[0].importance - 0.9).abs() < 1e-6);
    }

    #[test]
    fn remove_by_prefix() {
        let mut m = store_with(&[]);
        let f = m.add("temp note", &[], 0.1);
        assert!(m.remove(&f.id[..3]));
        assert!(m.all().is_empty());
    }
}
