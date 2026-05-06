use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use futures::channel::{mpsc, oneshot};
use futures::{pin_mut, select, FutureExt, SinkExt, StreamExt};
use jid::{BareJid, Jid};
use js_sys::{Function, Promise};
use minidom::Element;
use serde::{Deserialize, Serialize};
use waddle_xmpp_client::avatar::{request_avatar_with_iq, AvatarRequestFailure};
use waddle_xmpp_client::discovery::{
    self, build_disable_push_iq, build_disco_info_iq, build_disco_items_iq, build_enable_push_iq,
    build_muc_admin_affiliation_list_iq, build_muc_admin_affiliation_set_iq, build_roster_get_iq,
    build_upload_slot_iq, build_user_search_iq, build_waddle_inbox_mark_read_iq,
    build_waddle_inbox_query_iq, parse_muc_admin_affiliation_query, parse_roster_result,
    parse_user_search_result, parse_waddle_inbox_result, MucAdminAffiliationItem, UserSearchQuery,
    WaddleInboxMarkRead, WaddleInboxQuery,
};
use waddle_xmpp_client::error::parse_stanza_error;
use waddle_xmpp_client::mam::{self, build_mam_iq, build_mam_iq_extended};
use waddle_xmpp_client::messaging::{
    self, build_chat_state_message, build_correction_message, build_displayed_message,
    build_moderation_message, build_outbound_message, build_reaction_message,
    build_retraction_message, InboundMessage, InboundPresence, MarkupSpanData, MarkupSpanType,
    MucAffiliation, MucRole, ReferenceData, SendMessageOptions, SharedFileDisposition,
};
use waddle_xmpp_client::pep::{
    build_pep_items_iq, build_publish_activity_iq, build_publish_mood_iq, build_publish_tune_iq,
    build_retract_activity_iq, build_retract_mood_iq, build_retract_tune_iq, parse_pep_activity,
    parse_pep_mood, parse_pep_tune,
};
use waddle_xmpp_client::transport::{
    StreamClose, TransportEvent, TransportMessage, TransportState,
};
use waddle_xmpp_client::xep::{
    reply::{FallbackRange, ReplyMarker},
    thread::ThreadRef,
};
use waddle_xmpp_client::{
    AccessToken, ArchivedMessage, ClientConfig, ClientError, ClientEvent, ClientRequest,
    ClientResource, ConnectionConfig, ConnectionEvent, LifecycleEvent, MessageDeliveryEvent,
    OAuthBearerConfig, StanzaId, StreamManagementEvent, WasmTransportEvent, WasmWebSocket,
    WebSocketConfig, XmppRuntime,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};

const NS_CLIENT: &str = "jabber:client";
const NS_CHAT_STATES: &str = "http://jabber.org/protocol/chatstates";
const NS_CHAT_MARKERS: &str = "urn:xmpp:chat-markers:0";
const NS_REACTIONS: &str = "urn:xmpp:reactions:0";
const NS_RETRACT: &str = "urn:xmpp:message-retract:0";
const NS_REPLACE: &str = "urn:xmpp:message-correct:0";
const NS_FASTEN: &str = "urn:xmpp:fasten:0";
const NS_MODERATE: &str = "urn:xmpp:message-moderate:0";
const NS_HINTS: &str = "urn:xmpp:hints";
const NS_ROSTER: &str = "jabber:iq:roster";
const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const NS_VERSION: &str = "jabber:iq:version";
const NS_USER_SEARCH: &str = "jabber:iq:search";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_ADHOC_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_WADDLE_EXTENSION_1: &str = "urn:waddle:extension:1";
const EXTENSION_ROUTE_FORM_TYPE: &str = "urn:waddle:extension:1:routes";
const EXTENSION_ROUTE_ITEM_LIMIT: u32 = 100;

type DriverResult<T> = Result<T, ClientError>;

#[wasm_bindgen]
pub struct WaddleConfig {
    server_url: String,
    jid: String,
    access_token: String,
    resource: String,
}

#[wasm_bindgen]
impl WaddleConfig {
    #[wasm_bindgen(constructor)]
    pub fn new(
        server_url: String,
        jid: String,
        access_token: String,
        resource: String,
    ) -> WaddleConfig {
        WaddleConfig {
            server_url,
            jid,
            access_token,
            resource,
        }
    }
}

#[derive(Clone)]
struct StoredConfig {
    server_url: String,
    jid: String,
    access_token: String,
    resource: String,
}

impl From<&WaddleConfig> for StoredConfig {
    fn from(value: &WaddleConfig) -> Self {
        Self {
            server_url: value.server_url.clone(),
            jid: value.jid.clone(),
            access_token: value.access_token.clone(),
            resource: value.resource.clone(),
        }
    }
}

#[wasm_bindgen]
pub struct WaddleClient {
    inner: Rc<RefCell<WaddleClientInner>>,
}

struct WaddleClientInner {
    config: StoredConfig,
    cmd_tx: Option<mpsc::Sender<WasmCommand>>,
    on_message: Option<Function>,
    on_presence: Option<Function>,
    on_connected: Option<Function>,
    on_session_lifecycle: Option<Function>,
    on_disconnected: Option<Function>,
    on_error: Option<Function>,
    on_message_delivery_acked: Option<Function>,
    on_message_delivery_failed: Option<Function>,
}

enum WasmCommand {
    SendStanza {
        stanza: Element,
        responder: oneshot::Sender<DriverResult<()>>,
    },
    SendIq {
        stanza: Element,
        responder: oneshot::Sender<DriverResult<Element>>,
    },
    SendMamQuery {
        stanza: Element,
        query_id: String,
        responder: oneshot::Sender<DriverResult<waddle_xmpp_client::MamPage>>,
    },
    Disconnect {
        responder: oneshot::Sender<DriverResult<()>>,
    },
}

enum DriverEvent {
    Client(Box<ClientEvent>),
    Error(String),
    Disconnected,
}

fn client_driver_event(event: ClientEvent) -> DriverEvent {
    DriverEvent::Client(Box::new(event))
}

struct PendingMamQuery {
    query_id: String,
    messages: Vec<ArchivedMessage>,
    responder: oneshot::Sender<DriverResult<waddle_xmpp_client::MamPage>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedMessageDelivery {
    stanza_id: StanzaId,
    h: u32,
}

struct WasmDriverTask {
    runtime: XmppRuntime,
    ws: WasmWebSocket,
    cmd_rx: mpsc::Receiver<WasmCommand>,
    event_tx: mpsc::Sender<DriverEvent>,
    pending_iqs: HashMap<String, oneshot::Sender<DriverResult<Element>>>,
    pending_mam_queries: HashMap<String, PendingMamQuery>,
    sm_delivery_tracking_enabled: bool,
    outbound_h: u32,
    pending_message_deliveries: VecDeque<TrackedMessageDelivery>,
}

#[derive(Debug, Serialize)]
pub struct WaddleMarkupSpan {
    pub span_type: String,
    pub start: usize,
    pub end: usize,
    pub uri: Option<String>,
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
    pub origin_id: Option<String>,
    pub replaces_id: Option<String>,
    pub retracts_id: Option<String>,
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
    pub forum_post_kind: Option<String>,
    pub forum_title: Option<String>,
    pub forum_thread_title: Option<String>,
    pub is_sticker: bool,
    pub shared_files: Vec<WaddleSharedFile>,
}

#[derive(Debug, Serialize)]
pub struct WaddleArchivedMessage {
    pub mam_id: String,
    pub query_id: Option<String>,
    pub stanza_id: Option<String>,
    pub timestamp: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub message_type: String,
    pub body: Option<String>,
    pub subject: Option<String>,
    pub replaces_id: Option<String>,
    pub retracts_id: Option<String>,
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
    pub forum_post_kind: Option<String>,
    pub forum_title: Option<String>,
    pub forum_thread_title: Option<String>,
    pub is_sticker: bool,
    pub author_real_jid: Option<String>,
    pub shared_files: Vec<WaddleSharedFile>,
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
    pub total_unread: u32,
    pub conversations: Vec<WaddleInboxConversation>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaddleFetchInboxOptions {
    pub since: Option<i64>,
    pub only_unread: bool,
    pub room: Option<String>,
    pub threads: bool,
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

#[derive(Debug, Deserialize)]
pub struct WaddleMamPageParam {
    #[serde(rename = "type")]
    pub kind: String,
    pub before: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WaddleReference {
    pub ref_type: String,
    pub uri: String,
    pub begin: u32,
    pub end: u32,
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

#[wasm_bindgen]
impl WaddleClient {
    #[wasm_bindgen(constructor)]
    pub fn new(config: WaddleConfig) -> WaddleClient {
        WaddleClient {
            inner: Rc::new(RefCell::new(WaddleClientInner {
                config: StoredConfig::from(&config),
                cmd_tx: None,
                on_message: None,
                on_presence: None,
                on_connected: None,
                on_session_lifecycle: None,
                on_disconnected: None,
                on_error: None,
                on_message_delivery_acked: None,
                on_message_delivery_failed: None,
            })),
        }
    }

    pub fn set_on_message(&mut self, cb: Function) {
        self.inner.borrow_mut().on_message = Some(cb);
    }

    pub fn set_on_presence(&mut self, cb: Function) {
        self.inner.borrow_mut().on_presence = Some(cb);
    }

    pub fn set_on_connected(&mut self, cb: Function) {
        self.inner.borrow_mut().on_connected = Some(cb);
    }

    pub fn set_on_session_lifecycle(&mut self, cb: Function) {
        self.inner.borrow_mut().on_session_lifecycle = Some(cb);
    }

    pub fn set_on_disconnected(&mut self, cb: Function) {
        self.inner.borrow_mut().on_disconnected = Some(cb);
    }

    pub fn set_on_error(&mut self, cb: Function) {
        self.inner.borrow_mut().on_error = Some(cb);
    }

    pub fn set_on_message_delivery_acked(&mut self, cb: Function) {
        self.inner.borrow_mut().on_message_delivery_acked = Some(cb);
    }

    pub fn set_on_message_delivery_failed(&mut self, cb: Function) {
        self.inner.borrow_mut().on_message_delivery_failed = Some(cb);
    }

    pub fn connect(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            if inner.borrow().cmd_tx.is_some() {
                return Err(js_error("client is already connected"));
            }

            let stored = inner.borrow().config.clone();
            let config = build_client_config(&stored)?;
            let ws = WasmWebSocket::connect(config.transport.endpoint.as_str())
                .map_err(|err| js_error(format!("failed to open websocket: {:?}", err)))?;
            let (cmd_tx, cmd_rx) = mpsc::channel(64);
            let (event_tx, event_rx) = mpsc::channel(256);

            inner.borrow_mut().cmd_tx = Some(cmd_tx);

            spawn_local(event_dispatch_loop(inner.clone(), event_rx));
            spawn_local(driver_loop(config, ws, cmd_rx, event_tx));

            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn disconnect(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            disconnect_client(inner).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_chat_message(&self, peer_jid: String, body: String, options: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts = send_options_from_js(options)?;
            let (stanza_id, stanza) = build_outbound_message(&peer_jid, "chat", &body, &opts)
                .map_err(|err| js_error(err.to_string()))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::from_str(stanza_id.as_str()))
        })
    }

    pub fn send_groupchat_message(
        &self,
        room_jid: String,
        body: String,
        options: JsValue,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts = send_options_from_js(options)?;
            let (stanza_id, stanza) = build_outbound_message(&room_jid, "groupchat", &body, &opts)
                .map_err(|err| js_error(err.to_string()))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::from_str(stanza_id.as_str()))
        })
    }

