use super::*;

pub(crate) type DriverResult<T> = Result<T, ClientError>;

#[wasm_bindgen]
pub struct WaddleConfig {
    pub(crate) server_url: String,
    pub(crate) jid: String,
    pub(crate) access_token: String,
    pub(crate) resource: String,
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
pub(crate) struct StoredConfig {
    pub(crate) server_url: String,
    pub(crate) jid: String,
    pub(crate) access_token: String,
    pub(crate) resource: String,
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
    pub(crate) inner: Rc<RefCell<WaddleClientInner>>,
}

pub(crate) struct WaddleClientInner {
    pub(crate) config: StoredConfig,
    pub(crate) cmd_tx: Option<mpsc::Sender<WasmCommand>>,
    pub(crate) on_message: Option<Function>,
    pub(crate) on_presence: Option<Function>,
    pub(crate) on_connected: Option<Function>,
    pub(crate) on_session_lifecycle: Option<Function>,
    pub(crate) on_disconnected: Option<Function>,
    pub(crate) on_error: Option<Function>,
    pub(crate) on_message_delivery_acked: Option<Function>,
    pub(crate) on_message_delivery_failed: Option<Function>,
}

pub(crate) enum WasmCommand {
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

pub(crate) enum DriverEvent {
    Client(Box<ClientEvent>),
    Error(String),
    Disconnected,
}

pub(crate) fn client_driver_event(event: ClientEvent) -> DriverEvent {
    DriverEvent::Client(Box::new(event))
}

pub(crate) struct PendingMamQuery {
    pub(crate) query_id: String,
    pub(crate) messages: Vec<ArchivedMessage>,
    pub(crate) responder: oneshot::Sender<DriverResult<waddle_xmpp_client::MamPage>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackedMessageDelivery {
    pub(crate) stanza_id: StanzaId,
    pub(crate) h: u32,
}

pub(crate) struct WasmDriverTask {
    pub(crate) runtime: XmppRuntime,
    pub(crate) ws: WasmWebSocket,
    pub(crate) cmd_rx: mpsc::Receiver<WasmCommand>,
    pub(crate) event_tx: mpsc::Sender<DriverEvent>,
    pub(crate) pending_iqs: HashMap<String, oneshot::Sender<DriverResult<Element>>>,
    pub(crate) pending_mam_queries: HashMap<String, PendingMamQuery>,
    pub(crate) sm_delivery_tracking_enabled: bool,
    pub(crate) outbound_h: u32,
    pub(crate) pending_message_deliveries: VecDeque<TrackedMessageDelivery>,
}
