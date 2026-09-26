use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtensionResponse {
    pub effects: Vec<ExtensionEffect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ExtensionEffect {
    EnrichMessage(ExtensionEnvelope),
    PublishPubSub(PubSubPublish),
    ReferenceArtifact(ArtifactReference),
    CommandForm(DataForm),
    HostWarning(DisplayText),
    Noop,
    /// The guest's answer to a `DurableJob` event (see `types::events`).
    /// Only ever produced in response to that event; the host, not the
    /// message-hook pipeline, interprets and executes it (see
    /// `waddle-server::extension_job_outbox`).
    DurableJobResult(DurableJobOutcome),
}

/// One named judgment score. See `waddle-extension.wit`'s `judgment-score`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JudgmentScore {
    pub category: JudgmentCategory,
    pub probability: JudgmentProbability,
    pub taxonomy_version: JudgmentTaxonomyVersion,
}

/// See `waddle-extension.wit`'s `judgment-result`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JudgmentResult {
    pub model_version: JudgmentModelVersion,
    pub scores: Vec<JudgmentScore>,
}

/// See `waddle-extension.wit`'s `durable-job-failure`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DurableJobFailure {
    pub message: DisplayText,
    pub retryable: bool,
}

/// See `waddle-extension.wit`'s `durable-job-outcome`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DurableJobOutcome {
    Success(JudgmentResult),
    Failure(DurableJobFailure),
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
            // Only ever produced in direct response to a `DurableJob` event
            // that the host itself only dispatches to an actor holding the
            // grant (see `ExtensionManager::durable_job_handler`), so no
            // further per-payload validation is needed here — unlike
            // `EnrichMessage`/`PublishPubSub`, this effect never reaches an
            // untrusted wire surface through the ordinary message-hook path.
            Self::DurableJobResult(_) => {
                manifest.declares_capability(ExtensionCapability::DurableJob)
                    && grants.contains(&ExtensionCapability::DurableJob)
            }
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
