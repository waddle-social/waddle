// ── Data types ───────────────────────────────────────────────────────────────

#[derive(uniffi::Record, Clone)]
pub struct WaddleConfig {
    pub server_url: String,
    pub jid: String,
    pub access_token: String,
    pub resource: String,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleMessage {
    pub id: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub body: Option<String>,
    pub message_type: String,
    pub timestamp: Option<String>,
    pub stanza_id: Option<String>,
    pub origin_id: Option<String>,
    pub replaces_id: Option<String>,
    pub retracts_id: Option<String>,
    pub reaction_target_id: Option<String>,
    pub reaction_emojis: Vec<String>,
    /// XEP-0333 `<markable/>` request attached to this inbound message.
    pub displayed_marker_requested: bool,
    pub is_muc: bool,
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
    /// XEP-0461 reply target message id.
    pub reply_to_id: Option<String>,
    /// XEP-0461 reply target author JID (string form).
    pub reply_to_sender: Option<String>,
    /// XEP-0428 fallback range start (char offset, inclusive).
    pub reply_fallback_start: Option<u32>,
    /// XEP-0428 fallback range end (char offset, exclusive).
    pub reply_fallback_end: Option<u32>,
    /// XEP-0446 / XEP-0447 shared files attached to the message.
    pub shared_files: Vec<WaddleSharedFile>,
    /// XEP-0490 Message Displayed Synchronization PEP event payload.
    /// `None` when the message is not an MDS event; `Some(entries)`
    /// when it is — including `Some(vec![])` for an MDS event with
    /// zero items (rare but distinct from "not an MDS event").
    pub mds_displayed: Option<Vec<WaddleMdsDisplayedEntry>>,
}

/// XEP-0490 §3 displayed-marker entry surfaced to Swift. Mirrors
/// `waddle_xmpp_client::messaging::MdsDisplayedEntry` 1:1 — the
/// FFI does not collapse or rename fields so the Swift consumer
/// can correlate `chat_id` (PEP item id = bare JID of the chat)
/// with its locally-tracked conversation list directly.
#[derive(uniffi::Record, Clone)]
pub struct WaddleMdsDisplayedEntry {
    /// PEP item id = bare JID of the chat (DM contact or MUC room).
    pub chat_id: String,
    /// XEP-0359 id of the displayed message.
    pub stanza_id: String,
    /// JID that injected the stanza-id (the MUC room for group
    /// chats; the user's own server for 1:1 chats).
    pub stanza_id_by: String,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleArchivedMessage {
    pub mam_id: String,
    pub query_id: Option<String>,
    pub id: Option<String>,
    pub stanza_id: Option<String>,
    pub origin_id: Option<String>,
    pub timestamp: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub message_type: String,
    pub body: Option<String>,
    pub reaction_target_id: Option<String>,
    pub reaction_emojis: Vec<String>,
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<String>,
    pub reply_fallback_start: Option<u32>,
    pub reply_fallback_end: Option<u32>,
    pub shared_files: Vec<WaddleSharedFile>,
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
    pub reply: Option<WaddleReplyTarget>,
    pub fallback: Option<WaddleFallbackRange>,
    pub thread: Option<WaddleThreadTarget>,
    pub shared_files: Vec<WaddleSharedFile>,
    pub link_preview_token: Option<String>,
    pub request_displayed_marker: bool,
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

/// Typed A/V call event surfaced to Swift via `on_call(...)`.
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

#[uniffi::export(callback_interface)]
pub trait WaddleEventListener: Send + Sync {
    fn on_message(&self, message: WaddleMessage);
    fn on_presence(&self, presence: WaddlePresence);
    fn on_mam_result(&self, message: WaddleArchivedMessage);
    fn on_message_delivery_acked(&self, stanza_id: String);
    fn on_message_delivery_failed(&self, stanza_id: String);
    fn on_connected(&self);
    fn on_disconnected(&self);
    fn on_error(&self, description: String);
    /// XEP-0353 / XEP-0166 inbound call event. Fires for every
    /// JMI envelope and Jingle session control stanza addressed to
    /// the bound resource. The Swift app surfaces it as the
    /// ringing UI, the in-call HUD, and the hang-up handler.
    fn on_call(&self, event: WaddleCallEvent);
}