    pub fn send_chat_state(&self, to: String, msg_type: String, state: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_CHAT_STATES;
            let stanza = build_chat_state_message(&to, &state, &msg_type)
                .map_err(|err| js_error(err.to_string()))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_displayed(&self, to: String, msg_type: String, message_id: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_CHAT_MARKERS;
            let stanza = build_displayed_message(&to, &message_id, &msg_type);
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_reaction(
        &self,
        to: String,
        msg_type: String,
        target_id: String,
        emojis: Vec<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = (NS_REACTIONS, NS_HINTS);
            let stanza = build_reaction_message(&to, &msg_type, &target_id, &emojis);
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_retraction(&self, to: String, msg_type: String, retracts_id: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = (NS_RETRACT, NS_HINTS);
            let stanza = build_retraction_message(&to, &msg_type, &retracts_id);
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_moderation(
        &self,
        to: String,
        msg_type: String,
        target_id: String,
        reason: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = (NS_FASTEN, NS_MODERATE, NS_RETRACT, NS_HINTS);
            let stanza = build_moderation_message(&to, &msg_type, &target_id, reason.as_deref());
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_correction(
        &self,
        to: String,
        msg_type: String,
        body: String,
        replaces_id: String,
        options: JsValue,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_REPLACE;
            let opts = send_options_from_js(options)?;
            let (message_id, stanza) =
                build_correction_message(&to, &msg_type, &body, &replaces_id, &opts)
                    .map_err(|err| js_error(err.to_string()))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::from_str(message_id.as_str()))
        })
    }

    pub fn send_raw_iq(&self, xml: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = parse_raw_iq(&xml)?;
            let result = send_iq_command(inner, stanza).await?;
            Ok(JsValue::from_str(&element_to_xml_string(&result)?))
        })
    }

