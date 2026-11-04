//! XEP-0198 §5: resuming an unfinished session also replaces its live stream.

use jid::FullJid;
use minidom::Element;
use std::time::Duration;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::parser::ns::{STREAM, STREAMS};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::SM_NS;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::ping::Ping;

const ACCOUNTS: &[(&str, &str)] = &[
    ("alice", "attached-resume-alice"),
    ("bob", "attached-resume-bob"),
];

async fn send(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    client
        .send(&String::from_utf8(bytes).expect("XML UTF-8"))
        .await
        .expect("send XML");
}

async fn recv(client: &mut WsXmppClient) -> Element {
    client
        .recv()
        .await
        .expect("server frame")
        .parse()
        .expect("server XML")
}

async fn authenticate(server: &TestServer, account: usize) -> WsXmppClient {
    let mut client = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("fresh WebSocket");
    client
        .authenticate("localhost", ACCOUNTS[account].0, ACCOUNTS[account].1)
        .await
        .expect("SASL authentication");
    assert!(
        client.full_jid.is_none(),
        "resume precedes resource binding"
    );
    client
}

async fn original(server: &TestServer) -> (WsXmppClient, SmSessionId) {
    let mut client = authenticate(server, 0).await;
    client.bind("phone").await.expect("original resource bind");
    send(
        &mut client,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled = recv(&mut client).await;
    assert!(enabled.is("enabled", SM_NS));
    assert!(matches!(enabled.attr("resume"), Some("true" | "1")));
    let stream = SmSessionId::new(enabled.attr("id").expect("resumable stream ID"));
    (client, stream)
}

async fn resume(client: &mut WsXmppClient, stream: &SmSessionId, handled: u32) -> Element {
    send(
        client,
        Element::builder("resume", SM_NS)
            .attr(
                minidom::rxml::xml_ncname!("previd").to_owned(),
                stream.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("h").to_owned(),
                handled.to_string(),
            )
            .build(),
    )
    .await;
    recv(client).await
}

async fn ping(client: &mut WsXmppClient, id: &str) {
    send(client, Iq::from_get(id, Ping).into()).await;
    let result = client
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.attr("id") == Some(id))
        })
        .await
        .expect("ping result");
    let result: Element = result.parse().expect("ping XML");
    assert!(result.is("iq", xmpp_parsers::ns::JABBER_CLIENT));
    assert_eq!(result.attr("type"), Some("result"));
}

async fn assert_handled(client: &mut WsXmppClient, handled: u32) {
    send(client, Element::builder("r", SM_NS).build()).await;
    let ack = client
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.is("a", SM_NS))
        })
        .await
        .expect("SM acknowledgement");
    let ack: Element = ack.parse().expect("ack XML");
    assert_eq!(ack.attr("h"), Some(handled.to_string().as_str()));
}

async fn send_message(client: &mut WsXmppClient, to: FullJid, id: &str) {
    let mut message = Message::new(Some(to.into()));
    message.id = Some(Id(id.to_owned()));
    message.type_ = MessageType::Chat;
    message.bodies.insert(Lang::new(), id.to_owned());
    send(client, message.into()).await;
}

async fn receive_message(client: &mut WsXmppClient, id: &str) -> Message {
    let frame = client
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == "message" && element.attr("id") == Some(id))
        })
        .await
        .expect("routed message");
    Message::try_from(frame.parse::<Element>().expect("message XML")).expect("typed message")
}

/// A close must arrive; the SHOULD-level conflict is inspected when present.
async fn assert_replaced_transport_closes(client: &mut WsXmppClient) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match client.recv_raw_timeout(Duration::from_secs(10)).await {
                Ok(Some(frame)) if frame.is_close() => return,
                Ok(Some(frame)) => {
                    if let Ok(text) = frame.to_text() {
                        let element: Element = text.parse().expect("old stream XML");
                        if element.is("error", STREAM) {
                            assert!(element.get_child("conflict", STREAMS).is_some());
                        }
                    }
                }
                Ok(None) => return,
                Err(error) if !error.contains("Timeout") => return,
                Err(error) => panic!("old transport did not close: {error}"),
            }
        }
    })
    .await
    .expect("replaced transport closes promptly");
}

