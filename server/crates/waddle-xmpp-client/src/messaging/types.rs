//! Inbound message and presence models and outbound message options.

use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid, Jid};
use std::fmt;
use url::Url;
use waddle_xmpp_core::xep0359::StanzaId as StableStanzaId;
use waddle_xmpp_core::{CoreError, DirectVideoMediaType, PreviewImageMediaType};

use super::mentions::MentionDirectory;
use crate::error::ClientResult;
use crate::request::StanzaId;
use crate::xep::encrypted_file::EncryptedFile;
use crate::xep::{reply as xep_reply, thread as xep_thread};

#[derive(Debug, Clone, PartialEq)]
/// Half-open message-body range and its formatting or link interpretation.
pub struct MarkupSpan {
    /// Formatting or link interpretation for this body span.
    pub span_type: MarkupSpanType,
    /// Inclusive character offset in the message body.
    pub start: usize,
    /// Exclusive character offset in the message body.
    pub end: usize,
    /// URI identifying the referenced resource.
    pub uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
/// Formatting or link interpretation applied to a message-body range.
pub enum MarkupSpanType {
    /// Render the span with bold emphasis.
    Bold,
    /// Render the span with italic emphasis.
    Italic,
    /// Render the span with a line through its text.
    Strikethrough,
    /// Render the span as inline code.
    Code,
    /// Render the span as a block of preformatted code.
    CodeBlock,
    /// Render the span as quoted text.
    Blockquote,
    /// Render the span as a hyperlink using its URI.
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Whether shared content is displayed inline or offered as an attachment.
pub enum SharedFileDisposition {
    /// Display the file directly in the conversation when supported.
    Inline,
    #[default]
    /// Offer the file as a downloadable attachment.
    Attachment,
}

impl SharedFileDisposition {
    /// Honor an explicit inline/attachment value, otherwise infer inline display for supported media and PDF types.
    pub fn from_text_or_infer(value: Option<&str>, media_type: Option<&str>) -> Self {
        match value {
            Some("inline") => Self::Inline,
            Some("attachment") => Self::Attachment,
            _ => Self::infer_from_media_type(media_type),
        }
    }

