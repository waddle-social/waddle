use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, error, info, trace, warn};
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use super::http::execute_runtime_http_request;
use super::waddle::extension::host_tools::Host as HostToolsHost;
use super::waddle::extension::runtime::Host as RuntimeHost;
use super::waddle::extension::types as wit_types;
use super::wasi::logging::logging::{Host as LoggingHost, Level as LogLevel};
use crate::host_tools::{
    DenyingExtensionHostTools, ExtensionHostTools, HostToolError, InvocationContext, InvocationKind,
};
use crate::types::{DisplayText, ExtensionCapability, PluginId, WaddleId};

/// Host state made available to every WASM instance for satisfying WASI imports.
pub struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    tools: Arc<dyn ExtensionHostTools>,
    context: InvocationContext,
    pub config: String,
    grants: HashSet<ExtensionCapability>,
    allowed_http_origins: Vec<String>,
    pub(super) limits: wasmtime::StoreLimits,
    runtime_limits: crate::config::RuntimeLimits,
    http: super::http::HttpRuntime,
    http_requests: u32,
}

impl HostState {
    pub(super) fn new(
        tools: Arc<dyn ExtensionHostTools>,
        context: InvocationContext,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
        runtime_limits: crate::config::RuntimeLimits,
        http: super::http::HttpRuntime,
    ) -> Self {
        let mut wasi_builder = WasiCtxBuilder::new();
        if context.kind != InvocationKind::RoomMessageObserve {
            wasi_builder.inherit_stderr();
        }
        let wasi = wasi_builder.build();
        let limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(runtime_limits.memory_bytes)
            .table_elements(20_000)
            .tables(8)
            .memories(4)
            .instances(16)
            .trap_on_grow_failure(true)
            .build();
        Self {
            wasi,
            table: ResourceTable::new(),
            tools,
            context,
            config,
            grants,
            allowed_http_origins,
            limits,
            runtime_limits,
            http,
            http_requests: 0,
        }
    }

    pub(super) fn for_init() -> Self {
        Self::for_init_with(
            crate::config::RuntimeLimits::default(),
            super::http::HttpRuntime::new().expect("static HTTP client configuration"),
        )
    }

    pub(super) fn for_init_with(
        limits: crate::config::RuntimeLimits,
        http: super::http::HttpRuntime,
    ) -> Self {
        Self::new(
            Arc::new(DenyingExtensionHostTools),
            InvocationContext {
                waddle_id: WaddleId::new("init").expect("static waddle id is valid"),
                plugin_id: PluginId::new("initializing-extension")
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
                DisplayText::new(
                    "room observations return effects; message sending is unavailable",
                )
                .expect("static denial"),
            ));
        }
        if self.grants.contains(&capability) {
            Ok(())
        } else {
            Err(HostToolError::denied(
                DisplayText::new(format!(
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
                DisplayText::new(format!(
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
        // Background observations may contain private message bodies and provider credentials.
        // Their typed outcome is logged by the host; arbitrary guest logs are suppressed.
        if self.context.kind == InvocationKind::RoomMessageObserve {
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
        let result = match self.ensure_capability(ExtensionCapability::OutboundHttpRequest) {
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
                DisplayText::new("extension HTTP request limit exceeded").expect("static denial"),
            )),
            Err(error) => Err(error),
        };
        Ok(result.map_err(Into::into))
    }

    async fn current_timestamp(&mut self) -> wasmtime::Result<String> {
        Ok(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    }
}
