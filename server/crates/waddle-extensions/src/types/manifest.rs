use super::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ExtensionCapability {
    #[serde(rename = "message.enrich")]
    MessageEnrich,
    #[serde(rename = "message.observe")]
    MessageObserve,
    #[serde(rename = "host.channels.read")]
    HostChannelsRead,
    #[serde(rename = "host.spaces.read")]
    HostSpacesRead,
    #[serde(rename = "host.members.read")]
    HostMembersRead,
    #[serde(rename = "host.presence.read")]
    HostPresenceRead,
    #[serde(rename = "host.mam.read")]
    HostMamRead,
    #[serde(rename = "host.roster.read")]
    HostRosterRead,
    #[serde(rename = "host.message.send")]
    HostMessageSend,
    #[serde(rename = "outbound.http.request")]
    OutboundHttpRequest,
    #[serde(rename = "commands")]
    Commands,
    #[serde(rename = "launch")]
    Launch,
    #[serde(rename = "pubsub.publish")]
    PubSubPublish,
    #[serde(rename = "artifact.reference")]
    ArtifactReference,
    #[serde(rename = "ui.declarative")]
    UiDeclarative,
}

impl ExtensionCapability {
    pub fn as_str(self) -> &'static str {
        match self {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionManifest {
    pub id: PluginId,
    pub name: DisplayText,
    pub version: PluginVersion,
    pub payloads: Vec<PayloadRule>,
    pub capabilities: Vec<ExtensionCapability>,
    pub commands: Vec<CommandDescriptor>,
    pub routes: Vec<ExtensionRouteDescriptor>,
    pub pubsub_nodes: Vec<PubSubNode>,
    pub profile: Option<ExtensionProfile>,
    pub artifact: Option<ArtifactReference>,
}

impl ExtensionManifest {
    pub fn declares_capability(&self, capability: ExtensionCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn declares_payload(&self, surface: PayloadSurface, payload: &ExtensionPayload) -> bool {
        self.payloads.iter().any(|rule| {
            rule.surface == surface
                && rule.root.namespace == payload.namespace
                && rule.root.namespace == payload.root.namespace
                && rule.root.local_name == payload.root.local_name
        })
    }

    pub fn declares_command(&self, node: &CommandNode) -> bool {
        self.commands.iter().any(|command| command.node == *node)
    }

    pub fn declares_pubsub_node(&self, node: &PubSubNode) -> bool {
        self.pubsub_nodes
            .iter()
            .any(|declared| pubsub_node_pattern_matches(declared.as_str(), node.as_str()))
    }

    pub fn route_descriptors(&self) -> &[ExtensionRouteDescriptor] {
        &self.routes
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionProfile {
    pub display_name: DisplayText,
    pub description: Option<DisplayText>,
    pub accent: Option<String>,
    pub avatar: Option<ArtifactReference>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandDescriptor {
    pub node: CommandNode,
    pub name: DisplayText,
    pub scope: CommandScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_field: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub composer_execute: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum CommandScope {
    Global,
    Channel,
}

impl CommandScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Channel => "channel",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionRouteScope {
    Channel,
}

impl ExtensionRouteScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Channel => "channel",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionRouteSurface {
    Gallery,
    List,
}

impl ExtensionRouteSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gallery => "gallery",
            Self::List => "list",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionRouteDescriptor {
    pub plugin: PluginId,
    pub id: RouteId,
    pub label: DisplayText,
    pub scope: ExtensionRouteScope,
    pub surface: ExtensionRouteSurface,
    pub state_node: PubSubNode,
    pub payload_namespace: PayloadNamespace,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PayloadSurface {
    MessageEnrichment,
    LaunchPayload,
    PubSubItem,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PayloadRoot {
    pub namespace: PayloadNamespace,
    pub local_name: String,
}

impl PayloadRoot {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PayloadRule {
    pub surface: PayloadSurface,
    pub root: PayloadRoot,
}