#[tokio::test]
async fn attached_resume_replays_in_order_and_retires_the_original_transport() {
    let server = TestServer::start_with_extra_accounts(ACCOUNTS);
    let (mut old, stream) = original(&server).await;
    let old_jid: FullJid = old
        .full_jid
        .as_deref()
        .expect("bound JID")
        .parse()
        .expect("full JID");
    for id in ["acked", "retained-first", "retained-second"] {
        ping(&mut old, id).await;
    }
    assert_handled(&mut old, 3).await;

    // Keep the original socket alive and send no XMPP close or transport drop.
    let mut replacement = authenticate(&server, 0).await;
    let response = resume(&mut replacement, &stream, 1).await;
    assert!(
        response.is("resumed", SM_NS),
        "resume response: {response:?}"
    );
    assert_eq!(response.attr("previd"), Some(stream.as_str()));
    assert_eq!(response.attr("h"), Some("3"));
    for id in ["retained-first", "retained-second"] {
        let replay = recv(&mut replacement).await;
        assert!(replay.is("iq", xmpp_parsers::ns::JABBER_CLIENT));
        assert_eq!(replay.attr("id"), Some(id));
        assert_eq!(replay.attr("type"), Some("result"));
    }

    // Cleanup is complete by <resumed/>. These writes check the retired
    // socket stays unusable and successor routing survives its later drop.
    // Interruption during writes/drains is exercised by the transport tests.
    for element in [
        Element::builder("a", SM_NS)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "3")
            .build(),
        Iq::from_get("late-old-ping", Ping).into(),
    ] {
        let mut bytes = Vec::new();
        element.write_to(&mut bytes).expect("serialize stale frame");
        let _ = old
            .send(&String::from_utf8(bytes).expect("XML UTF-8"))
            .await;
    }
    assert_replaced_transport_closes(&mut old).await;
    drop(old);

    ping(&mut replacement, "successor-ping").await;
    assert_handled(&mut replacement, 4).await;
    let mut peer = authenticate(&server, 1).await;
    peer.bind("observer").await.expect("peer binds");
    let peer_jid: FullJid = peer
        .full_jid
        .as_deref()
        .expect("peer JID")
        .parse()
        .expect("full JID");
    send_message(&mut replacement, peer_jid, "same-bound-jid").await;
    let routed = receive_message(&mut peer, "same-bound-jid").await;
    assert_eq!(routed.from, Some(old_jid.clone().into()));
    send_message(&mut peer, old_jid, "new-owner-routing").await;
    receive_message(&mut replacement, "new-owner-routing").await;
    replacement.close().await.expect("successor closes");
    peer.close().await.expect("peer closes");
}

#[tokio::test]
async fn another_principal_cannot_detach_the_attached_owner() {
    let server = TestServer::start_with_extra_accounts(ACCOUNTS);
    let (mut owner, stream) = original(&server).await;
    let mut stranger = authenticate(&server, 1).await;
    let failed = resume(&mut stranger, &stream, 0).await;
    assert!(failed.is("failed", SM_NS), "resume response: {failed:?}");
    assert!(failed
        .get_child("not-authorized", xmpp_parsers::ns::XMPP_STANZAS)
        .is_some());
    // Positive barriers prove both the old stream and its SM state survived.
    ping(&mut owner, "owner-still-live").await;
    assert_handled(&mut owner, 1).await;
    let mut legitimate = authenticate(&server, 0).await;
    let response = resume(&mut legitimate, &stream, 1).await;
    assert!(
        response.is("resumed", SM_NS),
        "resume response: {response:?}"
    );
    assert_eq!(response.attr("h"), Some("1"));
    ping(&mut legitimate, "authorized-successor").await;
    assert_handled(&mut legitimate, 2).await;
    legitimate
        .close()
        .await
        .expect("legitimate successor closes");
    stranger.close().await.expect("stranger closes");
}

