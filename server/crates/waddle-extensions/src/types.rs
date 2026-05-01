use std::fmt;

use minidom::Element;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use xmpp_parsers::jid::{BareJid, FullJid};
use xmpp_parsers::message::Message;

pub const FRAMEWORK_NAMESPACE: &str = "urn:waddle:extension:1";
pub const INVOKE_COMMAND_NODE: &str = "urn:waddle:extension:1:invoke";

const MAX_XML_DEPTH: usize = 16;
const MAX_XML_ATTRIBUTES: usize = 64;
const MAX_XML_CHILDREN: usize = 256;
const MAX_XML_TEXT_BYTES: usize = 16 * 1024;
const MAX_XML_SERIALIZED_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum FrameworkTypeError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("plugin id {0:?} must use lowercase ASCII letters, digits, and hyphens")]
    InvalidPluginId(String),
    #[error("namespace {0:?} must be an absolute non-official namespace URI/URN")]
    InvalidPayloadNamespace(String),
    #[error("official XMPP namespace {0:?} cannot carry Waddle extension semantics")]
    OfficialNamespace(String),
    #[error("framework namespace {0:?} is reserved for Waddle control payloads")]
    ReservedFrameworkNamespace(String),
    #[error("command node {0:?} must be under urn:waddle:extension:1")]
    InvalidCommandNode(String),
    #[error("sha256 digest {0:?} must be exactly 64 hexadecimal characters")]
    InvalidSha256Digest(String),
    #[error("artifact URI {0:?} must be immutable HTTP(S) and include /sha256/")]
    InvalidArtifactUri(String),
    #[error("bare JID {0:?} is invalid")]
    InvalidBareJid(String),
    #[error("full JID {0:?} is invalid")]
    InvalidFullJid(String),
    #[error("artifact URI {uri:?} must include digest {sha256:?}")]
    ArtifactDigestMismatch { uri: String, sha256: String },
    #[error("body range end {end} must be greater than start {start}")]
    InvalidBodyRange { start: u32, end: u32 },
    #[error("XML local name {0:?} is invalid")]
    InvalidXmlName(String),
    #[error("XML element has duplicate attribute {namespace:?}:{local_name}")]
    DuplicateXmlAttribute {
        namespace: Option<String>,
        local_name: String,
    },
    #[error("XML namespaced attributes are not supported by the framework serializer")]
    NamespacedXmlAttributeUnsupported,
    #[error("XML payload exceeds {limit}")]
    XmlLimitExceeded { limit: &'static str },
}

