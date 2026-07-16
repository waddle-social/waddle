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
use crate::state::{SessionBinding, SessionPhase, SessionSnapshot};
use crate::stream_management::{SmResumeState, NS_SM};
use crate::transport::{
    StreamClose, StreamOpen, TransportEvent, TransportMessage, TransportState, TransportWriteResult,
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

fn config_with_resume_state() -> ClientConfig {
    let mut config = config();
    config.session.stream_management.resume_state =
        Some(SmResumeState::new("prev-stream", 0, 0).unwrap());
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
        websocket_close_started: false,
        last_resume_state: None,
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
    assert_eq!(state.previd(), "new-stream");

    assert!(task.handle_command(XmppCommand::Disconnect).await);

    let before_peer_close = drain_client_events(&mut rx);
    assert!(
        !before_peer_close
            .iter()
            .any(|event| matches!(event, ClientEvent::ResumeStateChanged(None))),
        "local stream close must preserve the resume snapshot until the peer replies"
    );

    assert!(
        task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Close(
            StreamClose,
        )))
        .await
    );

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

#[tokio::test(flavor = "current_thread")]
async fn driver_sends_one_ack_request_for_the_first_countable_stanza() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut rx) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;

    task.handle_command(XmppCommand::SendStanza(message_stanza("message-1")))
        .await;
    drain_task_transport_events(&mut task).await;

    let requests = shared
        .sent_messages()
        .into_iter()
        .filter(|message| {
            matches!(message, TransportMessage::Element(element) if element.name() == "r" && element.ns() == NS_SM)
        })
        .count();
    assert_eq!(
        requests, 1,
        "the generated <r/> must reach the transport once"
    );

    let mut request_events = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRequestSent { .. }
            ))
        ) {
            request_events += 1;
        }
    }
    assert_eq!(request_events, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn driver_aborts_uncleanly_after_thirty_seconds_without_ack_progress() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, _rx) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;

    task.handle_command(XmppCommand::SendStanza(message_stanza("message-1")))
        .await;
    drain_task_transport_events(&mut task).await;
    let resumable_before_abort = task.runtime.resume_state();
    assert!(resumable_before_abort.is_some());

    let keep_running = task
        .handle_stream_management_timer_at(
            crate::runtime::monotonic_now_ms().saturating_add(30_001),
        )
        .await;

    assert!(!keep_running);
    assert_eq!(shared.abort_count(), 1);
    assert_eq!(shared.close_count(), 0);
    assert!(
        !shared
            .sent_messages()
            .iter()
            .any(|message| matches!(message, TransportMessage::Close(_))),
        "a stalled resumable stream must not send a clean XML close"
    );
    assert_eq!(task.runtime.resume_state(), resumable_before_abort);
}

#[tokio::test(flavor = "current_thread")]
async fn generated_ack_request_write_failure_forces_terminal_unclean_state() {
    let shared = MockTransportShared::default();
    shared.fail_ack_request_write(TransportWriteResponsibility::PossiblyWritten);
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;

    assert!(
        !task
            .handle_command(XmppCommand::SendStanza(message_stanza("message-1")))
            .await,
        "the failed generated write must terminate the driver"
    );

    assert_eq!(task.runtime.snapshot().phase, SessionPhase::Disconnected);
    let resume = task
        .runtime
        .resume_state()
        .expect("resumable queue retained");
    assert_eq!(
        resume
            .unhandled_message_stanza_ids()
            .iter()
            .map(StanzaId::as_str)
            .collect::<Vec<_>>(),
        vec!["message-1"]
    );
    assert_eq!(shared.abort_count(), 1);
    assert_eq!(shared.close_count(), 0);
    assert!(!shared
        .sent_messages()
        .iter()
        .any(|message| matches!(message, TransportMessage::Close(_))));
    assert_terminal_transport_events_without_ack_request(&mut events);
}