    pub fn subscribe_to_presence(&self, peer_jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr("type", "subscribe")
                .attr("to", peer_jid.as_str())
                .build();
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn enable_push_notifications(
        &self,
        service_jid: String,
        node: String,
        token: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_enable_push_iq(&service_jid, &node, &token);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn disable_push_notifications(&self, service_jid: String, node: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_disable_push_iq(&service_jid, &node);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn get_server_version(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let domain = {
                let stored = inner.borrow().config.clone();
                jid_domain(&stored.jid)
            };
            let iq = Element::builder("iq", NS_CLIENT)
                .attr("type", "get")
                .attr("to", domain.as_str())
                .attr("id", uuid::Uuid::new_v4().to_string())
                .append(Element::builder("query", NS_VERSION).build())
                .build();
            let result = match send_iq_command(inner, iq).await {
                Ok(result) => result,
                Err(_) => return Ok(JsValue::NULL),
            };
            let Some(query) = result.get_child("query", NS_VERSION) else {
                return Ok(JsValue::NULL);
            };
            let version = WaddleServerVersion {
                name: query
                    .get_child("name", NS_VERSION)
                    .map(|child| child.text()),
                version: query
                    .get_child("version", NS_VERSION)
                    .map(|child| child.text()),
                os: query.get_child("os", NS_VERSION).map(|child| child.text()),
            };
            to_js_value(&version)
        })
    }

    pub fn list_room_members(&self, room_jid: String, affiliation: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_MUC_ADMIN;
            let affiliation = parse_muc_affiliation(&affiliation)?;
            let iq = build_muc_admin_affiliation_list_iq(&room_jid, affiliation);
            let result = send_iq_command(inner, iq).await?;
            let members = parse_muc_admin_affiliation_query(&result)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|item| {
                    Some(WaddleRoomMember {
                        jid: item.jid?,
                        affiliation: item.affiliation.map(muc_affiliation_to_string)?,
                    })
                })
                .collect::<Vec<_>>();
            to_js_value(&members)
        })
    }

    pub fn set_room_affiliation(
        &self,
        room_jid: String,
        jid: String,
        affiliation: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_MUC_ADMIN;
            let item = MucAdminAffiliationItem {
                jid: Some(jid),
                nick: None,
                affiliation: Some(parse_muc_affiliation(&affiliation)?),
                reason: None,
            };
            let iq = build_muc_admin_affiliation_set_iq(&room_jid, &[item]);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn fetch_inbox(&self, opts: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts = if opts.is_null() || opts.is_undefined() {
                WaddleFetchInboxOptions::default()
            } else {
                serde_wasm_bindgen::from_value(opts).map_err(|err| js_error(err.to_string()))?
            };
            let bare_jid = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            let iq = build_waddle_inbox_query_iq(
                bare_jid.as_str(),
                &WaddleInboxQuery {
                    since: opts.since.and_then(|value| u64::try_from(value).ok()),
                    only_unread: opts.only_unread,
                    room: opts.room,
                    threads: opts.threads,
                },
            );
            let result = send_iq_command(inner, iq).await?;
            let inbox = parse_waddle_inbox_result(&result)
                .map(inbox_result_to_js)
                .unwrap_or(WaddleInboxResult {
                    total_unread: 0,
                    conversations: Vec::new(),
                });
            to_js_value(&inbox)
        })
    }

    pub fn mark_inbox_read(&self, partner_jid: String, thread_id: Option<String>) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let bare_jid = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            let iq = build_waddle_inbox_mark_read_iq(
                bare_jid.as_str(),
                &WaddleInboxMarkRead {
                    partner: partner_jid,
                    thread: thread_id,
                },
            );
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn list_roster_contacts(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_ROSTER;
            let result = send_iq_command(inner, build_roster_get_iq(None, None)).await?;
            let contacts = parse_roster_result(&result)
                .map(|roster| {
                    roster
                        .items
                        .into_iter()
                        .map(|item| WaddleRosterContact {
                            jid: item.jid.to_string(),
                            name: item.name,
                            subscription: Some(item.subscription.to_string()),
                            groups: item.groups,
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            to_js_value(&contacts)
        })
    }

    pub fn publish_mood(&self, mood_json: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let mood: WaddleMoodOpts = serde_wasm_bindgen::from_value(mood_json)
                .map_err(|err| js_error(err.to_string()))?;
            let iq = build_publish_mood_iq(&mood.kind, mood.text.as_deref());
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn retract_mood(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = send_iq_command(inner, build_retract_mood_iq()).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn publish_activity(&self, activity_json: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let activity: WaddleActivityOpts = serde_wasm_bindgen::from_value(activity_json)
                .map_err(|err| js_error(err.to_string()))?;
            let iq = build_publish_activity_iq(
                &activity.general,
                activity.specific.as_deref(),
                activity.text.as_deref(),
            );
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn retract_activity(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = send_iq_command(inner, build_retract_activity_iq()).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn publish_tune(&self, tune_json: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let tune: WaddleTuneOpts = serde_wasm_bindgen::from_value(tune_json)
                .map_err(|err| js_error(err.to_string()))?;
            let iq = build_publish_tune_iq(
                tune.artist.as_deref(),
                tune.title.as_deref(),
                tune.source.as_deref(),
                tune.length,
                tune.rating,
                tune.track.as_deref(),
                tune.uri.as_deref(),
            );
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn retract_tune(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = send_iq_command(inner, build_retract_tune_iq()).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn fetch_user_pep_profile(&self, jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let mood = send_iq_command(
                inner.clone(),
                build_pep_items_iq(&jid, waddle_xmpp_client::pep::NS_MOOD),
            )
            .await
            .ok()
            .and_then(|result| parse_pep_mood(&result))
            .map(|mood| WaddleMoodResult {
                kind: mood.mood,
                text: mood.text,
            });
            let activity = send_iq_command(
                inner.clone(),
                build_pep_items_iq(&jid, waddle_xmpp_client::pep::NS_ACTIVITY),
            )
            .await
            .ok()
            .and_then(|result| parse_pep_activity(&result))
            .map(|activity| WaddleActivityResult {
                general: activity.activity,
                specific: activity.specific,
                text: activity.text,
            });
            let tune = send_iq_command(
                inner,
                build_pep_items_iq(&jid, waddle_xmpp_client::pep::NS_TUNE),
            )
            .await
            .ok()
            .and_then(|result| parse_pep_tune(&result))
            .map(|tune| WaddleTuneResult {
                artist: tune.artist,
                title: tune.title,
                source: tune.source,
                length: tune.length,
                rating: tune.rating,
                track: tune.track,
                uri: tune.uri,
            });
            let profile = WaddlePepProfile {
                mood,
                activity,
                tune,
            };
            to_js_value(&profile)
        })
    }

    pub fn fetch_room_history_by_thread(
        &self,
        room_jid: String,
        thread_id: String,
        max: u32,
        before_id: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                max,
                before_id.as_deref(),
                None,
                None,
                Some(&room_jid),
                Some(&thread_id),
                None,
                None,
                None,
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn search_room_history(&self, room_jid: String, query: String, max: u32) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                max,
                Some(""),
                None,
                None,
                Some(&room_jid),
                None,
                Some(&query),
                None,
                None,
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn search_dm_history(&self, peer_jid: String, query: String, max: u32) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                max,
                Some(""),
                None,
                Some(&peer_jid),
                None,
                None,
                Some(&query),
                None,
                None,
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn fetch_room_history_page(
        &self,
        room_jid: String,
        max: u32,
        page_param: JsValue,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let page: WaddleMamPageParam = serde_wasm_bindgen::from_value(page_param)
                .map_err(|err| js_error(err.to_string()))?;
            let before = match page.kind.as_str() {
                "latest" => Some(String::new()),
                "before" => page.before,
                _ => return Err(js_error("invalid page param type")),
            };
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                max,
                before.as_deref(),
                None,
                None,
                Some(&room_jid),
                None,
                None,
                None,
                None,
            );
            let result = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(result))
        })
    }

    pub fn fetch_dm_history_page(
        &self,
        peer_jid: String,
        max: u32,
        page_param: JsValue,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let page: WaddleMamPageParam = serde_wasm_bindgen::from_value(page_param)
                .map_err(|err| js_error(err.to_string()))?;
            let before = match page.kind.as_str() {
                "latest" => Some(String::new()),
                "before" => page.before,
                _ => return Err(js_error("invalid page param type")),
            };
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                max,
                before.as_deref(),
                None,
                Some(&peer_jid),
                None,
                None,
                None,
                None,
                None,
            );
            let result = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(result))
        })
    }

    pub fn search_users(&self, query: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_USER_SEARCH;
            let domain = {
                let stored = inner.borrow().config.clone();
                jid_domain(&stored.jid)
            };
            let iq = build_user_search_iq(
                &domain,
                &UserSearchQuery {
                    nick: Some(query),
                    ..UserSearchQuery::default()
                },
            );
            let result = send_iq_command(inner, iq).await?;
            let users = parse_user_search_result(&result)
                .map(|results| {
                    results
                        .items
                        .into_iter()
                        .filter_map(|item| {
                            item.nick.as_ref()?;
                            Some(WaddleUserSearchResult {
                                jid: item.jid,
                                username: item.nick.unwrap_or_default(),
                                display_name: item.first.or(item.last),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            to_js_value(&users)
        })
    }

    pub fn fetch_room_history(
        &self,
        room_jid: String,
        max: u32,
        before_id: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq(
                &iq_id,
                &query_id,
                max,
                before_id.as_deref(),
                None,
                Some(&room_jid),
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn fetch_dm_history(
        &self,
        peer_jid: String,
        max: u32,
        before_id: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq(
                &iq_id,
                &query_id,
                max,
                before_id.as_deref(),
                Some(&peer_jid),
                None,
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn join_room(&self, room_jid: String, nick: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let to = format!("{room_jid}/{nick}");
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr("to", to.as_str())
                .append(Element::builder("x", NS_MUC).build())
                .build();
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn join_room_without_history(&self, room_jid: String, nick: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let to = format!("{room_jid}/{nick}");
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr("to", to.as_str())
                .append(
                    Element::builder("x", NS_MUC)
                        .append(
                            Element::builder("history", NS_MUC)
                                .attr("maxstanzas", "0")
                                .build(),
                        )
                        .build(),
                )
                .build();
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn leave_room(&self, room_jid: String, nick: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let to = format!("{room_jid}/{nick}");
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr("to", to.as_str())
                .attr("type", "unavailable")
                .build();
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_presence(&self, status: Option<String>, show: Option<String>) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let mut builder = Element::builder("presence", NS_CLIENT);
            if let Some(status) = status.as_deref() {
                builder =
                    builder.append(Element::builder("status", NS_CLIENT).append(status).build());
            }
            if let Some(show) = show.as_deref() {
                builder = builder.append(Element::builder("show", NS_CLIENT).append(show).build());
            }
            send_stanza_command(inner, builder.build()).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn request_avatar(&self, jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let bare: BareJid = jid
                .parse()
                .map_err(|err| js_error(format!("invalid JID: {err}")))?;
            let avatar = request_avatar_with_iq(&bare, |stanza| {
                let inner = inner.clone();
                async move { send_avatar_iq_command(inner, stanza).await }
            })
            .await?;

            match avatar {
                Some(avatar) => to_js_value(&WaddleAvatar {
                    jid: avatar.jid.to_string(),
                    id: avatar.id,
                    mime_type: avatar.mime_type,
                    data: avatar.data,
                    url: avatar.url,
                }),
                None => Ok(JsValue::NULL),
            }
        })
    }

    pub fn discover_upload_service(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let domain = {
                let stored = inner.borrow().config.clone();
                jid_domain(&stored.jid)
            };
            let items_iq = build_disco_items_iq(&domain, None);
            let items_result = send_iq_command(inner.clone(), items_iq).await?;
            let items = discovery::parse_disco_items_result(&items_result)
                .ok_or_else(|| js_error("could not parse disco#items result"))?;

            for item in items {
                let info_iq = build_disco_info_iq(&item.jid, None);
                let info_result = send_iq_command(inner.clone(), info_iq).await?;
                if let Some(info) = discovery::parse_disco_info_result(&info_result, &item.jid) {
                    if info.has_feature(discovery::UPLOAD_NS) {
                        return Ok(JsValue::from_str(&item.jid));
                    }
                }
            }

            Ok(JsValue::NULL)
        })
    }

    pub fn request_upload_slot(
        &self,
        service_jid: String,
        filename: String,
        size: u64,
        content_type: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_upload_slot_iq(&service_jid, &filename, size, &content_type);
            let result = send_iq_command(inner, iq).await?;
            let slot = discovery::parse_upload_slot(&result)
                .ok_or_else(|| js_error("could not parse upload slot"))?;
            to_js_value(&upload_slot_to_js(slot))
        })
    }

    pub fn discover_extension_routes(&self, user_jid: Option<String>) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let requested_jid = user_jid.unwrap_or_else(|| inner.borrow().config.jid.clone());
            let jid: Jid = requested_jid
                .parse()
                .map_err(|err| js_error(format!("invalid JID: {err}")))?;
            let domain = jid_domain(&jid.to_string());
            let service_jid = discover_extension_command_service(inner.clone(), &domain).await;
            let mut routes = Vec::new();

            let items_result =
                send_iq_command(inner.clone(), build_disco_items_iq(&service_jid, None)).await?;
            let items = discovery::parse_disco_items_result(&items_result)
                .ok_or_else(|| js_error("could not parse extension route disco#items result"))?;
            for item in items {
                let Some(node) = item.node.as_deref() else {
                    continue;
                };
                let item_service_jid = item.jid.as_str();
                let info_result = send_iq_command(
                    inner.clone(),
                    build_disco_info_iq(item_service_jid, Some(node)),
                )
                .await?;
                if let Some(info) =
                    discovery::parse_disco_info_result(&info_result, item_service_jid)
                {
                    routes.extend(parse_extension_routes_from_info(&info, item_service_jid));
                }
            }

            if routes.is_empty() {
                let info_result =
                    send_iq_command(inner.clone(), build_disco_info_iq(&service_jid, None)).await?;
                if let Some(info) = discovery::parse_disco_info_result(&info_result, &service_jid) {
                    routes.extend(parse_extension_routes_from_info(&info, &service_jid));
                }
            }

            to_js_value(&routes)
        })
    }

    pub fn fetch_extension_route_items(&self, route: JsValue, room_jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let route: WaddleExtensionRoute = serde_wasm_bindgen::from_value(route)
                .map_err(|err| js_error(format!("invalid extension route: {err}")))?;
            let node = route.state_node.replace("{room}", &room_jid);
            let iq = build_extension_route_items_iq(
                &route.service_jid,
                &node,
                Some(EXTENSION_ROUTE_ITEM_LIMIT),
            );
            let result = send_iq_command(inner, iq).await?;
            let items = parse_extension_route_items_result(&result);
            to_js_value(&items)
        })
    }
}

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

