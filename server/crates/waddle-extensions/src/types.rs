use std::fmt;

use minidom::Element;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use xmpp_parsers::message::Message;

pub const FRAMEWORK_NAMESPACE: &str = "urn:waddle:extension:1";
pub const LINKS_TASK_BOARD_NAMESPACE: &str = "urn:waddle:links-task-board:1";
pub const PUB_QUIZ_NAMESPACE: &str = "urn:waddle:pub-quiz:1";
pub const AI_CHATBOT_NAMESPACE: &str = "urn:waddle:ai-chatbot:1";
pub const AI_ASSISTANT_CANVAS_NAMESPACE: &str = "urn:waddle:ai-assistant-canvas:1";
pub const DECISION_POLLS_NAMESPACE: &str = "urn:waddle:decision-polls:1";
pub const INVOKE_COMMAND_NODE: &str = "urn:waddle:extension:1:invoke";

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum FrameworkTypeError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("plugin id {0:?} must use lowercase ASCII letters, digits, and hyphens")]
    InvalidPluginId(String),
    #[error("namespace {0:?} must be Waddle-owned and start with urn:waddle:")]
    NonWaddleNamespace(String),
    #[error("official XMPP namespace {0:?} cannot carry Waddle extension semantics")]
    OfficialNamespace(String),
    #[error("command node {0:?} must be under urn:waddle:extension:1")]
    InvalidCommandNode(String),
    #[error("sha256 digest {0:?} must be exactly 64 hexadecimal characters")]
    InvalidSha256Digest(String),
    #[error("artifact URI {0:?} must be immutable HTTP(S) and include /sha256/")]
    InvalidArtifactUri(String),
    #[error("artifact URI {uri:?} must include digest {sha256:?}")]
    ArtifactDigestMismatch { uri: String, sha256: String },
    #[error("body range end {end} must be greater than start {start}")]
    InvalidBodyRange { start: u32, end: u32 },
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
typed_non_empty_string!(BoardCardId, "board card id");
typed_non_empty_string!(BoardColumnId, "board column id");
typed_non_empty_string!(BoardId, "board id");
typed_non_empty_string!(CanvasId, "canvas id");
typed_non_empty_string!(CollectionId, "collection id");
typed_non_empty_string!(DisplayText, "display text");
typed_non_empty_string!(EnrichmentId, "enrichment id");
typed_non_empty_string!(GameId, "game id");
typed_non_empty_string!(ListId, "list id");
typed_non_empty_string!(ListItemId, "list item id");
typed_non_empty_string!(LaunchId, "launch id");
typed_non_empty_string!(MediaType, "media type");
typed_non_empty_string!(OptionId, "option id");
typed_non_empty_string!(PluginVersion, "plugin version");
typed_non_empty_string!(ProfileId, "profile id");
typed_non_empty_string!(PubSubItemId, "pubsub item id");
typed_non_empty_string!(PubSubNode, "pubsub node");
typed_non_empty_string!(QuestionId, "question id");
typed_non_empty_string!(RenderId, "render id");
typed_non_empty_string!(RunId, "run id");
typed_non_empty_string!(StanzaId, "stanza id");
typed_non_empty_string!(Timestamp, "timestamp");
typed_non_empty_string!(UiActionId, "ui action id");
typed_non_empty_string!(UiViewId, "ui view id");
typed_non_empty_string!(Url, "url");
typed_non_empty_string!(WaddleId, "waddle id");

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
        if value.starts_with("urn:waddle:") {
            Ok(Self(value))
        } else {
            Err(FrameworkTypeError::NonWaddleNamespace(value))
        }
    }

    pub fn framework() -> Self {
        Self(FRAMEWORK_NAMESPACE.to_string())
    }

    pub fn links_task_board() -> Self {
        Self(LINKS_TASK_BOARD_NAMESPACE.to_string())
    }

    pub fn pub_quiz() -> Self {
        Self(PUB_QUIZ_NAMESPACE.to_string())
    }

    pub fn ai_chatbot() -> Self {
        Self(AI_CHATBOT_NAMESPACE.to_string())
    }

    pub fn ai_assistant_canvas() -> Self {
        Self(AI_ASSISTANT_CANVAS_NAMESPACE.to_string())
    }

    pub fn decision_polls() -> Self {
        Self(DECISION_POLLS_NAMESPACE.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
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
pub enum SamplePlugin {
    LinksTaskBoard,
    PubQuiz,
    AiChatbot,
    AiAssistantCanvas,
    DecisionPolls,
}

impl SamplePlugin {
    pub const ALL: [Self; 5] = [
        Self::LinksTaskBoard,
        Self::PubQuiz,
        Self::AiChatbot,
        Self::AiAssistantCanvas,
        Self::DecisionPolls,
    ];

    pub fn id(self) -> PluginId {
        PluginId::new(match self {
            Self::LinksTaskBoard => "links-task-board",
            Self::PubQuiz => "pub-quiz",
            Self::AiChatbot => "ai-chatbot",
            Self::AiAssistantCanvas => "ai-assistant-canvas",
            Self::DecisionPolls => "decision-polls",
        })
        .expect("sample plugin ids are valid")
    }

    pub fn payload_namespace(self) -> PayloadNamespace {
        match self {
            Self::LinksTaskBoard => PayloadNamespace::links_task_board(),
            Self::PubQuiz => PayloadNamespace::pub_quiz(),
            Self::AiChatbot => PayloadNamespace::ai_chatbot(),
            Self::AiAssistantCanvas => PayloadNamespace::ai_assistant_canvas(),
            Self::DecisionPolls => PayloadNamespace::decision_polls(),
        }
    }

    pub fn capabilities(self) -> Vec<ExtensionCapability> {
        match self {
            Self::LinksTaskBoard => vec![
                ExtensionCapability::MessageEnrich,
                ExtensionCapability::Launch,
                ExtensionCapability::Commands,
                ExtensionCapability::PubSubPublish,
                ExtensionCapability::ArtifactReference,
                ExtensionCapability::UiDeclarative,
            ],
            Self::PubQuiz => vec![
                ExtensionCapability::Commands,
                ExtensionCapability::Launch,
                ExtensionCapability::BotRespond,
                ExtensionCapability::PubSubPublish,
                ExtensionCapability::UiDeclarative,
            ],
            Self::AiChatbot => vec![
                ExtensionCapability::Commands,
                ExtensionCapability::Launch,
                ExtensionCapability::BotRespond,
                ExtensionCapability::MessageObserve,
                ExtensionCapability::AiInvoke,
            ],
            Self::AiAssistantCanvas => vec![
                ExtensionCapability::Commands,
                ExtensionCapability::Launch,
                ExtensionCapability::BotRespond,
                ExtensionCapability::ArtifactReference,
                ExtensionCapability::AiInvoke,
                ExtensionCapability::PubSubPublish,
                ExtensionCapability::UiDeclarative,
            ],
            Self::DecisionPolls => vec![
                ExtensionCapability::Commands,
                ExtensionCapability::Launch,
                ExtensionCapability::BotRespond,
                ExtensionCapability::PubSubPublish,
                ExtensionCapability::UiDeclarative,
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ExtensionCapability {
    #[serde(rename = "message.enrich")]
    MessageEnrich,
    #[serde(rename = "message.observe")]
    MessageObserve,
    #[serde(rename = "commands")]
    Commands,
    #[serde(rename = "launch")]
    Launch,
    #[serde(rename = "bot.respond")]
    BotRespond,
    #[serde(rename = "pubsub.read")]
    PubSubRead,
    #[serde(rename = "pubsub.publish")]
    PubSubPublish,
    #[serde(rename = "artifact.reference")]
    ArtifactReference,
    #[serde(rename = "ai.invoke")]
    AiInvoke,
    #[serde(rename = "ui.declarative")]
    UiDeclarative,
}

impl ExtensionCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MessageEnrich => "message.enrich",
            Self::MessageObserve => "message.observe",
            Self::Commands => "commands",
            Self::Launch => "launch",
            Self::BotRespond => "bot.respond",
            Self::PubSubRead => "pubsub.read",
            Self::PubSubPublish => "pubsub.publish",
            Self::ArtifactReference => "artifact.reference",
            Self::AiInvoke => "ai.invoke",
            Self::UiDeclarative => "ui.declarative",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionManifest {
    pub id: PluginId,
    pub name: DisplayText,
    pub version: PluginVersion,
    pub payload_namespace: PayloadNamespace,
    pub capabilities: Vec<ExtensionCapability>,
    pub artifact: Option<ArtifactReference>,
    pub bot: Option<BotIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotIdentity {
    pub localpart: PluginId,
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
    pub payloads: Vec<FrameworkPayload>,
    pub launches: Vec<LaunchDescriptor>,
}

impl MessageEnrichment {
    pub fn payloads_match_declared_namespace(&self) -> bool {
        self.payloads.iter().all(|payload| {
            let namespace = payload.payload_namespace();
            namespace == FRAMEWORK_NAMESPACE || namespace == self.payload_namespace.as_str()
        }) && self.launches.iter().all(|launch| {
            launch.payload.as_ref().is_none_or(|payload| {
                let namespace = payload.payload_namespace();
                namespace == FRAMEWORK_NAMESPACE || namespace == self.payload_namespace.as_str()
            })
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
            for payload in &self.payloads {
                payload_builder = payload_builder.append(payload.to_minidom());
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
    pub payload: Option<LaunchPayload>,
    pub expires_at: Option<Timestamp>,
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
        if let Some(expires_at) = &self.expires_at {
            builder = builder.attr("expires-at", expires_at.as_str());
        }
        if let Some(payload) = &self.payload {
            builder = builder.append(
                Element::builder("payload", FRAMEWORK_NAMESPACE)
                    .append(payload.to_minidom())
                    .build(),
            );
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FrameworkPayload {
    DeclarativeUi(UiView),
    List(ListView),
    Board(BoardView),
    LinkPreview(LinkPreview),
    QuizQuestion(QuizQuestion),
    AssistantAnswer(AssistantAnswer),
    CanvasRender(CanvasRender),
    DecisionPoll(DecisionPoll),
}

impl FrameworkPayload {
    pub fn payload_namespace(&self) -> &'static str {
        match self {
            Self::DeclarativeUi(_) | Self::List(_) | Self::Board(_) => FRAMEWORK_NAMESPACE,
            Self::LinkPreview(_) => LINKS_TASK_BOARD_NAMESPACE,
            Self::QuizQuestion(_) => PUB_QUIZ_NAMESPACE,
            Self::AssistantAnswer(_) => AI_CHATBOT_NAMESPACE,
            Self::CanvasRender(_) => AI_ASSISTANT_CANVAS_NAMESPACE,
            Self::DecisionPoll(_) => DECISION_POLLS_NAMESPACE,
        }
    }

    pub fn to_minidom(&self) -> Element {
        match self {
            Self::DeclarativeUi(view) => view.to_minidom(),
            Self::List(list) => list.to_minidom(),
            Self::Board(board) => board.to_minidom(),
            Self::LinkPreview(link) => link.to_minidom(),
            Self::QuizQuestion(question) => question.to_minidom(),
            Self::AssistantAnswer(answer) => answer.to_minidom(),
            Self::CanvasRender(canvas) => canvas.to_minidom(),
            Self::DecisionPoll(poll) => poll.to_minidom(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LaunchPayload {
    SaveLink(SaveLinkRequest),
    QuizAnswer(QuizAnswerRequest),
    AskFollowup(ChatFollowupRequest),
    CanvasRemix(CanvasRemixRequest),
    PollVote(PollVoteRequest),
    UiAction(UiActionRequest),
}

impl LaunchPayload {
    pub fn payload_namespace(&self) -> &'static str {
        match self {
            Self::SaveLink(_) => LINKS_TASK_BOARD_NAMESPACE,
            Self::QuizAnswer(_) => PUB_QUIZ_NAMESPACE,
            Self::AskFollowup(_) => AI_CHATBOT_NAMESPACE,
            Self::CanvasRemix(_) => AI_ASSISTANT_CANVAS_NAMESPACE,
            Self::PollVote(_) => DECISION_POLLS_NAMESPACE,
            Self::UiAction(_) => FRAMEWORK_NAMESPACE,
        }
    }

    pub fn to_minidom(&self) -> Element {
        match self {
            Self::SaveLink(request) => request.to_minidom(),
            Self::QuizAnswer(request) => request.to_minidom(),
            Self::AskFollowup(request) => request.to_minidom(),
            Self::CanvasRemix(request) => request.to_minidom(),
            Self::PollVote(request) => request.to_minidom(),
            Self::UiAction(request) => request.to_minidom(),
        }
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
}

impl UiBlock {
    fn to_minidom(&self) -> Element {
        match self {
            Self::Text(block) => block.to_minidom(),
            Self::Image(block) => block.to_minidom(),
            Self::Action(block) => block.to_minidom(),
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
pub struct BoardView {
    pub id: BoardId,
    pub title: Option<DisplayText>,
    pub columns: Vec<BoardColumn>,
}

impl BoardView {
    fn to_minidom(&self) -> Element {
        let mut builder =
            Element::builder("board", FRAMEWORK_NAMESPACE).attr("id", self.id.as_str());
        if let Some(title) = &self.title {
            builder = builder.attr("title", title.as_str());
        }
        for column in &self.columns {
            builder = builder.append(column.to_minidom());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardColumn {
    pub id: BoardColumnId,
    pub title: DisplayText,
    pub cards: Vec<BoardCard>,
}

impl BoardColumn {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("column", FRAMEWORK_NAMESPACE)
            .attr("id", self.id.as_str())
            .attr("title", self.title.as_str());
        for card in &self.cards {
            builder = builder.append(card.to_minidom());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardCard {
    pub id: BoardCardId,
    pub title: DisplayText,
    pub body: Option<DisplayText>,
    pub labels: Vec<DisplayText>,
    pub launch_id: Option<LaunchId>,
}

impl BoardCard {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("card", FRAMEWORK_NAMESPACE)
            .attr("id", self.id.as_str())
            .attr("title", self.title.as_str());
        if let Some(body) = &self.body {
            builder = builder.append(
                Element::builder("body", FRAMEWORK_NAMESPACE)
                    .append(body.as_str().to_string())
                    .build(),
            );
        }
        if let Some(launch_id) = &self.launch_id {
            builder = builder.attr("launch-id", launch_id.as_str());
        }
        for label in &self.labels {
            builder = builder.append(
                Element::builder("label", FRAMEWORK_NAMESPACE)
                    .append(label.as_str().to_string())
                    .build(),
            );
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkPreview {
    pub url: Url,
    pub title: Option<DisplayText>,
    pub site: Option<DisplayText>,
    pub image: Option<ArtifactReference>,
}

impl LinkPreview {
    fn to_minidom(&self) -> Element {
        let mut builder =
            Element::builder("link", LINKS_TASK_BOARD_NAMESPACE).attr("url", self.url.as_str());
        if let Some(title) = &self.title {
            builder = builder.attr("title", title.as_str());
        }
        if let Some(site) = &self.site {
            builder = builder.attr("site", site.as_str());
        }
        if let Some(image) = &self.image {
            builder = builder
                .attr("image", image.uri.as_str())
                .attr("image-sha256", image.sha256.as_str());
            if let Some(media_type) = &image.media_type {
                builder = builder.attr("media-type", media_type.as_str());
            }
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuizQuestion {
    pub game_id: GameId,
    pub question_id: QuestionId,
    pub prompt: DisplayText,
    pub choices: Vec<QuizChoice>,
    pub closes_at: Option<Timestamp>,
}

impl QuizQuestion {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("quiz-question", PUB_QUIZ_NAMESPACE)
            .attr("game-id", self.game_id.as_str())
            .attr("question-id", self.question_id.as_str());
        if let Some(closes_at) = &self.closes_at {
            builder = builder.attr("closes-at", closes_at.as_str());
        }
        builder = builder.append(
            Element::builder("prompt", PUB_QUIZ_NAMESPACE)
                .append(self.prompt.as_str().to_string())
                .build(),
        );
        for choice in &self.choices {
            builder = builder.append(choice.to_minidom());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuizChoice {
    pub id: OptionId,
    pub label: DisplayText,
}

impl QuizChoice {
    fn to_minidom(&self) -> Element {
        Element::builder("choice", PUB_QUIZ_NAMESPACE)
            .attr("id", self.id.as_str())
            .append(self.label.as_str().to_string())
            .build()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum AssistantContextSource {
    Message,
    Reply,
    Mam,
    Direct,
}

impl AssistantContextSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Reply => "reply",
            Self::Mam => "mam",
            Self::Direct => "direct",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantAnswer {
    pub run_id: RunId,
    pub profile: ProfileId,
    pub context_source: AssistantContextSource,
    pub summary: Option<DisplayText>,
}

impl AssistantAnswer {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("assistant-answer", AI_CHATBOT_NAMESPACE)
            .attr("run-id", self.run_id.as_str())
            .attr("profile", self.profile.as_str())
            .attr("context-source", self.context_source.as_str());
        if let Some(summary) = &self.summary {
            builder = builder.append(
                Element::builder("summary", AI_CHATBOT_NAMESPACE)
                    .append(summary.as_str().to_string())
                    .build(),
            );
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanvasRender {
    pub canvas_id: CanvasId,
    pub render_id: RenderId,
    pub artifact: ArtifactReference,
}

impl CanvasRender {
    fn to_minidom(&self) -> Element {
        self.artifact
            .add_attrs(
                Element::builder("canvas", AI_ASSISTANT_CANVAS_NAMESPACE)
                    .attr("canvas-id", self.canvas_id.as_str())
                    .attr("render-id", self.render_id.as_str()),
            )
            .build()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum PollMode {
    Single,
    Multiple,
}

impl PollMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multiple => "multiple",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionPoll {
    pub poll_id: OptionId,
    pub mode: PollMode,
    pub question: DisplayText,
    pub options: Vec<PollOption>,
    pub closes_at: Option<Timestamp>,
}

impl DecisionPoll {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("poll", DECISION_POLLS_NAMESPACE)
            .attr("poll-id", self.poll_id.as_str())
            .attr("mode", self.mode.as_str());
        if let Some(closes_at) = &self.closes_at {
            builder = builder.attr("closes-at", closes_at.as_str());
        }
        builder = builder.append(
            Element::builder("question", DECISION_POLLS_NAMESPACE)
                .append(self.question.as_str().to_string())
                .build(),
        );
        for option in &self.options {
            builder = builder.append(option.to_minidom());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollOption {
    pub id: OptionId,
    pub label: DisplayText,
}

impl PollOption {
    fn to_minidom(&self) -> Element {
        Element::builder("option", DECISION_POLLS_NAMESPACE)
            .attr("id", self.id.as_str())
            .append(self.label.as_str().to_string())
            .build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SaveLinkRequest {
    pub url: Url,
    pub collection_id: Option<CollectionId>,
}

impl SaveLinkRequest {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("save-link", LINKS_TASK_BOARD_NAMESPACE)
            .attr("url", self.url.as_str());
        if let Some(collection_id) = &self.collection_id {
            builder = builder.attr("collection-id", collection_id.as_str());
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuizAnswerRequest {
    pub game_id: GameId,
    pub question_id: QuestionId,
    pub choice_id: OptionId,
}

impl QuizAnswerRequest {
    fn to_minidom(&self) -> Element {
        Element::builder("answer-request", PUB_QUIZ_NAMESPACE)
            .attr("game-id", self.game_id.as_str())
            .attr("question-id", self.question_id.as_str())
            .attr("choice-id", self.choice_id.as_str())
            .build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatFollowupRequest {
    pub run_id: Option<RunId>,
    pub question: Option<DisplayText>,
}

impl ChatFollowupRequest {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("followup-request", AI_CHATBOT_NAMESPACE);
        if let Some(run_id) = &self.run_id {
            builder = builder.attr("run-id", run_id.as_str());
        }
        if let Some(question) = &self.question {
            builder = builder.append(
                Element::builder("question", AI_CHATBOT_NAMESPACE)
                    .append(question.as_str().to_string())
                    .build(),
            );
        }
        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanvasRemixRequest {
    pub canvas_id: CanvasId,
    pub render_id: RenderId,
}

impl CanvasRemixRequest {
    fn to_minidom(&self) -> Element {
        Element::builder("remix-source", AI_ASSISTANT_CANVAS_NAMESPACE)
            .attr("canvas-id", self.canvas_id.as_str())
            .attr("render-id", self.render_id.as_str())
            .build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollVoteRequest {
    pub poll_id: OptionId,
    pub option_id: OptionId,
}

impl PollVoteRequest {
    fn to_minidom(&self) -> Element {
        Element::builder("vote-request", DECISION_POLLS_NAMESPACE)
            .attr("poll-id", self.poll_id.as_str())
            .attr("option-id", self.option_id.as_str())
            .build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiActionRequest {
    pub view_id: UiViewId,
    pub action_id: UiActionId,
}

impl UiActionRequest {
    fn to_minidom(&self) -> Element {
        Element::builder("ui-action", FRAMEWORK_NAMESPACE)
            .attr("view-id", self.view_id.as_str())
            .attr("action-id", self.action_id.as_str())
            .build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExtensionEvent {
    MessageHook(MessageHook),
    Command(CommandInvocation),
    Launch(LaunchInvocation),
    PubSub(PubSubNotification),
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkTarget {
    pub url: Url,
    pub range: BodyRange,
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
    pub fields: Vec<FormFieldValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormFieldValue {
    pub name: UiActionId,
    pub values: Vec<DisplayText>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchInvocation {
    pub context: LaunchContext,
    pub launch_id: LaunchId,
    pub payload: Option<LaunchPayload>,
    pub fields: Vec<FormFieldValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PubSubNotification {
    pub node: PubSubNode,
    pub item_id: PubSubItemId,
    pub payload: FrameworkPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionResponse {
    pub effects: Vec<ExtensionEffect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExtensionEffect {
    EnrichMessage(ExtensionEnvelope),
    PublishPubSub(PubSubPublish),
    SendBotMessage(BotMessage),
    ReferenceArtifact(ArtifactReference),
    Noop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PubSubPublish {
    pub node: PubSubNode,
    pub item_id: PubSubItemId,
    pub payload: FrameworkPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotMessage {
    pub body: DisplayText,
    pub payloads: Vec<FrameworkPayload>,
    pub launches: Vec<LaunchDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectedLink {
    pub url: String,
    pub start_offset: u32,
    pub end_offset: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbedElement {
    pub element_name: String,
    pub namespace: String,
    #[serde(default)]
    pub attributes: Vec<(String, String)>,
}

impl EmbedElement {
    pub fn to_minidom(&self) -> Element {
        let mut builder = Element::builder(self.element_name.as_str(), self.namespace.as_str());
        for (key, value) in &self.attributes {
            builder = builder.attr(key.as_str(), value.as_str());
        }

        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeatureAdvertisement {
    pub namespace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionInfo {
    pub name: String,
    pub namespace: String,
    pub version: String,
    #[serde(default)]
    pub features: Vec<FeatureAdvertisement>,
}

pub fn message_has_embed_for_namespaces(msg: &Message, namespaces: &[String]) -> bool {
    msg.payloads
        .iter()
        .any(|payload| namespaces.iter().any(|ns| payload.ns() == ns.as_str()))
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