#[tokio::test(flavor = "current_thread")]
async fn generated_ack_request_definitely_not_written_does_not_fail_confirmed_message() {
    let shared = MockTransportShared::default();
    shared.fail_ack_request_write(TransportWriteResponsibility::DefinitelyNotWritten);
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;

    assert!(
        !task
            .handle_command(XmppCommand::SendStanza(message_stanza("confirmed-message")))
            .await
    );

    assert_eq!(resume_stanza_ids(&task), vec!["confirmed-message"]);
    assert_eq!(attempted_stanza_count(&shared, "confirmed-message"), 1);
    assert_eq!(attempted_ack_request_count(&shared), 1);
    let observed = drain_client_events(&mut events);
    assert!(!observed.iter().any(|event| matches!(
        event,
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id })
            if stanza_id.as_str() == "confirmed-message"
    )));
    assert_terminal_transport_event_slice(&observed);
    assert_eq!(shared.abort_count(), 1);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn permanently_pending_original_stanza_is_bounded_and_retained_once() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;
    shared.pend_stanza_write(StanzaId::new("pending-original").unwrap());
    let started = tokio::time::Instant::now();

    assert!(
        !task
            .handle_command(XmppCommand::SendStanza(message_stanza("pending-original")))
            .await
    );

    assert_eq!(
        tokio::time::Instant::now().duration_since(started),
        NATIVE_TRANSPORT_WRITE_DEADLINE
    );
    assert_eq!(resume_stanza_ids(&task), vec!["pending-original"]);
    assert_eq!(attempted_stanza_count(&shared, "pending-original"), 1);
    assert_eq!(attempted_ack_request_count(&shared), 0);
    assert!(!shared
        .sent_messages()
        .iter()
        .any(|message| transport_message_id(message) == Some("pending-original")));
    let observed = drain_client_events(&mut events);
    assert!(!observed.iter().any(|event| matches!(
        event,
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id })
            if stanza_id.as_str() == "pending-original"
    )));
    assert_terminal_transport_event_slice(&observed);
    assert_eq!(shared.abort_count(), 1);
    assert_eq!(shared.close_count(), 0);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn permanently_pending_generated_ack_is_bounded_without_rolling_back_message() {
    let shared = MockTransportShared::default();
    shared.pend_ack_request_write();
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;
    let started = tokio::time::Instant::now();

    assert!(
        !task
            .handle_command(XmppCommand::SendStanza(message_stanza(
                "confirmed-before-pending-r"
            )))
            .await
    );

    assert_eq!(
        tokio::time::Instant::now().duration_since(started),
        NATIVE_TRANSPORT_WRITE_DEADLINE
    );
    assert_eq!(resume_stanza_ids(&task), vec!["confirmed-before-pending-r"]);
    assert_eq!(
        attempted_stanza_count(&shared, "confirmed-before-pending-r"),
        1
    );
    assert_eq!(attempted_ack_request_count(&shared), 1);
    let observed = drain_client_events(&mut events);
    assert!(!observed.iter().any(|event| matches!(
        event,
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id })
            if stanza_id.as_str() == "confirmed-before-pending-r"
    )));
    assert!(!observed.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::AckRequestSent { .. }
        ))
    )));
    assert_terminal_transport_event_slice(&observed);
    assert_eq!(shared.abort_count(), 1);
    assert_eq!(shared.close_count(), 0);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn permanently_pending_stream_close_write_is_unfinished_bounded_and_terminal() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;
    assert!(
        task.handle_command(XmppCommand::SendStanza(message_stanza(
            "resume-after-close-timeout"
        )))
        .await
    );
    shared.pend_stream_close_write();
    let resumable_before = task.runtime.resume_state();
    let started = tokio::time::Instant::now();

    assert!(!task.handle_command(XmppCommand::Disconnect).await);

    assert_eq!(
        tokio::time::Instant::now().duration_since(started),
        NATIVE_TRANSPORT_WRITE_DEADLINE
    );
    assert_eq!(task.runtime.resume_state(), resumable_before);
    assert!(!task.explicit_disconnect);
    assert_eq!(shared.close_count(), 0);
    assert_eq!(shared.abort_count(), 1);
    assert!(!shared
        .sent_messages()
        .iter()
        .any(|message| matches!(message, TransportMessage::Close(_))));
    let observed = drain_client_events(&mut events);
    assert!(!observed
        .iter()
        .any(|event| matches!(event, ClientEvent::ResumeStateChanged(None))));
    assert_terminal_transport_event_slice(&observed);
}

