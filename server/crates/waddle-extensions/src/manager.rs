use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::future::join_all;
use regex::Regex;
use std::collections::HashSet;
use tokio::time::timeout;
use tracing::{debug, warn};
use xmpp_parsers::message::Message;

use crate::actor::WasmExtensionActor;
use crate::config::ExtensionConfig;
use crate::oci::OciExtensionPuller;
use crate::runtime::{LoadedExtension, WasmRuntime};
use crate::types::{message_has_embed_for_namespaces, DetectedLink};

const MAX_DETECTED_LINKS: usize = 3;
const EXTENSION_ENRICH_TIMEOUT: Duration = Duration::from_secs(5);

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
            // Advertise the configured namespace unconditionally, even if the
            // WASM component fails to load. This keeps disco#info deterministic
            // across deployments and lets clients send payloads in the
            // advertised namespace regardless of runtime load status.
            if !module.namespace.is_empty() && !feature_namespaces.contains(&module.namespace) {
                feature_namespaces.push(module.namespace.clone());
            }

            let wasm_path = match puller.resolve_wasm_path(module) {
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

            let config_json = module.config.to_string();
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
        if self.actors.is_empty() {
            return 0;
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

        let enrich_futures = self.actors.iter().map(|actor| {
            let actor_name = actor.info().name;
            let actor = Arc::clone(actor);
            let body = body.clone();
            let links = links.clone();
            async move {
                match timeout(EXTENSION_ENRICH_TIMEOUT, actor.enrich_message(body, links)).await {
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

        let mut count = 0usize;
        for embeds in results {
            for embed in embeds {
                msg.payloads.push(embed.to_minidom());
                count += 1;
            }
        }
        if count > 0 {
            debug!(embeds_added = count, "message enriched by extensions");
        }

        count
    }
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
    use super::{detect_links, MAX_DETECTED_LINKS};

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
}
