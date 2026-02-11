use std::collections::BTreeMap;
#[cfg(feature = "native")]
use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
#[cfg(feature = "native")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "native")]
use std::sync::{Mutex, mpsc};
#[cfg(feature = "native")]
use std::thread;
#[cfg(feature = "native")]
use std::time::{Duration, Instant};

use waddle_core::event::Event;
#[cfg(feature = "native")]
use waddle_core::event::{Channel, EventBus, EventPayload, EventSource};
use waddle_storage::Database;

#[cfg(feature = "native")]
use wasmtime::{
    Config, Engine, Instance, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
};

use crate::registry::{ManifestCapability, PluginManifest};

#[cfg(feature = "native")]
const AUTO_DISABLE_ERROR_THRESHOLD: usize = 5;
#[cfg(feature = "native")]
const ERROR_WINDOW: Duration = Duration::from_secs(60);
#[cfg(feature = "native")]
const BLOCKING_POOL_THREADS: usize = 2;

#[cfg(feature = "native")]
type BlockingTask = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRuntimeConfig {
    pub fuel_per_invocation: u64,
    pub fuel_per_render: u64,
    pub epoch_timeout_ms: u64,
    pub max_memory_bytes: u64,
}

impl Default for PluginRuntimeConfig {
    fn default() -> Self {
        Self {
            fuel_per_invocation: 1_000_000,
            fuel_per_render: 5_000_000,
            epoch_timeout_ms: 5_000,
            max_memory_bytes: 16_777_216,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin runtime is not implemented for this target")]
    NotImplemented,

    #[error("failed to compile plugin {id}: {reason}")]
    CompilationFailed { id: String, reason: String },

    #[error("failed to instantiate plugin {id}: {reason}")]
    InstantiationFailed { id: String, reason: String },

    #[error("plugin {id} init failed: {reason}")]
    InitFailed { id: String, reason: String },

    #[error("plugin {id} shutdown failed: {reason}")]
    ShutdownFailed { id: String, reason: String },

    #[error("plugin {id} invocation failed: {reason}")]
    InvocationFailed { id: String, reason: String },

    #[error("plugin {id} exceeded memory limits: {reason}")]
    MemoryLimitExceeded { id: String, reason: String },

    #[error("plugin {id} fuel exhausted")]
    FuelExhausted { id: String },

    #[error("plugin {id} epoch timeout")]
    EpochTimeout { id: String },

    #[error("invalid manifest for plugin {id}: {reason}")]
    InvalidManifest { id: String, reason: String },

    #[error("plugin {id} already loaded")]
    AlreadyLoaded { id: String },

    #[error("plugin {id} not found")]
    NotFound { id: String },

    #[error("plugin {id} auto-disabled: too many errors")]
    AutoDisabled { id: String },

    #[error("failed to run runtime task for plugin {id}: {reason}")]
    RuntimeTaskFailed { id: String, reason: String },

    #[error("failed to publish plugin event for {id}: {reason}")]
    EventPublishFailed { id: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHandle {
    pub id: String,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginStatus {
    Loading,
    Active,
    Error(String),
    Unloading,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginCapability {
    EventHandler,
    StanzaProcessor { priority: i32 },
    TuiRenderer,
    GuiMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub status: PluginStatus,
    pub capabilities: Vec<PluginCapability>,
    pub error_count: u32,
}

#[derive(Debug, Clone)]
pub enum PluginHook {
    Event(Box<Event>),
    InboundStanza(String),
    OutboundStanza(String),
    TuiRender { width: u16, height: u16 },
    GuiGetComponentInfo,
}

#[cfg(feature = "native")]
struct PluginStoreState {
    plugin_id: String,
    limits: StoreLimits,
}

#[cfg(feature = "native")]
enum LifecycleInit {
    Unit(TypedFunc<(), ()>),
    Status(TypedFunc<(), i32>),
}

#[cfg(feature = "native")]
enum LifecycleShutdown {
    Unit(TypedFunc<(), ()>),
    Status(TypedFunc<(), i32>),
}

#[cfg(feature = "native")]
struct LoadedPlugin {
    store: Store<PluginStoreState>,
    _instance: Instance,
    shutdown: LifecycleShutdown,
}

#[cfg(feature = "native")]
impl LoadedPlugin {
    fn shutdown(&mut self, fuel_per_invocation: u64) -> Result<(), PluginError> {
        let plugin_id = self.store.data().plugin_id.clone();
        prepare_invocation(&mut self.store, &plugin_id, fuel_per_invocation)?;

        match &self.shutdown {
            LifecycleShutdown::Unit(func) => func
                .call(&mut self.store, ())
                .map_err(|error| classify_invocation_error(&plugin_id, error)),
            LifecycleShutdown::Status(func) => {
                let status = func
                    .call(&mut self.store, ())
                    .map_err(|error| classify_invocation_error(&plugin_id, error))?;
                if status == 0 {
                    Ok(())
                } else {
                    Err(PluginError::ShutdownFailed {
                        id: plugin_id,
                        reason: format!("non-zero shutdown status: {status}"),
                    })
                }
            }
        }
    }
}

#[cfg(feature = "native")]
struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

#[cfg(feature = "native")]
impl EpochTicker {
    fn new(engine: Engine, tick_interval: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                thread::sleep(tick_interval);
                engine.increment_epoch();
            }
        });

        Self {
            stop,
            handle: Some(handle),
        }
    }
}

#[cfg(feature = "native")]
impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(feature = "native")]
struct BlockingPool {
    sender: Option<mpsc::Sender<BlockingTask>>,
    workers: Vec<thread::JoinHandle<()>>,
}

#[cfg(feature = "native")]
impl BlockingPool {
    fn new(worker_count: usize) -> Self {
        let worker_count = worker_count.max(1);
        let (sender, receiver) = mpsc::channel::<BlockingTask>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(worker_count);

        for index in 0..worker_count {
            let receiver = Arc::clone(&receiver);
            let handle = thread::Builder::new()
                .name(format!("waddle-plugin-runtime-{index}"))
                .spawn(move || {
                    loop {
                        let task = {
                            let guard = receiver.lock();
                            match guard {
                                Ok(guard) => guard.recv(),
                                Err(_) => return,
                            }
                        };

                        match task {
                            Ok(task) => task(),
                            Err(_) => break,
                        }
                    }
                })
                .expect("failed to spawn plugin runtime worker");
            workers.push(handle);
        }

        Self {
            sender: Some(sender),
            workers,
        }
    }

    fn execute(&self, task: BlockingTask) -> Result<(), String> {
        let Some(sender) = &self.sender else {
            return Err("thread pool is shutting down".to_string());
        };

        sender.send(task).map_err(|error| error.to_string())
    }
}

#[cfg(feature = "native")]
impl Drop for BlockingPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

pub struct PluginRuntime<D: Database> {
    config: PluginRuntimeConfig,
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
    db: Arc<D>,
    plugins: BTreeMap<String, PluginInfo>,
    #[cfg(feature = "native")]
    engine: Engine,
    #[cfg(feature = "native")]
    runtime_plugins: BTreeMap<String, LoadedPlugin>,
    #[cfg(feature = "native")]
    error_windows: BTreeMap<String, VecDeque<Instant>>,
    #[cfg(feature = "native")]
    disabled_plugins: BTreeSet<String>,
    #[cfg(feature = "native")]
    blocking_pool: BlockingPool,
    #[cfg(feature = "native")]
    _epoch_ticker: EpochTicker,
}

impl<D: Database> PluginRuntime<D> {
    #[cfg(feature = "native")]
    pub fn new(config: PluginRuntimeConfig, event_bus: Arc<dyn EventBus>, db: Arc<D>) -> Self {
        let mut wasmtime_config = Config::new();
        wasmtime_config.consume_fuel(true);
        wasmtime_config.epoch_interruption(true);
        wasmtime_config.wasm_component_model(true);
        let engine = Engine::new(&wasmtime_config)
            .expect("failed to create wasmtime engine for plugin runtime");

        let blocking_pool = BlockingPool::new(BLOCKING_POOL_THREADS);

        let tick_interval = Duration::from_millis(config.epoch_timeout_ms.max(1));
        let epoch_ticker = EpochTicker::new(engine.clone(), tick_interval);

        Self {
            config,
            event_bus,
            db,
            plugins: BTreeMap::new(),
            engine,
            runtime_plugins: BTreeMap::new(),
            error_windows: BTreeMap::new(),
            disabled_plugins: BTreeSet::new(),
            blocking_pool,
            _epoch_ticker: epoch_ticker,
        }
    }

    #[cfg(not(feature = "native"))]
    pub fn new(config: PluginRuntimeConfig, db: Arc<D>) -> Self {
        Self {
            config,
            db,
            plugins: BTreeMap::new(),
        }
    }

    pub fn config(&self) -> &PluginRuntimeConfig {
        &self.config
    }

    #[cfg(feature = "native")]
    pub fn event_bus(&self) -> &Arc<dyn EventBus> {
        &self.event_bus
    }

    pub fn database(&self) -> &Arc<D> {
        &self.db
    }

    pub async fn load_plugin(
        &mut self,
        manifest: PluginManifest,
        wasm_bytes: &[u8],
    ) -> Result<PluginHandle, PluginError> {
        #[cfg(feature = "native")]
        {
            manifest
                .validate()
                .map_err(|error| PluginError::InvalidManifest {
                    id: manifest.id().to_string(),
                    reason: error.to_string(),
                })?;

            let plugin_id = manifest.id().to_string();
            if self.disabled_plugins.contains(&plugin_id) {
                return Err(PluginError::AutoDisabled { id: plugin_id });
            }

            if self.runtime_plugins.contains_key(&plugin_id) {
                return Err(PluginError::AlreadyLoaded { id: plugin_id });
            }

            let plugin_name = manifest.name().to_string();
            let plugin_version = manifest.version().to_string();
            let capabilities = map_capabilities(&manifest);

            self.plugins.insert(
                plugin_id.clone(),
                PluginInfo {
                    id: plugin_id.clone(),
                    name: plugin_name.clone(),
                    version: plugin_version.clone(),
                    status: PluginStatus::Loading,
                    capabilities,
                    error_count: 0,
                },
            );

            let engine = self.engine.clone();
            let config = self.config.clone();
            let plugin_id_for_task = plugin_id.clone();
            let wasm = wasm_bytes.to_vec();
            let load_result = self
                .run_blocking_task(plugin_id.clone(), move || {
                    compile_and_init_plugin(engine, config, plugin_id_for_task, wasm)
                })
                .await;

            match load_result {
                Ok(loaded_plugin) => {
                    self.runtime_plugins
                        .insert(plugin_id.clone(), loaded_plugin);
                    self.error_windows.remove(&plugin_id);
                    self.disabled_plugins.remove(&plugin_id);

                    if let Some(plugin_info) = self.plugins.get_mut(&plugin_id) {
                        plugin_info.status = PluginStatus::Active;
                        plugin_info.error_count = 0;
                    }

                    let _ = self.emit_plugin_loaded(&plugin_id, &plugin_version);

                    Ok(PluginHandle {
                        id: plugin_id,
                        name: plugin_name,
                        version: plugin_version,
                    })
                }
                Err(error) => {
                    let reason = error.to_string();
                    let auto_disabled = self.record_plugin_error(&plugin_id, &reason);
                    let _ = self.emit_plugin_error(&plugin_id, &reason);

                    self.plugins.remove(&plugin_id);
                    self.runtime_plugins.remove(&plugin_id);

                    if auto_disabled {
                        let _ =
                            self.emit_plugin_error(&plugin_id, "auto-disabled: too many errors");
                        return Err(PluginError::AutoDisabled { id: plugin_id });
                    }

                    Err(error)
                }
            }
        }

        #[cfg(not(feature = "native"))]
        {
            let _ = manifest;
            let _ = wasm_bytes;
            Err(PluginError::NotImplemented)
        }
    }

    pub async fn unload_plugin(&mut self, plugin_id: &str) -> Result<(), PluginError> {
        #[cfg(feature = "native")]
        {
            let plugin_id = plugin_id.to_string();
            let Some(mut loaded_plugin) = self.runtime_plugins.remove(&plugin_id) else {
                return Err(PluginError::NotFound { id: plugin_id });
            };

            if let Some(plugin_info) = self.plugins.get_mut(&plugin_id) {
                plugin_info.status = PluginStatus::Unloading;
            }

            let config = self.config.clone();
            let task_plugin_id = plugin_id.clone();
            let shutdown_result = self
                .run_blocking_task(plugin_id.clone(), move || {
                    let _ = &task_plugin_id;
                    loaded_plugin.shutdown(config.fuel_per_invocation)
                })
                .await;

            self.plugins.remove(&plugin_id);

            match shutdown_result {
                Ok(()) => {
                    let _ = self.emit_plugin_unloaded(&plugin_id);
                    Ok(())
                }
                Err(error) => {
                    let reason = error.to_string();
                    let auto_disabled = self.record_plugin_error(&plugin_id, &reason);
                    let _ = self.emit_plugin_error(&plugin_id, &reason);
                    let _ = self.emit_plugin_unloaded(&plugin_id);

                    if auto_disabled {
                        let _ =
                            self.emit_plugin_error(&plugin_id, "auto-disabled: too many errors");
                        return Err(PluginError::AutoDisabled { id: plugin_id });
                    }

                    Err(error)
                }
            }
        }

        #[cfg(not(feature = "native"))]
        {
            let _ = plugin_id;
            Err(PluginError::NotImplemented)
        }
    }

    pub fn list_plugins(&self) -> Vec<PluginInfo> {
        self.plugins.values().cloned().collect()
    }

    pub fn get_plugin(&self, plugin_id: &str) -> Option<&PluginInfo> {
        self.plugins.get(plugin_id)
    }

    pub async fn invoke_hook(&self, hook: PluginHook) -> Result<(), PluginError> {
        let _ = hook;

        #[cfg(feature = "native")]
        {
            if self.runtime_plugins.is_empty() {
                return Ok(());
            }
            Err(PluginError::NotImplemented)
        }

        #[cfg(not(feature = "native"))]
        {
            Err(PluginError::NotImplemented)
        }
    }

    #[cfg(feature = "native")]
    async fn run_blocking_task<T, F>(&self, plugin_id: String, task: F) -> Result<T, PluginError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, PluginError> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<T, PluginError>>();
        self.blocking_pool
            .execute(Box::new(move || {
                let _ = tx.send(task());
            }))
            .map_err(|error| PluginError::RuntimeTaskFailed {
                id: plugin_id.clone(),
                reason: error,
            })?;

        rx.await.map_err(|error| PluginError::RuntimeTaskFailed {
            id: plugin_id,
            reason: error.to_string(),
        })?
    }

    #[cfg(feature = "native")]
    fn record_plugin_error(&mut self, plugin_id: &str, reason: &str) -> bool {
        let now = Instant::now();
        let window = self.error_windows.entry(plugin_id.to_string()).or_default();
        window.push_back(now);

        while let Some(timestamp) = window.front().copied() {
            if now.duration_since(timestamp) > ERROR_WINDOW {
                window.pop_front();
            } else {
                break;
            }
        }

        let error_count = window.len() as u32;
        if let Some(plugin_info) = self.plugins.get_mut(plugin_id) {
            plugin_info.error_count = error_count;
            plugin_info.status = PluginStatus::Error(reason.to_string());
        }

        if window.len() >= AUTO_DISABLE_ERROR_THRESHOLD {
            self.disabled_plugins.insert(plugin_id.to_string());
            self.runtime_plugins.remove(plugin_id);
            self.plugins.remove(plugin_id);
            return true;
        }

        false
    }

    #[cfg(feature = "native")]
    fn emit_plugin_loaded(&self, plugin_id: &str, version: &str) -> Result<(), PluginError> {
        self.emit_plugin_event(
            plugin_id,
            "loaded",
            EventPayload::PluginLoaded {
                plugin_id: plugin_id.to_string(),
                version: version.to_string(),
            },
        )
    }

    #[cfg(feature = "native")]
    fn emit_plugin_unloaded(&self, plugin_id: &str) -> Result<(), PluginError> {
        self.emit_plugin_event(
            plugin_id,
            "unloaded",
            EventPayload::PluginUnloaded {
                plugin_id: plugin_id.to_string(),
            },
        )
    }

    #[cfg(feature = "native")]
    fn emit_plugin_error(&self, plugin_id: &str, reason: &str) -> Result<(), PluginError> {
        self.emit_plugin_event(
            plugin_id,
            "error",
            EventPayload::PluginError {
                plugin_id: plugin_id.to_string(),
                error: reason.to_string(),
            },
        )
    }

    #[cfg(feature = "native")]
    fn emit_plugin_event(
        &self,
        plugin_id: &str,
        action: &str,
        payload: EventPayload,
    ) -> Result<(), PluginError> {
        let channel = plugin_channel(plugin_id, action)?;
        let event = Event::new(channel, EventSource::System("plugins".to_string()), payload);
        self.event_bus
            .publish(event)
            .map_err(|error| PluginError::EventPublishFailed {
                id: plugin_id.to_string(),
                reason: error.to_string(),
            })
    }
}

fn map_capabilities(manifest: &PluginManifest) -> Vec<PluginCapability> {
    manifest
        .capabilities()
        .into_iter()
        .filter_map(|capability| match capability {
            ManifestCapability::EventHandler => Some(PluginCapability::EventHandler),
            ManifestCapability::StanzaProcessor { priority } => {
                Some(PluginCapability::StanzaProcessor { priority })
            }
            ManifestCapability::TuiRenderer => Some(PluginCapability::TuiRenderer),
            ManifestCapability::GuiMetadata => Some(PluginCapability::GuiMetadata),
            ManifestCapability::KvStorage => None,
        })
        .collect()
}

#[cfg(feature = "native")]
fn compile_and_init_plugin(
    engine: Engine,
    config: PluginRuntimeConfig,
    plugin_id: String,
    wasm_bytes: Vec<u8>,
) -> Result<LoadedPlugin, PluginError> {
    let module =
        Module::new(&engine, wasm_bytes).map_err(|error| PluginError::CompilationFailed {
            id: plugin_id.clone(),
            reason: error.to_string(),
        })?;

    let memory_limit = usize::try_from(config.max_memory_bytes).unwrap_or(usize::MAX);
    let limits = StoreLimitsBuilder::new()
        .memory_size(memory_limit)
        .trap_on_grow_failure(true)
        .build();
    let mut store = Store::new(
        &engine,
        PluginStoreState {
            plugin_id: plugin_id.clone(),
            limits,
        },
    );
    store.limiter(|state| &mut state.limits);
    store.epoch_deadline_trap();

    let linker = Linker::new(&engine);
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|error| map_instantiation_error(&plugin_id, error.to_string()))?;

