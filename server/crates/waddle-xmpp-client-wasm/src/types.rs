//! Serializable JavaScript views of messages, presence, discovery, and protocol results.

use super::{Deserialize, Serialize};
use serde::ser::SerializeStruct;
use waddle_xmpp_client::xep::safety_scores::{SafetyScore, SafetyScoresFastening};

/// Formatting or link span over a message body, using Unicode character offsets.
#[derive(Debug, Serialize)]
pub struct WaddleMarkupSpan {
    /// Markup token: `bold`, `italic`, `strikethrough`, `code`, `code_block`, `blockquote`, or `link`.
    pub span_type: String,
    /// Inclusive Unicode character offset where the range begins.
    pub start: usize,
    /// Exclusive Unicode character offset where the range ends.
    pub end: usize,
    /// Link target URI; absent for formatting spans without a link.
    pub uri: Option<String>,
}

/// Stable message identifier paired with the XMPP entity that assigned it.
#[derive(Debug, Serialize)]
pub struct WaddleStanzaId {
    /// Stable stanza ID assigned by the authority in `by`.
    pub id: String,
    /// JID of the entity that assigned the stable stanza identifier.
    pub by: String,
}

/// Call session metadata attached to the message that starts its thread.
#[derive(Debug, Serialize)]
pub struct WaddleCallThreadAnchor {
    /// Call context discriminator: `muc` for a room or `dm` for a direct call.
    pub kind: String,
    /// Call session identifier shared by signaling messages.
    pub sid: String,
    /// Media types or capabilities negotiated for the call.
    pub media: Vec<String>,
    /// JID of the party that initiated the call.
    pub initiator: String,
    /// RFC 3339 time at which the call began.
    pub started: String,
}

/// End time and duration attached to an existing call-thread anchor.
#[derive(Debug, Serialize)]
pub struct WaddleCallThreadEnded {
    /// Identifier of the message anchoring the call thread.
    pub anchor_id: String,
    /// RFC 3339 time at which the call ended.
    pub ended: String,
    /// ISO 8601 duration of the completed call.
    pub duration: String,
}

/// XEP-0422 `urn:waddle:safety-scores:1` fastening. The payload stays typed
/// on the Rust side; `Serialize` (the JS boundary) writes the wire shape
/// `{ target_origin_id, target_stanza_id, target_stanza_by,
/// source_revision_id, scores: { model_version, scores: [...] } }`.
#[derive(Debug)]
pub struct WaddleSafetyScoresFastening(
    /// Typed fastening serialized into the JavaScript message view.
    pub SafetyScoresFastening,
);

impl Serialize for WaddleSafetyScoresFastening {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("WaddleSafetyScoresFastening", 5)?;
        out.serialize_field("target_origin_id", self.0.target_origin_id.as_str())?;
        out.serialize_field("target_stanza_id", self.0.target_stanza_id.as_str())?;
        out.serialize_field("target_stanza_by", &self.0.target_stanza_id.by.to_string())?;
        out.serialize_field("source_revision_id", self.0.source_revision_id.as_str())?;
        out.serialize_field("scores", &SafetyScoresView(&self.0.scores))?;
        out.end()
    }
}

struct SafetyScoresView<'a>(&'a waddle_xmpp_client::xep::safety_scores::SafetyScores);

impl Serialize for SafetyScoresView<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let views: Vec<SafetyScoreView<'_>> = self.0.scores.iter().map(SafetyScoreView).collect();
        let mut out = serializer.serialize_struct("SafetyScores", 2)?;
        out.serialize_field("model_version", self.0.model_version.as_str())?;
        out.serialize_field("scores", &views)?;
        out.end()
    }
}

struct SafetyScoreView<'a>(&'a SafetyScore);

impl Serialize for SafetyScoreView<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("SafetyScore", 3)?;
        out.serialize_field("category", self.0.category.as_wire())?;
        out.serialize_field("probability", &self.0.probability.value())?;
        out.serialize_field("taxonomy_version", self.0.taxonomy_version.as_str())?;
        out.end()
    }
}

/// Ephemeral call signals projected into JavaScript tagged objects.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum WaddleInCallSignal {
    /// An emoji reaction associated with an active call session.
    Reaction {
        /// Identifier of the call session receiving the reaction.
        sid: String,
        /// Unicode emoji selected by the reacting participant.
        emoji: String,
    },
}

/// Parsed live message with optional XMPP extensions exposed to JavaScript.
#[derive(Debug, Serialize)]
pub struct WaddleMessage {
    /// Message stanza ID supplied by the sender, if present.
    pub id: Option<String>,
    /// Sender JID, when present on the stanza.
    pub from: Option<String>,
    /// Recipient JID, when present on the stanza.
    pub to: Option<String>,
    /// Plain-text message body, if supplied.
    pub body: Option<String>,
    /// Optional message subject.
    pub subject: Option<String>,
    /// XMPP message type serialized as its wire token.
    pub message_type: String,
    /// RFC 3339 delayed-delivery or archive timestamp, when available.
    pub timestamp: Option<String>,
    /// Stable stanza identifier selected from the message's assigned IDs.
    pub stanza_id: Option<String>,
    /// JID of the authority that assigned the selected stanza ID.
    pub stanza_id_by: Option<String>,
    /// All stable stanza IDs and their assigning authorities.
    pub stanza_ids: Vec<WaddleStanzaId>,
    /// Sender-assigned original message identifier.
    pub origin_id: Option<String>,
    /// Identifier of the message corrected by this message.
    pub replaces_id: Option<String>,
    /// Identifier of the message targeted by a retraction request.
    pub retracts_id: Option<String>,
    /// Identifier retained for an archived retraction tombstone.
    pub retraction_id: Option<String>,
    /// Whether the archived message is a retraction tombstone.
    pub is_retracted: bool,
    /// Identifier of the message targeted by moderation.
    pub moderation_target_id: Option<String>,
    /// JID of the moderator who removed the message.
    pub moderated_by: Option<String>,
    /// Optional explanation supplied by the moderator.
    pub moderation_reason: Option<String>,
    /// Chat-state wire token, when a state notification is present.
    pub chat_state: Option<String>,
    /// Whether the sender requested a displayed marker for this message.
    pub displayed_marker_requested: bool,
    /// Identifier acknowledged by an included displayed marker.
    pub displayed_marker_id: Option<String>,
    /// Identifier of the message whose reaction set is being updated.
    pub reaction_target_id: Option<String>,
    /// Complete emoji reaction set supplied by the reacting sender.
    pub reaction_emojis: Vec<String>,
    /// Ephemeral signal for an active call, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_call: Option<WaddleInCallSignal>,
    /// Whether the message is a groupchat message.
    pub is_muc: bool,
    /// Whether the message is a private message between room occupants.
    pub muc_pm: bool,
    /// Identifier of the conversation thread containing the message.
    pub thread: Option<String>,
    /// Optional parent thread identifier.
    pub parent_thread_id: Option<String>,
    /// Identifier of the message being replied to.
    pub reply_to_id: Option<String>,
    /// JID of the author of the replied-to message.
    pub reply_to_sender: Option<String>,
    /// Inclusive character offset of quoted reply fallback text.
    pub reply_fallback_start: Option<u32>,
    /// Exclusive character offset of quoted reply fallback text.
    pub reply_fallback_end: Option<u32>,
    /// Formatting and link annotations over the body text.
    pub markup_spans: Vec<WaddleMarkupSpan>,
    /// Broadcast mention token detected in the body, when present.
    pub broadcast_mention: Option<String>,
    /// Mention target URIs extracted from the message references.
    pub mention_uris: Vec<String>,
    /// Typed body references, including explicit mention targets.
    pub references: Vec<WaddleReference>,
    /// Forum post discriminator supplied by the message extension.
    pub forum_post_kind: Option<String>,
    /// Title of a forum root post, when present.
    pub forum_title: Option<String>,
    /// Thread title carried by the forum message extension.
    pub forum_thread_title: Option<String>,
    /// Whether the attachment is marked as a sticker.
    pub is_sticker: bool,
    /// urn:waddle:call-thread:0 room call-thread anchor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_thread: Option<WaddleCallThreadAnchor>,
    /// End marker for the call anchored by a thread message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_thread_ended: Option<WaddleCallThreadEnded>,
    /// Revision-specific safety classification attached to a message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_scores: Option<WaddleSafetyScoresFastening>,
    /// Shared attachment metadata extracted from the message.
    pub shared_files: Vec<WaddleSharedFile>,
    /// Link previews attached to the message.
    pub link_previews: Vec<WaddleLinkPreview>,
    /// urn:waddle:pin:0 pin/unpin event surfaced from a system message
    /// (#414). `None` when the message carries no `<pin-event/>`.
    pub pin_event: Option<WaddlePinEvent>,
    /// Parsed extension enrichments, when present.
    pub extension_envelope: Option<WaddleExtensionEnvelope>,
    /// Whether the body is fallback text for an extension payload.
    pub extension_body_fallback: bool,
    /// PubSub item notifications carried by this message.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pubsub_events: Vec<WaddlePubsubEvent>,
    /// Conversation summary delivered as an inbox push update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox_push: Option<WaddleInboxConversation>,
    /// XEP-0280: present when this message was unwrapped from a verified
    /// carbon envelope (#1243). Exactly one of `sent`/`received` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carbon: Option<WaddleCarbonMarker>,
}

/// XEP-0280 carbon direction marker surfaced to JS as
/// `{ sent: boolean, received: boolean }`.
#[derive(Debug, Serialize)]
pub struct WaddleCarbonMarker {
    /// Whether this carbon copies a message sent by another local resource.
    pub sent: bool,
    /// Whether this carbon copies a message received by another local resource.
    pub received: bool,
}

/// Published or retracted items from one inbound PubSub node notification.
#[derive(Debug, Serialize)]
pub struct WaddlePubsubEvent {
    /// JID of the PubSub service or PEP account that sent the event.
    pub from: Option<String>,
    /// PubSub node that owns the items.
    pub node: String,
    /// Items supplied by this PubSub notification.
    pub items: Vec<WaddlePubsubEventItem>,
}

