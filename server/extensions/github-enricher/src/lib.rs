mod client;
mod detect;

wit_bindgen::generate!({
    path: "../../wit",
    world: "waddle-extension",
    generate_all,
});

use exports::waddle::extension::enrich::{
    DetectedLink as WitDetectedLink, EnrichmentResult, Guest as EnrichGuest,
};
use exports::waddle::extension::lifecycle::{ExtensionInfo, Guest as LifecycleGuest};
use serde::{Deserialize, Serialize};

pub use waddle::extension::types::{EmbedElement, FeatureAdvertisement};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ExtensionConfig {
    github_token: Option<String>,
}

struct Extension;

impl LifecycleGuest for Extension {
    fn init(config: String) -> Result<ExtensionInfo, String> {
        let _: ExtensionConfig = if config.trim().is_empty() {
            ExtensionConfig::default()
        } else {
            serde_json::from_str(&config).map_err(|err| err.to_string())?
        };

        Ok(ExtensionInfo {
            name: "github-enricher".to_string(),
            namespace: "urn:waddle:github:0".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![FeatureAdvertisement {
                namespace: "urn:waddle:github:0".to_string(),
            }],
        })
    }
}

impl EnrichGuest for Extension {
    fn enrich_message(body: String, links: Vec<WitDetectedLink>) -> EnrichmentResult {
        let github_links: Vec<String> = if links.is_empty() {
            detect::github_links(&body)
        } else {
            links
                .into_iter()
                .map(|link| link.url)
                .filter_map(|url| detect::normalize_github_url(&url))
                .collect()
        };

        EnrichmentResult {
            embeds: github_links
                .iter()
                .map(|url| client::build_repo_embed(url))
                .collect(),
        }
    }
}

export!(Extension);

#[cfg(test)]
mod tests {
    use super::{EnrichGuest, Extension, LifecycleGuest, WitDetectedLink};

    #[test]
    fn init_accepts_empty_json() {
        let info = Extension::init("{}".to_string()).expect("init should succeed");
        assert_eq!(info.name, "github-enricher");
        assert_eq!(info.namespace, "urn:waddle:github:0");
    }

    #[test]
    fn enrich_message_normalizes_detected_http_github_links() {
        let result = Extension::enrich_message(
            String::new(),
            vec![WitDetectedLink {
                url: "http://github.com/waddle-social/waddle".to_string(),
                start_offset: 0,
                end_offset: 37,
            }],
        );

        assert_eq!(result.embeds.len(), 1);
        assert_eq!(
            result.embeds[0].attributes[0].1,
            "https://github.com/waddle-social/waddle"
        );
    }

    #[test]
    fn enrich_message_rejects_non_github_detected_links() {
        let result = Extension::enrich_message(
            String::new(),
            vec![WitDetectedLink {
                url: "https://evil.example/github.com/waddle-social/waddle".to_string(),
                start_offset: 0,
                end_offset: 51,
            }],
        );

        assert!(result.embeds.is_empty());
    }
}
