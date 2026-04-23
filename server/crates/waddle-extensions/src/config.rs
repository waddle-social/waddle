use std::collections::BTreeMap;

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
    /// Optional map of config key -> file path. Each file is read at startup and
    /// injected into `config` as a string value under the given key.
    ///
    /// This is intended for secrets delivered by mounted files (for example CSI
    /// secret file mounts) while keeping `WADDLE_EXTENSIONS_JSON` in ConfigMap.
    #[serde(default, alias = "configSecretFiles")]
    pub config_secret_files: BTreeMap<String, String>,
    /// If set, load the WASM component from this filesystem path directly,
    /// bypassing the OCI puller. Intended for development and tests.
    #[serde(default, alias = "localPath")]
    pub local_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionConfig {
    pub enabled: bool,
    #[serde(alias = "cacheDir")]
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
                config_secret_files: BTreeMap::new(),
                local_path: None,
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

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::ExtensionConfig;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn default_module_has_empty_secret_file_map() {
        let config = ExtensionConfig::default();
        assert_eq!(config.modules.len(), 1);
        assert!(config.modules[0].config_secret_files.is_empty());
    }

    #[test]
    fn from_env_parses_camel_case_secret_file_fields() {
        let _guard = env_lock().lock().expect("lock should not be poisoned");
        let previous = std::env::var("WADDLE_EXTENSIONS_JSON").ok();

        std::env::set_var(
            "WADDLE_EXTENSIONS_JSON",
            r#"{
                "enabled": true,
                "cacheDir": "/srv/waddle/extensions",
                "modules": [{
                    "name": "github-enricher",
                    "registry": "ghcr.io/waddle-social/waddle/extensions/github-enricher",
                    "tag": "latest",
                    "namespace": "urn:waddle:github:0",
                    "config": {"log_level": "debug"},
                    "configSecretFiles": {
                        "github_token": "/var/run/secrets/github-token"
                    },
                    "localPath": "/srv/waddle/extensions/github-enricher.wasm"
                }]
            }"#,
        );

        let config = ExtensionConfig::from_env().expect("config should parse");
        assert_eq!(config.cache_dir, "/srv/waddle/extensions");
        assert_eq!(
            config.modules[0]
                .config_secret_files
                .get("github_token")
                .map(String::as_str),
            Some("/var/run/secrets/github-token")
        );
        assert_eq!(
            config.modules[0].local_path.as_deref(),
            Some("/srv/waddle/extensions/github-enricher.wasm")
        );

        if let Some(previous) = previous {
            std::env::set_var("WADDLE_EXTENSIONS_JSON", previous);
        } else {
            std::env::remove_var("WADDLE_EXTENSIONS_JSON");
        }
    }
}
