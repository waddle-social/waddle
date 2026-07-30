use super::*;

use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use jid::{BareJid, FullJid};
use minidom::Element;
use tokio::sync::{broadcast, mpsc, oneshot};
use url::Url;

use crate::bootstrap::{NS_BIND, NS_SASL, NS_STREAMS};
use crate::command::XmppCommand;
use crate::config::{AccessToken, ClientResource, OAuthBearerConfig, WebSocketConfig};
use crate::error::ClientError;
use crate::event::{ClientEvent, LifecycleEvent, MessageDeliveryEvent};
use crate::state::StreamId;
use crate::state::{SessionBinding, SessionPhase, SessionSnapshot};
use crate::stream_management::{SmResumeState, NS_SM};
use crate::transport::{StreamClose, StreamOpen, TransportEvent, TransportMessage, TransportState};
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

fn config_with_resume_state() -> ClientConfig {
    let mut config = config();
    config.session.stream_management.resume_state =
        Some(SmResumeState::new(StreamId::new("prev-stream"), 0, 0).unwrap());
    config
}

// ── helper constructors ───────────────────────────────────────────────────

fn make_driver_task(
    transport: MockTransport,
) -> (
    DriverTask,
    mpsc::Sender<XmppCommand>,
    broadcast::Receiver<ClientEvent>,
) {
    make_driver_task_with_config(config(), transport)
}

fn make_driver_task_with_config(
    config: ClientConfig,
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
        runtime: XmppRuntime::new(config).unwrap(),
        transport: Box::new(transport),
        commands: cmd_rx,
        events: evt_tx,
        state,
        pending_iqs: HashMap::new(),
        deferred_commands: VecDeque::new(),
        explicit_disconnect: false,
        last_resume_state: None,
    };
    (task, cmd_tx, evt_rx)
}

async fn confirm_latest_enable_sent(task: &mut DriverTask, shared: &MockTransportShared) {
    let enable = shared
        .sent_messages()
        .into_iter()
        .rev()
        .find(|message| {
            matches!(
                message,
                TransportMessage::Element(element)
                    if element.name() == "enable" && element.ns() == NS_SM
            )
        })
        .expect("transport sent stream-management enable");
    task.apply_transport_event(TransportEvent::MessageSent(enable))
        .await;
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "req-1")
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "error")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "req-1")
        .append(
            Element::builder("error", crate::NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "cancel")
                .append(
                    Element::builder("not-found", "urn:ietf:params:xml:ns:xmpp-stanzas").build(),
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "unknown")
        .build();

    task.dispatch_client_event(ClientEvent::IqResult {
        id: "unknown".to_string(),
        element: result_el,
    });
    // No panic, no hang: test passes.
}

#[tokio::test(flavor = "current_thread")]
async fn driver_cancels_unreplayable_iq_after_fresh_stream_fallback() {
    let (mut task, _cmd_tx, _rx) = make_driver_task(MockTransport::new(
        vec![],
        vec![],
        MockTransportShared::default(),
    ));
    let (iq_tx, iq_rx) = oneshot::channel();
    task.pending_iqs
        .insert("unreplayable-iq".to_string(), iq_tx);

    task.dispatch_client_event(ClientEvent::IqCancelled {
        stanza_id: StanzaId::new("unreplayable-iq").unwrap(),
    });

    assert!(!task.pending_iqs.contains_key("unreplayable-iq"));
    assert!(matches!(
        iq_rx.await.expect("cancelled IQ caller completes"),
        Err(ClientError::RequestCancelled)
    ));
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "test-1")
        .build();

    // Mock driver: read one command and immediately respond.
    tokio::spawn(async move {
        if let Some(XmppCommand::SendIq {
            stanza: _,
            responder,
        }) = cmd_rx.recv().await
        {
            let reply = Element::builder("iq", crate::NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "test-1")
                .build();
            let _ = responder.send(Ok(reply));
        }
    });

    let result = handle.send_iq(iq).await.unwrap();
    assert_eq!(result.attr("type"), Some("result"));
}

