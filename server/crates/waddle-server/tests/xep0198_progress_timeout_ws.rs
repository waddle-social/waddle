//! XEP-0198 §5: the runtime's no-progress reconnect preserves the server session.

use minidom::Element;
use std::{collections::VecDeque, time::Duration};
use waddle_server::sm::persistence::DatabaseSmPersistence;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::{
    pending_delivery::SmSessionId, stream_management::persistence::SmPersistenceStorage,
};
use waddle_xmpp_client::{
    stream_management::{SmState, SM_PROGRESS_TIMEOUT_MS},
    AccessToken, ClientConfig, ClientEvent, ClientRequest, ClientResource, ConnectionConfig,
    ConnectionEvent, OAuthBearerConfig, SessionBinding, StreamManagementEvent, StreamOpen,
    TransportEvent, TransportMessage, TransportState, WebSocketConfig, XmppRuntime, NS_BIND,
    NS_SASL, NS_SM, NS_STREAMS,
};
use xmpp_parsers::message::{Id, Message, MessageType};

const ACCOUNTS: &[(&str, &str)] = &[("alice", "progress-timeout-password")];

fn runtime(server: &TestServer) -> XmppRuntime {
    XmppRuntime::new(
        ClientConfig::new(
            ConnectionConfig::new("localhost".parse().expect("server JID")),
            WebSocketConfig::new(server.ws_url().parse().expect("WebSocket URL"))
                .expect("transport config"),
            OAuthBearerConfig::new(
                "alice@localhost".parse().expect("account JID"),
                ClientResource::new("progress-timeout").expect("resource"),
                AccessToken::new("unused-by-SCRAM-harness"),
            )
            .expect("auth config"),
        )
        .expect("client config"),
    )
    .expect("runtime")
}

fn receive(runtime: &mut XmppRuntime, element: Element) -> Vec<ClientEvent> {
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
            element,
        )))
        .expect("runtime receives XML")
}

// The real socket is already authenticated by the shared SCRAM harness. The
// runtime currently supports OAUTHBEARER only, so reflect that successful auth
// into its public bootstrap events without sending a second SASL exchange.
// All SM negotiation, counting, timeout decisions and resume writes below are
// driven by the runtime itself and exchanged with the real server.
fn reflect_authenticated_transport(runtime: &mut XmppRuntime) -> Vec<ClientEvent> {
    runtime
        .queue_request(ClientRequest::Connect)
        .expect("connect request");
    runtime
        .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
        .expect("open transport");
    let open = StreamOpen::from_server("localhost".parse().expect("server JID"));
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            open.clone(),
        )))
        .expect("initial stream open");
    receive(
        runtime,
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
            .build(),
    );
    receive(runtime, Element::builder("success", NS_SASL).build());
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
            open,
        )))
        .expect("authenticated stream open");
    receive(
        runtime,
        Element::builder("features", NS_STREAMS)
            .append(Element::builder("bind", NS_BIND).build())
            .append(Element::builder("sm", NS_SM).build())
            .build(),
    )
}

async fn send(client: &mut WsXmppClient, message: &TransportMessage) {
    client
        .send(&waddle_xmpp_client::encode_message(message).expect("typed frame encoding"))
        .await
        .expect("WebSocket write");
}

async fn apply_writes(
    runtime: &mut XmppRuntime,
    client: &mut Option<WsXmppClient>,
    events: Vec<ClientEvent>,
) {
    let mut pending = VecDeque::from(events);
    while let Some(event) = pending.pop_front() {
        match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(message)) => {
                send(client.as_mut().expect("live transport"), &message).await;
                pending.extend(
                    runtime
                        .apply_transport_event(TransportEvent::MessageSent(message))
                        .expect("confirm transport write"),
                );
            }
            ClientEvent::Connection(ConnectionEvent::CloseTransport) => {
                // WsXmppClient::close() sends terminal XMPP <close/>. Disposing
                // only the socket implements this transport-only instruction.
                drop(client.take().expect("transport to retire"));
                runtime
                    .apply_transport_event(TransportEvent::Closed)
                    .expect("transport closed");
            }
            _ => {}
        }
    }
}

async fn recv(client: &mut WsXmppClient) -> Element {
    client
        .recv()
        .await
        .expect("server frame")
        .parse()
        .expect("server XML")
}