async fn discover_extension_command_service(
    inner: Rc<RefCell<WaddleClientInner>>,
    domain: &str,
) -> String {
    let fallback_service_jid = format!("extensions.{domain}");
    let mut discovered_items = Vec::new();

    if let Ok(items_result) =
        send_iq_command(inner.clone(), build_disco_items_iq(domain, None)).await
    {
        if let Some(items) = discovery::parse_disco_items_result(&items_result) {
            discovered_items = items;
        }
    }

    for candidate in extension_command_service_candidates(domain, &discovered_items) {
        if let Ok(info_result) =
            send_iq_command(inner.clone(), build_disco_info_iq(&candidate, None)).await
        {
            if let Some(info) = discovery::parse_disco_info_result(&info_result, &candidate) {
                if is_waddle_extension_service(&info) {
                    return candidate;
                }
            }
        }
    }

    fallback_service_jid
}

fn extension_command_service_candidates(
    domain: &str,
    items: &[discovery::DiscoItem],
) -> Vec<String> {
    let fallback_service_jid = format!("extensions.{domain}");
    let mut candidates = Vec::new();

    for item in items {
        push_unique_candidate(&mut candidates, item.jid.as_str());
    }
    push_unique_candidate(&mut candidates, &fallback_service_jid);
    push_unique_candidate(&mut candidates, domain);

    candidates
}

fn push_unique_candidate(candidates: &mut Vec<String>, candidate: &str) {
    if !candidates.iter().any(|existing| existing == candidate) {
        candidates.push(candidate.to_string());
    }
}

fn is_waddle_extension_service(info: &discovery::DiscoInfoResult) -> bool {
    info.has_feature(NS_WADDLE_EXTENSION_1) && info.has_feature(NS_ADHOC_COMMANDS)
}

fn parse_extension_routes_from_info(
    info: &discovery::DiscoInfoResult,
    service_jid: &str,
) -> Vec<WaddleExtensionRoute> {
    info.forms
        .iter()
        .filter(|form| form.form_type.as_deref() == Some(EXTENSION_ROUTE_FORM_TYPE))
        .filter_map(|form| {
            let plugin_id = disco_form_value(form, "waddle#plugin_id")?;
            let route_id = disco_form_value(form, "waddle#route_id")?;
            let label = disco_form_value(form, "waddle#route_label")?;
            let scope = disco_form_value(form, "waddle#route_scope")?;
            let surface = disco_form_value(form, "waddle#route_surface")?;
            let state_node = disco_form_value(form, "waddle#state_node")?;
            let payload_namespace = disco_form_value(form, "waddle#payload_ns")?;
            if scope != "channel" || (surface != "gallery" && surface != "list") {
                return None;
            }
            Some(WaddleExtensionRoute {
                service_jid: service_jid.to_string(),
                plugin_id: plugin_id.to_string(),
                route_id: route_id.to_string(),
                label: label.to_string(),
                scope: scope.to_string(),
                surface: surface.to_string(),
                state_node: state_node.to_string(),
                payload_namespace: payload_namespace.to_string(),
            })
        })
        .collect()
}

fn disco_form_value<'a>(form: &'a discovery::DiscoDataForm, var: &str) -> Option<&'a str> {
    form.fields
        .iter()
        .find(|field| field.var == var)
        .and_then(|field| field.values.first())
        .map(String::as_str)
}

/// Build an XEP-0060 pubsub `<items>` IQ directed at `to` for `node`.
fn build_extension_route_items_iq(to: &str, node: &str, max_items: Option<u32>) -> Element {
    let id = format!("pubsub-items-{}", uuid::Uuid::new_v4());
    let mut items_builder = Element::builder("items", NS_PUBSUB).attr("node", node);
    let max_items_value;
    if let Some(max_items) = max_items {
        max_items_value = max_items.to_string();
        items_builder = items_builder.attr("max_items", max_items_value.as_str());
    }
    let items = items_builder.build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr("type", "get")
        .attr("id", id)
        .attr("to", to)
        .append(pubsub)
        .build()
}

fn parse_extension_route_items_result(iq: &Element) -> Vec<WaddleExtensionRouteItem> {
    let Some(pubsub) = iq.get_child("pubsub", NS_PUBSUB) else {
        return Vec::new();
    };
    let Some(items) = pubsub.get_child("items", NS_PUBSUB) else {
        return Vec::new();
    };
    items
        .children()
        .filter(|el| el.name() == "item" && el.ns() == NS_PUBSUB)
        .filter_map(parse_extension_route_item)
        .collect()
}

fn parse_extension_route_item(item: &Element) -> Option<WaddleExtensionRouteItem> {
    let payload = item
        .children()
        .find(|child| child.name() == "extension-item" && child.ns() == NS_WADDLE_EXTENSION_1)?;
    let mut view = WaddleExtensionRouteItem {
        id: item.attr("id").map(str::to_string),
        title: None,
        subtitle: None,
        link: None,
        description: None,
        fields: Vec::new(),
        options: Vec::new(),
        actions: Vec::new(),
    };

    for child in payload
        .children()
        .filter(|child| child.ns() == NS_WADDLE_EXTENSION_1)
    {
        match child.name() {
            "title" => {
                let value = child.text().trim().to_string();
                if !value.is_empty() {
                    view.title = Some(value);
                }
            }
            "subtitle" => {
                let value = child.text().trim().to_string();
                if !value.is_empty() {
                    view.subtitle = Some(value);
                }
            }
            "description" => {
                let value = child.text().trim().to_string();
                if !value.is_empty() {
                    view.description = Some(value);
                }
            }
            "link" => {
                if let Some(href) = child.attr("href").filter(|href| !href.trim().is_empty()) {
                    view.link = Some(WaddleExtensionRouteLink {
                        href: href.to_string(),
                    });
                }
            }
            "field" => {
                if let Some(name) = child.attr("name").filter(|name| !name.trim().is_empty()) {
                    view.fields.push(WaddleExtensionRouteItemField {
                        name: name.to_string(),
                        label: child
                            .attr("label")
                            .filter(|label| !label.trim().is_empty())
                            .map(str::to_string),
                        value: child.text().trim().to_string(),
                    });
                }
            }
            "option" => {
                if let (Some(id), Some(label)) = (child.attr("id"), child.attr("label")) {
                    if !id.trim().is_empty() && !label.trim().is_empty() {
                        view.options.push(WaddleExtensionRouteItemOption {
                            id: id.to_string(),
                            label: label.to_string(),
                        });
                    }
                }
            }
            "action" => {
                if let (
                    Some(launch_id),
                    Some(label),
                    Some(plugin_id),
                    Some(action_id),
                    Some(command_node),
                    Some(launch_token),
                    Some(expires_at),
                    Some(waddle_id),
                ) = (
                    child
                        .attr("launch-id")
                        .filter(|value| !value.trim().is_empty()),
                    child.attr("label").filter(|value| !value.trim().is_empty()),
                    child
                        .attr("plugin")
                        .filter(|value| !value.trim().is_empty()),
                    child
                        .attr("action")
                        .filter(|value| !value.trim().is_empty()),
                    child
                        .attr("command-node")
                        .filter(|value| !value.trim().is_empty()),
                    child.attr("token").filter(|value| !value.trim().is_empty()),
                    child
                        .attr("expires-at")
                        .filter(|value| !value.trim().is_empty()),
                    child
                        .attr("waddle-id")
                        .filter(|value| !value.trim().is_empty()),
                ) {
                    view.actions.push(WaddleExtensionRouteItemAction {
                        launch: WaddleExtensionRouteItemLaunch {
                            id: launch_id.to_string(),
                            plugin_id: plugin_id.to_string(),
                            action_id: action_id.to_string(),
                            command_node: command_node.to_string(),
                            label: label.to_string(),
                            launch_token: launch_token.to_string(),
                            expires_at: expires_at.to_string(),
                            waddle_id: waddle_id.to_string(),
                            room_jid: child.attr("room").map(str::to_string),
                            source_stanza_id: child.attr("source-stanza-id").map(str::to_string),
                        },
                    });
                }
            }
            _ => {}
        }
    }

    Some(view)
}

async fn driver_loop(
    config: ClientConfig,
    ws: WasmWebSocket,
    cmd_rx: mpsc::Receiver<WasmCommand>,
    event_tx: mpsc::Sender<DriverEvent>,
) {
    let mut task = match WasmDriverTask::new(config, ws, cmd_rx, event_tx.clone()) {
        Ok(task) => task,
        Err(err) => {
            let mut event_tx = event_tx;
            let _ = event_tx.send(DriverEvent::Error(err.to_string())).await;
            let _ = event_tx.send(DriverEvent::Disconnected).await;
            return;
        }
    };
    task.run().await;
}

