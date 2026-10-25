//! Actor-facing extension invocation and observation request contracts.

use crate::host_tools::DeliveryInvocation;
#[cfg(any(test, feature = "test-support"))]
use crate::observer_test_support::ObserverTestPlugin;
#[cfg(any(test, feature = "test-support"))]
use crate::types::ArtifactReference;
use crate::types::ConfiguredRoomObserver;
#[cfg(any(test, feature = "test-support"))]
use crate::types::ExtensionProfile;
use crate::types::ExtensionResponse;
use crate::types::ObservationFailure;
use crate::types::RoomMessageObserve;
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use tracing::warn;

use crate::host_tools::{
    DenyingExtensionHostTools, ExtensionHostTools, InvocationContext, InvocationKind,
};
use crate::runtime::LoadedExtension;
use crate::types::{
    DisplayText, ExtensionCapability, ExtensionEffect, ExtensionEvent, ExtensionManifest, WaddleId,
};
use xmpp_parsers::jid::FullJid;

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
    grants: HashSet<ExtensionCapability>,
    allowed_http_origins: Vec<String>,
    provider_room_grants: Vec<xmpp_parsers::jid::BareJid>,
    room_observer: Option<ConfiguredRoomObserver>,
    observation_permits: Arc<tokio::sync::Semaphore>,
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
            grants: HashSet::new(),
            allowed_http_origins: Vec::new(),
            provider_room_grants: Vec::new(),
            room_observer: None,
            observation_permits: Arc::new(tokio::sync::Semaphore::new(1)),
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

    pub(crate) async fn observe_room_message(
        &self,
        event: RoomMessageObserve,
        delivery: Option<Arc<dyn crate::host_tools::ExtensionDeliveryCapability>>,
    ) -> Result<ExtensionResponse, ObservationInvocationError> {
        let _permit = self
            .observation_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ObservationInvocationError::NotInvoked)?;
        let context = InvocationContext {
            // This is a display/runtime context only, never tenant authority.
            waddle_id: WaddleId::try_new("room-observation").expect("static context"),
            plugin_id: self.manifest.id.clone(),
            requester: None,
            source_room: Some(event.source.room.clone()),
            kind: InvocationKind::RoomMessageObserve,
            provider_room_grants: Vec::new(),
        };
        let result = if let Some(capability) = delivery {
            self.extension
                .call_handle_event_typed_with_delivery(
                    ExtensionEvent::RoomMessageObserve(event),
                    Arc::clone(&self.host_tools),
                    DeliveryInvocation {
                        context,
                        capability,
                    },
                    self.config.clone(),
                    self.grants.clone(),
                    self.allowed_http_origins.clone(),
                )
                .await
        } else {
            self.extension
                .call_handle_event_typed(
                    ExtensionEvent::RoomMessageObserve(event),
                    Arc::clone(&self.host_tools),
                    context,
                    self.config.clone(),
                    self.grants.clone(),
                    self.allowed_http_origins.clone(),
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

    /// Run an event in the default `local` community and return only permitted effects;
    /// guest failures become host warnings.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed nonblank `local` community identifier violates the
    /// identifier constructor invariant.
    pub async fn handle_event(&self, event: ExtensionEvent) -> Vec<ExtensionEffect> {
        self.handle_event_for_waddle(event, WaddleId::try_new("local").expect("static waddle id"))
            .await
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
        #[cfg(any(test, feature = "test-support"))]
        if let (Some(plugin), ExtensionEvent::MessageHook(hook)) = (&self.observer_test, &event) {
            if let Some(effects) = plugin.invoke(hook.clone()).await {
                return effects;
            }
        }
        let context = InvocationContext {
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
            .call_handle_event(
                event,
                Arc::clone(&self.host_tools),
                context,
                self.config.clone(),
                self.grants.clone(),
                self.allowed_http_origins.clone(),
            )
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
            Err(error) => {
                let message = format!("Extension {} failed: {error}", self.manifest.id);
                warn!(
                    extension = %self.manifest.id,
                    %error,
                    "extension handle-event failed; continuing fail-open"
                );
                vec![ExtensionEffect::HostWarning(
                    DisplayText::try_new(message).expect("host warning is non-empty"),
                )]
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
