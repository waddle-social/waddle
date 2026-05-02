//! UniFFI FFI bridge exposing the Waddle XMPP client to Swift.

uniffi::setup_scaffolding!("waddle_xmpp_client");

use std::sync::Arc;

use jid::{BareJid, Jid};
use tokio::sync::Mutex;
use url::Url;

use waddle_xmpp_client::{
    avatar::AvatarExt,
    discovery::DiscoveryExt,
    mam::MamExt,
    messaging::{self, InboundMessage, MessagingExt, SendMessageOptions},
    xep::{
        reply::{FallbackRange, ReplyMarker},
        thread::ThreadRef,
    },
    AccessToken, ClientConfig, ClientHandle, ConnectionConfig, LifecycleEvent, MessagingEvent,
    OAuthBearerConfig, WebSocketConfig,
};
use waddle_xmpp_client::{ClientEvent, ClientResource, XmppClient};

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
pub struct WaddleRoom {
    pub jid: String,
    pub name: String,
    pub channel_type: String,
    pub position: i32,
}

/// XEP-0084 user avatar fetched from the `urn:xmpp:avatar` PEP nodes.
///
/// `data` is the raw image bytes (base64-decoded) ready to be handed to
/// `UIImage(data:)` / `NSImage(data:)` on the Swift side.
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
    fn on_connected(&self);
    fn on_disconnected(&self);
    fn on_error(&self, description: String);
}

// ── Main client object ───────────────────────────────────────────────────────

#[derive(uniffi::Object)]
pub struct WaddleClient {
    config: WaddleConfig,
    // Stored as Arc<Box<...>> so the constructor can take a Box (UniFFI callback_interface
    // generates FfiConverter for Box<dyn Trait>, not Arc<dyn Trait>).
    listener: Arc<Box<dyn WaddleEventListener>>,
    handle: Mutex<Option<ClientHandle>>,
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    #[uniffi::constructor]
    pub fn new(config: WaddleConfig, listener: Box<dyn WaddleEventListener>) -> Arc<Self> {
        Arc::new(Self {
            config,
            listener: Arc::new(listener),
            handle: Mutex::new(None),
        })
    }