/// One item or retraction within a PubSub event.
#[derive(Debug, Serialize)]
pub struct WaddlePubsubEventItem {
    /// PubSub item ID used to correlate a publication or retraction.
    pub id: Option<String>,
    /// Whether this item ID was retracted rather than published.
    #[serde(skip_serializing_if = "is_false")]
    pub retracted: bool,
    /// Parsed or preserved payload associated with the item.
    pub payload: WaddlePubsubEventPayload,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Recognized attachment summary or preserved application XML in a PubSub item.
#[derive(Debug, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
#[non_exhaustive]
pub enum WaddlePubsubEventPayload {
    /// Aggregate reactions and noticed count attached to a message.
    AttachmentSummary {
        /// Reaction totals grouped by emoji.
        reactions: Vec<WaddlePubsubReactionSummary>,
        /// Number of users who noticed the message, when supplied.
        noticed_count: Option<u32>,
    },
    /// Payload XML not interpreted by this binding.
    Opaque {
        /// Serialized XML of the application payload element.
        xml: String,
    },
    /// Item with no payload, including a retraction notification.
    Empty,
}

/// Aggregate reaction count and the JIDs contributing that reaction.
#[derive(Debug, Serialize)]
pub struct WaddlePubsubReactionSummary {
    /// Unicode emoji representing the reaction.
    pub emoji: String,
    /// Number of reactions contributing to this emoji summary.
    pub count: u32,
    /// JIDs of the users contributing this reaction.
    pub reactors: Vec<String>,
}

/// Metadata and renderable media supplied for a link in a message.
#[derive(Debug, Serialize)]
pub struct WaddleLinkPreview {
    /// Original link URL as it appeared in the message.
    pub original_url: String,
    /// Canonicalized link URL supplied by the preview service.
    pub normalized_url: Option<String>,
    /// Human-readable title supplied by the producer.
    pub title: Option<String>,
    /// Optional descriptive text for display.
    pub description: Option<String>,
    /// Optional image resource associated with the preview.
    pub image: Option<WaddleLinkPreviewImage>,
    /// Optional direct video resource associated with the preview.
    pub video: Option<WaddleLinkPreviewVideo>,
    /// Optional embeddable player resource.
    pub player_embed: Option<WaddleLinkPreviewPlayer>,
    /// Whether preview media could not be made available for rendering.
    pub remote_media_unavailable: bool,
}

/// Direct video resource advertised by a link preview.
#[derive(Debug, Serialize)]
pub struct WaddleLinkPreviewVideo {
    /// URL of the video resource offered by the link preview.
    pub url: String,
    /// MIME media type of the resource.
    pub media_type: String,
}

/// Preview image resource and its optional display metadata.
#[derive(Debug, Serialize)]
pub struct WaddleLinkPreviewImage {
    /// URL of the image resource offered by the link preview.
    pub url: String,
    /// MIME media type of the resource.
    pub media_type: String,
    /// Width in pixels, when supplied.
    pub width: Option<u32>,
    /// Height in pixels, when supplied.
    pub height: Option<u32>,
    /// Alternative text describing the image.
    pub alt: Option<String>,
}

/// Embeddable player URL and its optional dimensions.
#[derive(Debug, Serialize)]
pub struct WaddleLinkPreviewPlayer {
    /// URL of the embeddable player offered by the link preview.
    pub url: String,
    /// Width in pixels, when supplied.
    pub width: Option<u32>,
    /// Height in pixels, when supplied.
    pub height: Option<u32>,
}

/// Versioned collection of extension enrichments attached to a message.
#[derive(Debug, Serialize)]
pub struct WaddleExtensionEnvelope {
    /// Version of the extension envelope format.
    pub version: u32,
    /// Extension contributions carried by the envelope.
    pub enrichments: Vec<WaddleExtensionEnrichment>,
}

/// Extension-produced metadata, payloads, and invocable actions for a message.
#[derive(Debug, Serialize)]
pub struct WaddleExtensionEnrichment {
    /// Identifier of this enrichment within the extension envelope.
    pub id: String,
    /// Identifier of the extension plugin producing this value.
    pub plugin: String,
    /// Plugin capability responsible for the enrichment.
    pub capability: String,
    /// XML namespace identifying this extension's payload format.
    pub payload_namespace: String,
    /// RFC 3339 creation time of the enrichment.
    pub created: String,
    /// Message and body range that this enrichment describes.
    pub source: Option<WaddleExtensionSource>,
    /// Human-readable title supplied by the producer.
    pub title: Option<String>,
    /// Short display summary of the enrichment.
    pub summary: Option<String>,
    /// Application XML payloads projected into JavaScript objects.
    pub payloads: Vec<WaddleExtensionPayloadElement>,
    /// Extension actions offered by the enrichment.
    pub launches: Vec<WaddleExtensionLaunch>,
}

/// Source message and optional body range enriched by an extension.
#[derive(Debug, Serialize)]
pub struct WaddleExtensionSource {
    /// Stable stanza identifier selected from the message's assigned IDs.
    pub stanza_id: String,
    /// Inclusive character offset of the enriched body range.
    pub body_start: Option<u32>,
    /// Exclusive character offset of the enriched body range.
    pub body_end: Option<u32>,
}

/// Authorized extension action and the context needed to invoke it.
#[derive(Debug, Serialize)]
pub struct WaddleExtensionLaunch {
    /// Identifier used to select this authorized launch.
    pub id: String,
    /// Identifier of the extension plugin producing this value.
    pub plugin: String,
    /// Identifier of the extension action to execute.
    pub action: String,
    /// XMPP ad-hoc command node used to invoke the action.
    pub command_node: String,
    /// Human-readable display label.
    pub label: String,
    /// Community and source-message context for invocation.
    pub context: WaddleExtensionLaunchContext,
    /// Application XML payloads projected into JavaScript objects.
    pub payloads: Vec<WaddleExtensionPayloadElement>,
    /// RFC 3339 expiry time of the action authorization.
    pub expires_at: Option<String>,
    /// Authorization token issued for this invocation.
    pub token: Option<String>,
}

/// Community, room, and source-message context for an extension launch.
#[derive(Debug, Serialize)]
pub struct WaddleExtensionLaunchContext {
    /// Community identifier associated with the action.
    pub waddle_id: String,
    /// Room identifier associated with this context.
    pub room: Option<String>,
    /// Stable stanza identifier of the source message, when available.
    pub source_stanza_id: Option<String>,
}

/// Recursive XML element projected into a JavaScript object at the binding boundary.
#[derive(Debug, Serialize)]
pub struct WaddleExtensionPayloadElement {
    /// XML namespace URI of this payload element.
    pub namespace: String,
    /// XML local name of this payload element.
    pub name: String,
    /// XML attributes on this payload element.
    pub attributes: Vec<WaddleExtensionPayloadAttribute>,
    /// Text content of this payload element, absent when empty.
    pub text: Option<String>,
    /// Nested payload elements in document order.
    pub children: Vec<WaddleExtensionPayloadElement>,
}

/// Name and value of an extension payload's XML attribute.
#[derive(Debug, Serialize)]
pub struct WaddleExtensionPayloadAttribute {
    /// XML name of this payload attribute.
    pub name: String,
    /// Decoded XML attribute value.
    pub value: String,
}

/// Pin/unpin event surfaced from an inbound system message (#414).
#[derive(Debug, Serialize)]
pub struct WaddlePinEvent {
    /// `pinned` or `unpinned`.
    pub action: String,
    /// XEP-0359 stanza-id of the targeted message.
    pub target_stanza_id: String,
    /// Bare JID of the user who applied the change.
    pub by: String,
    /// `Some("retracted")` when the unpin was triggered by an XEP-0424
    /// retraction cascade.
    pub reason: Option<String>,
    /// Frozen preview, present only on `pinned` events.
    pub preview: Option<WaddlePinPreview>,
}

/// One pinned-message entry returned by `fetch_room_pins` (#414).
#[derive(Debug, Serialize)]
pub struct WaddlePinEntry {
    /// XEP-0359 stanza-id of the pinned message.
    pub target_stanza_id: String,
    /// Bare JID of the user who pinned the message.
    pub pinner_jid: String,
    /// When the pin was applied (rfc3339).
    pub pinned_at: String,
    /// Frozen preview snapshot.
    pub preview: WaddlePinPreview,
}

/// Frozen preview of a pinned message (#414).
#[derive(Debug, Serialize)]
pub struct WaddlePinPreview {
    /// Bare JID of the message author at pin time.
    pub author_jid: String,
    /// Author's MUC nick at pin time, if known.
    pub author_nick: Option<String>,
    /// Truncated body text (≤280 chars).
    pub text: String,
    /// Original message timestamp (rfc3339).
    pub message_timestamp: String,
}

/// Archived message with MAM identity and parsed message extensions.
#[derive(Debug, Serialize)]
pub struct WaddleArchivedMessage {
    /// Archive-assigned identifier of this result.
    pub mam_id: String,
    /// MAM query identifier used to correlate this archived result.
    pub query_id: Option<String>,
    /// Original message stanza ID supplied by the sender, if present.
    pub id: Option<String>,
    /// Stable stanza identifier selected from the message's assigned IDs.
    pub stanza_id: Option<String>,
    /// JID of the authority that assigned the selected stanza ID.
    pub stanza_id_by: Option<String>,
    /// All stable stanza IDs and their assigning authorities.
    pub stanza_ids: Vec<WaddleStanzaId>,
    /// Sender-assigned original message identifier.
    pub origin_id: Option<String>,
    /// RFC 3339 delayed-delivery or archive timestamp, when available.
    pub timestamp: Option<String>,
    /// Sender JID, when present on the stanza.
    pub from: Option<String>,
    /// Recipient JID, when present on the stanza.
    pub to: Option<String>,
    /// XMPP message type serialized as its wire token.
    pub message_type: String,
    /// Whether the message is a private message between room occupants.
    pub muc_pm: bool,
    /// Plain-text message body, if supplied.
    pub body: Option<String>,
    /// Optional message subject.
    pub subject: Option<String>,
    /// Identifier of the message corrected by this message.
    pub replaces_id: Option<String>,
    /// Identifier of the message targeted by a retraction request.
    pub retracts_id: Option<String>,
    /// Identifier retained for an archived retraction tombstone.
    pub retraction_id: Option<String>,
    /// Whether the archived message is a retraction tombstone.
    pub is_retracted: bool,
    /// Identifier of the message targeted by moderation.
    pub moderation_target_id: Option<String>,
    /// JID of the moderator who removed the message.
    pub moderated_by: Option<String>,
    /// Optional explanation supplied by the moderator.
    pub moderation_reason: Option<String>,
    /// Identifier of the message whose reaction set is being updated.
    pub reaction_target_id: Option<String>,
    /// Complete emoji reaction set supplied by the reacting sender.
    pub reaction_emojis: Vec<String>,
    /// Identifier of the conversation thread containing the message.
    pub thread: Option<String>,
    /// Optional parent thread identifier.
    pub parent_thread_id: Option<String>,
    /// Identifier of the message being replied to.
    pub reply_to_id: Option<String>,
    /// JID of the author of the replied-to message.
    pub reply_to_sender: Option<String>,
    /// Inclusive character offset of quoted reply fallback text.
    pub reply_fallback_start: Option<u32>,
    /// Exclusive character offset of quoted reply fallback text.
    pub reply_fallback_end: Option<u32>,
    /// Formatting and link annotations over the body text.
    pub markup_spans: Vec<WaddleMarkupSpan>,
    /// Broadcast mention token detected in the body, when present.
    pub broadcast_mention: Option<String>,
    /// Mention target URIs extracted from the message references.
    pub mention_uris: Vec<String>,
    /// Typed body references, including explicit mention targets.
    pub references: Vec<WaddleReference>,
    /// Forum post discriminator supplied by the message extension.
    pub forum_post_kind: Option<String>,
    /// Title of a forum root post, when present.
    pub forum_title: Option<String>,
    /// Thread title carried by the forum message extension.
    pub forum_thread_title: Option<String>,
    /// Whether the attachment is marked as a sticker.
    pub is_sticker: bool,
    /// Real author JID disclosed by the room's archive, when available.
    pub author_real_jid: Option<String>,
    /// urn:waddle:call-thread:0 room call-thread anchor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_thread: Option<WaddleCallThreadAnchor>,
    /// End marker for the call anchored by a thread message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_thread_ended: Option<WaddleCallThreadEnded>,
    /// Revision-specific safety classification attached to a message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_scores: Option<WaddleSafetyScoresFastening>,
    /// Shared attachment metadata extracted from the message.
    pub shared_files: Vec<WaddleSharedFile>,
    /// Link previews attached to the message.
    pub link_previews: Vec<WaddleLinkPreview>,
    /// Parsed extension enrichments, when present.
    pub extension_envelope: Option<WaddleExtensionEnvelope>,
    /// Whether the body is fallback text for an extension payload.
    pub extension_body_fallback: bool,
    /// Call signaling event parsed from the archived message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_event: Option<WaddleCallEvent>,
}

/// Archive page and its result-set pagination identifiers.
#[derive(Debug, Serialize)]
pub struct WaddleMamPage {
    /// Archived messages returned in this page.
    pub messages: Vec<WaddleArchivedMessage>,
    /// Result-set identifier of the first returned archive item.
    pub first_id: Option<String>,
    /// Result-set identifier of the last returned archive item.
    pub last_id: Option<String>,
    /// Whether the server reports no more results in the queried direction.
    pub is_complete: bool,
}

/// Role label and identifying URI advertised on an occupant's presence.
#[derive(Debug, Serialize)]
pub struct WaddlePresenceHat {
    /// URI identifying the advertised role.
    pub uri: String,
    /// Human-readable title supplied by the producer.
    pub title: String,
}

/// Audio and video capabilities carried in a call offer or answer.
#[derive(Debug, Serialize)]
pub struct WaddleCallMedia {
    /// Whether audio media is included in the call.
    pub audio: bool,
    /// Whether video media is included in the call.
    pub video: bool,
}

/// Server-issued connection details and credentials for joining a call.
#[derive(Debug, Serialize)]
pub struct WaddleLiveKitJoin {
    /// WebSocket endpoint of the media service.
    pub url: String,
    /// Name of the media room to join.
    pub room: String,
    /// Identity assigned to this participant by the call service.
    pub identity: String,
    /// Bearer token authorizing this participant to join the media room.
    pub token: String,
}

/// JS-facing XEP-0215 external service (one `<service/>` entry). The chat maps
/// an array of these to `RTCIceServer[]` for LiveKit's `rtcConfig`. Wire-form
/// `serviceType`/`transport` strings come straight from the typed core value's
/// `as_str`, so the discriminator has a single source of truth.
#[derive(Debug, Serialize)]
pub struct WaddleExternalService {
    /// External service kind, such as STUN or TURN.
    #[serde(rename = "serviceType")]
    pub service_type: &'static str,
    /// Host name or address of the external service.
    pub host: String,
    /// Service port, if explicitly advertised.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Transport protocol advertised for the external service.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<&'static str>,
    /// Username supplied by the service or user directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Credential used to authenticate to the external service.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// RFC 3339 expiry time of the service credentials.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    /// Whether access to the service is restricted to authorized users.
    pub restricted: bool,
}

impl From<waddle_xmpp_client::xep::xep0215::ExternalService> for WaddleExternalService {
    fn from(service: waddle_xmpp_client::xep::xep0215::ExternalService) -> Self {
        Self {
            service_type: service.service_type.as_str(),
            host: service.host,
            port: service.port,
            transport: service.transport.map(|t| t.as_str()),
            username: service.username,
            password: service.password,
            // Serialize the typed expiry back to an RFC 3339 string for JSON
            // transit (the JS-facing boundary).
            expires: service.expires.map(|expires| expires.to_rfc3339()),
            restricted: service.restricted,
        }
    }
}

/// JS-facing call event. The `kind` discriminator drives a switch
/// on the chat side: `propose|ringing|proceed|reject|retract|finish|
/// session-initiate|session-accept|session-terminate`.
///
/// For `propose`, `session-initiate`, and `session-accept`, the
/// `media` field is populated. For `session-initiate` and
/// `session-accept`, the `join` field is populated with the
/// server-issued LiveKit credentials. For `session-terminate`, the
/// `reason` field carries the Jingle reason element name (e.g.
/// `success`, `busy`, `decline`).
#[derive(Debug, Serialize)]
pub struct WaddleCallEvent {
    /// JID of the peer that sent the call signaling event.
    pub from: String,
    /// Recipient JID supplied on the signaling stanza, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Call session identifier shared by signaling messages.
    pub sid: String,
    /// Call-event wire token; see the type documentation for supported values.
    pub kind: &'static str,
    /// Media types or capabilities negotiated for the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<WaddleCallMedia>,
    /// Server-issued credentials provided during Jingle session setup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join: Option<WaddleLiveKitJoin>,
    /// Protocol reason associated with the event, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether this signaling event resolves a simultaneous call proposal.
    #[serde(rename = "tieBreak", skip_serializing_if = "Option::is_none")]
    pub tie_break: Option<bool>,
    /// Session identifier of the replacement call after migration.
    #[serde(rename = "migratedTo", skip_serializing_if = "Option::is_none")]
    pub migrated_to: Option<String>,
}

/// Parsed availability, occupant information, and optional call presence state.
#[derive(Debug, Serialize)]
pub struct WaddlePresence {
    /// Contact JID or full room occupant JID that sent the presence.
    pub from: Option<String>,
    /// Recipient JID addressed by the presence, when supplied.
    pub to: Option<String>,
    /// XMPP presence type serialized as its wire token.
    pub presence_type: String,
    /// Availability token such as away, chat, dnd, or xa.
    pub show: Option<String>,
    /// Human-readable availability message.
    pub status: Option<String>,
    /// Role labels advertised for this occupant.
    pub hats: Vec<WaddlePresenceHat>,
    /// Persistent room affiliation of the occupant.
    pub muc_affiliation: Option<String>,
    /// Current room role of the occupant.
    pub muc_role: Option<String>,
    /// Real JID disclosed for the occupant by the room.
    pub muc_jid: Option<String>,
    /// XEP-0045 MUC `<status code='…'/>` markers as raw numbers (e.g.
    /// `110` self-presence, `100` non-anonymous). Omitted when the
    /// presence carries none, so the chat side sees `undefined`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub muc_status_codes: Vec<u16>,
    /// Legacy vCard avatar hash advertised on presence, including an empty hash for removal.
    pub vcard_avatar: Option<String>,
    /// XEP-0272 Muji `<muji xmlns='urn:xmpp:jingle:muji:0'/>`
    /// extension on a MUC occupant's presence, surfaced as
    /// `{ preparing, active, audio, video }` for the chat-side
    /// participant-tracking store. `None` per XEP-0272 §Leaving
    /// means the occupant has left the call.
    pub muji: Option<WaddleMujiPresence>,
    /// Waddle raised-hand call presence state (#1029). `true` when the
    /// occupant's presence carries
    /// `<in-call xmlns='urn:waddle:in-call:0'><hand-raised/></in-call>`
    /// alongside `<muji/>`. Defaults to `false` (lowered) so the chat
    /// side can build a per-participant raised-hand map.
    pub hand_raised: bool,
    /// Waddle mute call presence state (#1030). `true` when the
    /// occupant's presence carries a `<muted/>` marker on the same
    /// `<in-call>` carrier. Defaults to `false` (unmuted) so the chat
    /// side can build a per-participant mute map for tiles and the roster.
    pub muted: bool,
    /// XEP-0319 Last User Interaction — the raw `since` xs:dateTime from the
    /// contact's `<idle/>`, or `None` when the presence carries none. The chat
    /// side turns it into the rendered idle age on an away presence.
    pub idle_since: Option<String>,
    /// RFC 6120 §8.3 defined condition of an error presence (e.g. a
    /// rejected XEP-0045 room join), omitted for non-error presences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_condition: Option<String>,
    /// RFC 6120 §8.3.2 `type` attribute of the `<error/>` element
    /// ("auth" | "cancel" | "continue" | "modify" | "wait" | "unknown").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// Server-provided `<text/>` of the error, omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
}

/// Group-call preparation, participation, and media state from occupant presence.
#[derive(Debug, Serialize)]
pub struct WaddleMujiPresence {
    /// Whether the participant is preparing to join a group call.
    pub preparing: bool,
    /// Whether the participant is active in the group call.
    pub active: bool,
    /// Whether audio media is included in the call.
    pub audio: bool,
    /// Whether video media is included in the call.
    pub video: bool,
}

/// Fetched avatar bytes and the metadata identifying their owner and format.
#[derive(Debug, Serialize)]
pub struct WaddleAvatar {
    /// Bare JID of the account that published the avatar.
    pub jid: String,
    /// Advertised avatar item identifier associated with these bytes.
    pub id: String,
    /// MIME type describing the avatar bytes.
    pub mime_type: String,
    /// Decoded avatar image bytes.
    pub data: Vec<u8>,
}

/// Outcome of a §4.2-aware avatar fetch: `id` is the advertised item id;
/// `avatar` is absent when that id was already known to the caller.
#[derive(Debug, Serialize)]
pub struct WaddleAvatarFetch {
    /// Avatar item ID advertised by the peer.
    pub id: String,
    /// Fetched image, absent when the caller already knows the advertised ID.
    pub avatar: Option<WaddleAvatar>,
}

/// XEP-0084 metadata transition delivered to the browser callback surface.
#[derive(Debug, Serialize)]
pub struct WaddleAvatarChanged {
    /// Bare JID of the account whose avatar metadata changed.
    pub jid: String,
    /// New avatar item ID, or no ID when the avatar was removed.
    pub avatar_id: Option<String>,
}

/// Discovered extension route and its backing PubSub state node.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRoute {
    /// XMPP service address that processes this request or route.
    pub service_jid: String,
    /// Identifier of the extension plugin owning the route or action.
    pub plugin_id: String,
    /// Identifier of the discovered extension route.
    pub route_id: String,
    /// Human-readable display label.
    pub label: String,
    /// Context scope advertised by the extension route.
    pub scope: String,
    /// UI surface on which this route is intended to appear.
    pub surface: String,
    /// Backing PubSub node, potentially containing a `{room}` placeholder.
    pub state_node: String,
    /// XML namespace identifying this extension's payload format.
    pub payload_namespace: String,
}

