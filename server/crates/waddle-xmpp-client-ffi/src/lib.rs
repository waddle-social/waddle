//! UniFFI FFI bridge exposing the Waddle XMPP client to Swift.

uniffi::setup_scaffolding!("waddle_xmpp_client");

use std::sync::Arc;

use jid::BareJid;
use tokio::sync::Mutex;
use url::Url;

mod convert;
mod types;

pub use types::*;

use convert::{
    dispatch_event, empty_mam_page, empty_topology, jid_domain, mam_page_to_ffi,
    send_options_from_ffi, topology_to_ffi, upload_slot_to_ffi,
};
use waddle_xmpp_client::{
    avatar::AvatarExt, discovery::DiscoveryExt, mam::MamExt, messaging::MessagingExt, AccessToken,
    ClientConfig, ClientHandle, ConnectionConfig, OAuthBearerConfig, WebSocketConfig,
};
use waddle_xmpp_client::{ClientResource, XmppClient};

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
    ) -> String {
        let opts = match options.map(send_options_from_ffi).transpose() {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => {
                self.listener.on_error(e);
                return String::new();
            }
        };
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                String::new()
            }
            Some(h) => {
                let result = h.send_groupchat_message(&room_jid, &body, &opts).await;
                drop(guard);
                match result {
                    Ok(stanza_id) => stanza_id.to_string(),
                    Err(e) => {
                        self.listener
                            .on_error(format!("send_groupchat_message failed: {e}"));
                        String::new()
                    }
                }
            }
        }
    }

    pub async fn send_chat_message(
        &self,
        peer_jid: String,
        body: String,
        options: Option<WaddleSendOptions>,
    ) -> String {
        let opts = match options.map(send_options_from_ffi).transpose() {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => {
                self.listener.on_error(e);
                return String::new();
            }
        };
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                String::new()
            }
            Some(h) => {
                let result = h.send_chat_message(&peer_jid, &body, &opts).await;
                drop(guard);
                match result {
                    Ok(stanza_id) => stanza_id.to_string(),
                    Err(e) => {
                        self.listener
                            .on_error(format!("send_chat_message failed: {e}"));
                        String::new()
                    }
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

    pub async fn discover_topology(&self) -> WaddleTopology {
        let guard = self.handle.lock().await;
        let Some(h) = guard.as_ref() else {
            drop(guard);
            self.listener.on_error("Not connected".to_string());
            return empty_topology();
        };

        let spaces_domain = format!("spaces.{}", jid_domain(&self.config.jid));
        let spaces_jid: BareJid = match spaces_domain.parse() {
            Ok(jid) => jid,
            Err(e) => {
                drop(guard);
                self.listener
                    .on_error(format!("discover_topology: invalid spaces JID: {e}"));
                return empty_topology();
            }
        };

        match h.discover_topology(&spaces_jid).await {
            Ok(topology) => topology_to_ffi(topology),
            Err(e) => {
                drop(guard);
                self.listener
                    .on_error(format!("discover_topology failed: {e}"));
                empty_topology()
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
                    url: avatar.url,
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