    let init = resolve_init(&mut store, &instance, &plugin_id)?;
    invoke_init(&mut store, &init, &plugin_id, config.fuel_per_invocation)?;

    let shutdown = resolve_shutdown(&mut store, &instance, &plugin_id)?;

    Ok(LoadedPlugin {
        store,
        _instance: instance,
        shutdown,
    })
}

#[cfg(feature = "native")]
fn resolve_init(
    store: &mut Store<PluginStoreState>,
    instance: &Instance,
    plugin_id: &str,
) -> Result<LifecycleInit, PluginError> {
    for export_name in ["plugin_init", "init"] {
        if let Ok(func) = instance.get_typed_func::<(), i32>(&mut *store, export_name) {
            return Ok(LifecycleInit::Status(func));
        }
        if let Ok(func) = instance.get_typed_func::<(), ()>(&mut *store, export_name) {
            return Ok(LifecycleInit::Unit(func));
        }
    }

    Err(PluginError::InitFailed {
        id: plugin_id.to_string(),
        reason: "missing required export: plugin_init or init".to_string(),
    })
}

#[cfg(feature = "native")]
fn resolve_shutdown(
    store: &mut Store<PluginStoreState>,
    instance: &Instance,
    plugin_id: &str,
) -> Result<LifecycleShutdown, PluginError> {
    for export_name in ["plugin_shutdown", "shutdown"] {
        if let Ok(func) = instance.get_typed_func::<(), i32>(&mut *store, export_name) {
            return Ok(LifecycleShutdown::Status(func));
        }
        if let Ok(func) = instance.get_typed_func::<(), ()>(&mut *store, export_name) {
            return Ok(LifecycleShutdown::Unit(func));
        }
    }

    Err(PluginError::ShutdownFailed {
        id: plugin_id.to_string(),
        reason: "missing required export: plugin_shutdown or shutdown".to_string(),
    })
}

