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
    http: super::http::HttpRuntime,
}

impl WasmRuntime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        let engine = Engine::new(&config)
            .map_err(anyhow::Error::from)
            .context("failed to create wasmtime engine")?;
        Ok(Self {
            engine,
            http: super::http::HttpRuntime::new()?,
        })
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
    http: super::http::HttpRuntime,
    limits: crate::config::RuntimeLimits,
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
            http: runtime.http.clone(),
            limits: crate::config::RuntimeLimits::default(),
        })
    }

    pub fn with_limits(mut self, limits: crate::config::RuntimeLimits) -> Result<Self> {
        limits.validate().map_err(anyhow::Error::msg)?;
        self.limits = limits;
        Ok(self)
    }

    fn configure_store(&self, store: &mut Store<HostState>) -> Result<()> {
        store.limiter(|state| &mut state.limits);
        store.set_fuel(self.limits.wasm_fuel)?;
        store.fuel_async_yield_interval(Some(10_000))?;
        Ok(())
    }

    pub async fn call_init(&self, config: &str) -> Result<ExtensionManifest> {
        tokio::time::timeout(
            std::time::Duration::from_millis(u64::from(self.limits.invocation_timeout_ms)),
            self.invoke_init(config),
        )
        .await
        .context("extension init deadline exceeded")?
    }

    async fn invoke_init(&self, config: &str) -> Result<ExtensionManifest> {
        let mut store = Store::new(
            &self.engine,
            HostState::for_init_with(self.limits.clone(), self.http.clone()),
        );
        self.configure_store(&mut store)?;
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
        self.call_handle_event_typed(event, tools, context, config, grants, allowed_http_origins)
            .await
            .map_err(|error| anyhow::anyhow!("extension invocation failed: {error:?}"))
    }

    pub async fn call_handle_event_typed(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        context: InvocationContext,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
    ) -> std::result::Result<ExtensionResponse, crate::types::ObservationFailure> {
        tokio::time::timeout(
            std::time::Duration::from_millis(u64::from(self.limits.invocation_timeout_ms)),
            self.invoke(event, tools, context, config, grants, allowed_http_origins),
        )
        .await
        .map_err(|_| crate::types::ObservationFailure::DeadlineExceeded)?
    }

    async fn invoke(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        context: InvocationContext,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
    ) -> std::result::Result<ExtensionResponse, crate::types::ObservationFailure> {
        use crate::types::ObservationFailure;
        let mut store = Store::new(
            &self.engine,
            HostState::new(
                tools,
                context,
                config,
                grants,
                allowed_http_origins,
                self.limits.clone(),
                self.http.clone(),
            ),
        );
        self.configure_store(&mut store)
            .map_err(|_| ObservationFailure::ResourceLimit)?;
        let bindings =
            WaddleExtension::instantiate_async(&mut store, &self.component, &self.linker)
                .await
                .map_err(classify_runtime_error)?;
        let result = bindings
            .waddle_extension_framework()
            .call_handle_event(&mut store, &event.into())
            .await
            .map_err(classify_runtime_error)?;
        match result {
            Ok(response) => response
                .try_into()
                .map_err(|_| ObservationFailure::InvalidResult),
            Err(error) => Err(match error.code {
                super::waddle::extension::types::ExtensionErrorCode::TemporaryFailure => {
                    ObservationFailure::TemporaryFailure
                }
                super::waddle::extension::types::ExtensionErrorCode::Denied => {
                    ObservationFailure::Denied
                }
                super::waddle::extension::types::ExtensionErrorCode::InvalidRequest => {
                    ObservationFailure::InvalidRequest
                }
                super::waddle::extension::types::ExtensionErrorCode::UnsupportedEvent => {
                    ObservationFailure::UnsupportedEvent
                }
            }),
        }
    }
}

fn classify_runtime_error(error: wasmtime::Error) -> crate::types::ObservationFailure {
    if matches!(
        error.downcast_ref::<wasmtime::Trap>(),
        Some(wasmtime::Trap::OutOfFuel)
    ) {
        crate::types::ObservationFailure::ResourceLimit
    } else {
        crate::types::ObservationFailure::RuntimeFailure
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[tokio::test]
    async fn spinning_wasm_exhausts_fuel_without_blocking_the_executor() {
        let runtime = WasmRuntime::new().expect("runtime");
        let module = wasmtime::Module::new(
            runtime.engine(),
            r#"(module (func (export "run") (loop br 0)))"#,
        )
        .expect("module");
        let mut store = Store::new(runtime.engine(), HostState::for_init());
        store.set_fuel(20_000).expect("fuel");
        store.fuel_async_yield_interval(Some(1_000)).expect("yield");
        let instance = wasmtime::Instance::new_async(&mut store, &module, &[])
            .await
            .expect("instance");
        let run = instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("run");
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run.call_async(&mut store, ()),
        )
        .await
        .expect("guest must yield and terminate")
        .expect_err("fuel exhausted");
        assert_eq!(
            classify_runtime_error(error),
            crate::types::ObservationFailure::ResourceLimit
        );
    }

    #[tokio::test]
    async fn wasm_memory_growth_is_rejected_at_the_store_limit() {
        let runtime = WasmRuntime::new().expect("runtime");
        let module = wasmtime::Module::new(
            runtime.engine(),
            r#"(module
            (memory 1) (func (export "grow") (result i32) i32.const 1 memory.grow))"#,
        )
        .expect("module");
        let mut state = HostState::for_init();
        state.limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(65_536)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(runtime.engine(), state);
        store.limiter(|state| &mut state.limits);
        store.set_fuel(10_000).expect("fuel");
        let instance = wasmtime::Instance::new_async(&mut store, &module, &[])
            .await
            .expect("instance");
        let grow = instance
            .get_typed_func::<(), i32>(&mut store, "grow")
            .expect("grow");
        assert!(grow.call_async(&mut store, ()).await.is_err());
    }
}