impl WasmDriverTask {
    fn new(
        config: ClientConfig,
        ws: WasmWebSocket,
        cmd_rx: mpsc::Receiver<WasmCommand>,
        event_tx: mpsc::Sender<DriverEvent>,
    ) -> DriverResult<Self> {
        Ok(Self {
            runtime: XmppRuntime::new(config)?,
            ws,
            cmd_rx,
            event_tx,
            pending_iqs: HashMap::new(),
            pending_mam_queries: HashMap::new(),
            sm_delivery_tracking_enabled: false,
            outbound_h: 0,
            pending_message_deliveries: VecDeque::new(),
        })
    }

    async fn run(&mut self) {
        match self.runtime.queue_request(ClientRequest::Connect) {
            Ok(events) => {
                for event in events {
                    if !self.handle_client_event(event).await {
                        self.finish().await;
                        return;
                    }
                }
            }
            Err(err) => {
                self.emit_error(err.to_string()).await;
                self.finish().await;
                return;
            }
        }

        loop {
            let ws_event_fut = self.ws.rx.next().fuse();
            let cmd_fut = self.cmd_rx.next().fuse();
            pin_mut!(ws_event_fut, cmd_fut);

            let keep_running = select! {
                ws_event = ws_event_fut => self.handle_wasm_transport_event(ws_event).await,
                cmd = cmd_fut => self.handle_command(cmd).await,
            };

            if !keep_running {
                break;
            }
        }

        self.finish().await;
    }

    async fn handle_wasm_transport_event(&mut self, event: Option<WasmTransportEvent>) -> bool {
        match event {
            Some(WasmTransportEvent::Open) => {
                self.apply_transport_event(TransportEvent::StateChanged(TransportState::Connecting))
                    .await
                    && self
                        .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
                        .await
            }
            Some(WasmTransportEvent::Message(text)) => {
                match waddle_xmpp_client::decode_message(&text) {
                    Ok(message) => {
                        self.apply_transport_event(TransportEvent::MessageReceived(message))
                            .await
                    }
                    Err(err) => {
                        self.emit_error(err.to_string()).await;
                        let _ = self
                            .apply_transport_event(TransportEvent::StateChanged(
                                TransportState::Failed,
                            ))
                            .await;
                        let _ = self.apply_transport_event(TransportEvent::Closed).await;
                        false
                    }
                }
            }
            Some(WasmTransportEvent::Close { .. }) | None => {
                let _ = self
                    .apply_transport_event(TransportEvent::StateChanged(TransportState::Closed))
                    .await;
                let _ = self.apply_transport_event(TransportEvent::Closed).await;
                false
            }
            Some(WasmTransportEvent::Error) => {
                self.emit_error("websocket transport error".to_string())
                    .await;
                let _ = self
                    .apply_transport_event(TransportEvent::StateChanged(TransportState::Failed))
                    .await;
                let _ = self.apply_transport_event(TransportEvent::Closed).await;
                false
            }
        }
    }

    async fn handle_command(&mut self, cmd: Option<WasmCommand>) -> bool {
        match cmd {
            Some(WasmCommand::SendStanza { stanza, responder }) => {
                let result = self
                    .send_transport_message(TransportMessage::Element(stanza.clone()))
                    .await;
                let keep_running = result.is_ok();
                if let Err(err) = &result {
                    if let Some(stanza_id) = message_delivery_stanza_id(&stanza) {
                        self.emit_message_delivery_failed(stanza_id).await;
                    }
                    self.emit_error(err.to_string()).await;
                }
                let _ = responder.send(result);
                keep_running
            }
            Some(WasmCommand::SendIq { stanza, responder }) => {
                let id = stanza.attr("id").map(|value| value.to_string());
                match self
                    .send_transport_message(TransportMessage::Element(stanza))
                    .await
                {
                    Ok(()) => match id {
                        Some(id) => {
                            self.pending_iqs.insert(id, responder);
                            true
                        }
                        None => {
                            let _ = responder.send(Err(ClientError::Disconnected));
                            false
                        }
                    },
                    Err(err) => {
                        self.emit_error(err.to_string()).await;
                        let _ = responder.send(Err(err));
                        false
                    }
                }
            }
            Some(WasmCommand::SendMamQuery {
                stanza,
                query_id,
                responder,
            }) => {
                let id = stanza.attr("id").map(|value| value.to_string());
                match self
                    .send_transport_message(TransportMessage::Element(stanza))
                    .await
                {
                    Ok(()) => match id {
                        Some(id) => {
                            self.pending_mam_queries.insert(
                                id,
                                PendingMamQuery {
                                    query_id,
                                    messages: Vec::new(),
                                    responder,
                                },
                            );
                            true
                        }
                        None => {
                            let _ = responder.send(Err(ClientError::Disconnected));
                            false
                        }
                    },
                    Err(err) => {
                        self.emit_error(err.to_string()).await;
                        let _ = responder.send(Err(err));
                        false
                    }
                }
            }
            Some(WasmCommand::Disconnect { responder }) => {
                let result = self
                    .send_transport_message(TransportMessage::Close(StreamClose))
                    .await;
                let keep_running = result.is_ok();
                let _ = responder.send(result);
                keep_running
            }
            None => false,
        }
    }

    async fn apply_transport_event(&mut self, event: TransportEvent) -> bool {
        let events = match self.runtime.apply_transport_event(event) {
            Ok(events) => events,
            Err(err) => {
                self.emit_error(err.to_string()).await;
                return false;
            }
        };

        for event in events {
            if !self.handle_client_event(event).await {
                return false;
            }
        }

        true
    }

    async fn handle_client_event(&mut self, event: ClientEvent) -> bool {
        if let Some(message) = self.dispatch_client_event(event).await {
            if let Err(err) = self.send_transport_message(message).await {
                self.emit_error(err.to_string()).await;
                self.fail_all_pending_message_deliveries().await;
                return false;
            }
        }
        true
    }

    async fn apply_sent_event(&mut self, event: TransportEvent) -> DriverResult<()> {
        let events = self.runtime.apply_transport_event(event)?;
        for event in events {
            if self.dispatch_client_event(event).await.is_some() {
                return Err(ClientError::Disconnected);
            }
        }
        Ok(())
    }

    async fn dispatch_client_event(&mut self, event: ClientEvent) -> Option<TransportMessage> {
        match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(message)) => {
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::OutboundMessage(message.clone()),
                    )))
                    .await;
                Some(message)
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Enabled { previd },
            )) => {
                self.sm_delivery_tracking_enabled = true;
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::StreamManagement(StreamManagementEvent::Enabled {
                            previd,
                        }),
                    )))
                    .await;
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Resumed { h },
            )) => {
                self.sm_delivery_tracking_enabled = true;
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::StreamManagement(StreamManagementEvent::Resumed { h }),
                    )))
                    .await;
                self.emit_acked_message_deliveries(h).await;
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckReceived { h },
            )) => {
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::StreamManagement(StreamManagementEvent::AckReceived { h }),
                    )))
                    .await;
                self.emit_acked_message_deliveries(h).await;
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Failed,
            )) => {
                self.sm_delivery_tracking_enabled = false;
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::StreamManagement(StreamManagementEvent::Failed),
                    )))
                    .await;
                self.fail_all_pending_message_deliveries().await;
                None
            }
            ClientEvent::IqResult { id, element } => {
                if let Some(responder) = self.pending_iqs.remove(&id) {
                    let result = if element.attr("type") == Some("result") {
                        Ok(element)
                    } else {
                        Err(ClientError::StanzaError(parse_stanza_error(&element)))
                    };
                    let _ = responder.send(result);
                } else if let Some(pending) = self.pending_mam_queries.remove(&id) {
                    let result = if element.attr("type") == Some("result") {
                        let (rsm, is_complete) = mam::parse_fin_from_iq_result(&element);
                        Ok(waddle_xmpp_client::MamPage {
                            messages: pending.messages,
                            rsm,
                            query_id: pending.query_id,
                            is_complete,
                        })
                    } else {
                        Err(ClientError::StanzaError(parse_stanza_error(&element)))
                    };
                    let _ = pending.responder.send(result);
                }
                None
            }
            ClientEvent::MamResult(archived) => {
                if let Some(query_id) = archived.query_id.as_deref() {
                    if let Some((_, pending)) = self
                        .pending_mam_queries
                        .iter_mut()
                        .find(|(_, pending)| pending.query_id == query_id)
                    {
                        pending.messages.push(archived);
                    }
                }
                None
            }
            other => {
                let _ = self.event_tx.clone().send(client_driver_event(other)).await;
                None
            }
        }
    }

    async fn send_transport_message(&mut self, message: TransportMessage) -> DriverResult<()> {
        let sent_event = TransportEvent::MessageSent(message.clone());
        let frame = waddle_xmpp_client::encode_message(&message)?;
        self.ws
            .send(&frame)
            .map_err(|_| ClientError::TransportClosed)?;

        if let TransportMessage::Element(element) = &message {
            self.record_outbound_element(element);
        }

        if matches!(message, TransportMessage::Close(_)) {
            let _ = self.ws.close();
        }

        self.apply_sent_event(sent_event).await?;

        Ok(())
    }

    fn record_outbound_element(&mut self, element: &Element) {
        if is_stream_management_enable(element) {
            self.sm_delivery_tracking_enabled = true;
            self.outbound_h = 0;
            self.pending_message_deliveries.clear();
            return;
        }

        if !self.sm_delivery_tracking_enabled {
            return;
        }

        if !matches!(element.name(), "iq" | "message" | "presence") {
            return;
        }

        self.outbound_h = self.outbound_h.wrapping_add(1);
        if let Some(stanza_id) = message_delivery_stanza_id(element) {
            self.pending_message_deliveries
                .push_back(TrackedMessageDelivery {
                    stanza_id,
                    h: self.outbound_h,
                });
        }
    }

    async fn emit_acked_message_deliveries(&mut self, h: u32) {
        while self
            .pending_message_deliveries
            .front()
            .is_some_and(|pending| pending.h <= h)
        {
            if let Some(pending) = self.pending_message_deliveries.pop_front() {
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::MessageDelivery(
                        MessageDeliveryEvent::Acked {
                            stanza_id: pending.stanza_id,
                        },
                    )))
                    .await;
            }
        }
    }

    async fn fail_all_pending_message_deliveries(&mut self) {
        while let Some(pending) = self.pending_message_deliveries.pop_front() {
            self.emit_message_delivery_failed(pending.stanza_id).await;
        }
    }

    async fn emit_message_delivery_failed(&mut self, stanza_id: StanzaId) {
        let _ = self
            .event_tx
            .clone()
            .send(client_driver_event(ClientEvent::MessageDelivery(
                MessageDeliveryEvent::Failed { stanza_id },
            )))
            .await;
    }

    async fn emit_error(&mut self, description: String) {
        let _ = self
            .event_tx
            .clone()
            .send(DriverEvent::Error(description))
            .await;
    }

    async fn finish(&mut self) {
        self.fail_all_pending_message_deliveries().await;

        for (_, responder) in self.pending_iqs.drain() {
            let _ = responder.send(Err(ClientError::Disconnected));
        }
        for (_, pending) in self.pending_mam_queries.drain() {
            let _ = pending.responder.send(Err(ClientError::Disconnected));
        }

        let _ = self.event_tx.clone().send(DriverEvent::Disconnected).await;
    }
}