/// Renderable extension item with optional display metadata and actions.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRouteItem {
    /// PubSub item identifier, when supplied by the route.
    pub id: Option<String>,
    /// Human-readable title supplied by the producer.
    pub title: Option<String>,
    /// Optional secondary display text.
    pub subtitle: Option<String>,
    /// Optional navigation link associated with the item.
    pub link: Option<WaddleExtensionRouteLink>,
    /// Optional descriptive text for display.
    pub description: Option<String>,
    /// Named display fields supplied by the route item.
    pub fields: Vec<WaddleExtensionRouteItemField>,
    /// Selectable options supplied by the route item.
    pub options: Vec<WaddleExtensionRouteItemOption>,
    /// Invocable actions supplied by the route item.
    pub actions: Vec<WaddleExtensionRouteItemAction>,
}

/// Navigation target supplied by an extension route item.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleExtensionRouteLink {
    /// Navigation URL supplied by the route item.
    pub href: String,
}

/// Named textual field displayed on an extension route item.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleExtensionRouteItemField {
    /// Machine-readable field name supplied by the extension.
    pub name: String,
    /// Human-readable display label.
    pub label: Option<String>,
    /// Textual content displayed for the named field.
    pub value: String,
}

/// Selectable option offered by an extension route item.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleExtensionRouteItemOption {
    /// Machine-readable identifier of the selectable option.
    pub id: String,
    /// Human-readable display label.
    pub label: String,
}

