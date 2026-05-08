use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionEnvelope {
    pub version: u32,
    pub enrichments: Vec<MessageEnrichment>,
}

impl ExtensionEnvelope {
    pub fn new(enrichments: Vec<MessageEnrichment>) -> Self {
        Self {
            version: 1,
            enrichments,
        }
    }

    pub fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("extensions", FRAMEWORK_NAMESPACE)
            .attr("version", self.version.to_string());
        for enrichment in &self.enrichments {
            builder = builder.append(enrichment.to_minidom());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageEnrichment {
    pub id: EnrichmentId,
    pub plugin: PluginId,
    pub capability: ExtensionCapability,
    pub payload_namespace: PayloadNamespace,
    pub created_at: Timestamp,
    pub source: Option<MessageSource>,
    pub ui: Vec<UiView>,
    pub payloads: Vec<ExtensionPayload>,
    pub launches: Vec<LaunchDescriptor>,
}

impl MessageEnrichment {
    pub fn payloads_match_declared_namespace(&self) -> bool {
        self.payloads.iter().all(|payload| {
            payload.namespace == self.payload_namespace
                && payload.root.namespace == payload.namespace
        })
    }

    pub fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("enrichment", FRAMEWORK_NAMESPACE)
            .attr("id", self.id.as_str())
            .attr("plugin", self.plugin.as_str())
            .attr("capability", self.capability.as_str())
            .attr("payload-ns", self.payload_namespace.as_str())
            .attr("created", self.created_at.as_str());
        if let Some(source) = &self.source {
            builder = builder.append(source.to_minidom());
        }
        if !self.payloads.is_empty() {
            let mut payload_builder = Element::builder("payload", FRAMEWORK_NAMESPACE);
            for view in &self.ui {
                payload_builder = payload_builder.append(view.to_minidom());
            }
            for payload in &self.payloads {
                payload_builder = payload_builder.append(payload.to_minidom());
            }
            builder = builder.append(payload_builder.build());
        } else if !self.ui.is_empty() {
            let mut payload_builder = Element::builder("payload", FRAMEWORK_NAMESPACE);
            for view in &self.ui {
                payload_builder = payload_builder.append(view.to_minidom());
            }
            builder = builder.append(payload_builder.build());
        }
        for launch in &self.launches {
            builder = builder.append(launch.to_minidom());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageSource {
    pub stanza_id: StanzaId,
    pub body_range: Option<BodyRange>,
}

impl MessageSource {
    pub fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("source", FRAMEWORK_NAMESPACE)
            .attr("stanza-id", self.stanza_id.as_str());
        if let Some(range) = &self.body_range {
            builder = builder
                .attr("body-start", range.start.to_string())
                .attr("body-end", range.end.to_string());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchContext {
    pub waddle_id: WaddleId,
    pub room: Option<RoomJid>,
    pub source_stanza_id: Option<StanzaId>,
}

impl LaunchContext {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("context", FRAMEWORK_NAMESPACE)
            .attr("waddle-id", self.waddle_id.as_str());
        if let Some(room) = &self.room {
            builder = builder.attr("room", room.as_str());
        }
        if let Some(stanza_id) = &self.source_stanza_id {
            builder = builder.attr("stanza-id", stanza_id.as_str());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchDescriptor {
    pub id: LaunchId,
    pub plugin: PluginId,
    pub action: ActionId,
    pub command_node: CommandNode,
    pub label: DisplayText,
    pub context: LaunchContext,
    pub payloads: Vec<ExtensionPayload>,
    pub fallback: Option<UiView>,
    pub expires_at: Option<Timestamp>,
    pub token: Option<LaunchToken>,
}

impl LaunchDescriptor {
    pub fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("launch", FRAMEWORK_NAMESPACE)
            .attr("id", self.id.as_str())
            .attr("plugin", self.plugin.as_str())
            .attr("action", self.action.as_str())
            .attr("command-node", self.command_node.as_str())
            .attr("label", self.label.as_str())
            .append(self.context.to_minidom());
        if !self.payloads.is_empty() {
            let payloads = self.payloads.iter().fold(
                Element::builder("payload", FRAMEWORK_NAMESPACE),
                |builder, payload| builder.append(payload.to_minidom()),
            );
            builder = builder.append(payloads.build());
        }
        if let Some(expires_at) = &self.expires_at {
            builder = builder.attr("expires-at", expires_at.as_str());
        }
        if let Some(token) = &self.token {
            builder = builder.attr("token", token.as_str());
        }
        if let Some(fallback) = &self.fallback {
            builder = builder.append(
                Element::builder("fallback", FRAMEWORK_NAMESPACE)
                    .append(fallback.to_minidom())
                    .build(),
            );
        }
        builder.build()
    }
}