async fn event_dispatch_loop(
    inner: Rc<RefCell<WaddleClientInner>>,
    mut event_rx: mpsc::Receiver<DriverEvent>,
) {
    while let Some(event) = event_rx.next().await {
        match event {
            DriverEvent::Client(client_event) => dispatch_client_event(&inner, *client_event),
            DriverEvent::Error(description) => emit_error_callback(&inner, &description),
            DriverEvent::Disconnected => {
                inner.borrow_mut().cmd_tx = None;
                if let Some(callback) = inner.borrow().on_disconnected.as_ref() {
                    let _ = callback.call0(&JsValue::NULL);
                }
            }
        }
    }
}

fn dispatch_client_event(inner: &Rc<RefCell<WaddleClientInner>>, event: ClientEvent) {
    match event {
        ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_)) => {
            let borrowed = inner.borrow();
            if let Some(callback) = borrowed.on_connected.as_ref() {
                let _ = callback.call0(&JsValue::NULL);
            }
            if let Some(callback) = borrowed.on_session_lifecycle.as_ref() {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str("fresh"));
            }
        }
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Resumed { .. },
        )) => {
            if let Some(callback) = inner.borrow().on_session_lifecycle.as_ref() {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str("resumed"));
            }
        }
        ClientEvent::Messaging(waddle_xmpp_client::MessagingEvent::Message(message)) => {
            if let Some(callback) = inner.borrow().on_message.as_ref() {
                if let Ok(value) = to_js_value(&inbound_to_js(*message)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::Messaging(waddle_xmpp_client::MessagingEvent::Presence(presence)) => {
            if let Some(callback) = inner.borrow().on_presence.as_ref() {
                if let Ok(value) = to_js_value(&presence_to_js(presence)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id }) => {
            if let Some(callback) = inner.borrow().on_message_delivery_acked.as_ref() {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(stanza_id.as_str()));
            }
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id }) => {
            if let Some(callback) = inner.borrow().on_message_delivery_failed.as_ref() {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(stanza_id.as_str()));
            }
        }
        _ => {}
    }
}

fn emit_error_callback(inner: &Rc<RefCell<WaddleClientInner>>, description: &str) {
    if let Some(callback) = inner.borrow().on_error.as_ref() {
        let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(description));
    }
}

async fn send_stanza_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<(), JsValue> {
    let mut cmd_tx = command_sender(&inner)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendStanza { stanza, responder })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

async fn send_iq_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<Element, JsValue> {
    let mut cmd_tx = command_sender(&inner)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendIq { stanza, responder })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

async fn send_avatar_iq_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<Element, AvatarRequestFailure<JsValue>> {
    let mut cmd_tx = command_sender(&inner).map_err(AvatarRequestFailure::Other)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendIq { stanza, responder })
        .await
        .map_err(|_| AvatarRequestFailure::Other(js_error("client is disconnected")))?;
    rx.await
        .map_err(|_| AvatarRequestFailure::Other(js_error("client is disconnected")))?
        .map_err(|err| match err {
            ClientError::StanzaError(_) => AvatarRequestFailure::StanzaError,
            other => AvatarRequestFailure::Other(js_error(other.to_string())),
        })
}

async fn send_mam_query_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
    query_id: String,
) -> Result<waddle_xmpp_client::MamPage, JsValue> {
    let mut cmd_tx = command_sender(&inner)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendMamQuery {
            stanza,
            query_id,
            responder,
        })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

async fn disconnect_client(inner: Rc<RefCell<WaddleClientInner>>) -> Result<(), JsValue> {
    let mut cmd_tx = match inner.borrow().cmd_tx.clone() {
        Some(cmd_tx) => cmd_tx,
        None => return Ok(()),
    };

    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::Disconnect { responder })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    inner.borrow_mut().cmd_tx = None;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

fn command_sender(
    inner: &Rc<RefCell<WaddleClientInner>>,
) -> Result<mpsc::Sender<WasmCommand>, JsValue> {
    inner
        .borrow()
        .cmd_tx
        .clone()
        .ok_or_else(|| js_error("client is not connected"))
}

fn build_client_config(config: &StoredConfig) -> Result<ClientConfig, JsValue> {
    let jid: BareJid = config
        .jid
        .parse()
        .map_err(|err| js_error(format!("invalid JID: {err}")))?;
    let domain: BareJid = jid
        .domain()
        .to_string()
        .parse()
        .map_err(|err| js_error(format!("invalid domain: {err}")))?;
    let transport = WebSocketConfig::new(
        config
            .server_url
            .parse()
            .map_err(|err| js_error(format!("invalid server URL: {err}")))?,
    )
    .map_err(|err| js_error(err.to_string()))?;
    let resource =
        ClientResource::new(config.resource.clone()).map_err(|err| js_error(err.to_string()))?;
    let auth = OAuthBearerConfig::new(jid, resource, AccessToken::new(config.access_token.clone()))
        .map_err(|err| js_error(err.to_string()))?;
    ClientConfig::new(ConnectionConfig::new(domain), transport, auth)
        .map_err(|err| js_error(err.to_string()))
}

fn parse_raw_iq(xml: &str) -> Result<Element, JsValue> {
    // minidom 0.16 requires every element to carry a namespace at parse time.
    // Callers omit xmlns='jabber:client' on the <iq> element (it is implied by
    // the stream context).  Inject the namespace into the raw string before
    // parsing so minidom does not return MissingNamespace.
    //
    // We inspect only the opening <iq …> tag (up to the first '>') to avoid
    // matching xmlns= attributes on child elements.
    let normalized: std::borrow::Cow<str> = {
        let tag_end = xml.find('>').unwrap_or(xml.len());
        let iq_tag = &xml[..tag_end];
        if iq_tag.contains("xmlns=") {
            std::borrow::Cow::Borrowed(xml)
        } else {
            std::borrow::Cow::Owned(xml.replacen("<iq", "<iq xmlns='jabber:client'", 1))
        }
    };
    let stanza = normalized
        .parse::<Element>()
        .map_err(|err| js_error(format!("invalid IQ XML: {err}")))?;
    if stanza.name() != "iq" {
        return Err(js_error("raw stanza must be an <iq/> element"));
    }
    Ok(stanza)
}

fn element_to_xml_string(element: &Element) -> Result<String, JsValue> {
    let mut buffer = Vec::new();
    element
        .write_to(&mut buffer)
        .map_err(|err| js_error(format!("failed to serialize IQ XML: {err}")))?;
    String::from_utf8(buffer).map_err(|err| js_error(format!("IQ XML was not UTF-8: {err}")))
}

