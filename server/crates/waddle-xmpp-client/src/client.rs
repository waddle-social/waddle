use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use minidom::Element;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::command::XmppCommand;
use crate::config::ClientConfig;
use crate::error::{parse_stanza_error, ClientError, ClientResult};
use crate::event::{ClientEvent, ConnectionEvent, MessageDeliveryEvent, StreamManagementEvent};
use crate::request::{ClientRequest, StanzaId};
use crate::runtime::XmppRuntime;
use crate::state::{ClientState, SessionSnapshot};
use crate::transport::{
    DefaultTransportFactory, TransportEvent, TransportMessage, TransportState, WebSocketTransport,
    WebSocketTransportFactory,
};

/// Async control plane for a live XMPP session.
///
/// Cheaply cloneable — all clones share the same underlying session.
#[derive(Clone, Debug)]
pub struct ClientHandle {
    commands: mpsc::Sender<XmppCommand>,
    events: broadcast::Sender<ClientEvent>,
    state: Arc<RwLock<SessionSnapshot>>,
}

impl ClientHandle {
    /// Subscribe to the live event stream.
    pub fn events(&self) -> broadcast::Receiver<ClientEvent> {
        self.events.subscribe()
    }

    /// Current session snapshot (reads from shared state without blocking).
    pub fn snapshot(&self) -> SessionSnapshot {
        self.state.read().unwrap().clone()
    }

    /// Current high-level client state.
    pub fn state(&self) -> ClientState {
        self.snapshot().client_state()
    }

    /// Fire-and-forget: queue a raw stanza for sending.
    pub async fn send_stanza(&self, stanza: Element) -> ClientResult<()> {
        self.commands
            .send(XmppCommand::SendStanza(stanza))
            .await
            .map_err(|_| ClientError::Disconnected)
    }

    /// Send an IQ stanza and await a correlated result or error response.
    pub async fn send_iq(&self, stanza: Element) -> ClientResult<Element> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(XmppCommand::SendIq {
                stanza,
                responder: tx,
            })
            .await
            .map_err(|_| ClientError::Disconnected)?;
        rx.await.map_err(|_| ClientError::Disconnected)?
    }

    /// Request a clean disconnect.
    pub async fn disconnect(&self) -> ClientResult<()> {
        self.commands
            .send(XmppCommand::Disconnect)
            .await
            .map_err(|_| ClientError::Disconnected)
    }

    /// Construct a handle directly from its parts. Test-only — production code
    /// receives handles from [`ClientDriver::connect`] so that the accompanying
    /// driver task is guaranteed to be running.
    #[cfg(test)]
    pub(crate) fn from_parts(
        commands: mpsc::Sender<XmppCommand>,
        events: broadcast::Sender<ClientEvent>,
        state: Arc<RwLock<SessionSnapshot>>,
    ) -> Self {
        Self {
            commands,
            events,
            state,
        }
    }
}

/// XMPP client factory: validates configuration and creates drivers.
#[derive(Debug, Clone)]
pub struct XmppClient {
    config: ClientConfig,
}