/// Invocable action exposed by an extension route item.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRouteItemAction {
    /// Signed launch parameters for the action.
    pub launch: WaddleExtensionRouteItemLaunch,
}

/// Signed launch parameters for a discovered extension route action.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRouteItemLaunch {
    /// Identifier of this signed route-item launch.
    pub id: String,
    /// Identifier of the extension plugin owning the route or action.
    pub plugin_id: String,
    /// Identifier of the extension action to invoke.
    pub action_id: String,
    /// XMPP ad-hoc command node used to invoke the action.
    pub command_node: String,
    /// Human-readable display label.
    pub label: String,
    /// Signed authorization token required when invoking the action.
    pub launch_token: String,
    /// RFC 3339 expiry time of the action authorization.
    pub expires_at: String,
    /// Community identifier associated with the action.
    pub waddle_id: String,
    /// Room address associated with the request or action.
    pub room_jid: Option<String>,
    /// Stable stanza identifier of the source message, when available.
    pub source_stanza_id: Option<String>,
}

/// File-sharing metadata and optional encryption envelope for an attachment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaddleSharedFile {
    /// Download URL for plaintext file bytes or, when `encrypted` is present, ciphertext.
    pub url: String,
    /// Original filename suggested by the sender.
    pub name: Option<String>,
    /// MIME media type of the resource.
    pub media_type: Option<String>,
    /// File size in bytes, when supplied.
    pub size: Option<u64>,
    /// Width in pixels, when supplied.
    pub width: Option<u32>,
    /// Height in pixels, when supplied.
    pub height: Option<u32>,
    /// XEP-0446 / XEP-0300 plaintext content hashes inside `<file/>`.
    /// XEP-0448 requires at least one for encrypted sends; distinct from
    /// the ciphertext hashes nested inside `encrypted`.
    #[serde(default)]
    pub hashes: Vec<WaddleEncryptedFileHash>,
    /// File display disposition, such as attachment or inline.
    pub disposition: String,
    /// XEP-0448 envelope when the bytes at `url` are ciphertext rather than
    /// the plaintext file. Recipients MUST use these values to decrypt before
    /// rendering. Absent for plaintext shares and on platforms that do not
    /// produce encrypted attachments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<WaddleEncryptedFile>,
}

/// XEP-0448 envelope (cipher / key / iv / hashes / sources) bridged across
/// the WASM boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaddleEncryptedFile {
    /// Cipher URN, e.g. `urn:xmpp:ciphers:aes-256-gcm-nopadding:0`.
    pub cipher: String,
    /// Base64-encoded symmetric key.
    pub key_b64: String,
    /// Base64-encoded initialization vector / nonce.
    pub iv_b64: String,
    /// Content digests of the encrypted bytes.
    #[serde(default)]
    pub hashes: Vec<WaddleEncryptedFileHash>,
    /// Source URLs the ciphertext can be fetched from. Always non-empty.
    pub sources: Vec<String>,
}

/// XEP-0300 hash entry: nested under a `WaddleEncryptedFile` (ciphertext
/// digest) or inside a `WaddleSharedFile`'s file metadata (plaintext digest).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaddleEncryptedFileHash {
    /// XEP-0300 hash algorithm name.
    pub algo: String,
    /// Base64-encoded digest bytes.
    pub value_b64: String,
}

/// HTTP request header required by an allocated upload slot.
#[derive(Debug, Serialize)]
pub struct WaddleUploadHeader {
    /// HTTP header name to include in the upload request.
    pub name: String,
    /// HTTP header value to include in the upload request.
    pub value: String,
}

/// HTTP upload destination, download URL, and required PUT headers.
#[derive(Debug, Serialize)]
pub struct WaddleUploadSlot {
    /// URL to which the attachment bytes must be uploaded with HTTP PUT.
    pub put_url: String,
    /// URL from which the uploaded attachment can be downloaded.
    pub get_url: String,
    /// Headers that must accompany the HTTP PUT request.
    pub put_headers: Vec<WaddleUploadHeader>,
}

/// Optional software identification returned by a server version query.
#[derive(Debug, Serialize)]
pub struct WaddleServerVersion {
    /// Software name reported by the server.
    pub name: Option<String>,
    /// Software version reported by the server.
    pub version: Option<String>,
    /// Operating system reported by the server, when disclosed.
    pub os: Option<String>,
}

/// Result of `fetch_vapid_public_key` — the Push Service's currently-
/// active VAPID public key + kid as advertised via the XEP-0128 disco
/// extension form `urn:waddle:push:vapid:0`. The chat hands
/// `publicKey` directly to `pushManager.subscribe({ applicationServerKey })`
/// after a base64url-no-pad → `Uint8Array` decode, and stores `kid` so
/// rotation is detectable on subsequent reconnects.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleVapidAdvertisement {
    /// Base64url-encoded VAPID application server public key.
    pub public_key: String,
    /// Key identifier used to detect VAPID key rotation.
    pub kid: String,
}

/// Returned by `register_push_device`. Carries both the assigned
/// XEP-0357 node id (feeds the user-server `<enable/>` IQ) AND the
/// Push Service-assigned device row id (scopes the matching
/// `disable-device` opt-out). The chat MUST persist both; a missing
/// `device_id` would force "disable everywhere" semantics that take
/// down push for sibling devices on the same node.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleRegisterDeviceResult {
    /// PubSub node that owns the items.
    pub node: String,
    /// Push-service device identifier required for device-specific disablement.
    pub device_id: String,
}

