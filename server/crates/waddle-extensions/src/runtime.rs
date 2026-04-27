use std::path::Path;

use anyhow::{Context, Result};
use tracing::{debug, error, info, trace, warn};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::types::{DetectedLink, EmbedElement, ExtensionInfo, FeatureAdvertisement};

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "waddle-extension",
    imports: { default: tracing | trappable },
    exports: { default: async },
    with: {
        "wasi:io": wasmtime_wasi::p2::bindings::io,
        "wasi:clocks": wasmtime_wasi::p2::bindings::clocks,
    },
});

use self::exports::waddle::extension as wit_exports;
use self::waddle::extension::types as wit_types;
use self::wasi::logging::logging::{Host as LoggingHost, Level as LogLevel};

/// Host state made available to every WASM instance for satisfying WASI imports.
pub struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl HostState {
    fn new() -> Self {
        let wasi = WasiCtxBuilder::new().inherit_stderr().build();
        Self {
            wasi,
            table: ResourceTable::new(),
        }
    }
}

impl Default for HostState {
    fn default() -> Self {
        Self::new()
    }
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl LoggingHost for HostState {
    fn log(&mut self, level: LogLevel, context: String, message: String) -> wasmtime::Result<()> {
        let context_display = if context.is_empty() {
            "waddle-extension".to_string()
        } else {
            context
        };
        match level {
            LogLevel::Trace => {
                trace!(target: "waddle::extension", extension = %context_display, "{}", message)
            }
            LogLevel::Debug => {
                debug!(target: "waddle::extension", extension = %context_display, "{}", message)
            }
            LogLevel::Info => {
                info!(target: "waddle::extension", extension = %context_display, "{}", message)
            }
            LogLevel::Warn => {
                warn!(target: "waddle::extension", extension = %context_display, "{}", message)
            }
            LogLevel::Error | LogLevel::Critical => {
                error!(target: "waddle::extension", extension = %context_display, "{}", message)
            }
        }
        Ok(())
    }
}

/// Shared wasmtime engine used for all loaded extensions.
#[derive(Clone, Debug)]
pub struct WasmRuntime {
    engine: Engine,
}

impl WasmRuntime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)
            .map_err(anyhow::Error::from)
            .context("failed to create wasmtime engine")?;
        Ok(Self { engine })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

/// A compiled WASM component ready for repeated invocation.
pub struct LoadedExtension {
    engine: Engine,
    component: Component,
    linker: Linker<HostState>,
}

impl std::fmt::Debug for LoadedExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedExtension").finish()
    }
}

impl LoadedExtension {
    pub fn load(runtime: &WasmRuntime, wasm_path: &Path) -> Result<Self> {
        let engine = runtime.engine().clone();
        let component = Component::from_file(&engine, wasm_path)
            .map_err(anyhow::Error::from)
            .with_context(|| {
                format!("failed to load WASM component from {}", wasm_path.display())
            })?;

        let mut linker = Linker::<HostState>::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(anyhow::Error::from)
            .context("failed to add wasi linker imports")?;
        wasi::logging::logging::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(anyhow::Error::from)
            .context("failed to add wasi:logging linker imports")?;

        Ok(Self {
            engine,
            component,
            linker,
        })
    }

    pub async fn call_init(&self, config: &str) -> Result<ExtensionInfo> {
        let mut store = Store::new(&self.engine, HostState::new());
        let bindings: WaddleExtension =
            WaddleExtension::instantiate_async(&mut store, &self.component, &self.linker)
                .await
                .map_err(anyhow::Error::from)
                .context("failed to instantiate WASM component")?;

        let result: std::result::Result<wit_exports::lifecycle::ExtensionInfo, String> = bindings
            .waddle_extension_lifecycle()
            .call_init(&mut store, config)
            .await
            .map_err(anyhow::Error::from)
            .context("wasm init() call trapped")?;

        match result {
            Ok(info) => Ok(ExtensionInfo::from(info)),
            Err(message) => Err(anyhow::anyhow!("extension init failed: {message}")),
        }
    }

    pub async fn call_enrich_message(
        &self,
        body: String,
        links: Vec<DetectedLink>,
    ) -> Result<Vec<EmbedElement>> {
        let mut store = Store::new(&self.engine, HostState::new());
        let bindings: WaddleExtension =
            WaddleExtension::instantiate_async(&mut store, &self.component, &self.linker)
                .await
                .map_err(anyhow::Error::from)
                .context("failed to instantiate WASM component")?;

        let wit_links: Vec<wit_types::DetectedLink> = links.into_iter().map(Into::into).collect();

        let result: wit_exports::enrich::EnrichmentResult = bindings
            .waddle_extension_enrich()
            .call_enrich_message(&mut store, &body, &wit_links)
            .await
            .map_err(anyhow::Error::from)
            .context("wasm enrich-message() call trapped")?;

        Ok(result.embeds.into_iter().map(EmbedElement::from).collect())
    }
}

// ---- type conversions between WIT-generated types and domain types ----

impl From<DetectedLink> for wit_types::DetectedLink {
    fn from(value: DetectedLink) -> Self {
        Self {
            url: value.url,
            start_offset: value.start_offset,
            end_offset: value.end_offset,
        }
    }
}

impl From<wit_types::EmbedElement> for EmbedElement {
    fn from(value: wit_types::EmbedElement) -> Self {
        Self {
            element_name: value.element_name,
            namespace: value.namespace,
            attributes: value.attributes,
        }
    }
}

impl From<wit_exports::lifecycle::ExtensionInfo> for ExtensionInfo {
    fn from(value: wit_exports::lifecycle::ExtensionInfo) -> Self {
        Self {
            name: value.name,
            namespace: value.namespace,
            version: value.version,
            features: value
                .features
                .into_iter()
                .map(|feat| FeatureAdvertisement {
                    namespace: feat.namespace,
                })
                .collect(),
        }
    }
}
