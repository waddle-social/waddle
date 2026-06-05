//! UniFFI FFI bridge exposing the Waddle XMPP client to Swift.

uniffi::setup_scaffolding!("waddle_xmpp_client");

use std::sync::Arc;

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use tokio::sync::Mutex;
use url::Url;

mod convert;
mod types;

pub use types::*;

use convert::{
    dispatch_event, empty_mam_page, empty_topology, jid_domain, jingle_reason_from_ffi,
    mam_page_to_ffi, send_options_from_ffi, topology_to_ffi, upload_slot_to_ffi,
};
use waddle_xmpp_client::{
    avatar::AvatarExt,
    discovery::DiscoveryExt,
    mam::MamExt,
    messaging::{
        build_finish, build_finish_migrated, build_proceed, build_propose, build_reject,
        build_reject_with_options, build_retract, build_retract_with_options, build_session_accept,
        build_session_initiate, build_session_terminate, CallMedia, JingleReason, MessagingExt,
        SessionId, NS_CLIENT,
    },
    AccessToken, ClientConfig, ClientHandle, ConnectionConfig, OAuthBearerConfig, WebSocketConfig,
};
use waddle_xmpp_client::{ClientError, ClientResource, XmppClient};

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
        let account_bare_jid = self.config.jid.split('/').next().unwrap_or("").to_string();
        let listener = Arc::clone(&self.listener);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => dispatch_event(event, &account_bare_jid, &**listener),
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
    ) -> WaddleSendMessageOutcome {
        let room_jid = match room_jid.parse::<BareJid>() {
            Ok(jid) => jid,
            Err(e) => {
                self.listener.on_error(format!(
                    "send_groupchat_message failed: invalid room JID: {e}"
                ));
                return WaddleSendMessageOutcome::InvalidRecipient;
            }
        };
        let opts = match options.map(send_options_from_ffi).transpose() {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => {
                self.listener
                    .on_error(format!("send_groupchat_message failed: {e}"));
                return WaddleSendMessageOutcome::InvalidOptions;
            }
        };
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                WaddleSendMessageOutcome::NotConnected
            }
            Some(h) => {
                let result = h
                    .send_groupchat_message(room_jid.as_str(), &body, &opts)
                    .await;
                drop(guard);
                match result {
                    Ok(stanza_id) => WaddleSendMessageOutcome::Sent {
                        stanza_id: stanza_id.to_string(),
                    },
                    Err(e) => {
                        self.listener
                            .on_error(format!("send_groupchat_message failed: {e}"));
                        send_failure_outcome(&e)
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
    ) -> WaddleSendMessageOutcome {
        let peer_jid = match peer_jid.parse::<Jid>() {
            Ok(jid) => jid,
            Err(e) => {
                self.listener
                    .on_error(format!("send_chat_message failed: invalid peer JID: {e}"));
                return WaddleSendMessageOutcome::InvalidRecipient;
            }
        };
        let opts = match options.map(send_options_from_ffi).transpose() {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => {
                self.listener
                    .on_error(format!("send_chat_message failed: {e}"));
                return WaddleSendMessageOutcome::InvalidOptions;
            }
        };
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                WaddleSendMessageOutcome::NotConnected
            }
            Some(h) => {
                let result = h.send_chat_message(peer_jid.as_str(), &body, &opts).await;
                drop(guard);
                match result {
                    Ok(stanza_id) => WaddleSendMessageOutcome::Sent {
                        stanza_id: stanza_id.to_string(),
                    },
                    Err(e) => {
                        self.listener
                            .on_error(format!("send_chat_message failed: {e}"));
                        send_failure_outcome(&e)
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
        let Some(handle) = self.clone_handle().await else {
            return empty_topology();
        };
        // `discover_topology` now resolves the MUC and Spaces service
        // JIDs from the server domain itself (with conventional
        // subdomain fallbacks), enumerates rooms via *both* the
        // Spaces bookmarks and the MUC component directly, and
        // attaches non-bookmarked rooms to a synthetic "standalone"
        // space. So a fresh waddle deployment with no XEP-0503 space
        // bookmarks still produces a usable channel list.
        let server_domain = jid_domain(&self.config.jid);

        match handle.discover_topology(server_domain).await {
            Ok(topology) => topology_to_ffi(topology),
            Err(e) => {
                self.listener.on_error(format!(
                    "discover_topology failed: server_domain={server_domain} error={e}"
                ));
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

    // ── Push notifications (XEP-0357 + XEP-0050) ─────────────────────

    /// XEP-0357 §5 `<enable/>` IQ against the user's XMPP server.
    /// Never carries provider credentials — those flow through
    /// `register_push_device` (XEP-0050) at `push.<domain>`.
    pub async fn enable_push_notifications(&self, push_service_jid: String, node: String) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle
            .enable_push_notifications(&push_service_jid, &node, None)
            .await
        {
            Ok(()) => true,
            Err(e) => {
                self.listener
                    .on_error(format!("enable_push_notifications failed: {e}"));
                false
            }
        }
    }

    /// XEP-0357 §6.1 `<disable/>` IQ. A `None`/missing `node` disables
    /// ALL push nodes at the service for this user.
    pub async fn disable_push_notifications(
        &self,
        push_service_jid: String,
        node: Option<String>,
    ) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle
            .disable_push_notifications(&push_service_jid, node.as_deref())
            .await
        {
            Ok(()) => true,
            Err(e) => {
                self.listener
                    .on_error(format!("disable_push_notifications failed: {e}"));
                false
            }
        }
    }

    /// XEP-0050 `register-device` ad-hoc command on `push.<domain>`.
    /// Drives the multi-step dance and returns the assigned
    /// [`WaddleRegisterDeviceResult`] (node id + device id) on
    /// success. Returns `None` on failure with the diagnostic on the
    /// listener. The caller MUST persist both fields — node feeds
    /// the user-server XEP-0357 `<enable/>` IQ, device id scopes the
    /// per-device `disable_push_device` opt-out.
    pub async fn register_push_device(
        &self,
        push_service_jid: String,
        app_id: String,
        environment: WaddlePushEnvironment,
        credentials: WaddlePushDeviceCredentials,
    ) -> Option<WaddleRegisterDeviceResult> {
        let handle = self.clone_handle().await?;
        let env: waddle_xmpp_client::push::PushEnvironment = environment.into();
        let creds: waddle_xmpp_client::push::PushDeviceCredentials = credentials.into();
        match waddle_xmpp_client::push::register_push_device(
            &handle,
            &push_service_jid,
            &app_id,
            env,
            &creds,
        )
        .await
        {
            Ok(outcome) => Some(WaddleRegisterDeviceResult {
                node: outcome.node.into_string(),
                device_id: outcome.device_id.into_string(),
            }),
            Err(e) => {
                self.listener
                    .on_error(format!("register_push_device failed: {e}"));
                None
            }
        }
    }

    /// XEP-0050 `disable-device` ad-hoc command on `push.<domain>`.
    /// Per-device scope — `device_id` is the value returned by the
    /// preceding [`register_push_device`] call. Sibling devices on
    /// the same node keep receiving fan-out. Returns `true` when the
    /// command completes (including the idempotent already-disabled
    /// case).
    pub async fn disable_push_device(
        &self,
        push_service_jid: String,
        node: String,
        device_id: String,
    ) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        let form = waddle_xmpp_client::push::build_disable_device_submit_form(&node, &device_id);
        let iq = waddle_xmpp_client::xep::xep0050::build_xep0050_command_request(
            &push_service_jid,
            waddle_xmpp_client::push::DISABLE_DEVICE_NODE,
            waddle_xmpp_client::xep::xep0050::AdHocAction::Execute,
            Some(form),
        );
        match handle.send_iq(iq).await {
            Ok(_) => true,
            Err(e) => {
                self.listener
                    .on_error(format!("disable_push_device failed: {e}"));
                false
            }
        }
    }

    // ── A/V calls (XEP-0353 + XEP-0166) ──────────────────────────────

    /// Send a XEP-0353 §5.1.1 `<propose/>` to the peer's bare JID.
    /// The bare JID lets the responder's server ring every connected
    /// resource until one of them proceeds or rejects.
    pub async fn send_call_propose(
        &self,
        peer_bare_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> bool {
        let Some(peer) = self.parse_bare_jid(&peer_bare_jid, "send_call_propose") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_propose") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_propose(&sid, CallMedia { audio, video }),
        );
        self.send_stanza_or_error(stanza, "send_call_propose").await
    }

    /// Send a XEP-0353 §5.1.2 `<proceed/>` to the *full* JID of the
    /// originator (preserved from the propose `from` per §0.6).
    pub async fn send_call_proceed(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_proceed") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_proceed") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_proceed(&sid));
        self.send_stanza_or_error(stanza, "send_call_proceed").await
    }

    /// Send a XEP-0353 §5.1.3 `<reject/>` to the originator's full JID.
    pub async fn send_call_reject(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_reject") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_reject") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_reject(&sid));
        self.send_stanza_or_error(stanza, "send_call_reject").await
    }

    /// Send a XEP-0353 tie-break `<reject/>` carrying
    /// `<reason><expired/></reason>` plus `<tie-break/>`.
    pub async fn send_call_reject_tie_break(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_reject_tie_break") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_reject_tie_break") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_reject_with_options(&sid, Some(JingleReason::Expired), true),
        );
        self.send_stanza_or_error(stanza, "send_call_reject_tie_break")
            .await
    }

    /// Send a XEP-0353 §5.1.4 `<retract/>` to cancel a ringing call
    /// before the peer answers. Addressed to the responder's *bare*
    /// JID so every resource that may have been ringing receives the
    /// cancellation (XEP-0353 §5.1.4: a retract is addressed to the
    /// callee's bare JID, exactly like the originating propose).
    pub async fn send_call_retract(&self, peer_bare_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_bare_jid(&peer_bare_jid, "send_call_retract") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_retract") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_retract(&sid));
        self.send_stanza_or_error(stanza, "send_call_retract").await
    }

    /// Send a XEP-0353 tie-break `<retract/>` carrying
    /// `<reason><expired/></reason>` plus `<tie-break/>`.
    pub async fn send_call_retract_tie_break(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_retract_tie_break") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_retract_tie_break") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_retract_with_options(&sid, Some(JingleReason::Expired), true),
        );
        self.send_stanza_or_error(stanza, "send_call_retract_tie_break")
            .await
    }

    /// Send a `<finish/>` Waddle JMI extension signaling clean
    /// teardown after a call ended. Addressed to the peer's full JID
    /// so the originating resource sees the finish notice.
    pub async fn send_call_finish(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_finish") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_finish") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_finish(&sid));
        self.send_stanza_or_error(stanza, "send_call_finish").await
    }

    /// Send Waddle's XEP-0353-compatible migration marker:
    /// `<finish/>` with `<reason><expired/></reason>` and
    /// `<migrated to='new-sid'/>`.
    pub async fn send_call_finish_migrated(
        &self,
        peer_full_jid: String,
        old_sid: String,
        new_sid: String,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_finish_migrated") else {
            return false;
        };
        let Some(old_sid) = self.parse_session_id(old_sid, "send_call_finish_migrated") else {
            return false;
        };
        let Some(new_sid) = self.parse_session_id(new_sid, "send_call_finish_migrated") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_finish_migrated(&old_sid, JingleReason::Expired, &new_sid),
        );
        self.send_stanza_or_error(stanza, "send_call_finish_migrated")
            .await
    }

    /// Send a XEP-0166 §6.4 `session-initiate` IQ to the peer's full
    /// JID. `initiator_full_jid` names the call originator per §7.1;
    /// the server's Jingle handler additionally validates that the
    /// authenticated session matches. Validating both JIDs as
    /// `FullJid` at the FFI boundary surfaces a clear error rather
    /// than letting a malformed stanza hit the wire.
    pub async fn send_call_session_initiate(
        &self,
        peer_full_jid: String,
        initiator_full_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_session_initiate") else {
            return false;
        };
        let Some(initiator) =
            self.parse_full_jid(&initiator_full_jid, "send_call_session_initiate")
        else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_session_initiate") else {
            return false;
        };
        let payload = build_session_initiate(&sid, &initiator, CallMedia { audio, video });
        let iq = iq_set(&peer.into(), payload);
        self.send_iq_or_error(iq, "send_call_session_initiate")
            .await
    }

    /// Send a XEP-0166 §7.2 `session-accept` IQ. `responder` is
    /// validated as a full JID at the FFI
    /// boundary so a malformed JID surfaces as an error before the
    /// stanza hits the wire.
    pub async fn send_call_session_accept(
        &self,
        peer_full_jid: String,
        responder_full_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_session_accept") else {
            return false;
        };
        let Some(responder) = self.parse_full_jid(&responder_full_jid, "send_call_session_accept")
        else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_session_accept") else {
            return false;
        };
        let payload = build_session_accept(&sid, &responder, CallMedia { audio, video });
        let iq = iq_set(&peer.into(), payload);
        self.send_iq_or_error(iq, "send_call_session_accept").await
    }

    /// Send a XEP-0166 §7.4 `session-terminate` IQ. `reason` is the
    /// typed XEP-0166 condition (the FFI rejects unknown values at
    /// the Swift boundary by virtue of `reason` being a UniFFI enum
    /// — there is no way to express an unsupported condition in
    /// Swift, so the wire can't carry one either).
    pub async fn send_call_session_terminate(
        &self,
        peer_full_jid: String,
        sid: String,
        reason: Option<WaddleJingleReason>,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_session_terminate") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_session_terminate") else {
            return false;
        };
        let typed_reason = reason.map(jingle_reason_from_ffi);
        let payload = build_session_terminate(&sid, typed_reason);
        let iq = iq_set(&peer.into(), payload);
        self.send_iq_or_error(iq, "send_call_session_terminate")
            .await
    }
}

