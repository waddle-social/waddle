use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionModuleConfig {
    pub name: String,
    pub registry: String,
    pub tag: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub config: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionConfig {
    pub enabled: bool,
    pub cache_dir: String,
    #[serde(default)]
    pub modules: Vec<ExtensionModuleConfig>,
}

impl Default for ExtensionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_dir: "/var/lib/waddle/extensions".to_string(),
            modules: vec![ExtensionModuleConfig {
                name: "github-enricher".to_string(),
                registry: "ghcr.io/waddle-social/waddle/extensions/github-enricher".to_string(),
                tag: "latest".to_string(),
                namespace: "urn:waddle:github:0".to_string(),
                config: Value::Object(Default::default()),
            }],
        }
    }
}

impl ExtensionConfig {
    /// Load extension configuration from `WADDLE_EXTENSIONS_JSON`.
    ///
    /// If the variable is missing or blank, falls back to `Default`.
    pub fn from_env() -> Result<Self, String> {
        let Some(raw) = std::env::var("WADDLE_EXTENSIONS_JSON")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        else {
            return Ok(Self::default());
        };

        serde_json::from_str::<ExtensionConfig>(&raw)
            .map_err(|error| format!("invalid WADDLE_EXTENSIONS_JSON: {error}"))
    }
}
