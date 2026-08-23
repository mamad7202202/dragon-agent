//! Built-in tools the agent can call, plus execution and access tiers.

use crate::memory::MemoryStore;
use anyhow::{bail, Context as _, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct ToolCtx {
    pub memory: Arc<Mutex<MemoryStore>>,
    /// Master switch for the shell tool (legacy setting).
    pub allow_commands: bool,
    /// Current conversation id - used to scope save_memory.
    pub session_id: Option<String>,
    /// Present when the memory-graph engine is selected.
    pub graph: Option<Arc<Mutex<crate::memory::graph::GraphStore>>>,
}

/// Which approval tier a tool belongs to.
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Tier {
    /// Read-only, never needs permission.
    ReadOnly,
    /// Modifies the world - needs user approval unless auto-approved.
    Gated,
    /// Only available in agent mode.
    AgentOnly,
}

pub fn tier_of(name: &str) -> Tier {
    match name {
        "read_file" | "list_files" | "grep" | "search_memory" | "fetch_url" | "web_search" | "task_write" | "graph_set_section" | "graph_read" => Tier::ReadOnly,
        "save_memory" => Tier::ReadOnly,
        "write_file" | "edit_file" | "delete_file" | "run_shell" => Tier::Gated,
        _ => Tier::AgentOnly,
    }
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

fn prop(ty: &str, desc: &str) -> Value {
    json_obj(&[
        ("type", Value::String(ty.into())),
        ("description", Value::String(desc.into())),
    ])
}

/// Tool list for the current mode. `allow_shell` reflects the settings gate.
pub fn defs(mode: &crate::agent::Mode, allow_shell: bool, graph_engine: bool) -> Vec<crate::provider::ToolDef> {
    use crate::provider::ToolDef;
    let mut v = vec![
        ToolDef {
            name: "read_file".into(),
            description: "Read a text file from disk.".into(),
            parameters: schema(
                json_obj(&[
                    ("path", prop("string", "File path")),
                    ("start_line", prop("number", "optional 1-based first line")),
                    ("max_lines", prop("number", "optional cap on lines returned")),
                ]),
                &["path"],
            ),
        },
        ToolDef {
            name: "write_file".into(),
            description: "Create or overwrite a file with the given content.".into(),
            parameters: schema(
                json_obj(&[("path", prop("string", "File path")), ("content", prop("string", "Full file content"))]),
                &["path", "content"],
            ),
        },
        ToolDef {
            name: "edit_file".into(),
            description: "Replace an exact snippet inside a file. Prefer this over write_file for small changes.".into(),
            parameters: schema(
                json_obj(&[
                    ("path", prop("string", "File path")),
                    ("old", prop("string", "Exact text to find")),
                    ("new", prop("string", "Replacement text")),
                ]),
                &["path", "old", "new"],
            ),
        },
        ToolDef {
            name: "delete_file".into(),
            description: "Delete a file. Irreversible.".into(),
            parameters: schema(json_obj(&[("path", prop("string", "File path"))]), &["path"]),
        },
        ToolDef {
            name: "list_files".into(),
            description: "List files under a directory (recursive, depth-limited).".into(),
            parameters: schema(json_obj(&[("path", prop("string", "Directory path, defaults to '.'"))]), &[]),
        },
        ToolDef {
            name: "grep".into(),
            description: "Search file contents with a regex. Returns matching lines as path:line: text.".into(),
            parameters: schema(
                json_obj(&[("pattern", prop("string", "Regex")), ("path", prop("string", "Directory to search, defaults to '.'"))]),
                &["pattern"],
            ),
        },
        ToolDef {
            name: "fetch_url".into(),
            description: "Fetch a web page or API endpoint (HTTP GET) and return its body as text.".into(),
            parameters: schema(json_obj(&[("url", prop("string", "Absolute http(s) URL"))]), &["url"]),
        },
        ToolDef {
            name: "run_shell".into(),
            description: "Run a shell command and return combined stdout/stderr. Requires user approval unless pre-allowed.".into(),
            parameters: schema(json_obj(&[("command", prop("string", "The command line to run"))]), &["command"]),
        },
        ToolDef {
            name: "web_search".into(),
            description: "Search the web and return top results (title + url + snippet).".into(),
            parameters: schema(
                json_obj(&[("query", prop("string", "Search query"))]),
                &["query"],
            ),
        },
        ToolDef {
            name: "task_write".into(),
            description: "Replace your visible task board with this list. Call whenever the plan changes or a task finishes. Keep 3-9 short tasks; status is pending or done.".into(),
            parameters: {
                let item = json_obj(&[
                    ("type", Value::String("object".into())),
                    ("properties", json_obj(&[
                        ("text", prop("string", "short task text")),
                        ("status", json_obj(&[
                            ("type", Value::String("string".into())),
                            ("enum", serde_json::json!(["pending", "done"])),
                        ])),
                    ])),
                    ("required", serde_json::json!(["text"])),
                ]);
                let mut arr = serde_json::Map::new();
                arr.insert("type".into(), Value::String("array".into()));
                arr.insert("items".into(), item);
                schema(
                    json_obj(&[("tasks", Value::Object(arr))]),
                    &["tasks"],
                )
            },
        },        ToolDef {
            name: "graph_set_section".into(),
            description: "Write one section of the memory graph (info-graph). Bullets must be terse facts. Empty bullets delete the section.".into(),
            parameters: schema(
                json_obj(&[
                    ("scope", json_obj(&[("type", Value::String("string".into())), ("enum", serde_json::json!(["session","global"]))])),
                    ("id", prop("string", "short slug e.g. proj/stack/decisions")),
                    ("title", prop("string", "human title")),
                    ("bullets", json_obj(&[("type", Value::String("array".into())), ("items", prop("string", "terse bullet"))])),
                ]),
                &["scope","id","title","bullets"],
            ),
        },
        ToolDef {
            name: "graph_read".into(),
            description: "Read the current memory graph as compact text.".into(),
            parameters: schema(json_obj(&[]), &[]),
        },        ToolDef {
            name: "save_memory".into(),
            description: "Persist a fact to memory. Session facts describe THIS project/task; global facts are durable user preferences. Use scope=session by default; scope=global only for stable cross-project knowledge.".into(),
            parameters: schema(
                json_obj(&[
                    ("content", prop("string", "The fact, stated concisely")),
                    ("scope", json_obj(&[("type", Value::String("string".into())), ("enum", serde_json::json!(["session","global"])), ("description", prop("", ""))])),
                    ("importance", prop("number", "0.0-1.0")),
                ]),
                &["content"],
            ),
        },
        ToolDef {
            name: "search_memory".into(),
            description: "Search long-term memory (both scopes) for relevant facts.".into(),
            parameters: schema(json_obj(&[("query", prop("string", "What to look for"))]), &["query"]),
        },
    ];
    if *mode == crate::agent::Mode::Plan {
        // plan mode: research only
        v.retain(|t| tier_of(&t.name) == Tier::ReadOnly);
    }
    if !allow_shell {
        v.retain(|t| t.name != "run_shell");
    }
    if !graph_engine {
        v.retain(|t| !t.name.starts_with("graph_"));
    }
    v
}

const MAX_OUTPUT: usize = 6000;

/// Execute a tool. Gated tools must be approved by the caller beforehand.
pub async fn execute(name: &str, arguments: &str, ctx: &ToolCtx) -> Result<String> {
    let args: Value =
        serde_json::from_str(arguments).context("tool arguments are not valid JSON")?;


    let out = match name {
        "read_file" => {
            let p = arg_str(&args, "path")?;
            let raw = std::fs::read_to_string(&p).with_context(|| format!("cannot read {p}"))?;
            let start = args.get("start_line").and_then(|x| x.as_u64()).unwrap_or(1).max(1) as usize;
            let max = args.get("max_lines").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            if start > 1 || max > 0 {
                let take = if max == 0 { usize::MAX } else { max };
                let sel: Vec<&str> =
                    raw.lines().skip(start - 1).take(take).collect();
                clip(format!("[lines {start}..]\n{}", sel.join("\n")))
            } else {
                clip(raw)
            }
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
        "edit_file" => {
            let p = arg_str(&args, "path")?;
            let old = arg_str(&args, "old")?;
            let new = arg_str(&args, "new")?;
            let raw = std::fs::read_to_string(&p).with_context(|| format!("cannot read {p}"))?;
            let count = raw.matches(old).count();
            if count == 0 {
                bail!("snippet not found in {p} - read the file first and copy exactly");
            }
            if count > 1 {
                bail!("snippet appears {count} times in {p}; include more surrounding lines to make it unique");
            }
            std::fs::write(&p, raw.replacen(old, new, 1))?;
            format!("edited {p} (1 replacement)")
        }
        "delete_file" => {
            let p = arg_str(&args, "path")?;
            std::fs::remove_file(&p).with_context(|| format!("cannot delete {p}"))?;
            format!("deleted {p}")
        }
        "list_files" => {
            let p = args.get("path").and_then(|x| x.as_str()).unwrap_or(".").to_string();
            let mut lines = Vec::new();
            walk(&PathBuf::from(&p), 0, 3, &mut |entry| {
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
        "fetch_url" => {
            let url = arg_str(&args, "url")?;
            fetch_impl(url).await?
        }
        "web_search" => {
            let q = arg_str(&args, "query")?;
            web_search_impl(q).await?
        }
        "task_write" => {
            let tasks = args.get("tasks").and_then(|t| t.as_array()).cloned().unwrap_or_default();
            let mut lines = Vec::new();
            for t in &tasks {
                let text = t.get("text").and_then(|x| x.as_str()).unwrap_or("?").to_string();
                let status = t.get("status").and_then(|x| x.as_str()).unwrap_or("pending").to_string();
                lines.push(format!("[{status}] {text}"));
            }
            if let Some(sid) = &ctx.session_id {
                let dir = crate::config::Config::data_dir().join("tasks");
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(
                    dir.join(format!("{sid}.json")),
                    serde_json::to_string_pretty(&args.get("tasks")).unwrap_or_default(),
                );
            }
            if lines.is_empty() { "(empty board)".into() } else { lines.join("\n") }
        }
        "graph_set_section" => {
            let Some(g) = &ctx.graph else { bail!("memory graph engine is not enabled") };
            let scope = arg_str(&args, "scope")?;
            let id = arg_str(&args, "id")?;
            let title = arg_str(&args, "title")?;
            let bullets: Vec<String> = args
                .get("bullets")
                .and_then(|b| b.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let sid = if scope == "global" { None } else { ctx.session_id.clone() };
            g.lock().unwrap().set_section(sid.as_deref(), id, title, bullets)?;
            format!("section '{id}' written ({scope})")
        }
        "graph_read" => {
            let Some(g) = &ctx.graph else { bail!("memory graph engine is not enabled") };
            g.lock().unwrap().read_text(ctx.session_id.as_deref())
        }
        "run_shell" => {
            if !ctx.allow_commands {
                bail!("shell access is disabled in settings.");
            }
            let cmd = arg_str(&args, "command")?;
            run_command(&cmd).await?
        }
        "save_memory" => {
            let content = arg_str(&args, "content")?;
            let tags_src = args.get("tags").and_then(|t| t.as_array()).cloned();
            let scope = args.get("scope").and_then(|s| s.as_str()).unwrap_or("session");
            let importance = args.get("importance").and_then(|i| i.as_f64()).unwrap_or(0.6) as f32;
            let mut tags: Vec<String> = tags_src
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            tags.push(if scope == "global" { "global".into() } else { "session".into() });
            let sid = if scope == "global" { None } else { ctx.session_id.clone() };
            let fact = ctx.memory.lock().unwrap().add_scoped(&content, &tags, importance, sid.as_deref());
            ctx.memory.lock().unwrap().save()?;
            format!(
                "saved {} memory [{}] {}",
                if sid.is_some() { "session" } else { "global" },
                fact.id,
                fact.content
            )
        }
        "search_memory" => {
            let q = arg_str(&args, "query")?;
            let found = ctx
                .memory
                .lock()
                .unwrap()
                .recall_mixed(&q, 5, ctx.session_id.as_deref());
            if found.is_empty() {
                "no matching memories".into()
            } else {
                found
                    .iter()
                    .map(|f| {
                        format!("[{}]{} {}", f.id, if f.session.is_some() { "(s)" } else { "(g)" }, f.content)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        other => bail!("unknown tool '{other}'"),
    };
    Ok(clip(out))
}

/// Short human-readable summary used on approval cards.
fn summarize_args(name: &str, args: &Value) -> String {
    match name {
        "write_file" | "read_file" | "delete_file" | "edit_file" => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string(),
        "run_shell" => args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .chars()
            .take(120)
            .collect(),
        _ => serde_json::to_string(args).unwrap_or_default(),
    }
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
    walk(&PathBuf::from(dir), 0, 6, &mut |f| files.push(f))?;

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

async fn fetch_impl(url: &str) -> Result<String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("only http(s) URLs are supported");
    }
    let client = reqwest::Client::builder()
        .user_agent(concat!("dragon-agent/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client.get(url).send().await.context("request failed")?;
    let status = resp.status();
    let headers = format!(
        "status: {}\ncontent-type: {}\n\n",
        status,
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("?")
    );
    let body = resp.text().await.unwrap_or_default();
    Ok(clip(format!("{headers}{}", body.chars().take(MAX_OUTPUT).collect::<String>())))
}

/// Keyless web search via DuckDuckGo's HTML endpoint.
async fn web_search_impl(query: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("Mozilla/5.0 (compatible; dragon-agent/", env!("CARGO_PKG_VERSION"), ")"))
        .timeout(std::time::Duration::from_secs(12))
        .build()?;
    let html = client
        .post("https://html.duckduckgo.com/html/")
        .form(&[("q", query)])
        .send()
        .await
        .context("search request failed")?
        .text()
        .await?;

    let re_link = regex::Regex::new(r#"<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap();
    let re_snip = regex::Regex::new(r#"<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#).unwrap();
    let re_tag = regex::Regex::new(r"<[^>]+>").unwrap();

    let mut links: Vec<(String, String)> = Vec::new();
    for cap in re_link.captures_iter(&html) {
        let url = decode_html(&cap[1]);
        let title = re_tag.replace_all(&decode_html(&cap[2]), "").to_string();
        links.push((title.trim().to_string(), url));
    }
    let snippets: Vec<String> = re_snip
        .captures_iter(&html)
        .map(|c| re_tag.replace_all(&decode_html(&c[1]), "").to_string())
        .collect();

    if links.is_empty() {
        return Ok("(no results)".into());
    }
    let n = links.len().min(6);
    let mut out = String::new();
    for i in 0..n {
        let (t, u) = &links[i];
        out.push_str(&format!("{}) {}\n   {}\n", i + 1, t, u));
        if let Some(s) = snippets.get(i) {
            let s: String = s.chars().take(200).collect();
            out.push_str(&format!("   {s}\n"));
        }
        out.push('\n');
    }
    Ok(out.trim_end().to_string())
}

fn decode_html(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#x2F;", "/")
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