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
use crate::stream_management::SmResumeState;
use crate::transport::{
    DefaultTransportFactory, TransportEvent, TransportMessage, TransportState, WebSocketTransport,
    WebSocketTransportFactory,
};

const UNCLEAN_ABORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

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
            deferred_commands: VecDeque::new(),
            explicit_disconnect: false,
            last_resume_state: None,
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
    deferred_commands: VecDeque<DeferredXmppCommand>,
    /// Set on [`XmppCommand::Disconnect`]. XEP-0198 forbids resuming
    /// across a clean close, so the snapshot publisher pins the
    /// broadcast state to `None` from that point on — mirroring the
    /// wasm driver's `explicit_disconnect` flag.
    explicit_disconnect: bool,
    /// Last broadcast XEP-0198 resume snapshot; publishing is deduped
    /// against it so subscribers only see actual state transitions.
    last_resume_state: Option<SmResumeState>,
}

enum DeferredXmppCommand {
    SendStanza(Element),
    SendIq {
        stanza: Element,
        responder: oneshot::Sender<ClientResult<Element>>,
    },
}

impl DriverTask {
    async fn run(mut self) {
        // Publish the config-seeded resume state (if any) before the
        // first transport event, mirroring the wasm driver's snapshot
        // right after `queue_request(Connect)`.
        self.publish_resume_state_snapshot();

        // Process events the transport queued during connection setup.
        for event in self.transport.drain_events() {
            if !self.apply_transport_event(event).await {
                return;
            }
        }

        loop {
            let now_ms = crate::runtime::monotonic_now_ms();
            let sm_wakeup_ms = self.runtime.next_stream_management_wakeup_in_ms(now_ms);
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
                        Some(command) => {
                            if !self.handle_command(command).await {
                                return;
                            }
                        }
                        None => return,
                    }
                }
                _ = wait_for_stream_management_wakeup(sm_wakeup_ms) => {
                    if !self.handle_stream_management_timer().await {
                        return;
                    }
                }
            }
        }
    }

    async fn handle_stream_management_timer(&mut self) -> bool {
        self.handle_stream_management_timer_at(crate::runtime::monotonic_now_ms())
            .await
    }

    async fn handle_stream_management_timer_at(&mut self, now_ms: u64) -> bool {
        let events = self.runtime.poll_stream_management_at(now_ms);
        let progress_stalled = events.iter().any(|event| {
            matches!(
                event,
                ClientEvent::Connection(ConnectionEvent::StreamManagement(
                    StreamManagementEvent::AckProgressStalled { .. }
                ))
            )
        });

        for event in events {
            if let Some(message) = self.dispatch_client_event(event) {
                if self.send_transport_message(message).await.is_err() {
                    self.terminate_uncleanly().await;
                    return false;
                }
            }
        }

        if progress_stalled {
            self.terminate_uncleanly().await;
            return false;
        }
        true
    }

    async fn handle_command(&mut self, command: XmppCommand) -> bool {
        match command {
            XmppCommand::SendStanza(stanza) => {
                if !self.runtime.can_send_app_stanza() {
                    self.deferred_commands
                        .push_back(DeferredXmppCommand::SendStanza(stanza));
                    return true;
                }

                self.send_stanza_command(stanza).await;
                true
            }
            XmppCommand::SendIq { stanza, responder } => {
                if !self.runtime.can_send_app_stanza() {
                    self.deferred_commands
                        .push_back(DeferredXmppCommand::SendIq { stanza, responder });
                    return true;
                }

                self.send_iq_command(stanza, responder).await;
                true
            }
            XmppCommand::Disconnect => {
                // Pin the resume snapshot to `None` BEFORE closing:
                // XEP-0198 resume must not be attempted across an
                // explicit `</stream>` close (wasm driver parity).
                self.explicit_disconnect = true;
                self.publish_resume_state_snapshot();
                let _ = self.transport.close().await;
                // Drain close events so state reaches Disconnected before we exit.
                for event in self.transport.drain_events() {
                    self.apply_transport_event(event).await;
                }
                false
            }
        }
    }

    async fn send_stanza_command(&mut self, stanza: Element) {
        let maybe_message_id = message_delivery_stanza_id(&stanza);
        if self
            .send_transport_message(TransportMessage::Element(stanza))
            .await
            .is_err()
        {
            if let Some(stanza_id) = maybe_message_id {
                self.emit_message_delivery_failed(stanza_id);
            }
        }
    }

    async fn send_iq_command(
        &mut self,
        stanza: Element,
        responder: oneshot::Sender<ClientResult<Element>>,
    ) {
        let id = stanza.attr("id").map(|s| s.to_string());
        match self
            .send_transport_message(TransportMessage::Element(stanza))
            .await
        {
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

    async fn flush_deferred_commands(&mut self) {
        while self.runtime.can_send_app_stanza() {
            let Some(command) = self.deferred_commands.pop_front() else {
                return;
            };

            match command {
                DeferredXmppCommand::SendStanza(stanza) => {
                    self.send_stanza_command(stanza).await;
                }
                DeferredXmppCommand::SendIq { stanza, responder } => {
                    self.send_iq_command(stanza, responder).await;
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
        self.publish_resume_state_snapshot();

        for evt in client_events {
            if let Some(msg) = self.dispatch_client_event(evt) {
                if self.send_transport_message(msg).await.is_err() {
                    self.terminate_uncleanly().await;
                    return false;
                }
            }
        }

        self.flush_deferred_commands().await;

        *self.state.write().unwrap() = self.runtime.snapshot().clone();
        !is_terminal
    }

    /// End an unfinished XEP-0198 stream without ever sending RFC 7395
    /// `<close/>`. Transport abort is best effort and bounded; runtime
    /// observers still receive explicit Failed and Closed transitions when
    /// the sink errors, hangs, or neglects to queue terminal events.
    async fn terminate_uncleanly(&mut self) {
        self.apply_terminal_transport_event(TransportEvent::StateChanged(TransportState::Failed))
            .await;

        let _ = tokio::time::timeout(UNCLEAN_ABORT_TIMEOUT, self.transport.abort()).await;
        let queued = self.transport.drain_events();
        let mut saw_closed_state = false;
        let mut saw_closed = false;
        for event in queued {
            if matches!(event, TransportEvent::StateChanged(TransportState::Failed)) {
                continue;
            }
            saw_closed_state |=
                matches!(event, TransportEvent::StateChanged(TransportState::Closed));
            saw_closed |= matches!(event, TransportEvent::Closed);
            self.apply_terminal_transport_event(event).await;
        }
        if !saw_closed_state {
            self.apply_terminal_transport_event(TransportEvent::StateChanged(
                TransportState::Closed,
            ))
            .await;
        }
        if !saw_closed {
            self.apply_terminal_transport_event(TransportEvent::Closed)
                .await;
        }
        *self.state.write().unwrap() = self.runtime.snapshot().clone();
    }

    async fn apply_terminal_transport_event(&mut self, event: TransportEvent) {
        let Ok(events) = self.runtime.apply_transport_event(event) else {
            return;
        };
        self.publish_resume_state_snapshot();
        for event in events {
            let _ = self.dispatch_client_event(event);
        }
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
                let _ =
                    self.events
                        .send(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                            StreamManagementEvent::Resumed { h },
                        )));
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
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Failed,
            )) => {
                let _ =
                    self.events
                        .send(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                            StreamManagementEvent::Failed,
                        )));
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
                Ok(())
            }
            other => self.transport.send(other).await,
        }
    }

    /// Broadcast the current XEP-0198 resume snapshot when it differs
    /// from the last published one. Runs at the same semantic points
    /// the wasm driver refreshes its snapshot cell: once at task
    /// start, after every runtime transport transition, and (forced
    /// to `None`) on explicit disconnect. Deduped by value so the
    /// broadcast bus only carries actual transitions.
    fn publish_resume_state_snapshot(&mut self) {
        let resume_state = if self.explicit_disconnect {
            None
        } else {
            self.runtime.resume_state()
        };
        if resume_state == self.last_resume_state {
            return;
        }
        self.last_resume_state = resume_state.clone();
        let _ = self
            .events
            .send(ClientEvent::ResumeStateChanged(resume_state));
    }

    fn emit_message_delivery_failed(&self, stanza_id: StanzaId) {
        let _ = self
            .events
            .send(ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed {
                stanza_id,
            }));
    }
}

async fn wait_for_stream_management_wakeup(delay_ms: Option<u64>) {
    match delay_ms {
        Some(delay_ms) => tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await,
        None => std::future::pending::<()>().await,
    }
}

fn message_delivery_stanza_id(element: &Element) -> Option<StanzaId> {
    if element.name() != "message" {
        return None;
    }

    element.attr("id").and_then(|id| StanzaId::new(id).ok())
}

#[cfg(test)]
mod tests;
