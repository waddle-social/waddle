use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionResponse {
    pub effects: Vec<ExtensionEffect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExtensionEffect {
    EnrichMessage(ExtensionEnvelope),
    PublishPubSub(PubSubPublish),
    ReferenceArtifact(ArtifactReference),
    CommandForm(DataForm),
    HostWarning(DisplayText),
    Noop,
}

impl ExtensionEffect {
    pub fn validate_for_manifest(&self, manifest: &ExtensionManifest) -> bool {
        self.validate_for_manifest_and_grants(
            manifest,
            &manifest.capabilities.iter().copied().collect(),
        )
    }

    pub fn validate_for_manifest_and_grants(
        &self,
        manifest: &ExtensionManifest,
        grants: &HashSet<ExtensionCapability>,
    ) -> bool {
        match self {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PubSubPublish {
    pub node: PubSubNode,
    pub item_id: Option<PubSubItemId>,
    pub payload: ExtensionPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectedLink {
    pub url: String,
    pub start_offset: u32,
    pub end_offset: u32,
}

pub fn message_has_framework_envelope(msg: &Message) -> bool {
    msg.payloads
        .iter()
        .any(|payload| payload.name() == "extensions" && payload.ns() == FRAMEWORK_NAMESPACE)
}