macro_rules! typed_non_empty_string {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
                let value = value.into();
                if value.trim().is_empty() {
                    Err(FrameworkTypeError::Empty { field: $field })
                } else {
                    Ok(Self(value))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

typed_non_empty_string!(ActionId, "action id");
typed_non_empty_string!(CommandSessionId, "command session id");
typed_non_empty_string!(DisplayText, "display text");
typed_non_empty_string!(EnrichmentId, "enrichment id");
typed_non_empty_string!(ListId, "list id");
typed_non_empty_string!(ListItemId, "list item id");
typed_non_empty_string!(LaunchId, "launch id");
typed_non_empty_string!(LaunchToken, "launch token");
typed_non_empty_string!(MediaType, "media type");
typed_non_empty_string!(PluginVersion, "plugin version");
typed_non_empty_string!(PubSubItemId, "pubsub item id");
typed_non_empty_string!(PubSubNode, "pubsub node");
typed_non_empty_string!(StanzaId, "stanza id");
typed_non_empty_string!(Timestamp, "timestamp");
typed_non_empty_string!(ThreadId, "thread id");
typed_non_empty_string!(UiActionId, "ui action id");
typed_non_empty_string!(UiViewId, "ui view id");
typed_non_empty_string!(Url, "url");
typed_non_empty_string!(WaddleId, "waddle id");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomJid(String);

impl RoomJid {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        value
            .parse::<BareJid>()
            .map_err(|_| FrameworkTypeError::InvalidBareJid(value.clone()))?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for RoomJid {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for RoomJid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FullJidValue(String);

impl FullJidValue {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        value
            .parse::<FullJid>()
            .map_err(|_| FrameworkTypeError::InvalidFullJid(value.clone()))?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for FullJidValue {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for FullJidValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PluginId(String);

impl PluginId {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        if value.is_empty() {
            return Err(FrameworkTypeError::Empty { field: "plugin id" });
        }
        let valid = value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && value
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            && value
                .bytes()
                .last()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        if valid {
            Ok(Self(value))
        } else {
            Err(FrameworkTypeError::InvalidPluginId(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for PluginId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PayloadNamespace(String);

impl PayloadNamespace {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        if is_official_namespace(&value) {
            return Err(FrameworkTypeError::OfficialNamespace(value));
        }
        if value == FRAMEWORK_NAMESPACE {
            return Err(FrameworkTypeError::ReservedFrameworkNamespace(value));
        }
        if value.starts_with("urn:") || value.starts_with("https://") {
            Ok(Self(value))
        } else {
            Err(FrameworkTypeError::InvalidPayloadNamespace(value))
        }
    }

    pub fn framework() -> Self {
        Self(FRAMEWORK_NAMESPACE.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for PayloadNamespace {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PayloadNamespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandNode(String);

impl CommandNode {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        let valid = value == FRAMEWORK_NAMESPACE
            || value
                .strip_prefix(FRAMEWORK_NAMESPACE)
                .is_some_and(|suffix| suffix.starts_with(':'));
        if valid {
            Ok(Self(value))
        } else {
            Err(FrameworkTypeError::InvalidCommandNode(value))
        }
    }

    pub fn invoke() -> Self {
        Self(INVOKE_COMMAND_NODE.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for CommandNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(FrameworkTypeError::InvalidSha256Digest(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactUri(String);

impl ArtifactUri {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        let immutable_http = (value.starts_with("https://") || value.starts_with("http://"))
            && value.contains("/sha256/");
        if immutable_http {
            Ok(Self(value))
        } else {
            Err(FrameworkTypeError::InvalidArtifactUri(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactReference {
    pub uri: ArtifactUri,
    pub sha256: Sha256Digest,
    pub media_type: Option<MediaType>,
}

impl ArtifactReference {
    pub fn new(
        uri: impl Into<String>,
        sha256: impl Into<String>,
        media_type: Option<MediaType>,
    ) -> Result<Self, FrameworkTypeError> {
        let uri = ArtifactUri::new(uri)?;
        let sha256 = Sha256Digest::new(sha256)?;
        if !uri.as_str().to_ascii_lowercase().contains(sha256.as_str()) {
            return Err(FrameworkTypeError::ArtifactDigestMismatch {
                uri: uri.as_str().to_string(),
                sha256: sha256.as_str().to_string(),
            });
        }
        Ok(Self {
            uri,
            sha256,
            media_type,
        })
    }

    fn add_attrs(&self, builder: minidom::ElementBuilder) -> minidom::ElementBuilder {
        let mut builder = builder
            .attr("artifact-uri", self.uri.as_str())
            .attr("artifact-sha256", self.sha256.as_str());
        if let Some(media_type) = &self.media_type {
            builder = builder.attr("media-type", media_type.as_str());
        }
        builder
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BodyRange {
    pub start: u32,
    pub end: u32,
}

impl BodyRange {
    pub fn new(start: u32, end: u32) -> Result<Self, FrameworkTypeError> {
        if end > start {
            Ok(Self { start, end })
        } else {
            Err(FrameworkTypeError::InvalidBodyRange { start, end })
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ExtensionCapability {
    #[serde(rename = "message.enrich")]
    MessageEnrich,
    #[serde(rename = "message.observe")]
    MessageObserve,
    #[serde(rename = "bot.groupchat.send")]
    BotGroupchatSend,
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
            Self::BotGroupchatSend => "bot.groupchat.send",
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
    pub pubsub_nodes: Vec<PubSubNode>,
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
    pub source_stanza_id: Option<StanzaId>,
}

impl LaunchContext {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("context", FRAMEWORK_NAMESPACE)
            .attr("waddle-id", self.waddle_id.as_str());
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiView {
    pub id: UiViewId,
    pub title: Option<DisplayText>,
    pub blocks: Vec<UiBlock>,
}

impl UiView {
    fn to_minidom(&self) -> Element {
        let mut builder =
            Element::builder("view", FRAMEWORK_NAMESPACE).attr("id", self.id.as_str());
        if let Some(title) = &self.title {
            builder = builder.attr("title", title.as_str());
        }
        for block in &self.blocks {
            builder = builder.append(block.to_minidom());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum UiBlock {
    Text(TextBlock),
    Image(ImageBlock),
    Action(ActionBlock),
    Form(DataForm),
    List(ListView),
}

impl UiBlock {
    fn to_minidom(&self) -> Element {
        match self {
            Self::Text(block) => block.to_minidom(),
            Self::Image(block) => block.to_minidom(),
            Self::Action(block) => block.to_minidom(),
            Self::Form(form) => form.to_minidom(),
            Self::List(list) => list.to_minidom(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum TextStyle {
    Body,
    Heading,
    Muted,
    Code,
}

impl TextStyle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Heading => "heading",
            Self::Muted => "muted",
            Self::Code => "code",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextBlock {
    pub text: DisplayText,
    pub style: TextStyle,
}

impl TextBlock {
    fn to_minidom(&self) -> Element {
        Element::builder("text", FRAMEWORK_NAMESPACE)
            .attr("style", self.style.as_str())
            .append(self.text.as_str().to_string())
            .build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageBlock {
    pub artifact: ArtifactReference,
    pub alt: DisplayText,
}

impl ImageBlock {
    fn to_minidom(&self) -> Element {
        self.artifact
            .add_attrs(
                Element::builder("image", FRAMEWORK_NAMESPACE).attr("alt", self.alt.as_str()),
            )
            .build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionBlock {
    pub launch_id: LaunchId,
    pub label: DisplayText,
}

impl ActionBlock {
    fn to_minidom(&self) -> Element {
        Element::builder("action", FRAMEWORK_NAMESPACE)
            .attr("launch-id", self.launch_id.as_str())
            .attr("label", self.label.as_str())
            .build()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum DataFormType {
    Form,
    Submit,
    Cancel,
    Result,
}

impl DataFormType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Form => "form",
            Self::Submit => "submit",
            Self::Cancel => "cancel",
            Self::Result => "result",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum FormFieldType {
    Boolean,
    Fixed,
    Hidden,
    JidMulti,
    JidSingle,
    ListMulti,
    ListSingle,
    TextMulti,
    TextPrivate,
    TextSingle,
}

impl FormFieldType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Fixed => "fixed",
            Self::Hidden => "hidden",
            Self::JidMulti => "jid-multi",
            Self::JidSingle => "jid-single",
            Self::ListMulti => "list-multi",
            Self::ListSingle => "list-single",
            Self::TextMulti => "text-multi",
            Self::TextPrivate => "text-private",
            Self::TextSingle => "text-single",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormFieldOption {
    pub label: Option<DisplayText>,
    pub value: DataFormValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataFormField {
    pub name: UiActionId,
    pub field_type: FormFieldType,
    pub label: Option<DisplayText>,
    pub required: bool,
    pub values: Vec<DataFormValue>,
    pub options: Vec<FormFieldOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataFormValue(String);

impl DataFormValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataForm {
    pub form_type: DataFormType,
    pub title: Option<DisplayText>,
    pub instructions: Vec<DisplayText>,
    pub fields: Vec<DataFormField>,
}

impl DataForm {
    fn to_minidom(&self) -> Element {
        let mut builder =
            Element::builder("form", FRAMEWORK_NAMESPACE).attr("type", self.form_type.as_str());
        if let Some(title) = &self.title {
            builder = builder.attr("title", title.as_str());
        }
        for instruction in &self.instructions {
            builder = builder.append(
                Element::builder("instructions", FRAMEWORK_NAMESPACE)
                    .append(instruction.as_str().to_string())
                    .build(),
            );
        }
        for field in &self.fields {
            let mut field_builder = Element::builder("field", FRAMEWORK_NAMESPACE)
                .attr("var", field.name.as_str())
                .attr("type", field.field_type.as_str());
            if let Some(label) = &field.label {
                field_builder = field_builder.attr("label", label.as_str());
            }
            if field.required {
                field_builder =
                    field_builder.append(Element::builder("required", FRAMEWORK_NAMESPACE).build());
            }
            for value in &field.values {
                field_builder = field_builder.append(
                    Element::builder("value", FRAMEWORK_NAMESPACE)
                        .append(value.as_str().to_string())
                        .build(),
                );
            }
            for option in &field.options {
                let mut option_builder = Element::builder("option", FRAMEWORK_NAMESPACE);
                if let Some(label) = &option.label {
                    option_builder = option_builder.attr("label", label.as_str());
                }
                field_builder = field_builder.append(
                    option_builder
                        .append(
                            Element::builder("value", FRAMEWORK_NAMESPACE)
                                .append(option.value.as_str().to_string())
                                .build(),
                        )
                        .build(),
                );
            }
            builder = builder.append(field_builder.build());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListView {
    pub id: ListId,
    pub title: Option<DisplayText>,
    pub items: Vec<ListItem>,
}

impl ListView {
    fn to_minidom(&self) -> Element {
        let mut builder =
            Element::builder("list", FRAMEWORK_NAMESPACE).attr("id", self.id.as_str());
        if let Some(title) = &self.title {
            builder = builder.attr("title", title.as_str());
        }
        for item in &self.items {
            builder = builder.append(item.to_minidom());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListItem {
    pub id: ListItemId,
    pub label: DisplayText,
    pub description: Option<DisplayText>,
    pub image: Option<ArtifactReference>,
    pub launch_id: Option<LaunchId>,
}

impl ListItem {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("item", FRAMEWORK_NAMESPACE)
            .attr("id", self.id.as_str())
            .attr("label", self.label.as_str());
        if let Some(description) = &self.description {
            builder = builder.attr("description", description.as_str());
        }
        if let Some(launch_id) = &self.launch_id {
            builder = builder.attr("launch-id", launch_id.as_str());
        }
        if let Some(image) = &self.image {
            builder = image.add_attrs(builder);
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XmlAttribute {
    pub namespace: Option<PayloadNamespace>,
    pub local_name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum XmlNode {
    Element(XmlElement),
    Text(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XmlElement {
    pub namespace: PayloadNamespace,
    pub local_name: String,
    pub attributes: Vec<XmlAttribute>,
    pub children: Vec<XmlNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionPayload {
    pub namespace: PayloadNamespace,
    pub root: XmlElement,
}

impl ExtensionPayload {
    pub fn new(namespace: PayloadNamespace, root: XmlElement) -> Result<Self, FrameworkTypeError> {
        if root.namespace != namespace {
            return Err(FrameworkTypeError::InvalidPayloadNamespace(
                root.namespace.as_str().to_string(),
            ));
        }
        root.validate(0)?;
        let payload = Self { namespace, root };
        if payload.serialized_len() > MAX_XML_SERIALIZED_BYTES {
            return Err(FrameworkTypeError::XmlLimitExceeded {
                limit: "serialized payload bytes",
            });
        }
        Ok(payload)
    }

    pub fn to_minidom(&self) -> Element {
        self.root.to_minidom()
    }

    fn serialized_len(&self) -> usize {
        self.root.serialized_len()
    }
}

impl XmlElement {
    pub fn new(
        namespace: PayloadNamespace,
        local_name: impl Into<String>,
        attributes: Vec<XmlAttribute>,
        children: Vec<XmlNode>,
    ) -> Result<Self, FrameworkTypeError> {
        let element = Self {
            namespace,
            local_name: validate_xml_local_name(local_name.into())?,
            attributes,
            children,
        };
        element.validate(0)?;
        Ok(element)
    }

    fn validate(&self, depth: usize) -> Result<(), FrameworkTypeError> {
        if depth > MAX_XML_DEPTH {
            return Err(FrameworkTypeError::XmlLimitExceeded { limit: "depth" });
        }
        validate_xml_local_name(self.local_name.clone())?;
        if self.attributes.len() > MAX_XML_ATTRIBUTES {
            return Err(FrameworkTypeError::XmlLimitExceeded {
                limit: "attributes per element",
            });
        }
        if self.children.len() > MAX_XML_CHILDREN {
            return Err(FrameworkTypeError::XmlLimitExceeded {
                limit: "children per element",
            });
        }
        let mut seen = std::collections::HashSet::new();
        for attr in &self.attributes {
            validate_xml_local_name(attr.local_name.clone())?;
            if attr.namespace.is_some() {
                return Err(FrameworkTypeError::NamespacedXmlAttributeUnsupported);
            }
            let key = (
                attr.namespace.as_ref().map(|ns| ns.as_str().to_string()),
                attr.local_name.clone(),
            );
            if !seen.insert(key.clone()) {
                return Err(FrameworkTypeError::DuplicateXmlAttribute {
                    namespace: key.0,
                    local_name: key.1,
                });
            }
        }
        for child in &self.children {
            match child {
                XmlNode::Element(element) => element.validate(depth + 1)?,
                XmlNode::Text(text) if text.len() > MAX_XML_TEXT_BYTES => {
                    return Err(FrameworkTypeError::XmlLimitExceeded {
                        limit: "text node bytes",
                    });
                }
                XmlNode::Text(_) => {}
            }
        }
        Ok(())
    }

    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder(self.local_name.as_str(), self.namespace.as_str());
        for attr in &self.attributes {
            debug_assert!(attr.namespace.is_none());
            builder = builder.attr(attr.local_name.as_str(), attr.value.as_str());
        }
        for child in &self.children {
            builder = match child {
                XmlNode::Element(element) => builder.append(element.to_minidom()),
                XmlNode::Text(text) => builder.append(text.clone()),
            };
        }
        builder.build()
    }

    fn serialized_len(&self) -> usize {
        self.local_name.len()
            + self.namespace.as_str().len()
            + self
                .attributes
                .iter()
                .map(|attr| {
                    attr.local_name.len()
                        + attr.value.len()
                        + attr
                            .namespace
                            .as_ref()
                            .map(|ns| ns.as_str().len())
                            .unwrap_or(0)
                })
                .sum::<usize>()
            + self
                .children
                .iter()
                .map(|child| match child {
                    XmlNode::Element(element) => element.serialized_len(),
                    XmlNode::Text(text) => text.len(),
                })
                .sum::<usize>()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExtensionEvent {
    MessageHook(MessageHook),
    Command(CommandInvocation),
    Launch(LaunchInvocation),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageHook {
    pub context: MessageContext,
    pub body: DisplayText,
    pub links: Vec<LinkTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageContext {
    pub waddle_id: WaddleId,
    pub stanza_id: Option<StanzaId>,
    pub room: Option<RoomJid>,
    pub sender: Option<FullJidValue>,
    pub thread_id: Option<ThreadId>,
    pub reply_to: Option<ReplyTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkTarget {
    pub url: Url,
    pub range: BodyRange,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyTarget {
    pub id: StanzaId,
    pub to: Option<FullJidValue>,
}

impl TryFrom<DetectedLink> for LinkTarget {
    type Error = FrameworkTypeError;

    fn try_from(value: DetectedLink) -> Result<Self, Self::Error> {
        Ok(Self {
            url: Url::new(value.url)?,
            range: BodyRange::new(value.start_offset, value.end_offset)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandInvocation {
    pub waddle_id: WaddleId,
    pub command_node: CommandNode,
    pub session_id: Option<CommandSessionId>,
    pub action: Option<CommandAction>,
    pub form: Option<DataForm>,
    pub fields: Vec<FormFieldValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormFieldValue {
    pub name: UiActionId,
    pub values: Vec<DataFormValue>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum CommandAction {
    Execute,
    Next,
    Prev,
    Complete,
    Cancel,
}

impl CommandAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Next => "next",
            Self::Prev => "prev",
            Self::Complete => "complete",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchInvocation {
    pub context: LaunchContext,
    pub requester: WaddleId,
    pub launch_id: LaunchId,
    pub session_id: Option<CommandSessionId>,
    pub action: Option<CommandAction>,
    pub form: Option<DataForm>,
    pub fields: Vec<FormFieldValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionResponse {
    pub effects: Vec<ExtensionEffect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExtensionEffect {
    EnrichMessage(ExtensionEnvelope),
    BotGroupchatResponse(BotGroupchatResponse),
    PublishPubSub(PubSubPublish),
    ReferenceArtifact(ArtifactReference),
    HostWarning(DisplayText),
    Noop,
}

impl ExtensionEffect {
    pub fn validate_for_manifest(&self, manifest: &ExtensionManifest) -> bool {
        match self {
            Self::EnrichMessage(envelope) => envelope.enrichments.iter().all(|enrichment| {
                enrichment.plugin == manifest.id
                    && enrichment.capability == ExtensionCapability::MessageEnrich
                    && manifest.declares_capability(ExtensionCapability::MessageEnrich)
                    && enrichment.payloads_match_declared_namespace()
                    && enrichment.payloads.iter().all(|payload| {
                        manifest.declares_payload(PayloadSurface::MessageEnrichment, payload)
                    })
                    && enrichment.launches.iter().all(|launch| {
                        launch.plugin == manifest.id
                            && manifest.declares_capability(ExtensionCapability::Launch)
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
                    && manifest.declares_pubsub_node(&publish.node)
                    && publish.payload.namespace == publish.payload.root.namespace
                    && manifest.declares_payload(PayloadSurface::PubSubItem, &publish.payload)
            }
            Self::BotGroupchatResponse(_) => {
                manifest.declares_capability(ExtensionCapability::BotGroupchatSend)
            }
            Self::ReferenceArtifact(_) => {
                manifest.declares_capability(ExtensionCapability::ArtifactReference)
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
pub struct BotGroupchatResponse {
    pub body: DisplayText,
    pub room: RoomJid,
    pub thread_id: Option<ThreadId>,
    pub reply_to: Option<ReplyTarget>,
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

pub fn is_official_namespace(value: &str) -> bool {
    value.starts_with("urn:xmpp:")
        || value.starts_with("jabber:")
        || value.starts_with("http://jabber.org/")
}

fn validate_xml_local_name(value: String) -> Result<String, FrameworkTypeError> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(FrameworkTypeError::InvalidXmlName(value));
    };
    let valid_start = first == '_' || first.is_ascii_alphabetic();
    let valid_rest =
        chars.all(|ch| ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_alphanumeric());
    if valid_start && valid_rest && !value.contains(':') && !value.starts_with("xml") {
        Ok(value)
    } else {
        Err(FrameworkTypeError::InvalidXmlName(value))
    }
}
