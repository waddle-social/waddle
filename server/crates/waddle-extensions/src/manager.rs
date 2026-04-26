use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::future::join_all;
use minidom::Element;
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
use crate::types::{message_has_embed_for_namespaces, DetectedLink, EmbedElement};

const MAX_DETECTED_LINKS: usize = 3;
const EXTENSION_ENRICH_TIMEOUT: Duration = Duration::from_secs(5);
const GITHUB_NAMESPACE: &str = "urn:waddle:github:0";

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
    actor_namespaces: HashSet<String>,
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
                actor_namespaces: HashSet::new(),
            });
        }

        let runtime = WasmRuntime::new()?;
        let puller = OciExtensionPuller::new(&config.cache_dir);
        let mut actors = Vec::new();
        let mut feature_namespaces = Vec::new();
        let mut actor_namespaces = HashSet::new();

        for module in &config.modules {
            // Advertise the configured namespace unconditionally, even if the
            // WASM component fails to load. This keeps disco#info deterministic
            // across deployments and lets clients send payloads in the
            // advertised namespace regardless of runtime load status.
            if !module.namespace.is_empty() && !feature_namespaces.contains(&module.namespace) {
                feature_namespaces.push(module.namespace.clone());
            }

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
                        "failed to resolve extension WASM path; skipping enrichment actor"
                    );
                    continue;
                }
            };

            let loaded = match LoadedExtension::load(&runtime, &wasm_path) {
                Ok(loaded) => loaded,
                Err(error) => {
                    warn!(
                        extension = %module.name,
                        %error,
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
                        "extension init() failed; skipping enrichment actor"
                    );
                    continue;
                }
            };

            let info = actor.info();
            if !info.namespace.is_empty() && !feature_namespaces.contains(&info.namespace) {
                feature_namespaces.push(info.namespace.clone());
            }
            if !info.namespace.is_empty() {
                actor_namespaces.insert(info.namespace.clone());
            }
            for feature in &info.features {
                if !feature_namespaces.contains(&feature.namespace) {
                    feature_namespaces.push(feature.namespace.clone());
                }
                actor_namespaces.insert(feature.namespace.clone());
            }

            actors.push(Arc::new(actor));
        }

        Ok(Self {
            actors,
            feature_namespaces,
            actor_namespaces,
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
        if self
            .feature_namespaces
            .iter()
            .any(|namespace| namespace == GITHUB_NAMESPACE)
        {
            retain_valid_github_payloads(msg);
        }

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
            if self.actor_namespaces.contains(GITHUB_NAMESPACE)
                && message_has_valid_github_embed(msg)
            {
                return count;
            }
        }

        if self
            .feature_namespaces
            .iter()
            .any(|namespace| namespace == GITHUB_NAMESPACE)
        {
            for embed in github_embeds_from_links(&links) {
                msg.payloads.push(embed.to_minidom());
                count += 1;
            }
        }
        if count > 0 {
            debug!(
                embeds_added = count,
                namespace = GITHUB_NAMESPACE,
                "message enriched by built-in GitHub fallback"
            );
        }
        count
    }
}

pub fn message_has_valid_github_embed(msg: &Message) -> bool {
    msg.payloads.iter().any(is_valid_github_payload)
}

fn retain_valid_github_payloads(msg: &mut Message) {
    msg.payloads
        .retain(|payload| payload.ns() != GITHUB_NAMESPACE || is_valid_github_payload(payload));
}

fn is_valid_github_payload(payload: &Element) -> bool {
    if payload.ns() != GITHUB_NAMESPACE {
        return false;
    }

    let Some(url) = payload.attr("url") else {
        return false;
    };
    let Some(embed) = github_embed_from_url(url) else {
        return false;
    };

    payload.name() == embed.element_name
        && github_attribute_matches(payload, &embed, "url")
        && github_attribute_matches(payload, &embed, "owner")
        && github_attribute_matches(payload, &embed, "name")
}

fn github_attribute_matches(payload: &Element, embed: &EmbedElement, name: &str) -> bool {
    let Some(expected) = embed
        .attributes
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value.as_str()))
    else {
        return false;
    };
    payload.attr(name) == Some(expected)
}