#[tokio::test(flavor = "current_thread")]
async fn driver_forwards_core_message_delivery_ack() {
    let (mut task, _cmd_tx, mut rx) = make_driver_task(MockTransport::new(
        vec![],
        vec![],
        MockTransportShared::default(),
    ));

    task.dispatch_client_event(ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked {
        stanza_id: StanzaId::new("core-tracked").unwrap(),
    }));

    let mut got_ack = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
                if stanza_id.as_str() == "core-tracked" =>
            {
                got_ack = true
            }
            _ => {}
        }
    }
    assert!(got_ack, "expected forwarded delivery ack event");
}

#[tokio::test(flavor = "current_thread")]
async fn driver_defers_app_stanzas_until_sm_resume_completes() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, _rx) = make_driver_task_with_config(
        config_with_resume_state(),
        MockTransport::new(vec![], vec![], shared.clone()),
    );

    drive_task_to_resume_attempt(&mut task).await;
    let sent_before_command = shared.sent_messages().len();

    task.handle_command(XmppCommand::SendStanza(message_stanza("queued-1")))
        .await;

    assert_eq!(
        shared.sent_messages().len(),
        sent_before_command,
        "app stanza must stay behind the resume barrier"
    );

    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        resumed("prev-stream", 0),
    )))
    .await;

    let sent = shared.sent_messages();
    assert!(
        sent.iter()
            .any(|message| transport_message_id(message) == Some("queued-1")),
        "queued app stanza should flush after resumed"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn driver_keeps_deferred_stanzas_behind_fresh_fallback_sm_enable() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, _rx) = make_driver_task_with_config(
        config_with_resume_state(),
        MockTransport::new(vec![], vec![], shared.clone()),
    );

    drive_task_to_resume_attempt(&mut task).await;

    task.handle_command(XmppCommand::SendStanza(message_stanza("queued-1")))
        .await;

    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        failed_sm(0),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        bind_result("bind-1"),
    )))
    .await;
    confirm_latest_enable_sent(&mut task, &shared).await;

    assert!(
        !shared
            .sent_messages()
            .iter()
            .any(|message| transport_message_id(message) == Some("queued-1")),
        "fresh fallback must not flush app stanzas before SM is enabled"
    );

    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        enabled_sm("new-stream"),
    )))
    .await;

    let sent = shared.sent_messages();
    assert!(
        sent.iter()
            .any(|message| transport_message_id(message) == Some("queued-1")),
        "queued app stanza should flush after fresh SM enable"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn driver_flushes_deferred_stanzas_when_fresh_sm_enable_fails() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, _rx) = make_driver_task_with_config(
        config_with_resume_state(),
        MockTransport::new(vec![], vec![], shared.clone()),
    );

    drive_task_to_resume_attempt(&mut task).await;

    task.handle_command(XmppCommand::SendStanza(message_stanza("queued-1")))
        .await;

    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        failed_sm(0),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        bind_result("bind-1"),
    )))
    .await;
    confirm_latest_enable_sent(&mut task, &shared).await;

    assert!(
        !shared
            .sent_messages()
            .iter()
            .any(|message| transport_message_id(message) == Some("queued-1")),
        "app stanza should stay deferred until SM either enables or fails"
    );

    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        Element::builder("failed", NS_SM).build(),
    )))
    .await;

    let sent = shared.sent_messages();
    assert!(
        sent.iter()
            .any(|message| transport_message_id(message) == Some("queued-1")),
        "queued app stanza should flush after fresh SM enable fails"
    );
}

// ── XEP-0198 resume snapshot broadcast ────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn driver_broadcasts_resume_state_transitions() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut rx) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));

    task.runtime
        .queue_request(ClientRequest::Connect)
        .expect("connect request should queue");
    task.apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
        .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
        StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        pre_auth_features(),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        Element::builder("success", NS_SASL).build(),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
        StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        post_auth_features_with_sm(),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        bind_result("bind-1"),
    )))
    .await;
    confirm_latest_enable_sent(&mut task, &shared).await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        enabled_sm("new-stream"),
    )))
    .await;

    let mut snapshots = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ClientEvent::ResumeStateChanged(state) = event {
            snapshots.push(state);
        }
    }
    assert_eq!(
        snapshots.len(),
        1,
        "identical snapshots must be deduped; only the <enabled/> transition broadcasts"
    );
    let state = snapshots[0]
        .clone()
        .expect("resumable snapshot after <enabled/>");
    assert_eq!(state.previd().as_str(), "new-stream");

    task.handle_command(XmppCommand::Disconnect).await;

    let mut saw_cleared = false;
    while let Ok(event) = rx.try_recv() {
        if let ClientEvent::ResumeStateChanged(state) = event {
            assert!(
                state.is_none(),
                "explicit disconnect must clear the resume snapshot"
            );
            saw_cleared = true;
        }
    }
    assert!(
        saw_cleared,
        "expected ResumeStateChanged(None) on explicit disconnect"
    );
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