/// Arguments to `WaddleClient::register_push_device`. Bundled into a
/// struct so the JS call site passes a single typed object. The
/// platform discriminator + provider-specific fields drive the
/// XEP-0050 submit form one layer down via the typed
/// [`waddle_xmpp_client::push::PushDeviceCredentials`] enum.
///
/// Field shape pinned by the camelCase tests at the bottom of this
/// module. The chat-side wrapper at
/// `chat/src/lib/xmpp/client.ts::registerPushDevice` builds the
/// matching JS object.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterPushDeviceOptions {
    /// XMPP service address that processes this request or route.
    pub service_jid: String,
    /// Application identifier used by the push registration service.
    pub app_id: String,
    /// Wire-typed via the upstream `PushEnvironment` enum
    /// (`"prod"` / `"sandbox"`). Unknown values fail at
    /// `serde_wasm_bindgen::from_value` before reaching the IQ
    /// builder.
    pub environment: waddle_xmpp_client::push::PushEnvironment,
    /// Wire-typed via the upstream `PushPlatform` enum
    /// (`"web"` / `"apns"` / `"fcm"`).
    pub platform: waddle_xmpp_client::push::PushPlatform,
    /// Web Push subscription endpoint URL (only valid for
    /// `platform: "web"`).
    pub endpoint: Option<String>,
    /// Web Push P-256 public key (only valid for
    /// `platform: "web"`).
    pub p256dh: Option<String>,
    /// Web Push subscription auth secret (only valid for
    /// `platform: "web"`).
    pub auth: Option<String>,
    /// APNs device token (only valid for `platform: "apns"`).
    pub apns_token: Option<String>,
    /// FCM registration token (only valid for `platform: "fcm"`).
    pub fcm_token: Option<String>,
    /// FCM envelope-encryption P-256 public key, base64url (only valid
    /// for `platform: "fcm"`).
    pub fcm_p256dh: Option<String>,
    /// FCM envelope-encryption auth secret, base64url (only valid for
    /// `platform: "fcm"`).
    pub fcm_auth: Option<String>,
}

impl RegisterPushDeviceOptions {
    /// Convert the JS-flat options into the typed
    /// `PushDeviceCredentials` enum. Returns a diagnostic `String`
    /// when the platform-specific required fields are missing — the
    /// WASM bridge wraps that as a rejected `Promise` at the call
    /// site (keeps `JsValue` out of this conversion so native tests
    /// can exercise the validation path).
    ///
    /// # Errors
    /// Returns a diagnostic naming the first missing or empty credential field required
    /// by the selected platform.
    pub fn into_credentials(
        self,
    ) -> Result<waddle_xmpp_client::push::PushDeviceCredentials, String> {
        use waddle_xmpp_client::push::{PushDeviceCredentials, PushPlatform};
        match self.platform {
            PushPlatform::Web => Ok(PushDeviceCredentials::WebPush {
                endpoint: require_field(self.endpoint, "endpoint", "web")?,
                p256dh: require_field(self.p256dh, "p256dh", "web")?,
                auth: require_field(self.auth, "auth", "web")?,
            }),
            PushPlatform::Apns => Ok(PushDeviceCredentials::Apns {
                device_token: require_field(self.apns_token, "apnsToken", "apns")?,
            }),
            PushPlatform::Fcm => Ok(PushDeviceCredentials::Fcm {
                registration_token: require_field(self.fcm_token, "fcmToken", "fcm")?,
                p256dh: require_field(self.fcm_p256dh, "fcmP256dh", "fcm")?,
                auth: require_field(self.fcm_auth, "fcmAuth", "fcm")?,
            }),
            _ => Err("register_push_device: unsupported platform".to_string()),
        }
    }
}

fn require_field(
    value: Option<String>,
    field_name: &str,
    platform: &str,
) -> Result<String, String> {
    match value {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(format!(
            "register_push_device: platform '{platform}' requires field '{field_name}'"
        )),
    }
}

/// Persistent affiliation entry returned by a room administration query.
#[derive(Debug, Serialize)]
pub struct WaddleRoomMember {
    /// Bare JID of the user holding this room affiliation.
    pub jid: String,
    /// Persistent MUC affiliation expressed as its wire token.
    pub affiliation: String,
}

/// Room bot identity with an optional display name.
#[derive(Debug, Serialize)]
pub struct WaddleRoomBot {
    /// JID of an extension bot that has posted in the room.
    pub jid: String,
    /// Display name advertised for the room bot.
    pub name: Option<String>,
}

/// Conversation summary carrying unread counts and latest-message metadata.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleInboxConversation {
    /// Conversation partner JID, or room JID for a group conversation.
    pub partner: String,
    /// Conversation kind: `direct` or `muc`.
    pub kind: String,
    /// Stable identifier of the conversation or thread's latest message.
    pub last_stanza_id: String,
    /// Unix timestamp in seconds of the latest conversation update; zero when metadata is absent.
    pub last_updated: i64,
    /// Number of unread messages in this conversation or thread.
    pub unread: u32,
    /// Optional short preview of the most recent or root message.
    pub preview: Option<String>,
    /// Identifier of the conversation thread containing the message.
    pub thread: Option<String>,
    /// Optional display title of the thread.
    pub thread_title: Option<String>,
    /// Number of replies recorded for this thread.
    pub reply_count: Option<u32>,
    /// Author identity associated with the conversation's latest message.
    pub author: Option<String>,
}

/// Inbox conversation summaries and server-wide unread totals.
#[derive(Debug, Serialize)]
pub struct WaddleInboxResult {
    /// Sum of unread counts across the requesting user's whole inbox
    /// (server-side total). For backwards compatibility this is also
    /// surfaced under the same name the chat layer used pre-XEP-0430.
    pub total_unread: u32,
    /// XEP-0430 fin `total` — number of conversations matched.
    pub total: u32,
    /// XEP-0430 fin `unread` — number of conversations with unread > 0.
    pub unread_conversations: u32,
    /// Conversation summaries returned by the inbox query.
    pub conversations: Vec<WaddleInboxConversation>,
}

/// JS-facing options for [`crate::WaddleClient::fetch_inbox`].
///
/// Maps to XEP-0430 query attributes: `only_unread` ↔ `unread-only`,
/// `no_messages` ↔ `!messages` (inverted so the JS default — embedding
/// MAM bodies — needs no flag). The `since`/`room`/`threads` fields
/// from the legacy Waddle-private surface are removed; thread paging
/// now lives behind `fetch_threads` and time-window filtering is a
/// follow-up.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaddleFetchInboxOptions {
    /// Request only conversations containing unread messages.
    pub only_unread: bool,
    /// Omit embedded message bodies from the inbox response.
    pub no_messages: bool,
}

/// Roster contact with subscription state and group memberships.
#[derive(Debug, Serialize)]
pub struct WaddleRosterContact {
    /// Bare JID of the roster contact.
    pub jid: String,
    /// Locally assigned display name of the roster contact.
    pub name: Option<String>,
    /// Roster subscription state expressed as its wire token.
    pub subscription: Option<String>,
    /// Roster group names assigned to the contact.
    pub groups: Vec<String>,
}

/// Matching user identity returned by an XMPP directory search.
#[derive(Debug, Serialize)]
pub struct WaddleUserSearchResult {
    /// JID of the matched account.
    pub jid: String,
    /// Username supplied by the service or user directory.
    pub username: String,
    /// Optional human-readable name returned by the user directory.
    pub display_name: Option<String>,
}

/// Mood name and optional accompanying text to publish through PEP.
#[derive(Debug, Deserialize)]
pub struct WaddleMoodOpts {
    /// XEP-0107 mood element name to publish.
    pub kind: String,
    /// Optional description accompanying the published mood.
    pub text: Option<String>,
}

/// General and optional specific activity to publish through PEP.
#[derive(Debug, Deserialize)]
pub struct WaddleActivityOpts {
    /// General activity category defined by XEP-0108.
    pub general: String,
    /// Specific activity nested within the general category.
    pub specific: Option<String>,
    /// Optional description accompanying the published activity.
    pub text: Option<String>,
}

/// Optional track metadata to publish through PEP.
#[derive(Debug, Deserialize)]
pub struct WaddleTuneOpts {
    /// Performing artist of the current track.
    pub artist: Option<String>,
    /// Human-readable title supplied by the producer.
    pub title: Option<String>,
    /// Album or collection containing the track.
    pub source: Option<String>,
    /// Track duration in seconds.
    pub length: Option<u32>,
    /// User rating of the track on the XEP-0118 scale of 1 through 10.
    pub rating: Option<u8>,
    /// Track number or identifier supplied by the publisher.
    pub track: Option<String>,
    /// URI identifying the track.
    pub uri: Option<String>,
}

/// Published mood name and accompanying text returned by PEP.
#[derive(Debug, Serialize)]
pub struct WaddleMoodResult {
    /// XEP-0107 mood element name returned by PEP.
    pub kind: String,
    /// Optional description accompanying the fetched mood.
    pub text: Option<String>,
}

/// Published general and specific activity returned by PEP.
#[derive(Debug, Serialize)]
pub struct WaddleActivityResult {
    /// General activity category defined by XEP-0108.
    pub general: String,
    /// Specific activity nested within the general category.
    pub specific: Option<String>,
    /// Optional description accompanying the fetched activity.
    pub text: Option<String>,
}

/// Published track metadata returned by PEP.
#[derive(Debug, Serialize)]
pub struct WaddleTuneResult {
    /// Performing artist of the current track.
    pub artist: Option<String>,
    /// Human-readable title supplied by the producer.
    pub title: Option<String>,
    /// Album or collection containing the track.
    pub source: Option<String>,
    /// Track duration in seconds.
    pub length: Option<u32>,
    /// User rating of the track on the XEP-0118 scale of 1 through 10.
    pub rating: Option<u8>,
    /// Track number or identifier supplied by the publisher.
    pub track: Option<String>,
    /// URI identifying the track.
    pub uri: Option<String>,
}

/// Mood, activity, and tune fetched from a user's personal event nodes.
#[derive(Debug, Serialize)]
pub struct WaddlePepProfile {
    /// Published mood, absent when unavailable or retracted.
    pub mood: Option<WaddleMoodResult>,
    /// Published activity, absent when unavailable or retracted.
    pub activity: Option<WaddleActivityResult>,
    /// Published tune, absent when unavailable or retracted.
    pub tune: Option<WaddleTuneResult>,
}

