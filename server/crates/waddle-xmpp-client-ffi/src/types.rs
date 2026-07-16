// ── Data types ───────────────────────────────────────────────────────────────

#[derive(uniffi::Record, Clone)]
pub struct WaddleConfig {
    pub server_url: String,
    pub jid: String,
    pub access_token: String,
    pub resource: String,
    /// XEP-0198 resume snapshot from a previous session. When present
    /// the runtime attempts `<resume/>` before resource binding and
    /// replays the queued outbound stanzas the server never acked.
    pub resume_state: Option<WaddleSmResumeState>,
}

/// XEP-0198 client resume snapshot crossing the FFI as an opaque
/// persistence round-trip: the Swift app stores it on disconnect and
/// feeds it back through [`WaddleConfig`] on the next connect. Queued
/// outbound stanzas travel as serialized XML strings — the message
/// stanza-id is re-derived from the element's `id` attribute on
/// restore, and the original enqueue instant survives only when the
/// element already carries a `<delay/>` stamp (identical semantics to
/// the wasm client's localStorage persistence of the same snapshot).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WaddleSmResumeState {
    /// SM resumption token from `<enabled id='…'/>`.
    pub previd: String,
    /// Count of inbound stanzas handled by this client.
    pub inbound_h: u32,
    /// Count of outbound stanzas sent by this client.
    pub outbound_h: u32,
    /// Server-advertised resumption window in seconds, when supplied.
    pub max_resume_seconds: Option<u32>,
    /// Outbound stanzas the server had not acked at snapshot time,
    /// serialized to XML in send order for lossless replay.
    pub queued_stanzas_xml: Vec<String>,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleMessage {
    pub id: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub body: Option<String>,
    /// RFC 6121 `<subject/>` — room topic changes and forum topic posts.
    pub subject: Option<String>,
    pub message_type: String,
    pub timestamp: Option<String>,
    pub stanza_id: Option<String>,
    /// XEP-0359 `by` authority of `stanza_id`. NOT verified here —
    /// consumers MUST check it against the expected archive authority
    /// before trusting the id (XEP-0359 §Security Considerations).
    pub stanza_id_by: Option<String>,
    /// Every XEP-0359 `<stanza-id/>` on the stanza, one per archiving
    /// entity, in document order.
    pub stanza_ids: Vec<WaddleStanzaId>,
    pub origin_id: Option<String>,
    pub replaces_id: Option<String>,
    pub retracts_id: Option<String>,
    /// XEP-0424 tombstone: id of the retraction message that replaced
    /// the original in the archive.
    pub retraction_id: Option<String>,
    /// XEP-0424: true when this message is a retraction tombstone.
    pub is_retracted: bool,
    /// XEP-0425 moderation target — the XEP-0359 id of the moderated
    /// message when this stanza is a moderation broadcast.
    pub moderation_target_id: Option<String>,
    /// XEP-0425 moderator JID (string form).
    pub moderated_by: Option<String>,
    /// XEP-0425 human-readable moderation reason.
    pub moderation_reason: Option<String>,
    pub reaction_target_id: Option<String>,
    pub reaction_emojis: Vec<String>,
    /// XEP-0085 chat state notification carried on this message.
    /// `None` when absent or when the wire value is not one of the
    /// five defined states.
    pub chat_state: Option<WaddleChatState>,
    /// XEP-0333 `<markable/>` request attached to this inbound message.
    pub displayed_marker_requested: bool,
    /// XEP-0333 `<displayed id='…'/>` marker target id.
    pub displayed_marker_id: Option<String>,
    pub is_muc: bool,
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
    /// XEP-0394 message markup spans over the body.
    pub markup_spans: Vec<WaddleMarkupSpan>,
    /// XEP-0372 broadcast mention URI (`@everyone` / `@here`), when
    /// one of the mention references targets the whole room.
    pub broadcast_mention: Option<String>,
    /// XEP-0372 mention URIs, flattened from `references`.
    pub mention_uris: Vec<String>,
    /// XEP-0372 references attached to this message — every
    /// `<reference/>` with the required `type` and `uri`, regardless
    /// of type. `mention_uris`/`broadcast_mention` are derived views.
    pub references: Vec<WaddleReference>,
    /// Forum channel classification; `None` for plain chat or when the
    /// wire value is not a recognised kind.
    pub forum_post_kind: Option<WaddleForumPostKind>,
    /// Forum topic title (the subject of a `topic` post).
    pub forum_title: Option<String>,
    /// XEP-0449: the message body is a sticker.
    pub is_sticker: bool,
    /// XEP-0511 link preview metadata (`urn:waddle:link-preview:0`
    /// pipeline output), one entry per previewed URL.
    pub link_previews: Vec<WaddleLinkPreview>,
    /// urn:waddle:pin:0 pin/unpin room system event. `None` when the
    /// message carries no `<pin-event/>` payload.
    pub pin_event: Option<WaddlePinEvent>,
    /// urn:waddle:call-thread:0 ended fastening targeting a
    /// call-thread anchor.
    pub call_thread_ended: Option<WaddleCallThreadEnded>,
    /// XEP-0280: direction of the carbon envelope this message was
    /// unwrapped from. Only stamped after the runtime verified the
    /// wrapping stanza came from the account's own bare JID (§11
    /// forgery rule); `None` for directly received stanzas.
    pub carbon: Option<WaddleCarbonDirection>,
    /// XEP-0461 reply target message id.
    pub reply_to_id: Option<String>,
    /// XEP-0461 reply target author JID (string form).
    pub reply_to_sender: Option<String>,
    /// XEP-0428 fallback range start (char offset, inclusive).
    pub reply_fallback_start: Option<u32>,
    /// XEP-0428 fallback range end (char offset, exclusive).
    pub reply_fallback_end: Option<u32>,
    /// urn:waddle:call-thread:0 call-thread anchor marker, if present.
    pub call_thread: Option<WaddleCallThreadAnchor>,
    /// XEP-0446 / XEP-0447 shared files attached to the message.
    pub shared_files: Vec<WaddleSharedFile>,
    /// XEP-0490 Message Displayed Synchronization PEP event payload.
    /// `None` when the message is not an MDS event; `Some(entries)`
    /// when it is — including `Some(vec![])` for an MDS event with
    /// zero items (rare but distinct from "not an MDS event").
    pub mds_displayed: Option<Vec<WaddleMdsDisplayedEntry>>,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleCallThreadAnchor {
    pub kind: String,
    pub sid: String,
    pub media: Vec<String>,
    pub initiator: String,
    pub started: String,
}

/// urn:waddle:call-thread:0 `<call-thread-ended/>` fastening. Mirrors
/// `waddle_xmpp_client::xep::call_thread::CallThreadEnded` 1:1.
#[derive(uniffi::Record, Clone)]
pub struct WaddleCallThreadEnded {
    /// XEP-0422 `<apply-to id='…'/>` target: the anchor's stanza id.
    pub anchor_id: String,
    /// RFC 3339 instant the call ended.
    pub ended: String,
    /// ISO 8601 duration of the call (e.g. `PT5M`).
    pub duration: String,
}

/// One XEP-0359 `<stanza-id/>` entry. Mirrors the core `StanzaId`
/// shape: `by` is required by XEP-0359 and always present.
#[derive(uniffi::Record, Clone)]
pub struct WaddleStanzaId {
    pub id: String,
    /// JID (string form) of the archiving entity that assigned `id`.
    pub by: String,
}

/// XEP-0085 chat state notification states. Wire values outside these
/// five are dropped to `None` at the conversion boundary.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleChatState {
    Active,
    Composing,
    Paused,
    Inactive,
    Gone,
}

