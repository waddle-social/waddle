//! Typed message content, revisions, links, and extension-enrichment envelopes.

use super::{
    ActionId, BodyRange, CommandNode, Deserialize, DisplayText, Element, EnrichmentId,
    ExtensionCapability, ExtensionPayload, LaunchId, LaunchToken, PayloadNamespace, PluginId,
    RoomJid, Serialize, StanzaId, Timestamp, UiView, WaddleId, FRAMEWORK_NAMESPACE,
};

/// Versioned message envelope containing extension enrichments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionEnvelope {
    /// Envelope format version; `new` initializes this to 1.
    pub version: u32,
    /// Extension-produced enrichments attached to the message.
    pub enrichments: Vec<MessageEnrichment>,
}

impl ExtensionEnvelope {
    /// Create a version-1 extension envelope containing the supplied enrichments.
    #[must_use]
    pub fn new(enrichments: Vec<MessageEnrichment>) -> Self {
        Self {
            version: 1,
            enrichments,
        }
    }

    /// Serialize this extension model into the framework's typed XML element tree.
    #[must_use]
    pub fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("extensions", FRAMEWORK_NAMESPACE).attr(
            minidom::rxml::xml_ncname!("version").to_owned(),
            self.version.to_string(),
        );
        for enrichment in &self.enrichments {
            builder = builder.append(enrichment.to_minidom());
        }
        builder.build()
    }
}

/// Plugin-owned message annotations, payloads, UI views, and authorized launch descriptors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageEnrichment {
    /// Identifier of this plugin contribution within the message envelope.
    pub id: EnrichmentId,
    /// Extension identifier owning this value.
    pub plugin: PluginId,
    /// Capability under which this enrichment was produced.
    pub capability: ExtensionCapability,
    /// Namespace declared for the extension payloads carried by this value.
    pub payload_namespace: PayloadNamespace,
    /// Creation timestamp serialized as supplied by the extension.
    pub created_at: Timestamp,
    /// Canonical source metadata identifying the message targeted by this operation.
    pub source: Option<MessageSource>,
    /// Declarative views rendered by the host alongside this enrichment's payloads.
    pub ui: Vec<UiView>,
    /// Extension payload elements carried alongside the standard protocol fields.
    pub payloads: Vec<ExtensionPayload>,
    /// Authorized actions offered for this enrichment.
    pub launches: Vec<LaunchDescriptor>,
}

impl MessageEnrichment {
    /// Whether every payload's advertised namespace and root namespace match this
    /// enrichment's declared namespace.
    #[must_use]
    pub fn payloads_match_declared_namespace(&self) -> bool {
        self.payloads.iter().all(|payload| {
            payload.namespace == self.payload_namespace
                && payload.root.namespace == payload.namespace
        })
    }

    /// Serialize this extension model into the framework's typed XML element tree.
    #[must_use]
    pub fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("enrichment", FRAMEWORK_NAMESPACE)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                self.id.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("plugin").to_owned(),
                self.plugin.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("capability").to_owned(),
                self.capability.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("payload-ns").to_owned(),
                self.payload_namespace.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("created").to_owned(),
                self.created_at.as_str(),
            );
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

/// Original stanza identity and optional body range targeted by an enrichment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageSource {
    /// Identifier of the source message targeted by this enrichment.
    pub stanza_id: StanzaId,
    /// Range of message-body text associated with the enrichment.
    pub body_range: Option<BodyRange>,
}

impl MessageSource {
    /// Serialize this extension model into the framework's typed XML element tree.
    #[must_use]
    pub fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("source", FRAMEWORK_NAMESPACE).attr(
            minidom::rxml::xml_ncname!("stanza-id").to_owned(),
            self.stanza_id.as_str(),
        );
        if let Some(range) = &self.body_range {
            builder = builder
                .attr(
                    minidom::rxml::xml_ncname!("body-start").to_owned(),
                    range.start.to_string(),
                )
                .attr(
                    minidom::rxml::xml_ncname!("body-end").to_owned(),
                    range.end.to_string(),
                );
        }
        builder.build()
    }
}

/// Community, optional room, and source stanza bound into a launch token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchContext {
    /// Identifier of the Waddle community providing invocation context.
    pub waddle_id: WaddleId,
    /// Bare JID of the room in scope.
    pub room: Option<RoomJid>,
    /// Identifier of the source stanza associated with this value.
    pub source_stanza_id: Option<StanzaId>,
}

impl LaunchContext {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("context", FRAMEWORK_NAMESPACE).attr(
            minidom::rxml::xml_ncname!("waddle-id").to_owned(),
            self.waddle_id.as_str(),
        );
        if let Some(room) = &self.room {
            builder = builder.attr(minidom::rxml::xml_ncname!("room").to_owned(), room.as_str());
        }
        if let Some(stanza_id) = &self.source_stanza_id {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("stanza-id").to_owned(),
                stanza_id.as_str(),
            );
        }
        builder.build()
    }
}

/// Extension action with payloads, display fallback, and optional signed invocation authorization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchDescriptor {
    /// Identifier used to correlate this launch descriptor with later invocations.
    pub id: LaunchId,
    /// Extension identifier owning this value.
    pub plugin: PluginId,
    /// Action requested by the client or exposed by the UI.
    pub action: ActionId,
    /// Ad-hoc command node used to dispatch the requested extension action.
    pub command_node: CommandNode,
    /// Human-readable label rendered by the host.
    pub label: DisplayText,
    /// Community and source-message context bound to the launch authorization.
    pub context: LaunchContext,
    /// Extension payload elements carried alongside the standard protocol fields.
    pub payloads: Vec<ExtensionPayload>,
    /// Optional declarative view shown when a richer launch presentation is unavailable.
    pub fallback: Option<UiView>,
    /// Optional RFC 3339 deadline bound into the signed launch token.
    pub expires_at: Option<Timestamp>,
    /// Optional host-issued signature authorizing this launch and its bound context.
    pub token: Option<LaunchToken>,
}

impl LaunchDescriptor {
    /// Serialize this extension model into the framework's typed XML element tree.
    #[must_use]
    pub fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("launch", FRAMEWORK_NAMESPACE)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                self.id.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("plugin").to_owned(),
                self.plugin.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("action").to_owned(),
                self.action.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("command-node").to_owned(),
                self.command_node.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("label").to_owned(),
                self.label.as_str(),
            )
            .append(self.context.to_minidom());
        if !self.payloads.is_empty() {
            let payloads = self.payloads.iter().fold(
                Element::builder("payload", FRAMEWORK_NAMESPACE),
                |builder, payload| builder.append(payload.to_minidom()),
            );
            builder = builder.append(payloads.build());
        }
        if let Some(expires_at) = &self.expires_at {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("expires-at").to_owned(),
                expires_at.as_str(),
            );
        }
        if let Some(token) = &self.token {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("token").to_owned(),
                token.as_str(),
            );
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