#[tokio::test(flavor = "current_thread")]
async fn peer_initiated_close_writes_reciprocal_before_websocket_close() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, _events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;
    assert!(
        task.handle_command(XmppCommand::SendStanza(message_stanza(
            "peer-close-unacked"
        )))
        .await
    );
    assert!(task.runtime.resume_state().is_some());

    assert!(
        task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Close(
            StreamClose
        ),))
            .await
    );

    let attempted = shared.attempted_messages();
    assert!(matches!(attempted.last(), Some(TransportMessage::Close(_))));
    assert!(matches!(
        shared.sent_messages().last(),
        Some(TransportMessage::Close(_))
    ));
    assert_eq!(shared.close_count(), 1);
    assert!(task.websocket_close_started);
    assert!(task.runtime.stream_close_complete());
    assert!(task.runtime.resume_state().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn local_close_waits_for_peer_before_websocket_close_and_sm_destruction() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, _events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;
    assert!(
        task.handle_command(XmppCommand::SendStanza(message_stanza(
            "local-close-unacked"
        )))
        .await
    );

    assert!(task.handle_command(XmppCommand::Disconnect).await);
    assert!(matches!(
        shared.sent_messages().last(),
        Some(TransportMessage::Close(_))
    ));
    assert_eq!(shared.close_count(), 0);
    assert!(!task.runtime.stream_close_complete());
    assert!(task.runtime.resume_state().is_some());

    assert!(
        task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Close(
            StreamClose
        ),))
            .await
    );
    assert_eq!(shared.close_count(), 1);
    assert!(task.websocket_close_started);
    assert!(task.runtime.stream_close_complete());
    assert!(task.runtime.resume_state().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn failed_peer_close_reciprocal_preserves_resume_state_and_aborts_uncleanly() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;
    assert!(
        task.handle_command(XmppCommand::SendStanza(message_stanza(
            "peer-close-failure"
        )))
        .await
    );
    let resume_before = task.runtime.resume_state();
    shared.fail_stream_close_write(TransportWriteResponsibility::DefinitelyNotWritten);

    assert!(
        !task
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Close(
                StreamClose
            ),))
            .await
    );

    assert_eq!(task.runtime.resume_state(), resume_before);
    assert!(!task.runtime.stream_close_complete());
    assert_eq!(shared.close_count(), 0);
    assert_eq!(shared.abort_count(), 1);
    assert!(matches!(
        shared.attempted_messages().last(),
        Some(TransportMessage::Close(_))
    ));
    assert!(!shared
        .sent_messages()
        .iter()
        .any(|message| matches!(message, TransportMessage::Close(_))));
    assert_terminal_transport_event_slice(&drain_client_events(&mut events));
}