/// Typed vCard4 payload exchanged with JS. Every property is optional and is
/// dropped from the published payload when `None` / undefined.
///
/// `pronouns` is carried through the wasm boundary even if the connected
/// server has not yet learned to round-trip XEP-0292 `PRONOUNS` — the editor
/// always offers the field, and the server is responsible for preserving or
/// dropping it.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WaddleVCard4 {
    /// Formatted full name, serialized using the `fn` property.
    #[serde(rename = "fn", default, skip_serializing_if = "Option::is_none")]
    pub full_name: Option<String>,
    /// Preferred nickname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// Pronouns supplied by the profile owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pronouns: Option<String>,
    /// Free-form profile note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Personal website URL supplied by the profile owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// URI of the profile photo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo_uri: Option<String>,
}

/// One XEP-0490 §3 displayed entry surfaced to the chat client.
#[derive(Debug, Serialize)]
pub struct WaddleMdsDisplayedEntry {
    /// The PEP item id — bare for DMs/rooms, full occupant for MUC PMs.
    pub chat_id: String,
    /// XEP-0359 stanza-id of the displayed message.
    pub stanza_id: String,
    /// Assigning entity's XEP-0359 `by` JID; preserve exactly for
    /// stanza-id authority matching.
    pub stanza_id_by: String,
}

/// Archive pagination mode and its optional cursor or time boundary.
#[derive(Debug, Deserialize)]
pub struct WaddleMamPageParam {
    /// Pagination mode: `latest`, `before`, or `after`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Archive cursor before which results should be fetched.
    pub before: Option<String>,
    /// Archive cursor after which results should be fetched.
    pub after: Option<String>,
    /// Optional RFC 3339 lower time boundary applied with any pagination mode.
    pub start: Option<String>,
}

/// Original author and message identifier of an outbound reply.
#[derive(Debug, Clone, Deserialize)]
pub struct WaddleReplyTarget {
    /// JID of the author of the replied-to message.
    pub author_jid: String,
    /// Identifier of the message being replied to.
    pub message_id: String,
}

/// Half-open Unicode character range of fallback text in a reply body.
#[derive(Debug, Clone, Deserialize)]
pub struct WaddleFallbackRange {
    /// Inclusive Unicode character offset where the range begins.
    pub start: u32,
    /// Exclusive Unicode character offset where the range ends.
    pub end: u32,
}

/// Thread identifier and optional parent thread for an outbound message.
#[derive(Debug, Clone, Deserialize)]
pub struct WaddleThreadTarget {
    /// Thread identifier included in the outbound message.
    pub id: String,
    /// Optional parent thread identifier.
    pub parent: Option<String>,
}

/// Body formatting span supplied to an outbound message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WaddleMarkupSpanInput {
    /// Markup token: `bold`, `italic`, `strikethrough`, `code`, `code_block`, `blockquote`, or `link`.
    pub span_type: String,
    /// Inclusive Unicode character offset where the range begins.
    pub start: u32,
    /// Exclusive Unicode character offset where the range ends.
    pub end: u32,
    /// Link target URI; absent for formatting spans without a link.
    pub uri: Option<String>,
}

/// Typed reference URI and its half-open character range in the message body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WaddleReference {
    /// XEP-0372 reference type, such as mention or data.
    pub ref_type: String,
    /// Target URI of the XEP-0372 reference, such as `xmpp:user@example.org`.
    pub uri: String,
    /// Inclusive character offset of the reference in the body.
    pub begin: u32,
    /// Exclusive Unicode character offset where the range ends.
    pub end: u32,
    /// Optional anchor URI identifying the reference target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

/// Optional message identifiers, threading, attachments, and body annotations.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaddleSendOptions {
    /// Caller-selected outgoing message stanza ID; omitted to generate an ID.
    pub stanza_id: Option<String>,
    /// Optional message subject.
    pub subject: Option<String>,
    /// Message and author targeted by this reply.
    pub reply: Option<WaddleReplyTarget>,
    /// Body range containing the quoted reply fallback.
    pub fallback: Option<WaddleFallbackRange>,
    /// Identifier of the conversation thread containing the message.
    pub thread: Option<WaddleThreadTarget>,
    /// Server-issued token associating a prepared link preview with this send.
    pub link_preview_token: Option<String>,
    /// Request a displayed marker from the recipient.
    pub request_displayed_marker: bool,
    /// XEP-0045 §7.5 MUC private message marker (see `SendMessageOptions::muc_pm`).
    pub muc_pm: bool,
    /// Shared attachment metadata extracted from the message.
    pub shared_files: Vec<WaddleSharedFile>,
    /// Formatting and link annotations over the body text.
    pub markup_spans: Vec<WaddleMarkupSpanInput>,
    /// Typed body references, including explicit mention targets.
    pub references: Vec<WaddleReference>,
    /// Sender-side directory for resolving typed `@label` tokens to
    /// XEP-0372 mention references (see `SendMessageOptions::mention_directory`).
    pub mention_directory: Option<WaddleMentionDirectory>,
    /// Source pack of an XEP-0449 sticker. Missing means an ordinary file send.
    pub sticker: Option<WaddleStickerRef>,
}

/// Typed reference to a sticker's original PEP or PubSub pack.
#[derive(Debug, Clone, Deserialize)]
pub struct WaddleStickerRef {
    /// Identifier of the advertised sticker pack.
    pub pack_id: String,
    /// Optional bare JID of the pack owner.
    pub pack_jid: Option<String>,
    /// Optional PubSub node hosting the referenced pack.
    pub pack_node: Option<String>,
}

/// Known mention targets and permission to resolve broadcast mention tokens.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaddleMentionDirectory {
    /// Known labels and JIDs eligible for mention resolution.
    pub targets: Vec<WaddleMentionTarget>,
    /// Whether broadcast mentions such as `@everyone` may be resolved.
    pub broadcasts: bool,
}

/// Visible mention label paired with the addressed XMPP identity.
#[derive(Debug, Clone, Deserialize)]
pub struct WaddleMentionTarget {
    /// Human-readable display label.
    pub label: String,
    /// Bare JID addressed when this mention label is resolved.
    pub jid: String,
}

/// XEP-style payload for one entry in a urn:waddle:threads:0 response,
/// shaped for JS consumption.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleThreadEntry {
    /// Channel JID used to scope a thread or listing.
    pub channel: String,
    /// Identifier of the thread represented by this entry.
    pub thread_id: String,
    /// Stable identifier of the conversation or thread's latest message.
    pub last_stanza_id: String,
    /// RFC 3339 timestamp.
    pub last_activity: String,
    /// Number of unread messages in this conversation or thread.
    pub unread: u32,
    /// Number of replies recorded for this thread.
    pub reply_count: u32,
    /// Whether this thread contains unread messages.
    pub has_unread: bool,
    /// Author JID of the thread's root message, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_author: Option<String>,
    /// Optional short preview of the most recent or root message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Optional display title of the thread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_title: Option<String>,
    /// Call-thread anchor summary, present when this thread anchors a call.
    #[serde(rename = "callThread", skip_serializing_if = "Option::is_none")]
    pub call_thread: Option<WaddleThreadCallAnchor>,
    /// Ended-call summary, present when the anchored call has ended.
    #[serde(rename = "callThreadEnded", skip_serializing_if = "Option::is_none")]
    pub call_thread_ended: Option<WaddleThreadCallEnded>,
}

/// Call-thread anchor summary on a `urn:waddle:threads:0` entry.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleThreadCallAnchor {
    /// `"muc"` or `"dm"`.
    pub kind: String,
    /// Ordered media tokens, e.g. `["audio", "video"]`.
    pub media: Vec<String>,
}

/// Ended-call summary on a `urn:waddle:threads:0` entry.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleThreadCallEnded {
    /// RFC 3339 timestamp when the call ended.
    pub ended: String,
    /// ISO-8601 duration, e.g. `"PT5M"`.
    pub duration: String,
}

/// Paged response to a urn:waddle:threads:0 query, shaped for JS.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WaddleThreadsPage {
    /// Total number of threads matching the query.
    pub total: u64,
    /// Number of matching threads that contain unread messages.
    pub unread_threads: u64,
    /// Thread entries returned in this page.
    pub entries: Vec<WaddleThreadEntry>,
    /// Opaque cursor for requesting the next page, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Subset of threads requested from the server.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaddleThreadStatusFilter {
    /// All threads matching the other query filters.
    All,
    /// Only threads containing unread messages.
    Unread,
    /// Only threads followed by the requesting account.
    Following,
}

/// Ordering applied by the server to a thread listing.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaddleThreadSort {
    /// Most recently active threads first.
    Recent,
    /// Highest unread count first, with most recent activity breaking count ties.
    Unread,
    /// Threads with the most replies first.
    Replies,
}

/// Options bag for `fetch_threads`. All fields optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaddleFetchThreadsOptions {
    /// Maximum number of thread entries requested per page.
    pub page_size: Option<u32>,
    /// Opaque cursor returned by the preceding thread page.
    pub after_cursor: Option<String>,
    /// Filter for all, unread, or followed threads.
    pub status: Option<WaddleThreadStatusFilter>,
    /// RFC 3339 lower bound on the thread's most recent activity.
    pub active_since: Option<String>,
    /// Channel JID used to scope a thread or listing.
    pub channel: Option<String>,
    /// Text filter applied to the thread listing.
    pub search: Option<String>,
    /// Requested server-side ordering of thread entries.
    pub sort: Option<WaddleThreadSort>,
}

