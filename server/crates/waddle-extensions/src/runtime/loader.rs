//! Compilation, instantiation, and execution of extension Wasm components.

use crate::config::RuntimeLimits;
use crate::host_tools::HostToolError;
use crate::types::ObservationFailure;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store};

use super::exports::waddle::extension as wit_exports;
use super::{waddle, wasi, HostState, WaddleExtension};
use crate::host_tools::{
    DeliveryInvocation, ExtensionDeliveryCapability, ExtensionHostTools, InvocationContext,
};
use crate::types::{ExtensionCapability, ExtensionEvent, ExtensionManifest, ExtensionResponse};
use std::sync::mpsc;
use std::time::Duration;
use waddle_broker_core::quotas::TenantMemoryTracker;

/// Interval at which the Wasmtime engine epoch counter advances.
const EPOCH_TICK_INTERVAL_MS: u64 = 50;

/// Background epoch ticker stopped cleanly when the last [`WasmRuntime`] drops.
struct EpochTickerGuard {
    stop_tx: mpsc::Sender<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for EpochTickerGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EpochTickerGuard").finish_non_exhaustive()
    }
}

impl Drop for EpochTickerGuard {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Shared wasmtime engine used for all loaded extensions.
#[derive(Clone, Debug)]
pub struct WasmRuntime {
    engine: Engine,
    http: super::http::HttpRuntime,
    _epoch_ticker: Arc<EpochTickerGuard>,
}

impl WasmRuntime {
    /// Create the fuel-metered, epoch-interruptible component engine and policy-constrained
    /// HTTP client shared by loaded extensions.
    ///
    /// # Errors
    ///
    /// Returns an error if the Wasmtime engine, epoch ticker thread, or HTTP client cannot
    /// be initialized.
    pub fn try_new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)
            .map_err(anyhow::Error::from)
            .context("failed to create wasmtime engine")?;
        let ticker_engine = engine.clone();
        let (stop_tx, stop_rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("wasm-epoch-ticker".to_owned())
            .spawn(move || {
                let interval = Duration::from_millis(EPOCH_TICK_INTERVAL_MS);
                while let Err(mpsc::RecvTimeoutError::Timeout) = stop_rx.recv_timeout(interval) {
                    ticker_engine.increment_epoch();
                }
            })
            .context("failed to spawn wasm epoch ticker thread")?;
        Ok(Self {
            engine,
            http: super::http::HttpRuntime::try_new()?,
            _epoch_ticker: Arc::new(EpochTickerGuard {
                stop_tx,
                handle: Some(handle),
            }),
        })
    }

    /// Borrow the shared Wasmtime engine configured for components, fuel metering, and
    /// epoch interruption.
    #[must_use]
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
    limits: RuntimeLimits,
}

impl std::fmt::Debug for LoadedExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedExtension").finish()
    }
}

impl LoadedExtension {
    /// Compile a component file and link its WASI, logging, extension host-tool,
    /// delivery, and runtime imports.
    ///
    /// # Errors
    ///
    /// Returns an error if the component cannot be read/compiled or its WASI and
    /// extension host imports cannot be linked.
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
        waddle::extension::delivery::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(anyhow::Error::from)
            .context("failed to add delivery capability linker imports")?;
        waddle::extension::runtime::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(anyhow::Error::from)
            .context("failed to add waddle runtime linker imports")?;