#[tokio::test(flavor = "current_thread")]
async fn direct_message_possibly_written_failure_is_retained_once_and_terminal() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;
    shared.fail_stanza_write(
        StanzaId::new("uncertain-message").unwrap(),
        TransportWriteResponsibility::PossiblyWritten,
    );
    let stanza = message_stanza("uncertain-message");

    assert!(!task.handle_command(XmppCommand::SendStanza(stanza)).await);

    assert_eq!(
        resume_stanza_ids(&task),
        vec!["uncertain-message"],
        "uncertain transport responsibility must enter the SM queue exactly once"
    );
    assert_eq!(
        attempted_stanza_count(&shared, "uncertain-message"),
        1,
        "the failed frame must be attempted exactly once"
    );
    let observed = drain_client_events(&mut events);
    assert!(!observed.iter().any(|event| matches!(
        event,
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id })
            if stanza_id.as_str() == "uncertain-message"
    )));
    assert_terminal_transport_event_slice(&observed);
    assert_eq!(shared.abort_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn direct_iq_possibly_written_failure_is_retained_once_and_terminal() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;
    shared.fail_stanza_write(
        StanzaId::new("uncertain-iq").unwrap(),
        TransportWriteResponsibility::PossiblyWritten,
    );
    let (responder, response) = oneshot::channel();

    assert!(
        !task
            .handle_command(XmppCommand::SendIq {
                stanza: iq_stanza("uncertain-iq"),
                responder,
            })
            .await
    );

    assert!(matches!(
        response.await.expect("IQ responder must resolve"),
        Err(ClientError::Disconnected)
    ));
    assert_eq!(resume_stanza_ids(&task), vec!["uncertain-iq"]);
    assert_eq!(attempted_stanza_count(&shared, "uncertain-iq"), 1);
    assert_terminal_transport_event_slice(&drain_client_events(&mut events));
    assert_eq!(shared.abort_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn definitely_not_written_message_fails_without_entering_resume_queue() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;
    shared.fail_next_write(TransportWriteResponsibility::DefinitelyNotWritten);

    assert!(
        !task
            .handle_command(XmppCommand::SendStanza(message_stanza("not-written")))
            .await
    );

    assert!(resume_stanza_ids(&task).is_empty());
    let observed = drain_client_events(&mut events);
    assert_eq!(
        observed
            .iter()
            .filter(|event| matches!(
                event,
                ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id })
                    if stanza_id.as_str() == "not-written"
            ))
            .count(),
        1
    );
    assert_terminal_transport_event_slice(&observed);
    assert_eq!(shared.abort_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn confirmed_write_before_later_uncertain_failure_is_reconciled_once() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;

    assert!(
        task.handle_command(XmppCommand::SendStanza(message_stanza("confirmed-first")))
            .await
    );
    shared.fail_stanza_write(
        StanzaId::new("uncertain-second").unwrap(),
        TransportWriteResponsibility::PossiblyWritten,
    );
    assert!(
        !task
            .handle_command(XmppCommand::SendStanza(message_stanza("uncertain-second")))
            .await
    );

    assert_eq!(
        resume_stanza_ids(&task),
        vec!["confirmed-first", "uncertain-second"]
    );
    assert_eq!(attempted_stanza_count(&shared, "confirmed-first"), 1);
    assert_eq!(attempted_stanza_count(&shared, "uncertain-second"), 1);
    assert_terminal_transport_event_slice(&drain_client_events(&mut events));
}

#[tokio::test(flavor = "current_thread")]
async fn deferred_failure_stops_later_writes_and_retains_only_responsible_stanzas() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut events) = make_driver_task_with_config(
        config_with_resume_state(),
        MockTransport::new(vec![], vec![], shared.clone()),
    );
    drive_task_to_resume_attempt(&mut task).await;

    for id in ["deferred-first", "deferred-fails", "deferred-never"] {
        assert!(
            task.handle_command(XmppCommand::SendStanza(message_stanza(id)))
                .await
        );
    }
    shared.fail_stanza_write(
        StanzaId::new("deferred-fails").unwrap(),
        TransportWriteResponsibility::PossiblyWritten,
    );

    assert!(
        !task
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                resumed("prev-stream", 0),
            )))
            .await
    );

    assert_eq!(
        resume_stanza_ids(&task),
        vec!["deferred-first", "deferred-fails"]
    );
    assert_eq!(attempted_stanza_count(&shared, "deferred-first"), 1);
    assert_eq!(attempted_stanza_count(&shared, "deferred-fails"), 1);
    assert_eq!(attempted_stanza_count(&shared, "deferred-never"), 0);
    assert_terminal_transport_event_slice(&drain_client_events(&mut events));
}

#[tokio::test(flavor = "current_thread")]
async fn permanently_pending_abort_returns_after_one_second_with_resume_snapshot() {
    let shared = MockTransportShared::default();
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;
    shared.fail_stanza_write(
        StanzaId::new("abort-timeout").unwrap(),
        TransportWriteResponsibility::PossiblyWritten,
    );
    shared.set_abort_pending(true);
    let started = std::time::Instant::now();

    let keep_running = tokio::time::timeout(
        std::time::Duration::from_millis(1_500),
        task.handle_command(XmppCommand::SendStanza(message_stanza("abort-timeout"))),
    )
    .await
    .expect("the bounded abort must return");

    assert!(!keep_running);
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(900),
        "the permanently pending abort should be bounded by the one-second deadline"
    );
    assert_eq!(resume_stanza_ids(&task), vec!["abort-timeout"]);
    assert_eq!(shared.abort_count(), 1);
    assert_terminal_transport_event_slice(&drain_client_events(&mut events));
}

