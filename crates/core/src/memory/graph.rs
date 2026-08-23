//! Memory Graph — the "info-graph" memory engine.
//!
//! Knowledge lives as a tiny hierarchical outline: sections → terse bullets.
//! The whole thing renders into a few hundred tokens, so the model can keep
//! the *entire* graph in view instead of fuzzy-recalling fragments. The model
//! maintains it itself through the `graph_set_section` tool.
//!
//! Layout on disk (data/memory/graph.json):
//! { "global":   [section…], "sessions": { "<id>": [section…] } }

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    /// short stable slug, e.g. "proj", "stack", "decisions"
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub bullets: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphStore {
    #[serde(default)]
    pub global: Vec<Section>,
    #[serde(default)]
    pub sessions: BTreeMap<String, Vec<Section>>,
}

const MAX_BULLETS_PER_SECTION: usize = 12;
const MAX_BULLET_CHARS: usize = 160;
const MAX_SECTIONS: usize = 16;

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
        let path = dir.join("graph.json");
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    fn bucket_mut(&mut self, session: Option<&str>) -> &mut Vec<Section> {
        match session {
            Some(sid) => self.sessions.entry(sid.to_string()).or_default(),
            None => &mut self.global,
        }
    }

    /// Insert or replace a section. Keeps bullets terse and bounded.
    pub fn set_section(
        &mut self,
        scope: Option<&str>, // None=global, Some(sid)=session
        id: &str,
        title: &str,
        mut bullets: Vec<String>,
    ) -> Result<Section> {
        let bucket = self.bucket_mut(scope);
        bullets = bullets
            .into_iter()
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
            .map(|b| {
                if b.chars().count() > MAX_BULLET_CHARS {
                    format!("{}…", b.chars().take(MAX_BULLET_CHARS).collect::<String>())
                } else {
                    b
                }
            })
            .take(MAX_BULLETS_PER_SECTION)
            .collect();

        if bullets.is_empty() {
            // empty update == delete section
            bucket.retain(|s| s.id != id);
            return Ok(Section { id: id.into(), title: title.into(), bullets });
        }

        let section = Section { id: id.trim().to_lowercase(), title: title.trim().to_string(), bullets };
        if let Some(existing) = bucket.iter_mut().find(|s| s.id == section.id) {
            *existing = section.clone();
        } else {
            if bucket.len() >= MAX_SECTIONS {
                bucket.remove(0); // FIFO when overflowing
            }
            bucket.push(section.clone());
        }
        self.save()?;
        Ok(section)
    }

    /// Render the compact info-graph block for a prompt.
    /// Global first, then the active session's own sections.
    pub fn render(&self, current_session: Option<&str>, max_bullets: usize) -> Option<String> {
        let mut out = String::from("[MEMORY GRAPH - maintained by dragon; keep it accurate]\n");
        let mut count = 0usize;
        let mut emit = |title: &str, id: &str, bullets: &[String], out: &mut String, count: &mut usize| {
            out.push_str(&format!("#{id} {title}: "));
            out.push_str(&bullets.join(" · "));
            out.push('\n');
            *count += bullets.len();
        };

        for s in &self.global {
            if count >= max_bullets { break; }
            let take = s.bullets.len().min(max_bullets - count);
            emit(&s.title, &s.id, &s.bullets[..take], &mut out, &mut count);
        }
        if let Some(sid) = current_session {
            if let Some(sections) = self.sessions.get(sid) {
                for s in sections {
                    if count >= max_bullets { break; }
                    let take = s.bullets.len().min(max_bullets - count);
                    emit(&s.title, &s.id, &s.bullets[..take], &mut out, &mut count);
                }
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

    /// Session summary used at resume time - one line per section.
    pub fn session_digest(&self, sid: &str) -> Option<String> {
        let sections = self.sessions.get(sid)?;
        if sections.is_empty() {
            return None;
        }
        Some(
            sections
                .iter()
                .map(|s| format!("#{} {}: {}", s.id, s.title, s.bullets.join(" · ")))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> GraphStore {
        GraphStore::default()
    }

    #[test]
    fn set_and_render() {
        let mut g = store();
        g.set_section(None, "user", "User", vec!["prefers rust".into(), "dark mode".into()])
            .unwrap();
        let r = g.render(Some("s1"), 100).unwrap();
        assert!(r.contains("#user User: prefers rust · dark mode"));
    }

    #[test]
    fn empty_bullets_delete_section() {
        let mut g = store();
        g.set_section(Some("s1"), "tmp", "Tmp", vec!["x".into()]).unwrap();
        g.set_section(Some("s1"), "tmp", "Tmp", vec![]).unwrap();
        assert!(g.sessions["s1"].is_empty());
    }

    #[test]
    fn bullets_capped_and_trimmed() {
        let mut g = store();
        let many: Vec<String> = (0..30).map(|i| format!("bullet {i}")).collect();
        let s = g.set_section(None, "big", "Big", many).unwrap();
        assert_eq!(s.bullets.len(), MAX_BULLETS_PER_SECTION);
    }
}