    pub async fn connect(&self) {
        let jid: BareJid = match self.config.jid.parse() {
            Ok(j) => j,
            Err(e) => {
                self.listener.on_error(format!("Invalid JID: {e}"));
                return;
            }
        };

        let domain: BareJid = match jid.domain().to_string().parse() {
            Ok(d) => d,
            Err(e) => {
                self.listener.on_error(format!("Invalid domain: {e}"));
                return;
            }
        };

        let url: Url = match self.config.server_url.parse() {
            Ok(u) => u,
            Err(e) => {
                self.listener.on_error(format!("Invalid server URL: {e}"));
                return;
            }
        };

        let transport = match WebSocketConfig::new(url) {
            Ok(t) => t,
            Err(e) => {
                self.listener
                    .on_error(format!("Invalid WebSocket config: {e}"));
                return;
            }
        };

        let resource = match ClientResource::new(&self.config.resource) {
            Ok(r) => r,
            Err(e) => {
                self.listener.on_error(format!("Invalid resource: {e}"));
                return;
            }
        };

        let auth =
            OAuthBearerConfig::new(jid, resource, AccessToken::new(&self.config.access_token));
        let auth = match auth {
            Ok(a) => a,
            Err(e) => {
                self.listener.on_error(format!("Invalid auth config: {e}"));
                return;
            }
        };

        let client_config = match ClientConfig::new(ConnectionConfig::new(domain), transport, auth)
        {
            Ok(c) => c,
            Err(e) => {
                self.listener
                    .on_error(format!("Invalid client config: {e}"));
                return;
            }
        };

        let xmpp_client = match XmppClient::new(client_config) {
            Ok(c) => c,
            Err(e) => {
                self.listener
                    .on_error(format!("Failed to create XMPP client: {e}"));
                return;
            }
        };

        let driver = match xmpp_client.driver() {
            Ok(d) => d,
            Err(e) => {
                self.listener
                    .on_error(format!("Failed to create driver: {e}"));
                return;
            }
        };

        let client_handle = match driver.connect().await {
            Ok(h) => h,
            Err(e) => {
                self.listener.on_error(format!("Failed to connect: {e}"));
                return;
            }
        };

        let mut events = client_handle.events();
        let listener = Arc::clone(&self.listener);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => dispatch_event(event, &**listener),
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        listener.on_disconnected();
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });

        *self.handle.lock().await = Some(client_handle);
    }

    pub async fn disconnect(&self) {
        let mut guard = self.handle.lock().await;
        if let Some(h) = guard.take() {
            let _ = h.disconnect().await;
        }
    }

    pub async fn fetch_room_history(
        &self,
        room_jid: String,
        max_messages: u32,
        before_id: Option<String>,
    ) -> WaddleMamPage {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                empty_mam_page()
            }
            Some(h) => {
                match h
                    .fetch_room_history(&room_jid, max_messages, before_id.as_deref())
                    .await
                {
                    Ok(page) => mam_page_to_ffi(page),
                    Err(e) => {
                        drop(guard);
                        self.listener
                            .on_error(format!("fetch_room_history failed: {e}"));
                        empty_mam_page()
                    }
                }
            }
        }
    }

    pub async fn fetch_dm_history(
        &self,
        peer_jid: String,
        max_messages: u32,
        before_id: Option<String>,
    ) -> WaddleMamPage {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                empty_mam_page()
            }
            Some(h) => {
                match h
                    .fetch_dm_history(&peer_jid, max_messages, before_id.as_deref())
                    .await
                {
                    Ok(page) => mam_page_to_ffi(page),
                    Err(e) => {
                        drop(guard);
                        self.listener
                            .on_error(format!("fetch_dm_history failed: {e}"));
                        empty_mam_page()
                    }
                }
            }
        }
    }

    pub async fn send_groupchat_message(
        &self,
        room_jid: String,
        body: String,
        options: Option<WaddleSendOptions>,
    ) {
        let opts = match options.map(send_options_from_ffi).transpose() {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => {
                self.listener.on_error(e);
                return;
            }
        };
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
            }
            Some(h) => {
                let result = h.send_groupchat_message(&room_jid, &body, &opts).await;
                drop(guard);
                if let Err(e) = result {
                    self.listener
                        .on_error(format!("send_groupchat_message failed: {e}"));
                }
            }
        }
    }

    pub async fn send_chat_message(
        &self,
        peer_jid: String,
        body: String,
        options: Option<WaddleSendOptions>,
    ) {
        let opts = match options.map(send_options_from_ffi).transpose() {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => {
                self.listener.on_error(e);
                return;
            }
        };
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
            }
            Some(h) => {
                let result = h.send_chat_message(&peer_jid, &body, &opts).await;
                drop(guard);
                if let Err(e) = result {
                    self.listener
                        .on_error(format!("send_chat_message failed: {e}"));
                }
            }
        }
    }

    pub async fn join_room(&self, room_jid: String, nick: String) {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
            }
            Some(h) => {
                let result = h.join_room(&room_jid, &nick).await;
                drop(guard);
                if let Err(e) = result {
                    self.listener.on_error(format!("join_room failed: {e}"));
                }
            }
        }
    }

    pub async fn leave_room(&self, room_jid: String, nick: String) {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
            }
            Some(h) => {
                let result = h.leave_room(&room_jid, &nick).await;
                drop(guard);
                if let Err(e) = result {
                    self.listener.on_error(format!("leave_room failed: {e}"));
                }
            }
        }
    }

    pub async fn list_rooms(&self) -> Vec<WaddleRoom> {
        let guard = self.handle.lock().await;
        let Some(h) = guard.as_ref() else {
            drop(guard);
            self.listener.on_error("Not connected".to_string());
            return vec![];
        };

        let spaces_domain = format!("spaces.{}", jid_domain(&self.config.jid));

        // Step 1: discover the canonical space node (internal — not returned to caller).
        let space_items = match h.discover_items(&spaces_domain, None).await {
            Ok(items) => items,
            Err(e) => {
                drop(guard);
                self.listener
                    .on_error(format!("list_rooms: space discovery failed: {e}"));
                return vec![];
            }
        };

        let Some(space) = space_items.into_iter().next() else {
            return vec![];
        };

        let space_node = space.node.unwrap_or_else(|| space.jid.clone());

        // Step 2: discover rooms within the space. Room JIDs are returned as-is
        // so callers can use them directly for all XMPP operations.
        match h.discover_items(&spaces_domain, Some(&space_node)).await {
            Ok(items) => items
                .into_iter()
                .map(|item| WaddleRoom {
                    jid: item.jid.clone(),
                    name: item.name.unwrap_or_default(),
                    channel_type: "text".to_string(),
                    position: 0,
                })
                .collect(),
            Err(e) => {
                drop(guard);
                self.listener
                    .on_error(format!("list_rooms: room discovery failed: {e}"));
                vec![]
            }
        }
    }

    pub async fn send_presence(&self, status: Option<String>, show: Option<String>) {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
            }
            Some(h) => {
                let result = h.send_presence(status.as_deref(), show.as_deref()).await;
                drop(guard);
                if let Err(e) = result {
                    self.listener.on_error(format!("send_presence failed: {e}"));
                }
            }
        }
    }

    /// Request the XEP-0084 avatar for a user. Returns `None` when the target
    /// JID hasn't published an avatar or the fetch failed; errors are
    /// reported on the event listener so the caller can treat `None` as
    /// "fall back to initials".
    pub async fn request_avatar(&self, jid: String) -> Option<WaddleAvatar> {
        let bare: BareJid = match jid.parse() {
            Ok(j) => j,
            Err(e) => {
                self.listener
                    .on_error(format!("Invalid JID for avatar fetch: {e}"));
                return None;
            }
        };
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                None
            }
            Some(h) => match h.request_avatar(&bare).await {
                Ok(Some(avatar)) => Some(WaddleAvatar {
                    jid: avatar.jid.to_string(),
                    id: avatar.id,
                    mime_type: avatar.mime_type,
                    data: avatar.data,
                }),
                Ok(None) => None,
                Err(e) => {
                    drop(guard);
                    self.listener
                        .on_error(format!("request_avatar failed: {e}"));
                    None
                }
            },
        }
    }

    pub async fn discover_upload_service(&self) -> Option<String> {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                None
            }
            Some(h) => match h
                .discover_upload_service(jid_domain(&self.config.jid))
                .await
            {
                Ok(service_jid) => service_jid,
                Err(e) => {
                    drop(guard);
                    self.listener
                        .on_error(format!("discover_upload_service failed: {e}"));
                    None
                }
            },
        }
    }

    pub async fn request_upload_slot(
        &self,
        service_jid: String,
        filename: String,
        size: u64,
        content_type: String,
    ) -> Option<WaddleUploadSlot> {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                None
            }
            Some(h) => match h
                .request_upload_slot(&service_jid, &filename, size, &content_type)
                .await
            {
                Ok(slot) => Some(upload_slot_to_ffi(slot)),
                Err(e) => {
                    drop(guard);
                    self.listener
                        .on_error(format!("request_upload_slot failed: {e}"));
                    None
                }
            },
        }
    }
}

