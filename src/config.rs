//! Configuration: TOML on disk, BYOK providers, model resolution.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Model spec like "openrouter/anthropic/claude-sonnet-4".
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub providers: Vec<ProviderCfg>,
    #[serde(default)]
    pub settings: Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCfg {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// "openai" or "anthropic". Auto-detected from base_url when omitted.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// When history grows past this many entries it gets compacted into a summary.
    #[serde(default = "default_compaction")]
    pub compaction_messages: usize,
    /// Allow the agent to run shell commands via its run_shell tool.
    #[serde(default)]
    pub allow_commands: bool,
}

fn default_compaction() -> usize {
    36
}

impl Default for ProviderCfg {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            kind: None,
            models: Vec::new(),
        }
    }
}

impl Config {
    pub fn dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dragon")
    }

    pub fn path() -> PathBuf {
        Self::dir().join("config.toml")
    }

    /// Where sessions and memory live.
    pub fn data_dir() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dragon")
    }

    pub fn load() -> Result<Self> {
        let p = Self::path();
        if !p.exists() {
            return Ok(Config::default());
        }
        let raw =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", p.display()))
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(Self::dir())?;
        std::fs::write(Self::path(), toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn find_provider(&self, name: &str) -> Option<&ProviderCfg> {
        self.providers.iter().find(|p| p.name == name)
    }

    pub fn provider_mut(&mut self, name: &str) -> Option<&mut ProviderCfg> {
        self.providers.iter_mut().find(|p| p.name == name)
    }

    /// Resolve a model spec ("provider/model", possibly with more slashes) to
    /// a concrete provider + model id. `None` falls back to default_model,
    /// then to the single configured provider if there is exactly one.
    pub fn resolve_model(&self, spec: Option<&str>) -> Result<(&ProviderCfg, &str)> {
        let spec = spec.or(self.default_model.as_deref());

        let (prov_name, model_id) = match spec {
            Some(s) => match s.split_once('/') {
                Some((p, m)) => (p.to_string(), m.to_string()),
                None => {
                    // bare model id: needs exactly one provider
                    if self.providers.len() == 1 {
                        (self.providers[0].name.clone(), s.to_string())
                    } else {
                        bail!(
                            "model '{s}' has no provider prefix; use 'provider/model' \
                             (configured providers: {})",
                            self.provider_names().join(", ")
                        );
                    }
                }
            },
            None => {
                // no spec, no default: single provider + single model shortcut
                if let Some(p) = self.providers.first() {
                    if p.models.len() == 1 || self.default_model.is_some() {
                        let m = p.models.first().map(|x| x.clone()).unwrap_or_default();
                        bail!("no default model set; try 'dragon --model {}/{}'", p.name, m);
                    }
                    bail!("no default model set; pick one from provider '{}'", p.name);
                }
                bail!(
                    "no models configured yet.\n\nAdd your first model:\n  dragon model add openai https://api.openai.com/v1 --key sk-... --models gpt-4o-mini\n\nOr point at anything OpenAI-compatible (OpenRouter, Groq, Ollama, LM Studio):\n  dragon model add ollama http://localhost:11434/v1 --key ollama --models llama3.1"
                );
            }
        };

        let prov = self
            .find_provider(&prov_name)
            .with_context(|| format!("provider '{prov_name}' not found"))?;
        Ok((prov, &model_id))
    }

    pub fn provider_names(&self) -> Vec<String> {
        self.providers.iter().map(|p| p.name.clone()).collect()
    }
}
