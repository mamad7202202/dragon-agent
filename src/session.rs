//! Session persistence: one JSONL file per conversation under data/sessions/.

use crate::provider::{Message, Role};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub model: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Entry {
    Meta(SessionMeta),
    Msg(Message),
}

pub struct SessionLog {
    path: PathBuf,
    meta: SessionMeta,
}

impl SessionLog {
    pub fn sessions_dir() -> PathBuf {
        crate::config::Config::data_dir().join("sessions")
    }

    /// Create a fresh session file and write its meta line.
    pub fn create(model: &str) -> Result<Self> {
        let dir = Self::sessions_dir();
        std::fs::create_dir_all(&dir)?;
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let path = dir.join(format!("{id}.jsonl"));
        let mut file = std::fs::File::create(&path)?;
        let meta = SessionMeta {
            id,
            title: "new session".into(),
            model: model.into(),
            created_at: chrono::Local::now().to_rfc3339(),
        };
        writeln!(file, "{}", serde_json::to_string(&Entry::Meta(meta.clone()))?)?;
        Ok(Self { path, meta })
    }

    /// Open an existing session for appending; returns messages so far.
    pub fn resume(path: &Path) -> Result<(Self, Vec<Message>)> {
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut meta: Option<SessionMeta> = None;
        let mut msgs = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Entry>(&line) {
                Ok(Entry::Meta(m)) => meta = Some(m),
                Ok(Entry::Msg(m)) => msgs.push(m),
                Err(_) => {} // tolerate torn writes
            }
        }
        let meta = meta.ok_or_else(|| anyhow::anyhow!("session file has no meta line"))?;
        let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
        file.flush()?;
        Ok((Self { path: path.to_path_buf(), meta }, msgs))
    }

    pub fn append_message(&mut self, msg: &Message) -> Result<()> {
        let mut file = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&Entry::Msg(msg.clone()))?)?;
        Ok(())
    }

    pub fn set_title_if_new(&mut self, first_user_text: &str) {
        let t: String = first_user_text
            .split_whitespace()
            .take(10)
            .collect::<Vec<_>>()
            .join(" ");
        if t.len() < 3 {
            return;
        }
        let title: String = if t.chars().count() > 60 {
            format!("{}...", t.chars().take(57).collect::<String>())
        } else {
            t
        };
        self.meta.title = title;
        self.rewrite_meta();
    }

    /// Update the meta line (first line of the file).
    fn rewrite_meta(&mut self) {
        let entries: Vec<String> = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw.lines().skip(1).map(|s| s.to_string()).collect(),
            Err(_) => return,
        };
        let mut out = match serde_json::to_string(&Entry::Meta(self.meta.clone())) {
            Ok(s) => s,
            Err(_) => return,
        };
        out.push('\n');
        for e in entries {
            out.push_str(&e);
            out.push('\n');
        }
        let _ = std::fs::write(&self.path, out);
    }

    pub fn meta(&self) -> &SessionMeta {
        &self.meta
    }
}

/// List sessions, newest first.
pub fn list_sessions() -> Vec<(PathBuf, SessionMeta)> {
    let dir = crate::config::Config::data_dir().join("sessions");
    let mut out: Vec<(PathBuf, SessionMeta)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(Some(meta)) = read_meta(&p) {
                out.push((p, meta));
            }
        }
    }
    out.sort_by(|a, b| b.1.created_at.cmp(&a.1.created_at));
    out
}

fn read_meta(path: &Path) -> Result<Option<SessionMeta>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if let Ok(Entry::Meta(m)) = serde_json::from_str(line.trim()) {
        return Ok(Some(m));
    }
    Ok(None)
}

/// Rebuild message history from a session file (for --resume).
pub fn load_messages(path: &Path) -> Result<Vec<Message>> {
    let (_, msgs) = SessionLog::resume(path)?;
    Ok(msgs
        .into_iter()
        .filter(|m| m.role == Role::User || m.role == Role::Assistant)
        .collect())
}