fn post_auth_features_with_sm() -> Element {
    Element::builder("features", NS_STREAMS)
        .append(Element::builder("bind", NS_BIND).build())
        .append(
            Element::builder("sm", NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .build(),
        )
        .build()
}

fn bind_result(stanza_id: &str) -> Element {
    Element::builder("iq", crate::NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), stanza_id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
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

fn message_stanza(stanza_id: &str) -> Element {
    Element::builder("message", crate::NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), stanza_id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .append(
            Element::builder("body", crate::NS_CLIENT)
                .append("queued")
                .build(),
        )
        .build()
}

fn resumed(previd: &str, h: u32) -> Element {
    Element::builder("resumed", NS_SM)
        .attr(minidom::rxml::xml_ncname!("previd").to_owned(), previd)
        .attr(minidom::rxml::xml_ncname!("h").to_owned(), h.to_string())
        .build()
}

fn failed_sm(h: u32) -> Element {
    Element::builder("failed", NS_SM)
        .attr(minidom::rxml::xml_ncname!("h").to_owned(), h.to_string())
        .build()
}

fn enabled_sm(previd: &str) -> Element {
    Element::builder("enabled", NS_SM)
        .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), previd)
        .build()
}

fn transport_message_id(message: &TransportMessage) -> Option<&str> {
    match message {
        TransportMessage::Element(element) => element.attr("id"),
        _ => None,
    }
}

async fn drive_task_to_resume_attempt(task: &mut DriverTask) {
    task.runtime
        .queue_request(ClientRequest::Connect)
        .expect("connect request should queue");
    task.apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
        .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
        StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        pre_auth_features(),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        Element::builder("success", NS_SASL).build(),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
        StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        post_auth_features_with_sm(),
    )))
    .await;
    assert_eq!(task.runtime.snapshot().phase, SessionPhase::Resuming);
}

#[tokio::test(flavor = "current_thread")]
async fn native_driver_routes_stream_management_clock_retries_timeout_and_terminal_close() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut events) = make_driver_task_with_config(
        config_with_resume_state(),
        MockTransport::new(vec![], vec![], shared.clone()),
    );
    drive_task_to_resume_attempt(&mut task).await;
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        resumed("prev-stream", 0),
    )))
    .await;
    task.apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
        message_stanza("clocked"),
    )))
    .await;
    let now = chrono::Utc::now();

    assert!(
        task.poll_stream_management_clock_at(now + chrono::Duration::milliseconds(250))
            .await
    );
    assert!(
        task.poll_stream_management_clock_at(now + chrono::Duration::seconds(5))
            .await
    );
    assert!(
        task.poll_stream_management_clock_at(now + chrono::Duration::seconds(30))
            .await
    );

    let sent = shared.sent_messages();
    assert_eq!(
        sent.iter()
            .filter(|message| matches!(
                message,
                TransportMessage::Element(element) if element.name() == "r" && element.ns() == NS_SM
            ))
            .count(),
        3,
    );
    assert_eq!(shared.close_count(), 1);
    assert!(
        std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRequestTimedOut
            ))
        ))
    );

    assert!(
        task.poll_stream_management_clock_at(now + chrono::Duration::seconds(35))
            .await
    );
    assert_eq!(shared.sent_messages().len(), sent.len());
}

#[tokio::test(flavor = "current_thread")]
async fn native_driver_drops_its_stream_management_clock_on_teardown() {
    let shared = MockTransportShared::default();
    let (task, command_sender, _events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));

    drop(command_sender);
    task.run().await;

    // No detached timer owns the transport after DriverTask exits.
    assert!(shared.sent_messages().is_empty());
    assert_eq!(shared.close_count(), 0);
}
