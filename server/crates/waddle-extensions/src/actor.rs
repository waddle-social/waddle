use std::sync::Arc;

use anyhow::Result;
use tracing::warn;

use crate::runtime::LoadedExtension;
use crate::types::{DisplayText, ExtensionEffect, ExtensionEvent, ExtensionManifest};

/// An extension loaded into wasmtime and ready to handle typed framework events.
#[derive(Debug)]
pub struct WasmExtensionActor {
    manifest: ExtensionManifest,
    extension: Arc<LoadedExtension>,
}

impl WasmExtensionActor {
    /// Load the extension and run its lifecycle `init` to obtain the extension info.
    ///
    /// `config` is the JSON config payload to forward to the guest's `init` function.
    pub async fn initialize(extension: LoadedExtension, config: &str) -> Result<Self> {
        let manifest = extension.call_init(config).await?;
        Ok(Self {
            manifest,
            extension: Arc::new(extension),
        })
    }

    pub fn manifest(&self) -> ExtensionManifest {
        self.manifest.clone()
    }

    pub async fn handle_event(&self, event: ExtensionEvent) -> Vec<ExtensionEffect> {
        match self.extension.call_handle_event(event).await {
            Ok(response) => response
                .effects
                .into_iter()
                .filter(|effect| effect.validate_for_manifest(&self.manifest))
                .collect(),
            Err(error) => {
                let message = format!("Extension {} failed: {error}", self.manifest.id);
                warn!(
                    extension = %self.manifest.id,
                    %error,
                    "extension handle-event failed; continuing fail-open"
                );
                vec![ExtensionEffect::HostWarning(
                    DisplayText::new(message).expect("host warning is non-empty"),
                )]
            }
        }
    }
}