/// JS-facing shape for one XEP-0402 bookmark item exposed via
/// `WaddleClient::fetch_user_bookmarks`.
///
/// `jid` is the bookmarked room's bare JID as a string (JS-friendly);
/// `notify_mode` is the XEP-0492 fallback setting (no `identity-*`
/// attrs). `None` means the bookmark has no `<notify/>` extension
/// and the chat UI should resolve against the conversation-kind
/// default from XEP-0492 §3.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleBookmarkItem {
    /// Bare JID of the bookmarked MUC room.
    pub jid: String,
    /// Optional display name assigned to the room bookmark.
    pub name: Option<String>,
    /// Whether the client should join this bookmarked room automatically.
    pub autojoin: bool,
    /// Optional fallback notification mode carried by the bookmark.
    pub notify_mode: Option<waddle_xmpp_client::xep::xep0492::NotifyMode>,
    /// Waddle rich XEP-0357 push-summary opt-in carried in the
    /// XEP-0492 §2.3 `<advanced/>` block of this conversation's
    /// `<notify/>` (see
    /// [`waddle_xmpp_client::xep::xep0492::read_rich_payload_opt_in`]).
    /// `false` — the default and absence-of-marker case — keeps the
    /// minimal XEP-0357 summary payload (#719).
    pub rich_payload_opt_in: bool,
}

/// JS-facing typed outcome for outbound chat/groupchat sends.
///
/// Mirrors the UniFFI `WaddleSendMessageOutcome`: expected validation and
/// stanza/transport failures resolve to this tagged shape instead of forcing
/// callers to parse rejected Promise strings. Serialization failures at the
/// wasm boundary can still reject.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum WaddleSendMessageOutcome {
    /// The outgoing message was accepted by the transport command.
    Sent {
        /// Identifier of the outgoing stanza for delivery correlation.
        stanza_id: String,
    },
    /// The client has no active driver connection.
    NotConnected,
    /// The recipient could not be parsed as the required JID type.
    InvalidRecipient,
    /// Message options could not be decoded or validated.
    InvalidOptions,
    /// The send failed with an XMPP stanza error.
    StanzaError,
    /// The transport could not send the message.
    TransportError,
    /// The message could not be constructed or failed for another client reason.
    Error,
}

/// JS-facing outcome for DM Jingle `session-terminate`.
///
/// `Orphaned` is deliberately narrow: it means the server rejected the
/// terminate because its in-memory Jingle participant registry no
/// longer contains the sender/peer pair, so the chat layer may still
/// send the XEP-0353 `<finish/>` message-level bookend. Other stanza
/// errors remain generic `Error` and must not be treated as a completed
/// Jingle termination.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum WaddleCallSessionTerminateOutcome {
    /// The server acknowledged the Jingle termination.
    Ok,
    /// The server no longer recognizes the caller as a participant in this session.
    Orphaned,
    /// Another stanza-level error prevented termination.
    Error,
}

/// JS-facing tagged outcome of `WaddleClient::set_room_notification_mode`.
///
/// Returned by always-resolving the Promise (never rejecting on
/// stanza-level errors). Transport / serialization failures still
/// reject the Promise via `JsValue`. This shape keeps the chat-side
/// branching typed — the `kind` discriminator lets the wrapper in
/// `BrowserXmppClient.setRoomNotificationMode` switch on a string
/// union without parsing message bodies. Round-9 reviewer P2 fix
/// for the typed-payloads hard rule.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum WaddleSetRoomNotificationModeOutcome {
    /// The updated room bookmark was published successfully.
    Ok {
        /// Bookmark containing the persisted notification settings.
        item: WaddleBookmarkItem,
    },
    /// XEP-0060 `precondition-not-met` on the publish — typically a
    /// pre-existing XEP-0223-style PEP node with a divergent
    /// `access_model`.
    NodeConfigMismatch,
    /// Any other stanza-level error (forbidden, internal-server-error,
    /// etc). The specific RFC 6120 §8.3 condition stays on the Rust
    /// side — emitted through `tracing::warn` so a debug build can
    /// recover it from logs — rather than crossing the JS boundary as
    /// a stringly-typed payload. The chat UI shows a generic "couldn't
    /// save" message; if future UX needs to discriminate more cases,
    /// the right move is to add new typed variants here (e.g.
    /// `Forbidden`, `InternalServerError`) instead of widening
    /// `Error` with a condition string. Round-13 PR compliance.
    Error,
}

/// Options bag for `WaddleClient::set_room_notification_mode`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRoomNotificationModeOptions {
    /// Bare JID of the room whose XEP-0402 bookmark carries the
    /// XEP-0492 `<notify/>` extension.
    pub room_jid: String,
    /// Typed at the WASM boundary — unknown values fail at
    /// `serde_wasm_bindgen::from_value` rather than reaching the IQ
    /// builder.
    pub mode: waddle_xmpp_client::xep::xep0492::NotifyMode,
    /// `<conference name='…'>` to use when no bookmark exists yet.
    /// Optional; XEP-0402 §2.2 makes the `name` attribute optional on
    /// the wire (XSD `use='optional'`). Callers SHOULD pass a name
    /// so the freshly-created bookmark carries a useful display
    /// label for clients that lean on it, but the WASM bridge
    /// happily publishes without one. When the room already has a
    /// bookmark its existing `name` is preserved verbatim regardless
    /// of this field.
    pub name: Option<String>,
    /// Waddle rich XEP-0357 push-summary opt-in (#719). When `true`,
    /// the publish writes `<advanced><rich-payload
    /// xmlns='urn:waddle:push:rich:0'/></advanced>` under the fallback
    /// `<notify/>` setting; when `false` it strips our marker while
    /// preserving any foreign `<advanced/>` children. Defaults to
    /// `false` (minimal payload) when the JS caller omits the field.
    #[serde(default)]
    pub rich_payload_opt_in: bool,
}

/// JS-facing shape for one Waddle DM-bookmark item (issue #720)
/// exposed via `WaddleClient::fetch_dm_bookmarks`.
///
/// The DM counterpart to [`WaddleBookmarkItem`]: the carrier is the
/// project-local `urn:waddle:dm-bookmarks:0` PEP node, whose item id
/// is the contact's bare JID and whose payload directly hosts one
/// official XEP-0492 `<notify/>` (no `<extensions/>` wrapper, no
/// native bookmark field). `jid` is that contact bare JID as a string;
/// `notify_mode` is the XEP-0492 fallback setting (no `identity-*`
/// attrs). The DM node is sparse / override-only — an item exists ONLY
/// when the DM has an override — so a `None` `notify_mode` means the
/// `<notify/>` carries only identity-scoped siblings; the chat UI
/// resolves against the XEP-0492 §3 direct-chat default (`always`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleDmBookmarkItem {
    /// The contact's bare JID (PEP item id) as a string.
    pub jid: String,
    /// XEP-0492 fallback setting read from the hosted `<notify/>`.
    pub notify_mode: Option<waddle_xmpp_client::xep::xep0492::NotifyMode>,
    /// Waddle rich XEP-0357 push-summary opt-in carried in the
    /// XEP-0492 §2.3 `<advanced/>` block of this DM's `<notify/>`
    /// (#719). `false` is the default / absence-of-marker case.
    pub rich_payload_opt_in: bool,
}

/// JS-facing tagged outcome of `WaddleClient::set_dm_notification_mode`
/// (issue #720).
///
/// Mirrors [`WaddleSetRoomNotificationModeOutcome`] but for the DM
/// carrier, with one extra variant: the DM node is sparse /
/// override-only, so returning a DM to the XEP-0492 §3 direct-chat
/// default (`always`, no opt-in, no foreign `<advanced/>`) RETRACTS the
/// item instead of publishing. That path resolves to `Removed`, letting
/// the chat reconcile its store to "no override" without a refetch.
/// Always-resolves the Promise on stanza-level errors (transport /
/// serialization failures still reject via `JsValue`).
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum WaddleSetDmNotificationModeOutcome {
    /// The DM carried an override; the item was published. Carries the
    /// surfaced item so the chat reconciles without a refetch.
    Ok {
        /// Published DM override containing the persisted notification settings.
        item: WaddleDmBookmarkItem,
    },
    /// The merged `<notify/>` collapsed to the §3 direct-chat default,
    /// so the item was retracted (XEP-0060 `<retract>`). `jid` echoes
    /// the contact so the chat can drop its override entry.
    Removed {
        /// Contact whose redundant DM override was removed.
        jid: String,
    },
    /// XEP-0060 `precondition-not-met` on the publish — a pre-existing
    /// node with a divergent `access_model`. Same semantics as the
    /// room path's variant of the same name.
    NodeConfigMismatch,
    /// Any other stanza-level error. The specific RFC 6120 §8.3
    /// condition stays on the Rust side (emitted via `tracing::warn`)
    /// rather than crossing the JS boundary as a stringly-typed
    /// payload — matching [`WaddleSetRoomNotificationModeOutcome`].
    Error,
}

/// Options bag for `WaddleClient::set_dm_notification_mode` (issue
/// #720). Mirrors [`SetRoomNotificationModeOptions`]'s camelCase wire
/// contract; a DM has no `name` field (no `<conference/>` to label).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDmNotificationModeOptions {
    /// Bare JID of the direct-chat contact whose DM-bookmark carries
    /// the XEP-0492 `<notify/>`. Serialized as `dmJid` on the JS side.
    pub dm_jid: String,
    /// Typed at the WASM boundary — unknown values fail at
    /// `serde_wasm_bindgen::from_value` rather than reaching the IQ
    /// builder.
    pub mode: waddle_xmpp_client::xep::xep0492::NotifyMode,
    /// Waddle rich XEP-0357 push-summary opt-in (#719). Serialized as
    /// `richPayloadOptIn`. Defaults to `false` (minimal payload) when
    /// the JS caller omits the field.
    #[serde(default)]
    pub rich_payload_opt_in: bool,
}

#[cfg(test)]
mod tests {
    use super::{RegisterPushDeviceOptions, WaddleExternalService};
    use waddle_xmpp_client::push::{PushDeviceCredentials, PushEnvironment, PushPlatform};
    use waddle_xmpp_client::xep::xep0215::{
        ExternalService, ExternalServiceTransport, ExternalServiceType,
    };

