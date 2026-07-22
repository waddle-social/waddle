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
pub struct WaddleCallThreadAnchor {
    pub kind: String,
    pub sid: String,
    pub media: Vec<String>,
    pub initiator: String,
    pub started: String,
}

#[derive(Debug, Serialize)]
pub struct WaddleCallThreadEnded {
    pub anchor_id: String,
    pub ended: String,
    pub duration: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WaddleInCallSignal {
    Reaction { sid: String, emoji: String },
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
    pub displayed_marker_requested: bool,
    pub displayed_marker_id: Option<String>,
    pub reaction_target_id: Option<String>,
    pub reaction_emojis: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_call: Option<WaddleInCallSignal>,
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
    /// urn:waddle:call-thread:0 room call-thread anchor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_thread: Option<WaddleCallThreadAnchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_thread_ended: Option<WaddleCallThreadEnded>,
    pub shared_files: Vec<WaddleSharedFile>,
    pub link_previews: Vec<WaddleLinkPreview>,
    /// urn:waddle:pin:0 pin/unpin event surfaced from a system message
    /// (#414). `None` when the message carries no `<pin-event/>`.
    pub pin_event: Option<WaddlePinEvent>,
    pub extension_envelope: Option<WaddleExtensionEnvelope>,
    pub extension_body_fallback: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pubsub_events: Vec<WaddlePubsubEvent>,
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
    pub sent: bool,
    pub received: bool,
}

#[derive(Debug, Serialize)]
pub struct WaddlePubsubEvent {
    pub from: Option<String>,
    pub node: String,
    pub items: Vec<WaddlePubsubEventItem>,
}

#[derive(Debug, Serialize)]
pub struct WaddlePubsubEventItem {
    pub id: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub retracted: bool,
    pub payload: WaddlePubsubEventPayload,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum WaddlePubsubEventPayload {
    AttachmentSummary {
        reactions: Vec<WaddlePubsubReactionSummary>,
        noticed_count: Option<u32>,
    },
    Opaque {
        xml: String,
    },
    Empty,
}

#[derive(Debug, Serialize)]
pub struct WaddlePubsubReactionSummary {
    pub emoji: String,
    pub count: u32,
    pub reactors: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddleLinkPreview {
    pub original_url: String,
    pub normalized_url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<WaddleLinkPreviewImage>,
    pub video: Option<WaddleLinkPreviewVideo>,
    pub player_embed: Option<WaddleLinkPreviewPlayer>,
    pub remote_media_unavailable: bool,
}

#[derive(Debug, Serialize)]
pub struct WaddleLinkPreviewVideo {
    pub url: String,
    pub media_type: String,
}

#[derive(Debug, Serialize)]
pub struct WaddleLinkPreviewImage {
    pub url: String,
    pub media_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaddleLinkPreviewPlayer {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
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
    /// urn:waddle:call-thread:0 room call-thread anchor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_thread: Option<WaddleCallThreadAnchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_thread_ended: Option<WaddleCallThreadEnded>,
    pub shared_files: Vec<WaddleSharedFile>,
    pub link_previews: Vec<WaddleLinkPreview>,
    pub extension_envelope: Option<WaddleExtensionEnvelope>,
    pub extension_body_fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_event: Option<WaddleCallEvent>,
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

/// JS-facing XEP-0215 external service (one `<service/>` entry). The chat maps
/// an array of these to `RTCIceServer[]` for LiveKit's `rtcConfig`. Wire-form
/// `serviceType`/`transport` strings come straight from the typed core value's
/// `as_str`, so the discriminator has a single source of truth.
#[derive(Debug, Serialize)]
pub struct WaddleExternalService {
    #[serde(rename = "serviceType")]
    pub service_type: &'static str,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
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
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub sid: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<WaddleCallMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join: Option<WaddleLiveKitJoin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "tieBreak", skip_serializing_if = "Option::is_none")]
    pub tie_break: Option<bool>,
    #[serde(rename = "migratedTo", skip_serializing_if = "Option::is_none")]
    pub migrated_to: Option<String>,
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
    /// XEP-0045 MUC `<status code='…'/>` markers as raw numbers (e.g.
    /// `110` self-presence, `100` non-anonymous). Omitted when the
    /// presence carries none, so the chat side sees `undefined`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub muc_status_codes: Vec<u16>,
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

#[derive(Debug, Serialize)]
pub struct WaddleMujiPresence {
    pub preparing: bool,
    pub active: bool,
    pub audio: bool,
    pub video: bool,
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
    /// XEP-0446 / XEP-0300 plaintext content hashes inside `<file/>`.
    /// XEP-0448 requires at least one for encrypted sends; distinct from
    /// the ciphertext hashes nested inside `encrypted`.
    #[serde(default)]
    pub hashes: Vec<WaddleEncryptedFileHash>,
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

/// XEP-0300 hash entry: nested under a `WaddleEncryptedFile` (ciphertext
/// digest) or inside a `WaddleSharedFile`'s file metadata (plaintext digest).
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

/// Result of `fetch_vapid_public_key` — the Push Service's currently-
/// active VAPID public key + kid as advertised via the XEP-0128 disco
/// extension form `urn:waddle:push:vapid:0`. The chat hands
/// `publicKey` directly to `pushManager.subscribe({ applicationServerKey })`
/// after a base64url-no-pad → `Uint8Array` decode, and stores `kid` so
/// rotation is detectable on subsequent reconnects.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleVapidAdvertisement {
    pub public_key: String,
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
    pub node: String,
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
    pub service_jid: String,
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
}

impl RegisterPushDeviceOptions {
    /// Convert the JS-flat options into the typed
    /// `PushDeviceCredentials` enum. Returns a diagnostic `String`
    /// when the platform-specific required fields are missing — the
    /// WASM bridge wraps that as a rejected `Promise` at the call
    /// site (keeps `JsValue` out of this conversion so native tests
    /// can exercise the validation path).
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
            }),
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

#[derive(Debug, Deserialize)]
pub struct WaddleMamPageParam {
    #[serde(rename = "type")]
    pub kind: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub start: Option<String>,
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
    pub link_preview_token: Option<String>,
    pub request_displayed_marker: bool,
    /// XEP-0045 §7.5 MUC private message marker (see `SendMessageOptions::muc_pm`).
    pub muc_pm: bool,
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
    pub total: u64,
    pub unread_threads: u64,
    pub entries: Vec<WaddleThreadEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaddleThreadStatusFilter {
    All,
    Unread,
    Following,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaddleThreadSort {
    Recent,
    Unread,
    Replies,
}

/// Options bag for `fetch_threads`. All fields optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaddleFetchThreadsOptions {
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
    pub status: Option<WaddleThreadStatusFilter>,
    pub active_since: Option<String>,
    pub channel: Option<String>,
    pub search: Option<String>,
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
    pub jid: String,
    pub name: Option<String>,
    pub autojoin: bool,
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
pub enum WaddleSendMessageOutcome {
    Sent { stanza_id: String },
    NotConnected,
    InvalidRecipient,
    InvalidOptions,
    StanzaError,
    TransportError,
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
pub enum WaddleCallSessionTerminateOutcome {
    Ok,
    Orphaned,
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
pub enum WaddleSetRoomNotificationModeOutcome {
    Ok {
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
pub enum WaddleSetDmNotificationModeOutcome {
    /// The DM carried an override; the item was published. Carries the
    /// surfaced item so the chat reconciles without a refetch.
    Ok { item: WaddleDmBookmarkItem },
    /// The merged `<notify/>` collapsed to the §3 direct-chat default,
    /// so the item was retracted (XEP-0060 `<retract>`). `jid` echoes
    /// the contact so the chat can drop its override entry.
    Removed { jid: String },
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
        });
        let parsed: RegisterPushDeviceOptions =
            serde_json::from_value(json).expect("camelCase deserialize");
        assert_eq!(
            parsed.into_credentials().unwrap(),
            PushDeviceCredentials::Fcm {
                registration_token: "fcm-reg-token".to_string()
            }
        );
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
