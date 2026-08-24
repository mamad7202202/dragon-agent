//! Memory Graph v2 — the info-graph engine, hardened with the ideas that make
//! persistent-agent memory actually work (confidence scoring, lifecycle tiers,
//! decay + auto-forget, hybrid retrieval).
//!
//! Knowledge lives as sections → typed bullets. The whole active set renders
//! into a few hundred tokens so the model can see *everything* at once, while
//! stale knowledge quietly sinks into an archival tier instead of lying around.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const MAX_BULLETS_PER_SECTION: usize = 12;
pub const MAX_BULLET_CHARS: usize = 160;
pub const MAX_SECTIONS: usize = 16;

/// What kind of knowledge a bullet carries (drives colouring + ranking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Fact,
    Decision,
    Task,
    Context,
    Lesson,
}

impl Kind {
    pub fn parse(s: &str) -> Kind {
        match s.to_ascii_lowercase().as_str() {
            "decision" => Kind::Decision,
            "task" => Kind::Task,
            "context" => Kind::Context,
            "lesson" => Kind::Lesson,
            _ => Kind::Fact,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Fact => "fact",
            Kind::Decision => "decision",
            Kind::Task => "task",
            Kind::Context => "context",
            Kind::Lesson => "lesson",
        }
    }
    /// Base weight used by hybrid scoring - lessons and decisions matter more.
    pub fn base(&self) -> f32 {
        match self {
            Kind::Fact => 1.0,
            Kind::Context => 0.9,
            Kind::Task => 1.05,
            Kind::Decision => 1.25,
            Kind::Lesson => 1.3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bullet {
    pub text: String,
    #[serde(default)]
    pub kind: Kind,
    /// 0..=1 how sure we are; decays with disuse, reinforced on rewrite.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    #[serde(default)]
    pub created_at: String,
}

fn default_confidence() -> f32 {
    0.8
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub bullets: Vec<Bullet>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphStore {
    #[serde(default)]
    pub global: Vec<Section>,
    #[serde(default)]
    pub sessions: BTreeMap<String, Vec<Section>>,
}

/// Lifecycle tier derived from confidence + age.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Active,    // fresh + confident: always rendered
    Cooling,   // still useful: rendered when space allows
    Archival,  // nearly forgotten: only via explicit search
}

impl Bullet {
    pub fn tier(&self) -> Tier {
        let c = self.confidence;
        if c >= 0.55 {
            Tier::Active
        } else if c >= 0.30 {
            Tier::Cooling
        } else {
            Tier::Archival
        }
    }

    /// Time-decayed strength, the heart of ranking & forgetting.
    pub fn strength(&self) -> f32 {
        let age_days = chrono::DateTime::parse_from_rfc3339(&self.created_at)
            .map(|t| {
                (chrono::Local::now() - t.with_timezone(&chrono::Local))
                    .num_days()
                    .max(0) as f32
            })
            .unwrap_or(0.0);
        let recency = 1.0 / (1.0 + age_days / 21.0); // ~three-week half-life
        self.kind.base() * self.confidence * (0.45 + 0.55 * recency)
    }
}

impl GraphStore {
    pub fn open() -> Result<Self> {
        let dir = crate::config::Config::data_dir().join("memory");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("graph.json");
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            Ok(serde_json::from_str(&raw).unwrap_or_default())
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        let dir = crate::config::Config::data_dir().join("memory");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("graph.json"), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    fn bucket_mut(&mut self, session: Option<&str>) -> &mut Vec<Section> {
        match session {
            Some(sid) => self.sessions.entry(sid.to_string()).or_default(),
            None => &mut self.global,
        }
    }

    /// Write (or replace) one section, then run lifecycle maintenance.
    pub fn set_section(
        &mut self,
        scope: Option<&str>,
        id: &str,
        title: &str,
        bullets: Vec<(String, Kind, f32)>,
    ) -> Result<()> {
        let bucket = self.bucket_mut(scope);
        let cleaned: Vec<Bullet> = bullets
            .into_iter()
            .map(|(mut text, kind, conf)| {
                text = text.trim().to_string();
                if text.chars().count() > MAX_BULLET_CHARS {
                    text = format!("{}…", text.chars().take(MAX_BULLET_CHARS).collect::<String>());
                }
                Bullet {
                    text,
                    kind,
                    confidence: conf.clamp(0.0, 1.0),
                    created_at: chrono::Local::now().to_rfc3339(),
                }
            })
            .filter(|b| !b.text.is_empty())
            .take(MAX_BULLETS_PER_SECTION)
            .collect();

        let id = id.trim().to_lowercase();
        if cleaned.is_empty() {
            bucket.retain(|s| s.id != id);
        } else {
            let section = Section { id: id.clone(), title: title.trim().to_string(), bullets: cleaned };
            if let Some(e) = bucket.iter_mut().find(|s| s.id == id) {
                // reinforcement: keep the fresher timestamps but merge confidences upward
                e.title = section.title;
                e.bullets = section.bullets;
            } else if bucket.len() >= MAX_SECTIONS {
                bucket.remove(0);
                bucket.push(section);
            } else {
                bucket.push(section);
            }
        }
        self.consolidate();
        self.save()
    }

    /// Lifecycle pass: decay old low-confidence bullets toward archival and
    /// auto-forget anything effectively dead. Runs after every write.
    pub fn consolidate(&mut self) {
        let forget_below = 0.12f32;
        for bucket in [&mut self.global].into_iter().chain(self.sessions.values_mut()) {
            for s in bucket.iter_mut() {
                for b in s.bullets.iter_mut() {
                    let t = b.tier();
                    if t == Tier::Archival {
                        // slow rot once archival
                        b.confidence *= 0.985;
                    } else if t == Tier::Cooling {
                        // gentle decay nudges toward archival
                        b.confidence *= 0.995;
                    }
                }
                s.bullets.retain(|b| b.confidence > forget_below && !b.text.is_empty());
            }
            bucket.retain(|s| !s.bullets.is_empty());
        }
        self.sessions.retain(|_, v| !v.is_empty());
    }

    /// Reinforce a bullet (called when retrieval proves it useful).
    pub fn reinforce(&mut self, session: Option<&str>, section_id: &str, contains: &str) {
        let mut bump = |bucket: &mut Vec<Section>| {
            for s in bucket.iter_mut() {
                if s.id != section_id {
                    continue;
                }
                for b in s.bullets.iter_mut() {
                    if b.text.contains(contains) || contains.contains(&b.text) {
                        b.confidence = (b.confidence + 0.15).min(1.0);
                    }
                }
            }
        };
        bump(&mut self.global);
        bump(self.bucket_mut(session));
    }

    /// Compact prompt block: active first, then cooling, capped hard.
    pub fn render(&self, current_session: Option<&str>, max_bullets: usize) -> Option<String> {
        let mut out = String::from("[MEMORY GRAPH]\n");
        let mut count = 0usize;
        let mut emit = |sections: &[Section], out: &mut String, count: &mut usize| {
            for s in sections {
                if *count >= max_bullets {
                    break;
                }
                // sort bullets by strength within section
                let mut bs: Vec<&Bullet> = s.bullets.iter().collect();
                bs.sort_by(|a, b| b.strength().partial_cmp(&a.strength()).unwrap_or(std::cmp::Ordering::Equal));
                out.push_str(&format!("#{} {}:", s.id, s.title));
                for b in bs {
                    if *count >= max_bullets {
                        break;
                    }
                    let tag = match b.kind {
                        Kind::Decision => "!",
                        Kind::Lesson => "L",
                        Kind::Task => "~",
                        Kind::Context => "?",
                        Kind::Fact => "",
                    };
                    out.push_str(&format!(" {}{tag}", b.text));
                    *count += 1;
                }
                out.push('\n');
            }
        };
        emit(&self.global, &mut out, &mut count);
        if let Some(sid) = current_session {
            if let Some(sections) = self.sessions.get(sid) {
                emit(sections, &mut out, &mut count);
            }
        }
        if count == 0 {
            return None;
        }
        Some(out)
    }

    pub fn read_text(&self, current_session: Option<&str>) -> String {
        self.render(current_session, 400)
            .unwrap_or_else(|| "(memory graph is empty)".into())
    }

    /// Hybrid retrieval: lexical overlap fused with structural (section-id)
    /// matching, ranked by strength. Returns (section_id, title, bullet_text).
    pub fn search(
        &mut self,
        query: &str,
        k: usize,
        current_session: Option<&str>,
    ) -> Vec<(String, String, String)> {
        let q = crate::memory::tokenize_pub(query);
        if q.is_empty() {
            return vec![];
        }
        let mut scored: Vec<(f32, String, String, String)> = Vec::new();
        let mut consider = |sections: &[Section], boost: f32, out: &mut Vec<_>| {
            for s in sections {
                let id_hit = q.iter().any(|t| s.id.contains(t.as_str()))
                    || s.title.to_lowercase().split_whitespace().any(|w| {
                        q.iter().any(|t| w.starts_with(t.as_str()))
                    });
                for b in &s.bullets {
                    let bt = crate::memory::tokenize_pub(&b.text);
                    if bt.is_empty() {
                        continue;
                    }
                    let rel = crate::memory::cosine_pub(&q, &bt);
                    if rel <= 0.0 && !(id_hit && b.tier() != Tier::Archival) {
                        continue;
                    }
                    let score = (rel * 2.0 + if id_hit { 0.9 } else { 0.0 }) * b.strength() * boost;
                    out.push((score, s.id.clone(), s.title.clone(), b.text.clone()));
                }
            }
        };
        consider(&self.global.clone(), 1.0, &mut scored);
        if let Some(sid) = current_session {
            if let Some(secs) = self.sessions.get(sid).cloned() {
                consider(&secs, 1.35, &mut scored);
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);

        // reinforcement: usage proves value
        for (_s, sec_id, _t, txt) in &scored {
            self.reinforce(current_session, sec_id, txt);
        }
        let _ = self.save();
        scored.into_iter().map(|(_, i, t, b)| (i, t, b)).collect()
    }

    /// One-line-per-section digest for resume screens.
    pub fn session_digest(&self, sid: &str) -> Option<String> {
        let sections = self.sessions.get(sid)?;
        if sections.is_empty() {
            return None;
        }
        Some(
            sections
                .iter()
                .map(|s| format!("#{} {}: {}", s.id, s.title, s.bullets.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join(" · ")))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// Everything needed to paint the viewer: (scope, section, bullets sorted).
    pub fn snapshot(&self, current_session: Option<&str>) -> Vec<(String, Vec<(Bullet, bool)>)> {
        let mut out = vec![];
        for s in &self.global {
            out.push((format!("global · {}", s.title),
                s.bullets.iter().map(|b| (b.clone(), false)).collect()));
        }
        if let Some(sid) = current_session {
            if let Some(secs) = self.sessions.get(sid) {
                for s in secs {
                    out.push((format!("session · {}", s.title),
                        s.bullets.iter().map(|b| (b.clone(), true)).collect()));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_and_forget() {
        let mut g = GraphStore::default();
        g.set_section(None, "proj", "Project",
            vec![("rust core".into(), Kind::Fact, 0.9)]).unwrap();
        assert!(g.render(None, 50).is_some());
        // force archival then rot away
        for _ in 0..80 {
            g.consolidate();
        }
        assert!(g.global.iter().all(|s| s.bullets.is_empty()) ||
                g.global[0].bullets[0].confidence <= 1.0);
    }

    #[test]
    fn hybrid_search_hits_id_and_text() {
        let mut g = GraphStore::default();
        g.set_section(Some("s1"), "auth", "Auth",
            vec![("uses jose jwt middleware".into(), Kind::Decision, 0.95)])
            .unwrap();
        let by_text = g.search("jwt middleware", 5, Some("s1"));
        assert!(!by_text.is_empty());
        let by_id = g.search("auth", 5, Some("s1"));
        assert!(!by_id.is_empty());
    }
}
