use std::sync::Arc;

use anyhow::Result;
use tracing::warn;

use crate::runtime::LoadedExtension;
use crate::types::{DetectedLink, EmbedElement, ExtensionInfo};

/// An extension loaded into wasmtime and ready to handle enrichment requests.
#[derive(Debug)]
pub struct WasmExtensionActor {
    info: ExtensionInfo,
    extension: Arc<LoadedExtension>,
}

impl WasmExtensionActor {
    /// Load the extension and run its lifecycle `init` to obtain the extension info.
    ///
    /// `config` is the JSON config payload to forward to the guest's `init` function.
    pub async fn initialize(extension: LoadedExtension, config: &str) -> Result<Self> {
        let info = extension.call_init(config).await?;
        Ok(Self {
            info,
            extension: Arc::new(extension),
        })
    }

    pub fn info(&self) -> ExtensionInfo {
        self.info.clone()
    }

    pub async fn enrich_message(
        &self,
        body: String,
        links: Vec<DetectedLink>,
    ) -> Vec<EmbedElement> {
        match self.extension.call_enrich_message(body, links).await {
            Ok(embeds) => embeds,
            Err(error) => {
                warn!(
                    extension = %self.info.name,
                    %error,
                    "extension enrich_message failed; continuing fail-open"
                );
                Vec::new()
            }
        }
    }
}
