use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use minidom::Element;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::command::XmppCommand;
use crate::config::ClientConfig;
use crate::error::{parse_stanza_error, ClientError, ClientResult};
use crate::event::{ClientEvent, ConnectionEvent};
use crate::request::ClientRequest;
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
                            let _ = self.transport.send(TransportMessage::Element(el)).await;
                        }
                        Some(XmppCommand::SendIq { stanza, responder }) => {
                            let id = stanza.attr("id").map(|s| s.to_string());
                            match self.transport.send(TransportMessage::Element(stanza)).await {
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
                let result = match msg {
                    TransportMessage::Close(_) => self.transport.close().await,
                    other => self.transport.send(other).await,
                };
                if result.is_err() {
                    *self.state.write().unwrap() = self.runtime.snapshot().clone();
                    return false;
                }
            }
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
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::str::FromStr;
    use std::sync::{Arc, Mutex};

    use futures::future::BoxFuture;
    use jid::{BareJid, FullJid};
    use minidom::Element;
    use tokio::sync::{broadcast, mpsc, oneshot};
    use url::Url;

    use super::*;
    use crate::bootstrap::{NS_BIND, NS_SASL, NS_STREAMS};
    use crate::command::XmppCommand;
    use crate::config::{AccessToken, ClientResource, OAuthBearerConfig, WebSocketConfig};
    use crate::error::ClientError;
    use crate::event::{ClientEvent, LifecycleEvent};
    use crate::state::{SessionBinding, SessionPhase, SessionSnapshot};
    use crate::transport::{
        StreamClose, StreamOpen, TransportEvent, TransportMessage, TransportState,
    };
    use crate::ConnectionConfig;

    fn config() -> ClientConfig {
        ClientConfig::new(
            ConnectionConfig::new(BareJid::from_str("waddle.example").unwrap()),
            WebSocketConfig::new(Url::parse("wss://chat.example.com/ws").unwrap()).unwrap(),
            OAuthBearerConfig::new(
                BareJid::from_str("alice@example.com").unwrap(),
                ClientResource::new("macbook").unwrap(),
                AccessToken::new("token"),
            )
            .unwrap(),
        )
        .unwrap()
    }

    // ── helper constructors ───────────────────────────────────────────────────

    fn make_driver_task(
        transport: MockTransport,
    ) -> (
        DriverTask,
        mpsc::Sender<XmppCommand>,
        broadcast::Receiver<ClientEvent>,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<XmppCommand>(64);
        let (evt_tx, evt_rx) = broadcast::channel::<ClientEvent>(256);
        let state = Arc::new(RwLock::new(SessionSnapshot::new()));
        let task = DriverTask {
            runtime: XmppRuntime::new(config()).unwrap(),
            transport: Box::new(transport),
            commands: cmd_rx,
            events: evt_tx,
            state,
            pending_iqs: HashMap::new(),
        };
        (task, cmd_tx, evt_rx)
    }

    // ── IQ correlation unit tests ─────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn driver_resolves_iq_result_to_oneshot() {
        let (mut task, _cmd_tx, _rx) = make_driver_task(MockTransport::new(
            vec![],
            vec![],
            MockTransportShared::default(),
        ));

        let (iq_tx, iq_rx) = oneshot::channel();
        task.pending_iqs.insert("req-1".to_string(), iq_tx);

        let result_el = Element::builder("iq", crate::NS_CLIENT)
            .attr("type", "result")
            .attr("id", "req-1")
            .build();

        task.dispatch_client_event(ClientEvent::IqResult {
            id: "req-1".to_string(),
            element: result_el,
        });

        let result = iq_rx.await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().attr("type"), Some("result"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn driver_resolves_iq_error_to_oneshot() {
        let (mut task, _cmd_tx, _rx) = make_driver_task(MockTransport::new(
            vec![],
            vec![],
            MockTransportShared::default(),
        ));

        let (iq_tx, iq_rx) = oneshot::channel();
        task.pending_iqs.insert("req-1".to_string(), iq_tx);

        let error_el = Element::builder("iq", crate::NS_CLIENT)
            .attr("type", "error")
            .attr("id", "req-1")
            .append(
                Element::builder("error", crate::NS_CLIENT)
                    .attr("type", "cancel")
                    .append(
                        Element::builder("not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
                            .build(),
                    )
                    .build(),
            )
            .build();

        task.dispatch_client_event(ClientEvent::IqResult {
            id: "req-1".to_string(),
            element: error_el,
        });

        let result = iq_rx.await.unwrap();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ClientError::StanzaError(_)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn driver_ignores_iq_with_unknown_id() {
        let (mut task, _cmd_tx, _rx) = make_driver_task(MockTransport::new(
            vec![],
            vec![],
            MockTransportShared::default(),
        ));

        // No pending IQ — dispatch should silently drop the event.
        let result_el = Element::builder("iq", crate::NS_CLIENT)
            .attr("type", "result")
            .attr("id", "unknown")
            .build();

        task.dispatch_client_event(ClientEvent::IqResult {
            id: "unknown".to_string(),
            element: result_el,
        });
        // No panic, no hang: test passes.
    }

    // ── send_iq round-trip through mock ───────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn send_iq_resolves_via_mock_driver() {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<XmppCommand>(1);
        let (evt_tx, _) = broadcast::channel::<ClientEvent>(1);
        let state = Arc::new(RwLock::new(SessionSnapshot::new()));

        let handle = ClientHandle {
            commands: cmd_tx,
            events: evt_tx,
            state,
        };

        let iq = Element::builder("iq", crate::NS_CLIENT)
            .attr("type", "get")
            .attr("id", "test-1")
            .build();

        // Mock driver: read one command and immediately respond.
        tokio::spawn(async move {
            if let Some(XmppCommand::SendIq {
                stanza: _,
                responder,
            }) = cmd_rx.recv().await
            {
                let reply = Element::builder("iq", crate::NS_CLIENT)
                    .attr("type", "result")
                    .attr("id", "test-1")
                    .build();
                let _ = responder.send(Ok(reply));
            }
        });

        let result = handle.send_iq(iq).await.unwrap();
        assert_eq!(result.attr("type"), Some("result"));
    }

    // ── full bootstrap integration tests ─────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn driver_connects_runtime_and_transport_until_ready() {
        let shared = MockTransportShared::default();
        let factory = MockTransportFactory::new(
            MockTransport::new(
                vec![
                    TransportEvent::StateChanged(TransportState::Connecting),
                    TransportEvent::StateChanged(TransportState::Open),
                ],
                vec![
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Open(StreamOpen::from_server(
                            BareJid::from_str("waddle.example").unwrap(),
                        )),
                    ))),
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Element(pre_auth_features()),
                    ))),
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Element(Element::builder("success", NS_SASL).build()),
                    ))),
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Open(StreamOpen::from_server(
                            BareJid::from_str("waddle.example").unwrap(),
                        )),
                    ))),
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Element(post_auth_features()),
                    ))),
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Element(bind_result("bind-1")),
                    ))),
                    // No Ok(None): driver blocks waiting for next event.
                ],
                shared.clone(),
            ),
            false,
        );

        let client = XmppClient::new(config()).unwrap();
        let driver = client.driver_with_factory(factory).unwrap();

        let handle = driver.connect().await.unwrap();

        // Subscribe before yielding so we don't miss any events.
        let mut rx = handle.events();

        // Wait for the session-ready lifecycle event (driver blocks after bind).
        let mut got_ready = false;
        loop {
            match rx.recv().await {
                Ok(ClientEvent::Lifecycle(LifecycleEvent::SessionReady(binding))) => {
                    assert_eq!(
                        binding.jid,
                        FullJid::from_str("alice@example.com/macbook").unwrap()
                    );
                    got_ready = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }

        assert!(got_ready, "expected SessionReady event");
        assert_eq!(handle.state(), ClientState::Ready);
        assert_eq!(
            handle.snapshot().binding,
            Some(SessionBinding {
                jid: FullJid::from_str("alice@example.com/macbook").unwrap(),
                stream_id: None,
                resumable: false,
            })
        );

        let sent = shared.sent_messages();
        assert_eq!(sent.len(), 4, "expected Open, SASL-auth, Open, bind-IQ");
        assert!(matches!(sent[0], TransportMessage::Open(_)));
        assert!(matches!(sent[1], TransportMessage::Element(_)));
        assert!(matches!(sent[2], TransportMessage::Open(_)));
        assert!(matches!(sent[3], TransportMessage::Element(_)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn driver_disconnects_cleanly() {
        let shared = MockTransportShared::default();
        let factory = MockTransportFactory::new(
            MockTransport::new(
                vec![
                    TransportEvent::StateChanged(TransportState::Connecting),
                    TransportEvent::StateChanged(TransportState::Open),
                ],
                vec![
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Open(StreamOpen::from_server(
                            BareJid::from_str("waddle.example").unwrap(),
                        )),
                    ))),
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Element(pre_auth_features()),
                    ))),
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Element(Element::builder("success", NS_SASL).build()),
                    ))),
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Open(StreamOpen::from_server(
                            BareJid::from_str("waddle.example").unwrap(),
                        )),
                    ))),
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Element(post_auth_features()),
                    ))),
                    Ok(Some(TransportEvent::MessageReceived(
                        TransportMessage::Element(bind_result("bind-1")),
                    ))),
                    // Driver blocks here until a command arrives.
                ],
                shared.clone(),
            ),
            false,
        );

        let client = XmppClient::new(config()).unwrap();
        let driver = client.driver_with_factory(factory).unwrap();
        let handle = driver.connect().await.unwrap();
        let mut rx = handle.events();

        // Wait for session ready before disconnecting.
        loop {
            match rx.recv().await {
                Ok(ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_))) => break,
                Ok(_) => {}
                Err(_) => panic!("channel closed before SessionReady"),
            }
        }

        handle.disconnect().await.unwrap();

        // Wait for the Disconnected state change.
        loop {
            match rx.recv().await {
                Ok(ClientEvent::Lifecycle(LifecycleEvent::StateChanged(snapshot)))
                    if snapshot.phase == SessionPhase::Disconnected =>
                {
                    break;
                }
                Ok(_) => {}
                Err(_) => panic!("channel closed before Disconnected"),
            }
        }

        assert_eq!(handle.state(), ClientState::Disconnected);

        let sent = shared.sent_messages();
        assert!(
            matches!(sent.last(), Some(TransportMessage::Close(StreamClose))),
            "last sent should be Close"
        );
        assert_eq!(shared.close_count(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn driver_cleans_up_failed_connects() {
        let factory = MockTransportFactory::new(
            MockTransport::new(vec![], vec![], MockTransportShared::default()),
            true,
        );

        let client = XmppClient::new(config()).unwrap();
        let driver = client.driver_with_factory(factory).unwrap();

        let error = driver.connect().await.unwrap_err();
        assert!(matches!(error, ClientError::TransportClosed));
    }

    // ── mock transport infrastructure ─────────────────────────────────────────

    #[derive(Clone, Default)]
    struct MockTransportShared {
        sent_messages: Arc<Mutex<Vec<TransportMessage>>>,
        close_count: Arc<Mutex<usize>>,
    }

    impl MockTransportShared {
        fn sent_messages(&self) -> Vec<TransportMessage> {
            self.sent_messages.lock().unwrap().clone()
        }

        fn close_count(&self) -> usize {
            *self.close_count.lock().unwrap()
        }
    }

    struct MockTransportFactory {
        transport: Mutex<Option<MockTransport>>,
        fail_connect: bool,
    }

    impl MockTransportFactory {
        fn new(transport: MockTransport, fail_connect: bool) -> Self {
            Self {
                transport: Mutex::new(Some(transport)),
                fail_connect,
            }
        }
    }

    impl WebSocketTransportFactory for MockTransportFactory {
        fn connect<'a>(
            &'a self,
            _config: &'a ClientConfig,
        ) -> BoxFuture<'a, ClientResult<Box<dyn WebSocketTransport>>> {
            Box::pin(async move {
                if self.fail_connect {
                    return Err(ClientError::TransportClosed);
                }
                Ok(Box::new(self.transport.lock().unwrap().take().unwrap())
                    as Box<dyn WebSocketTransport>)
            })
        }
    }

    struct MockTransport {
        pending_events: VecDeque<TransportEvent>,
        next_events: VecDeque<ClientResult<Option<TransportEvent>>>,
        shared: MockTransportShared,
    }

    impl MockTransport {
        fn new(
            pending_events: Vec<TransportEvent>,
            next_events: Vec<ClientResult<Option<TransportEvent>>>,
            shared: MockTransportShared,
        ) -> Self {
            Self {
                pending_events: pending_events.into(),
                next_events: next_events.into(),
                shared,
            }
        }
    }

    impl WebSocketTransport for MockTransport {
        fn drain_events(&mut self) -> Vec<TransportEvent> {
            self.pending_events.drain(..).collect()
        }

        fn send<'a>(&'a mut self, message: TransportMessage) -> BoxFuture<'a, ClientResult<()>> {
            Box::pin(async move {
                self.shared
                    .sent_messages
                    .lock()
                    .unwrap()
                    .push(message.clone());
                self.pending_events
                    .push_back(TransportEvent::MessageSent(message));
                Ok(())
            })
        }

        fn next_event<'a>(&'a mut self) -> BoxFuture<'a, ClientResult<Option<TransportEvent>>> {
            Box::pin(async move {
                if let Some(event) = self.pending_events.pop_front() {
                    return Ok(Some(event));
                }
                if let Some(event) = self.next_events.pop_front() {
                    return event;
                }
                // No more scripted events — park the task until cancelled.
                std::future::pending::<ClientResult<Option<TransportEvent>>>().await
            })
        }

        fn close<'a>(&'a mut self) -> BoxFuture<'a, ClientResult<()>> {
            Box::pin(async move {
                *self.shared.close_count.lock().unwrap() += 1;
                self.shared
                    .sent_messages
                    .lock()
                    .unwrap()
                    .push(TransportMessage::Close(StreamClose));
                self.pending_events.extend([
                    TransportEvent::StateChanged(TransportState::Closing),
                    TransportEvent::MessageSent(TransportMessage::Close(StreamClose)),
                    TransportEvent::StateChanged(TransportState::Closed),
                    TransportEvent::Closed,
                ]);
                Ok(())
            })
        }
    }

    // ── XMPP fixture helpers ──────────────────────────────────────────────────

    fn pre_auth_features() -> Element {
        Element::builder("features", NS_STREAMS)
            .append(
                Element::builder("mechanisms", NS_SASL)
                    .append(
                        Element::builder("mechanism", NS_SASL)
                            .append("OAUTHBEARER")
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    fn post_auth_features() -> Element {
        Element::builder("features", NS_STREAMS)
            .append(Element::builder("bind", NS_BIND).build())
            .build()
    }

    fn bind_result(stanza_id: &str) -> Element {
        Element::builder("iq", crate::NS_CLIENT)
            .attr("id", stanza_id)
            .attr("type", "result")
            .append(
                Element::builder("bind", NS_BIND)
                    .append(
                        Element::builder("jid", NS_BIND)
                            .append("alice@example.com/macbook")
                            .build(),
                    )
                    .build(),
            )
            .build()
    }
}
