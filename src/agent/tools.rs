//! Built-in tools the agent can call, plus their execution.

use crate::memory::MemoryStore;
use anyhow::{bail, Context as _, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct ToolCtx {
    pub memory: Arc<Mutex<MemoryStore>>,
    pub allow_commands: bool,
}

fn schema(props: Value, required: &[&str]) -> Value {
    json_obj(&[
        ("type", Value::String("object".into())),
        ("properties", props),
        ("required", serde_json::to_value(required).unwrap_or_default()),
    ])
}

fn json_obj(pairs: &[(&str, Value)]) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v.clone());
    }
    Value::Object(m)
}

pub fn defs() -> Vec<crate::provider::ToolDef> {
    use crate::provider::ToolDef;
    vec![
        ToolDef {
            name: "read_file".into(),
            description: "Read a text file from disk.".into(),
            parameters: schema(
                json_obj(&[(
                    "path",
                    json_obj(&[("type", Value::String("string".into())), ("description", Value::String("File path".into()))]),
                )]),
                &["path"],
            ),
        },
        ToolDef {
            name: "write_file".into(),
            description: "Create or overwrite a file with the given content.".into(),
            parameters: schema(
                json_obj(&[
                    ("path", json_obj(&[("type", Value::String("string".into()))])),
                    ("content", json_obj(&[("type", Value::String("string".into()))])),
                ]),
                &["path", "content"],
            ),
        },
        ToolDef {
            name: "list_files".into(),
            description: "List files under a directory (recursive, depth-limited).".into(),
            parameters: schema(
                json_obj(&[(
                    "path",
                    json_obj(&[("type", Value::String("string".into())), ("description", Value::String("Directory path, defaults to '.'".into()))]),
                )]),
                &[],
            ),
        },
        ToolDef {
            name: "grep".into(),
            description: "Search file contents with a regex. Returns matching lines as path:line: text.".into(),
            parameters: schema(
                json_obj(&[
                    ("pattern", json_obj(&[("type", Value::String("string".into()))])),
                    ("path", json_obj(&[("type", Value::String("string".into())), ("description", Value::String("Directory to search, defaults to '.'".into()))])),
                ]),
                &["pattern"],
            ),
        },
        ToolDef {
            name: "run_shell".into(),
            description: "Run a shell command and return combined stdout/stderr. Requires user permission setting.".into(),
            parameters: schema(
                json_obj(&[("command", json_obj(&[("type", Value::String("string".into()))]))]),
                &["command"],
            ),
        },
        ToolDef {
            name: "save_memory".into(),
            description: "Persist an important fact about the user or project to long-term memory. Use for stable preferences, project decisions, key facts - not transient chatter.".into(),
            parameters: schema(
                json_obj(&[
                    ("content", json_obj(&[("type", Value::String("string".into())), ("description", Value::String("The fact, stated concisely".into()))])),
                    ("tags", json_obj(&[("type", Value::String("array".into())), ("items", json_obj(&[("type", Value::String("string".into()))])))])),
                    ("importance", json_obj(&[("type", Value::String("number".into())), ("description", Value::String("0.0-1.0".into()))])),
                ]),
                &["content"],
            ),
        },
        ToolDef {
            name: "search_memory".into(),
            description: "Search long-term memory for relevant facts.".into(),
            parameters: schema(
                json_obj(&[("query", json_obj(&[("type", Value::String("string".into()))]))]),
                &["query"],
            ),
        },
    ]
}

const MAX_OUTPUT: usize = 6000;

