//! Built-in provider presets so users never have to memorize base URLs.

pub struct Preset {
    pub name: &'static str,
    pub label: &'static str,
    pub base_url: &'static str,
    pub kind: &'static str,
    pub models: &'static [&'static str],
    pub key_required: bool,
    /// Where to get a key / extra hint shown during setup.
    pub note: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "google",
        label: "Google AI Studio (Gemini)",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        kind: "openai",
        models: &["gemini-2.5-flash", "gemini-2.5-pro"],
        key_required: true,
        note: "free key at https://aistudio.google.com/apikey",
    },
    Preset {
        name: "openrouter",
        label: "OpenRouter (400+ models, one key)",
        base_url: "https://openrouter.ai/api/v1",
        kind: "openai",
        models: &["anthropic/claude-sonnet-4", "openai/gpt-4o", "google/gemini-2.5-pro"],
        key_required: true,
        note: "key at https://openrouter.ai/keys",
    },
    Preset {
        name: "openai",
        label: "OpenAI",
        base_url: "https://api.openai.com/v1",
        kind: "openai",
        models: &["gpt-4o-mini", "gpt-4o"],
        key_required: true,
        note: "key at https://platform.openai.com/api-keys",
    },
    Preset {
        name: "anthropic",
        label: "Anthropic (Claude, native protocol)",
        base_url: "https://api.anthropic.com",
        kind: "anthropic",
        models: &["claude-sonnet-4", "claude-opus-4"],
        key_required: true,
        note: "key at https://console.anthropic.com",
    },
    Preset {
        name: "groq",
        label: "Groq (ultra-fast inference)",
        base_url: "https://api.groq.com/openai/v1",
        kind: "openai",
        models: &["llama-3.3-70b-versatile", "mixtral-8x7b-32768"],
        key_required: true,
        note: "key at https://console.groq.com/keys",
    },
    Preset {
        name: "deepseek",
        label: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        kind: "openai",
        models: &["deepseek-chat", "deepseek-reasoner"],
        key_required: true,
        note: "key at https://platform.deepseek.com",
    },
    Preset {
        name: "ollama",
        label: "Ollama (local, no cloud)",
        base_url: "http://localhost:11434/v1",
        kind: "openai",
        models: &["llama3.1", "qwen2.5-coder"],
        key_required: false,
        note: "run `ollama serve` first - any key is accepted",
    },
    Preset {
        name: "lmstudio",
        label: "LM Studio (local)",
        base_url: "http://localhost:1234/v1",
        kind: "openai",
        models: &["local-model"],
        key_required: false,
        note: "start the LM Studio server first",
    },
];

pub fn find(name: &str) -> Option<&'static Preset> {
    let n = name.trim().to_ascii_lowercase();
    PRESETS.iter().find(|p| p.name == n)
}

/// Numbered menu used by the interactive setup wizard and `dragon setup`.
pub fn menu() -> String {
    let mut s = String::from("pick a provider - type the number or its name:\n");
    for (i, p) in PRESETS.iter().enumerate() {
        s.push_str(&format!("  {:>2}. {:<10} {}\n", i + 1, p.name, p.label));
    }
    s.push_str("      custom     any other OpenAI-compatible endpoint");
    s
}