fn send_options_from_js(options: JsValue) -> Result<SendMessageOptions, JsValue> {
    let options = if options.is_null() || options.is_undefined() {
        WaddleSendOptions::default()
    } else {
        serde_wasm_bindgen::from_value(options).map_err(|err| js_error(err.to_string()))?
    };

    let reply = match options.reply {
        Some(target) => Some(ReplyMarker {
            to: target
                .author_jid
                .parse::<Jid>()
                .map_err(|err| js_error(format!("invalid reply author JID: {err}")))?,
            id: target.message_id,
        }),
        None => None,
    };

    let stanza_id = options
        .stanza_id
        .map(StanzaId::new)
        .transpose()
        .map_err(|err| js_error(err.to_string()))?;

    Ok(SendMessageOptions {
        stanza_id,
        subject: options.subject,
        reply,
        fallback: options.fallback.map(|range| FallbackRange {
            start: range.start,
            end: range.end,
        }),
        thread: options.thread.map(|thread| ThreadRef {
            id: thread.id,
            parent: thread.parent,
        }),
        markup_spans: options
            .markup_spans
            .into_iter()
            .map(|span| MarkupSpanData {
                span_type: span.span_type,
                start: span.start,
                end: span.end,
                uri: span.uri,
            })
            .collect(),
        references: options
            .references
            .into_iter()
            .map(|reference| ReferenceData {
                ref_type: reference.ref_type,
                uri: reference.uri,
                begin: reference.begin,
                end: reference.end,
            })
            .collect(),
        shared_files: options
            .shared_files
            .into_iter()
            .map(|file| messaging::SharedFile {
                url: file.url,
                name: file.name,
                media_type: file.media_type.clone(),
                size: file.size,
                width: file.width,
                height: file.height,
                disposition: SharedFileDisposition::from_text_or_infer(
                    Some(file.disposition.as_str()),
                    file.media_type.as_deref(),
                ),
            })
            .collect(),
    })
}

