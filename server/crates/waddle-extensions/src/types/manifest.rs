//! Extension capabilities, supported payloads, commands, and routing declarations.

use super::*;

/// Explicit extension permissions requested in manifests and granted by the operator.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ExtensionCapability {
    /// Emit a durable room-observation result for host-owned publication.
    #[serde(rename = "room.result.publish")]
    RoomResultPublish,
    /// Attach validated metadata and payloads to a message.
    #[serde(rename = "message.enrich")]
    MessageEnrich,
    /// Receive room-message observation hooks.
    #[serde(rename = "message.observe")]
    MessageObserve,
    /// Discover channels through the authorized host service.
    #[serde(rename = "host.channels.read")]
    HostChannelsRead,
    /// Discover community spaces through the authorized host service.
    #[serde(rename = "host.spaces.read")]
    HostSpacesRead,
    /// Read permitted room membership or occupancy information.
    #[serde(rename = "host.members.read")]
    HostMembersRead,
    /// Read visible account resource presence.
    #[serde(rename = "host.presence.read")]
    HostPresenceRead,
    /// Query authorized message archives.
    #[serde(rename = "host.mam.read")]
    HostMamRead,
    /// Read an authorized account roster.
    #[serde(rename = "host.roster.read")]
    HostRosterRead,
    /// Send messages through host-mediated XMPP services.
    #[serde(rename = "host.message.send")]
    HostMessageSend,
    /// Make bounded HTTP requests to operator-allowed HTTPS origins.
    #[serde(rename = "outbound.http.request")]
    OutboundHttpRequest,
    /// Expose and handle XMPP ad-hoc commands.
    #[serde(rename = "commands")]
    Commands,
    /// Offer and invoke signed extension launch actions.
    #[serde(rename = "launch")]
    Launch,
    /// Publish payloads to declared PubSub nodes.
    #[serde(rename = "pubsub.publish")]
    PubSubPublish,
    /// Return immutable artifact references.
    #[serde(rename = "artifact.reference")]
    ArtifactReference,
    /// Expose declarative views and discoverable routes rendered by the host.
    #[serde(rename = "ui.declarative")]
    UiDeclarative,
}

impl ExtensionCapability {
    /// Return the canonical wire token used by extension XML and WIT.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoomResultPublish => "room.result.publish",
            Self::MessageEnrich => "message.enrich",
            Self::MessageObserve => "message.observe",
            Self::HostChannelsRead => "host.channels.read",
            Self::HostSpacesRead => "host.spaces.read",
            Self::HostMembersRead => "host.members.read",
            Self::HostPresenceRead => "host.presence.read",
            Self::HostMamRead => "host.mam.read",
            Self::HostRosterRead => "host.roster.read",
            Self::HostMessageSend => "host.message.send",
            Self::OutboundHttpRequest => "outbound.http.request",
            Self::Commands => "commands",
            Self::Launch => "launch",
            Self::PubSubPublish => "pubsub.publish",
            Self::ArtifactReference => "artifact.reference",
            Self::UiDeclarative => "ui.declarative",
        }
    }
}

/// Guest-declared identity, capabilities, payload schemas, commands, and discoverable routes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionManifest {
    /// Unique extension identity used for dispatch, capability grants, and payload ownership.
    pub id: PluginId,
    /// Human-readable name advertised to clients.
    pub name: DisplayText,
    /// Version advertised by the extension or protocol entity.
    pub version: PluginVersion,
    /// Extension payload elements carried alongside the standard protocol fields.
    pub payloads: Vec<PayloadRule>,
    /// Host capabilities requested or supported by the extension.
    pub capabilities: Vec<ExtensionCapability>,
    /// Ad-hoc commands exposed by the extension.
    pub commands: Vec<CommandDescriptor>,
    /// Route descriptors exposed by the extension.
    pub routes: Vec<ExtensionRouteDescriptor>,
    /// PubSub nodes or placeholder patterns the extension declares it may publish.
    pub pubsub_nodes: Vec<PubSubNode>,
    /// Optional host-rendered bot/profile identity for the extension.
    pub profile: Option<ExtensionProfile>,
    /// Immutable artifact referenced by this value.
    pub artifact: Option<ArtifactReference>,
}

impl ExtensionManifest {
    /// Whether this capability occurs in the manifest's requested capabilities.
    pub fn declares_capability(&self, capability: ExtensionCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// Whether the manifest declares this payload's surface, namespace, and XML root local name.
    pub fn declares_payload(&self, surface: PayloadSurface, payload: &ExtensionPayload) -> bool {
        self.payloads.iter().any(|rule| {
            rule.surface == surface
                && rule.root.namespace == payload.namespace
                && rule.root.namespace == payload.root.namespace
                && rule.root.local_name == payload.root.local_name
        })
    }

    /// Whether the manifest declares the exact ad-hoc command node.
    pub fn declares_command(&self, node: &CommandNode) -> bool {
        self.commands.iter().any(|command| command.node == *node)
    }

    /// Whether a declared node pattern matches this node; `{placeholder}` segments match
    /// one nonempty colon-separated segment.
    pub fn declares_pubsub_node(&self, node: &PubSubNode) -> bool {
        self.pubsub_nodes
            .iter()
            .any(|declared| pubsub_node_pattern_matches(declared.as_str(), node.as_str()))
    }

    /// Borrow the extension routes retained for discovery and UI rendering.
    pub fn route_descriptors(&self) -> &[ExtensionRouteDescriptor] {
        &self.routes
    }
}

/// Display identity used by the host for an extension's bot or profile presentation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionProfile {
    /// Human-readable display identity of the extension.
    pub display_name: DisplayText,
    /// Longer human-readable description.
    pub description: Option<DisplayText>,
    /// Optional presentation accent supplied by the extension.
    pub accent: Option<String>,
    /// Optional immutable avatar image for the extension's display identity.
    pub avatar: Option<ArtifactReference>,
    /// Optional role label shown with the extension's room bot presence.
    pub bot_hat_label: Option<DisplayText>,
}