impl XmppClient {
    pub fn new(config: ClientConfig) -> ClientResult<Self> {
        config.validate()?;
        Ok(Self { config })
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Create a fresh runtime for standalone inspection or testing.
    pub fn runtime(&self) -> ClientResult<XmppRuntime> {
        XmppRuntime::new(self.config.clone())
    }

    pub fn driver(&self) -> ClientResult<ClientDriver<DefaultTransportFactory>> {
        self.driver_with_factory(DefaultTransportFactory)
    }

    pub fn driver_with_factory<TFactory>(
        &self,
        transport_factory: TFactory,
    ) -> ClientResult<ClientDriver<TFactory>>
    where
        TFactory: WebSocketTransportFactory,
    {
        ClientDriver::new(self.config.clone(), transport_factory)
    }
}

/// Connects to the XMPP server and returns an async [`ClientHandle`].
pub struct ClientDriver<TFactory = DefaultTransportFactory>
where
    TFactory: WebSocketTransportFactory,
{
    runtime: XmppRuntime,
    transport_factory: TFactory,
}

impl<TFactory> ClientDriver<TFactory>
where
    TFactory: WebSocketTransportFactory,
{
    pub fn new(config: ClientConfig, transport_factory: TFactory) -> ClientResult<Self> {
        Ok(Self {
            runtime: XmppRuntime::new(config)?,
            transport_factory,
        })
    }

    pub fn config(&self) -> &ClientConfig {
        self.runtime.config()
    }

    /// Connect to the server and return an async control-plane handle.
    ///
    /// Consumes the driver, spawns a background task that owns the transport,
    /// and returns a [`ClientHandle`] that can be cloned and used concurrently.
    pub async fn connect(mut self) -> ClientResult<ClientHandle>
    where
        TFactory: Send + 'static,
    {
        // Transition bootstrap state before opening the transport.
        let _init_events = self.runtime.queue_request(ClientRequest::Connect)?;

        let transport = match self.transport_factory.connect(self.runtime.config()).await {
            Ok(t) => t,
            Err(e) => return Err(e),
        };

        let (cmd_tx, cmd_rx) = mpsc::channel::<XmppCommand>(64);
        let (evt_tx, _) = broadcast::channel::<ClientEvent>(256);
        let state = Arc::new(RwLock::new(self.runtime.snapshot().clone()));

        let handle = ClientHandle {
            commands: cmd_tx,
            events: evt_tx.clone(),
            state: state.clone(),
        };

        let task = DriverTask {
            runtime: self.runtime,
            transport,
            commands: cmd_rx,
            events: evt_tx,
            state,
            pending_iqs: HashMap::new(),
            sm_delivery_tracking_enabled: false,
            outbound_h: 0,
            pending_message_deliveries: VecDeque::new(),
        };

        tokio::spawn(task.run());

        Ok(handle)
    }
}

/// Internal driver task: owns the transport and drives the session loop.
struct DriverTask {
    runtime: XmppRuntime,
    transport: Box<dyn WebSocketTransport>,
    commands: mpsc::Receiver<XmppCommand>,
    events: broadcast::Sender<ClientEvent>,
    state: Arc<RwLock<SessionSnapshot>>,
    pending_iqs: HashMap<String, oneshot::Sender<ClientResult<Element>>>,
    sm_delivery_tracking_enabled: bool,
    outbound_h: u32,
    pending_message_deliveries: VecDeque<TrackedMessageDelivery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedMessageDelivery {
    stanza_id: StanzaId,
    h: u32,
}

impl DriverTask {
    async fn run(mut self) {
        // Process events the transport queued during connection setup.
        for event in self.transport.drain_events() {
            if !self.apply_transport_event(event).await {
                return;
            }
        }

        loop {
            tokio::select! {
                result = self.transport.next_event() => {
                    match result {
                        Ok(Some(event)) => {
                            if !self.apply_transport_event(event).await {
                                return;
                            }
                        }
                        Ok(None) => {
                            self.apply_transport_event(TransportEvent::Closed).await;
                            return;
                        }
                        Err(_) => {
                            self.apply_transport_event(
                                TransportEvent::StateChanged(TransportState::Failed),
                            )
                            .await;
                            self.apply_transport_event(TransportEvent::Closed).await;
                            return;
                        }
                    }
                }
                cmd = self.commands.recv() => {
                    match cmd {
                        Some(XmppCommand::SendStanza(el)) => {
                            let maybe_message_id = message_delivery_stanza_id(&el);
                            match self.send_transport_message(TransportMessage::Element(el)).await {
                                Ok(()) => {}
                                Err(_) => {
                                    if let Some(stanza_id) = maybe_message_id {
                                        self.emit_message_delivery_failed(stanza_id);
                                    }
                                }
                            }
                        }
                        Some(XmppCommand::SendIq { stanza, responder }) => {
                            let id = stanza.attr("id").map(|s| s.to_string());
                            match self.send_transport_message(TransportMessage::Element(stanza)).await {
                                Err(_) => {
                                    let _ = responder.send(Err(ClientError::Disconnected));
                                }
                                Ok(()) => match id {
                                    Some(id) => {
                                        self.pending_iqs.insert(id, responder);
                                    }
                                    None => {
                                        let _ = responder.send(Err(ClientError::Disconnected));
                                    }
                                },
                            }
                        }
                        Some(XmppCommand::Disconnect) => {
                            let _ = self.transport.close().await;
                            // Drain close events so state reaches Disconnected before we exit.
                            for event in self.transport.drain_events() {
                                self.apply_transport_event(event).await;
                            }
                            return;
                        }
                        None => return,
                    }
                }
            }
        }
    }