#[tokio::test(flavor = "current_thread")]
async fn abort_failure_still_publishes_terminal_state_and_preserves_resume_snapshot() {
    let shared = MockTransportShared::default();
    shared.set_abort_failure(true);
    let (mut task, _cmd_tx, mut events) =
        make_driver_task(MockTransport::new(vec![], vec![], shared.clone()));
    drive_task_to_sm_enabled(&mut task).await;

    task.handle_command(XmppCommand::SendStanza(message_stanza("message-1")))
        .await;
    drain_task_transport_events(&mut task).await;
    let resume_before = task.runtime.resume_state();
    assert!(resume_before.is_some());

    assert!(!task.handle_stream_management_timer_at(u64::MAX).await);

    assert_eq!(task.runtime.snapshot().phase, SessionPhase::Disconnected);
    assert_eq!(task.runtime.resume_state(), resume_before);
    assert_eq!(shared.abort_count(), 1);
    assert_eq!(shared.close_count(), 0);
    assert!(!shared
        .sent_messages()
        .iter()
        .any(|message| matches!(message, TransportMessage::Close(_))));
    assert_terminal_transport_events(&mut events);
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
        )
        .with_peer_close_after_local_close(),
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
    attempted_messages: Arc<Mutex<Vec<TransportMessage>>>,
    sent_messages: Arc<Mutex<Vec<TransportMessage>>>,
    close_count: Arc<Mutex<usize>>,
    abort_count: Arc<Mutex<usize>>,
    write_failures: Arc<Mutex<VecDeque<MockWriteFailure>>>,
    pending_writes: Arc<Mutex<VecDeque<MockWriteTarget>>>,
    close_pending: Arc<Mutex<bool>>,
    abort_failure: Arc<Mutex<bool>>,
    abort_pending: Arc<Mutex<bool>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MockWriteTarget {
    Any,
    AckRequest,
    StreamClose,
    Stanza(StanzaId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MockWriteFailure {
    target: MockWriteTarget,
    responsibility: TransportWriteResponsibility,
}

impl MockTransportShared {
    fn sent_messages(&self) -> Vec<TransportMessage> {
        self.sent_messages.lock().unwrap().clone()
    }

    fn attempted_messages(&self) -> Vec<TransportMessage> {
        self.attempted_messages.lock().unwrap().clone()
    }

    fn close_count(&self) -> usize {
        *self.close_count.lock().unwrap()
    }

    fn abort_count(&self) -> usize {
        *self.abort_count.lock().unwrap()
    }

    fn fail_next_write(&self, responsibility: TransportWriteResponsibility) {
        self.write_failures
            .lock()
            .unwrap()
            .push_back(MockWriteFailure {
                target: MockWriteTarget::Any,
                responsibility,
            });
    }

    fn fail_ack_request_write(&self, responsibility: TransportWriteResponsibility) {
        self.write_failures
            .lock()
            .unwrap()
            .push_back(MockWriteFailure {
                target: MockWriteTarget::AckRequest,
                responsibility,
            });
    }

    fn fail_stanza_write(&self, stanza_id: StanzaId, responsibility: TransportWriteResponsibility) {
        self.write_failures
            .lock()
            .unwrap()
            .push_back(MockWriteFailure {
                target: MockWriteTarget::Stanza(stanza_id),
                responsibility,
            });
    }

    fn fail_stream_close_write(&self, responsibility: TransportWriteResponsibility) {
        self.write_failures
            .lock()
            .unwrap()
            .push_back(MockWriteFailure {
                target: MockWriteTarget::StreamClose,
                responsibility,
            });
    }

    fn pend_stanza_write(&self, stanza_id: StanzaId) {
        self.pending_writes
            .lock()
            .unwrap()
            .push_back(MockWriteTarget::Stanza(stanza_id));
    }

    fn pend_ack_request_write(&self) {
        self.pending_writes
            .lock()
            .unwrap()
            .push_back(MockWriteTarget::AckRequest);
    }

    fn pend_stream_close_write(&self) {
        self.pending_writes
            .lock()
            .unwrap()
            .push_back(MockWriteTarget::StreamClose);
    }

    fn set_abort_failure(&self, fail: bool) {
        *self.abort_failure.lock().unwrap() = fail;
    }

    fn set_abort_pending(&self, pending: bool) {
        *self.abort_pending.lock().unwrap() = pending;
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
    peer_close_after_local_close: bool,
    peer_close_delivered: bool,
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
            peer_close_after_local_close: false,
            peer_close_delivered: false,
        }
    }

    fn with_peer_close_after_local_close(mut self) -> Self {
        self.peer_close_after_local_close = true;
        self
    }
}

impl WebSocketTransport for MockTransport {
    fn drain_events(&mut self) -> Vec<TransportEvent> {
        self.pending_events.drain(..).collect()
    }

    fn send<'a>(
        &'a mut self,
        message: TransportMessage,
    ) -> BoxFuture<'a, TransportWriteResult<()>> {
        Box::pin(async move {
            self.shared
                .attempted_messages
                .lock()
                .unwrap()
                .push(message.clone());
            let pending = {
                let mut pending_writes = self.shared.pending_writes.lock().unwrap();
                let matches = pending_writes
                    .front()
                    .is_some_and(|target| mock_write_target_matches(target, &message));
                matches.then(|| pending_writes.pop_front())
            };
            if pending.is_some() {
                return std::future::pending::<TransportWriteResult<()>>().await;
            }
            let failure = {
                let mut failures = self.shared.write_failures.lock().unwrap();
                let matches = failures
                    .front()
                    .is_some_and(|failure| mock_write_target_matches(&failure.target, &message));
                matches.then(|| failures.pop_front().expect("front exists"))
            };
            if let Some(failure) = failure {
                return Err(match failure.responsibility {
                    TransportWriteResponsibility::DefinitelyNotWritten => {
                        TransportWriteFailure::definitely_not_written(ClientError::TransportClosed)
                    }
                    TransportWriteResponsibility::PossiblyWritten => {
                        TransportWriteFailure::possibly_written(ClientError::TransportClosed)
                    }
                });
            }
            self.shared
                .sent_messages
                .lock()
                .unwrap()
                .push(message.clone());
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
            if self.peer_close_after_local_close && !self.peer_close_delivered {
                loop {
                    if self
                        .shared
                        .sent_messages()
                        .iter()
                        .any(|message| matches!(message, TransportMessage::Close(_)))
                    {
                        self.peer_close_delivered = true;
                        return Ok(Some(TransportEvent::MessageReceived(
                            TransportMessage::Close(StreamClose),
                        )));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            }
            // No more scripted events — park the task until cancelled.
            std::future::pending::<ClientResult<Option<TransportEvent>>>().await
        })
    }

    fn close_websocket<'a>(&'a mut self) -> BoxFuture<'a, TransportWriteResult<()>> {
        Box::pin(async move {
            *self.shared.close_count.lock().unwrap() += 1;
            if *self.shared.close_pending.lock().unwrap() {
                return std::future::pending::<TransportWriteResult<()>>().await;
            }
            self.pending_events.extend([
                TransportEvent::StateChanged(TransportState::Closing),
                TransportEvent::StateChanged(TransportState::Closed),
                TransportEvent::Closed,
            ]);
            Ok(())
        })
    }

    fn abort<'a>(&'a mut self) -> BoxFuture<'a, ClientResult<()>> {
        Box::pin(async move {
            *self.shared.abort_count.lock().unwrap() += 1;
            if *self.shared.abort_pending.lock().unwrap() {
                return std::future::pending::<ClientResult<()>>().await;
            }
            if *self.shared.abort_failure.lock().unwrap() {
                return Err(ClientError::TransportClosed);
            }
            self.pending_events.extend([
                TransportEvent::StateChanged(TransportState::Closing),
                TransportEvent::StateChanged(TransportState::Closed),
                TransportEvent::Closed,
            ]);
            Ok(())
        })
    }
}

