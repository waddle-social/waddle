//! Per-invocation Wasmtime state and host capability access.

use crate::config::RuntimeLimits;
use crate::types::RoomMessageSource;
use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, error, info, trace, warn};
use wasmtime::component::{Resource, ResourceTable};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use super::http::execute_runtime_http_request;
use super::inference::{
    DenyingExtensionInferenceHost, ExtensionInferenceHost, HostInferenceChunk, HostInferenceError,
    HostInferenceErrorCode, HostInferenceMessage, HostInferenceRequest, InferenceCredentialRef,
    InferenceStream, VerifiedInferenceUsage,
};
use super::waddle::extension::host_tools::Host as HostToolsHost;
use super::waddle::extension::inference::{
    self as wit_inference, Host as InferenceHost, HostInferenceStream,
};
use super::waddle::extension::runtime::Host as RuntimeHost;
use super::waddle::extension::types as wit_types;
use super::wasi::logging::logging::{Host as LoggingHost, Level as LogLevel};
use crate::host_tools::{
    DenyingExtensionHostTools, ExtensionDeliveryCapability, ExtensionHostTools, HostToolError,
    InvocationContext, InvocationKind,
};
use crate::types::{DisplayText, ExtensionCapability, PluginId, WaddleId};
use waddle_broker_core::quotas::TenantMemoryTracker;

/// `wasmtime::ResourceLimiter` enforcing an aggregate linear memory ceiling across
/// **all** memories in a single store (preventing `memories(4) * memory_size(N)`
/// multiplication) and optionally charging a shared per-tenant [`TenantMemoryTracker`].
pub(super) struct AggregateStoreLimits {
    inner: wasmtime::StoreLimits,
    max_total_memory_bytes: usize,
    allocated_memory_bytes: usize,
    last_pending_delta: usize,
    tenant_memory_tracker: Option<Arc<TenantMemoryTracker>>,
}

impl AggregateStoreLimits {
    pub(super) fn new(
        max_total_memory_bytes: usize,
        tenant_memory_tracker: Option<Arc<TenantMemoryTracker>>,
    ) -> Self {
        let inner = wasmtime::StoreLimitsBuilder::new()
            .memory_size(max_total_memory_bytes)
            .table_elements(20_000)
            .tables(8)
            .memories(4)
            .instances(16)
            .trap_on_grow_failure(true)
            .build();
        Self {
            inner,
            max_total_memory_bytes,
            allocated_memory_bytes: 0,
            last_pending_delta: 0,
            tenant_memory_tracker,
        }
    }
}

impl Drop for AggregateStoreLimits {
    fn drop(&mut self) {
        if let Some(tracker) = &self.tenant_memory_tracker {
            tracker.release(self.allocated_memory_bytes);
        }
        self.allocated_memory_bytes = 0;
    }
}

impl wasmtime::ResourceLimiter for AggregateStoreLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.last_pending_delta = 0;
        let delta = desired.saturating_sub(current);
        let next_total = self.allocated_memory_bytes.saturating_add(delta);
        if next_total > self.max_total_memory_bytes {
            return Err(wasmtime::format_err!(
                "store aggregate linear memory limit ({} bytes) exceeded",
                self.max_total_memory_bytes
            ));
        }
        if let Some(tracker) = &self.tenant_memory_tracker {
            tracker.try_reserve(delta).map_err(|_| {
                wasmtime::format_err!(
                    "tenant aggregate Wasm linear memory limit ({} bytes) exceeded",
                    tracker.max_bytes()
                )
            })?;
        }
        match self.inner.memory_growing(current, desired, maximum) {
            Ok(true) => {
                self.allocated_memory_bytes = next_total;
                self.last_pending_delta = delta;
                Ok(true)
            }
            Ok(false) => {
                if let Some(tracker) = &self.tenant_memory_tracker {
                    tracker.release(delta);
                }
                self.last_pending_delta = 0;
                Ok(false)
            }
            Err(err) => {
                if let Some(tracker) = &self.tenant_memory_tracker {
                    tracker.release(delta);
                }
                self.last_pending_delta = 0;
                Err(err)
            }
        }
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        let delta = std::mem::take(&mut self.last_pending_delta);
        if delta > 0 {
            self.allocated_memory_bytes = self.allocated_memory_bytes.saturating_sub(delta);
            if let Some(tracker) = &self.tenant_memory_tracker {
                tracker.release(delta);
            }
        }
        self.inner.memory_grow_failed(error)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.inner.table_growing(current, desired, maximum)
    }

    fn table_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.inner.table_grow_failed(error)
    }

    fn instances(&self) -> usize {
        self.inner.instances()
    }

    fn tables(&self) -> usize {
        self.inner.tables()
    }

    fn memories(&self) -> usize {
        self.inner.memories()
    }
}