// ── Send helpers ────────────────────────────────────────────────────────────

impl WaddleClient {
    /// Send a built stanza fire-and-forget. Returns `true` on success.
    /// On failure (not connected, transport closed) emits an
    /// `on_error` and returns `false` so callers can short-circuit.
    ///
    /// Holds the client mutex only long enough to clone the
    /// [`ClientHandle`] (cheap — it is `Arc`-backed) and releases
    /// before awaiting the underlying send. Holding across the
    /// `send_stanza` await would be safe today (it returns once the
    /// stanza hits the command channel) but matches the same shape
    /// used by `send_iq_or_error`, which *must* release because
    /// `send_iq` awaits the correlated server reply.
    async fn send_stanza_or_error(&self, stanza: Element, op: &'static str) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle.send_stanza(stanza).await {
            Ok(()) => true,
            Err(e) => {
                self.listener.on_error(format!("{op} failed: {e}"));
                false
            }
        }
    }

    /// Send a built IQ and await its correlated result. Returns
    /// `true` on `<iq type='result'>`, `false` on `<iq type='error'>`
    /// or transport failure (with an `on_error` for both).
    ///
    /// Clones the [`ClientHandle`] under the mutex and **drops the
    /// guard before awaiting** the IQ response. `send_iq` blocks on
    /// a `oneshot::Receiver` until the server replies — holding the
    /// mutex across that wait would serialise every concurrent
    /// FFI call against the slowest in-flight IQ.
    async fn send_iq_or_error(&self, stanza: Element, op: &'static str) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle.send_iq(stanza).await {
            Ok(_) => true,
            Err(e) => {
                self.listener.on_error(format!("{op} failed: {e}"));
                false
            }
        }
    }

    /// Take a cheap [`ClientHandle`] clone under the mutex, dropping
    /// the guard before returning so callers can await freely.
    async fn clone_handle(&self) -> Option<ClientHandle> {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.listener.on_error("Not connected".to_string());
                None
            }
            Some(h) => {
                let handle = h.clone();
                drop(guard);
                Some(handle)
            }
        }
    }

    fn parse_full_jid(&self, value: &str, op: &'static str) -> Option<FullJid> {
        match value.parse::<FullJid>() {
            Ok(j) => Some(j),
            Err(e) => {
                self.listener
                    .on_error(format!("{op} failed: invalid full JID '{value}': {e}"));
                None
            }
        }
    }

    fn parse_bare_jid(&self, value: &str, op: &'static str) -> Option<BareJid> {
        match value.parse::<BareJid>() {
            Ok(j) => Some(j),
            Err(e) => {
                self.listener
                    .on_error(format!("{op} failed: invalid bare JID '{value}': {e}"));
                None
            }
        }
    }

    /// Wrap a caller-supplied id in [`SessionId`] after rejecting
    /// empty or whitespace-only inputs. XEP-0166 §7 uses `sid` as
    /// the correlation key for every later stanza in the call; a
    /// whitespace-only string passes the empty check but never
    /// matches anything on the wire, leaving the call UI in a
    /// stuck state. `SessionId(String)` is a thin newtype with no
    /// runtime check, so this method is the gate.
    fn parse_session_id(&self, value: String, op: &'static str) -> Option<SessionId> {
        if value.trim().is_empty() {
            self.listener.on_error(format!(
                "{op} failed: session id must be a non-empty, non-whitespace string"
            ));
            return None;
        }
        Some(SessionId(value))
    }
}