fn mock_write_target_matches(target: &MockWriteTarget, message: &TransportMessage) -> bool {
    match target {
        MockWriteTarget::Any => true,
        MockWriteTarget::AckRequest => matches!(
            message,
            TransportMessage::Element(element)
                if element.name() == "r" && element.ns() == NS_SM
        ),
        MockWriteTarget::StreamClose => matches!(message, TransportMessage::Close(_)),
        MockWriteTarget::Stanza(stanza_id) => matches!(
            message,
            TransportMessage::Element(element)
                if element.attr("id") == Some(stanza_id.as_str())
        ),
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

fn iq_stanza(stanza_id: &str) -> Element {
    Element::builder("iq", crate::NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), stanza_id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
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

async fn drive_task_to_sm_enabled(task: &mut DriverTask) {
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
    task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
        enabled_sm("stream-1"),
    )))
    .await;
    drain_task_transport_events(task).await;
    assert_eq!(task.runtime.snapshot().phase, SessionPhase::Established);
}

async fn drain_task_transport_events(task: &mut DriverTask) {
    loop {
        let events = task.transport.drain_events();
        if events.is_empty() {
            return;
        }
        for event in events {
            assert!(task.apply_transport_event(event).await);
        }
    }
}

fn assert_terminal_transport_events(events: &mut broadcast::Receiver<ClientEvent>) {
    let mut saw_failed = false;
    let mut saw_closed_state = false;
    let mut saw_closed = false;
    while let Ok(event) = events.try_recv() {
        match event {
            ClientEvent::Transport(TransportEvent::StateChanged(TransportState::Failed)) => {
                saw_failed = true;
            }
            ClientEvent::Transport(TransportEvent::StateChanged(TransportState::Closed)) => {
                saw_closed_state = true;
            }
            ClientEvent::Transport(TransportEvent::Closed) => saw_closed = true,
            _ => {}
        }
    }
    assert!(saw_failed, "unclean failure must be observable");
    assert!(
        saw_closed_state,
        "closed transport state must be observable"
    );
    assert!(saw_closed, "terminal Closed event must be observable");
}