fn upload_slot_to_js(slot: discovery::UploadSlot) -> WaddleUploadSlot {
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

fn markup_span_type_to_string(span_type: MarkupSpanType) -> String {
    match span_type {
        MarkupSpanType::Bold => "bold",
        MarkupSpanType::Italic => "italic",
        MarkupSpanType::Strikethrough => "strikethrough",
        MarkupSpanType::Code => "code",
        MarkupSpanType::CodeBlock => "code_block",
        MarkupSpanType::Blockquote => "blockquote",
        MarkupSpanType::Link => "link",
    }
    .to_string()
}

fn markup_spans_to_js(spans: Vec<messaging::MarkupSpan>) -> Vec<WaddleMarkupSpan> {
    spans
        .into_iter()
        .map(|span| WaddleMarkupSpan {
            span_type: markup_span_type_to_string(span.span_type),
            start: span.start,
            end: span.end,
            uri: span.uri,
        })
        .collect()
}

fn inbound_to_js(message: InboundMessage) -> WaddleMessage {
    let (reply_fallback_start, reply_fallback_end) = match message.reply_fallback {
        Some((start, end)) => (Some(start), Some(end)),
        None => (None, None),
    };
    let forum_thread_title = if message.forum_post_kind.as_deref() == Some("topic") {
        message
            .forum_title
            .clone()
            .or_else(|| message.subject.clone())
    } else {
        None
    };

    WaddleMessage {
        id: message.id,
        from: message.from,
        to: message.to,
        body: message.body,
        subject: message.subject,
        message_type: message.message_type.clone(),
        timestamp: message.timestamp.map(|timestamp| timestamp.to_rfc3339()),
        stanza_id: message.stanza_id,
        origin_id: message.origin_id,
        replaces_id: message.replaces_id,
        retracts_id: message.retracts_id,
        moderation_target_id: message.moderation_target_id,
        moderated_by: message.moderated_by,
        moderation_reason: message.moderation_reason,
        chat_state: message.chat_state,
        displayed_marker_id: message.displayed_marker_id,
        reaction_target_id: message.reaction_target_id,
        reaction_emojis: message.reaction_emojis,
        is_muc: message.message_type == "groupchat",
        thread: message.thread_id.or(message.thread),
        parent_thread_id: message.parent_thread_id,
        reply_to_id: message.reply_to_id,
        reply_to_sender: message.reply_to_sender,
        reply_fallback_start,
        reply_fallback_end,
        markup_spans: markup_spans_to_js(message.markup_spans),
        broadcast_mention: message.broadcast_mention,
        mention_uris: message.mention_uris,
        forum_post_kind: message.forum_post_kind,
        forum_title: message.forum_title,
        forum_thread_title,
        is_sticker: message.is_sticker,
        shared_files: message
            .shared_files
            .into_iter()
            .map(shared_file_to_js)
            .collect(),
    }
}

fn archived_to_js(archived: ArchivedMessage) -> WaddleArchivedMessage {
    let parsed = match messaging::parse(&archived.inner) {
        Some(waddle_xmpp_client::MessagingEvent::Message(message)) => Some(message),
        _ => None,
    };
    let (reply_fallback_start, reply_fallback_end) = parsed
        .as_ref()
        .and_then(|message| message.reply_fallback)
        .map(|(start, end)| (Some(start), Some(end)))
        .unwrap_or((None, None));
    let forum_thread_title = parsed.as_ref().and_then(|message| {
        if message.forum_post_kind.as_deref() == Some("topic") {
            message
                .forum_title
                .clone()
                .or_else(|| message.subject.clone())
        } else {
            None
        }
    });

    WaddleArchivedMessage {
        mam_id: archived.mam_id,
        query_id: archived.query_id,
        stanza_id: archived.stanza_id,
        timestamp: archived.timestamp.map(|timestamp| timestamp.to_rfc3339()),
        from: archived.from,
        to: archived.to,
        message_type: archived.message_type,
        body: archived.body,
        subject: parsed.as_ref().and_then(|message| message.subject.clone()),
        replaces_id: parsed
            .as_ref()
            .and_then(|message| message.replaces_id.clone()),
        retracts_id: parsed
            .as_ref()
            .and_then(|message| message.retracts_id.clone()),
        moderation_target_id: parsed
            .as_ref()
            .and_then(|message| message.moderation_target_id.clone()),
        moderated_by: parsed
            .as_ref()
            .and_then(|message| message.moderated_by.clone()),
        moderation_reason: parsed
            .as_ref()
            .and_then(|message| message.moderation_reason.clone()),
        reaction_target_id: parsed
            .as_ref()
            .and_then(|message| message.reaction_target_id.clone()),
        reaction_emojis: parsed
            .as_ref()
            .map(|message| message.reaction_emojis.clone())
            .unwrap_or_default(),
        thread: archived.thread,
        parent_thread_id: archived.parent_thread_id,
        reply_to_id: parsed
            .as_ref()
            .and_then(|message| message.reply_to_id.clone()),
        reply_to_sender: parsed
            .as_ref()
            .and_then(|message| message.reply_to_sender.clone()),
        reply_fallback_start,
        reply_fallback_end,
        markup_spans: parsed
            .as_ref()
            .map(|message| markup_spans_to_js(message.markup_spans.clone()))
            .unwrap_or_default(),
        broadcast_mention: parsed
            .as_ref()
            .and_then(|message| message.broadcast_mention.clone()),
        mention_uris: parsed
            .as_ref()
            .map(|message| message.mention_uris.clone())
            .unwrap_or_default(),
        forum_post_kind: parsed
            .as_ref()
            .and_then(|message| message.forum_post_kind.clone()),
        forum_title: parsed
            .as_ref()
            .and_then(|message| message.forum_title.clone()),
        forum_thread_title,
        is_sticker: parsed.as_ref().is_some_and(|message| message.is_sticker),
        author_real_jid: archived.author_real_jid,
        shared_files: parsed
            .as_ref()
            .map(|message| {
                message
                    .shared_files
                    .iter()
                    .cloned()
                    .map(shared_file_to_js)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn mam_page_to_js(page: waddle_xmpp_client::MamPage) -> WaddleMamPage {
    WaddleMamPage {
        messages: page.messages.into_iter().map(archived_to_js).collect(),
        first_id: page.rsm.first,
        last_id: page.rsm.last,
        is_complete: page.is_complete,
    }
}

fn inbox_result_to_js(result: discovery::WaddleInboxResult) -> WaddleInboxResult {
    WaddleInboxResult {
        total_unread: result.total_unread.unwrap_or(0),
        conversations: result
            .conversations
            .into_iter()
            .filter_map(|conversation| {
                Some(WaddleInboxConversation {
                    partner: conversation.partner,
                    kind: conversation.kind,
                    last_stanza_id: conversation.last_stanza_id?,
                    last_updated: i64::try_from(conversation.last_updated?).ok()?,
                    unread: conversation.unread,
                    preview: conversation.preview,
                    thread: conversation.thread,
                    thread_title: conversation.thread_title,
                    reply_count: conversation.reply_count,
                    author: conversation.author,
                })
            })
            .collect(),
    }
}

fn shared_file_to_js(file: messaging::SharedFile) -> WaddleSharedFile {
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

fn presence_to_js(presence: InboundPresence) -> WaddlePresence {
    WaddlePresence {
        from: presence.from,
        to: presence.to,
        presence_type: presence
            .presence_type
            .unwrap_or_else(|| "available".to_string()),
        show: presence.show,
        status: presence.status,
        hats: presence
            .hats
            .into_iter()
            .map(|hat| WaddlePresenceHat {
                uri: hat.uri,
                title: hat.title,
            })
            .collect(),
        muc_affiliation: presence.muc_affiliation.map(muc_affiliation_to_string),
        muc_role: presence.muc_role.map(muc_role_to_string),
        muc_jid: presence.muc_jid,
        vcard_avatar: presence.vcard_avatar,
    }
}

fn muc_affiliation_to_string(value: MucAffiliation) -> String {
    match value {
        MucAffiliation::Owner => "owner",
        MucAffiliation::Admin => "admin",
        MucAffiliation::Member => "member",
        MucAffiliation::Outcast => "outcast",
        MucAffiliation::None => "none",
    }
    .to_string()
}

fn muc_role_to_string(value: MucRole) -> String {
    match value {
        MucRole::Moderator => "moderator",
        MucRole::Participant => "participant",
        MucRole::Visitor => "visitor",
        MucRole::None => "none",
    }
    .to_string()
}

fn bare_jid(jid: &str) -> String {
    jid.split('/').next().unwrap_or(jid).to_string()
}

fn jid_domain(jid: &str) -> String {
    let bare = bare_jid(jid);
    bare.split('@')
        .next_back()
        .unwrap_or(bare.as_str())
        .to_string()
}

fn parse_muc_affiliation(value: &str) -> Result<MucAffiliation, JsValue> {
    match value {
        "owner" => Ok(MucAffiliation::Owner),
        "admin" => Ok(MucAffiliation::Admin),
        "member" => Ok(MucAffiliation::Member),
        "outcast" => Ok(MucAffiliation::Outcast),
        "none" => Ok(MucAffiliation::None),
        _ => Err(js_error(format!("invalid affiliation: {value}"))),
    }
}

fn message_delivery_stanza_id(element: &Element) -> Option<StanzaId> {
    if element.name() != "message" {
        return None;
    }

    element.attr("id").and_then(|id| StanzaId::new(id).ok())
}

fn is_stream_management_enable(element: &Element) -> bool {
    element.name() == "enable" && element.ns() == waddle_xmpp_client::stream_management::NS_SM
}

fn to_js_value<T: Serialize + ?Sized>(value: &T) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(value).map_err(|err| js_error(err.to_string()))
}

fn js_error(message: impl ToString) -> JsValue {
    JsValue::from_str(&message.to_string())
}

#[cfg(test)]
mod extension_route_tests {
    use super::*;

    #[test]
    fn extension_route_items_iq_uses_xep_0060_items_with_limit() {
        let iq = build_extension_route_items_iq(
            "extensions.example.com",
            "urn:waddle:link-board:1:channel:general@muc.example.com:links",
            Some(100),
        );

        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.ns(), NS_CLIENT);
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("extensions.example.com"));
        let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub");
        let items = pubsub.get_child("items", NS_PUBSUB).expect("items");
        assert_eq!(
            items.attr("node"),
            Some("urn:waddle:link-board:1:channel:general@muc.example.com:links")
        );
        assert_eq!(items.attr("max_items"), Some("100"));
    }

    #[test]
    fn parses_xep_0128_route_forms_from_disco_info() {
        let info = discovery::DiscoInfoResult {
            jid: "extensions.example.com".to_string(),
            node: Some("urn:waddle:link-board:1:channel:{room}:links".to_string()),
            identities: Vec::new(),
            features: Vec::new(),
            forms: vec![discovery::DiscoDataForm {
                form_type: Some(EXTENSION_ROUTE_FORM_TYPE.to_string()),
                fields: vec![
                    field("FORM_TYPE", EXTENSION_ROUTE_FORM_TYPE),
                    field("waddle#plugin_id", "link-board"),
                    field("waddle#route_id", "saved-links"),
                    field("waddle#route_label", "Saved Links"),
                    field("waddle#route_scope", "channel"),
                    field("waddle#route_surface", "gallery"),
                    field(
                        "waddle#state_node",
                        "urn:waddle:link-board:1:channel:{room}:links",
                    ),
                    field("waddle#payload_ns", "urn:waddle:link-board:1"),
                ],
            }],
        };

        let routes = parse_extension_routes_from_info(&info, "extensions.example.com");

        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].service_jid, "extensions.example.com");
        assert_eq!(routes[0].plugin_id, "link-board");
        assert_eq!(routes[0].route_id, "saved-links");
        assert_eq!(routes[0].surface, "gallery");
        assert_eq!(
            routes[0].state_node,
            "urn:waddle:link-board:1:channel:{room}:links"
        );
    }

    #[test]
    fn extension_service_candidates_probe_advertised_items_before_domain_fallbacks() {
        let items = vec![
            disco_item("muc.waddle.local"),
            disco_item("upload.waddle.social"),
            disco_item("extensions.waddle.local"),
        ];

        let candidates = extension_command_service_candidates("waddle.social", &items);

        assert_eq!(
            candidates,
            vec![
                "muc.waddle.local",
                "upload.waddle.social",
                "extensions.waddle.local",
                "extensions.waddle.social",
                "waddle.social",
            ]
        );
    }

    #[test]
    fn waddle_extension_service_requires_framework_and_commands_features() {
        assert!(!is_waddle_extension_service(&disco_info(&[
            NS_ADHOC_COMMANDS
        ])));
        assert!(!is_waddle_extension_service(&disco_info(&[
            NS_WADDLE_EXTENSION_1
        ])));
        assert!(is_waddle_extension_service(&disco_info(&[
            NS_WADDLE_EXTENSION_1,
            NS_ADHOC_COMMANDS,
        ])));
    }

    #[test]
    fn parses_extension_item_envelopes_from_pubsub_items() {
        let iq = Element::builder("iq", NS_CLIENT)
            .attr("type", "result")
            .append(
                Element::builder("pubsub", NS_PUBSUB)
                    .append(
                        Element::builder("items", NS_PUBSUB)
                            .attr("node", "urn:waddle:decision-polls:1:channel:general:polls")
                            .append(
                                Element::builder("item", NS_PUBSUB)
                                    .attr("id", "poll-42")
                                    .append(
                                        Element::builder("extension-item", NS_WADDLE_EXTENSION_1)
                                            .append(
                                                Element::builder("title", NS_WADDLE_EXTENSION_1)
                                                    .append("Lunch tomorrow?")
                                                    .build(),
                                            )
                                            .append(
                                                Element::builder("subtitle", NS_WADDLE_EXTENSION_1)
                                                    .append("Open")
                                                    .build(),
                                            )
                                            .append(
                                                Element::builder("link", NS_WADDLE_EXTENSION_1)
                                                    .attr("href", "https://example.org/poll")
                                                    .build(),
                                            )
                                            .append(
                                                Element::builder("field", NS_WADDLE_EXTENSION_1)
                                                    .attr("name", "saved-at")
                                                    .attr("label", "Saved")
                                                    .append("2026-04-27T00:00:00Z")
                                                    .build(),
                                            )
                                            .append(
                                                Element::builder("option", NS_WADDLE_EXTENSION_1)
                                                    .attr("id", "a")
                                                    .attr("label", "Pizza")
                                                    .build(),
                                            )
                                            .append(
                                                Element::builder("action", NS_WADDLE_EXTENSION_1)
                                                    .attr("launch-id", "vote-42")
                                                    .attr("label", "Vote")
                                                    .attr("plugin", "decision-polls")
                                                    .attr("action", "vote")
                                                    .attr(
                                                        "command-node",
                                                        "urn:waddle:extensions:invoke",
                                                    )
                                                    .attr("token", "signed-token")
                                                    .attr("expires-at", "2026-04-27T00:00:00Z")
                                                    .attr("waddle-id", "alice@example.com")
                                                    .build(),
                                            )
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("item", NS_PUBSUB)
                                    .attr("id", "raw")
                                    .append(
                                        Element::builder("saved-item", "urn:waddle:unknown:1")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build();

        let items = parse_extension_route_items_result(&iq);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.as_deref(), Some("poll-42"));
        assert_eq!(items[0].title.as_deref(), Some("Lunch tomorrow?"));
        assert_eq!(items[0].subtitle.as_deref(), Some("Open"));
        assert_eq!(
            items[0].link.as_ref().map(|link| link.href.as_str()),
            Some("https://example.org/poll")
        );
        assert_eq!(items[0].fields[0].name, "saved-at");
        assert_eq!(items[0].fields[0].label.as_deref(), Some("Saved"));
        assert_eq!(items[0].options[0].id, "a");
        assert_eq!(items[0].actions[0].launch.id, "vote-42");
        assert_eq!(items[0].actions[0].launch.launch_token, "signed-token");
    }

    fn field(var: &str, value: &str) -> discovery::DiscoDataField {
        discovery::DiscoDataField {
            var: var.to_string(),
            values: vec![value.to_string()],
        }
    }

    fn disco_item(jid: &str) -> discovery::DiscoItem {
        discovery::DiscoItem {
            jid: jid.to_string(),
            name: None,
            node: None,
        }
    }

    fn disco_info(features: &[&str]) -> discovery::DiscoInfoResult {
        discovery::DiscoInfoResult {
            jid: "example.com".to_string(),
            node: None,
            identities: Vec::new(),
            features: features.iter().map(|feature| feature.to_string()).collect(),
            forms: Vec::new(),
        }
    }
}
