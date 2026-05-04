use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::BytesMut;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use tracing::{debug, error, info, trace, warn};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use xmpp_parsers::jid::BareJid;

use crate::host_tools as host_domain;
use crate::host_tools::{
    DenyingExtensionHostTools, ExtensionHostTools, HostToolError, HostToolErrorCode,
    InvocationContext, InvocationKind,
};
use crate::types::{
    ActionBlock, ActionId, ArtifactReference, BodyRange, CommandAction, CommandDescriptor,
    CommandInvocation, CommandNode, CommandScope, CommandSessionId, DataForm, DataFormField,
    DataFormType, DataFormValue, DisplayText, EnrichmentId, ExtensionCapability, ExtensionEffect,
    ExtensionEnvelope, ExtensionEvent, ExtensionManifest, ExtensionPayload, ExtensionResponse,
    ExtensionRouteDescriptor, ExtensionRouteScope, ExtensionRouteSurface, FormFieldOption,
    FormFieldType, FormFieldValue, FullJidValue, ImageBlock, LaunchContext, LaunchDescriptor,
    LaunchId, LaunchInvocation, LinkTarget, ListId, ListItem, ListItemId, ListView, MediaType,
    MessageContext, MessageEnrichment, MessageHook, MessageSource, PayloadNamespace, PayloadRoot,
    PayloadRule, PayloadSurface, PluginId, PluginVersion, PubSubItemId, PubSubNode, PubSubPublish,
    ReplyTarget, RoomJid, RouteId, Sha256Digest, StanzaId, TextBlock, TextStyle, ThreadId,
    Timestamp, UiActionId, UiBlock, UiView, UiViewId, Url, WaddleId, XmlAttribute, XmlElement,
    XmlNode,
};

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "waddle-extension",
    imports: { default: async | tracing | trappable },
    exports: { default: async },
    with: {
        "wasi:io": wasmtime_wasi::p2::bindings::io,
        "wasi:clocks": wasmtime_wasi::p2::bindings::clocks,
    },
});

const EXTENSION_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const EXTENSION_HTTP_MAX_BODY_BYTES: u64 = 1024 * 1024;

use self::exports::waddle::extension as wit_exports;
use self::waddle::extension::host_tools::Host as HostToolsHost;
use self::waddle::extension::runtime::Host as RuntimeHost;
use self::waddle::extension::types as wit_types;
use self::wasi::logging::logging::{Host as LoggingHost, Level as LogLevel};

/// Host state made available to every WASM instance for satisfying WASI imports.
pub struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    tools: Arc<dyn ExtensionHostTools>,
    context: InvocationContext,
    pub config: String,
    grants: HashSet<ExtensionCapability>,
    allowed_http_origins: Vec<String>,
}

impl HostState {
    fn new(
        tools: Arc<dyn ExtensionHostTools>,
        context: InvocationContext,
        config: String,
        grants: HashSet<ExtensionCapability>,
        allowed_http_origins: Vec<String>,
    ) -> Self {
        let wasi = WasiCtxBuilder::new().inherit_stderr().build();
        Self {
            wasi,
            table: ResourceTable::new(),
            tools,
            context,
            config,
            grants,
            allowed_http_origins,
        }
    }

    fn for_init() -> Self {
        Self::new(
            Arc::new(DenyingExtensionHostTools),
            InvocationContext {
                waddle_id: WaddleId::new("init").expect("static waddle id is valid"),
                plugin_id: PluginId::new("initializing-extension")
                    .expect("static plugin id is valid"),
                requester: None,
                source_room: None,
                kind: InvocationKind::Launch,
            },
            String::new(),
            HashSet::new(),
            Vec::new(),
        )
    }

    fn ensure_capability(
        &self,
        capability: ExtensionCapability,
    ) -> std::result::Result<(), HostToolError> {
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
            Ok(()) => execute_runtime_http_request(request, &self.allowed_http_origins).await,
            Err(error) => Err(error),
        };
        Ok(result.map_err(Into::into))
    }

    async fn current_timestamp(&mut self) -> wasmtime::Result<String> {
        Ok(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    }
}

async fn execute_runtime_http_request(
    request: wit_types::OutgoingHttpRequest,
    allowed_origins: &[String],
) -> std::result::Result<wit_types::HttpResponse, HostToolError> {
    const MAX_EXTENSION_HTTP_REQUEST_BODY_BYTES: usize = 256 * 1024;
    let url = request.url.value;
    let parsed = reqwest::Url::parse(&url).map_err(|_| {
        HostToolError::invalid_request(
            DisplayText::new("extension HTTP request URL is invalid")
                .expect("static HTTP error is non-empty"),
        )
    })?;
    if parsed.scheme() != "https" {
        return Err(HostToolError::invalid_request(
            DisplayText::new("extension HTTP requests must use https://")
                .expect("static HTTP error is non-empty"),
        ));
    }
    let origin = http_origin(&parsed).ok_or_else(|| {
        HostToolError::invalid_request(
            DisplayText::new("extension HTTP request URL must include a host")
                .expect("static HTTP error is non-empty"),
        )
    })?;
    if !allowed_origins
        .iter()
        .filter_map(|allowed| normalize_http_origin(allowed))
        .any(|allowed| allowed == origin)
    {
        return Err(HostToolError::denied(
            DisplayText::new("extension HTTP origin is not allowed")
                .expect("static HTTP error is non-empty"),
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(EXTENSION_HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()
        .map_err(|error| HostToolError {
            code: HostToolErrorCode::TemporaryFailure,
            message: DisplayText::new(format!("extension HTTP client failed: {error}"))
                .expect("HTTP error is non-empty"),
        })?;
    let mut builder = match request.method {
        wit_types::HttpMethod::Get => client.get(&url),
        wit_types::HttpMethod::Post => client.post(&url),
    };
    builder = apply_runtime_http_headers(builder, request.headers)?;
    if let Some(body) = request.body {
        if body.len() > MAX_EXTENSION_HTTP_REQUEST_BODY_BYTES {
            return Err(HostToolError::invalid_request(
                DisplayText::new("extension HTTP request body is too large")
                    .expect("static HTTP error is non-empty"),
            ));
        }
        builder = builder.body(body);
    }
    let response = builder.send().await.map_err(|error| HostToolError {
        code: HostToolErrorCode::TemporaryFailure,
        message: DisplayText::new(format!("extension HTTP request failed: {error}"))
            .expect("HTTP error is non-empty"),
    })?;
    let status = response.status().as_u16();
    let mut stream = response.bytes_stream();
    let mut body = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| HostToolError {
            code: HostToolErrorCode::TemporaryFailure,
            message: DisplayText::new(format!("extension HTTP response body failed: {error}"))
                .expect("HTTP error is non-empty"),
        })?;
        if body.len() + chunk.len() > EXTENSION_HTTP_MAX_BODY_BYTES as usize {
            return Err(HostToolError::invalid_request(
                DisplayText::new("extension HTTP response body exceeded limit")
                    .expect("static HTTP error is non-empty"),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let body = String::from_utf8(body.to_vec()).map_err(|error| HostToolError {
        code: HostToolErrorCode::TemporaryFailure,
        message: DisplayText::new(format!(
            "extension HTTP response body was not UTF-8: {error}"
        ))
        .expect("HTTP error is non-empty"),
    })?;
    Ok(wit_types::HttpResponse { status, body })
}

fn apply_runtime_http_headers(
    mut builder: reqwest::RequestBuilder,
    headers: Vec<wit_types::HttpHeader>,
) -> std::result::Result<reqwest::RequestBuilder, HostToolError> {
    builder = builder.header("accept-encoding", "identity");
    for header in headers {
        if header.name.trim().is_empty() {
            return Err(HostToolError::invalid_request(
                DisplayText::new("extension HTTP header name must be non-empty")
                    .expect("static HTTP error is non-empty"),
            ));
        }
        if is_disallowed_extension_http_header(&header.name) {
            return Err(HostToolError::invalid_request(
                DisplayText::new("extension HTTP header is controlled by the host")
                    .expect("static HTTP error is non-empty"),
            ));
        }
        builder = builder.header(header.name, header.value);
    }
    Ok(builder)
}

fn http_origin(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    let Some(port) = url.port() else {
        return Some(format!("{}://{}", url.scheme(), host));
    };
    Some(format!("{}://{}:{}", url.scheme(), host, port))
}

fn normalize_http_origin(value: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(value).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    http_origin(&parsed)
}

fn is_disallowed_extension_http_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "te"
            | "trailer"
            | "upgrade"
            | "keep-alive"
            | "accept-encoding"
            | "proxy-authorization"
            | "proxy-authenticate"
    )
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

// ---- type conversions between WIT-generated types and domain types ----

macro_rules! domain_newtype_to_wit {
    ($value:expr, $wit:ident) => {
        wit_types::$wit {
            value: $value.as_str().to_string(),
        }
    };
}

macro_rules! wit_newtype_to_domain {
    ($value:expr, $domain:ty) => {
        <$domain>::new($value.value).map_err(anyhow::Error::from)
    };
}

impl TryFrom<wit_exports::lifecycle::ExtensionManifest> for ExtensionManifest {
    type Error = anyhow::Error;

    fn try_from(value: wit_exports::lifecycle::ExtensionManifest) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, PluginId)?,
            name: wit_newtype_to_domain!(value.name, DisplayText)?,
            version: wit_newtype_to_domain!(value.version, PluginVersion)?,
            payloads: value
                .payloads
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
            capabilities: value.capabilities.into_iter().map(Into::into).collect(),
            commands: value
                .commands
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
            routes: value
                .routes
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
            pubsub_nodes: value
                .pubsub_nodes
                .into_iter()
                .map(|node| wit_newtype_to_domain!(node, PubSubNode))
                .collect::<Result<Vec<_>>>()?,
            artifact: value.artifact.map(TryInto::try_into).transpose()?,
        })
    }
}

impl TryFrom<wit_types::CommandDescriptor> for CommandDescriptor {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::CommandDescriptor) -> Result<Self> {
        Ok(Self {
            node: wit_newtype_to_domain!(value.node, CommandNode)?,
            name: wit_newtype_to_domain!(value.name, DisplayText)?,
            scope: value.scope.into(),
        })
    }
}

