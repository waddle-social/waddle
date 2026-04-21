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
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<String>,
    pub reply_fallback_start: Option<u32>,
    pub reply_fallback_end: Option<u32>,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleMamPage {
    pub messages: Vec<WaddleArchivedMessage>,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
    pub is_complete: bool,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddlePresence {
    pub from: Option<String>,
    pub to: Option<String>,
    pub presence_type: String,
    pub show: Option<String>,
    pub status: Option<String>,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleDiscoveredWaddle {
    pub id: String,
    pub name: String,
    pub is_public: bool,
}

#[derive(uniffi::Record, Clone)]
pub struct WaddleDiscoveredChannel {
    pub id: String,
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

    pub async fn discover_waddles(&self) -> Vec<WaddleDiscoveredWaddle> {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                vec![]
            }
            Some(h) => {
                let spaces_domain = format!("spaces.{}", jid_domain(&self.config.jid));
                match h.discover_items(&spaces_domain, None).await {
                    Ok(items) => items
                        .into_iter()
                        .map(|item| WaddleDiscoveredWaddle {
                            id: item.node.unwrap_or_else(|| item.jid.clone()),
                            name: item.name.unwrap_or_default(),
                            is_public: true,
                        })
                        .collect(),
                    Err(e) => {
                        drop(guard);
                        self.listener
                            .on_error(format!("discover_waddles failed: {e}"));
                        vec![]
                    }
                }
            }
        }
    }

    pub async fn discover_channels(&self, waddle_id: String) -> Vec<WaddleDiscoveredChannel> {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                vec![]
            }
            Some(h) => {
                let spaces_domain = format!("spaces.{}", jid_domain(&self.config.jid));
                match h.discover_items(&spaces_domain, Some(&waddle_id)).await {
                    Ok(items) => items
                        .into_iter()
                        .map(|item| {
                            // item.jid = "{waddleUUID}_{channelUUID}@muc.{domain}"
                            let local = item.jid.split('@').next().unwrap_or(item.jid.as_str());
                            let channel_id =
                                local.splitn(2, '_').nth(1).unwrap_or(local).to_string();
                            WaddleDiscoveredChannel {
                                id: channel_id,
                                name: item.name.unwrap_or_default(),
                                channel_type: "text".to_string(),
                                position: 0,
                            }
                        })
                        .collect(),
                    Err(e) => {
                        drop(guard);
                        self.listener
                            .on_error(format!("discover_channels failed: {e}"));
                        vec![]
                    }
                }
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
}

// ── Event dispatch ───────────────────────────────────────────────────────────

fn dispatch_event(event: ClientEvent, listener: &dyn WaddleEventListener) {
    match event {
        ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_)) => listener.on_connected(),
        ClientEvent::Messaging(MessagingEvent::Message(msg)) => {
            listener.on_message(inbound_to_ffi(msg));
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
    jid.split('@').last().unwrap_or(jid)
}

fn mam_page_to_ffi(page: waddle_xmpp_client::mam::MamPage) -> WaddleMamPage {
    WaddleMamPage {
        messages: page.messages.into_iter().map(archived_to_ffi).collect(),
        first_id: page.rsm.first,
        last_id: page.rsm.last,
        is_complete: page.is_complete,
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
        is_muc,
        thread: msg.thread_id.or(msg.thread),
        parent_thread_id: msg.parent_thread_id,
        reply_to_id: msg.reply_to_id,
        reply_to_sender: msg.reply_to_sender,
        reply_fallback_start: fb_start,
        reply_fallback_end: fb_end,
    }
}

/// Convert an archived MAM message into the UniFFI record. Re-parses the
/// wrapped inner element through the full messaging parser so that replies,
/// fallback ranges, and nested-thread parents survive history loads.
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
        thread: archived.thread,
        parent_thread_id: parsed.as_ref().and_then(|m| m.parent_thread_id.clone()),
        reply_to_id: parsed.as_ref().and_then(|m| m.reply_to_id.clone()),
        reply_to_sender: parsed.as_ref().and_then(|m| m.reply_to_sender.clone()),
        reply_fallback_start: fb_start,
        reply_fallback_end: fb_end,
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

    Ok(SendMessageOptions {
        reply,
        fallback,
        thread,
    })
}
