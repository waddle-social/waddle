use super::*;

#[derive(Debug, Serialize)]
pub struct WaddleMarkupSpan {
    pub span_type: String,
    pub start: usize,
    pub end: usize,
    pub uri: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddleStanzaId {
    pub id: String,
    pub by: String,
}

#[derive(Debug, Serialize)]
pub struct WaddleMessage {
    pub id: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub body: Option<String>,
    pub subject: Option<String>,
    pub message_type: String,
    pub timestamp: Option<String>,
    pub stanza_id: Option<String>,
    pub stanza_id_by: Option<String>,
    pub stanza_ids: Vec<WaddleStanzaId>,
    pub origin_id: Option<String>,
    pub replaces_id: Option<String>,
    pub retracts_id: Option<String>,
    pub retraction_id: Option<String>,
    pub is_retracted: bool,
    pub moderation_target_id: Option<String>,
    pub moderated_by: Option<String>,
    pub moderation_reason: Option<String>,
    pub chat_state: Option<String>,
    pub displayed_marker_id: Option<String>,
    pub reaction_target_id: Option<String>,
    pub reaction_emojis: Vec<String>,
    pub is_muc: bool,
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<String>,
    pub reply_fallback_start: Option<u32>,
    pub reply_fallback_end: Option<u32>,
    pub markup_spans: Vec<WaddleMarkupSpan>,
    pub broadcast_mention: Option<String>,
    pub mention_uris: Vec<String>,
    pub references: Vec<WaddleReference>,
    pub forum_post_kind: Option<String>,
    pub forum_title: Option<String>,
    pub forum_thread_title: Option<String>,
    pub is_sticker: bool,
    pub shared_files: Vec<WaddleSharedFile>,
    /// urn:waddle:pin:0 pin/unpin event surfaced from a system message
    /// (#414). `None` when the message carries no `<pin-event/>`.
    pub pin_event: Option<WaddlePinEvent>,
    pub extension_envelope: Option<WaddleExtensionEnvelope>,
    pub extension_body_fallback: bool,
}

#[derive(Debug, Serialize)]
pub struct WaddleExtensionEnvelope {
    pub version: u32,
    pub enrichments: Vec<WaddleExtensionEnrichment>,
}

#[derive(Debug, Serialize)]
pub struct WaddleExtensionEnrichment {
    pub id: String,
    pub plugin: String,
    pub capability: String,
    pub payload_namespace: String,
    pub created: String,
    pub source: Option<WaddleExtensionSource>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub payloads: Vec<WaddleExtensionPayloadElement>,
    pub launches: Vec<WaddleExtensionLaunch>,
}

#[derive(Debug, Serialize)]
pub struct WaddleExtensionSource {
    pub stanza_id: String,
    pub body_start: Option<u32>,
    pub body_end: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct WaddleExtensionLaunch {
    pub id: String,
    pub plugin: String,
    pub action: String,
    pub command_node: String,
    pub label: String,
    pub context: WaddleExtensionLaunchContext,
    pub payloads: Vec<WaddleExtensionPayloadElement>,
    pub expires_at: Option<String>,
    pub token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddleExtensionLaunchContext {
    pub waddle_id: String,
    pub room: Option<String>,
    pub source_stanza_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddleExtensionPayloadElement {
    pub namespace: String,
    pub name: String,
    pub attributes: Vec<WaddleExtensionPayloadAttribute>,
    pub text: Option<String>,
    pub children: Vec<WaddleExtensionPayloadElement>,
}

#[derive(Debug, Serialize)]
pub struct WaddleExtensionPayloadAttribute {
    pub name: String,
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

#[derive(Debug, Serialize)]
pub struct WaddleArchivedMessage {
    pub mam_id: String,
    pub query_id: Option<String>,
    pub id: Option<String>,
    pub stanza_id: Option<String>,
    pub stanza_id_by: Option<String>,
    pub stanza_ids: Vec<WaddleStanzaId>,
    pub origin_id: Option<String>,
    pub timestamp: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub message_type: String,
    pub body: Option<String>,
    pub subject: Option<String>,
    pub replaces_id: Option<String>,
    pub retracts_id: Option<String>,
    pub retraction_id: Option<String>,
    pub is_retracted: bool,
    pub moderation_target_id: Option<String>,
    pub moderated_by: Option<String>,
    pub moderation_reason: Option<String>,
    pub reaction_target_id: Option<String>,
    pub reaction_emojis: Vec<String>,
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<String>,
    pub reply_fallback_start: Option<u32>,
    pub reply_fallback_end: Option<u32>,
    pub markup_spans: Vec<WaddleMarkupSpan>,
    pub broadcast_mention: Option<String>,
    pub mention_uris: Vec<String>,
    pub references: Vec<WaddleReference>,
    pub forum_post_kind: Option<String>,
    pub forum_title: Option<String>,
    pub forum_thread_title: Option<String>,
    pub is_sticker: bool,
    pub author_real_jid: Option<String>,
    pub shared_files: Vec<WaddleSharedFile>,
    pub extension_envelope: Option<WaddleExtensionEnvelope>,
    pub extension_body_fallback: bool,
}

#[derive(Debug, Serialize)]
pub struct WaddleMamPage {
    pub messages: Vec<WaddleArchivedMessage>,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
    pub is_complete: bool,
}

#[derive(Debug, Serialize)]
pub struct WaddlePresenceHat {
    pub uri: String,
    pub title: String,
}

#[derive(Debug, Serialize)]
pub struct WaddleCallMedia {
    pub audio: bool,
    pub video: bool,
}

#[derive(Debug, Serialize)]
pub struct WaddleLiveKitJoin {
    pub url: String,
    pub room: String,
    pub identity: String,
    pub token: String,
}

/// JS-facing call event. The `kind` discriminator drives a switch
/// on the chat side: `propose|proceed|reject|retract|finish|
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
    pub from: String,
    pub sid: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<WaddleCallMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join: Option<WaddleLiveKitJoin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddlePresence {
    pub from: Option<String>,
    pub to: Option<String>,
    pub presence_type: String,
    pub show: Option<String>,
    pub status: Option<String>,
    pub hats: Vec<WaddlePresenceHat>,
    pub muc_affiliation: Option<String>,
    pub muc_role: Option<String>,
    pub muc_jid: Option<String>,
    pub vcard_avatar: Option<String>,
    /// XEP-0272 Muji `<muji xmlns='urn:xmpp:jingle:muji:0'/>`
    /// extension on a MUC occupant's presence, surfaced as
    /// `{ preparing, active }` for the chat-side participant-tracking
    /// store. `None` per XEP-0272 §Leaving means the occupant has
    /// left the call.
    pub muji: Option<WaddleMujiPresence>,
}

#[derive(Debug, Serialize)]
pub struct WaddleMujiPresence {
    pub preparing: bool,
    pub active: bool,
}

#[derive(Debug, Serialize)]
pub struct WaddleAvatar {
    pub jid: String,
    pub id: String,
    pub mime_type: String,
    pub data: Vec<u8>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRoute {
    pub service_jid: String,
    pub plugin_id: String,
    pub route_id: String,
    pub label: String,
    pub scope: String,
    pub surface: String,
    pub state_node: String,
    pub payload_namespace: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRouteItem {
    pub id: Option<String>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub link: Option<WaddleExtensionRouteLink>,
    pub description: Option<String>,
    pub fields: Vec<WaddleExtensionRouteItemField>,
    pub options: Vec<WaddleExtensionRouteItemOption>,
    pub actions: Vec<WaddleExtensionRouteItemAction>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WaddleExtensionRouteLink {
    pub href: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WaddleExtensionRouteItemField {
    pub name: String,
    pub label: Option<String>,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WaddleExtensionRouteItemOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRouteItemAction {
    pub launch: WaddleExtensionRouteItemLaunch,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRouteItemLaunch {
    pub id: String,
    pub plugin_id: String,
    pub action_id: String,
    pub command_node: String,
    pub label: String,
    pub launch_token: String,
    pub expires_at: String,
    pub waddle_id: String,
    pub room_jid: Option<String>,
    pub source_stanza_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaddleSharedFile {
    pub url: String,
    pub name: Option<String>,
    pub media_type: Option<String>,
    pub size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
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
    #[serde(default)]
    pub hashes: Vec<WaddleEncryptedFileHash>,
    /// Source URLs the ciphertext can be fetched from. Always non-empty.
    pub sources: Vec<String>,
}

/// XEP-0300 hash entry nested under a `WaddleEncryptedFile`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaddleEncryptedFileHash {
    pub algo: String,
    pub value_b64: String,
}

#[derive(Debug, Serialize)]
pub struct WaddleUploadHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct WaddleUploadSlot {
    pub put_url: String,
    pub get_url: String,
    pub put_headers: Vec<WaddleUploadHeader>,
}

#[derive(Debug, Serialize)]
pub struct WaddleServerVersion {
    pub name: Option<String>,
    pub version: Option<String>,
    pub os: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddleRoomMember {
    pub jid: String,
    pub affiliation: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WaddleInboxConversation {
    pub partner: String,
    pub kind: String,
    pub last_stanza_id: String,
    pub last_updated: i64,
    pub unread: u32,
    pub preview: Option<String>,
    pub thread: Option<String>,
    pub thread_title: Option<String>,
    pub reply_count: Option<u32>,
    pub author: Option<String>,
}

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
    pub only_unread: bool,
    pub no_messages: bool,
}

#[derive(Debug, Serialize)]
pub struct WaddleRosterContact {
    pub jid: String,
    pub name: Option<String>,
    pub subscription: Option<String>,
    pub groups: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddleUserSearchResult {
    pub jid: String,
    pub username: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WaddleMoodOpts {
    pub kind: String,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WaddleActivityOpts {
    pub general: String,
    pub specific: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WaddleTuneOpts {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub source: Option<String>,
    pub length: Option<u32>,
    pub rating: Option<u8>,
    pub track: Option<String>,
    pub uri: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddleMoodResult {
    pub kind: String,
    pub text: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddleActivityResult {
    pub general: String,
    pub specific: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddleTuneResult {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub source: Option<String>,
    pub length: Option<u32>,
    pub rating: Option<u8>,
    pub track: Option<String>,
    pub uri: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddlePepProfile {
    pub mood: Option<WaddleMoodResult>,
    pub activity: Option<WaddleActivityResult>,
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
    #[serde(rename = "fn", default, skip_serializing_if = "Option::is_none")]
    pub full_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pronouns: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// One XEP-0490 §3 displayed entry surfaced to the chat client.
#[derive(Debug, Serialize)]
pub struct WaddleMdsDisplayedEntry {
    /// The PEP item id — the bare JID of the chat (DM contact or
    /// MUC room).
    pub chat_id: String,
    /// XEP-0359 stanza-id of the displayed message.
    pub stanza_id: String,
    /// JID that injected the stanza-id (the MUC room for group
    /// chats; the user's own server for 1:1 chats).
    pub stanza_id_by: String,
}

#[derive(Debug, Deserialize)]
pub struct WaddleMamPageParam {
    #[serde(rename = "type")]
    pub kind: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaddleReplyTarget {
    pub author_jid: String,
    pub message_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaddleFallbackRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaddleThreadTarget {
    pub id: String,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WaddleMarkupSpanInput {
    pub span_type: String,
    pub start: u32,
    pub end: u32,
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WaddleReference {
    pub ref_type: String,
    pub uri: String,
    pub begin: u32,
    pub end: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaddleSendOptions {
    pub stanza_id: Option<String>,
    pub subject: Option<String>,
    pub reply: Option<WaddleReplyTarget>,
    pub fallback: Option<WaddleFallbackRange>,
    pub thread: Option<WaddleThreadTarget>,
    pub shared_files: Vec<WaddleSharedFile>,
    pub markup_spans: Vec<WaddleMarkupSpanInput>,
    pub references: Vec<WaddleReference>,
}

/// XEP-style payload for one entry in a urn:waddle:threads:0 response,
/// shaped for JS consumption.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleThreadEntry {
    pub channel: String,
    pub thread_id: String,
    pub last_stanza_id: String,
    /// RFC 3339 timestamp.
    pub last_activity: String,
    pub unread: u32,
    pub reply_count: u32,
    pub has_unread: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_title: Option<String>,
}

/// Paged response to a urn:waddle:threads:0 query, shaped for JS.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WaddleThreadsPage {
    pub total: u64,
    pub unread_threads: u64,
    pub entries: Vec<WaddleThreadEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Options bag for `fetch_threads`. All fields optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaddleFetchThreadsOptions {
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}
