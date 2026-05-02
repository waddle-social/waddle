use std::collections::BTreeMap;

use oci_client::Reference;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::ExtensionCapability;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionModuleConfig {
    pub name: String,
    #[serde(default)]
    pub registry: String,
    /// Immutable OCI artifact digest for the extension WASM component.
    ///
    /// Production extension modules must be loaded by digest, never by a mutable
    /// tag. `local_path` development modules may omit this field.
    #[serde(default, alias = "artifactDigest")]
    pub digest: Option<String>,
    /// Deprecated development-only locator retained so local test fixtures can
    /// name a component without publishing it. Non-local modules reject tags.
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub config: Value,
    /// Host/operator-granted capabilities for this module. The runtime grants
    /// only the intersection of this list and the extension manifest.
    #[serde(default, alias = "capabilityGrants")]
    pub capability_grants: Vec<ExtensionCapability>,
    /// HTTPS origins allowed for `outbound.http.request`, e.g.
    /// `https://api.example.com`. Empty means no outbound HTTP targets.
    #[serde(default, alias = "allowedHttpOrigins")]
    pub allowed_http_origins: Vec<String>,
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
            modules: Vec::new(),
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

        let config = serde_json::from_str::<ExtensionConfig>(&raw)
            .map_err(|error| format!("invalid WADDLE_EXTENSIONS_JSON: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.cache_dir.trim().is_empty() {
            return Err("extension cacheDir must not be empty".to_string());
        }
        for module in &self.modules {
            module.validate()?;
        }
        Ok(())
    }
}

impl ExtensionModuleConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("extension module name must not be empty".to_string());
        }

        if let Some(local_path) = self.local_path.as_deref() {
            if local_path.trim().is_empty() {
                return Err(format!(
                    "extension {} localPath must not be empty",
                    self.name
                ));
            }
            if let Some(digest) = self.digest.as_deref() {
                validate_digest(&self.name, digest)?;
            }
            return Ok(());
        }

        self.oci_registry_and_digest()?;
        Ok(())
    }

    pub(crate) fn oci_registry_and_digest(&self) -> Result<(&str, &str), String> {
        if self.registry.trim().is_empty() {
            return Err(format!(
                "extension {} registry must not be empty",
                self.name
            ));
        }
        if self.registry.trim() != self.registry {
            return Err(format!(
                "extension {} registry must not include surrounding whitespace",
                self.name
            ));
        }
        if self.tag.is_some() {
            return Err(format!(
                "extension {} must use an immutable digest instead of tag",
                self.name
            ));
        }

        let digest = self.digest.as_deref().ok_or_else(|| {
            format!(
                "extension {} must set digest unless localPath is configured",
                self.name
            )
        })?;
        validate_digest(&self.name, digest)?;

        let reference = parse_digest_reference(&self.name, &self.registry, digest)?;
        if reference.tag().is_some() {
            return Err(format!(
                "extension {} registry must not include a mutable tag; set digest separately",
                self.name
            ));
        }

        Ok((&self.registry, digest))
    }

    pub fn required_digest(&self) -> Result<&str, String> {
        let digest = self.digest.as_deref().ok_or_else(|| {
            format!(
                "extension {} must set digest unless localPath is configured",
                self.name
            )
        })?;
        validate_digest(&self.name, digest)?;
        Ok(digest)
    }
}

fn validate_digest(module_name: &str, digest: &str) -> Result<(), String> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(format!(
            "extension {module_name} digest must use sha256:<64 hex>"
        ));
    };
    if hex.len() != 64 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(format!(
            "extension {module_name} digest must use sha256:<64 hex>"
        ));
    }
    Ok(())
}

