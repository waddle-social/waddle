use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures::channel::{mpsc, oneshot};
use futures::{pin_mut, select, FutureExt, SinkExt, StreamExt};
use jid::{BareJid, Jid};
use js_sys::{Function, Promise, Uint8Array};
use minidom::Element;
use serde::{Deserialize, Serialize};
use waddle_xmpp_client::avatar::{
    build_data_request_iq, build_metadata_request_iq, parse_data_response, parse_metadata_response,
};
use waddle_xmpp_client::discovery::{
    self, build_disco_info_iq, build_disco_items_iq, build_upload_slot_iq,
};
use waddle_xmpp_client::error::parse_stanza_error;
use waddle_xmpp_client::mam::{self, build_mam_iq};
use waddle_xmpp_client::messaging::{
    self, build_outbound_message, InboundMessage, InboundPresence, MucAffiliation, MucRole,
    SendMessageOptions, SharedFileDisposition,
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
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{future_to_promise, spawn_local, JsFuture};

const NS_CLIENT: &str = "jabber:client";
const NS_MUC: &str = "http://jabber.org/protocol/muc";

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
    Client(ClientEvent),
    Error(String),
    Disconnected,
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
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<String>,
    pub reply_fallback_start: Option<u32>,
    pub reply_fallback_end: Option<u32>,
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
}

#[derive(Debug, Serialize)]
pub struct WaddleRoom {
    pub jid: String,
    pub name: String,
    pub channel_type: String,
    pub position: i32,
}