/// The Foundation delivery key remains inside the server implementation. This
/// table entry has no public constructor, representation, or receipt operation.
pub struct DeliveryKey {
    capability: Arc<dyn ExtensionDeliveryCapability>,
    source: RoomMessageSource,
}

/// Host state made available to every WASM instance for satisfying WASI imports.
pub struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    delivery: Option<Resource<DeliveryKey>>,
    tools: Arc<dyn ExtensionHostTools>,
    inference: Arc<dyn ExtensionInferenceHost>,
    chunk_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    context: InvocationContext,
    /// Guest configuration returned by the runtime configuration host import.
    pub config: String,
    grants: HashSet<ExtensionCapability>,
    allowed_http_origins: Vec<String>,
    pub(super) limits: AggregateStoreLimits,
    runtime_limits: RuntimeLimits,
    http: super::http::HttpRuntime,
    http_requests: u32,
    inference_invocation: bool,
}

/// Construction parameters for [`HostState::from_spec`].
pub struct HostStateSpec {
    /// Host tools service implementation.
    pub tools: Arc<dyn ExtensionHostTools>,
    /// Brokered inference stream host implementation.
    pub inference: Arc<dyn ExtensionInferenceHost>,
    /// Optional bounded channel forwarding progressive inference chunks to the transport.
    pub chunk_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    /// Invocation authority and room/tenant context.
    pub context: InvocationContext,
    /// Serialized extension configuration JSON.
    pub config: String,
    /// Granted capabilities for this extension module.
    pub grants: HashSet<ExtensionCapability>,
    /// Allowed HTTPS origins for non-inference HTTP calls.
    pub allowed_http_origins: Vec<String>,
    /// Per-invocation runtime limits.
    pub runtime_limits: RuntimeLimits,
    /// Shared host HTTP runtime.
    pub http: super::http::HttpRuntime,
    /// Optional per-tenant memory quota tracker.
    pub tenant_memory_tracker: Option<Arc<TenantMemoryTracker>>,
    /// Whether this invocation is an inference invocation.
    pub inference_invocation: bool,
}

