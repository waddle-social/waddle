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
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleArchivedMessage {
    pub mam_id: String,
    pub query_id: Option<String>,
    pub stanza_id: Option<String>,
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
}