impl From<wit_types::CommandScope> for CommandScope {
    fn from(value: wit_types::CommandScope) -> Self {
        match value {
            wit_types::CommandScope::Global => CommandScope::Global,
            wit_types::CommandScope::Channel => CommandScope::Channel,
        }
    }
}

impl TryFrom<wit_types::ExtensionRouteDescriptor> for ExtensionRouteDescriptor {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ExtensionRouteDescriptor) -> Result<Self> {
        Ok(Self {
            plugin: wit_newtype_to_domain!(value.plugin, PluginId)?,
            id: wit_newtype_to_domain!(value.id, RouteId)?,
            label: wit_newtype_to_domain!(value.label, DisplayText)?,
            scope: value.scope.into(),
            surface: value.surface.into(),
            state_node: wit_newtype_to_domain!(value.state_node, PubSubNode)?,
            payload_namespace: wit_newtype_to_domain!(value.payload_namespace, PayloadNamespace)?,
        })
    }
}

impl From<wit_types::ExtensionRouteScope> for ExtensionRouteScope {
    fn from(value: wit_types::ExtensionRouteScope) -> Self {
        match value {
            wit_types::ExtensionRouteScope::Channel => Self::Channel,
        }
    }
}

impl From<wit_types::ExtensionRouteSurface> for ExtensionRouteSurface {
    fn from(value: wit_types::ExtensionRouteSurface) -> Self {
        match value {
            wit_types::ExtensionRouteSurface::Gallery => Self::Gallery,
            wit_types::ExtensionRouteSurface::ListView => Self::List,
        }
    }
}

impl TryFrom<wit_types::PayloadRule> for PayloadRule {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::PayloadRule) -> Result<Self> {
        Ok(Self {
            surface: value.surface.into(),
            root: value.root.try_into()?,
        })
    }
}

impl From<wit_types::PayloadSurface> for PayloadSurface {
    fn from(value: wit_types::PayloadSurface) -> Self {
        match value {
            wit_types::PayloadSurface::MessageEnrichment => Self::MessageEnrichment,
            wit_types::PayloadSurface::LaunchPayload => Self::LaunchPayload,
            wit_types::PayloadSurface::PubsubItem => Self::PubSubItem,
        }
    }
}

impl From<ExtensionEvent> for wit_types::ExtensionEvent {
    fn from(value: ExtensionEvent) -> Self {
        match value {
            ExtensionEvent::MessageHook(event) => Self::MessageHook(event.into()),
            ExtensionEvent::Command(event) => Self::Command(event.into()),
            ExtensionEvent::Launch(event) => Self::Launch(event.into()),
        }
    }
}