fn pubsub_node_pattern_matches(pattern: &str, candidate: &str) -> bool {
    if pattern == candidate {
        return true;
    }
    let pattern_parts: Vec<_> = pattern.split(':').collect();
    let candidate_parts: Vec<_> = candidate.split(':').collect();
    pattern_parts.len() == candidate_parts.len()
        && pattern_parts
            .iter()
            .zip(candidate_parts)
            .all(|(pattern, candidate)| {
                (pattern.starts_with('{') && pattern.ends_with('}') && !candidate.is_empty())
                    || *pattern == candidate
            })
}

/// Discoverable ad-hoc command with optional composer integration hints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandDescriptor {
    /// PubSub or discovery node selected by the request.
    pub node: CommandNode,
    /// Human-readable name advertised to clients.
    pub name: DisplayText,
    /// Scope in which the route, command, or observation applies.
    pub scope: CommandScope,
    /// Optional command prefix offered in the chat composer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer_prefix: Option<String>,
    /// Optional form field populated from text entered after the composer prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_field: Option<String>,
    /// Whether composer submission may execute this command directly.
    #[serde(default, skip_serializing_if = "is_false")]
    pub composer_execute: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Whether a command applies globally or within a channel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum CommandScope {
    /// Command applies independently of a channel.
    Global,
    /// Command requires a channel context.
    Channel,
}

impl CommandScope {
    /// Return the canonical wire token used by extension XML and WIT.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Channel => "channel",
        }
    }
}

/// Context required to render an extension route.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionRouteScope {
    /// Route is rendered within a channel context.
    Channel,
}

impl ExtensionRouteScope {
    /// Return the canonical wire token used by extension XML and WIT.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Channel => "channel",
        }
    }
}

/// Host presentation style used to render an extension route.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionRouteSurface {
    /// Render items as a visual gallery.
    Gallery,
    /// Render items as a list.
    List,
}

impl ExtensionRouteSurface {
    /// Return the canonical wire token used by extension XML and WIT.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gallery => "gallery",
            Self::List => "list",
        }
    }
}

/// Discoverable extension route and the PubSub node backing its state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionRouteDescriptor {
    /// Extension identifier owning this value.
    pub plugin: PluginId,
    /// Route identifier unique within the owning plugin.
    pub id: RouteId,
    /// Human-readable label rendered by the host.
    pub label: DisplayText,
    /// Scope in which the route, command, or observation applies.
    pub scope: ExtensionRouteScope,
    /// Host presentation style used to render the route.
    pub surface: ExtensionRouteSurface,
    /// Backing PubSub state node; declared patterns may include a `{room}` placeholder.
    pub state_node: PubSubNode,
    /// Namespace declared for the extension payloads carried by this value.
    pub payload_namespace: PayloadNamespace,
}

/// Use of a custom payload for validating the matching manifest declaration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PayloadSurface {
    /// Payload returned as a durable room-observation result.
    RoomResult,
    /// Payload attached to a message enrichment.
    MessageEnrichment,
    /// Payload carried by a signed launch descriptor.
    LaunchPayload,
    /// Application payload stored as a PubSub item.
    PubSubItem,
}

/// Namespace and validated local XML name identifying a declared payload shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PayloadRoot {
    /// XML namespace URI of the element or attribute.
    pub namespace: PayloadNamespace,
    /// Local XML name without a namespace prefix.
    pub local_name: String,
}

impl PayloadRoot {
    /// Create a namespace/local-name pair after validating the unprefixed XML name.
    ///
    /// # Errors
    ///
    /// Returns `InvalidXmlName` unless the local name starts with an ASCII letter or
    /// underscore, continues with ASCII letters/digits/underscore/hyphen/period, contains
    /// no colon, and does not start with lowercase `xml`.
    pub fn new(
        namespace: PayloadNamespace,
        local_name: impl Into<String>,
    ) -> Result<Self, FrameworkTypeError> {
        let local_name = validate_xml_local_name(local_name.into())?;
        Ok(Self {
            namespace,
            local_name,
        })
    }
}

/// Payload root permitted on one declared extension output surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PayloadRule {
    /// Output surface on which the declared payload root is allowed.
    pub surface: PayloadSurface,
    /// Namespace-qualified XML root identifying this payload or declaration.
    pub root: PayloadRoot,
}