#[cfg(feature = "native")]
fn invoke_init(
    store: &mut Store<PluginStoreState>,
    init: &LifecycleInit,
    plugin_id: &str,
    fuel_per_invocation: u64,
) -> Result<(), PluginError> {
    prepare_invocation(store, plugin_id, fuel_per_invocation)?;

    match init {
        LifecycleInit::Unit(func) => func
            .call(store, ())
            .map_err(|error| classify_invocation_error(plugin_id, error)),
        LifecycleInit::Status(func) => {
            let status = func
                .call(store, ())
                .map_err(|error| classify_invocation_error(plugin_id, error))?;
            if status == 0 {
                Ok(())
            } else {
                Err(PluginError::InitFailed {
                    id: plugin_id.to_string(),
                    reason: format!("non-zero init status: {status}"),
                })
            }
        }
    }
}

#[cfg(feature = "native")]
fn prepare_invocation(
    store: &mut Store<PluginStoreState>,
    plugin_id: &str,
    fuel_per_invocation: u64,
) -> Result<(), PluginError> {
    store
        .set_fuel(fuel_per_invocation)
        .map_err(|error| PluginError::InvocationFailed {
            id: plugin_id.to_string(),
            reason: error.to_string(),
        })?;
    store.set_epoch_deadline(1);
    Ok(())
}