    /// Borrow the stored text or canonical protocol token without allocating.
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
/// File metadata, display disposition, and download or encrypted source information.
pub struct SharedFile {
    /// Resource URL exposed to the host or user.
    pub url: String,
    /// Human-readable name advertised to clients.
    pub name: Option<String>,
    /// MIME media type of the referenced content.
    pub media_type: Option<String>,
    /// Content size in bytes.
    pub size: Option<u64>,
    /// Media width in pixels.
    pub width: Option<u32>,
    /// Media height in pixels.
    pub height: Option<u32>,
    /// XEP-0446 lang-less `<desc/>` textual fallback. Mandatory on
    /// XEP-0449 sticker files (usually an emoji); optional elsewhere.
    pub desc: Option<String>,
    /// XEP-0446 / XEP-0300 `urn:xmpp:hashes:2` content hashes inside
    /// `<file/>`. Distinct from the XEP-0448 ciphertext hashes carried
    /// on `encrypted`.
    pub hashes: Vec<crate::stickers::StickerHash>,
    /// Whether a file should be displayed inline or offered as an attachment.
    pub disposition: SharedFileDisposition,
    /// XEP-0448 encryption envelope, when the bytes at `url` are ciphertext
    /// rather than the plaintext file. Recipients MUST use these values to
    /// decrypt before rendering.
    pub encrypted: Option<EncryptedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// XEP-0085 chat-state token extracted from a message.
pub struct ChatStatePayload {
    /// Current lifecycle or protocol state.
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// XEP-0333 identifier of a message displayed by the sender.
pub struct DisplayedMarkerPayload {
    /// Identifier used to correlate this value with its protocol or domain record.
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// XEP-0333 request that recipients send displayed markers.
pub struct MarkableMarkerPayload;

#[derive(Debug, Clone, PartialEq, Eq)]
/// XEP-0444 complete reaction set targeting an earlier message.
pub struct ReactionPayload {
    /// Identifier of the message or item this operation affects.
    pub target_id: String,
    /// Complete set of reaction emoji strings for the target message.
    pub emojis: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Non-blank identifier correlating an in-call signal with its session.
pub struct InCallSessionId(String);

impl InCallSessionId {
    /// Validate a non-blank call-session identifier while retaining its original text.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::BadRequest` if the call-session identifier is blank after trimming.
    pub fn new(value: impl Into<String>) -> ClientResult<Self> {
        let value = value.into();
        let value = value.trim();
        if value.is_empty() {
            return Err(CoreError::bad_request(Some(
                "in-call session id must not be empty".to_string(),
            ))
            .into());
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Non-blank reaction text transmitted during a call.
pub struct InCallReactionEmoji(String);

impl InCallReactionEmoji {
    /// Validate non-blank reaction text while retaining its original text.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::BadRequest` if the reaction text is blank after trimming.
    pub fn new(value: impl Into<String>) -> ClientResult<Self> {
        let value = value.into();
        let value = value.trim();
        if value.is_empty() {
            return Err(CoreError::bad_request(Some(
                "in-call reaction emoji must not be empty".to_string(),
            ))
            .into());
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Call-session identifier and emoji carried by an in-call reaction message.
pub struct InCallReactionPayload {
    /// Call-session identifier used to correlate call signals.
    pub sid: InCallSessionId,
    /// Emoji text carried by the reaction.
    pub emoji: InCallReactionEmoji,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Call-session signals carried by ordinary XMPP message stanzas.
pub enum InCallSignal {
    /// An emoji reaction associated with a specific call session.
    Reaction(InCallReactionPayload),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Direct-message or room recipient determining call-reaction addressing and message type.
pub enum InCallReactionRecipient {
    /// Send a chat signal to the peer's full resource JID.
    Direct(FullJid),
    /// Broadcast a groupchat signal to the room's bare JID.
    Room(BareJid),
}

impl InCallReactionRecipient {
    /// Format the recipient's JID for an outgoing call-reaction stanza.
    pub fn to_jid_string(&self) -> String {
        match self {
            Self::Direct(jid) => jid.to_string(),
            Self::Room(jid) => jid.to_string(),
        }
    }

    /// Return the chat or groupchat message type required by the call recipient.
    pub fn message_type(&self) -> &'static str {
        match self {
            Self::Direct(_) => "chat",
            Self::Room(_) => "groupchat",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// XEP-0424 target identifier and optional tombstone metadata.
pub struct RetractionPayload {
    /// Identifier of the message or item this operation affects.
    pub target_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetractionTombstonePayload {
    pub retraction_id: Option<String>,
    pub moderated_by: Option<jid::Jid>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// XEP-0425 moderation target, moderator identity, and optional reason.
pub struct ModerationPayload {
    /// Identifier of the message or item this operation affects.
    pub target_id: String,
    /// XMPP address of the moderator responsible for the operation.
    pub moderated_by: Option<jid::Jid>,
    /// Human-readable explanation for the operation.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// XEP-0308 identifier of the message whose body is being replaced.
pub struct CorrectionPayload {
    /// Identifier of the message whose body is corrected.
    pub replaces_id: String,
}

/// XEP-0280 carbon direction of an unwrapped forwarded copy.
///
/// `Sent` mirrors a message another of our resources sent (`<sent/>`);
/// `Received` mirrors a message another of our resources received
/// (`<received/>`). Only stamped after the runtime has verified the
/// wrapping envelope came from the account's own bare JID (XEP-0280
/// §11 forgery rule) — a parsed message carrying `Some(direction)` is
/// always a verified carbon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarbonDirection {
    /// The outbound stanza was written to the transport.
    Sent,
    /// The stanza or notification was received from the peer.
    Received,
}

#[derive(Debug, Clone, PartialEq)]
/// Parsed message content, identities, rich annotations, and supported extension payloads.
pub struct InboundMessage {
    /// XMPP address of the sender.
    pub from: Option<String>,
    /// XMPP address of the recipient.
    pub to: Option<String>,
    /// XMPP message type used to route this message.
    pub message_type: String,
    /// XEP-0045 §7.5 private-message marker. Its absence does not rule out a MUC PM.
    pub muc_pm: bool,
    /// Identifier used to correlate this value with its protocol or domain record.
    pub id: Option<String>,
    /// First-listed XEP-0359 `<stanza-id/>` (a stanza may carry several,
    /// one per archiving entity). The `by` attribute is preserved but NOT
    /// verified here — consumers MUST check `by` against the expected
    /// archive authority before trusting the id (XEP-0359 §Security
    /// Considerations); prefer scanning `stanza_ids` for the authority
    /// you need instead of reaching for this first-child convenience.
    pub stanza_id: Option<StableStanzaId>,
    /// All archive-assigned XEP-0359 identifiers attached to the message.
    pub stanza_ids: Vec<StableStanzaId>,
    /// Sender-assigned XEP-0359 origin identifier.
    pub origin_id: Option<String>,
    /// Text body of the message.
    pub body: Option<String>,
    /// Optional message subject.
    pub subject: Option<String>,
    /// Conversation thread identifier.
    pub thread: Option<String>,
    /// Timestamp associated with the message or event.
    pub timestamp: Option<DateTime<Utc>>,
    /// Identifier of the message whose body is corrected.
    pub replaces_id: Option<String>,
    /// Identifier of the retracts associated with this value.
    pub retracts_id: Option<String>,
    /// Identifier of the retraction message, when one was supplied.
    pub retraction_id: Option<String>,
    /// Whether the message has been retracted and should be rendered as a tombstone.
    pub is_retracted: bool,
    /// Identifier of the moderation target associated with this value.
    pub moderation_target_id: Option<String>,
    /// XMPP address of the moderator responsible for the operation.
    pub moderated_by: Option<jid::Jid>,
    /// Human-readable explanation supplied by the moderator.
    pub moderation_reason: Option<String>,
    /// Identifier of the reaction target associated with this value.
    pub reaction_target_id: Option<String>,
    /// Complete reaction emoji set for the target message.
    pub reaction_emojis: Vec<String>,
    /// In-call signal attached to this message.
    pub in_call: Option<InCallSignal>,
    /// Identifier of the reply to associated with this value.
    pub reply_to_id: Option<String>,
    /// XMPP address of the original message sender.
    pub reply_to_sender: Option<String>,
    /// XEP-0428 fallback range, in Unicode scalar offsets, end exclusive.
    pub reply_fallback: Option<(u32, u32)>,
    /// Character ranges and formatting instructions for the message body.
    pub markup_spans: Vec<MarkupSpan>,
    /// XEP-0085 chat state advertised by the sender.
    pub chat_state: Option<String>,
    /// Whether the sender requested XEP-0333 displayed markers.
    pub displayed_marker_requested: bool,
    /// Identifier of the displayed marker associated with this value.
    pub displayed_marker_id: Option<String>,
    /// File metadata and sources attached to the message.
    pub shared_files: Vec<SharedFile>,
    /// Preview metadata attached to message URLs.
    pub link_previews: Vec<LinkPreviewData>,
    /// Room-wide or channel-wide mention classification.
    pub broadcast_mention: Option<String>,
    /// URIs identifying the targets of explicit mentions.
    pub mention_uris: Vec<String>,
    /// XEP-0372 references attached to this message. Populated for *every*
    /// `<reference xmlns="urn:xmpp:reference:0"/>` child, regardless of type
    /// (`mention`, `data`, or any other), as long as `type` and `uri` are
    /// present (both required by XEP-0372).
    pub references: Vec<ReferenceData>,
    /// Classification of this forum post.
    pub forum_post_kind: Option<String>,
    /// Title of the forum post.
    pub forum_title: Option<String>,
    /// Conversation thread identifier.
    pub thread_id: Option<String>,
    /// Parent conversation thread identifier, when present.
    pub parent_thread_id: Option<String>,
    /// urn:waddle:call-thread:0 call anchor authored by the room.
    /// `None` when the message carries no `<call-thread/>` payload.
    pub call_thread: Option<crate::xep::call_thread::CallThreadAnchor>,
    /// urn:waddle:call-thread:0 ended fastening targeting a call-thread anchor.
    pub call_thread_ended: Option<crate::xep::call_thread::CallThreadEnded>,
    /// XEP-0422 fastening carrying `urn:waddle:safety-scores:1` scores for
    /// the message named by its `<apply-to id='…'/>`. Already filtered to
    /// the one trusted sender (the room itself, bare room JID,
    /// `type='groupchat'`) by [`crate::xep::safety_scores::parse_room_safety_scores_child`];
    /// `None` for anything else, including a direct message or an
    /// occupant's own claim.
    pub safety_scores: Option<crate::xep::safety_scores::SafetyScoresFastening>,
    /// Whether the file is marked as a sticker.
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
    /// Generic typed XEP-0060 PubSub event notifications carried by
    /// `<event xmlns='http://jabber.org/protocol/pubsub#event'/>`.
    pub pubsub_events: Vec<crate::pubsub_event::PubsubEvent>,
    /// XEP-0280: `Some(direction)` when this message was unwrapped from
    /// a `<sent/>`/`<received/>` carbon envelope whose wrapping stanza
    /// passed the §11 own-bare-JID check. `None` for directly received
    /// stanzas. Stamped by the runtime, never by the parser.
    pub carbon: Option<CarbonDirection>,
}

/// XEP-0490 §3 displayed entry as surfaced from a PEP event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdsDisplayedEntry {
    /// PEP item id = chat JID: bare for DMs/rooms, full occupant for MUC PMs.
    pub chat_id: Jid,
    /// XEP-0359 id of the displayed message.
    pub stanza_id: StanzaId,
    /// Resource-less assigning entity from the XEP-0359 `by` attribute.
    pub stanza_id_by: BareJid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Metadata used to render a URL preview without reparsing protocol XML.
pub struct LinkPreviewData {
    /// URL as it appeared in the source message.
    pub original_url: Url,
    /// Canonical URL used for preview lookup and deduplication.
    pub normalized_url: Option<Url>,
    /// Human-readable title rendered by the host.
    pub title: Option<String>,
    /// Longer human-readable description.
    pub description: Option<String>,
    /// Image metadata exposed by the link preview.
    pub image: Option<LinkPreviewImageData>,
    /// Native-playable media advertised by the page's `og:video` with a direct
    /// (non-iframe) media type. Rendered in a native `<video>` on user action.
    pub video: Option<LinkPreviewVideoData>,
    /// Embeddable player metadata exposed by the preview.
    pub player_embed: Option<LinkPreviewPlayer>,
    /// Whether the preview's remote media is known to be unavailable.
    pub remote_media_unavailable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Embeddable player resource and dimensions supplied by a link preview.
pub struct LinkPreviewPlayer {
    /// Resource URL exposed to the host or user.
    pub url: Url,
    /// Media width in pixels.
    pub width: Option<u32>,
    /// Media height in pixels.
    pub height: Option<u32>,
}

/// Native-playable `og:video` media surfaced from an XEP-0511 link card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewVideoData {
    pub url: Url,
    pub media_type: DirectVideoMediaType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewImageData {
    pub url: Url,
    pub media_type: PreviewImageMediaType,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Extension-produced enrichment envelope parsed from an incoming message.
pub struct ExtensionEnvelopeData {
    /// Version advertised by the extension or protocol entity.
    pub version: u32,
    /// Extension-produced enrichments attached to the message.
    pub enrichments: Vec<ExtensionEnrichmentData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One extension's enrichment identity, payload, attribution, and launch metadata.
pub struct ExtensionEnrichmentData {
    /// Identifier used to correlate this value with its protocol or domain record.
    pub id: ExtensionTextId,
    /// Extension identifier owning this value.
    pub plugin: ExtensionPluginId,
    /// Host permission required by this operation.
    pub capability: ExtensionCapabilityData,
    /// Namespace identifying the extension payload vocabulary.
    pub payload_namespace: ExtensionNamespace,
    /// Creation timestamp reported by the service.
    pub created: ExtensionTimestamp,
    /// Typed source data used by the invocation.
    pub source: Option<ExtensionSourceData>,
    /// Human-readable title rendered by the host.
    pub title: Option<ExtensionDisplayText>,
    /// Human-readable summary of the event or content.
    pub summary: Option<ExtensionDisplayText>,
    /// Extension payload elements carried alongside the standard protocol fields.
    pub payloads: Vec<ExtensionPayloadElementData>,
    /// Launch surfaces advertised by the extension.
    pub launches: Vec<ExtensionLaunchData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed XML tree belonging to an extension's declared payload namespace.
pub struct ExtensionPayloadElementData {
    /// XML namespace URI of the element or attribute.
    pub namespace: ExtensionNamespace,
    /// Unprefixed local XML name of the element or attribute.
    pub name: ExtensionXmlName,
    /// Attributes attached to this XML element.
    pub attributes: Vec<ExtensionPayloadAttributeData>,
    /// Human-readable text associated with this value.
    pub text: Option<String>,
    /// Child XML nodes in document order.
    pub children: Vec<ExtensionPayloadElementData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Name, namespace, and value of an extension payload attribute.
pub struct ExtensionPayloadAttributeData {
    /// Unprefixed local XML name of the element or attribute.
    pub name: ExtensionXmlName,
    /// Underlying validated value.
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Source link and message-body range associated with an enrichment.
pub struct ExtensionSourceData {
    /// Identifier used for stanza correlation.
    pub stanza_id: ExtensionTextId,
    /// Inclusive Unicode-character offset in the message body.
    pub body_start: Option<u32>,
    /// Exclusive Unicode-character offset in the message body.
    pub body_end: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Launch target and authorization metadata exposed by an enrichment.
pub struct ExtensionLaunchData {
    /// Identifier used to correlate this value with its protocol or domain record.
    pub id: ExtensionTextId,
    /// Extension identifier owning this value.
    pub plugin: ExtensionPluginId,
    /// Action requested by the client or exposed by the UI.
    pub action: ExtensionTextId,
    /// Ad-hoc command node selected for execution.
    pub command_node: ExtensionCommandNode,
    /// Human-readable label rendered by the host.
    pub label: ExtensionDisplayText,
    /// Authority and identity context for the current invocation.
    pub context: ExtensionLaunchContextData,
    /// Extension payload elements carried alongside the standard protocol fields.
    pub payloads: Vec<ExtensionPayloadElementData>,
    /// Timestamp after which this value is no longer valid.
    pub expires_at: Option<ExtensionTimestamp>,
    /// Opaque token used to authorize or correlate this operation.
    pub token: Option<ExtensionTextId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Room, message, and extension state context carried by a launch.
pub struct ExtensionLaunchContextData {
    /// Identifier of the Waddle community providing invocation context.
    pub waddle_id: ExtensionTextId,
    /// Bare JID of the room in scope.
    pub room: Option<ExtensionRoomJid>,
    /// Identifier of the source stanza associated with this value.
    pub source_stanza_id: Option<ExtensionTextId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Extension permissions recognized in incoming enrichment envelopes.
pub enum ExtensionCapabilityData {
    /// May annotate messages with extension-produced enrichments.
    MessageEnrich,
    /// May observe authorized room messages.
    MessageObserve,
    /// May list channels through the host.
    HostChannelsRead,
    /// May list spaces through the host.
    HostSpacesRead,
    /// May list room membership through the host.
    HostMembersRead,
    /// May read current presence through the host.
    HostPresenceRead,
    /// May query authorized message archives through the host.
    HostMamRead,
    /// May read the account roster through the host.
    HostRosterRead,
    /// May send messages through the host.
    HostMessageSend,
    /// May make outbound HTTP requests within the configured origin policy.
    OutboundHttpRequest,
    /// May expose ad-hoc commands.
    Commands,
    /// An authorized extension launch invocation.
    Launch,
    /// May publish permitted PubSub items.
    PubSubPublish,
    /// May return immutable artifact references.
    ArtifactReference,
    /// May return declarative UI for host rendering.
    UiDeclarative,
}

impl ExtensionCapabilityData {
    /// Interpret a protocol attribute, preserving the documented default for unknown or missing values.
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

    /// Borrow the stored text or canonical protocol token without allocating.
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
/// Non-blank identifier used by parsed extension payloads.
pub struct ExtensionTextId(String);

impl ExtensionTextId {
    /// Create `ExtensionTextId` from non-blank text, returning `None` for empty or whitespace-only input.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Non-blank user-visible text supplied by an extension.
pub struct ExtensionDisplayText(String);

impl ExtensionDisplayText {
    /// Create `ExtensionDisplayText` from non-blank text, returning `None` for empty or whitespace-only input.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Non-blank timestamp token supplied by an extension.
pub struct ExtensionTimestamp(String);

impl ExtensionTimestamp {
    /// Retain an RFC 3339 timestamp token, returning None if parsing fails.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        chrono::DateTime::parse_from_rfc3339(value.as_str())
            .ok()
            .map(|_| Self(value))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Ad-hoc command node constrained to the Waddle extension namespace.
pub struct ExtensionCommandNode(String);

impl ExtensionCommandNode {
    /// Accept the framework namespace or a colon-separated child command node; other nodes return None.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let valid = value == crate::messaging::namespaces::NS_WADDLE_EXTENSION
            || value
                .strip_prefix(crate::messaging::namespaces::NS_WADDLE_EXTENSION)
                .is_some_and(|suffix| suffix.starts_with(':'));
        valid.then_some(Self(value))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Validated bare room JID exposed by an extension payload.
pub struct ExtensionRoomJid(String);

impl ExtensionRoomJid {
    /// Retain a valid bare room JID, returning None when parsing fails.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        value.parse::<jid::BareJid>().ok()?;
        Some(Self(value))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Nonempty extension identifier containing lowercase ASCII letters, digits, or hyphens.
pub struct ExtensionPluginId(String);

impl ExtensionPluginId {
    /// Accept nonempty lowercase ASCII letters, digits, and hyphens; other input returns None.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
        valid.then_some(Self(value))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Non-blank namespace token retained from an incoming extension payload.
pub struct ExtensionNamespace(String);

impl ExtensionNamespace {
    /// Create `ExtensionNamespace` from non-blank text, returning `None` for empty or whitespace-only input.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Non-blank element-name token retained from an incoming extension payload.
pub struct ExtensionXmlName(String);

impl ExtensionXmlName {
    /// Create `ExtensionXmlName` from non-blank text, returning `None` for empty or whitespace-only input.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq)]
/// MUC hat URI and display title identifying an occupant's role.
pub struct PresenceHat {
    /// URI identifying the referenced resource.
    pub uri: String,
    /// Human-readable title rendered by the host.
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Persistent relationship between a user and a MUC room.
pub enum MucAffiliation {
    /// Persistent owner affiliation with full administrative authority.
    Owner,
    /// Persistent administrator affiliation.
    Admin,
    /// Persistent member affiliation permitting access to a restricted node or room.
    Member,
    /// Persistent ban preventing access to the node or room.
    Outcast,
    /// No affiliation, subscription, or optional protocol value is assigned.
    None,
}

impl MucAffiliation {
    /// Interpret a protocol attribute, preserving the documented default for unknown or missing values.
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

    /// Borrow the stored text or canonical protocol token without allocating.
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
/// Role held by the occupant for its current MUC session.
pub enum MucRole {
    /// Room-session role permitted to moderate other occupants.
    Moderator,
    /// Normal room-session role permitted to broadcast messages.
    Participant,
    /// Room-session role without voice in a moderated room.
    Visitor,
    /// No affiliation, subscription, or optional protocol value is assigned.
    None,
}

impl MucRole {
    /// Interpret a protocol attribute, preserving the documented default for unknown or missing values.
    pub fn from_attr(value: &str) -> Option<Self> {
        match value {
            "moderator" => Some(Self::Moderator),
            "participant" => Some(Self::Participant),
            "visitor" => Some(Self::Visitor),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// Borrow the stored text or canonical protocol token without allocating.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Moderator => "moderator",
            Self::Participant => "participant",
            Self::Visitor => "visitor",
            Self::None => "none",
        }
    }
}

/// XEP-0045 MUC status codes carried as `<status code='…'/>` children
/// of the `http://jabber.org/protocol/muc#user` payload. Only the codes
/// Waddle's client acts on are named; every other registered code is
/// preserved losslessly as `Other(u16)` so the typed value never drops
/// information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucStatus {
    /// 100 — the room is non-anonymous (occupants' real JIDs exposed).
    NonAnonymous,
    /// 110 — self-presence: this presence refers to the recipient. On
    /// join it is sent last and marks the end of the room roster
    /// (XEP-0045 §7.2.2).
    SelfPresence,
    /// Any other registered status code, preserved verbatim.
    Other(u16),
}

impl MucStatus {
    /// Recognize a standardized MUC status code while preserving unknown codes.
    pub fn from_code(code: u16) -> Self {
        match code {
            100 => Self::NonAnonymous,
            110 => Self::SelfPresence,
            other => Self::Other(other),
        }
    }

    /// Return the numeric MUC status code.
    pub fn code(self) -> u16 {
        match self {
            Self::NonAnonymous => 100,
            Self::SelfPresence => 110,
            Self::Other(code) => code,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
/// Parsed resource availability and MUC membership, role, and affiliation metadata.
pub struct InboundPresence {
    /// XMPP address of the sender.
    pub from: Option<String>,
    /// XMPP address of the recipient.
    pub to: Option<String>,
    /// XMPP presence stanza type.
    pub presence_type: Option<String>,
    /// Optional human-readable status text.
    pub status: Option<String>,
    /// Advertised availability state.
    pub show: Option<String>,
    /// Role labels advertised for this room occupant.
    pub hats: Vec<PresenceHat>,
    /// Persistent affiliation assigned to the room occupant.
    pub muc_affiliation: Option<MucAffiliation>,
    /// Session role assigned to the room occupant.
    pub muc_role: Option<MucRole>,
    /// Bare JID of the MUC service or room.
    pub muc_jid: Option<String>,
    /// XEP-0045 `<status code='…'/>` markers from the `muc#user`
    /// payload. Empty when the presence carries none. `110`
    /// (`MucStatus::SelfPresence`) identifies the recipient's own
    /// presence regardless of room anonymity or nick.
    pub muc_status: Vec<MucStatus>,
    /// Hash identifying the vCard-based avatar.
    pub vcard_avatar: Option<String>,
    /// XEP-0272 (Muji) presence advertisement —
    /// `<muji xmlns='urn:xmpp:jingle:muji:0'>…</muji>` — carried on
    /// a MUC occupant's presence to advertise that they have joined
    /// or left the room's group call. `None` when the presence
    /// carries no Muji element, which per XEP-0272 §Leaving means
    /// the occupant is NOT in the call. Drives the chat-side "N in
    /// call" indicator.
    pub muji: Option<MujiPresence>,
    /// Waddle raised-hand call presence state (#1029) —
    /// `<in-call xmlns='urn:waddle:in-call:0'><hand-raised/></in-call>`
    /// carried *alongside* `<muji/>` on a MUC occupant's presence. `true`
    /// when the occupant currently advertises a raised hand; `false` when
    /// the child is absent (lowered). Drives the chat-side raised-hand
    /// indicator in the Participants panel and on tiles.
    pub hand_raised: bool,
    /// Waddle mute call presence state (#1030) — a `<muted/>` marker on
    /// the same `<in-call>` carrier. `true` when the occupant advertises a
    /// muted microphone; `false` when the child is absent (unmuted).
    /// Drives the authoritative remote-mute indicator on tiles and in the
    /// roster.
    pub muted: bool,
    /// XEP-0319 Last User Interaction — the typed instant from
    /// `<idle xmlns='urn:xmpp:idle:1' since='…'/>`, or `None` when the presence
    /// carries no `<idle/>` (the contact is interacting now). Parsed once at the
    /// wire boundary (typed-payloads rule); re-serialized to an xs:dateTime only
    /// at the JS boundary so the chat side can render the idle age.
    pub idle_since: Option<chrono::DateTime<chrono::Utc>>,
    /// RFC 6120 §8.3 stanza error parsed from an error presence
    /// (`type="error"`), e.g. a rejected XEP-0045 room join. `None` for
    /// non-error presences. Parsed once at the wire boundary
    /// (typed-payloads rule) so consumers never re-parse the `<error/>`
    /// child.
    pub error: Option<crate::error::StanzaError>,
}

/// Parsed `<muji xmlns='urn:xmpp:jingle:muji:0'>` extension on a MUC
/// presence. Wire-typed because chat-side consumers want the
/// preparing-vs-active distinction without re-parsing the element
/// shape or the RTP content descriptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MujiPresence {
    /// True when the presence carried a `<preparing/>` child (XEP-0272
    /// §Joining two-phase flow). Indicator UIs typically don't pulse
    /// until contents are advertised — preparing alone is "about to
    /// join" rather than "in the call."
    pub preparing: bool,
    /// True when the presence advertised at least one `<content/>`
    /// child — the occupant is actively participating in the call.
    pub active: bool,
    /// True when at least one content description advertises audio.
    pub audio: bool,
    /// True when at least one content description advertises video.
    pub video: bool,
}

#[derive(Debug, Clone, PartialEq)]
/// Typed incoming message, presence, or room lifecycle event.
pub enum MessagingEvent {
    /// A parsed inbound message and its supported payloads.
    Message(Box<InboundMessage>),
    /// A parsed inbound resource or MUC presence update.
    Presence(Box<InboundPresence>),
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
    /// Resolve plain-text `@label` tokens in the body to XEP-0372 mention
    /// references at build time. Ranges already covered by `references`,
    /// the reply fallback and code markup are left alone. `None` resolves
    /// nothing.
    pub mention_directory: Option<MentionDirectory>,
    /// XEP-0446 / XEP-0447 shared files attached to the message.
    pub shared_files: Vec<SharedFile>,
    /// Waddle-private link-preview token request consumed server-side before
    /// fanout/archive and replaced by conformant XEP-0511 metadata.
    pub link_preview_token: Option<LinkPreviewToken>,
    /// XEP-0333 `<markable/>` request for displayed markers on this
    /// outbound chat/groupchat message.
    pub request_displayed_marker: bool,
    /// XEP-0045 §7.5: this message is a MUC private message addressed to
    /// an occupant JID. The builder appends an
    /// `<x xmlns='http://jabber.org/protocol/muc#user'/>` element so the
    /// user's other clients can classify the sent-carbon copy as a MUC PM
    /// without knowing the room.
    pub muc_pm: bool,
    /// XEP-0449 `<sticker/>` marker: this message sends a sticker. The
    /// attached `shared_files` entry carries the sticker image; the
    /// marker optionally references the source pack.
    pub sticker: Option<crate::stickers::StickerMarker>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Exactly 64 hexadecimal characters identifying a link-preview subscription.
pub struct LinkPreviewToken(String);

impl LinkPreviewToken {
    /// Validate and retain a 64-character ASCII hexadecimal preview token.
    ///
    /// # Errors
    ///
    /// Returns `InvalidLinkPreviewToken` unless the token contains exactly 64 ASCII hexadecimal characters.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidLinkPreviewToken> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(InvalidLinkPreviewToken);
        }
        Ok(Self(value))
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Failure to validate a 64-character hexadecimal link-preview token.
pub struct InvalidLinkPreviewToken;

impl fmt::Display for InvalidLinkPreviewToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("link preview token must not be empty")
    }
}

impl std::error::Error for InvalidLinkPreviewToken {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Formatting kind and Unicode-character range parsed from a rich message payload.
pub struct MarkupSpanData {
    /// Formatting or link interpretation for this body span.
    pub span_type: String,
    /// Inclusive character offset in the message body.
    pub start: u32,
    /// Exclusive character offset in the message body.
    pub end: u32,
    /// URI identifying the referenced resource.
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Message-body reference range, resource URI, and optional anchor.
pub struct ReferenceData {
    /// Reference type advertised by the sender.
    pub ref_type: String,
    /// URI identifying the referenced resource.
    pub uri: String,
    /// Inclusive character offset in the message body.
    pub begin: u32,
    /// Exclusive character offset in the message body.
    pub end: u32,
    /// XEP-0372 optional `anchor` attribute. Carries the original, unresolved
    /// display text (or arbitrary anchor URI) when offsets cannot be relied on.
    pub anchor: Option<String>,
}
