use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub provider: ProviderSection,
    #[serde(default)]
    pub capture: CaptureSection,
    #[serde(default)]
    pub ui: UiSection,
    #[serde(default)]
    pub hotkey: HotkeySection,
    #[serde(default)]
    pub image: ImageSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProviderSection {
    /// "openai" (openai-compatible: OpenAI, Ollama /v1, OpenRouter, …)
    /// or "anthropic".
    pub kind: String,
    pub base_url: Option<String>,
    pub model: String,
    pub api_key: String,
    /// Output token cap per request. Reasoning-heavy models spend most of
    /// this on thinking; answers cut mid-sentence mean this is too low
    /// (truncation is flagged in the UI when it happens).
    pub max_tokens: u32,
    /// Extra named providers so one config can hold several, e.g.
    /// [provider.named.ollama] kind="openai" base_url="http://localhost:11434/v1" …
    /// selected per-session with SetModel / --provider.
    pub named: BTreeMap<String, NamedProvider>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedProvider {
    pub kind: String,
    pub base_url: Option<String>,
    pub model: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CaptureSection {
    /// auto | gnome | kwin | grim | x11 | portal | none
    pub backend: String,
    /// "active" in v1; multi-output later.
    pub output: String,
}

impl Default for CaptureSection {
    fn default() -> Self {
        Self { backend: "auto".into(), output: "active".into() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UiSection {
    /// auto | extension | gtk | none — which frontend the daemon prefers.
    pub preferred_frontend: String,
    pub panel_width: u32,
}

impl Default for UiSection {
    fn default() -> Self {
        Self { preferred_frontend: "auto".into(), panel_width: 420 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HotkeySection {
    pub toggle: String,
    pub attach: String,
}

impl Default for HotkeySection {
    fn default() -> Self {
        Self { toggle: "super+shift+a".into(), attach: "super+shift+s".into() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ImageSection {
    pub max_edge: u32,
    pub jpeg_quality: u32,
    pub max_bytes: usize,
    /// How many most-recent images are kept in outbound context (spec §9.3).
    pub keep_images_last: usize,
}

impl Default for ImageSection {
    fn default() -> Self {
        Self { max_edge: 1568, jpeg_quality: 85, max_bytes: 4_000_000, keep_images_last: 2 }
    }
}

impl Default for ProviderSection {
    fn default() -> Self {
        Self {
            kind: "openai".into(),
            base_url: None,
            model: "gpt-4o".into(),
            api_key: String::new(),
            max_tokens: 8192,
            named: BTreeMap::new(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: ProviderSection::default(),
            capture: CaptureSection::default(),
            ui: UiSection::default(),
            hotkey: HotkeySection::default(),
            image: ImageSection::default(),
        }
    }
}

impl Config {
    pub fn load_or_default() -> (Self, Option<String>) {
        match Self::load() {
            Ok(cfg) => (cfg, None),
            Err(e) => (Self { provider: ProviderSection { api_key: Self::env_key(), ..Default::default() }, ..Default::default() }, Some(e.to_string())),
        }
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        let mut cfg: Config = toml::from_str(&text)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        if cfg.provider.api_key.is_empty() {
            cfg.provider.api_key = Self::env_key();
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// AISLE_API_KEY, falling back to the pre-rename SIDEAGENT_API_KEY.
    fn env_key() -> String {
        std::env::var("AISLE_API_KEY")
            .or_else(|_| std::env::var("SIDEAGENT_API_KEY"))
            .unwrap_or_default()
    }

    pub fn config_path() -> Result<PathBuf> {
        if let Ok(p) = std::env::var("AISLE_CONFIG") {
            return Ok(PathBuf::from(p));
        }
        if let Ok(p) = std::env::var("SIDEAGENT_CONFIG") { // pre-rename, still honored
            return Ok(PathBuf::from(p));
        }
        let base = dirs::config_dir().ok_or_else(|| Error::Config("no config dir".into()))?;
        let path = base.join("aisle").join("config.toml");
        let legacy = base.join("sideagent").join("config.toml"); // pre-rename dir
        if !path.exists() && legacy.exists() {
            return Ok(legacy);
        }
        Ok(path)
    }

    pub fn data_dir() -> Result<PathBuf> {
        if let Ok(p) = std::env::var("AISLE_DATA") {
            return Ok(PathBuf::from(p));
        }
        if let Ok(p) = std::env::var("SIDEAGENT_DATA") { // pre-rename, still honored
            return Ok(PathBuf::from(p));
        }
        let base = dirs::data_dir().ok_or_else(|| Error::Config("no data dir".into()))?;
        let dir = base.join("aisle");
        let legacy = base.join("sideagent"); // pre-rename dir: sessions keep loading
        if !dir.exists() && legacy.exists() {
            return Ok(legacy);
        }
        Ok(dir)
    }

    fn validate(&self) -> Result<()> {
        if matches!(self.provider.kind.as_str(), "openai" | "anthropic") == false {
            return Err(Error::Config(format!("unknown provider kind {:?}", self.provider.kind)));
        }
        if self.provider.api_key.trim().is_empty() {
            return Err(Error::Config("provider.api_key is empty".into()));
        }
        if self.provider.model.trim().is_empty() {
            return Err(Error::Config("provider.model is empty".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_example() {
        let toml_src = r#"
[provider]
kind = "openai"
base_url = "http://localhost:11434/v1"
model = "qwen2.5vl:7b"
api_key = "dummy"

[provider.named.claude]
kind = "anthropic"
model = "claude-sonnet-4-5"
api_key = "sk-ant-x"

[capture]
backend = "gnome"

[image]
max_edge = 1200
"#;
        let cfg: Config = toml::from_str(toml_src).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.capture.backend, "gnome");
        assert_eq!(cfg.image.max_edge, 1200);
        assert_eq!(cfg.image.keep_images_last, 2);
        assert_eq!(cfg.provider.named["claude"].kind, "anthropic");
        assert_eq!(cfg.ui.panel_width, 420);
        assert_eq!(cfg.provider.max_tokens, 8192); // default survives partial sections
    }

    #[test]
    fn rejects_empty_key() {
        let cfg: Config = toml::from_str("[provider]\napi_key=\"\"\n").unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_field() {
        let r: std::result::Result<Config, _> =
            toml::from_str("[provider]\napi_key=\"x\"\ntypo=1\n");
        assert!(r.is_err());
    }
}
