use crate::types::{DetectedLink, EmbedElement, ExtensionInfo};

#[derive(Debug, Clone)]
pub struct WasmExtensionActor {
    info: ExtensionInfo,
}

impl WasmExtensionActor {
    pub fn new(info: ExtensionInfo) -> Self {
        Self { info }
    }

    pub fn info(&self) -> ExtensionInfo {
        self.info.clone()
    }

    pub async fn enrich_message(
        &self,
        _body: String,
        _links: Vec<DetectedLink>,
    ) -> Vec<EmbedElement> {
        // TODO(phase2): Wire wasmtime runtime to execute extension WASM modules via WIT contract.
        Vec::new()
    }
}