        Ok(Self {
            engine,
            component,
            linker,
            http: runtime.http.clone(),
            limits: RuntimeLimits::default(),
        })
    }

    /// Replace the execution and network resource limits used when creating invocation stores.
    ///
    /// # Errors
    ///
    /// Returns an explanatory error if the limits are outside host-supported bounds.
    pub fn with_limits(mut self, limits: RuntimeLimits) -> Result<Self> {
        limits.validate().map_err(anyhow::Error::msg)?;
        self.limits = limits;
        Ok(self)
    }

    fn configure_store(&self, store: &mut Store<HostState>) -> Result<()> {
        store.limiter(|state| &mut state.limits);
        store.set_fuel(self.limits.wasm_fuel)?;
        store.fuel_async_yield_interval(Some(10_000))?;
        let deadline_ticks = u64::from(self.limits.invocation_timeout_ms)
            .div_ceil(EPOCH_TICK_INTERVAL_MS)
            .max(1);
        store.set_epoch_deadline(deadline_ticks);
        store.epoch_deadline_trap();
        Ok(())
    }

    /// Instantiate a fresh guest store and invoke lifecycle initialization with the
    /// supplied configuration, under the configured deadline and resource limits.
    ///
    /// # Errors
    ///
    /// Returns an error for initialization deadline/resource failures, component
    /// instantiation/traps, a guest-reported error, or an invalid manifest.
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

    /// Instantiate a fresh guest store and dispatch an event with the supplied host
    /// tools, authority, configuration, capability grants, and HTTP origins. Failures
    /// retain diagnostic context.
    ///
    /// # Errors
    ///
    /// Returns an invocation failure for deadline/resource exhaustion, runtime traps,
    /// rejected host access, guest-reported failures, or invalid guest response values.
    pub async fn call_handle_event(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        context: InvocationContext,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
    ) -> Result<ExtensionResponse> {
        self.call_handle_event_with_memory_tracker(
            event,
            tools,
            context,
            config,
            grants,
            allowed_http_origins,
            None,
        )
        .await
    }

    /// Same as [`Self::call_handle_event`], charging linear memory growth against an
    /// optional per-tenant [`TenantMemoryTracker`].
    ///
    /// # Errors
    ///
    /// Returns an invocation failure for deadline/resource exhaustion, runtime traps,
    /// rejected host access, guest-reported failures, or invalid guest response values.
    #[allow(clippy::too_many_arguments)]
    pub async fn call_handle_event_with_memory_tracker(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        context: InvocationContext,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
        tenant_memory_tracker: Option<Arc<TenantMemoryTracker>>,
    ) -> Result<ExtensionResponse> {
        self.invoke_with_deadline(
            event,
            tools,
            InvocationBinding {
                context,
                delivery: None,
                tenant_memory_tracker,
            },
            config,
            grants,
            allowed_http_origins,
        )
        .await
        .map_err(|(failure, message)| match message {
            Some(message) => {
                anyhow::anyhow!("extension invocation failed: {failure:?}: {message}")
            }
            None => anyhow::anyhow!("extension invocation failed: {failure:?}"),
        })
    }

    /// Dispatch an event in a fresh bounded store, returning a stable observation failure
    /// category rather than a diagnostic string.
    ///
    /// # Errors
    ///
    /// Returns an invocation failure for deadline/resource exhaustion, runtime traps,
    /// rejected host access, guest-reported failures, or invalid guest response values.
    pub async fn call_handle_event_typed(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        context: InvocationContext,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
    ) -> std::result::Result<ExtensionResponse, ObservationFailure> {
        self.invoke_with_deadline(
            event,
            tools,
            InvocationBinding {
                context,
                delivery: None,
                tenant_memory_tracker: None,
            },
            config,
            grants,
            allowed_http_origins,
        )
        .await
        .map_err(|(failure, _)| failure)
    }

    /// Invoke with server-owned authority, without serializing the canonical key.
    ///
    /// # Errors
    ///
    /// Returns an invocation failure for deadline/resource exhaustion, runtime traps,
    /// rejected host access, guest-reported failures, or invalid guest response values.
    pub async fn call_handle_event_typed_with_delivery(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        invocation: DeliveryInvocation,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
    ) -> std::result::Result<ExtensionResponse, ObservationFailure> {
        self.call_handle_event_typed_with_delivery_and_tracker(
            event,
            tools,
            invocation,
            config,
            grants,
            allowed_http_origins,
            None,
        )
        .await
    }

    /// Invoke with server-owned authority and optional per-tenant memory tracking.
    ///
    /// # Errors
    ///
    /// Returns an invocation failure for deadline/resource exhaustion, runtime traps,
    /// rejected host access, guest-reported failures, or invalid guest response values.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call_handle_event_typed_with_delivery_and_tracker(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        invocation: DeliveryInvocation,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
        tenant_memory_tracker: Option<Arc<TenantMemoryTracker>>,
    ) -> std::result::Result<ExtensionResponse, ObservationFailure> {
        self.invoke_with_deadline(
            event,
            tools,
            InvocationBinding {
                context: invocation.context,
                delivery: Some(invocation.capability),
                tenant_memory_tracker,
            },
            config,
            grants,
            allowed_http_origins,
        )
        .await
        .map_err(|(failure, _)| failure)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call_handle_event_typed_with_tracker(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        context: InvocationContext,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
        tenant_memory_tracker: Option<Arc<TenantMemoryTracker>>,
    ) -> std::result::Result<ExtensionResponse, ObservationFailure> {
        self.invoke_with_deadline(
            event,
            tools,
            InvocationBinding {
                context,
                delivery: None,
                tenant_memory_tracker,
            },
            config,
            grants,
            allowed_http_origins,
        )
        .await
        .map_err(|(failure, _)| failure)
    }

    /// Like [`Self::call_handle_event_typed`], but keeps the guest's error
    /// message so command callers can surface why an invocation failed.
    async fn invoke_with_deadline(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        invocation: InvocationBinding,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
    ) -> std::result::Result<ExtensionResponse, (ObservationFailure, Option<String>)> {
        tokio::time::timeout(
            std::time::Duration::from_millis(u64::from(self.limits.invocation_timeout_ms)),
            self.invoke(
                event,
                tools,
                invocation,
                config,
                grants,
                allowed_http_origins,
            ),
        )
        .await
        .map_err(|_| (ObservationFailure::DeadlineExceeded, None))?
    }

    async fn invoke(
        &self,
        event: ExtensionEvent,
        tools: Arc<dyn ExtensionHostTools>,
        invocation: InvocationBinding,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
    ) -> std::result::Result<ExtensionResponse, (ObservationFailure, Option<String>)> {
        use crate::types::ObservationFailure;
        let mut store = Store::new(
            &self.engine,
            HostState::new(
                tools,
                invocation.context,
                config,
                grants,
                allowed_http_origins,
                self.limits.clone(),
                self.http.clone(),
                invocation.tenant_memory_tracker,
            ),
        );
        if let Some(capability) = invocation.delivery {
            let ExtensionEvent::RoomMessageObserve(observation) = &event else {
                return Err((ObservationFailure::Denied, None));
            };
            store
                .data_mut()
                .bind_delivery(capability, observation.source.clone())
                .await
                .map_err(|error| (classify_delivery_error(error), None))?;
        }
        self.configure_store(&mut store)
            .map_err(|_| (ObservationFailure::ResourceLimit, None))?;
        let bindings =
            WaddleExtension::instantiate_async(&mut store, &self.component, &self.linker)
                .await
                .map_err(|error| (classify_runtime_error(error), None))?;
        let delivery = store.data().delivery_resource();
        let result = bindings
            .waddle_extension_framework()
            .call_handle_event(&mut store, &event.into(), delivery)
            .await
            .map_err(|error| (classify_runtime_error(error), None))?;
        store
            .data_mut()
            .validate_delivery()
            .await
            .map_err(|error| (classify_delivery_error(error), None))?;
        match result {
            Ok(response) => response
                .try_into()
                .map_err(|_| (ObservationFailure::InvalidResult, None)),
            Err(error) => Err((
                match error.code {
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
                },
                Some(error.message.value),
            )),
        }
    }
}