#[tokio::test]
async fn progress_timeout_resumes_retained_state_but_logout_ends_the_session() {
    let directory = tempfile::tempdir().expect("database directory");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("progress-timeout.db").display()
    );
    let server = TestServer::start_persistent_with_extra_accounts(&database_url, ACCOUNTS);
    let storage = DatabaseSmPersistence::open(Some(&database_url))
        .await
        .expect("session inspection storage");
    let mut client = Some(
        WsXmppClient::connect_and_auth(
            &server.ws_url(),
            "localhost",
            ACCOUNTS[0].0,
            ACCOUNTS[0].1,
            "progress-timeout",
        )
        .await
        .expect("authenticated connection"),
    );
    let mut runtime = runtime(&server);
    reflect_authenticated_transport(&mut runtime);
    let binding = SessionBinding {
        jid: client
            .as_ref()
            .expect("transport")
            .full_jid
            .as_ref()
            .expect("bound JID")
            .parse()
            .expect("typed bound JID"),
        stream_id: None,
        resumable: true,
    };
    let events = runtime
        .bind_session(binding)
        .expect("reflect resource bind");
    apply_writes(&mut runtime, &mut client, events).await;
    let enabled = recv(client.as_mut().expect("transport")).await;
    assert!(enabled.is("enabled", NS_SM));
    receive(&mut runtime, enabled);
    assert!(runtime.can_send_app_stanza());

    // This unsupported application payload elicits one countable error stanza.
    // Neither that stanza nor the server's ACK is delivered to the runtime:
    // its acknowledgement progress stalls while both peers retain their tails.
    let mut offered = Message::new(Some("alice@localhost".parse().expect("recipient JID")));
    offered.id = Some(Id("progress-timeout-replay".to_owned()));
    offered.type_ = MessageType::Chat;
    offered
        .payloads
        .push(Element::builder("result", waddle_xmpp::xep::NS_INBOX).build());
    let message = TransportMessage::Element(offered.into());
    send(client.as_mut().expect("transport"), &message).await;
    let events = runtime
        .apply_transport_event(TransportEvent::MessageSent(message))
        .expect("record sent stanza");
    apply_writes(&mut runtime, &mut client, events).await;
    let original: Element = client
        .as_mut()
        .expect("transport")
        .recv_matching(|frame| frame.contains("progress-timeout-replay"))
        .await
        .expect("server error stanza")
        .parse()
        .expect("error XML");
    assert_eq!(original.attr("type"), Some("error"));
    let retained = runtime.resume_state().expect("client resume snapshot");
    assert_eq!(retained.inbound_h(), 0);
    assert_eq!(retained.outbound_h(), 1);
    assert!(retained.has_unhandled_outbound_stanzas());
    let sent_at = retained
        .unhandled_outbound_entries()
        .next()
        .expect("client retained stanza")
        .sent_at();
    let events = runtime.poll_stream_management_clock(
        sent_at + chrono::Duration::milliseconds(SM_PROGRESS_TIMEOUT_MS),
    );
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::ReconnectRequired
        ))
    )));
    let sends_gated_immediately = !runtime.can_send_app_stanza();
    apply_writes(&mut runtime, &mut client, events).await;
    assert!(sends_gated_immediately, "gate sends immediately");
    assert!(client.is_none(), "runtime retires only the old transport");
    assert_eq!(runtime.resume_state(), Some(retained.clone()));
    assert!(runtime
        .poll_stream_management_clock(sent_at + chrono::Duration::minutes(1))
        .is_empty());

    let session_id = SmSessionId::new(retained.previd().as_str());
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(session) = storage
                .get_session(&session_id)
                .await
                .expect("inspect detached session")
            {
                assert_eq!(session.inbound_count, 1);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("runtime close preserves a detached server session");

    let mut resumed = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("replacement WebSocket");
    resumed
        .authenticate("localhost", ACCOUNTS[0].0, ACCOUNTS[0].1)
        .await
        .expect("authenticate before resume");
    client = Some(resumed);
    let events = reflect_authenticated_transport(&mut runtime);
    apply_writes(&mut runtime, &mut client, events).await;
    let resumed = recv(client.as_mut().expect("replacement transport")).await;
    assert!(resumed.is("resumed", NS_SM), "resume response: {resumed:?}");
    assert_eq!(resumed.attr("previd"), Some(retained.previd().as_str()));
    assert_eq!(resumed.attr("h"), Some("1"));
    let events = receive(&mut runtime, resumed);
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::MessageDelivery(waddle_xmpp_client::MessageDeliveryEvent::Acked { stanza_id })
            if stanza_id.as_str() == "progress-timeout-replay"
    )));
    apply_writes(&mut runtime, &mut client, events).await;
    assert!(runtime.can_send_app_stanza());
    let settled = runtime.resume_state().expect("same resumed client session");
    assert_eq!(settled.previd(), retained.previd());
    assert_eq!(settled.outbound_h(), 1);
    assert!(!settled.has_unhandled_outbound_stanzas());
    let replay = recv(client.as_mut().expect("replacement transport")).await;
    assert_eq!(replay.attr("id"), original.attr("id"));
    assert_eq!(replay.attr("type"), Some("error"));
    assert_eq!(
        replay.get_child("error", xmpp_parsers::ns::JABBER_CLIENT),
        original.get_child("error", xmpp_parsers::ns::JABBER_CLIENT)
    );
    receive(&mut runtime, replay);
    assert_eq!(runtime.resume_state().expect("resume state").inbound_h(), 1);

    // Intentional logout must still send the terminal XMPP close on this socket.
    let events = runtime
        .queue_request(ClientRequest::Disconnect)
        .expect("logout request");
    apply_writes(&mut runtime, &mut client, events).await;
    let close = client
        .as_mut()
        .expect("logout transport")
        .recv_matching(|frame| {
            matches!(
                waddle_xmpp_client::decode_message(frame),
                Ok(TransportMessage::Close(_))
            )
        })
        .await
        .expect("server confirms terminal XMPP close");
    runtime
        .apply_transport_event(TransportEvent::MessageReceived(
            waddle_xmpp_client::decode_message(&close).expect("typed close"),
        ))
        .expect("terminal close response");
    drop(client.take());
    let mut after_logout = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("post-logout transport");
    after_logout
        .authenticate("localhost", ACCOUNTS[0].0, ACCOUNTS[0].1)
        .await
        .expect("post-logout authentication");
    send(
        &mut after_logout,
        &TransportMessage::Element(SmState::build_resume(retained.previd(), 1)),
    )
    .await;
    let failed = recv(&mut after_logout).await;
    assert!(
        failed.is("failed", NS_SM),
        "logout resume response: {failed:?}"
    );
    assert!(failed
        .get_child("item-not-found", xmpp_parsers::ns::XMPP_STANZAS)
        .is_some());
}