#[tokio::test]
async fn ahead_resume_counter_does_not_detach_the_attached_owner() {
    let server = TestServer::start_with_extra_accounts(ACCOUNTS);
    let (mut owner, stream) = original(&server).await;
    ping(&mut owner, "before-invalid-resume").await;
    assert_handled(&mut owner, 1).await;

    let mut invalid = authenticate(&server, 0).await;
    let error = resume(&mut invalid, &stream, 2).await;
    assert!(error.is("error", STREAM), "resume response: {error:?}");
    assert!(error.get_child("undefined-condition", STREAMS).is_some());
    assert!(error.get_child("handled-count-too-high", SM_NS).is_some());

    // Rejecting an impossible count must leave the live owner and its
    // counters intact, so it can continue before a later valid takeover.
    ping(&mut owner, "owner-survives-ahead-counter").await;
    assert_handled(&mut owner, 2).await;
    let mut legitimate = authenticate(&server, 0).await;
    let response = resume(&mut legitimate, &stream, 2).await;
    assert!(
        response.is("resumed", SM_NS),
        "resume response: {response:?}"
    );
    assert_eq!(response.attr("h"), Some("2"));
    ping(&mut legitimate, "successor-after-ahead-counter").await;
    assert_handled(&mut legitimate, 3).await;
    legitimate
        .close()
        .await
        .expect("legitimate successor closes");
}

#[tokio::test]
async fn regressed_resume_counter_preserves_the_attached_owner_and_routing() {
    let server = TestServer::start_with_extra_accounts(ACCOUNTS);
    let (mut owner, stream) = original(&server).await;
    let owner_jid: FullJid = owner
        .full_jid
        .as_deref()
        .expect("bound JID")
        .parse()
        .expect("full JID");
    for id in ["acknowledged-first", "acknowledged-second"] {
        ping(&mut owner, id).await;
    }
    send(
        &mut owner,
        Element::builder("a", SM_NS)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "2")
            .build(),
    )
    .await;
    // The response to the following request is a same-socket barrier:
    // last_acked is already two before the replacement supplies one.
    assert_handled(&mut owner, 2).await;

    let mut invalid = authenticate(&server, 0).await;
    let failed = resume(&mut invalid, &stream, 1).await;
    assert!(failed.is("failed", SM_NS), "resume response: {failed:?}");
    assert!(failed
        .get_child("resource-constraint", xmpp_parsers::ns::XMPP_STANZAS)
        .is_some());
    ping(&mut owner, "owner-survives-regressed-counter").await;
    assert_handled(&mut owner, 3).await;

    let mut peer = authenticate(&server, 1).await;
    peer.bind("observer").await.expect("peer binds");
    send_message(&mut peer, owner_jid.clone(), "original-owner-routing").await;
    receive_message(&mut owner, "original-owner-routing").await;

    let mut legitimate = authenticate(&server, 0).await;
    let response = resume(&mut legitimate, &stream, 4).await;
    assert!(
        response.is("resumed", SM_NS),
        "resume response: {response:?}"
    );
    assert_eq!(response.attr("h"), Some("3"));
    ping(&mut legitimate, "successor-after-regressed-counter").await;
    assert_handled(&mut legitimate, 4).await;
    send_message(&mut peer, owner_jid, "successor-routing").await;
    receive_message(&mut legitimate, "successor-routing").await;
    legitimate
        .close()
        .await
        .expect("legitimate successor closes");
    invalid.close().await.expect("rejected replacement closes");
    peer.close().await.expect("peer closes");
}

#[tokio::test]
async fn intentional_logout_cannot_be_force_resumed() {
    let server = TestServer::start_with_extra_accounts(ACCOUNTS);
    let (owner, stream) = original(&server).await;
    owner.close().await.expect("terminal XMPP close");
    let mut replacement = authenticate(&server, 0).await;
    let failed = resume(&mut replacement, &stream, 0).await;
    assert!(failed.is("failed", SM_NS), "resume response: {failed:?}");
    replacement.close().await.expect("replacement closes");
}

