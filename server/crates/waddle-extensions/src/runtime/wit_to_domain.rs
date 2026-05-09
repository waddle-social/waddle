use anyhow::Result;

use super::exports::waddle::extension as wit_exports;
use super::waddle::extension::types as wit_types;
use crate::types::{
    CommandDescriptor, CommandNode, CommandScope, DisplayText, EnrichmentId, ExtensionCapability,
    ExtensionEffect, ExtensionEnvelope, ExtensionManifest, ExtensionResponse,
    ExtensionRouteDescriptor, ExtensionRouteScope, ExtensionRouteSurface, FullJidValue,
    MessageEnrichment, PayloadNamespace, PayloadRoot, PayloadRule, PayloadSurface, PluginId,
    PluginVersion, PubSubNode, ReplyTarget, RouteId, StanzaId, Timestamp,
};

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
            composer_prefix: value.composer_prefix,
            inline_field: value.inline_field,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn wit_descriptor(
        node: &str,
        composer_prefix: Option<&str>,
        inline_field: Option<&str>,
    ) -> wit_types::CommandDescriptor {
        wit_types::CommandDescriptor {
            node: wit_types::CommandNode {
                value: node.to_string(),
            },
            name: wit_types::DisplayText {
                value: "Test Command".to_string(),
            },
            scope: wit_types::CommandScope::Global,
            composer_prefix: composer_prefix.map(str::to_string),
            inline_field: inline_field.map(str::to_string),
        }
    }

    #[test]
    fn command_descriptor_round_trip_carries_composer_prefix_and_inline_field() {
        let descriptor: CommandDescriptor = wit_descriptor(
            "urn:waddle:extension:1:ai-chatbot",
            Some("ai"),
            Some("prompt"),
        )
        .try_into()
        .expect("convert");
        assert_eq!(descriptor.composer_prefix.as_deref(), Some("ai"));
        assert_eq!(descriptor.inline_field.as_deref(), Some("prompt"));
    }

    #[test]
    fn command_descriptor_round_trip_preserves_none_when_absent() {
        let descriptor: CommandDescriptor =
            wit_descriptor("urn:waddle:extension:1:invoke", None, None)
                .try_into()
                .expect("convert");
        assert!(descriptor.composer_prefix.is_none());
        assert!(descriptor.inline_field.is_none());
    }
}
