//! Host effects returned by extension invocations.

use super::*;

/// Requested host effects and optional provider usage reported by one invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionResponse {
    /// Requested host operations, subject to manifest and operator-grant validation.
    pub effects: Vec<ExtensionEffect>,
    /// Optional provider/model cost accounting reported by the invocation.
    pub usage: Option<InvocationUsage>,
}

/// Host-side effects requested by a guest and filtered against its manifest and grants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExtensionEffect {
    /// Return a durable observation payload for host-owned publication.
    PublishRoomResult(ExtensionPayload),
    /// Attach validated plugin enrichments to the source message.
    EnrichMessage(ExtensionEnvelope),
    /// Request publication to a declared PubSub node.
    PublishPubSub(PubSubPublish),
    /// Expose an immutable artifact reference to the host.
    ReferenceArtifact(ArtifactReference),
    /// Return a form as the ad-hoc command's next interaction or result.
    CommandForm(DataForm),
    /// Surface a nonblank diagnostic without stopping the surrounding message flow.
    HostWarning(DisplayText),
    /// Produce no host operation.
    Noop,
}

impl ExtensionEffect {
    /// Whether the effect is permitted by the manifest, treating all declared
    /// capabilities as granted.
    #[must_use]
    pub fn validate_for_manifest(&self, manifest: &ExtensionManifest) -> bool {
        self.validate_for_manifest_and_grants(
            manifest,
            &manifest.capabilities.iter().copied().collect(),
        )
    }

    /// Whether the effect's capability, owning plugin, payload roots, command nodes, and
    /// PubSub node are permitted by both the manifest and operator grants. Warnings and
    /// no-ops are always permitted.
    #[must_use]
    pub fn validate_for_manifest_and_grants(
        &self,
        manifest: &ExtensionManifest,
        grants: &HashSet<ExtensionCapability>,
    ) -> bool {
        match self {
            Self::PublishRoomResult(payload) => {
                manifest.declares_capability(ExtensionCapability::RoomResultPublish)
                    && grants.contains(&ExtensionCapability::RoomResultPublish)
                    && manifest.declares_payload(PayloadSurface::RoomResult, payload)
            }
            Self::EnrichMessage(envelope) => envelope.enrichments.iter().all(|enrichment| {
                enrichment.plugin == manifest.id
                    && enrichment.capability == ExtensionCapability::MessageEnrich
                    && manifest.declares_capability(ExtensionCapability::MessageEnrich)
                    && grants.contains(&ExtensionCapability::MessageEnrich)
                    && enrichment.payloads_match_declared_namespace()
                    && enrichment.payloads.iter().all(|payload| {
                        manifest.declares_payload(PayloadSurface::MessageEnrichment, payload)
                    })
                    && enrichment.launches.iter().all(|launch| {
                        launch.plugin == manifest.id
                            && manifest.declares_capability(ExtensionCapability::Launch)
                            && grants.contains(&ExtensionCapability::Launch)
                            && launch.payloads.iter().all(|payload| {
                                payload.namespace == payload.root.namespace
                                    && manifest
                                        .declares_payload(PayloadSurface::LaunchPayload, payload)
                            })
                            && (launch.command_node == CommandNode::invoke()
                                || manifest.declares_command(&launch.command_node))
                    })
            }),
            Self::PublishPubSub(publish) => {
                manifest.declares_capability(ExtensionCapability::PubSubPublish)
                    && grants.contains(&ExtensionCapability::PubSubPublish)
                    && manifest.declares_pubsub_node(&publish.node)
                    && publish.payload.namespace == publish.payload.root.namespace
                    && (publish.payload.is_framework_item()
                        || manifest.declares_payload(PayloadSurface::PubSubItem, &publish.payload))
            }
            Self::ReferenceArtifact(_) => {
                manifest.declares_capability(ExtensionCapability::ArtifactReference)
                    && grants.contains(&ExtensionCapability::ArtifactReference)
            }
            Self::CommandForm(_) => {
                manifest.declares_capability(ExtensionCapability::Commands)
                    && grants.contains(&ExtensionCapability::Commands)
            }
            Self::HostWarning(_) => true,
            Self::Noop => true,
        }
    }
}

/// PubSub node, optional item ID, and validated payload requested for publication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PubSubPublish {
    /// PubSub or discovery node selected by the request.
    pub node: PubSubNode,
    /// Identifier of the PubSub item.
    pub item_id: Option<PubSubItemId>,
    /// Validated application XML to publish under the requested node.
    pub payload: ExtensionPayload,
}

/// Raw detected link and its half-open UTF-8 byte offsets before typed conversion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectedLink {
    /// Resource URL exposed to the host or user.
    pub url: String,
    /// Inclusive UTF-8 byte offset where the detected URL begins.
    pub start_offset: u32,
    /// Exclusive UTF-8 byte offset where the detected URL ends.
    pub end_offset: u32,
}

/// Whether a message already carries an extension envelope in the framework namespace.
#[must_use]
pub fn message_has_framework_envelope(msg: &Message) -> bool {
    msg.payloads
        .iter()
        .any(|payload| payload.name() == "extensions" && payload.ns() == FRAMEWORK_NAMESPACE)
}
