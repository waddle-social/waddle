//! Actor-facing extension invocation and observation request contracts.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use tracing::warn;
use xmpp_parsers::jid::FullJid;

use crate::host_tools::{
    DeliveryInvocation, DenyingExtensionHostTools, ExtensionHostTools, InvocationContext,
    InvocationKind,
};
#[cfg(any(test, feature = "test-support"))]
use crate::observer_test_support::ObserverTestPlugin;
use crate::runtime::{
    is_inference_plugin, DenyingExtensionInferenceHost, ExtensionInferenceHost,
    InferenceInvocationParams, LoadedExtension,
};
#[cfg(any(test, feature = "test-support"))]
use crate::types::{ArtifactReference, ExtensionProfile};
use crate::types::{
    ConfiguredRoomObserver, DisplayText, ExtensionCapability, ExtensionEffect, ExtensionEvent,
    ExtensionManifest, ExtensionResponse, ObservationFailure, RoomMessageObserve, WaddleId,
};
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::quotas::TenantResourceController;

/// Host admission is distinct from any error after entering the runtime.
pub(crate) enum ObservationInvocationError {
    NotInvoked,
    Invoked(ObservationFailure),
}

/// An extension loaded into wasmtime and ready to handle typed framework events.
pub struct WasmExtensionActor {
    manifest: ExtensionManifest,
    #[cfg(any(test, feature = "test-support"))]
    observer_test: Option<Arc<ObserverTestPlugin>>,
    extension: Arc<LoadedExtension>,
    config: String,
    host_tools: Arc<dyn ExtensionHostTools>,
    inference_host: Arc<dyn ExtensionInferenceHost>,
    grants: HashSet<ExtensionCapability>,
    allowed_http_origins: Vec<String>,
    provider_room_grants: Vec<xmpp_parsers::jid::BareJid>,
    room_observer: Option<ConfiguredRoomObserver>,
    observation_permits: Arc<tokio::sync::Semaphore>,
    tenant_id: Option<TenantId>,
    resource_controller: Option<Arc<TenantResourceController>>,
}

impl std::fmt::Debug for WasmExtensionActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmExtensionActor")
            .field("manifest", &self.manifest)
            .field("extension", &self.extension)
            .finish_non_exhaustive()
    }
}