#[cfg(feature = "native")]
fn classify_invocation_error(plugin_id: &str, error: wasmtime::Error) -> PluginError {
    if let Some(trap) = error.downcast_ref::<wasmtime::Trap>() {
        match trap {
            wasmtime::Trap::OutOfFuel => {
                return PluginError::FuelExhausted {
                    id: plugin_id.to_string(),
                };
            }
            wasmtime::Trap::Interrupt => {
                return PluginError::EpochTimeout {
                    id: plugin_id.to_string(),
                };
            }
            wasmtime::Trap::MemoryOutOfBounds
            | wasmtime::Trap::AllocationTooLarge
            | wasmtime::Trap::HeapMisaligned => {
                return PluginError::MemoryLimitExceeded {
                    id: plugin_id.to_string(),
                    reason: error.to_string(),
                };
            }
            _ => {}
        }
    }

    let reason = error.to_string();
    let mut reason_chain_lc = String::new();
    for cause in error.chain() {
        reason_chain_lc.push_str(&cause.to_string().to_ascii_lowercase());
        reason_chain_lc.push('\n');
    }
    if reason_chain_lc.contains("out of fuel") || reason_chain_lc.contains("all fuel consumed") {
        return PluginError::FuelExhausted {
            id: plugin_id.to_string(),
        };
    }
    if reason_chain_lc.contains("interrupt") || reason_chain_lc.contains("epoch deadline") {
        return PluginError::EpochTimeout {
            id: plugin_id.to_string(),
        };
    }
    if is_memory_limit_error(&reason_chain_lc) {
        return PluginError::MemoryLimitExceeded {
            id: plugin_id.to_string(),
            reason,
        };
    }

    PluginError::InvocationFailed {
        id: plugin_id.to_string(),
        reason,
    }
}