// ── Event dispatch ───────────────────────────────────────────────────────────

fn dispatch_event(event: ClientEvent, listener: &dyn WaddleEventListener) {
    match event {
        ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_)) => listener.on_connected(),
        ClientEvent::Messaging(MessagingEvent::Message(msg)) => {
            listener.on_message(inbound_to_ffi(*msg));
        }
        ClientEvent::Messaging(MessagingEvent::Presence(pres)) => {
            listener.on_presence(WaddlePresence {
                from: pres.from,
                to: pres.to,
                presence_type: pres
                    .presence_type
                    .unwrap_or_else(|| "available".to_string()),
                show: pres.show,
                status: pres.status,
                hats: pres.hats.into_iter().map(presence_hat_to_ffi).collect(),
                muc_affiliation: pres.muc_affiliation.map(muc_affiliation_to_ffi),
                muc_role: pres.muc_role.map(muc_role_to_ffi),
            });
        }
        ClientEvent::MamResult(archived) => {
            listener.on_mam_result(archived_to_ffi(archived));
        }
        _ => {}
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Extract the domain part from a JID like `user@domain` or `domain`.
fn jid_domain(jid: &str) -> &str {
    jid.split('@').next_back().unwrap_or(jid)
}

fn mam_page_to_ffi(page: waddle_xmpp_client::mam::MamPage) -> WaddleMamPage {
    WaddleMamPage {
        messages: page.messages.into_iter().map(archived_to_ffi).collect(),
        first_id: page.rsm.first,
        last_id: page.rsm.last,
        is_complete: page.is_complete,
    }
}