    /// Apply one transport event; returns `false` when the session is fully closed.
    async fn apply_transport_event(&mut self, event: TransportEvent) -> bool {
        let is_terminal = matches!(event, TransportEvent::Closed);

        let client_events = match self.runtime.apply_transport_event(event) {
            Ok(events) => events,
            Err(_) => {
                *self.state.write().unwrap() = self.runtime.snapshot().clone();
                return false;
            }
        };

        for evt in client_events {
            if let Some(msg) = self.dispatch_client_event(evt) {
                if self.send_transport_message(msg).await.is_err() {
                    self.fail_all_pending_message_deliveries();
                    *self.state.write().unwrap() = self.runtime.snapshot().clone();
                    return false;
                }
            }
        }

        if is_terminal {
            self.fail_all_pending_message_deliveries();
        }

        *self.state.write().unwrap() = self.runtime.snapshot().clone();
        !is_terminal
    }

    /// Dispatch one client event.
    ///
    /// * `OutboundMessage` — broadcast the event and return the message so the
    ///   caller can write it to the transport.
    /// * `IqResult` — silently resolve the pending IQ oneshot; never broadcast.
    /// * Everything else — broadcast and return `None`.
    fn dispatch_client_event(&mut self, event: ClientEvent) -> Option<TransportMessage> {
        match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(msg)) => {
                let msg_clone = msg.clone();
                let _ =
                    self.events
                        .send(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                            msg,
                        )));
                Some(msg_clone)
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Enabled { previd },
            )) => {
                self.sm_delivery_tracking_enabled = true;
                let _ =
                    self.events
                        .send(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                            StreamManagementEvent::Enabled { previd },
                        )));
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Resumed { h },
            )) => {
                self.sm_delivery_tracking_enabled = true;
                let _ =
                    self.events
                        .send(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                            StreamManagementEvent::Resumed { h },
                        )));
                self.emit_acked_message_deliveries(h);
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckReceived { h },
            )) => {
                let _ =
                    self.events
                        .send(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                            StreamManagementEvent::AckReceived { h },
                        )));
                self.emit_acked_message_deliveries(h);
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Failed,
            )) => {
                self.sm_delivery_tracking_enabled = false;
                let _ =
                    self.events
                        .send(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                            StreamManagementEvent::Failed,
                        )));
                self.fail_all_pending_message_deliveries();
                None
            }
            ClientEvent::IqResult { id, element } => {
                if let Some(responder) = self.pending_iqs.remove(&id) {
                    let type_attr = element.attr("type").unwrap_or("").to_string();
                    let result = if type_attr == "result" {
                        Ok(element)
                    } else {
                        Err(ClientError::StanzaError(parse_stanza_error(&element)))
                    };
                    let _ = responder.send(result);
                }
                None
            }
            other => {
                let _ = self.events.send(other);
                None
            }
        }
    }

    async fn send_transport_message(&mut self, message: TransportMessage) -> ClientResult<()> {
        match message {
            TransportMessage::Close(_) => self.transport.close().await,
            TransportMessage::Element(element) => {
                self.transport
                    .send(TransportMessage::Element(element.clone()))
                    .await?;
                self.record_outbound_element(&element);
                Ok(())
            }
            other => self.transport.send(other).await,
        }
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

    fn emit_acked_message_deliveries(&mut self, h: u32) {
        loop {
            let should_ack = self
                .pending_message_deliveries
                .front()
                .is_some_and(|pending| pending.h <= h);
            if !should_ack {
                break;
            }

            if let Some(pending) = self.pending_message_deliveries.pop_front() {
                let _ =
                    self.events
                        .send(ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked {
                            stanza_id: pending.stanza_id,
                        }));
            }
        }
    }

    fn fail_all_pending_message_deliveries(&mut self) {
        while let Some(pending) = self.pending_message_deliveries.pop_front() {
            self.emit_message_delivery_failed(pending.stanza_id);
        }
    }

    fn emit_message_delivery_failed(&self, stanza_id: StanzaId) {
        let _ = self
            .events
            .send(ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed {
                stanza_id,
            }));
    }
}

fn message_delivery_stanza_id(element: &Element) -> Option<StanzaId> {
    if element.name() != "message" {
        return None;
    }

    element.attr("id").and_then(|id| StanzaId::new(id).ok())
}

fn is_stream_management_enable(element: &Element) -> bool {
    element.name() == "enable" && element.ns() == crate::stream_management::NS_SM
}

#[cfg(test)]
mod tests;