impl HostState {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        tools: Arc<dyn ExtensionHostTools>,
        context: InvocationContext,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
        runtime_limits: RuntimeLimits,
        http: super::http::HttpRuntime,
        tenant_memory_tracker: Option<Arc<TenantMemoryTracker>>,
    ) -> Self {
        Self::from_spec(HostStateSpec {
            tools,
            inference: Arc::new(DenyingExtensionInferenceHost),
            chunk_tx: None,
            context,
            config,
            grants,
            allowed_http_origins,
            runtime_limits,
            http,
            tenant_memory_tracker,
            inference_invocation: false,
        })
    }

    /// Construct a [`HostState`] from a [`HostStateSpec`].
    #[must_use]
    pub fn from_spec(spec: HostStateSpec) -> Self {
        let mut wasi_builder = WasiCtxBuilder::new();
        // Stderr is never inherited from the guest onto the host pod stderr.
        let wasi = wasi_builder.build();
        let limits =
            AggregateStoreLimits::new(spec.runtime_limits.memory_bytes, spec.tenant_memory_tracker);
        let is_inference =
            spec.inference_invocation || is_inference_plugin(&spec.context.plugin_id);
        let effective_allowed_origins = if is_inference {
            Vec::new()
        } else {
            spec.allowed_http_origins
        };
        Self {
            wasi,
            table: ResourceTable::new(),
            delivery: None,
            tools: spec.tools,
            inference: spec.inference,
            chunk_tx: spec.chunk_tx,
            context: spec.context,
            config: spec.config,
            grants: spec.grants,
            allowed_http_origins: effective_allowed_origins,
            limits,
            runtime_limits: spec.runtime_limits,
            http: spec.http,
            http_requests: 0,
            inference_invocation: is_inference,
        }
    }

    /// Invoke the guest logging handler (`wasi:logging/logging`) for integration
    /// testing of log suppression on inference/command invocations.
    pub async fn emit_guest_log_for_test(&mut self, context: &str, message: &str) {
        let _ = <Self as LoggingHost>::log(
            self,
            LogLevel::Error,
            context.to_owned(),
            message.to_owned(),
        )
        .await;
    }

    pub(super) async fn bind_delivery(
        &mut self,
        capability: Arc<dyn ExtensionDeliveryCapability>,
        source: RoomMessageSource,
    ) -> Result<(), HostToolError> {
        capability.validate(&self.context, &source).await?;
        let resource = self
            .table
            .push(DeliveryKey { capability, source })
            .map_err(|_| {
                HostToolError::denied(
                    DisplayText::try_new("delivery resource unavailable").expect("static denial"),
                )
            })?;
        self.delivery = Some(resource);
        Ok(())
    }

    pub(super) fn delivery_resource(&self) -> Option<Resource<DeliveryKey>> {
        self.delivery
            .as_ref()
            .map(|key| Resource::new_borrow(key.rep()))
    }

    pub(super) async fn validate_delivery(&mut self) -> Result<(), HostToolError> {
        if let Some(resource) = self.delivery.as_ref() {
            let key = self.table.get(resource).map_err(|_| {
                HostToolError::denied(
                    DisplayText::try_new("delivery resource unavailable").expect("static denial"),
                )
            })?;
            key.capability.validate(&self.context, &key.source).await?;
        }
        Ok(())
    }

    pub(super) fn for_init() -> Self {
        Self::for_init_with(
            RuntimeLimits::default(),
            super::http::HttpRuntime::try_new().expect("static HTTP client configuration"),
        )
    }

    pub(super) fn for_init_with(limits: RuntimeLimits, http: super::http::HttpRuntime) -> Self {
        Self::new(
            Arc::new(DenyingExtensionHostTools),
            InvocationContext {
                tenant_id: None,
                waddle_id: WaddleId::try_new("init").expect("static waddle id is valid"),
                plugin_id: PluginId::try_new("initializing-extension")
                    .expect("static plugin id is valid"),
                requester: None,
                source_room: None,
                kind: InvocationKind::Launch,
                provider_room_grants: Vec::new(),
            },
            String::new(),
            HashSet::new(),
            Vec::new(),
            limits,
            http,
            None,
        )
    }

    fn ensure_capability(
        &self,
        capability: ExtensionCapability,
    ) -> std::result::Result<(), HostToolError> {
        if self.context.kind == InvocationKind::RoomMessageObserve
            && capability == ExtensionCapability::HostMessageSend
        {
            return Err(HostToolError::denied(
                DisplayText::try_new(
                    "room observations return effects; message sending is unavailable",
                )
                .expect("static denial"),
            ));
        }
        if self.grants.contains(&capability) {
            Ok(())
        } else {
            Err(HostToolError::denied(
                DisplayText::try_new(format!(
                    "missing extension capability {}",
                    capability.as_str()
                ))
                .expect("capability denial message is non-empty"),
            ))
        }
    }

    fn ensure_command_invocation(
        &self,
        tool_name: &'static str,
    ) -> std::result::Result<(), HostToolError> {
        if self.context.kind == InvocationKind::Command {
            Ok(())
        } else {
            Err(HostToolError::denied(
                DisplayText::try_new(format!(
                    "{tool_name} is only available during ad-hoc command execution"
                ))
                .expect("private host-tool denial message is non-empty"),
            ))
        }
    }
}