#[derive(Debug, Serialize)]
pub struct WaddleAvatar {
    pub jid: String,
    pub id: String,
    pub mime_type: String,
    pub data: Vec<u8>,
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WaddleSendOptions {
    pub stanza_id: Option<String>,
    pub reply: Option<WaddleReplyTarget>,
    pub fallback: Option<WaddleFallbackRange>,
    pub thread: Option<WaddleThreadTarget>,
    pub shared_files: Vec<WaddleSharedFile>,
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
            Ok(serde_wasm_bindgen::to_value(&mam_page_to_js(page))
                .map_err(|err| js_error(err.to_string()))?)
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
            Ok(serde_wasm_bindgen::to_value(&mam_page_to_js(page))
                .map_err(|err| js_error(err.to_string()))?)
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
            let meta_iq = build_metadata_request_iq(&bare);
            let meta_response = match send_iq_command(inner.clone(), meta_iq).await {
                Ok(response) => response,
                Err(err) if err.as_string().is_some() => return Ok(JsValue::NULL),
                Err(err) => return Err(err),
            };
            let Some(info) = parse_metadata_response(&meta_response) else {
                return Ok(JsValue::NULL);
            };

            let data = if let Some(url) = info.url.as_deref() {
                fetch_avatar_url(url).await?
            } else {
                let data_iq = build_data_request_iq(&bare, &info.id);
                let data_response = match send_iq_command(inner, data_iq).await {
                    Ok(response) => response,
                    Err(err) if err.as_string().is_some() => return Ok(JsValue::NULL),
                    Err(err) => return Err(err),
                };
                let Some(base64_text) = parse_data_response(&data_response) else {
                    return Ok(JsValue::NULL);
                };
                let cleaned: String = base64_text.chars().filter(|c| !c.is_whitespace()).collect();
                BASE64_STANDARD
                    .decode(cleaned.as_bytes())
                    .map_err(|err| js_error(format!("invalid avatar data: {err}")))?
            };

            let avatar = WaddleAvatar {
                jid: bare.to_string(),
                id: info.id,
                mime_type: info.mime_type,
                data,
            };
            Ok(serde_wasm_bindgen::to_value(&avatar).map_err(|err| js_error(err.to_string()))?)
        })
    }

    pub fn discover_upload_service(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let domain = {
                let stored = inner.borrow().config.clone();
                jid_domain(&stored.jid).to_string()
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
            Ok(serde_wasm_bindgen::to_value(&upload_slot_to_js(slot))
                .map_err(|err| js_error(err.to_string()))?)
        })
    }

    pub fn list_rooms(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let spaces_domain = {
                let stored = inner.borrow().config.clone();
                format!("spaces.{}", jid_domain(&stored.jid))
            };

            let space_items =
                send_iq_command(inner.clone(), build_disco_items_iq(&spaces_domain, None)).await?;
            let Some(space) = discovery::parse_disco_items_result(&space_items)
                .and_then(|items| items.into_iter().next())
            else {
                return Ok(serde_wasm_bindgen::to_value(&Vec::<WaddleRoom>::new())
                    .map_err(|err| js_error(err.to_string()))?);
            };
            let space_node = space.node.unwrap_or_else(|| space.jid.clone());
            let rooms_result = send_iq_command(
                inner,
                build_disco_items_iq(&spaces_domain, Some(&space_node)),
            )
            .await?;
            let rooms = discovery::parse_disco_items_result(&rooms_result)
                .unwrap_or_default()
                .into_iter()
                .map(|item| WaddleRoom {
                    jid: item.jid,
                    name: item.name.unwrap_or_default(),
                    channel_type: "text".to_string(),
                    position: 0,
                })
                .collect::<Vec<_>>();
            Ok(serde_wasm_bindgen::to_value(&rooms).map_err(|err| js_error(err.to_string()))?)
        })
    }
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
                    .send(DriverEvent::Client(ClientEvent::Connection(
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
                    .send(DriverEvent::Client(ClientEvent::Connection(
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
                    .send(DriverEvent::Client(ClientEvent::Connection(
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
                    .send(DriverEvent::Client(ClientEvent::Connection(
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
                    .send(DriverEvent::Client(ClientEvent::Connection(
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
                let _ = self.event_tx.clone().send(DriverEvent::Client(other)).await;
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
                    .send(DriverEvent::Client(ClientEvent::MessageDelivery(
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
            .send(DriverEvent::Client(ClientEvent::MessageDelivery(
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
            DriverEvent::Client(client_event) => dispatch_client_event(&inner, client_event),
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
            if let Some(callback) = inner.borrow().on_connected.as_ref() {
                let _ = callback.call0(&JsValue::NULL);
            }
        }
        ClientEvent::Messaging(waddle_xmpp_client::MessagingEvent::Message(message)) => {
            if let Some(callback) = inner.borrow().on_message.as_ref() {
                if let Ok(value) = serde_wasm_bindgen::to_value(&inbound_to_js(*message)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::Messaging(waddle_xmpp_client::MessagingEvent::Presence(presence)) => {
            if let Some(callback) = inner.borrow().on_presence.as_ref() {
                if let Ok(value) = serde_wasm_bindgen::to_value(&presence_to_js(presence)) {
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
        reply,
        fallback: options.fallback.map(|range| FallbackRange {
            start: range.start,
            end: range.end,
        }),
        thread: options.thread.map(|thread| ThreadRef {
            id: thread.id,
            parent: thread.parent,
        }),
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

fn inbound_to_js(message: InboundMessage) -> WaddleMessage {
    let (reply_fallback_start, reply_fallback_end) = match message.reply_fallback {
        Some((start, end)) => (Some(start), Some(end)),
        None => (None, None),
    };

    WaddleMessage {
        id: message.id,
        from: message.from,
        to: message.to,
        body: message.body,
        message_type: message.message_type.clone(),
        timestamp: message.timestamp.map(|timestamp| timestamp.to_rfc3339()),
        stanza_id: message.stanza_id,
        origin_id: message.origin_id,
        replaces_id: message.replaces_id,
        retracts_id: message.retracts_id,
        reaction_target_id: message.reaction_target_id,
        reaction_emojis: message.reaction_emojis,
        is_muc: message.message_type == "groupchat",
        thread: message.thread_id.or(message.thread),
        parent_thread_id: message.parent_thread_id,
        reply_to_id: message.reply_to_id,
        reply_to_sender: message.reply_to_sender,
        reply_fallback_start,
        reply_fallback_end,
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

    WaddleArchivedMessage {
        mam_id: archived.mam_id,
        query_id: archived.query_id,
        stanza_id: archived.stanza_id,
        timestamp: archived.timestamp.map(|timestamp| timestamp.to_rfc3339()),
        from: archived.from,
        to: archived.to,
        message_type: archived.message_type,
        body: archived.body,
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

fn jid_domain(jid: &str) -> &str {
    jid.split('@').next_back().unwrap_or(jid)
}

async fn fetch_avatar_url(url: &str) -> Result<Vec<u8>, JsValue> {
    let window = web_sys::window().ok_or_else(|| js_error("window is unavailable"))?;
    let response = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|err| js_error(format!("avatar fetch failed: {:?}", err)))?;
    let response: web_sys::Response = response
        .dyn_into()
        .map_err(|_| js_error("avatar fetch returned an invalid response"))?;

    if !response.ok() {
        return Err(js_error(format!(
            "avatar fetch returned HTTP {}",
            response.status()
        )));
    }

    let buffer = JsFuture::from(
        response
            .array_buffer()
            .map_err(|err| js_error(format!("avatar response missing body: {:?}", err)))?,
    )
    .await
    .map_err(|err| js_error(format!("avatar body read failed: {:?}", err)))?;

    Ok(Uint8Array::new(&buffer).to_vec())
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

fn js_error(message: impl ToString) -> JsValue {
    JsValue::from_str(&message.to_string())
}
