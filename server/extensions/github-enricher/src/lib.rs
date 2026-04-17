mod cache;
mod client;
mod detect;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionConfig {
    pub github_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedLink {
    pub url: String,
    pub start_offset: u32,
    pub end_offset: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedElement {
    pub element_name: String,
    pub namespace: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentResult {
    pub embeds: Vec<EmbedElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureAdvertisement {
    pub namespace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionInfo {
    pub name: String,
    pub namespace: String,
    pub version: String,
    pub features: Vec<FeatureAdvertisement>,
}

pub fn init(config: &str) -> Result<ExtensionInfo, String> {
    let _: ExtensionConfig = if config.trim().is_empty() {
        ExtensionConfig { github_token: None }
    } else {
        serde_json::from_str(config).map_err(|e| e.to_string())?
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

pub fn enrich_message(body: &str, links: &[DetectedLink]) -> EnrichmentResult {
    let github_links = if links.is_empty() {
        detect::github_links(body)
    } else {
        links
            .iter()
            .map(|link| link.url.clone())
            .filter(|url| url.contains("github.com/"))
            .collect()
    };

    EnrichmentResult {
        embeds: github_links
        .into_iter()
        .map(|link| client::build_repo_embed(&link))
        .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{enrich_message, init, DetectedLink};

    #[test]
    fn init_accepts_empty_json() {
        let initialized = init("{}").expect("init should work");
        assert_eq!(initialized.name, "github-enricher");
        assert_eq!(initialized.namespace, "urn:waddle:github:0");
    }

    #[test]
    fn enrich_returns_embeds_for_github_links() {
        let result = enrich_message(
            "https://github.com/waddle-social/waddle",
            &[DetectedLink {
                url: "https://github.com/waddle-social/waddle".to_string(),
                start_offset: 0,
                end_offset: 39,
            }],
        );
        assert_eq!(result.embeds.len(), 1);
        assert_eq!(result.embeds[0].namespace, "urn:waddle:github:0");
    }
}