impl Default for HostState {
    fn default() -> Self {
        Self::for_init()
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
    async fn log(
        &mut self,
        level: LogLevel,
        context: String,
        message: String,
    ) -> wasmtime::Result<()> {
        // Background observations and inference invocations may contain private
        // prompts, message bodies, model outputs, or credential metadata.
        // Guest log forwarding is unconditionally suppressed for them.
        if self.context.kind == InvocationKind::RoomMessageObserve
            || self.context.kind == InvocationKind::Command
            || self.inference_invocation
        {
            return Ok(());
        }
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

impl HostToolsHost for HostState {
    async fn list_channels(
        &mut self,
        request: wit_types::ListChannelsRequest,
    ) -> wasmtime::Result<
        std::result::Result<wit_types::ListChannelsResponse, wit_types::HostToolError>,
    > {
        let result = match self.ensure_capability(ExtensionCapability::HostChannelsRead) {
            Ok(()) => match request.try_into() {
                Ok(request) => self.tools.list_channels(&self.context, request).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        Ok(result.map(Into::into).map_err(Into::into))
    }

    async fn list_spaces(
        &mut self,
        request: wit_types::ListSpacesRequest,
    ) -> wasmtime::Result<
        std::result::Result<wit_types::ListSpacesResponse, wit_types::HostToolError>,
    > {
        let result = match self.ensure_capability(ExtensionCapability::HostSpacesRead) {
            Ok(()) => match request.try_into() {
                Ok(request) => self.tools.list_spaces(&self.context, request).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        Ok(result.map(Into::into).map_err(Into::into))
    }

    async fn list_room_members(
        &mut self,
        request: wit_types::ListRoomMembersRequest,
    ) -> wasmtime::Result<
        std::result::Result<wit_types::ListRoomMembersResponse, wit_types::HostToolError>,
    > {
        let result = match self.ensure_capability(ExtensionCapability::HostMembersRead) {
            Ok(()) => match request.try_into() {
                Ok(request) => self.tools.list_room_members(&self.context, request).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        Ok(result.map(Into::into).map_err(Into::into))
    }

    async fn get_presence(
        &mut self,
        request: wit_types::GetPresenceRequest,
    ) -> wasmtime::Result<
        std::result::Result<wit_types::GetPresenceResponse, wit_types::HostToolError>,
    > {
        let result = match self
            .ensure_capability(ExtensionCapability::HostPresenceRead)
            .and_then(|()| self.ensure_command_invocation("presence"))
        {
            Ok(()) => match request.try_into() {
                Ok(request) => self.tools.get_presence(&self.context, request).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        Ok(result.map(Into::into).map_err(Into::into))
    }

    async fn get_roster(
        &mut self,
        request: wit_types::GetRosterRequest,
    ) -> wasmtime::Result<std::result::Result<wit_types::GetRosterResponse, wit_types::HostToolError>>
    {
        let result = match self
            .ensure_capability(ExtensionCapability::HostRosterRead)
            .and_then(|()| self.ensure_command_invocation("roster"))
        {
            Ok(()) => match request.try_into() {
                Ok(request) => self.tools.get_roster(&self.context, request).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        Ok(result.map(Into::into).map_err(Into::into))
    }

    async fn query_mam(
        &mut self,
        query: wit_types::MamQuery,
    ) -> wasmtime::Result<std::result::Result<wit_types::MamQueryResponse, wit_types::HostToolError>>
    {
        let result = match self.ensure_capability(ExtensionCapability::HostMamRead) {
            Ok(()) => match query.try_into() {
                Ok(query) => self.tools.query_mam(&self.context, query).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        Ok(result.map(Into::into).map_err(Into::into))
    }

    async fn send_message(
        &mut self,
        request: wit_types::SendMessageRequest,
    ) -> wasmtime::Result<
        std::result::Result<wit_types::SendMessageResponse, wit_types::HostToolError>,
    > {
        let result = match self.ensure_capability(ExtensionCapability::HostMessageSend) {
            Ok(()) => match request.try_into() {
                Ok(request) => self.tools.send_message(&self.context, request).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        Ok(result.map(Into::into).map_err(Into::into))
    }

    async fn pubsub_get_items(
        &mut self,
        request: wit_types::PubsubGetItemsRequest,
    ) -> wasmtime::Result<
        std::result::Result<wit_types::PubsubGetItemsResponse, wit_types::HostToolError>,
    > {
        let result = match self.ensure_capability(ExtensionCapability::PubSubPublish) {
            Ok(()) => match request.try_into() {
                Ok(request) => self.tools.pubsub_get_items(&self.context, request).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        Ok(result.map(Into::into).map_err(Into::into))
    }
}

impl RuntimeHost for HostState {
    async fn get_config(&mut self) -> wasmtime::Result<String> {
        Ok(self.config.clone())
    }

    async fn http_request(
        &mut self,
        request: wit_types::OutgoingHttpRequest,
    ) -> wasmtime::Result<std::result::Result<wit_types::HttpResponse, wit_types::HostToolError>>
    {
        if self.inference_invocation {
            return Ok(Err(HostToolError::denied(
                DisplayText::try_new("outbound.http.request is disabled for inference invocations")
                    .expect("static denial"),
            )
            .into()));
        }
        let result = match self
            .validate_delivery()
            .await
            .and_then(|()| self.ensure_capability(ExtensionCapability::OutboundHttpRequest))
        {
            Ok(()) if self.http_requests < self.runtime_limits.http_max_requests => {
                self.http_requests += 1;
                execute_runtime_http_request(
                    request,
                    &self.allowed_http_origins,
                    &self.http,
                    &self.runtime_limits,
                )
                .await
            }
            Ok(()) => Err(HostToolError::denied(
                DisplayText::try_new("extension HTTP request limit exceeded")
                    .expect("static denial"),
            )),
            Err(error) => Err(error),
        };
        Ok(result.map_err(Into::into))
    }

    async fn current_timestamp(&mut self) -> wasmtime::Result<String> {
        Ok(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    }
}

impl super::waddle::extension::delivery::Host for HostState {}

impl super::waddle::extension::delivery::HostDeliveryKey for HostState {
    async fn validate(
        &mut self,
        resource: Resource<DeliveryKey>,
    ) -> wasmtime::Result<Result<(), wit_types::HostToolError>> {
        let key = self.table.get(&resource)?;
        Ok(key
            .capability
            .validate(&self.context, &key.source)
            .await
            .map_err(Into::into))
    }

    async fn drop(&mut self, resource: Resource<DeliveryKey>) -> wasmtime::Result<()> {
        self.table.delete(resource)?;
        Ok(())
    }
}

impl InferenceHost for HostState {
    async fn start_stream(
        &mut self,
        request: wit_inference::InferenceRequest,
    ) -> wasmtime::Result<Result<Resource<InferenceStream>, wit_inference::InferenceError>> {
        let domain_req = HostInferenceRequest {
            credential: match request.credential {
                wit_inference::CredentialRef::Tenant(name) => InferenceCredentialRef::Tenant(name),
                wit_inference::CredentialRef::InvokingUser(provider) => {
                    InferenceCredentialRef::InvokingUser(provider)
                }
            },
            provider: request.provider.value,
            deployment: request.deployment,
            model: request.model.value,
            messages: request
                .messages
                .into_iter()
                .map(|m| HostInferenceMessage {
                    role: m.role,
                    content: m.content,
                })
                .collect(),
            max_output_tokens: request.max_output_tokens,
        };

        match self.inference.start_stream(&self.context, domain_req).await {
            Ok(opened) => {
                let resource = self.table.push(InferenceStream::from_opened(opened))?;
                Ok(Ok(resource))
            }
            Err(err) => Ok(Err(err.into())),
        }
    }
}

impl HostInferenceStream for HostState {
    async fn next_chunk(
        &mut self,
        resource: Resource<InferenceStream>,
    ) -> wasmtime::Result<
        Result<Option<wit_inference::InferenceChunk>, wit_inference::InferenceError>,
    > {
        let stream = self.table.get_mut(&resource)?;
        match stream.next_chunk().await {
            Ok(Some(chunk)) => {
                if let Some(chunk_tx) = &self.chunk_tx {
                    let _ = chunk_tx.try_send(chunk.delta.clone());
                }
                Ok(Ok(Some(chunk.into())))
            }
            Ok(None) => Ok(Ok(None)),
            Err(err) => Ok(Err(err.into())),
        }
    }

    async fn terminal_usage(
        &mut self,
        resource: Resource<InferenceStream>,
    ) -> wasmtime::Result<Option<wit_inference::VerifiedUsage>> {
        let stream = self.table.get(&resource)?;
        Ok(stream.terminal_usage().map(Into::into))
    }

    async fn drop(&mut self, resource: Resource<InferenceStream>) -> wasmtime::Result<()> {
        // Removing from ResourceTable drops InferenceStream synchronously, which
        // fires `Drop::drop` -> `cancel.cancel_sync()` unconditionally.
        let _ = self.table.delete(resource)?;
        Ok(())
    }
}

impl From<HostInferenceChunk> for wit_inference::InferenceChunk {
    fn from(value: HostInferenceChunk) -> Self {
        Self {
            delta: value.delta,
            sequence: value.sequence,
            finish_reason: value.finish_reason,
        }
    }
}

impl From<VerifiedInferenceUsage> for wit_inference::VerifiedUsage {
    fn from(value: VerifiedInferenceUsage) -> Self {
        Self {
            provider: wit_types::ProviderId {
                value: value.provider,
            },
            model: wit_types::ModelId { value: value.model },
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            cost_microcents: value.cost_microcents,
        }
    }
}

impl From<HostInferenceErrorCode> for wit_inference::InferenceErrorCode {
    fn from(value: HostInferenceErrorCode) -> Self {
        match value {
            HostInferenceErrorCode::Denied => Self::Denied,
            HostInferenceErrorCode::InvalidRequest => Self::InvalidRequest,
            HostInferenceErrorCode::Canceled => Self::Canceled,
            HostInferenceErrorCode::RateLimited => Self::RateLimited,
            HostInferenceErrorCode::TemporaryFailure => Self::TemporaryFailure,
            HostInferenceErrorCode::ChunkLimitExceeded => Self::ChunkLimitExceeded,
        }
    }
}

impl From<HostInferenceError> for wit_inference::InferenceError {
    fn from(value: HostInferenceError) -> Self {
        Self {
            code: value.code.into(),
            message: value.message,
        }
    }
}

pub(crate) fn is_inference_plugin(plugin_id: &PluginId) -> bool {
    matches!(plugin_id.as_str(), "ai-chatbot" | "jev-judgments")
}