/// Waddle forum post classification: `Topic` (thread + body + subject)
/// or `Reply` (thread + body). Wire values outside these two are
/// dropped to `None` at the conversion boundary.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleForumPostKind {
    Topic,
    Reply,
}

/// RFC 6120 §8.3.2 stanza-error `type` attribute. Mirrors the client
/// crate's `StanzaErrorType`; `Unknown` marks an unrecognised wire
/// value so consumers can distinguish it from every defined type.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleStanzaErrorType {
    Auth,
    Cancel,
    Continue,
    Modify,
    Wait,
    Unknown,
}

/// RFC 6120 §6.5 SASL failure conditions. Mirrors the client crate's
/// `SaslFailureCondition`; `Unknown` marks an unrecognised wire value.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleSaslCondition {
    Aborted,
    AccountDisabled,
    CredentialsExpired,
    EncryptionRequired,
    IncorrectEncoding,
    InvalidAuthzid,
    InvalidMechanism,
    MalformedRequest,
    MechanismTooWeak,
    NotAuthorized,
    TemporaryAuthFailure,
    Unknown,
}

/// XEP-0394 markup span kind. Mirrors the client crate's
/// `MarkupSpanType`; `Link` is Waddle's `urn:waddle:markup:0` span
/// extension (XEP-0394 defines no link span).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleMarkupSpanType {
    Bold,
    Italic,
    Strikethrough,
    Code,
    CodeBlock,
    Blockquote,
    Link,
}