#[tokio::test]
async fn failed_resume_can_bind_and_enable_a_fresh_session_on_the_same_websocket() {
    let server = TestServer::start_with_extra_accounts(ACCOUNTS);
    let (owner, stream) = original(&server).await;
    owner.close().await.expect("terminal XMPP close");

    let mut replacement = authenticate(&server, 0).await;
    let failed = resume(&mut replacement, &stream, 0).await;
    assert!(failed.is("failed", SM_NS), "resume response: {failed:?}");
    assert!(failed
        .get_child("item-not-found", xmpp_parsers::ns::XMPP_STANZAS)
        .is_some());

    // This is the same client and physical WebSocket used for the failed
    // resume. Binding and exchanging a ping prove the transport is still live.
    replacement
        .bind("fresh-after-failure")
        .await
        .expect("fresh bind");
    send(
        &mut replacement,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled = recv(&mut replacement).await;
    assert!(enabled.is("enabled", SM_NS));
    assert!(matches!(enabled.attr("resume"), Some("true" | "1")));
    assert_ne!(enabled.attr("id").expect("new SM ID"), stream.as_str());
    assert_handled(&mut replacement, 0).await;
    ping(&mut replacement, "fresh-session-ping").await;
    assert_handled(&mut replacement, 1).await;
    replacement.close().await.expect("fresh session closes");
}

#[cfg(feature = "test-support")]
mod seams {
    use super::*;
    use waddle_ws_test_support::FailedResumeSeam;

    const ALICE_BARE: &str = "alice@localhost";
    const ALICE_PHONE: &str = "alice@localhost/phone";

    async fn wait_for(seam: &FailedResumeSeam, event: String) {
        seam.wait_for_event(&event)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
    }

    #[tokio::test]
    #[ignore = "unignore after every local and remote endpoint enqueue path is provisional-gated"]
    async fn published_resume_gap_detaches_then_same_socket_rebind_survives_delayed_cleanup() {
        let (server, seam) =
            TestServer::start_with_failed_resume_seam(ACCOUNTS, ALICE_BARE, ALICE_PHONE);
        let (mut owner, stream) = original(&server).await;
        let stream_id = stream.as_str();
        seam.arm_stream(stream_id);

        let mut peer = authenticate(&server, 1).await;
        peer.bind("observer").await.expect("peer binds");
        let mut replacement = authenticate(&server, 0).await;
        send(
            &mut replacement,
            Element::builder("resume", SM_NS)
                .attr(minidom::rxml::xml_ncname!("previd").to_owned(), stream_id)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                .build(),
        )
        .await;

        wait_for(
            &seam,
            format!("published {stream_id} {ALICE_PHONE} lifecycle=running"),
        )
        .await;
        seam.continue_finalization();
        wait_for(
            &seam,
            format!("gap-injected {stream_id} {ALICE_PHONE} claimed-session"),
        )
        .await;
        let failed = recv(&mut replacement).await;
        assert!(failed.is("failed", SM_NS), "resume response: {failed:?}");
        assert!(failed
            .get_child("resource-constraint", xmpp_parsers::ns::XMPP_STANZAS)
            .is_some());
        wait_for(
            &seam,
            format!("detached {stream_id} {ALICE_PHONE} lifecycle=detached"),
        )
        .await;
        assert_replaced_transport_closes(&mut owner).await;

        // The failed-resume socket remains the same `replacement` WebSocket.
        replacement
            .bind("phone")
            .await
            .expect("fresh bind after replay gap");
        send(
            &mut replacement,
            Element::builder("enable", SM_NS)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .build(),
        )
        .await;
        let enabled = recv(&mut replacement).await;
        assert!(enabled.is("enabled", SM_NS));
        assert!(matches!(enabled.attr("resume"), Some("true" | "1")));
        assert_ne!(enabled.attr("id"), Some(stream_id));
        assert_handled(&mut replacement, 0).await;
        ping(&mut replacement, "fresh-after-gap-ping").await;
        assert_handled(&mut replacement, 1).await;

        let fresh_jid: FullJid = replacement
            .full_jid
            .as_deref()
            .expect("fresh binding JID")
            .parse()
            .expect("valid fresh binding JID");
        send_message(&mut peer, fresh_jid.clone(), "before-delayed-cleanup").await;
        receive_message(&mut replacement, "before-delayed-cleanup").await;

        seam.release_stale_cleanup();
        wait_for(
            &seam,
            format!("cleanup-finished {stream_id} {ALICE_PHONE} removed=false replacement=true"),
        )
        .await;
        send_message(&mut peer, fresh_jid, "after-delayed-cleanup").await;
        receive_message(&mut replacement, "after-delayed-cleanup").await;

        replacement.close().await.expect("fresh session closes");
        peer.close().await.expect("peer closes");
    }
}