fn shared_file_to_ffi(file: waddle_xmpp_client::messaging::SharedFile) -> WaddleSharedFile {
    WaddleSharedFile {
        url: file.url,
        name: file.name,
        media_type: file.media_type,
        size: file.size,
        width: file.width,
        height: file.height,
        disposition: file.disposition.as_str().to_string(),
    }
}

fn upload_slot_to_ffi(slot: waddle_xmpp_client::discovery::UploadSlot) -> WaddleUploadSlot {
    WaddleUploadSlot {
        put_url: slot.put_url,
        get_url: slot.get_url,
        put_headers: slot
            .put_headers
            .into_iter()
            .map(|(name, value)| WaddleUploadHeader { name, value })
            .collect(),
    }
}

fn presence_hat_to_ffi(hat: waddle_xmpp_client::messaging::PresenceHat) -> WaddlePresenceHat {
    WaddlePresenceHat {
        uri: hat.uri,
        title: hat.title,
    }
}

fn muc_affiliation_to_ffi(
    affiliation: waddle_xmpp_client::messaging::MucAffiliation,
) -> WaddleMucAffiliation {
    use waddle_xmpp_client::messaging::MucAffiliation;
    match affiliation {
        MucAffiliation::Owner => WaddleMucAffiliation::Owner,
        MucAffiliation::Admin => WaddleMucAffiliation::Admin,
        MucAffiliation::Member => WaddleMucAffiliation::Member,
        MucAffiliation::Outcast => WaddleMucAffiliation::Outcast,
        MucAffiliation::None => WaddleMucAffiliation::None,
    }
}

fn muc_role_to_ffi(role: waddle_xmpp_client::messaging::MucRole) -> WaddleMucRole {
    use waddle_xmpp_client::messaging::MucRole;
    match role {
        MucRole::Moderator => WaddleMucRole::Moderator,
        MucRole::Participant => WaddleMucRole::Participant,
        MucRole::Visitor => WaddleMucRole::Visitor,
        MucRole::None => WaddleMucRole::None,
    }
}

/// Convert a parsed inbound message into the UniFFI record, flattening the
/// XEP-0428 char range into two optional `u32` fields (UniFFI has no tuple
/// support).
fn inbound_to_ffi(msg: InboundMessage) -> WaddleMessage {
    let is_muc = msg.message_type == "groupchat";
    let (fb_start, fb_end) = match msg.reply_fallback {
        Some((s, e)) => (Some(s), Some(e)),
        None => (None, None),
    };
    WaddleMessage {
        id: msg.id,
        from: msg.from,
        to: msg.to,
        body: msg.body,
        message_type: msg.message_type,
        timestamp: msg.timestamp.map(|t| t.to_rfc3339()),
        stanza_id: msg.stanza_id,
        origin_id: msg.origin_id,
        replaces_id: msg.replaces_id,
        retracts_id: msg.retracts_id,
        reaction_target_id: msg.reaction_target_id,
        reaction_emojis: msg.reaction_emojis,
        is_muc,
        thread: msg.thread_id.or(msg.thread),
        parent_thread_id: msg.parent_thread_id,
        reply_to_id: msg.reply_to_id,
        reply_to_sender: msg.reply_to_sender,
        reply_fallback_start: fb_start,
        reply_fallback_end: fb_end,
        shared_files: msg
            .shared_files
            .into_iter()
            .map(shared_file_to_ffi)
            .collect(),
    }
}