/// XEP-0394 markup span over the message body. Offsets count Unicode
/// scalar values; `end` is exclusive. `uri` is populated only for
/// `Link` spans.
#[derive(uniffi::Record, Clone)]
pub struct WaddleMarkupSpan {
    pub span_type: WaddleMarkupSpanType,
    pub start: u32,
    pub end: u32,
    pub uri: Option<String>,
}

/// XEP-0372 §4 reference `type`. `Other` preserves unrecognised wire
/// values explicitly so extension references survive the boundary
/// instead of being silently dropped or smuggled through a bare String.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WaddleReferenceType {
    Mention,
    Data,
    Other { value: String },
}

/// XEP-0372 `<reference/>`. `begin`/`end` are body offsets; `(0, 0)`
/// is the "no body position" sentinel used by anchor-only references.
#[derive(uniffi::Record, Clone)]
pub struct WaddleReference {
    pub ref_type: WaddleReferenceType,
    pub uri: String,
    pub begin: u32,
    pub end: u32,
    /// XEP-0372 optional `anchor` attribute: the original, unresolved
    /// display text (or anchor URI) when offsets cannot be relied on.
    pub anchor: Option<String>,
}

/// XEP-0511 link preview card produced by the server-side
/// `urn:waddle:link-preview:0` pipeline.
#[derive(uniffi::Record, Clone)]
pub struct WaddleLinkPreview {
    pub original_url: String,
    pub normalized_url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<WaddleLinkPreviewImage>,
    /// Native-playable `og:video` media with a direct (non-iframe)
    /// media type; rendered in a native player on user action.
    pub video: Option<WaddleLinkPreviewVideo>,
    pub player_embed: Option<WaddleLinkPreviewPlayer>,
    /// True when the preview referenced remote media the server did
    /// not cache; the client must not fetch it itself.
    pub remote_media_unavailable: bool,
}

/// Cached preview image of a XEP-0511 link card.
#[derive(uniffi::Record, Clone)]
pub struct WaddleLinkPreviewImage {
    pub url: String,
    pub media_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt: Option<String>,
}

/// Native-playable `og:video` media surfaced from a XEP-0511 card.
#[derive(uniffi::Record, Clone)]
pub struct WaddleLinkPreviewVideo {
    pub url: String,
    pub media_type: String,
}

/// `og:video` iframe player embed of a XEP-0511 card.
#[derive(uniffi::Record, Clone)]
pub struct WaddleLinkPreviewPlayer {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Status of a composer-side `urn:waddle:link-preview:0` lookup IQ
/// (`WaddleClient::lookup_link_preview`).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleLinkPreviewLookupStatus {
    /// Preview resolved; the token in the paired preview payload is
    /// valid until its `expires_at` instant.
    Ready,
    /// The URL cannot be previewed (unparsable or unsupported).
    Unsupported,
    /// The server's resolver is disabled or refused the URL.
    Blocked,
    /// Resolution failed, or the response payload was malformed.
    Failed,
}

/// Ready payload of a link-preview lookup. Attach `token` through
/// [`WaddleSendOptions::link_preview_token`] — but only while the
/// RFC 3339 `expires_at` instant is still in the future.
#[derive(uniffi::Record, Clone)]
pub struct WaddleLinkPreviewLookupPreview {
    pub token: String,
    pub original_url: String,
    pub normalized_url: String,
    pub expires_at: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<WaddleLinkPreviewImage>,
    pub player_embed: Option<WaddleLinkPreviewPlayer>,
}

/// Typed outcome of the composer-side lookup: `preview` is present
/// exactly when `status` is [`WaddleLinkPreviewLookupStatus::Ready`].
#[derive(uniffi::Record, Clone)]
pub struct WaddleLinkPreviewLookup {
    pub status: WaddleLinkPreviewLookupStatus,
    pub preview: Option<WaddleLinkPreviewLookupPreview>,
}

/// urn:waddle:pin:0 pin/unpin action carried on a room system message.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddlePinAction {
    Pinned,
    Unpinned,
}

/// Frozen preview snapshot of a pinned message, taken at pin time.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WaddlePinPreview {
    /// Bare JID of the message author at pin time.
    pub author_jid: String,
    /// Author's MUC nick at pin time, if known.
    pub author_nick: Option<String>,
    /// Truncated body text (≤280 chars).
    pub text: String,
    /// Original message timestamp (RFC 3339), not the pin time.
    pub message_timestamp: String,
}