    /// A TLS-TURN entry converts to the JS surface with the `turns`
    /// discriminator, credentials, and `restricted` preserved. Pins the
    /// camelCase `serviceType` key the chat-side mapping reads.
    #[test]
    fn turns_service_serializes_with_camelcase_and_credentials() {
        let surfaced = WaddleExternalService::from(ExternalService {
            service_type: ExternalServiceType::Turns,
            host: "turn.waddle.social".into(),
            port: Some(443),
            transport: Some(ExternalServiceTransport::Tcp),
            username: Some("u".into()),
            password: Some("p".into()),
            expires: Some(
                chrono::DateTime::parse_from_rfc3339("2026-06-17T12:00:00Z").expect("rfc3339"),
            ),
            restricted: true,
        });
        let json = serde_json::to_value(&surfaced).expect("serialize");
        assert_eq!(json["serviceType"], "turns");
        assert_eq!(json["host"], "turn.waddle.social");
        assert_eq!(json["port"], 443);
        assert_eq!(json["transport"], "tcp");
        assert_eq!(json["username"], "u");
        assert_eq!(json["password"], "p");
        assert_eq!(json["expires"], "2026-06-17T12:00:00+00:00");
        assert_eq!(json["restricted"], true);
    }

    /// A STUN entry has no credentials; the optional fields are omitted from
    /// the JS object entirely (not emitted as `null`).
    #[test]
    fn stun_service_omits_absent_credential_fields() {
        let surfaced = WaddleExternalService::from(ExternalService {
            service_type: ExternalServiceType::Stun,
            host: "turn.waddle.social".into(),
            port: None,
            transport: Some(ExternalServiceTransport::Udp),
            username: None,
            password: None,
            expires: None,
            restricted: false,
        });
        let json = serde_json::to_value(&surfaced).expect("serialize");
        assert_eq!(json["serviceType"], "stun");
        let obj = json.as_object().expect("object");
        // Absent optional fields (incl. an omitted port) are dropped, not null.
        assert!(!obj.contains_key("port"));
        assert!(!obj.contains_key("username"));
        assert!(!obj.contains_key("password"));
        assert!(!obj.contains_key("expires"));
        assert_eq!(json["restricted"], false);
    }

    /// Pin the camelCase wire contract for the `register_push_device`
    /// WASM bridge. The chat-side wrapper at
    /// `chat/src/lib/xmpp/client.ts` builds a JS object with these
    /// camelCase keys.
    #[test]
    fn register_push_device_options_deserializes_web_push_payload() {
        let json = serde_json::json!({
            "serviceJid": "push.example.com",
            "appId": "app-web",
            "environment": "prod",
            "platform": "web",
            "endpoint": "https://fcm.googleapis.com/wp/abcdef",
            "p256dh": "p256-key",
            "auth": "auth-secret",
        });
        let parsed: RegisterPushDeviceOptions =
            serde_json::from_value(json).expect("camelCase deserialize");
        assert_eq!(parsed.service_jid, "push.example.com");
        assert_eq!(parsed.app_id, "app-web");
        assert_eq!(parsed.environment, PushEnvironment::Production);
        assert_eq!(parsed.platform, PushPlatform::Web);
        assert_eq!(
            parsed.into_credentials().unwrap(),
            PushDeviceCredentials::WebPush {
                endpoint: "https://fcm.googleapis.com/wp/abcdef".to_string(),
                p256dh: "p256-key".to_string(),
                auth: "auth-secret".to_string(),
            }
        );
    }

    #[test]
    fn register_push_device_options_deserializes_apns_payload() {
        let json = serde_json::json!({
            "serviceJid": "push.example.com",
            "appId": "app-ios",
            "environment": "sandbox",
            "platform": "apns",
            "apnsToken": "apns-token-1234",
        });
        let parsed: RegisterPushDeviceOptions =
            serde_json::from_value(json).expect("camelCase deserialize");
        assert_eq!(parsed.environment, PushEnvironment::Sandbox);
        assert_eq!(parsed.platform, PushPlatform::Apns);
        assert_eq!(
            parsed.into_credentials().unwrap(),
            PushDeviceCredentials::Apns {
                device_token: "apns-token-1234".to_string()
            }
        );
    }

    #[test]
    fn register_push_device_options_deserializes_fcm_payload() {
        let json = serde_json::json!({
            "serviceJid": "push.example.com",
            "appId": "app-android",
            "environment": "prod",
            "platform": "fcm",
            "fcmToken": "fcm-reg-token",
            "fcmP256dh": "fcm-p256dh-key",
            "fcmAuth": "fcm-auth-secret",
        });
        let parsed: RegisterPushDeviceOptions =
            serde_json::from_value(json).expect("camelCase deserialize");
        assert_eq!(
            parsed.into_credentials().unwrap(),
            PushDeviceCredentials::Fcm {
                registration_token: "fcm-reg-token".to_string(),
                p256dh: "fcm-p256dh-key".to_string(),
                auth: "fcm-auth-secret".to_string(),
            }
        );
    }

    #[test]
    fn register_push_device_options_requires_fcm_envelope_keys() {
        let json = serde_json::json!({
            "serviceJid": "push.example.com",
            "appId": "app-android",
            "environment": "prod",
            "platform": "fcm",
            "fcmToken": "fcm-reg-token",
            "fcmP256dh": "fcm-p256dh-key",
        });
        let parsed: RegisterPushDeviceOptions =
            serde_json::from_value(json).expect("camelCase deserialize");
        let err = parsed
            .into_credentials()
            .expect_err("fcmAuth is required for fcm");
        assert!(err.contains("fcmAuth"), "{err}");
    }

    #[test]
    fn register_push_device_options_rejects_snake_case() {
        // Sanity check the rename: snake_case keys must NOT match.
        let json = serde_json::json!({
            "service_jid": "push.example.com",
            "app-id": "app-web",
            "environment": "prod",
            "platform": "web",
            "endpoint": "https://example",
            "p256dh": "p",
            "auth": "a",
        });
        let parsed: Result<RegisterPushDeviceOptions, _> = serde_json::from_value(json);
        assert!(
            parsed.is_err(),
            "snake_case keys must NOT deserialize (rename_all=camelCase contract)"
        );
    }

    #[test]
    fn register_push_device_options_rejects_missing_required_field() {
        let json = serde_json::json!({
            "appId": "app-web",
            "environment": "prod",
            "platform": "web",
            "endpoint": "https://example",
            "p256dh": "p",
            "auth": "a",
        });
        let parsed: Result<RegisterPushDeviceOptions, _> = serde_json::from_value(json);
        assert!(parsed.is_err(), "missing serviceJid must error");
    }

    #[test]
    fn register_push_device_options_rejects_unknown_environment() {
        let json = serde_json::json!({
            "serviceJid": "push.example.com",
            "appId": "app-web",
            "environment": "staging",
            "platform": "web",
            "endpoint": "https://example",
            "p256dh": "p",
            "auth": "a",
        });
        let parsed: Result<RegisterPushDeviceOptions, _> = serde_json::from_value(json);
        assert!(
            parsed.is_err(),
            "unknown environment 'staging' must fail at the WASM boundary"
        );
    }

    #[test]
    fn register_push_device_options_rejects_unknown_platform() {
        let json = serde_json::json!({
            "serviceJid": "push.example.com",
            "appId": "app-web",
            "environment": "prod",
            "platform": "windows-mobile",
        });
        let parsed: Result<RegisterPushDeviceOptions, _> = serde_json::from_value(json);
        assert!(
            parsed.is_err(),
            "unknown platform must fail at the WASM boundary"
        );
    }

    #[test]
    fn register_push_device_options_rejects_web_payload_missing_endpoint() {
        let json = serde_json::json!({
            "serviceJid": "push.example.com",
            "appId": "app-web",
            "environment": "prod",
            "platform": "web",
            "p256dh": "p",
            "auth": "a",
        });
        let parsed: RegisterPushDeviceOptions = serde_json::from_value(json).expect("parses");
        assert!(
            parsed.into_credentials().is_err(),
            "web platform must require all three Web Push fields"
        );
    }

    /// Pin the JSON shape the chat client consumes for `WasmThreadEntry`
    /// (see `chat/src/lib/xmpp/wasm-types.ts`): a call-thread entry carries
    /// the camelCase `callThread` and, once ended, `callThreadEnded` keys.
    #[test]
    fn thread_entry_serializes_call_thread_fields() {
        let entry = super::WaddleThreadEntry {
            channel: "room@x".into(),
            thread_id: "call-1".into(),
            last_stanza_id: "S1".into(),
            last_activity: "2026-06-07T14:30:00Z".into(),
            unread: 0,
            reply_count: 4,
            has_unread: false,
            root_author: None,
            preview: None,
            thread_title: None,
            call_thread: Some(super::WaddleThreadCallAnchor {
                kind: "muc".into(),
                media: vec!["audio".into(), "video".into()],
            }),
            call_thread_ended: Some(super::WaddleThreadCallEnded {
                ended: "2026-06-07T14:35:00Z".into(),
                duration: "PT5M".into(),
            }),
        };
        let value = serde_json::to_value(&entry).expect("serializes");
        assert_eq!(
            value["callThread"],
            serde_json::json!({ "kind": "muc", "media": ["audio", "video"] })
        );
        assert_eq!(
            value["callThreadEnded"],
            serde_json::json!({ "ended": "2026-06-07T14:35:00Z", "duration": "PT5M" })
        );
    }

    #[test]
    fn thread_entry_omits_call_thread_fields_when_absent() {
        let entry = super::WaddleThreadEntry {
            channel: "room@x".into(),
            thread_id: "plain-1".into(),
            last_stanza_id: "S2".into(),
            last_activity: "2026-06-07T13:00:00Z".into(),
            unread: 0,
            reply_count: 1,
            has_unread: false,
            root_author: None,
            preview: None,
            thread_title: None,
            call_thread: None,
            call_thread_ended: None,
        };
        let value = serde_json::to_value(&entry).expect("serializes");
        let map = value.as_object().expect("object");
        assert!(!map.contains_key("callThread"));
        assert!(!map.contains_key("callThreadEnded"));
    }
}