/// Convert an archived MAM message into the UniFFI record. Re-parses the
/// wrapped inner element through the full messaging parser so that replies
/// and fallback ranges survive history loads. The XEP-0201 nested-thread
/// `parent_thread_id` is read directly from the typed `archived` value
/// (the client parser extracts it via `crate::xep::thread::parse_thread`)
/// instead of being recovered from the re-parse — closes the parent-leak
/// path when `inner` is unparseable downstream.
fn archived_to_ffi(archived: waddle_xmpp_client::ArchivedMessage) -> WaddleArchivedMessage {
    let parsed = match messaging::parse(&archived.inner) {
        Some(MessagingEvent::Message(m)) => Some(m),
        _ => None,
    };
    let (fb_start, fb_end) = parsed
        .as_ref()
        .and_then(|m| m.reply_fallback)
        .map(|(s, e)| (Some(s), Some(e)))
        .unwrap_or((None, None));
    WaddleArchivedMessage {
        mam_id: archived.mam_id,
        query_id: archived.query_id,
        stanza_id: archived.stanza_id,
        timestamp: archived.timestamp.map(|t| t.to_rfc3339()),
        from: archived.from,
        to: archived.to,
        message_type: archived.message_type,
        body: archived.body,
        reaction_target_id: parsed.as_ref().and_then(|m| m.reaction_target_id.clone()),
        reaction_emojis: parsed
            .as_ref()
            .map(|m| m.reaction_emojis.clone())
            .unwrap_or_default(),
        thread: archived.thread,
        parent_thread_id: archived.parent_thread_id,
        reply_to_id: parsed.as_ref().and_then(|m| m.reply_to_id.clone()),
        reply_to_sender: parsed.as_ref().and_then(|m| m.reply_to_sender.clone()),
        reply_fallback_start: fb_start,
        reply_fallback_end: fb_end,
        shared_files: parsed
            .as_ref()
            .map(|m| {
                m.shared_files
                    .clone()
                    .into_iter()
                    .map(shared_file_to_ffi)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn empty_mam_page() -> WaddleMamPage {
    WaddleMamPage {
        messages: vec![],
        first_id: None,
        last_id: None,
        is_complete: false,
    }
}

/// Convert the FFI options record into the typed `SendMessageOptions`. JIDs
/// are parsed here (the earliest boundary) so the rest of the send path flows
/// through typed values per the typed-payloads hard rule. Returns a
/// human-readable error string on malformed input — surfaced via the listener.
fn send_options_from_ffi(opts: WaddleSendOptions) -> Result<SendMessageOptions, String> {
    let reply = match opts.reply {
        Some(target) => {
            let to = target
                .author_jid
                .parse::<Jid>()
                .map_err(|e| format!("Invalid reply author JID '{}': {e}", target.author_jid))?;
            Some(ReplyMarker {
                to,
                id: target.message_id,
            })
        }
        None => None,
    };

    let fallback = opts.fallback.map(|r| FallbackRange {
        start: r.start,
        end: r.end,
    });

    let thread = opts.thread.map(|t| ThreadRef {
        id: t.id,
        parent: t.parent,
    });

    let shared_files = opts
        .shared_files
        .into_iter()
        .map(|file| {
            let disposition = messaging::SharedFileDisposition::from_text_or_infer(
                Some(file.disposition.as_str()),
                file.media_type.as_deref(),
            );
            messaging::SharedFile {
                url: file.url,
                name: file.name,
                media_type: file.media_type,
                size: file.size,
                width: file.width,
                height: file.height,
                disposition,
            }
        })
        .collect();

    Ok(SendMessageOptions {
        reply,
        fallback,
        thread,
        shared_files,
    })
}