/// urn:waddle:pin:0 `<pin-event/>` room broadcast. Mirrors
/// `waddle_xmpp_client::pin::PinEvent`.
#[derive(uniffi::Record, Clone)]
pub struct WaddlePinEvent {
    pub action: WaddlePinAction,
    /// XEP-0359 stanza-id of the targeted message.
    pub target_stanza_id: String,
    /// Bare JID of the pinner/unpinner.
    pub by: String,
    /// `Some("retracted")` when the unpin was triggered by a
    /// XEP-0424 retraction cascade.
    pub reason: Option<String>,
    /// Frozen preview, present only on `Pinned` events.
    pub preview: Option<WaddlePinPreview>,
}

/// One `urn:waddle:pin:0` pinned-message entry returned by
/// `fetch_room_pins`. Mirrors `waddle_xmpp_client::pin::PinEntry` 1:1.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WaddlePinEntry {
    /// XEP-0359 stanza-id of the pinned message in the room archive.
    pub target_stanza_id: String,
    /// Bare JID of the user who pinned the message.
    pub pinner_jid: String,
    /// When the pin was applied (RFC 3339, server-stamped).
    pub pinned_at: String,
    /// Frozen preview snapshot taken at pin time.
    pub preview: WaddlePinPreview,
}

/// XEP-0280 carbon direction of an unwrapped forwarded copy. Always
/// §11-verified by the runtime before it reaches this boundary.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleCarbonDirection {
    /// Mirror of a message another of our resources sent (`<sent/>`).
    Sent,
    /// Mirror of a message another of our resources received
    /// (`<received/>`).
    Received,
}