fn github_embeds_from_links(links: &[DetectedLink]) -> Vec<EmbedElement> {
    let mut seen = HashSet::new();
    links
        .iter()
        .filter_map(|link| {
            let url = normalize_github_url(&link.url)?;
            if !seen.insert(url.clone()) {
                return None;
            }
            github_embed_from_normalized_url(url)
        })
        .collect()
}

fn github_embed_from_url(raw_url: &str) -> Option<EmbedElement> {
    let url = normalize_github_url(raw_url)?;
    github_embed_from_normalized_url(url)
}

fn github_embed_from_normalized_url(url: String) -> Option<EmbedElement> {
    let path = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 2 && parts.len() != 4 {
        return None;
    }

    let owner = parts[0];
    let name = parts[1];
    if owner.is_empty() || name.is_empty() {
        return None;
    }

    let element_name = match parts.as_slice() {
        [_, _] => "repo",
        [_, _, "issues", number] if is_decimal(number) => "issue",
        [_, _, "pull", number] if is_decimal(number) => "pr",
        _ => return None,
    };

    Some(EmbedElement {
        element_name: element_name.to_string(),
        namespace: GITHUB_NAMESPACE.to_string(),
        attributes: vec![
            ("url".to_string(), url.clone()),
            ("owner".to_string(), owner.to_string()),
            ("name".to_string(), name.to_string()),
        ],
        children: Vec::new(),
    })
}

fn normalize_github_url(raw_url: &str) -> Option<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^https?://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(?:/(issues|pull)/\d+)?$")
            .expect("valid regex")
    });

    if !re.is_match(raw_url) {
        return None;
    }

    Some(match raw_url.strip_prefix("http://") {
        Some(path) => format!("https://{path}"),
        None => raw_url.to_string(),
    })
}