#[cfg(feature = "native")]
fn map_instantiation_error(plugin_id: &str, reason: String) -> PluginError {
    let reason_lc = reason.to_ascii_lowercase();
    if is_memory_limit_error(&reason_lc) {
        return PluginError::MemoryLimitExceeded {
            id: plugin_id.to_string(),
            reason,
        };
    }

    PluginError::InstantiationFailed {
        id: plugin_id.to_string(),
        reason,
    }
}

#[cfg(feature = "native")]
fn is_memory_limit_error(reason: &str) -> bool {
    (reason.contains("memory") || reason.contains("linear memory"))
        && (reason.contains("grow") || reason.contains("limit") || reason.contains("minimum"))
}

#[cfg(feature = "native")]
fn plugin_channel(plugin_id: &str, action: &str) -> Result<Channel, PluginError> {
    let safe_plugin_id = plugin_id.replace('-', "_");
    let channel_name = format!("plugin.{safe_plugin_id}.{action}");
    Channel::new(channel_name.clone()).map_err(|error| PluginError::EventPublishFailed {
        id: plugin_id.to_string(),
        reason: error.to_string(),
    })
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use std::path::Path;

    use waddle_core::event::BroadcastEventBus;
    use waddle_storage::open_database;

    use super::*;

    fn test_manifest(plugin_id: &str) -> PluginManifest {
        let toml = format!(
            r#"
[plugin]
id = "{plugin_id}"
name = "Runtime Test Plugin"
version = "1.0.0"
description = "Runtime test plugin"
license = "MIT"

[permissions]
stanza_access = false
event_subscriptions = []
kv_storage = false

[hooks]
stanza_processor = false
stanza_priority = 0
event_handler = false
tui_renderer = false
gui_metadata = false
"#
        );

        PluginManifest::from_toml_str(&toml).expect("manifest should be valid")
    }

    async fn open_runtime(
        config: PluginRuntimeConfig,
    ) -> (PluginRuntime<impl Database>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("runtime.db");
        let db = open_database(Path::new(&db_path))
            .await
            .expect("failed to open db");
        let event_bus = Arc::new(BroadcastEventBus::new(64));
        let runtime = PluginRuntime::new(config, event_bus, Arc::new(db));
        (runtime, dir)
    }

    #[tokio::test]
    async fn load_init_and_unload_plugin() {
        let (mut runtime, _dir) = open_runtime(PluginRuntimeConfig::default()).await;
        let manifest = test_manifest("com.waddle.runtime.plugin");
        let wasm = r#"
            (module
              (func (export "plugin_init") (result i32)
                i32.const 0)
              (func (export "plugin_shutdown")))
        "#;

        let handle = runtime
            .load_plugin(manifest.clone(), wasm.as_bytes())
            .await
            .expect("load should succeed");
        assert_eq!(handle.id, "com.waddle.runtime.plugin");
        assert_eq!(runtime.list_plugins().len(), 1);
        assert!(matches!(
            runtime
                .get_plugin("com.waddle.runtime.plugin")
                .map(|info| &info.status),
            Some(PluginStatus::Active)
        ));

        let duplicate = runtime.load_plugin(manifest, wasm.as_bytes()).await;
        assert!(matches!(
            duplicate,
            Err(PluginError::AlreadyLoaded { id }) if id == "com.waddle.runtime.plugin"
        ));

        runtime
            .unload_plugin("com.waddle.runtime.plugin")
            .await
            .expect("unload should succeed");
        assert!(runtime.list_plugins().is_empty());
    }

    #[tokio::test]
    async fn auto_disables_plugin_after_five_errors() {
        let config = PluginRuntimeConfig {
            fuel_per_invocation: 10_000,
            ..PluginRuntimeConfig::default()
        };
        let (mut runtime, _dir) = open_runtime(config).await;
        let manifest = test_manifest("com.waddle.runtime.autodisable");
        let wasm = r#"
            (module
              (func (export "plugin_init")
                unreachable)
              (func (export "plugin_shutdown")))
        "#;

        for _ in 0..4 {
            let result = runtime.load_plugin(manifest.clone(), wasm.as_bytes()).await;
            assert!(
                matches!(
                    result,
                    Err(PluginError::InvocationFailed { ref id, .. }) if id == "com.waddle.runtime.autodisable"
                ),
                "unexpected result: {result:?}"
            );
        }

        let fifth = runtime.load_plugin(manifest.clone(), wasm.as_bytes()).await;
        assert!(matches!(
            fifth,
            Err(PluginError::AutoDisabled { id }) if id == "com.waddle.runtime.autodisable"
        ));

        let sixth = runtime.load_plugin(manifest, wasm.as_bytes()).await;
        assert!(matches!(
            sixth,
            Err(PluginError::AutoDisabled { id }) if id == "com.waddle.runtime.autodisable"
        ));
    }

    #[tokio::test]
    async fn memory_limit_is_enforced_for_init() {
        let config = PluginRuntimeConfig {
            max_memory_bytes: 65_536,
            ..PluginRuntimeConfig::default()
        };
        let (mut runtime, _dir) = open_runtime(config).await;
        let manifest = test_manifest("com.waddle.runtime.memory");
        let wasm = r#"
            (module
              (memory 1)
              (func (export "plugin_init")
                i32.const 1
                memory.grow
                drop)
              (func (export "plugin_shutdown")))
        "#;

        let result = runtime.load_plugin(manifest, wasm.as_bytes()).await;
        assert!(
            matches!(
                result,
                Err(PluginError::MemoryLimitExceeded { ref id, .. }) if id == "com.waddle.runtime.memory"
            ),
            "unexpected result: {result:?}"
        );
    }

    #[tokio::test]
    async fn fuel_limit_is_enforced_for_init() {
        let config = PluginRuntimeConfig {
            fuel_per_invocation: 500,
            ..PluginRuntimeConfig::default()
        };
        let (mut runtime, _dir) = open_runtime(config).await;
        let manifest = test_manifest("com.waddle.runtime.fuel");
        let wasm = r#"
            (module
              (func (export "plugin_init")
                (local i32)
                i32.const 1000000
                local.set 0
                (block
                (loop
                  local.get 0
                  i32.eqz
                  br_if 1
                  local.get 0
                  i32.const 1
                  i32.sub
                  local.set 0
                  br 0)))
              (func (export "plugin_shutdown")))
        "#;

        let result = runtime.load_plugin(manifest, wasm.as_bytes()).await;
        assert!(
            matches!(
                result,
                Err(PluginError::FuelExhausted { ref id }) if id == "com.waddle.runtime.fuel"
            ),
            "unexpected result: {result:?}"
        );
    }
}