struct InvocationBinding {
    context: InvocationContext,
    delivery: Option<Arc<dyn ExtensionDeliveryCapability>>,
    tenant_memory_tracker: Option<Arc<TenantMemoryTracker>>,
}

fn classify_delivery_error(error: HostToolError) -> ObservationFailure {
    use crate::host_tools::HostToolErrorCode;
    use crate::types::ObservationFailure;
    match error.code {
        HostToolErrorCode::TemporaryFailure => ObservationFailure::TemporaryFailure,
        HostToolErrorCode::InvalidRequest => ObservationFailure::InvalidRequest,
        HostToolErrorCode::Denied
        | HostToolErrorCode::NotFound
        | HostToolErrorCode::Unsupported => ObservationFailure::Denied,
    }
}

fn classify_runtime_error(error: wasmtime::Error) -> ObservationFailure {
    match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) => ObservationFailure::ResourceLimit,
        Some(wasmtime::Trap::Interrupt) => ObservationFailure::DeadlineExceeded,
        _ => ObservationFailure::RuntimeFailure,
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[tokio::test]
    async fn spinning_wasm_exhausts_fuel_without_blocking_the_executor() {
        let runtime = WasmRuntime::try_new().expect("runtime");
        let module = wasmtime::Module::new(
            runtime.engine(),
            r#"(module (func (export "run") (loop br 0)))"#,
        )
        .expect("module");
        let mut store = Store::new(runtime.engine(), HostState::for_init());
        store.set_fuel(20_000).expect("fuel");
        store.fuel_async_yield_interval(Some(1_000)).expect("yield");
        store.set_epoch_deadline(100);
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
        let runtime = WasmRuntime::try_new().expect("runtime");
        let module = wasmtime::Module::new(
            runtime.engine(),
            r#"(module
            (memory 1) (func (export "grow") (result i32) i32.const 1 memory.grow))"#,
        )
        .expect("module");
        let mut state = HostState::for_init();
        state.limits = super::super::host_state::AggregateStoreLimits::new(65_536, None);
        let mut store = Store::new(runtime.engine(), state);
        store.limiter(|state| &mut state.limits);
        store.set_fuel(10_000).expect("fuel");
        store.set_epoch_deadline(100);
        store.epoch_deadline_trap();
        let instance = wasmtime::Instance::new_async(&mut store, &module, &[])
            .await
            .expect("instance");
        let grow = instance
            .get_typed_func::<(), i32>(&mut store, "grow")
            .expect("grow");
        assert!(grow.call_async(&mut store, ()).await.is_err());
    }

    #[tokio::test]
    async fn multi_memory_wasm_module_is_capped_by_aggregate_limit() {
        let runtime = WasmRuntime::try_new().expect("runtime");
        // Two 1-page memories = 131,072 bytes > 65,536 byte aggregate cap.
        let module = wasmtime::Module::new(runtime.engine(), r#"(module (memory 1) (memory 1))"#)
            .expect("module");
        let tenant = waddle_broker_core::identity::TenantId::parse("mem.example").expect("valid");
        let tracker = Arc::new(TenantMemoryTracker::new(tenant, 65_536));
        let mut state = HostState::for_init();
        state.limits =
            super::super::host_state::AggregateStoreLimits::new(65_536, Some(Arc::clone(&tracker)));
        let mut store = Store::new(runtime.engine(), state);
        store.limiter(|state| &mut state.limits);
        store.set_fuel(100_000).expect("fuel");
        store.set_epoch_deadline(100);
        store.epoch_deadline_trap();

        let err = wasmtime::Instance::new_async(&mut store, &module, &[])
            .await
            .expect_err("second memory must exceed aggregate store limit");
        assert!(
            err.to_string().contains("aggregate") || err.to_string().contains("memory"),
            "unexpected error: {err}"
        );
        drop(store);
        assert_eq!(
            tracker.allocated_bytes(),
            0,
            "tenant memory tracker must release all reserved bytes on store drop"
        );
    }

    #[tokio::test]
    async fn epoch_interruption_traps_abundant_fuel_guest_as_deadline_exceeded() {
        let runtime = WasmRuntime::try_new().expect("runtime");
        let module = wasmtime::Module::new(
            runtime.engine(),
            r#"(module (func (export "spin") (loop br 0)))"#,
        )
        .expect("module");
        let mut store = Store::new(runtime.engine(), HostState::for_init());
        // Abundant fuel so fuel cannot exhaust before epoch deadline fires.
        store.set_fuel(1_000_000_000).expect("fuel");
        store
            .fuel_async_yield_interval(Some(10_000))
            .expect("yield");
        store.set_epoch_deadline(2);
        store.epoch_deadline_trap();

        let instance = wasmtime::Instance::new_async(&mut store, &module, &[])
            .await
            .expect("instance");
        let spin = instance
            .get_typed_func::<(), ()>(&mut store, "spin")
            .expect("spin");
        let error = tokio::time::timeout(Duration::from_secs(2), spin.call_async(&mut store, ()))
            .await
            .expect("epoch ticker must interrupt spinning guest")
            .expect_err("epoch deadline must trap");
        assert_eq!(
            classify_runtime_error(error),
            crate::types::ObservationFailure::DeadlineExceeded
        );
    }
}