// ── XML envelopes ───────────────────────────────────────────────────────────

/// Wrap a JMI payload in a `<message to='...'/>` envelope.
/// The destination is taken as a typed [`Jid`] so the caller has
/// already validated the JID at the FFI entry point; the `to`
/// attribute is rendered from the typed value.
fn message_with_jmi(to: &Jid, jmi: Element) -> Element {
    Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.to_string())
        .append(jmi)
        .build()
}

/// Wrap a Jingle payload in an `<iq type='set' id='...' to='...'/>`
/// envelope. A v4 UUID id is minted so the correlator in
/// [`ClientHandle::send_iq`] can route the result. `to` is a typed
/// [`Jid`] — see [`message_with_jmi`].
fn iq_set(to: &Jid, payload: Element) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            uuid::Uuid::new_v4().to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.to_string())
        .append(payload)
        .build()
}

fn send_failure_outcome(error: &ClientError) -> WaddleSendMessageOutcome {
    match error {
        ClientError::Disconnected => WaddleSendMessageOutcome::NotConnected,
        ClientError::EmptyStanzaId => WaddleSendMessageOutcome::InvalidOptions,
        ClientError::StanzaError(_) => WaddleSendMessageOutcome::StanzaError,
        ClientError::TransportClosed
        | ClientError::EmptyTransportFrame
        | ClientError::TransportFrameTooLarge { .. }
        | ClientError::InvalidTransportFrame
        | ClientError::UnsupportedWebSocketMessage => WaddleSendMessageOutcome::TransportError,
        _ => WaddleSendMessageOutcome::Error,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingListener {
        errors: Arc<StdMutex<Vec<String>>>,
    }

    impl RecordingListener {
        fn errors(&self) -> Vec<String> {
            self.errors.lock().unwrap().clone()
        }
    }

    impl WaddleEventListener for RecordingListener {
        fn on_message(&self, _message: WaddleMessage) {}

        fn on_presence(&self, _presence: WaddlePresence) {}

        fn on_mam_result(&self, _message: WaddleArchivedMessage) {}

        fn on_message_delivery_acked(&self, _stanza_id: String) {}

        fn on_message_delivery_failed(&self, _stanza_id: String) {}

        fn on_connected(&self) {}

        fn on_disconnected(&self) {}

        fn on_error(&self, description: String) {
            self.errors.lock().unwrap().push(description);
        }

        fn on_call(&self, _event: WaddleCallEvent) {}
    }

    fn test_client(listener: RecordingListener) -> Arc<WaddleClient> {
        Arc::new(WaddleClient {
            config: WaddleConfig {
                server_url: "wss://xmpp.waddle.test".to_string(),
                jid: "alice@waddle.test".to_string(),
                access_token: "token".to_string(),
                resource: "test".to_string(),
            },
            listener: Arc::new(Box::new(listener) as Box<dyn WaddleEventListener>),
            handle: tokio::sync::Mutex::new(None),
        })
    }

    fn invalid_send_options() -> WaddleSendOptions {
        WaddleSendOptions {
            shared_files: vec![WaddleSharedFile {
                url: "https://cdn.waddle.test/file.enc".to_string(),
                name: None,
                media_type: None,
                size: None,
                width: None,
                height: None,
                disposition: "attachment".to_string(),
                encrypted: Some(WaddleEncryptedFile {
                    cipher: "urn:waddle:not-a-cipher".to_string(),
                    key_b64: "key".to_string(),
                    iv_b64: "iv".to_string(),
                    hashes: Vec::new(),
                    sources: vec!["https://cdn.waddle.test/file.enc".to_string()],
                }),
            }],
            ..WaddleSendOptions::default()
        }
    }

    #[tokio::test]
    async fn send_chat_message_reports_not_connected_as_typed_outcome() {
        let listener = Arc::new(RecordingListener::default());
        let client = test_client((*listener).clone());

        let outcome = client
            .send_chat_message("bob@waddle.test".to_string(), "hello".to_string(), None)
            .await;

        assert_eq!(outcome, WaddleSendMessageOutcome::NotConnected);
        assert_eq!(listener.errors(), vec!["Not connected"]);
    }

    #[tokio::test]
    async fn send_groupchat_message_reports_invalid_options_as_typed_outcome() {
        let listener = Arc::new(RecordingListener::default());
        let client = test_client((*listener).clone());

        let outcome = client
            .send_groupchat_message(
                "room@muc.waddle.test".to_string(),
                "hello".to_string(),
                Some(invalid_send_options()),
            )
            .await;

        assert_eq!(outcome, WaddleSendMessageOutcome::InvalidOptions);
        assert!(
            listener
                .errors()
                .first()
                .is_some_and(|error| error.contains("unknown cipher")),
            "expected invalid cipher diagnostic"
        );
    }

    #[tokio::test]
    async fn send_chat_message_reports_invalid_recipient_as_typed_outcome() {
        let listener = Arc::new(RecordingListener::default());
        let client = test_client((*listener).clone());

        let outcome = client
            .send_chat_message("not a jid".to_string(), "hello".to_string(), None)
            .await;

        assert_eq!(outcome, WaddleSendMessageOutcome::InvalidRecipient);
        assert!(
            listener
                .errors()
                .first()
                .is_some_and(|error| error.contains("invalid peer JID")),
            "expected invalid peer JID diagnostic"
        );
    }

    #[tokio::test]
    async fn send_groupchat_message_reports_invalid_recipient_as_typed_outcome() {
        let listener = Arc::new(RecordingListener::default());
        let client = test_client((*listener).clone());

        let outcome = client
            .send_groupchat_message("not a room".to_string(), "hello".to_string(), None)
            .await;

        assert_eq!(outcome, WaddleSendMessageOutcome::InvalidRecipient);
        assert!(
            listener
                .errors()
                .first()
                .is_some_and(|error| error.contains("invalid room JID")),
            "expected invalid room JID diagnostic"
        );
    }
}
