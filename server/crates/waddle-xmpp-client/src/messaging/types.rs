use chrono::{DateTime, Utc};
use waddle_xmpp_core::xep0359::StanzaId as StableStanzaId;

use crate::request::StanzaId;
use crate::xep::encrypted_file::EncryptedFile;
use crate::xep::{reply as xep_reply, thread as xep_thread};

#[derive(Debug, Clone, PartialEq)]
pub struct MarkupSpan {
    pub span_type: MarkupSpanType,
    pub start: usize,
    pub end: usize,
    pub uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkupSpanType {
    Bold,
    Italic,
    Strikethrough,
    Code,
    CodeBlock,
    Blockquote,
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SharedFileDisposition {
    Inline,
    #[default]
    Attachment,
}

impl SharedFileDisposition {
    pub fn from_text_or_infer(value: Option<&str>, media_type: Option<&str>) -> Self {
        match value {
            Some("inline") => Self::Inline,
            Some("attachment") => Self::Attachment,
            _ => Self::infer_from_media_type(media_type),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Attachment => "attachment",
        }
    }

    fn infer_from_media_type(media_type: Option<&str>) -> Self {
        if media_type.is_some_and(|m| {
            m.starts_with("image/")
                || m.starts_with("video/")
                || m.starts_with("audio/")
                || m == "application/pdf"
        }) {
            Self::Inline
        } else {
            Self::Attachment
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFile {
    pub url: String,
    pub name: Option<String>,
    pub media_type: Option<String>,
    pub size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub disposition: SharedFileDisposition,
    /// XEP-0448 encryption envelope, when the bytes at `url` are ciphertext
    /// rather than the plaintext file. Recipients MUST use these values to
    /// decrypt before rendering.
    pub encrypted: Option<EncryptedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatStatePayload {
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayedMarkerPayload {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionPayload {
    pub target_id: String,
    pub emojis: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetractionPayload {
    pub target_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetractionTombstonePayload {
    pub retraction_id: Option<String>,
    pub moderated_by: Option<jid::Jid>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationPayload {
    pub target_id: String,
    pub moderated_by: Option<jid::Jid>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionPayload {
    pub replaces_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboundMessage {
    pub from: Option<String>,
    pub to: Option<String>,
    pub message_type: String,
    pub id: Option<String>,
    pub stanza_id: Option<StableStanzaId>,
    pub stanza_ids: Vec<StableStanzaId>,
    pub origin_id: Option<String>,
    pub body: Option<String>,
    pub subject: Option<String>,
    pub thread: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub replaces_id: Option<String>,
    pub retracts_id: Option<String>,
    pub retraction_id: Option<String>,
    pub is_retracted: bool,
    pub moderation_target_id: Option<String>,
    pub moderated_by: Option<jid::Jid>,
    pub moderation_reason: Option<String>,
    pub reaction_target_id: Option<String>,
    pub reaction_emojis: Vec<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<String>,
    /// XEP-0428 fallback range, in Unicode scalar offsets, end exclusive.
    pub reply_fallback: Option<(u32, u32)>,
    pub markup_spans: Vec<MarkupSpan>,
    pub chat_state: Option<String>,
    pub displayed_marker_id: Option<String>,
    pub shared_files: Vec<SharedFile>,
    pub broadcast_mention: Option<String>,
    pub mention_uris: Vec<String>,
    /// XEP-0372 references attached to this message. Populated for *every*
    /// `<reference xmlns="urn:xmpp:reference:0"/>` child, regardless of type
    /// (`mention`, `data`, or any other), as long as `type` and `uri` are
    /// present (both required by XEP-0372).
    pub references: Vec<ReferenceData>,
    pub forum_post_kind: Option<String>,
    pub forum_title: Option<String>,
    pub thread_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub is_sticker: bool,
    /// urn:waddle:pin:0 pin/unpin system event surfaced by the room.
    /// `None` when the message carries no `<pin-event/>` payload.
    pub pin_event: Option<crate::pin::PinEvent>,
    /// Waddle typed extension envelope attached to the message.
    pub extension_envelope: Option<ExtensionEnvelopeData>,
    /// XEP-0428 marks the readable body as fallback for the Waddle extension payload.
    pub extension_body_fallback: bool,
    /// XEP-0490 §3 Message Displayed Synchronization PEP event
    /// entries, when the inbound `<message>` carries a
    /// `<event xmlns='...#event'><items node='urn:xmpp:mds:displayed:0'>`
    /// payload. Each entry maps a chat JID (PEP item id) to the
    /// XEP-0359 stanza-id of the latest displayed message in that
    /// chat. Empty `Vec` is distinct from `None`: `None` means the
    /// message is not an MDS event, `Some(vec![])` means an MDS
    /// event with no items (e.g. retracted via empty publish — not
    /// a shape XEP-0490 currently defines but cheap to surface
    /// faithfully).
    pub mds_displayed: Option<Vec<MdsDisplayedEntry>>,
}

/// XEP-0490 §3 displayed entry as surfaced from a PEP event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdsDisplayedEntry {
    /// PEP item id = bare JID of the chat (DM contact or MUC room).
    pub chat_id: String,
    /// XEP-0359 id of the displayed message.
    pub stanza_id: String,
    /// JID that injected the stanza-id (the MUC room for group
    /// chats; the user's own server for 1:1 chats).
    pub stanza_id_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionEnvelopeData {
    pub version: u32,
    pub enrichments: Vec<ExtensionEnrichmentData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionEnrichmentData {
    pub id: ExtensionTextId,
    pub plugin: ExtensionPluginId,
    pub capability: ExtensionCapabilityData,
    pub payload_namespace: ExtensionNamespace,
    pub created: ExtensionTimestamp,
    pub source: Option<ExtensionSourceData>,
    pub title: Option<ExtensionDisplayText>,
    pub summary: Option<ExtensionDisplayText>,
    pub payloads: Vec<ExtensionPayloadElementData>,
    pub launches: Vec<ExtensionLaunchData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionPayloadElementData {
    pub namespace: ExtensionNamespace,
    pub name: ExtensionXmlName,
    pub attributes: Vec<ExtensionPayloadAttributeData>,
    pub text: Option<String>,
    pub children: Vec<ExtensionPayloadElementData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionPayloadAttributeData {
    pub name: ExtensionXmlName,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionSourceData {
    pub stanza_id: ExtensionTextId,
    pub body_start: Option<u32>,
    pub body_end: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionLaunchData {
    pub id: ExtensionTextId,
    pub plugin: ExtensionPluginId,
    pub action: ExtensionTextId,
    pub command_node: ExtensionCommandNode,
    pub label: ExtensionDisplayText,
    pub context: ExtensionLaunchContextData,
    pub payloads: Vec<ExtensionPayloadElementData>,
    pub expires_at: Option<ExtensionTimestamp>,
    pub token: Option<ExtensionTextId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionLaunchContextData {
    pub waddle_id: ExtensionTextId,
    pub room: Option<ExtensionRoomJid>,
    pub source_stanza_id: Option<ExtensionTextId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtensionCapabilityData {
    MessageEnrich,
    MessageObserve,
    HostChannelsRead,
    HostSpacesRead,
    HostMembersRead,
    HostPresenceRead,
    HostMamRead,
    HostRosterRead,
    HostMessageSend,
    OutboundHttpRequest,
    Commands,
    Launch,
    PubSubPublish,
    ArtifactReference,
    UiDeclarative,
}

impl ExtensionCapabilityData {
    pub fn from_attr(value: &str) -> Option<Self> {
        match value {
            "message.enrich" => Some(Self::MessageEnrich),
            "message.observe" => Some(Self::MessageObserve),
            "host.channels.read" => Some(Self::HostChannelsRead),
            "host.spaces.read" => Some(Self::HostSpacesRead),
            "host.members.read" => Some(Self::HostMembersRead),
            "host.presence.read" => Some(Self::HostPresenceRead),
            "host.mam.read" => Some(Self::HostMamRead),
            "host.roster.read" => Some(Self::HostRosterRead),
            "host.message.send" => Some(Self::HostMessageSend),
            "outbound.http.request" => Some(Self::OutboundHttpRequest),
            "commands" => Some(Self::Commands),
            "launch" => Some(Self::Launch),
            "pubsub.publish" => Some(Self::PubSubPublish),
            "artifact.reference" => Some(Self::ArtifactReference),
            "ui.declarative" => Some(Self::UiDeclarative),
            _ => None,
        }
    }

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionTextId(String);

impl ExtensionTextId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionDisplayText(String);

impl ExtensionDisplayText {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionTimestamp(String);

impl ExtensionTimestamp {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        chrono::DateTime::parse_from_rfc3339(value.as_str())
            .ok()
            .map(|_| Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionCommandNode(String);

impl ExtensionCommandNode {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let valid = value == crate::messaging::namespaces::NS_WADDLE_EXTENSION
            || value
                .strip_prefix(crate::messaging::namespaces::NS_WADDLE_EXTENSION)
                .is_some_and(|suffix| suffix.starts_with(':'));
        valid.then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionRoomJid(String);

impl ExtensionRoomJid {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        value.parse::<jid::BareJid>().ok()?;
        Some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionPluginId(String);

impl ExtensionPluginId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
        valid.then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionNamespace(String);

impl ExtensionNamespace {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionXmlName(String);

impl ExtensionXmlName {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresenceHat {
    pub uri: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucAffiliation {
    Owner,
    Admin,
    Member,
    Outcast,
    None,
}

impl MucAffiliation {
    pub fn from_attr(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            "outcast" => Some(Self::Outcast),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Outcast => "outcast",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucRole {
    Moderator,
    Participant,
    Visitor,
    None,
}

impl MucRole {
    pub fn from_attr(value: &str) -> Option<Self> {
        match value {
            "moderator" => Some(Self::Moderator),
            "participant" => Some(Self::Participant),
            "visitor" => Some(Self::Visitor),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Moderator => "moderator",
            Self::Participant => "participant",
            Self::Visitor => "visitor",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboundPresence {
    pub from: Option<String>,
    pub to: Option<String>,
    pub presence_type: Option<String>,
    pub status: Option<String>,
    pub show: Option<String>,
    pub hats: Vec<PresenceHat>,
    pub muc_affiliation: Option<MucAffiliation>,
    pub muc_role: Option<MucRole>,
    pub muc_jid: Option<String>,
    pub vcard_avatar: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessagingEvent {
    Message(Box<InboundMessage>),
    Presence(InboundPresence),
}

// ─── Outbound options ────────────────────────────────────────────────────

/// Options attached to an outbound chat or groupchat message, carrying
/// typed XEP payloads. Protocol values flow through typed XEP structs —
/// never raw `String` blobs — per the typed-payloads hard rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendMessageOptions {
    /// Caller-supplied stanza id for optimistic UI / delivery correlation.
    /// Generated by the client when omitted.
    pub stanza_id: Option<StanzaId>,
    /// Optional RFC 6121 `<subject/>`, used by forum topic posts.
    pub subject: Option<String>,
    /// XEP-0461 reply marker identifying the quoted message.
    pub reply: Option<xep_reply::ReplyMarker>,
    /// XEP-0428 fallback range over the body identifying the quoted prefix.
    /// Offsets count Unicode scalar values and `end` is exclusive.
    pub fallback: Option<xep_reply::FallbackRange>,
    /// XEP-0201 thread reference (with optional parent for nested threads).
    pub thread: Option<xep_thread::ThreadRef>,
    /// XEP-0394 message styling spans.
    pub markup_spans: Vec<MarkupSpanData>,
    /// XEP-0372 references such as mentions.
    pub references: Vec<ReferenceData>,
    /// XEP-0446 / XEP-0447 shared files attached to the message.
    pub shared_files: Vec<SharedFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkupSpanData {
    pub span_type: String,
    pub start: u32,
    pub end: u32,
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferenceData {
    pub ref_type: String,
    pub uri: String,
    pub begin: u32,
    pub end: u32,
    /// XEP-0372 optional `anchor` attribute. Carries the original, unresolved
    /// display text (or arbitrary anchor URI) when offsets cannot be relied on.
    pub anchor: Option<String>,
}
