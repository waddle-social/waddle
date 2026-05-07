use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store};

use super::exports::waddle::extension as wit_exports;
use super::{waddle, wasi, HostState, WaddleExtension};
use crate::host_tools::{ExtensionHostTools, InvocationContext};
use crate::types::{ExtensionCapability, ExtensionEvent, ExtensionManifest, ExtensionResponse};

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
        waddle::extension::host_tools::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(anyhow::Error::from)
            .context("failed to add waddle host tool linker imports")?;
        waddle::extension::runtime::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(anyhow::Error::from)
            .context("failed to add waddle runtime linker imports")?;

        Ok(Self {
            engine,
            component,
            linker,
        })
    }

    pub async fn call_init(&self, config: &str) -> Result<ExtensionManifest> {
        let mut store = Store::new(&self.engine, HostState::for_init());
        let bindings: WaddleExtension =
            WaddleExtension::instantiate_async(&mut store, &self.component, &self.linker)
                .await
                .map_err(anyhow::Error::from)
                .context("failed to instantiate WASM component")?;

        let result: std::result::Result<wit_exports::lifecycle::ExtensionManifest, String> =
            bindings
                .waddle_extension_lifecycle()
                .call_init(&mut store, config)
                .await
                .map_err(anyhow::Error::from)
                .context("wasm init() call trapped")?;

        match result {
            Ok(manifest) => manifest.try_into(),
            Err(message) => Err(anyhow::anyhow!("extension init failed: {message}")),
        }
    }

    pub async fn call_handle_event(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        context: InvocationContext,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
    ) -> Result<ExtensionResponse> {
        let mut store = Store::new(
            &self.engine,
            HostState::new(tools, context, config, grants, allowed_http_origins),
        );
        let bindings: WaddleExtension =
            WaddleExtension::instantiate_async(&mut store, &self.component, &self.linker)
                .await
                .map_err(anyhow::Error::from)
                .context("failed to instantiate WASM component")?;

        let result = bindings
            .waddle_extension_framework()
            .call_handle_event(&mut store, &event.into())
            .await
            .map_err(anyhow::Error::from)
            .context("wasm handle-event() call trapped")?;

        match result {
            Ok(response) => response.try_into(),
            Err(error) => Err(anyhow::anyhow!(
                "extension handle-event failed: {:?}: {}",
                error.code,
                error.message.value
            )),
        }
    }
}