/// XEP-0490 §3 displayed-marker entry surfaced to Swift. Mirrors
/// `waddle_xmpp_client::messaging::MdsDisplayedEntry` 1:1 — the
/// FFI does not collapse or rename fields so the Swift consumer
/// can correlate `chat_id` (PEP item id = the exact chat JID)
/// with its locally-tracked conversation list directly.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WaddleMdsDisplayedEntry {
    /// PEP item id = bare for DMs/rooms, full occupant for MUC PMs.
    pub chat_id: String,
    /// XEP-0359 id of the displayed message.
    pub stanza_id: String,
    /// Resource-less room or account-server authority from the XEP-0359 `by`
    /// attribute, as required by XEP-0490.
    pub stanza_id_by: String,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleArchivedMessage {
    pub mam_id: String,
    pub query_id: Option<String>,
    pub id: Option<String>,
    pub stanza_id: Option<String>,
    /// XEP-0359 `by` authority of `stanza_id` (unverified — see
    /// `WaddleMessage::stanza_id_by`).
    pub stanza_id_by: Option<String>,
    /// Every XEP-0359 `<stanza-id/>` on the inner message.
    pub stanza_ids: Vec<WaddleStanzaId>,
    pub origin_id: Option<String>,
    pub timestamp: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub message_type: String,
    pub body: Option<String>,
    /// RFC 6121 `<subject/>` of the inner message.
    pub subject: Option<String>,
    /// XEP-0308 correction target id.
    pub replaces_id: Option<String>,
    /// XEP-0424 retraction target id.
    pub retracts_id: Option<String>,
    /// XEP-0424 tombstone retraction message id.
    pub retraction_id: Option<String>,
    /// XEP-0424: true when the archived row is a retraction tombstone.
    pub is_retracted: bool,
    /// XEP-0425 moderation target id.
    pub moderation_target_id: Option<String>,
    /// XEP-0425 moderator JID (string form).
    pub moderated_by: Option<String>,
    /// XEP-0425 human-readable moderation reason.
    pub moderation_reason: Option<String>,
    pub reaction_target_id: Option<String>,
    pub reaction_emojis: Vec<String>,
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<String>,
    pub reply_fallback_start: Option<u32>,
    pub reply_fallback_end: Option<u32>,
    /// XEP-0394 markup spans of the inner message.
    pub markup_spans: Vec<WaddleMarkupSpan>,
    /// XEP-0372 broadcast mention URI, when present.
    pub broadcast_mention: Option<String>,
    /// XEP-0372 mention URIs, flattened from `references`.
    pub mention_uris: Vec<String>,
    /// XEP-0372 references of the inner message.
    pub references: Vec<WaddleReference>,
    /// Forum classification of the archived post.
    pub forum_post_kind: Option<WaddleForumPostKind>,
    /// Forum topic title (the subject of a `topic` post).
    pub forum_title: Option<String>,
    /// XEP-0449: the archived body is a sticker.
    pub is_sticker: bool,
    /// XEP-0045 real author JID from the archived `muc#user` payload,
    /// exposed by non-anonymous room archives.
    pub author_real_jid: Option<String>,
    /// urn:waddle:call-thread:0 call-thread anchor marker, if present.
    pub call_thread: Option<WaddleCallThreadAnchor>,
    /// urn:waddle:call-thread:0 ended fastening, if present.
    pub call_thread_ended: Option<WaddleCallThreadEnded>,
    pub shared_files: Vec<WaddleSharedFile>,
    /// XEP-0511 link previews of the inner message.
    pub link_previews: Vec<WaddleLinkPreview>,
    pub call_event: Option<WaddleCallEvent>,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleMamPage {
    pub messages: Vec<WaddleArchivedMessage>,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
    pub is_complete: bool,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddlePresenceHat {
    pub uri: String,
    pub title: String,
}

#[derive(uniffi::Enum, Clone)]
pub enum WaddleMucAffiliation {
    Owner,
    Admin,
    Member,
    Outcast,
    None,
}

#[derive(uniffi::Enum, Clone)]
pub enum WaddleMucRole {
    Moderator,
    Participant,
    Visitor,
    None,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddlePresence {
    pub from: Option<String>,
    pub to: Option<String>,
    pub presence_type: String,
    pub show: Option<String>,
    pub status: Option<String>,
    pub hats: Vec<WaddlePresenceHat>,
    pub muc_affiliation: Option<WaddleMucAffiliation>,
    pub muc_role: Option<WaddleMucRole>,
    /// XEP-0045 real occupant JID from the `muc#user` item, exposed
    /// by non-anonymous rooms.
    pub muc_jid: Option<String>,
    /// XEP-0045 `<status code='…'/>` markers from the `muc#user`
    /// payload. `110` identifies the recipient's own presence.
    pub muc_status_codes: Vec<u16>,
    /// XEP-0153 vCard avatar hash from `<x xmlns='vcard-temp:x:update'>`.
    pub vcard_avatar: Option<String>,
    /// XEP-0319 last-interaction instant (RFC 3339) from
    /// `<idle since='…'/>`; `None` when the contact is interacting now.
    pub idle_since: Option<String>,
    /// RFC 6120 §8.3 error condition element name (e.g. `not-found`)
    /// for `type="error"` presences; `None` otherwise.
    pub error_condition: Option<String>,
    /// RFC 6120 §8.3 error `type` attribute.
    pub error_type: Option<WaddleStanzaErrorType>,
    /// RFC 6120 §8.3 human-readable `<text/>` of an error presence.
    pub error_text: Option<String>,
    /// urn:waddle:in-call:0 raised-hand marker carried alongside
    /// `<muji/>`; `false` when the child is absent (lowered).
    pub hand_raised: bool,
    /// urn:waddle:in-call:0 muted-microphone marker; `false` when the
    /// child is absent (unmuted).
    pub muted: bool,
    /// XEP-0272 Muji presence advertisement
    /// `<muji xmlns='urn:xmpp:jingle:muji:0'/>` indicating the
    /// occupant has joined the room's group call. `None` when the
    /// presence carries no `<muji/>` child — per XEP-0272 §Leaving,
    /// that absence IS the leave marker. Drives the chat-side
    /// "N in call" indicator and the per-tile call badge.
    pub muji: Option<WaddleMujiPresence>,
}

/// Typed payload of the `urn:xmpp:jingle:muji:0` MUC presence
/// extension (XEP-0272).
#[derive(uniffi::Record, Clone)]
pub struct WaddleMujiPresence {
    /// True when the presence carried a `<preparing/>` child
    /// (XEP-0272 §Joining two-phase flow). UIs typically don't
    /// surface a chip until contents are advertised.
    pub preparing: bool,
    /// True when the presence advertised at least one `<content/>`
    /// child — the occupant is actively participating in the call.
    pub active: bool,
    /// True when at least one content description advertises audio.
    pub audio: bool,
    /// True when at least one content description advertises video.
    pub video: bool,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleSpace {
    pub id: String,
    pub service_jid: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleChannel {
    pub id: String,
    pub room_jid: String,
    pub name: String,
    pub description: Option<String>,
    pub channel_type: String,
    pub position: i32,
    pub space_id: String,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleTopology {
    pub spaces: Vec<WaddleSpace>,
    pub channels: Vec<WaddleChannel>,
}

/// XEP-0084 user avatar fetched from the `urn:xmpp:avatar` PEP nodes.
///
/// `data` is the raw image bytes (base64-decoded) when carried by XMPP.
/// `url` is present when XEP-0084 metadata or vCard `EXTVAL` points to an
/// externally hosted avatar.
#[derive(uniffi::Record, Clone)]
pub struct WaddleAvatar {
    /// Bare JID the avatar belongs to (string form).
    pub jid: String,
    /// SHA-1 content hash advertised on the metadata node.
    pub id: String,
    /// MIME type (e.g. `image/png`).
    pub mime_type: String,
    /// Decoded image bytes.
    pub data: Vec<u8>,
    /// Externally hosted avatar URL.
    pub url: Option<String>,
}

/// XEP-0446 / XEP-0447 shared-file metadata exposed to Swift.
#[derive(uniffi::Record, Clone)]
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
    /// rendering.
    pub encrypted: Option<WaddleEncryptedFile>,
}

/// XEP-0448 cipher/key/iv/hashes envelope for encrypted Stateless File
/// Sharing payloads.
#[derive(uniffi::Record, Clone)]
pub struct WaddleEncryptedFile {
    /// Cipher URN, e.g. `urn:xmpp:ciphers:aes-256-gcm-nopadding:0`.
    pub cipher: String,
    /// Base64-encoded symmetric key.
    pub key_b64: String,
    /// Base64-encoded initialization vector / nonce.
    pub iv_b64: String,
    pub hashes: Vec<WaddleEncryptedFileHash>,
    /// Source URLs the ciphertext can be fetched from. Always non-empty.
    pub sources: Vec<String>,
}

/// XEP-0300 hash entry nested under an `<encrypted/>` envelope.
#[derive(uniffi::Record, Clone)]
pub struct WaddleEncryptedFileHash {
    pub algo: String,
    pub value_b64: String,
}

/// Header the client must include when uploading to a XEP-0363 slot.
#[derive(uniffi::Record, Clone)]
pub struct WaddleUploadHeader {
    pub name: String,
    pub value: String,
}

/// XEP-0363 upload slot with PUT/GET URLs and required PUT headers.
#[derive(uniffi::Record, Clone)]
pub struct WaddleUploadSlot {
    pub put_url: String,
    pub get_url: String,
    pub put_headers: Vec<WaddleUploadHeader>,
}

/// XEP-0461 reply target attached to an outbound message.
#[derive(uniffi::Record, Clone)]
pub struct WaddleReplyTarget {
    /// JID (string form) of the author of the message being replied to.
    /// For MUC this is the occupant full JID; for 1:1 the bare JID.
    pub author_jid: String,
    /// Id of the message being replied to.
    pub message_id: String,
}

/// XEP-0428 fallback range identifying the quoted-prefix inside the body.
/// Offsets count Unicode scalar values and `end` is exclusive.
#[derive(uniffi::Record, Clone)]
pub struct WaddleFallbackRange {
    pub start: u32,
    pub end: u32,
}

/// XEP-0201 thread reference with optional parent for nested threads.
#[derive(uniffi::Record, Clone)]
pub struct WaddleThreadTarget {
    pub id: String,
    pub parent: Option<String>,
}

/// Options bag attached to an outbound chat or groupchat send.
#[derive(uniffi::Record, Clone, Default)]
pub struct WaddleSendOptions {
    pub stanza_id: Option<String>,
    /// RFC 6121 `<subject/>`, used by forum topic posts.
    pub subject: Option<String>,
    pub reply: Option<WaddleReplyTarget>,
    pub fallback: Option<WaddleFallbackRange>,
    pub thread: Option<WaddleThreadTarget>,
    /// XEP-0394 markup spans over the outbound body.
    pub markup_spans: Vec<WaddleMarkupSpan>,
    /// XEP-0372 references (mentions) attached to the send.
    pub references: Vec<WaddleReference>,
    pub shared_files: Vec<WaddleSharedFile>,
    pub link_preview_token: Option<String>,
    pub request_displayed_marker: bool,
    /// XEP-0045 §7.5: mark the message as a MUC private message so
    /// the sender's other clients can classify the sent-carbon copy
    /// without knowing the room.
    pub muc_pm: bool,
}

/// Typed outcome for outbound message sends. The old FFI surface
/// returned an empty string for not-connected, invalid options, and
/// transport/protocol failures; this keeps Swift-side control flow
/// explicit without moving human diagnostics out of `on_error`.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WaddleSendMessageOutcome {
    Sent { stanza_id: String },
    NotConnected,
    InvalidRecipient,
    InvalidOptions,
    StanzaError,
    TransportError,
    Error,
}

// ── A/V calls (XEP-0353 JMI + XEP-0166 Jingle) ──────────────────────────────

/// Media kinds offered or accepted on a call. Mirrors
/// `waddle_xmpp_client::messaging::CallMedia` 1:1 so the Swift side
/// can read the boolean flags directly without a wrapper enum.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleCallMedia {
    pub audio: bool,
    pub video: bool,
}

/// LiveKit join credentials extracted from the server-issued
/// `urn:waddle:transports:livekit:0` transport on a Jingle
/// session-initiate / session-accept. The Swift app feeds these
/// straight to the LiveKit iOS/macOS SDK.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleLiveKitJoin {
    pub url: String,
    pub room: String,
    pub identity: String,
    pub token: String,
}

/// Variants of an inbound A/V call event. Matches the wire shapes
/// `messaging::call::CallEventKind` already parses for the wasm
/// chat client — flattened for UniFFI (no nested struct payloads
/// per variant).
#[derive(uniffi::Enum, Clone, Debug)]
pub enum WaddleCallEventKind {
    /// XEP-0353 §5.1.1 `<propose/>` — the ringing UI start signal.
    Propose { media: WaddleCallMedia },
    /// XEP-0353 §3.2 `<ringing/>` — responder device is ringing.
    Ringing,
    /// XEP-0353 §5.1.2 `<proceed/>` — peer is accepting the call.
    Proceed,
    /// XEP-0353 §5.1.3 `<reject/>` — peer declined the call.
    Reject {
        reason: Option<WaddleJingleReason>,
        tie_break: bool,
    },
    /// XEP-0353 §5.1.4 `<retract/>` — caller cancelled before answer.
    Retract {
        reason: Option<WaddleJingleReason>,
        tie_break: bool,
    },
    /// XEP-0353 `<finish/>` — call ended cleanly, or migrated to a
    /// replacement session in the existing-session tie-break case.
    Finish {
        reason: Option<WaddleJingleReason>,
        migrated_to: Option<String>,
    },
    /// XEP-0166 §6.4 `session-initiate` with a populated LiveKit
    /// transport. The Swift app uses `join` to connect to the room.
    SessionInitiate {
        join: WaddleLiveKitJoin,
        media: WaddleCallMedia,
    },
    /// XEP-0166 §7.2 `session-accept`. Carries the responder's
    /// LiveKit credentials.
    SessionAccept {
        join: WaddleLiveKitJoin,
        media: WaddleCallMedia,
    },
    /// XEP-0166 §7.4 `session-terminate`. `reason` is the typed
    /// XEP-0166 condition; `None` when the terminate carries no
    /// `<reason/>` child. Unknown conditions seen on the wire are
    /// surfaced as `None` and logged via `on_error` rather than
    /// passed through as an opaque string (typed-payloads hard
    /// rule in `CLAUDE.md`).
    SessionTerminate { reason: Option<WaddleJingleReason> },
}

/// XEP-0166 §7.4 session-terminate reason conditions. Mirrors the
/// 17 variants in `xmpp_parsers::jingle::Reason` so the wire
/// parser's enum is the single source of truth — outbound calls
/// re-parse via `FromStr` and inbound events are emitted only when
/// the wire value resolves to one of these variants.
#[derive(uniffi::Enum, Clone, Debug, Copy, PartialEq, Eq)]
pub enum WaddleJingleReason {
    AlternativeSession,
    Busy,
    Cancel,
    ConnectivityError,
    Decline,
    Expired,
    FailedApplication,
    FailedTransport,
    GeneralError,
    Gone,
    IncompatibleParameters,
    MediaError,
    SecurityError,
    Success,
    Timeout,
    UnsupportedApplications,
    UnsupportedTransports,
}

/// Typed A/V call event surfaced to Swift via
/// `WaddleClientEvent::Call`.
/// `from` is the stamped sender JID (a *full* JID for propose /
/// session-initiate per XEP-0353 §0.6); `to` is the stamped stanza
/// recipient when available; `sid` is the Jingle session id used to
/// correlate every later event in the call.
#[derive(uniffi::Record, Clone)]
pub struct WaddleCallEvent {
    pub from: String,
    pub to: Option<String>,
    pub sid: String,
    pub kind: WaddleCallEventKind,
}

// ── Push notifications (XEP-0357 + XEP-0050) ─────────────────────────────────

/// Outcome of a successful `register_push_device` UniFFI call.
/// Carries the assigned XEP-0357 node id AND the Push Service-assigned
/// device row id. The Apple client persists both: node feeds the
/// user-server XEP-0357 `<enable/>` IQ; device id scopes the
/// matching `disable_push_device` opt-out so a per-device unsubscribe
/// doesn't take down push for sibling devices on the same node.
#[derive(uniffi::Record, Clone)]
pub struct WaddleRegisterDeviceResult {
    pub node: String,
    pub device_id: String,
}

/// Provider deployment environment. `Sandbox` distinguishes the APNs
/// `apns_development` endpoint from `apns_production`; Web Push and
/// FCM accept only `Production` today.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum WaddlePushEnvironment {
    Production,
    Sandbox,
}

impl From<WaddlePushEnvironment> for waddle_xmpp_client::push::PushEnvironment {
    fn from(value: WaddlePushEnvironment) -> Self {
        match value {
            WaddlePushEnvironment::Production => Self::Production,
            WaddlePushEnvironment::Sandbox => Self::Sandbox,
        }
    }
}

/// Platform-discriminated provider credentials. Mirrors the upstream
/// `PushDeviceCredentials` enum exactly; UniFFI generates a Swift
/// associated-value enum that the Apple client populates per
/// platform.
#[derive(uniffi::Enum, Clone)]
pub enum WaddlePushDeviceCredentials {
    WebPush {
        endpoint: String,
        p256dh: String,
        auth: String,
    },
    Apns {
        device_token: String,
    },
    Fcm {
        registration_token: String,
    },
}

impl From<WaddlePushDeviceCredentials> for waddle_xmpp_client::push::PushDeviceCredentials {
    fn from(value: WaddlePushDeviceCredentials) -> Self {
        match value {
            WaddlePushDeviceCredentials::WebPush {
                endpoint,
                p256dh,
                auth,
            } => Self::WebPush {
                endpoint,
                p256dh,
                auth,
            },
            WaddlePushDeviceCredentials::Apns { device_token } => Self::Apns { device_token },
            WaddlePushDeviceCredentials::Fcm { registration_token } => {
                Self::Fcm { registration_token }
            }
        }
    }
}

// ── Callback interface ───────────────────────────────────────────────────────

/// Single typed event stream surfaced to the app. New event kinds are
/// added as enum variants, which keeps the callback protocol itself
/// stable across FFI releases.
#[derive(uniffi::Enum, Clone)]
pub enum WaddleClientEvent {
    /// Session is bound and ready (fires on `SessionReady`).
    Connected,
    /// The event stream closed; no further events will fire.
    Disconnected,
    /// Typed inbound chat/groupchat message (or message-shaped event).
    Message { message: WaddleMessage },
    /// Typed inbound presence.
    Presence { presence: WaddlePresence },
    /// XEP-0313 archived message from an active history query.
    MamResult { message: WaddleArchivedMessage },
    /// XEP-0198: the server acked the outbound message with this id.
    DeliveryAcked { stanza_id: String },
    /// XEP-0198: transport-level delivery failure for this id.
    DeliveryFailed { stanza_id: String },
    /// XEP-0353 / XEP-0166 inbound call event. Fires for every JMI
    /// envelope and Jingle session control stanza addressed to the
    /// bound resource. The Swift app surfaces it as the ringing UI,
    /// the in-call HUD, and the hang-up handler.
    Call { event: WaddleCallEvent },
    /// XEP-0198 resume snapshot changed. `None` after an explicit
    /// disconnect or when the session carries no resumable state;
    /// persist `Some(state)` and feed it back via
    /// `WaddleConfig.resume_state` on the next connect.
    ResumeStateChanged { state: Option<WaddleSmResumeState> },
    /// RFC 6120 §6.5 SASL failure during connect. Terminal for the
    /// presented credentials — apps must not blindly retry with the
    /// same token (web #1164: surface "sign in again", never an
    /// eternal reconnect spinner).
    AuthenticationFailed { condition: WaddleSaslCondition },
    /// Human-readable diagnostic. Never carries protocol data.
    Error { description: String },
}

#[uniffi::export(callback_interface)]
pub trait WaddleEventListener: Send + Sync {
    fn on_event(&self, event: WaddleClientEvent);
}
