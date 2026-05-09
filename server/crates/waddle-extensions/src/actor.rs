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

/// An extension loaded into wasmtime and ready to handle typed framework events.
pub struct WasmExtensionActor {
    manifest: ExtensionManifest,
    extension: Arc<LoadedExtension>,
    config: String,
    host_tools: Arc<dyn ExtensionHostTools>,
    grants: HashSet<ExtensionCapability>,
    allowed_http_origins: Vec<String>,
    provider_room_grants: Vec<xmpp_parsers::jid::BareJid>,
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
    pub async fn initialize(extension: LoadedExtension, config: &str) -> Result<Self> {
        let manifest = extension.call_init(config).await?;
        Ok(Self {
            manifest,
            extension: Arc::new(extension),
            config: config.to_string(),
            host_tools: Arc::new(DenyingExtensionHostTools),
            grants: HashSet::new(),
            allowed_http_origins: Vec::new(),
            provider_room_grants: Vec::new(),
        })
    }

    pub fn with_host_tools(mut self, host_tools: Arc<dyn ExtensionHostTools>) -> Self {
        self.host_tools = host_tools;
        self
    }

    pub fn with_grants(mut self, grants: HashSet<ExtensionCapability>) -> Self {
        self.grants = grants;
        self
    }

    pub fn with_allowed_http_origins(mut self, origins: Vec<String>) -> Self {
        self.allowed_http_origins = origins;
        self
    }

    pub fn with_provider_room_grants(mut self, rooms: Vec<xmpp_parsers::jid::BareJid>) -> Self {
        self.provider_room_grants = rooms;
        self
    }

    pub fn manifest(&self) -> ExtensionManifest {
        self.manifest.clone()
    }

    pub fn has_grant(&self, capability: ExtensionCapability) -> bool {
        self.grants.contains(&capability)
    }

    pub fn validate_effect(&self, effect: &ExtensionEffect) -> bool {
        effect.validate_for_manifest_and_grants(&self.manifest, &self.grants)
    }

    pub async fn handle_event(&self, event: ExtensionEvent) -> Vec<ExtensionEffect> {
        self.handle_event_for_waddle(event, WaddleId::new("local").expect("static waddle id"))
            .await
    }

    pub async fn handle_event_for_waddle(
        &self,
        event: ExtensionEvent,
        waddle_id: WaddleId,
    ) -> Vec<ExtensionEffect> {
        self.handle_event_for_waddle_with_requester(event, waddle_id, None)
            .await
    }

    pub async fn handle_event_for_waddle_with_requester(
        &self,
        event: ExtensionEvent,
        waddle_id: WaddleId,
        requester: Option<xmpp_parsers::jid::BareJid>,
    ) -> Vec<ExtensionEffect> {
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
                    effect.validate_for_manifest_and_grants(&self.manifest, &self.grants)
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
                    DisplayText::new(message).expect("host warning is non-empty"),
                )]
            }
        }
    }
}

fn invocation_kind_for_event(event: &ExtensionEvent) -> InvocationKind {
    match event {
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
        ExtensionEvent::ProviderWebhook(_) => None,
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
        ExtensionEvent::ProviderWebhook(_) => None,
    }
}