impl WasmExtensionActor {
    /// Load the extension and run its lifecycle `init` to obtain the extension info.
    ///
    /// `config` is the JSON config payload to forward to the guest's `init` function.
    ///
    /// # Errors
    ///
    /// Returns an initialization error for deadline/resource exhaustion, component traps,
    /// guest-reported failure, or a manifest that cannot be converted to validated host
    /// types.
    pub async fn initialize(extension: LoadedExtension, config: &str) -> Result<Self> {
        let manifest = extension.call_init(config).await?;
        Ok(Self {
            manifest,
            #[cfg(any(test, feature = "test-support"))]
            observer_test: None,
            extension: Arc::new(extension),
            config: config.to_string(),
            host_tools: Arc::new(DenyingExtensionHostTools),
            inference_host: Arc::new(DenyingExtensionInferenceHost),
            grants: HashSet::new(),
            allowed_http_origins: Vec::new(),
            provider_room_grants: Vec::new(),
            room_observer: None,
            observation_permits: Arc::new(tokio::sync::Semaphore::new(1)),
            tenant_id: None,
            resource_controller: None,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn with_observer_test(mut self, plugin: Arc<ObserverTestPlugin>) -> Self {
        self.manifest.id = plugin.id.clone();
        self.observer_test = Some(plugin);
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn set_profile_avatar(&mut self, avatar: ArtifactReference) {
        let display_name = self.manifest.name.clone();
        self.manifest
            .profile
            .get_or_insert(ExtensionProfile {
                display_name,
                description: None,
                accent: None,
                avatar: None,
                bot_hat_label: None,
            })
            .avatar = Some(avatar);
    }

    /// Install the host service implementation used by future invocations; capabilities
    /// still require explicit grants.
    #[must_use]
    pub fn with_host_tools(mut self, host_tools: Arc<dyn ExtensionHostTools>) -> Self {
        self.host_tools = host_tools;
        self
    }

    /// Install the brokered inference host used by `waddle:extension/inference@4.0.0`.
    #[must_use]
    pub fn with_inference_host(mut self, inference_host: Arc<dyn ExtensionInferenceHost>) -> Self {
        self.inference_host = inference_host;
        self
    }

    /// Replace the operator-granted capability set used to authorize host calls and
    /// returned effects.
    #[must_use]
    pub fn with_grants(mut self, grants: HashSet<ExtensionCapability>) -> Self {
        self.grants = grants;
        self
    }

    /// Replace the HTTPS origin allowlist used by outbound HTTP calls during guest execution.
    #[must_use]
    pub fn with_allowed_http_origins(mut self, origins: Vec<String>) -> Self {
        self.allowed_http_origins = origins;
        self
    }

    /// Replace the rooms that provider webhook invocations may target without a user requester.
    #[must_use]
    pub fn with_provider_room_grants(mut self, rooms: Vec<xmpp_parsers::jid::BareJid>) -> Self {
        self.provider_room_grants = rooms;
        self
    }

    pub(crate) fn provider_room_grants(&self) -> &[xmpp_parsers::jid::BareJid] {
        &self.provider_room_grants
    }

    pub(crate) fn with_room_observer(mut self, observer: Option<ConfiguredRoomObserver>) -> Self {
        if let Some(observer) = &observer {
            self.observation_permits = Arc::new(tokio::sync::Semaphore::new(
                observer.max_concurrent as usize,
            ));
        }
        self.room_observer = observer;
        self
    }

    pub(crate) fn room_observer(&self) -> Option<&ConfiguredRoomObserver> {
        self.room_observer.as_ref()
    }

    /// Bind this extension actor to a canonical [`TenantId`].
    #[must_use]
    pub fn with_tenant_id(mut self, tenant_id: TenantId) -> Self {
        self.tenant_id = Some(tenant_id);
        self
    }

    /// Return the canonical [`TenantId`] bound to this actor, if configured.
    #[must_use]
    pub fn tenant_id(&self) -> Option<&TenantId> {
        self.tenant_id.as_ref()
    }

    /// Attach a per-tenant [`TenantResourceController`] enforced before Wasm
    /// store creation and instantiation.
    #[must_use]
    pub fn with_resource_controller(mut self, controller: Arc<TenantResourceController>) -> Self {
        self.resource_controller = Some(controller);
        self
    }

    pub(crate) async fn observe_room_message(
        &self,
        waddle_id: WaddleId,
        event: RoomMessageObserve,
        delivery: Option<Arc<dyn crate::host_tools::ExtensionDeliveryCapability>>,
    ) -> Result<ExtensionResponse, ObservationInvocationError> {
        let _permit = self
            .observation_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ObservationInvocationError::NotInvoked)?;
        let wasm_permit = match (&self.resource_controller, &self.tenant_id) {
            (Some(controller), Some(tenant_id)) => Some(
                controller
                    .try_acquire_wasm_invocation(tenant_id, 0)
                    .map_err(|_| ObservationInvocationError::NotInvoked)?,
            ),
            _ => None,
        };
        let memory_tracker = wasm_permit.as_ref().map(|p| p.memory_tracker());
        let context = InvocationContext {
            tenant_id: self.tenant_id.clone(),
            waddle_id,
            plugin_id: self.manifest.id.clone(),
            requester: None,
            source_room: Some(event.source.room.clone()),
            kind: InvocationKind::RoomMessageObserve,
            provider_room_grants: Vec::new(),
        };
        let inference_invocation = is_inference_plugin(&self.manifest.id);
        let result = if let Some(capability) = delivery {
            self.extension
                .call_handle_event_typed_with_delivery_and_tracker(
                    ExtensionEvent::RoomMessageObserve(event),
                    Arc::clone(&self.host_tools),
                    DeliveryInvocation {
                        context,
                        capability,
                    },
                    Arc::clone(&self.inference_host),
                    inference_invocation,
                    self.config.clone(),
                    self.grants.clone(),
                    self.allowed_http_origins.clone(),
                    memory_tracker,
                )
                .await
        } else {
            self.extension
                .call_handle_event_typed_with_tracker(
                    ExtensionEvent::RoomMessageObserve(event),
                    Arc::clone(&self.host_tools),
                    context,
                    Arc::clone(&self.inference_host),
                    inference_invocation,
                    self.config.clone(),
                    self.grants.clone(),
                    self.allowed_http_origins.clone(),
                    memory_tracker,
                )
                .await
        };
        result.map_err(ObservationInvocationError::Invoked)
    }

    /// Clone the manifest returned by this component's successful initialization.
    #[must_use]
    pub fn manifest(&self) -> ExtensionManifest {
        self.manifest.clone()
    }

    /// Whether the operator granted this capability; test fixtures additionally honor
    /// observation revocation.
    #[must_use]
    pub fn has_grant(&self, capability: ExtensionCapability) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        if capability == ExtensionCapability::MessageObserve
            && self
                .observer_test
                .as_ref()
                .is_some_and(|plugin| plugin.is_revoked())
        {
            return false;
        }
        self.grants.contains(&capability)
    }

    /// Whether the effect matches both the manifest's declarations and this actor's
    /// granted capabilities.
    #[must_use]
    pub fn validate_effect(&self, effect: &ExtensionEffect) -> bool {
        effect.validate_for_manifest_and_grants(&self.manifest, &self.grants)
    }

    /// Run an event in the specified community, deriving requester authority from the event.
    pub async fn handle_event_for_waddle(
        &self,
        event: ExtensionEvent,
        waddle_id: WaddleId,
    ) -> Vec<ExtensionEffect> {
        self.handle_event_for_waddle_with_requester(event, waddle_id, None)
            .await
    }

    /// Run an event using explicit requester authority when supplied, otherwise deriving
    /// it from the event. Guest failures become warnings; undeclared effects, room-result
    /// effects, and durable observation events are excluded.
    ///
    /// # Panics
    ///
    /// Panics if construction of the nonempty host-warning diagnostic violates the
    /// display-text invariant. Test-support fixtures can also panic if their
    /// recorded-invocation mutex was poisoned.
    pub async fn handle_event_for_waddle_with_requester(
        &self,
        event: ExtensionEvent,
        waddle_id: WaddleId,
        requester: Option<xmpp_parsers::jid::BareJid>,
    ) -> Vec<ExtensionEffect> {
        self.handle_event_for_waddle_with_chunks(event, waddle_id, requester, None)
            .await
    }

    /// Run an event using explicit requester authority and an optional progressive
    /// stream chunk channel. Inference failures and `Denied` errors fail closed as
    /// [`ExtensionEffect::HostDenial`].
    ///
    /// # Panics
    ///
    /// Panics if construction of the nonempty diagnostic violates the display-text
    /// invariant. Test-support fixtures can also panic if their recorded-invocation
    /// mutex was poisoned.
    pub async fn handle_event_for_waddle_with_chunks(
        &self,
        event: ExtensionEvent,
        waddle_id: WaddleId,
        requester: Option<xmpp_parsers::jid::BareJid>,
        chunk_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    ) -> Vec<ExtensionEffect> {
        #[cfg(any(test, feature = "test-support"))]
        if let (Some(plugin), ExtensionEvent::MessageHook(hook)) = (&self.observer_test, &event) {
            if let Some(effects) = plugin.invoke(hook.clone()).await {
                return effects;
            }
        }
        let inference_invocation = is_inference_plugin(&self.manifest.id);
        let wasm_permit = match (&self.resource_controller, &self.tenant_id) {
            (Some(controller), Some(tenant_id)) => {
                match controller.try_acquire_wasm_invocation(tenant_id, 0) {
                    Ok(permit) => Some(permit),
                    Err(err) => {
                        warn!(
                            plugin = %self.manifest.id,
                            error = %err,
                            "extension invocation rejected before instantiation by tenant quota"
                        );
                        let text = DisplayText::try_new(format!(
                            "Extension {} resource limit exceeded",
                            self.manifest.id
                        ))
                        .expect("quota warning is non-empty");
                        return if inference_invocation {
                            vec![ExtensionEffect::HostDenial(text)]
                        } else {
                            vec![ExtensionEffect::HostWarning(text)]
                        };
                    }
                }
            }
            _ => None,
        };
        let memory_tracker = wasm_permit.as_ref().map(|p| p.memory_tracker());
        let context = InvocationContext {
            tenant_id: self.tenant_id.clone(),
            waddle_id,
            plugin_id: self.manifest.id.clone(),
            requester: requester.or_else(|| requester_for_event(&event)),
            source_room: source_room_for_event(&event),
            kind: invocation_kind_for_event(&event),
            provider_room_grants: if matches!(event, ExtensionEvent::ProviderWebhook(_)) {
                self.provider_room_grants.clone()
            } else {
                Vec::new()
            },
        };
        // Durable observations must use the explicit subscription/generation entry point.
        if matches!(event, ExtensionEvent::RoomMessageObserve(_)) {
            return Vec::new();
        }
        match self
            .extension
            .call_handle_event_with_inference_classified(InferenceInvocationParams {
                event,
                tools: Arc::clone(&self.host_tools),
                inference: Arc::clone(&self.inference_host),
                chunk_tx,
                context,
                config: self.config.clone(),
                grants: self.grants.clone(),
                allowed_http_origins: self.allowed_http_origins.clone(),
                tenant_memory_tracker: memory_tracker,
                inference_invocation,
            })
            .await
        {
            Ok(response) => response
                .effects
                .into_iter()
                .filter(|effect| {
                    !matches!(effect, ExtensionEffect::PublishRoomResult(_))
                        && effect.validate_for_manifest_and_grants(&self.manifest, &self.grants)
                })
                .collect(),
            Err((failure, detail)) => {
                let message = match detail {
                    Some(detail) => {
                        format!(
                            "Extension {} failed ({failure:?}): {detail}",
                            self.manifest.id
                        )
                    }
                    None => format!("Extension {} failed: {failure:?}", self.manifest.id),
                };
                if inference_invocation || matches!(failure, ObservationFailure::Denied) {
                    warn!(
                        extension = %self.manifest.id,
                        ?failure,
                        "inference extension handle-event failed; failing closed"
                    );
                    vec![ExtensionEffect::HostDenial(
                        DisplayText::try_new(message).expect("host denial is non-empty"),
                    )]
                } else {
                    warn!(
                        extension = %self.manifest.id,
                        ?failure,
                        "extension handle-event failed; continuing fail-open"
                    );
                    vec![ExtensionEffect::HostWarning(
                        DisplayText::try_new(message).expect("host warning is non-empty"),
                    )]
                }
            }
        }
    }
}

fn invocation_kind_for_event(event: &ExtensionEvent) -> InvocationKind {
    match event {
        ExtensionEvent::RoomMessageObserve(_) => InvocationKind::RoomMessageObserve,
        ExtensionEvent::MessageHook(_) => InvocationKind::MessageHook,
        ExtensionEvent::Command(_) => InvocationKind::Command,
        ExtensionEvent::Launch(_) => InvocationKind::Launch,
        ExtensionEvent::ProviderWebhook(_) => InvocationKind::ProviderWebhook,
    }
}

fn requester_for_event(event: &ExtensionEvent) -> Option<xmpp_parsers::jid::BareJid> {
    match event {
        ExtensionEvent::MessageHook(hook) => hook
            .context
            .sender
            .as_ref()
            .and_then(|sender| sender.as_str().parse::<FullJid>().ok())
            .map(|jid| jid.to_bare()),
        ExtensionEvent::Command(command) => command
            .requester
            .as_str()
            .parse::<FullJid>()
            .ok()
            .map(|jid| jid.to_bare()),
        ExtensionEvent::Launch(launch) => launch
            .requester
            .as_str()
            .parse::<FullJid>()
            .ok()
            .map(|jid| jid.to_bare()),
        ExtensionEvent::RoomMessageObserve(_) | ExtensionEvent::ProviderWebhook(_) => None,
    }
}

fn source_room_for_event(event: &ExtensionEvent) -> Option<xmpp_parsers::jid::BareJid> {
    match event {
        ExtensionEvent::MessageHook(hook) => hook
            .context
            .room
            .as_ref()
            .and_then(|room| room.as_str().parse().ok()),
        ExtensionEvent::Command(command) => command
            .room
            .as_ref()
            .and_then(|room| room.as_str().parse().ok()),
        ExtensionEvent::Launch(launch) => launch
            .context
            .room
            .as_ref()
            .and_then(|room| room.as_str().parse().ok()),
        ExtensionEvent::RoomMessageObserve(event) => Some(event.source.room.clone()),
        ExtensionEvent::ProviderWebhook(_) => None,
    }
}