impl From<MessageHook> for wit_types::MessageHook {
    fn from(value: MessageHook) -> Self {
        Self {
            context: value.context.into(),
            body: value.body.into(),
            links: value.links.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<MessageContext> for wit_types::MessageContext {
    fn from(value: MessageContext) -> Self {
        Self {
            waddle_id: value.waddle_id.into(),
            stanza_id: value.stanza_id.map(Into::into),
            room: value.room.map(Into::into),
            sender: value.sender.map(Into::into),
            thread_id: value.thread_id.map(Into::into),
            reply_to: value.reply_to.map(Into::into),
        }
    }
}

impl From<ReplyTarget> for wit_types::ReplyTarget {
    fn from(value: ReplyTarget) -> Self {
        Self {
            id: value.id.into(),
            to: value.to.map(Into::into),
        }
    }
}

impl From<LinkTarget> for wit_types::LinkTarget {
    fn from(value: LinkTarget) -> Self {
        Self {
            url: value.url.into(),
            range: wit_types::BodyRange {
                start: value.range.start,
                end: value.range.end,
            },
        }
    }
}

impl From<CommandInvocation> for wit_types::CommandInvocation {
    fn from(value: CommandInvocation) -> Self {
        Self {
            waddle_id: value.waddle_id.into(),
            room: value.room.map(Into::into),
            requester: value.requester.into(),
            command_node: value.command_node.into(),
            session_id: value.session_id.map(Into::into),
            action: value.action.map(Into::into),
            form: value.form.map(Into::into),
            fields: value.fields.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<LaunchInvocation> for wit_types::LaunchInvocation {
    fn from(value: LaunchInvocation) -> Self {
        Self {
            context: value.context.into(),
            requester: value.requester.into(),
            launch_id: value.launch_id.into(),
            session_id: value.session_id.map(Into::into),
            action: value.action.map(Into::into),
            form: value.form.map(Into::into),
            fields: value.fields.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<FormFieldValue> for wit_types::FormFieldValue {
    fn from(value: FormFieldValue) -> Self {
        Self {
            name: value.name.into(),
            values: value.values.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<CommandAction> for wit_types::CommandAction {
    fn from(value: CommandAction) -> Self {
        match value {
            CommandAction::Execute => Self::Execute,
            CommandAction::Next => Self::Next,
            CommandAction::Prev => Self::Prev,
            CommandAction::Complete => Self::Complete,
            CommandAction::Cancel => Self::Cancel,
        }
    }
}

impl From<LaunchContext> for wit_types::LaunchContext {
    fn from(value: LaunchContext) -> Self {
        Self {
            waddle_id: value.waddle_id.into(),
            room: value.room.map(Into::into),
            source_stanza_id: value.source_stanza_id.map(Into::into),
        }
    }
}

impl From<DataForm> for wit_types::DataForm {
    fn from(value: DataForm) -> Self {
        Self {
            form_type: value.form_type.into(),
            title: value.title.map(Into::into),
            instructions: value.instructions.into_iter().map(Into::into).collect(),
            fields: value.fields.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<DataFormType> for wit_types::DataFormType {
    fn from(value: DataFormType) -> Self {
        match value {
            DataFormType::Form => Self::Form,
            DataFormType::Submit => Self::Submit,
            DataFormType::Cancel => Self::Cancel,
            DataFormType::Result => Self::ResultForm,
        }
    }
}

impl From<FormFieldType> for wit_types::FormFieldType {
    fn from(value: FormFieldType) -> Self {
        match value {
            FormFieldType::Boolean => Self::Boolean,
            FormFieldType::Fixed => Self::Fixed,
            FormFieldType::Hidden => Self::Hidden,
            FormFieldType::JidMulti => Self::JidMulti,
            FormFieldType::JidSingle => Self::JidSingle,
            FormFieldType::ListMulti => Self::ListMulti,
            FormFieldType::ListSingle => Self::ListSingle,
            FormFieldType::TextMulti => Self::TextMulti,
            FormFieldType::TextPrivate => Self::TextPrivate,
            FormFieldType::TextSingle => Self::TextSingle,
        }
    }
}

impl From<DataFormField> for wit_types::DataFormField {
    fn from(value: DataFormField) -> Self {
        Self {
            name: value.name.into(),
            field_type: value.field_type.into(),
            label: value.label.map(Into::into),
            required: value.required,
            values: value.values.into_iter().map(Into::into).collect(),
            options: value.options.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<FormFieldOption> for wit_types::FormFieldOption {
    fn from(value: FormFieldOption) -> Self {
        Self {
            label: value.label.map(Into::into),
            value: value.value.into(),
        }
    }
}

impl From<DataFormValue> for wit_types::DataFormValue {
    fn from(value: DataFormValue) -> Self {
        Self {
            value: value.into_string(),
        }
    }
}

impl From<ExtensionPayload> for wit_types::ExtensionPayload {
    fn from(value: ExtensionPayload) -> Self {
        let mut tokens = Vec::new();
        push_xml_tokens(&value.root, &mut tokens);
        Self {
            namespace: value.namespace.into(),
            root: PayloadRoot {
                namespace: value.root.namespace.clone(),
                local_name: value.root.local_name.clone(),
            }
            .into(),
            tokens,
        }
    }
}

impl From<XmlElement> for wit_types::XmlElement {
    fn from(value: XmlElement) -> Self {
        Self {
            namespace: value.namespace.into(),
            local_name: value.local_name,
            attributes: value.attributes.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<XmlAttribute> for wit_types::XmlAttribute {
    fn from(value: XmlAttribute) -> Self {
        Self {
            namespace: value.namespace.map(Into::into),
            local_name: value.local_name,
            value: value.value,
        }
    }
}

impl From<PayloadRoot> for wit_types::PayloadRoot {
    fn from(value: PayloadRoot) -> Self {
        Self {
            namespace: value.namespace.into(),
            local_name: value.local_name,
        }
    }
}

impl TryFrom<wit_types::PayloadRoot> for PayloadRoot {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::PayloadRoot) -> Result<Self> {
        PayloadRoot::new(
            wit_newtype_to_domain!(value.namespace, PayloadNamespace)?,
            value.local_name,
        )
        .map_err(anyhow::Error::from)
    }
}

fn push_xml_tokens(element: &XmlElement, tokens: &mut Vec<wit_types::XmlToken>) {
    tokens.push(wit_types::XmlToken::StartElement(wit_types::XmlElement {
        namespace: element.namespace.clone().into(),
        local_name: element.local_name.clone(),
        attributes: element
            .attributes
            .clone()
            .into_iter()
            .map(Into::into)
            .collect(),
    }));
    for child in &element.children {
        match child {
            XmlNode::Element(child) => push_xml_tokens(child, tokens),
            XmlNode::Text(text) => tokens.push(wit_types::XmlToken::Text(text.clone())),
        }
    }
    tokens.push(wit_types::XmlToken::EndElement);
}

macro_rules! impl_domain_newtype_to_wit {
    ($domain:ty, $wit:ident) => {
        impl From<$domain> for wit_types::$wit {
            fn from(value: $domain) -> Self {
                domain_newtype_to_wit!(value, $wit)
            }
        }
    };
}

impl_domain_newtype_to_wit!(ActionId, ActionId);
impl_domain_newtype_to_wit!(CommandNode, CommandNode);
impl_domain_newtype_to_wit!(CommandSessionId, CommandSessionId);
impl_domain_newtype_to_wit!(DisplayText, DisplayText);
impl_domain_newtype_to_wit!(EnrichmentId, EnrichmentId);
impl_domain_newtype_to_wit!(FullJidValue, FullJid);
impl_domain_newtype_to_wit!(LaunchId, LaunchId);
impl_domain_newtype_to_wit!(ListId, ListId);
impl_domain_newtype_to_wit!(ListItemId, ListItemId);
impl_domain_newtype_to_wit!(MediaType, MediaType);
impl_domain_newtype_to_wit!(PayloadNamespace, PayloadNamespace);
impl_domain_newtype_to_wit!(PluginId, PluginId);
impl_domain_newtype_to_wit!(PubSubItemId, PubsubItemId);
impl_domain_newtype_to_wit!(PubSubNode, PubsubNode);
impl_domain_newtype_to_wit!(RoomJid, RoomJid);
impl_domain_newtype_to_wit!(Sha256Digest, Sha256Digest);
impl_domain_newtype_to_wit!(StanzaId, StanzaId);
impl_domain_newtype_to_wit!(ThreadId, ThreadId);
impl_domain_newtype_to_wit!(Timestamp, Timestamp);
impl_domain_newtype_to_wit!(UiActionId, UiActionId);
impl_domain_newtype_to_wit!(UiViewId, UiViewId);
impl_domain_newtype_to_wit!(Url, Url);
impl_domain_newtype_to_wit!(WaddleId, WaddleId);

impl TryFrom<wit_types::ExtensionResponse> for ExtensionResponse {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ExtensionResponse) -> Result<Self> {
        Ok(Self {
            effects: value
                .effects
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl TryFrom<wit_types::ExtensionEffect> for ExtensionEffect {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ExtensionEffect) -> Result<Self> {
        Ok(match value {
            wit_types::ExtensionEffect::EnrichMessage(envelope) => {
                Self::EnrichMessage(envelope.try_into()?)
            }
            wit_types::ExtensionEffect::PublishPubsub(publish) => {
                Self::PublishPubSub(publish.try_into()?)
            }
            wit_types::ExtensionEffect::ReferenceArtifact(artifact) => {
                Self::ReferenceArtifact(artifact.try_into()?)
            }
            wit_types::ExtensionEffect::CommandForm(form) => Self::CommandForm(form.try_into()?),
            wit_types::ExtensionEffect::HostWarning(message) => {
                Self::HostWarning(wit_newtype_to_domain!(message, DisplayText)?)
            }
            wit_types::ExtensionEffect::Noop => Self::Noop,
        })
    }
}

impl TryFrom<wit_types::ReplyTarget> for ReplyTarget {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ReplyTarget) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, StanzaId)?,
            to: value
                .to
                .map(|to| wit_newtype_to_domain!(to, FullJidValue))
                .transpose()?,
        })
    }
}

impl TryFrom<wit_types::ExtensionEnvelope> for ExtensionEnvelope {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ExtensionEnvelope) -> Result<Self> {
        Ok(Self {
            version: value.version,
            enrichments: value
                .enrichments
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl TryFrom<wit_types::MessageEnrichment> for MessageEnrichment {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::MessageEnrichment) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, EnrichmentId)?,
            plugin: wit_newtype_to_domain!(value.plugin, PluginId)?,
            capability: value.capability.into(),
            payload_namespace: wit_newtype_to_domain!(value.payload_namespace, PayloadNamespace)?,
            created_at: wit_newtype_to_domain!(value.created_at, Timestamp)?,
            source: value.source.map(TryInto::try_into).transpose()?,
            ui: value
                .ui
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
            payloads: value
                .payloads
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
            launches: value
                .launches
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl From<wit_types::ExtensionCapability> for ExtensionCapability {
    fn from(value: wit_types::ExtensionCapability) -> Self {
        match value {
            wit_types::ExtensionCapability::MessageEnrich => Self::MessageEnrich,
            wit_types::ExtensionCapability::MessageObserve => Self::MessageObserve,
            wit_types::ExtensionCapability::HostChannelsRead => Self::HostChannelsRead,
            wit_types::ExtensionCapability::HostSpacesRead => Self::HostSpacesRead,
            wit_types::ExtensionCapability::HostMembersRead => Self::HostMembersRead,
            wit_types::ExtensionCapability::HostPresenceRead => Self::HostPresenceRead,
            wit_types::ExtensionCapability::HostMamRead => Self::HostMamRead,
            wit_types::ExtensionCapability::HostRosterRead => Self::HostRosterRead,
            wit_types::ExtensionCapability::HostMessageSend => Self::HostMessageSend,
            wit_types::ExtensionCapability::OutboundHttpRequest => Self::OutboundHttpRequest,
            wit_types::ExtensionCapability::Commands => Self::Commands,
            wit_types::ExtensionCapability::Launch => Self::Launch,
            wit_types::ExtensionCapability::PubsubPublish => Self::PubSubPublish,
            wit_types::ExtensionCapability::ArtifactReference => Self::ArtifactReference,
            wit_types::ExtensionCapability::UiDeclarative => Self::UiDeclarative,
        }
    }
}

impl TryFrom<wit_types::ListChannelsRequest> for host_domain::ListChannelsRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::ListChannelsRequest) -> Result<Self, Self::Error> {
        let _ = value;
        Ok(Self)
    }
}

impl From<host_domain::ListChannelsResponse> for wit_types::ListChannelsResponse {
    fn from(value: host_domain::ListChannelsResponse) -> Self {
        Self {
            channels: value.channels.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::ChannelSummary> for wit_types::ChannelSummary {
    fn from(value: host_domain::ChannelSummary) -> Self {
        Self {
            room: RoomJid::new(value.room.to_string())
                .expect("host returned valid bare room jid")
                .into(),
            name: value.name.map(Into::into),
            description: value.description.map(Into::into),
        }
    }
}

impl TryFrom<wit_types::ListSpacesRequest> for host_domain::ListSpacesRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::ListSpacesRequest) -> Result<Self, Self::Error> {
        let _ = value;
        Ok(Self)
    }
}

impl From<host_domain::ListSpacesResponse> for wit_types::ListSpacesResponse {
    fn from(value: host_domain::ListSpacesResponse) -> Self {
        Self {
            spaces: value.spaces.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::SpaceSummary> for wit_types::SpaceSummary {
    fn from(value: host_domain::SpaceSummary) -> Self {
        Self {
            service: wit_types::BareJid {
                value: value.service.to_string(),
            },
            node: value.node.into(),
            name: value.name.map(Into::into),
            description: value.description.map(Into::into),
            channels: value.channels.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<wit_types::ListRoomMembersRequest> for host_domain::ListRoomMembersRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::ListRoomMembersRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            room: parse_bare_jid(value.room.value)?,
        })
    }
}

impl From<host_domain::ListRoomMembersResponse> for wit_types::ListRoomMembersResponse {
    fn from(value: host_domain::ListRoomMembersResponse) -> Self {
        Self {
            members: value.members.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::RoomMember> for wit_types::RoomMember {
    fn from(value: host_domain::RoomMember) -> Self {
        Self {
            room: RoomJid::new(value.room.to_string())
                .expect("host returned valid bare room jid")
                .into(),
            jid: wit_types::Jid {
                value: value.jid.to_string(),
            },
            nick: value.nick.map(Into::into),
            role: value.role.into(),
            affiliation: value.affiliation.into(),
        }
    }
}

impl From<host_domain::MucRole> for wit_types::MucRole {
    fn from(value: host_domain::MucRole) -> Self {
        match value {
            host_domain::MucRole::None => Self::None,
            host_domain::MucRole::Visitor => Self::Visitor,
            host_domain::MucRole::Participant => Self::Participant,
            host_domain::MucRole::Moderator => Self::Moderator,
        }
    }
}

impl From<host_domain::MucAffiliation> for wit_types::MucAffiliation {
    fn from(value: host_domain::MucAffiliation) -> Self {
        match value {
            host_domain::MucAffiliation::None => Self::None,
            host_domain::MucAffiliation::Outcast => Self::Outcast,
            host_domain::MucAffiliation::Member => Self::Member,
            host_domain::MucAffiliation::Admin => Self::Admin,
            host_domain::MucAffiliation::Owner => Self::Owner,
        }
    }
}

impl TryFrom<wit_types::GetPresenceRequest> for host_domain::GetPresenceRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::GetPresenceRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            subject: parse_bare_jid(value.subject.value)?,
        })
    }
}

impl From<host_domain::GetPresenceResponse> for wit_types::GetPresenceResponse {
    fn from(value: host_domain::GetPresenceResponse) -> Self {
        Self {
            resources: value.resources.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::PresenceState> for wit_types::PresenceState {
    fn from(value: host_domain::PresenceState) -> Self {
        Self {
            jid: wit_types::Jid {
                value: value.jid.to_string(),
            },
            availability: value.availability.into(),
            show: value.show.map(Into::into),
            status: value.status.map(Into::into),
            priority: value.priority,
        }
    }
}

impl From<host_domain::PresenceAvailability> for wit_types::PresenceAvailability {
    fn from(value: host_domain::PresenceAvailability) -> Self {
        match value {
            host_domain::PresenceAvailability::Available => Self::Available,
            host_domain::PresenceAvailability::Unavailable => Self::Unavailable,
        }
    }
}

impl From<host_domain::PresenceShow> for wit_types::PresenceShow {
    fn from(value: host_domain::PresenceShow) -> Self {
        match value {
            host_domain::PresenceShow::Chat => Self::Chat,
            host_domain::PresenceShow::Away => Self::Away,
            host_domain::PresenceShow::ExtendedAway => Self::Xa,
            host_domain::PresenceShow::DoNotDisturb => Self::Dnd,
        }
    }
}

impl TryFrom<wit_types::GetRosterRequest> for host_domain::GetRosterRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::GetRosterRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            owner: parse_bare_jid(value.owner.value)?,
        })
    }
}

impl From<host_domain::GetRosterResponse> for wit_types::GetRosterResponse {
    fn from(value: host_domain::GetRosterResponse) -> Self {
        Self {
            entries: value.entries.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::RosterEntry> for wit_types::RosterEntry {
    fn from(value: host_domain::RosterEntry) -> Self {
        Self {
            jid: wit_types::BareJid {
                value: value.jid.to_string(),
            },
            name: value.name.map(Into::into),
            subscription: value.subscription.into(),
            ask: value.ask.map(Into::into),
            groups: value.groups.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::RosterSubscription> for wit_types::RosterSubscription {
    fn from(value: host_domain::RosterSubscription) -> Self {
        match value {
            host_domain::RosterSubscription::None => Self::None,
            host_domain::RosterSubscription::To => Self::SubscribedTo,
            host_domain::RosterSubscription::From => Self::SubscribedFrom,
            host_domain::RosterSubscription::Both => Self::Both,
            host_domain::RosterSubscription::Remove => Self::Remove,
        }
    }
}

impl From<host_domain::RosterAsk> for wit_types::RosterAsk {
    fn from(value: host_domain::RosterAsk) -> Self {
        match value {
            host_domain::RosterAsk::Subscribe => Self::Subscribe,
        }
    }
}

impl TryFrom<wit_types::MamQuery> for host_domain::MamQuery {
    type Error = HostToolError;

    fn try_from(value: wit_types::MamQuery) -> Result<Self, Self::Error> {
        Ok(Self {
            target: value.target.try_into()?,
            start: value
                .start
                .map(|timestamp| parse_timestamp(timestamp.value))
                .transpose()?,
            end: value
                .end
                .map(|timestamp| parse_timestamp(timestamp.value))
                .transpose()?,
            thread_id: value
                .thread_id
                .map(|thread_id| ThreadId::new(thread_id.value))
                .transpose()
                .map_err(host_type_error)?,
            sender: value
                .sender
                .map(|sender| parse_bare_jid(sender.value))
                .transpose()?,
            text: value
                .text
                .map(|text| {
                    DisplayText::new(text.value).map_err(|error| {
                        HostToolError::invalid_request(
                            DisplayText::new(error.to_string())
                                .expect("type error message is non-empty"),
                        )
                    })
                })
                .transpose()?,
            max_results: value.max_results,
        })
    }
}

impl TryFrom<wit_types::MamTarget> for host_domain::MamTarget {
    type Error = HostToolError;

    fn try_from(value: wit_types::MamTarget) -> Result<Self, Self::Error> {
        Ok(match value {
            wit_types::MamTarget::Room(room) => Self::Room(parse_bare_jid(room.value)?),
            wit_types::MamTarget::Conversation(jid) => {
                Self::Conversation(parse_bare_jid(jid.value)?)
            }
        })
    }
}

impl From<host_domain::MamQueryResponse> for wit_types::MamQueryResponse {
    fn from(value: host_domain::MamQueryResponse) -> Self {
        Self {
            messages: value.messages.into_iter().map(Into::into).collect(),
            complete: value.complete,
        }
    }
}

impl From<host_domain::ArchivedMessage> for wit_types::ArchivedMessage {
    fn from(value: host_domain::ArchivedMessage) -> Self {
        Self {
            stanza_id: value.stanza_id.into(),
            from_jid: wit_types::Jid {
                value: value.from.to_string(),
            },
            to_jid: wit_types::Jid {
                value: value.to.to_string(),
            },
            sent_at: Timestamp::new(value.sent_at.to_rfc3339())
                .expect("rfc3339 timestamp is non-empty")
                .into(),
            body: value.body.map(Into::into),
            thread_id: value.thread_id.map(Into::into),
            reply_to: value.reply_to.map(Into::into),
        }
    }
}

impl TryFrom<wit_types::SendMessageRequest> for host_domain::SendMessageRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::SendMessageRequest) -> Result<Self, Self::Error> {
        let body = DisplayText::new(value.body.value).map_err(|error| {
            HostToolError::invalid_request(
                DisplayText::new(error.to_string()).expect("type error message is non-empty"),
            )
        })?;
        Ok(Self {
            target: value.target.try_into()?,
            body,
            thread_id: value
                .thread_id
                .map(|thread_id| ThreadId::new(thread_id.value))
                .transpose()
                .map_err(host_type_error)?,
            reply_to: value.reply_to.map(TryInto::try_into).transpose().map_err(
                |error: anyhow::Error| {
                    HostToolError::invalid_request(
                        DisplayText::new(error.to_string())
                            .expect("type error message is non-empty"),
                    )
                },
            )?,
            extensions: value
                .extensions
                .map(TryInto::try_into)
                .transpose()
                .map_err(|error: anyhow::Error| {
                    HostToolError::invalid_request(
                        DisplayText::new(error.to_string())
                            .expect("type error message is non-empty"),
                    )
                })?,
        })
    }
}

impl TryFrom<wit_types::MessageTarget> for host_domain::MessageTarget {
    type Error = HostToolError;

    fn try_from(value: wit_types::MessageTarget) -> Result<Self, Self::Error> {
        Ok(match value {
            wit_types::MessageTarget::Muc(room) => Self::Muc(parse_bare_jid(room.value)?),
            wit_types::MessageTarget::Direct(jid) => Self::Direct(parse_bare_jid(jid.value)?),
        })
    }
}

impl From<host_domain::SendMessageResponse> for wit_types::SendMessageResponse {
    fn from(value: host_domain::SendMessageResponse) -> Self {
        Self {
            stanza_id: value.stanza_id.into(),
        }
    }
}

impl From<HostToolError> for wit_types::HostToolError {
    fn from(value: HostToolError) -> Self {
        Self {
            code: value.code.into(),
            message: value.message.into(),
        }
    }
}

impl From<HostToolErrorCode> for wit_types::HostToolErrorCode {
    fn from(value: HostToolErrorCode) -> Self {
        match value {
            HostToolErrorCode::Denied => Self::Denied,
            HostToolErrorCode::InvalidRequest => Self::InvalidRequest,
            HostToolErrorCode::NotFound => Self::NotFound,
            HostToolErrorCode::Unsupported => Self::Unsupported,
            HostToolErrorCode::TemporaryFailure => Self::TemporaryFailure,
        }
    }
}

fn parse_bare_jid(value: String) -> std::result::Result<BareJid, HostToolError> {
    value.parse::<BareJid>().map_err(|_| {
        HostToolError::invalid_request(
            DisplayText::new("invalid bare JID").expect("static host-tool error is non-empty"),
        )
    })
}

fn parse_timestamp(value: String) -> std::result::Result<DateTime<Utc>, HostToolError> {
    DateTime::parse_from_rfc3339(&value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| {
            HostToolError::invalid_request(
                DisplayText::new("invalid RFC3339 timestamp")
                    .expect("static host-tool error is non-empty"),
            )
        })
}

fn host_type_error(error: crate::types::FrameworkTypeError) -> HostToolError {
    HostToolError::invalid_request(
        DisplayText::new(error.to_string()).expect("type error message is non-empty"),
    )
}

impl TryFrom<wit_types::MessageSource> for MessageSource {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::MessageSource) -> Result<Self> {
        Ok(Self {
            stanza_id: wit_newtype_to_domain!(value.stanza_id, StanzaId)?,
            body_range: value
                .body_range
                .map(|range| BodyRange::new(range.start, range.end))
                .transpose()?,
        })
    }
}

impl TryFrom<wit_types::LaunchDescriptor> for LaunchDescriptor {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::LaunchDescriptor) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, LaunchId)?,
            plugin: wit_newtype_to_domain!(value.plugin, PluginId)?,
            action: wit_newtype_to_domain!(value.action, ActionId)?,
            command_node: wit_newtype_to_domain!(value.command_node, CommandNode)?,
            label: wit_newtype_to_domain!(value.label, DisplayText)?,
            context: value.context.try_into()?,
            payloads: value
                .payloads
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
            fallback: value.fallback.map(TryInto::try_into).transpose()?,
            expires_at: value
                .expires_at
                .map(|expires_at| wit_newtype_to_domain!(expires_at, Timestamp))
                .transpose()?,
            token: None,
        })
    }
}

impl TryFrom<wit_types::LaunchContext> for LaunchContext {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::LaunchContext) -> Result<Self> {
        Ok(Self {
            waddle_id: wit_newtype_to_domain!(value.waddle_id, WaddleId)?,
            room: value
                .room
                .map(|room| wit_newtype_to_domain!(room, RoomJid))
                .transpose()?,
            source_stanza_id: value
                .source_stanza_id
                .map(|stanza_id| wit_newtype_to_domain!(stanza_id, StanzaId))
                .transpose()?,
        })
    }
}

impl TryFrom<wit_types::UiView> for UiView {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::UiView) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, UiViewId)?,
            title: value
                .title
                .map(|title| wit_newtype_to_domain!(title, DisplayText))
                .transpose()?,
            blocks: value
                .blocks
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl TryFrom<wit_types::UiBlock> for UiBlock {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::UiBlock) -> Result<Self> {
        Ok(match value {
            wit_types::UiBlock::Text(block) => Self::Text(block.try_into()?),
            wit_types::UiBlock::Image(block) => Self::Image(block.try_into()?),
            wit_types::UiBlock::Action(block) => Self::Action(block.try_into()?),
            wit_types::UiBlock::Form(form) => Self::Form(form.try_into()?),
            wit_types::UiBlock::ListBlock(list) => Self::List(list.try_into()?),
        })
    }
}

impl TryFrom<wit_types::TextBlock> for TextBlock {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::TextBlock) -> Result<Self> {
        Ok(Self {
            text: wit_newtype_to_domain!(value.text, DisplayText)?,
            style: value.style.into(),
        })
    }
}

impl From<wit_types::TextStyle> for TextStyle {
    fn from(value: wit_types::TextStyle) -> Self {
        match value {
            wit_types::TextStyle::Body => Self::Body,
            wit_types::TextStyle::Heading => Self::Heading,
            wit_types::TextStyle::Muted => Self::Muted,
            wit_types::TextStyle::Code => Self::Code,
        }
    }
}

impl TryFrom<wit_types::ImageBlock> for ImageBlock {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ImageBlock) -> Result<Self> {
        Ok(Self {
            artifact: value.artifact.try_into()?,
            alt: wit_newtype_to_domain!(value.alt, DisplayText)?,
        })
    }
}

impl TryFrom<wit_types::ActionBlock> for ActionBlock {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ActionBlock) -> Result<Self> {
        Ok(Self {
            launch_id: wit_newtype_to_domain!(value.launch_id, LaunchId)?,
            label: wit_newtype_to_domain!(value.label, DisplayText)?,
        })
    }
}

impl TryFrom<wit_types::DataForm> for DataForm {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::DataForm) -> Result<Self> {
        Ok(Self {
            form_type: value.form_type.into(),
            title: value
                .title
                .map(|title| wit_newtype_to_domain!(title, DisplayText))
                .transpose()?,
            instructions: value
                .instructions
                .into_iter()
                .map(|instruction| wit_newtype_to_domain!(instruction, DisplayText))
                .collect::<Result<Vec<_>>>()?,
            fields: value
                .fields
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl From<wit_types::DataFormType> for DataFormType {
    fn from(value: wit_types::DataFormType) -> Self {
        match value {
            wit_types::DataFormType::Form => Self::Form,
            wit_types::DataFormType::Submit => Self::Submit,
            wit_types::DataFormType::Cancel => Self::Cancel,
            wit_types::DataFormType::ResultForm => Self::Result,
        }
    }
}

impl TryFrom<wit_types::DataFormField> for DataFormField {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::DataFormField) -> Result<Self> {
        Ok(Self {
            name: wit_newtype_to_domain!(value.name, UiActionId)?,
            field_type: value.field_type.into(),
            label: value
                .label
                .map(|label| wit_newtype_to_domain!(label, DisplayText))
                .transpose()?,
            required: value.required,
            values: value
                .values
                .into_iter()
                .map(|value| DataFormValue::new(value.value))
                .collect(),
            options: value
                .options
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl From<wit_types::FormFieldType> for FormFieldType {
    fn from(value: wit_types::FormFieldType) -> Self {
        match value {
            wit_types::FormFieldType::Boolean => Self::Boolean,
            wit_types::FormFieldType::Fixed => Self::Fixed,
            wit_types::FormFieldType::Hidden => Self::Hidden,
            wit_types::FormFieldType::JidMulti => Self::JidMulti,
            wit_types::FormFieldType::JidSingle => Self::JidSingle,
            wit_types::FormFieldType::ListMulti => Self::ListMulti,
            wit_types::FormFieldType::ListSingle => Self::ListSingle,
            wit_types::FormFieldType::TextMulti => Self::TextMulti,
            wit_types::FormFieldType::TextPrivate => Self::TextPrivate,
            wit_types::FormFieldType::TextSingle => Self::TextSingle,
        }
    }
}

impl TryFrom<wit_types::FormFieldOption> for FormFieldOption {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::FormFieldOption) -> Result<Self> {
        Ok(Self {
            label: value
                .label
                .map(|label| wit_newtype_to_domain!(label, DisplayText))
                .transpose()?,
            value: DataFormValue::new(value.value.value),
        })
    }
}

impl TryFrom<wit_types::ListView> for ListView {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ListView) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, ListId)?,
            title: value
                .title
                .map(|title| wit_newtype_to_domain!(title, DisplayText))
                .transpose()?,
            items: value
                .items
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl TryFrom<wit_types::ListItem> for ListItem {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ListItem) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, ListItemId)?,
            label: wit_newtype_to_domain!(value.label, DisplayText)?,
            description: value
                .description
                .map(|description| wit_newtype_to_domain!(description, DisplayText))
                .transpose()?,
            image: value.image.map(TryInto::try_into).transpose()?,
            launch_id: value
                .launch_id
                .map(|launch_id| wit_newtype_to_domain!(launch_id, LaunchId))
                .transpose()?,
        })
    }
}

impl TryFrom<wit_types::ArtifactReference> for ArtifactReference {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ArtifactReference) -> Result<Self> {
        ArtifactReference::new(
            value.uri.value,
            value.sha256.value,
            value
                .media_type
                .map(|media_type| wit_newtype_to_domain!(media_type, MediaType))
                .transpose()?,
        )
        .map_err(anyhow::Error::from)
    }
}

impl TryFrom<wit_types::ExtensionPayload> for ExtensionPayload {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ExtensionPayload) -> Result<Self> {
        ExtensionPayload::new(
            wit_newtype_to_domain!(value.namespace, PayloadNamespace)?,
            xml_element_from_tokens(value.root, value.tokens)?,
        )
        .map_err(anyhow::Error::from)
    }
}

fn xml_element_from_tokens(
    root: wit_types::PayloadRoot,
    tokens: Vec<wit_types::XmlToken>,
) -> Result<XmlElement> {
    let expected_namespace = wit_newtype_to_domain!(root.namespace, PayloadNamespace)?;
    let expected_local_name = root.local_name;
    let mut stack: Vec<XmlElement> = Vec::new();
    let mut root_element = None;

    for token in tokens {
        match token {
            wit_types::XmlToken::StartElement(element) => {
                stack.push(element.try_into()?);
            }
            wit_types::XmlToken::Text(text) => {
                let Some(parent) = stack.last_mut() else {
                    return Err(anyhow::anyhow!("XML text token without open element"));
                };
                parent.children.push(XmlNode::Text(text));
            }
            wit_types::XmlToken::EndElement => {
                let Some(element) = stack.pop() else {
                    return Err(anyhow::anyhow!(
                        "XML end-element token without open element"
                    ));
                };
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XmlNode::Element(element));
                } else if root_element.replace(element).is_some() {
                    return Err(anyhow::anyhow!("XML token stream has multiple roots"));
                }
            }
        }
    }

    if !stack.is_empty() {
        return Err(anyhow::anyhow!("XML token stream ended with open elements"));
    }
    let Some(element) = root_element else {
        return Err(anyhow::anyhow!("XML token stream has no root"));
    };
    if element.namespace != expected_namespace || element.local_name != expected_local_name {
        return Err(anyhow::anyhow!(
            "XML token stream root does not match declared root"
        ));
    }
    Ok(element)
}

impl TryFrom<wit_types::XmlElement> for XmlElement {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::XmlElement) -> Result<Self> {
        XmlElement::new(
            wit_newtype_to_domain!(value.namespace, PayloadNamespace)?,
            value.local_name,
            value
                .attributes
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
            Vec::new(),
        )
        .map_err(anyhow::Error::from)
    }
}

impl TryFrom<wit_types::XmlAttribute> for XmlAttribute {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::XmlAttribute) -> Result<Self> {
        Ok(Self {
            namespace: value
                .namespace
                .map(|namespace| wit_newtype_to_domain!(namespace, PayloadNamespace))
                .transpose()?,
            local_name: value.local_name,
            value: value.value,
        })
    }
}

impl TryFrom<wit_types::PubsubPublish> for PubSubPublish {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::PubsubPublish) -> Result<Self> {
        Ok(Self {
            node: wit_newtype_to_domain!(value.node, PubSubNode)?,
            item_id: value
                .item_id
                .map(|item_id| wit_newtype_to_domain!(item_id, PubSubItemId))
                .transpose()?,
            payload: value.payload.try_into()?,
        })
    }
}

#[cfg(test)]
mod host_tool_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;

    #[derive(Debug, Default)]
    struct MockHostTools {
        list_channels_calls: AtomicUsize,
        send_message_calls: AtomicUsize,
    }

    #[async_trait]
    impl ExtensionHostTools for MockHostTools {
        async fn list_channels(
            &self,
            context: &InvocationContext,
            _request: host_domain::ListChannelsRequest,
        ) -> std::result::Result<host_domain::ListChannelsResponse, HostToolError> {
            self.list_channels_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                context
                    .requester
                    .as_ref()
                    .expect("trusted invocation requester")
                    .to_string(),
                "alice@example.com"
            );
            Ok(host_domain::ListChannelsResponse {
                channels: vec![host_domain::ChannelSummary {
                    room: "room@muc.example.com".parse().expect("room jid"),
                    name: Some(DisplayText::new("Room").expect("display text")),
                    description: None,
                }],
            })
        }

        async fn list_spaces(
            &self,
            _context: &InvocationContext,
            _request: host_domain::ListSpacesRequest,
        ) -> std::result::Result<host_domain::ListSpacesResponse, HostToolError> {
            Err(unsupported())
        }

        async fn list_room_members(
            &self,
            _context: &InvocationContext,
            _request: host_domain::ListRoomMembersRequest,
        ) -> std::result::Result<host_domain::ListRoomMembersResponse, HostToolError> {
            Err(unsupported())
        }

        async fn get_presence(
            &self,
            _context: &InvocationContext,
            _request: host_domain::GetPresenceRequest,
        ) -> std::result::Result<host_domain::GetPresenceResponse, HostToolError> {
            Err(unsupported())
        }

        async fn get_roster(
            &self,
            _context: &InvocationContext,
            _request: host_domain::GetRosterRequest,
        ) -> std::result::Result<host_domain::GetRosterResponse, HostToolError> {
            Err(unsupported())
        }

        async fn query_mam(
            &self,
            _context: &InvocationContext,
            _query: host_domain::MamQuery,
        ) -> std::result::Result<host_domain::MamQueryResponse, HostToolError> {
            Err(unsupported())
        }

        async fn send_message(
            &self,
            context: &InvocationContext,
            request: host_domain::SendMessageRequest,
        ) -> std::result::Result<host_domain::SendMessageResponse, HostToolError> {
            self.send_message_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                context
                    .requester
                    .as_ref()
                    .expect("trusted invocation requester")
                    .to_string(),
                "alice@example.com"
            );
            assert_eq!(request.body.as_str(), "hello from extension");
            Ok(host_domain::SendMessageResponse {
                stanza_id: StanzaId::new("extension-stanza").expect("stanza id"),
            })
        }
    }

    #[tokio::test]
    async fn denied_capability_fails_closed_before_delegating() {
        let tools = Arc::new(MockHostTools::default());
        let mut state = host_state(Arc::clone(&tools), HashSet::new());

        let result = HostToolsHost::list_channels(
            &mut state,
            wit_types::ListChannelsRequest { reserved: None },
        )
        .await
        .expect("host import does not trap");

        let error = result.expect_err("missing capability is denied");
        assert!(matches!(error.code, wit_types::HostToolErrorCode::Denied));
        assert_eq!(tools.list_channels_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn granted_host_import_delegates_to_trait() {
        let tools = Arc::new(MockHostTools::default());
        let mut grants = HashSet::new();
        grants.insert(ExtensionCapability::HostChannelsRead);
        let mut state = host_state(Arc::clone(&tools), grants);

        let response = HostToolsHost::list_channels(
            &mut state,
            wit_types::ListChannelsRequest { reserved: None },
        )
        .await
        .expect("host import does not trap")
        .expect("capability grant allows delegation");

        assert_eq!(tools.list_channels_calls.load(Ordering::SeqCst), 1);
        assert_eq!(response.channels[0].room.value, "room@muc.example.com");
    }

    #[tokio::test]
    async fn granted_send_message_import_delegates_to_trait() {
        let tools = Arc::new(MockHostTools::default());
        let mut grants = HashSet::new();
        grants.insert(ExtensionCapability::HostMessageSend);
        let mut state = host_state(Arc::clone(&tools), grants);

        let response = HostToolsHost::send_message(
            &mut state,
            wit_types::SendMessageRequest {
                target: wit_types::MessageTarget::Muc(wit_types::RoomJid {
                    value: "room@muc.example.com".to_string(),
                }),
                body: wit_types::DisplayText {
                    value: "hello from extension".to_string(),
                },
                thread_id: None,
                reply_to: None,
                extensions: None,
            },
        )
        .await
        .expect("host import does not trap")
        .expect("capability grant allows delegation");

        assert_eq!(tools.send_message_calls.load(Ordering::SeqCst), 1);
        assert_eq!(response.stanza_id.value, "extension-stanza");
    }

    #[tokio::test]
    async fn requester_private_tools_are_command_only() {
        let tools = Arc::new(MockHostTools::default());
        let mut grants = HashSet::new();
        grants.insert(ExtensionCapability::HostPresenceRead);
        let mut state = host_state(Arc::clone(&tools), grants);
        state.context.kind = InvocationKind::MessageHook;

        let result = HostToolsHost::get_presence(
            &mut state,
            wit_types::GetPresenceRequest {
                subject: wit_types::BareJid {
                    value: "alice@example.com".to_string(),
                },
            },
        )
        .await
        .expect("host import does not trap");

        let error = result.expect_err("message hooks cannot read requester-private presence");
        assert!(matches!(error.code, wit_types::HostToolErrorCode::Denied));
    }

    #[tokio::test]
    async fn runtime_http_denies_unconfigured_origin_before_network() {
        let error = execute_runtime_http_request(
            wit_types::OutgoingHttpRequest {
                method: wit_types::HttpMethod::Get,
                url: wit_types::Url {
                    value: "https://api.example.test/v1/chat".to_string(),
                },
                headers: Vec::new(),
                body: None,
            },
            &[],
        )
        .await
        .expect_err("origin allowlist is enforced");

        assert!(matches!(error.code, HostToolErrorCode::Denied));
    }

    #[tokio::test]
    async fn runtime_http_caps_request_body_before_network() {
        let error = execute_runtime_http_request(
            wit_types::OutgoingHttpRequest {
                method: wit_types::HttpMethod::Post,
                url: wit_types::Url {
                    value: "https://api.example.test/v1/chat".to_string(),
                },
                headers: Vec::new(),
                body: Some("x".repeat(256 * 1024 + 1)),
            },
            &["https://api.example.test".to_string()],
        )
        .await
        .expect_err("request body cap is enforced");

        assert!(matches!(error.code, HostToolErrorCode::InvalidRequest));
    }

    #[tokio::test]
    async fn runtime_http_rejects_accept_encoding_before_network() {
        let error = execute_runtime_http_request(
            wit_types::OutgoingHttpRequest {
                method: wit_types::HttpMethod::Post,
                url: wit_types::Url {
                    value: "https://api.example.test/v1/chat".to_string(),
                },
                headers: vec![wit_types::HttpHeader {
                    name: "accept-encoding".to_string(),
                    value: "gzip".to_string(),
                }],
                body: None,
            },
            &["https://api.example.test".to_string()],
        )
        .await
        .expect_err("accept-encoding is host-controlled");

        assert!(matches!(error.code, HostToolErrorCode::InvalidRequest));
    }

    #[test]
    fn runtime_http_sets_identity_accept_encoding() {
        let client = reqwest::Client::new();
        let request = apply_runtime_http_headers(
            client.post("https://api.example.test/v1/chat"),
            vec![wit_types::HttpHeader {
                name: "accept".to_string(),
                value: "application/json".to_string(),
            }],
        )
        .expect("headers are valid")
        .build()
        .expect("request builds");

        assert_eq!(
            request.headers().get("accept-encoding").unwrap(),
            "identity"
        );
        assert_eq!(request.headers().get("accept").unwrap(), "application/json");
    }

    #[test]
    fn runtime_http_normalizes_allowed_origins() {
        assert_eq!(
            normalize_http_origin("https://API.example.test/"),
            Some("https://api.example.test".to_string())
        );
        assert_eq!(
            normalize_http_origin("https://api.example.test:8443/path"),
            Some("https://api.example.test:8443".to_string())
        );
        assert_eq!(normalize_http_origin("http://api.example.test"), None);
    }

    #[test]
    fn runtime_http_rejects_host_controlled_headers() {
        assert!(is_disallowed_extension_http_header("Host"));
        assert!(is_disallowed_extension_http_header("content-length"));
        assert!(is_disallowed_extension_http_header("Transfer-Encoding"));
        assert!(is_disallowed_extension_http_header("Accept-Encoding"));
        assert!(!is_disallowed_extension_http_header("authorization"));
        assert!(!is_disallowed_extension_http_header("content-type"));
    }

    fn host_state(tools: Arc<MockHostTools>, grants: HashSet<ExtensionCapability>) -> HostState {
        HostState::new(
            tools,
            InvocationContext {
                waddle_id: WaddleId::new("test").expect("waddle id"),
                plugin_id: PluginId::new("test-extension").expect("plugin id"),
                requester: Some("alice@example.com".parse().expect("requester jid")),
                source_room: Some("room@muc.example.com".parse().expect("room jid")),
                kind: InvocationKind::Command,
            },
            "{}".to_string(),
            grants,
            Vec::new(),
        )
    }

    fn unsupported() -> HostToolError {
        HostToolError {
            code: HostToolErrorCode::Unsupported,
            message: DisplayText::new("unsupported").expect("display text"),
        }
    }
}