fn resume_stanza_ids(task: &DriverTask) -> Vec<String> {
    task.runtime
        .resume_state()
        .map(|state| {
            state
                .unhandled_outbound_stanzas()
                .filter_map(|element| element.attr("id"))
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn attempted_stanza_count(shared: &MockTransportShared, stanza_id: &str) -> usize {
    shared
        .attempted_messages()
        .iter()
        .filter(|message| transport_message_id(message) == Some(stanza_id))
        .count()
}

fn attempted_ack_request_count(shared: &MockTransportShared) -> usize {
    shared
        .attempted_messages()
        .iter()
        .filter(|message| {
            matches!(
                message,
                TransportMessage::Element(element)
                    if element.name() == "r" && element.ns() == NS_SM
            )
        })
        .count()
}

fn drain_client_events(events: &mut broadcast::Receiver<ClientEvent>) -> Vec<ClientEvent> {
    let mut observed = Vec::new();
    while let Ok(event) = events.try_recv() {
        observed.push(event);
    }
    observed
}

fn assert_terminal_transport_event_slice(events: &[ClientEvent]) {
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Transport(TransportEvent::StateChanged(TransportState::Failed))
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Transport(TransportEvent::StateChanged(TransportState::Closed))
    )));
    assert!(events
        .iter()
        .any(|event| matches!(event, ClientEvent::Transport(TransportEvent::Closed))));
}

fn assert_terminal_transport_events_without_ack_request(
    events: &mut broadcast::Receiver<ClientEvent>,
) {
    let mut saw_failed = false;
    let mut saw_closed_state = false;
    let mut saw_closed = false;
    let mut saw_ack_request = false;
    while let Ok(event) = events.try_recv() {
        match event {
            ClientEvent::Transport(TransportEvent::StateChanged(TransportState::Failed)) => {
                saw_failed = true;
            }
            ClientEvent::Transport(TransportEvent::StateChanged(TransportState::Closed)) => {
                saw_closed_state = true;
            }
            ClientEvent::Transport(TransportEvent::Closed) => saw_closed = true,
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRequestSent { .. },
            )) => saw_ack_request = true,
            _ => {}
        }
    }
    assert!(saw_failed, "unclean failure must be observable");
    assert!(saw_closed_state, "closed state must be observable");
    assert!(saw_closed, "terminal Closed event must be observable");
    assert!(
        !saw_ack_request,
        "a failed generated <r/> write must not publish AckRequestSent"
    );
}