pub async fn execute(name: &str, arguments: &str, ctx: &ToolCtx) -> Result<String> {
    let args: Value =
        serde_json::from_str(arguments).context("tool arguments are not valid JSON")?;
    let out = match name {
        "read_file" => {
            let p = arg_str(&args, "path")?;
            let raw = std::fs::read_to_string(&p)
                .with_context(|| format!("cannot read {p}"))?;
            clip(raw)
        }
        "write_file" => {
            let p = arg_str(&args, "path")?;
            let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
            if let Some(parent) = PathBuf::from(&p).parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&p, content).with_context(|| format!("cannot write {p}"))?;
            format!("wrote {} bytes to {p}", content.len())
        }
        "list_files" => {
            let p = args.get("path").and_then(|x| x.as_str()).unwrap_or(".").to_string();
            let mut lines = Vec::new();
            walk(PathBuf::from(&p), 0, 3, &mut |entry| {
                lines.push(entry);
            })?;
            if lines.is_empty() {
                "(empty)".into()
            } else {
                clip(lines.into_iter().take(400).collect::<Vec<_>>().join("\n"))
            }
        }
        "grep" => {
            let pattern = arg_str(&args, "pattern")?;
            let dir = args.get("path").and_then(|x| x.as_str()).unwrap_or(".").to_string();
            grep_impl(&dir, &pattern)?
        }
        "run_shell" => {
            if !ctx.allow_commands {
                bail!(
                    "shell access is disabled. Enable it with:\n  \
                     dragon settings set allow_commands true"
                );
            }
            let cmd = arg_str(&args, "command")?;
            run_command(&cmd).await?
        }
        "save_memory" => {
            let content = arg_str(&args, "content")?;
            let tags: Vec<String> = args
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let importance =
                args.get("importance").and_then(|i| i.as_f64()).unwrap_or(0.6) as f32;
            let fact = ctx.memory.lock().unwrap().add(&content, &tags, importance);
            ctx.memory.lock().unwrap().save()?;
            format!("saved memory [{}] {}", fact.id, fact.content)
        }
        "search_memory" => {
            let q = arg_str(&args, "query")?;
            let found = ctx.memory.lock().unwrap().recall(&q, 5);
            if found.is_empty() {
                "no matching memories".into()
            } else {
                found
                    .iter()
                    .map(|f| format!("[{}] {}", f.id, f.content))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        other => bail!("unknown tool '{other}'"),
    };
    Ok(clip(out))
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing string argument '{key}'"))
}

fn clip(s: String) -> String {
    if s.len() > MAX_OUTPUT {
        let mut cut: String = s.chars().take(MAX_OUTPUT).collect();
        cut.push_str("\n...[truncated]");
        cut
    } else {
        s
    }
}

fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".hg" | ".svn" | "node_modules" | "target" | "dist" | "build"
            | "__pycache__" | ".venv" | "venv" | ".idea" | ".vscode" | ".next"
    )
}

fn walk(dir: &PathBuf, depth: usize, max_depth: usize, f: &mut impl FnMut(String)) -> Result<()> {
    if depth > max_depth {
        return Ok(());
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let path = e.path();
        if is_dir {
            if !name.starts_with('.') && !skip_dir(&name) {
                walk(&path, depth + 1, max_depth, f)?;
            }
        } else {
            f(path.to_string_lossy().to_string());
        }
    }
    Ok(())
}

fn grep_impl(dir: &str, pattern: &str) -> Result<String> {
    let re = regex::Regex::new(pattern).context("invalid regex pattern")?;
    let mut files = Vec::new();
    walk(PathBuf::from(dir), 0, 6, &mut |f| files.push(f))?;

    let mut hits: Vec<String> = Vec::new();
    'files: for f in files {
        let Ok(meta) = std::fs::metadata(&f) else { continue };
        if meta.len() > 1_500_000 {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&f) else { continue };
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                hits.push(format!("{}:{}: {}", f, i + 1, line.trim()));
                if hits.len() >= 60 || hits.join("\n").len() > MAX_OUTPUT {
                    break 'files;
                }
            }
        }
    }
    if hits.is_empty() {
        Ok("(no matches)".into())
    } else {
        Ok(hits.join("\n"))
    }
}

async fn run_command(cmd: &str) -> Result<String> {
    #[cfg(target_os = "windows")]
    let (program, args) = ("cmd", vec!["/C".to_string(), cmd.to_string()]);
    #[cfg(not(target_os = "windows"))]
    let (program, args) = ("sh", vec!["-c".to_string(), cmd.to_string()]);

    let out = tokio::process::Command::new(program)
        .args(&args)
        .output()
        .await
        .context("failed to spawn shell")?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&out.stdout));
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("[stderr]\n");
        text.push_str(&err);
    }
    if !out.status.success() {
        text.push_str(&format!("\n[exit code: {}]", out.status.code().unwrap_or(-1)));
    }
    if text.trim().is_empty() {
        text = "(no output)".into();
    }
    Ok(text)
}