fn parse_digest_reference(
    module_name: &str,
    registry: &str,
    digest: &str,
) -> Result<Reference, String> {
    let reference = format!("{registry}@{digest}");
    reference.parse::<Reference>().map_err(|error| {
        format!("extension {module_name} OCI reference {reference} is invalid: {error}")
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use serde_json::Value;

    use crate::types::ExtensionCapability;

    use super::{ExtensionConfig, ExtensionModuleConfig};

    const TEST_DIGEST: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn default_has_no_bundled_modules() {
        let config = ExtensionConfig::default();
        assert!(config.modules.is_empty());
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
                    "name": "example-extension",
                    "registry": "ghcr.io/waddle-social/waddle/extensions/example-extension",
                    "artifactDigest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "namespace": "urn:example:extension:1",
                    "config": {"log_level": "debug"},
                    "configSecretFiles": {
                        "github_token": "/var/run/secrets/github-token"
                    },
                    "localPath": "/srv/waddle/extensions/example-extension.wasm"
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
            Some("/srv/waddle/extensions/example-extension.wasm")
        );

        if let Some(previous) = previous {
            std::env::set_var("WADDLE_EXTENSIONS_JSON", previous);
        } else {
            std::env::remove_var("WADDLE_EXTENSIONS_JSON");
        }
    }

    #[test]
    fn from_env_rejects_non_local_tagged_modules() {
        let _guard = env_lock().lock().expect("lock should not be poisoned");
        let previous = std::env::var("WADDLE_EXTENSIONS_JSON").ok();

        std::env::set_var(
            "WADDLE_EXTENSIONS_JSON",
            r#"{
                "enabled": true,
                "cacheDir": "/srv/waddle/extensions",
                "modules": [{
                    "name": "example-extension",
                    "registry": "ghcr.io/waddle-social/waddle/extensions/example-extension",
                    "tag": "latest"
                }]
            }"#,
        );

        let error = ExtensionConfig::from_env().expect_err("tag-only module should fail");
        assert!(error.contains("immutable digest instead of tag"));

        if let Some(previous) = previous {
            std::env::set_var("WADDLE_EXTENSIONS_JSON", previous);
        } else {
            std::env::remove_var("WADDLE_EXTENSIONS_JSON");
        }
    }

    #[test]
    fn from_env_parses_digest_pinned_oci_module() {
        let _guard = env_lock().lock().expect("lock should not be poisoned");
        let previous = std::env::var("WADDLE_EXTENSIONS_JSON").ok();

        std::env::set_var(
            "WADDLE_EXTENSIONS_JSON",
            r#"{
                "enabled": true,
                "cacheDir": "/srv/waddle/extensions",
                "modules": [{
                    "name": "example-extension",
                    "registry": "ghcr.io/waddle-social/waddle/extensions/example-extension",
                    "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "namespace": "urn:example:extension:1",
                    "capabilityGrants": ["commands", "outbound.http.request"],
                    "allowedHttpOrigins": ["https://api.example.test"]
                }]
            }"#,
        );

        let config = ExtensionConfig::from_env().expect("config should parse");
        assert_eq!(config.modules[0].digest.as_deref(), Some(TEST_DIGEST));
        assert!(config.modules[0].tag.is_none());
        assert!(config.modules[0].local_path.is_none());
        assert_eq!(
            config.modules[0].capability_grants,
            vec![
                ExtensionCapability::Commands,
                ExtensionCapability::OutboundHttpRequest,
            ]
        );
        assert_eq!(
            config.modules[0].allowed_http_origins,
            vec!["https://api.example.test"]
        );

        if let Some(previous) = previous {
            std::env::set_var("WADDLE_EXTENSIONS_JSON", previous);
        } else {
            std::env::remove_var("WADDLE_EXTENSIONS_JSON");
        }
    }

    #[test]
    fn validate_rejects_missing_digest_for_oci_module() {
        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
            digest: None,
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: Value::Object(Default::default()),
            capability_grants: Vec::new(),
            allowed_http_origins: Vec::new(),
            config_secret_files: Default::default(),
            local_path: None,
        };

        let error = module
            .validate()
            .expect_err("OCI module without digest should fail validation");
        assert!(error.contains("must set digest unless localPath is configured"));
    }

    #[test]
    fn validate_rejects_registry_with_embedded_tag() {
        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension:latest"
                .to_string(),
            digest: Some(TEST_DIGEST.to_string()),
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: Value::Object(Default::default()),
            capability_grants: Vec::new(),
            allowed_http_origins: Vec::new(),
            config_secret_files: Default::default(),
            local_path: None,
        };

        let error = module
            .validate()
            .expect_err("registry tag should fail validation");
        assert!(error.contains("registry must not include a mutable tag"));
    }

    #[test]
    fn validate_preserves_local_path_without_oci_reference() {
        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: String::new(),
            digest: None,
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: Value::Object(Default::default()),
            capability_grants: Vec::new(),
            allowed_http_origins: Vec::new(),
            config_secret_files: Default::default(),
            local_path: Some("/srv/waddle/extensions/example-extension.wasm".to_string()),
        };

        module
            .validate()
            .expect("localPath development module should not require OCI registry or digest");
    }

    #[test]
    fn from_env_preserves_local_path_without_oci_reference() {
        let _guard = env_lock().lock().expect("lock should not be poisoned");
        let previous = std::env::var("WADDLE_EXTENSIONS_JSON").ok();

        std::env::set_var(
            "WADDLE_EXTENSIONS_JSON",
            r#"{
                "enabled": true,
                "cacheDir": "/srv/waddle/extensions",
                "modules": [{
                    "name": "example-extension",
                    "localPath": "/srv/waddle/extensions/example-extension.wasm"
                }]
            }"#,
        );

        let config = ExtensionConfig::from_env().expect("localPath config should parse");
        assert_eq!(config.modules[0].registry, "");
        assert_eq!(
            config.modules[0].local_path.as_deref(),
            Some("/srv/waddle/extensions/example-extension.wasm")
        );

        if let Some(previous) = previous {
            std::env::set_var("WADDLE_EXTENSIONS_JSON", previous);
        } else {
            std::env::remove_var("WADDLE_EXTENSIONS_JSON");
        }
    }
}
