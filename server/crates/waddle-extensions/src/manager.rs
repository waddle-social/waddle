use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Error as AnyhowError, Result};
use futures::future::join_all;
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use thiserror::Error;
use tokio::time::timeout;
use tracing::{debug, warn};
use xmpp_parsers::message::Message;

use crate::actor::WasmExtensionActor;
use crate::config::{ExtensionConfig, ExtensionModuleConfig};
use crate::oci::OciExtensionPuller;
use crate::runtime::{LoadedExtension, WasmRuntime};
use crate::types::{message_has_embed_for_namespaces, DetectedLink};

const MAX_DETECTED_LINKS: usize = 3;
const EXTENSION_ENRICH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
enum EffectiveModuleConfigError {
    #[error("extension {extension} config_secret_files requires config to be a JSON object")]
    NonObjectBaseConfig { extension: String },
    #[error("failed to read config_secret_files[{key}] from {path} for extension {extension}")]
    ReadSecretFile {
        extension: String,
        key: String,
        path: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct ExtensionManager {
    actors: Vec<Arc<WasmExtensionActor>>,
    feature_namespaces: Vec<String>,
}

impl ExtensionManager {
    /// Build an `ExtensionManager` from the given configuration.
    ///
    /// Failures to load an individual extension are logged (fail-open) and do not
    /// prevent the remaining extensions from loading.
    pub async fn from_config(config: ExtensionConfig) -> Result<Self> {
        if !config.enabled {
            return Ok(Self {
                actors: Vec::new(),
                feature_namespaces: Vec::new(),
            });
        }

        let runtime = WasmRuntime::new()?;
        let puller = OciExtensionPuller::new(&config.cache_dir);
        let mut actors = Vec::new();
        let mut feature_namespaces = Vec::new();

        for module in &config.modules {
            let config_json = match effective_module_config_json(module) {
                Ok(config_json) => config_json,
                Err(error) => {
                    warn!(
                        extension = %module.name,
                        %error,
                        "failed to prepare extension config; skipping enrichment actor"
                    );
                    continue;
                }
            };

            let wasm_path = match puller.resolve_wasm_path(module).await {
                Ok(path) => path,
                Err(error) => {
                    warn!(
                        extension = %module.name,
                        %error,
                        error_chain = %format_error_chain(&error),
                        "failed to resolve extension WASM path; skipping enrichment actor"
                    );
                    continue;
                }
            };

            let loaded = match LoadedExtension::load(&runtime, &wasm_path) {
                Ok(loaded) => loaded,
                Err(error) => {
                    if module.local_path.is_none() {
                        remove_invalid_cached_extension(module, &wasm_path);
                    }
                    warn!(
                        extension = %module.name,
                        %error,
                        error_chain = %format_error_chain(&error),
                        "failed to compile extension component; skipping enrichment actor"
                    );
                    continue;
                }
            };

            let actor = match WasmExtensionActor::initialize(loaded, &config_json).await {
                Ok(actor) => actor,
                Err(error) => {
                    warn!(
                        extension = %module.name,
                        %error,
                        error_chain = %format_error_chain(&error),
                        "extension init() failed; skipping enrichment actor"
                    );
                    continue;
                }
            };

            let info = actor.info();
            if !info.namespace.is_empty() && !feature_namespaces.contains(&info.namespace) {
                feature_namespaces.push(info.namespace.clone());
            }
            for feature in &info.features {
                if !feature_namespaces.contains(&feature.namespace) {
                    feature_namespaces.push(feature.namespace.clone());
                }
            }

            actors.push(Arc::new(actor));
        }

        Ok(Self {
            actors,
            feature_namespaces,
        })
    }

    pub async fn from_env() -> Result<Self> {
        let config = ExtensionConfig::from_env().map_err(anyhow::Error::msg)?;
        Self::from_config(config).await
    }

    pub fn feature_namespaces(&self) -> &[String] {
        &self.feature_namespaces
    }

    pub fn extension_features(&self) -> Vec<String> {
        self.feature_namespaces.clone()
    }

    pub async fn enrich_message(&self, msg: &mut Message) -> usize {
        if message_has_embed_for_namespaces(msg, &self.feature_namespaces) {
            return 0;
        }

        let Some(body) = msg
            .bodies
            .get("")
            .or_else(|| msg.bodies.values().next())
            .map(|body| body.0.clone())
        else {
            return 0;
        };

        let links = detect_links(&body);
        if links.is_empty() {
            return 0;
        }

        let mut count = 0usize;
        if !self.actors.is_empty() {
            let enrich_futures = self.actors.iter().map(|actor| {
                let actor_name = actor.info().name;
                let actor = Arc::clone(actor);
                let body = body.clone();
                let links = links.clone();
                async move {
                    match timeout(EXTENSION_ENRICH_TIMEOUT, actor.enrich_message(body, links)).await
                    {
                        Ok(embeds) => embeds,
                        Err(_) => {
                            warn!(
                                extension = %actor_name,
                                timeout_secs = EXTENSION_ENRICH_TIMEOUT.as_secs(),
                                "extension enrichment timed out; continuing fail-open"
                            );
                            Vec::new()
                        }
                    }
                }
            });
            let results = join_all(enrich_futures).await;

            for embeds in results {
                for embed in embeds {
                    msg.payloads.push(embed.to_minidom());
                    count += 1;
                }
            }
            if count > 0 {
                debug!(embeds_added = count, "message enriched by extensions");
            }
        }
        count
    }
}

fn remove_invalid_cached_extension(module: &ExtensionModuleConfig, wasm_path: &Path) {
    match std::fs::remove_file(wasm_path) {
        Ok(()) => {
            warn!(
                extension = %module.name,
                cache_path = %wasm_path.display(),
                "removed cached extension after component load failure"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            warn!(
                extension = %module.name,
                cache_path = %wasm_path.display(),
                %error,
                "failed to remove cached extension after component load failure"
            );
        }
    }
}

fn effective_module_config_json(
    module: &ExtensionModuleConfig,
) -> Result<String, EffectiveModuleConfigError> {
    effective_module_config_with_reader(module, |path| std::fs::read_to_string(path))
        .map(|value| value.to_string())
}

fn effective_module_config_with_reader<F>(
    module: &ExtensionModuleConfig,
    mut read_to_string: F,
) -> Result<Value, EffectiveModuleConfigError>
where
    F: FnMut(&Path) -> std::io::Result<String>,
{
    if module.config_secret_files.is_empty() {
        return Ok(module.config.clone());
    }

    let mut config = match module.config.clone() {
        Value::Object(config) => config,
        _ => {
            return Err(EffectiveModuleConfigError::NonObjectBaseConfig {
                extension: module.name.clone(),
            });
        }
    };

    for (key, path) in &module.config_secret_files {
        let contents = read_to_string(Path::new(path)).map_err(|source| {
            EffectiveModuleConfigError::ReadSecretFile {
                extension: module.name.clone(),
                key: key.clone(),
                path: path.clone(),
                source,
            }
        })?;
        config.insert(key.clone(), Value::String(contents));
    }

    Ok(Value::Object(config))
}

fn detect_links(body: &str) -> Vec<DetectedLink> {
    static FENCED_CODE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static INLINE_CODE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static URL_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let fenced_re =
        FENCED_CODE_RE.get_or_init(|| Regex::new(r"(?s)```.*?```").expect("valid fenced regex"));
    let inline_re =
        INLINE_CODE_RE.get_or_init(|| Regex::new(r"`[^`\n]*`").expect("valid inline regex"));
    let re =
        URL_RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"'`]+"#).expect("valid link regex"));

    let mut ignored_ranges: Vec<(usize, usize)> = fenced_re
        .find_iter(body)
        .map(|m| (m.start(), m.end()))
        .collect();
    ignored_ranges.extend(inline_re.find_iter(body).map(|m| (m.start(), m.end())));
    ignored_ranges.sort_unstable_by_key(|(start, _)| *start);

    let mut seen_urls = HashSet::new();
    let mut links = Vec::new();

    for m in re.find_iter(body) {
        if links.len() >= MAX_DETECTED_LINKS {
            break;
        }
        if ignored_ranges
            .iter()
            .any(|(start, end)| m.start() >= *start && m.start() < *end)
        {
            continue;
        }

        let trimmed = m
            .as_str()
            .trim_end_matches(['.', ',', '!', '?', ';', ':', ')', ']']);
        if trimmed.is_empty() || seen_urls.contains(trimmed) {
            continue;
        }
        seen_urls.insert(trimmed.to_string());

        links.push(DetectedLink {
            url: trimmed.to_string(),
            start_offset: m.start() as u32,
            end_offset: (m.start() + trimmed.len()) as u32,
        });
    }

    links
}

fn format_error_chain(error: &AnyhowError) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use xmpp_parsers::message::{Body, Message};

    use super::{
        detect_links, effective_module_config_json, effective_module_config_with_reader,
        EffectiveModuleConfigError, ExtensionManager, MAX_DETECTED_LINKS,
    };
    use crate::config::{ExtensionConfig, ExtensionModuleConfig};

    #[test]
    fn detects_urls() {
        let links = detect_links("hello https://github.com/waddle-social/waddle world");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://github.com/waddle-social/waddle");
    }

    #[test]
    fn deduplicates_and_caps_links() {
        let links = detect_links(
            "https://a.test https://a.test https://b.test https://c.test https://d.test",
        );
        assert_eq!(links.len(), MAX_DETECTED_LINKS);
        assert_eq!(links[0].url, "https://a.test");
        assert_eq!(links[1].url, "https://b.test");
    }

    #[test]
    fn skips_urls_inside_code_and_trims_punctuation() {
        let body = "Use `https://example.com/in-code` and:\nhttps://github.com/waddle-social/waddle).\n```https://skip.me```";
        let links = detect_links(body);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://github.com/waddle-social/waddle");
    }

    #[test]
    fn merges_secret_file_values_into_effective_config() {
        let mut config_secret_files = BTreeMap::new();
        config_secret_files.insert(
            "github_token".to_string(),
            "/secrets/github-token".to_string(),
        );
        config_secret_files.insert(
            "webhook_secret".to_string(),
            "/secrets/webhook-secret".to_string(),
        );

        let module = ExtensionModuleConfig {
            name: "github-enricher".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/github-enricher".to_string(),
            tag: "latest".to_string(),
            namespace: "urn:waddle:github:0".to_string(),
            config: json!({
                "github_token": "from-config",
                "log_level": "debug"
            }),
            config_secret_files,
            local_path: None,
        };

        let merged = effective_module_config_with_reader(&module, |path| match path.to_str() {
            Some("/secrets/github-token") => Ok("from-secret-file".to_string()),
            Some("/secrets/webhook-secret") => Ok("webhook-value".to_string()),
            other => panic!("unexpected path: {other:?}"),
        })
        .expect("config should merge");

        assert_eq!(
            merged,
            json!({
                "github_token": "from-secret-file",
                "log_level": "debug",
                "webhook_secret": "webhook-value"
            })
        );
    }

    #[test]
    fn rejects_non_object_config_when_secret_files_are_enabled() {
        let mut config_secret_files = BTreeMap::new();
        config_secret_files.insert(
            "github_token".to_string(),
            "/secrets/github-token".to_string(),
        );

        let module = ExtensionModuleConfig {
            name: "github-enricher".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/github-enricher".to_string(),
            tag: "latest".to_string(),
            namespace: "urn:waddle:github:0".to_string(),
            config: json!(["not", "an", "object"]),
            config_secret_files,
            local_path: None,
        };

        let error = effective_module_config_with_reader(&module, |_| Ok(String::new()))
            .expect_err("non-object config should fail");
        assert!(matches!(
            error,
            EffectiveModuleConfigError::NonObjectBaseConfig { extension }
            if extension == "github-enricher"
        ));
    }

    #[test]
    fn reads_secret_files_from_disk_when_building_effective_config() {
        let artifact_dir = TestArtifacts::new();
        let secret_path = artifact_dir.write("github-token", "file-secret\n");

        let mut config_secret_files = BTreeMap::new();
        config_secret_files.insert(
            "github_token".to_string(),
            secret_path.to_string_lossy().into_owned(),
        );

        let module = ExtensionModuleConfig {
            name: "github-enricher".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/github-enricher".to_string(),
            tag: "latest".to_string(),
            namespace: "urn:waddle:github:0".to_string(),
            config: json!({}),
            config_secret_files,
            local_path: None,
        };

        let config_json =
            effective_module_config_json(&module).expect("secret file should be read from disk");
        assert_eq!(config_json, r#"{"github_token":"file-secret\n"}"#);
    }

    #[tokio::test]
    async fn from_config_does_not_advertise_namespace_or_fallback_when_actor_cannot_load() {
        let config = ExtensionConfig {
            enabled: true,
            cache_dir: "/var/lib/waddle/extensions".to_string(),
            modules: vec![ExtensionModuleConfig {
                name: "github-enricher".to_string(),
                registry: "ghcr.io/waddle-social/waddle/extensions/github-enricher".to_string(),
                tag: "latest".to_string(),
                namespace: "urn:waddle:github:0".to_string(),
                config: json!({}),
                config_secret_files: Default::default(),
                local_path: Some("missing-github-enricher-test.wasm".to_string()),
            }],
        };

        let manager = ExtensionManager::from_config(config)
            .await
            .expect("manager should stay fail-open");
        assert!(manager.feature_namespaces().is_empty());

        let mut msg = Message::new(None);
        msg.bodies.insert(
            String::new(),
            Body("https://github.com/waddle-social/waddle".to_string()),
        );
        assert_eq!(manager.enrich_message(&mut msg).await, 0);
        assert!(msg.payloads.is_empty());
    }

    #[tokio::test]
    async fn enrich_message_does_not_fallback_without_loaded_actor() {
        let manager = ExtensionManager {
            actors: Vec::new(),
            feature_namespaces: vec!["urn:waddle:github:0".to_string()],
        };

        let mut msg = Message::new(None);
        msg.bodies.insert(
            String::new(),
            Body("https://github.com/waddle-social/waddle".to_string()),
        );

        assert_eq!(manager.enrich_message(&mut msg).await, 0);
        assert!(msg.payloads.is_empty());
    }

    struct TestArtifacts {
        root: PathBuf,
    }

    impl TestArtifacts {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should move forward")
                .as_nanos();
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("test-artifacts")
                .join(format!("manager-{nonce}-{}", std::process::id()));
            fs::create_dir_all(&root).expect("artifact directory should be created");
            Self { root }
        }

        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::write(&path, contents).expect("artifact file should be written");
            path
        }
    }

    impl Drop for TestArtifacts {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