fn is_decimal(value: &str) -> bool {
    !value.is_empty() && value.as_bytes().iter().all(u8::is_ascii_digit)
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use minidom::Element;
    use serde_json::json;
    use xmpp_parsers::message::{Body, Message};

    use super::{
        detect_links, effective_module_config_json, effective_module_config_with_reader,
        github_embed_from_url, EffectiveModuleConfigError, ExtensionManager, GITHUB_NAMESPACE,
        MAX_DETECTED_LINKS,
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
    async fn from_config_uses_github_fallback_when_actor_cannot_load() {
        let mut config_secret_files = BTreeMap::new();
        config_secret_files.insert(
            "github_token".to_string(),
            "/path/that/does/not/exist".to_string(),
        );

        let config = ExtensionConfig {
            enabled: true,
            cache_dir: "/var/lib/waddle/extensions".to_string(),
            modules: vec![ExtensionModuleConfig {
                name: "github-enricher".to_string(),
                registry: "ghcr.io/waddle-social/waddle/extensions/github-enricher".to_string(),
                tag: "latest".to_string(),
                namespace: "urn:waddle:github:0".to_string(),
                config: json!({}),
                config_secret_files,
                local_path: Some("missing-but-never-read.wasm".to_string()),
            }],
        };

        let manager = ExtensionManager::from_config(config)
            .await
            .expect("manager should stay fail-open");
        assert_eq!(manager.feature_namespaces(), [GITHUB_NAMESPACE]);

        let mut msg = Message::new(None);
        msg.bodies.insert(
            String::new(),
            Body("https://github.com/waddle-social/waddle".to_string()),
        );
        assert_eq!(manager.enrich_message(&mut msg).await, 1);
        assert_eq!(msg.payloads[0].name(), "repo");
        assert_eq!(msg.payloads[0].ns(), GITHUB_NAMESPACE);
        assert_eq!(
            msg.payloads[0].attr("url"),
            Some("https://github.com/waddle-social/waddle")
        );
    }

    #[tokio::test]
    async fn enrich_message_does_not_fallback_without_github_namespace() {
        let manager = ExtensionManager {
            actors: Vec::new(),
            feature_namespaces: Vec::new(),
            actor_namespaces: HashSet::new(),
        };

        let mut msg = Message::new(None);
        msg.bodies.insert(
            String::new(),
            Body("https://github.com/waddle-social/waddle".to_string()),
        );

        assert_eq!(manager.enrich_message(&mut msg).await, 0);
        assert!(msg.payloads.is_empty());
    }

    #[tokio::test]
    async fn github_fallback_deduplicates_after_url_normalization() {
        let manager = ExtensionManager {
            actors: Vec::new(),
            feature_namespaces: vec![GITHUB_NAMESPACE.to_string()],
            actor_namespaces: HashSet::new(),
        };

        let mut msg = Message::new(None);
        msg.bodies.insert(
            String::new(),
            Body(
                "http://github.com/waddle-social/waddle https://github.com/waddle-social/waddle"
                    .to_string(),
            ),
        );

        assert_eq!(manager.enrich_message(&mut msg).await, 1);
        assert_eq!(
            msg.payloads[0].attr("url"),
            Some("https://github.com/waddle-social/waddle")
        );
    }

    #[tokio::test]
    async fn github_fallback_removes_invalid_client_supplied_payloads() {
        let manager = ExtensionManager {
            actors: Vec::new(),
            feature_namespaces: vec![GITHUB_NAMESPACE.to_string()],
            actor_namespaces: HashSet::new(),
        };

        let mut msg = Message::new(None);
        msg.bodies.insert(
            String::new(),
            Body("https://github.com/waddle-social/waddle".to_string()),
        );
        msg.payloads.push(
            Element::builder("repo", GITHUB_NAMESPACE)
                .attr("url", "javascript:alert(1)")
                .attr("owner", "waddle-social")
                .attr("name", "waddle")
                .build(),
        );

        assert_eq!(manager.enrich_message(&mut msg).await, 1);
        assert_eq!(msg.payloads.len(), 1);
        assert_eq!(
            msg.payloads[0].attr("url"),
            Some("https://github.com/waddle-social/waddle")
        );
    }

    #[tokio::test]
    async fn github_fallback_preserves_valid_client_supplied_payloads() {
        let manager = ExtensionManager {
            actors: Vec::new(),
            feature_namespaces: vec![GITHUB_NAMESPACE.to_string()],
            actor_namespaces: HashSet::new(),
        };

        let mut msg = Message::new(None);
        msg.bodies.insert(
            String::new(),
            Body("https://github.com/waddle-social/waddle".to_string()),
        );
        msg.payloads.push(
            Element::builder("repo", GITHUB_NAMESPACE)
                .attr("url", "https://github.com/waddle-social/waddle")
                .attr("owner", "waddle-social")
                .attr("name", "waddle")
                .build(),
        );

        assert_eq!(manager.enrich_message(&mut msg).await, 0);
        assert_eq!(msg.payloads.len(), 1);
    }

    #[test]
    fn github_fallback_builds_repo_issue_and_pr_embeds() {
        let repo = github_embed_from_url("https://github.com/waddle-social/waddle")
            .expect("repo should parse");
        let issue = github_embed_from_url("https://github.com/waddle-social/waddle/issues/42")
            .expect("issue should parse");
        let pr = github_embed_from_url("http://github.com/waddle-social/waddle/pull/48")
            .expect("pull request should parse");

        assert_eq!(repo.element_name, "repo");
        assert_eq!(issue.element_name, "issue");
        assert_eq!(pr.element_name, "pr");
        assert_eq!(
            pr.attributes[0],
            (
                "url".to_string(),
                "https://github.com/waddle-social/waddle/pull/48".to_string()
            )
        );
    }

    #[test]
    fn github_fallback_rejects_spoofed_or_malformed_urls() {
        assert!(
            github_embed_from_url("https://github.com.evil.test/waddle-social/waddle").is_none()
        );
        assert!(github_embed_from_url("https://evil.test/waddle-social/waddle").is_none());
        assert!(github_embed_from_url("javascript:alert(1)").is_none());
        assert!(
            github_embed_from_url("https://github.com/waddle-social/waddle?tab=readme").is_none()
        );
        assert!(github_embed_from_url("https://github.com/waddle-social/waddle#readme").is_none());
        assert!(
            github_embed_from_url("https://github.com/waddle-social/waddle/pull/not-a-number")
                .is_none()
        );
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
