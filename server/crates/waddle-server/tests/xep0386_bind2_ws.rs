//! XEP-0386 Bind 2 over the production WebSocket router with SASL2
//! (XEP-0388): the advertisement inside the SASL2 `<inline/>`, the inline
//! session features a `<bind/>` may request, the generated resource, the
//! offline messages and archive state at the bind, and the end of the
//! client's earlier sessions.
//!
//! The fixture negotiates SASL2 only because it opts into the test
//! capability, and processes Bind 2 only because it selects the durable
//! stream-management authority; a production build does neither by default.
#![cfg(feature = "test-support")]

use axum::serve::Listener;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use jid::{BareJid, FullJid};
use minidom::Element;
use std::future;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use waddle_server::test_support::{
    create_test_session, router, websocket_state_with_sasl2_and_durable_sm,
    websocket_state_with_sasl2_without_durable_sm, WebSocketState,
};
use waddle_xmpp::mam::{ArchivedMessage, MamArchiveKind};
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};
use waddle_xmpp_core::carbons::CARBONS_NS;
use waddle_xmpp_core::disco::Feature;
use waddle_xmpp_core::xep0359::StanzaId;
use waddle_xmpp_core::xep0388::{
    Sasl2Authenticate, Sasl2Data, Sasl2Failure, Sasl2FailureCondition, Sasl2Feature,
    Sasl2Mechanism, Sasl2Success,
};
use waddle_xmpp_core::xep0388_inline::{
    Bind2Bound, Bind2Feature, Bind2Request, Bind2Tag, Sasl2InlineOffer, Sasl2InlineRequests,
    Sasl2InlineResults, NS_BIND2, NS_SM,
};
use xmpp_parsers::message::{Message as XmppMessage, MessageType};

const TIMEOUT: Duration = Duration::from_secs(5);
const NS_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
const NS_STREAM: &str = "http://etherx.jabber.org/streams";
const NS_MAM: &str = "urn:xmpp:mam:2";
const CLIENT_A: &str = "d4565fa7-4d72-4749-b3d3-740edbf87770";
const CLIENT_B: &str = "6ba7b810-9dad-41d1-80b4-00c04fd430c8";

type Peer = WebSocketStream<DuplexStream>;

struct OneConnection(Option<DuplexStream>);

impl Listener for OneConnection {
    type Io = DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.take() {
            Some(io) => (io, ()),
            None => future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(())
    }
}

async fn connect(state: Arc<WebSocketState>) -> (Peer, JoinHandle<()>) {
    let (server_io, peer_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        axum::serve(OneConnection(Some(server_io)), router(state))
            .await
            .expect("serve");
    });
    let (peer, response) = tokio_tungstenite::client_async("ws://localhost/ws", peer_io)
        .await
        .expect("websocket upgrade");
    assert_eq!(response.status(), 101);
    (peer, server)
}

async fn send(peer: &mut Peer, element: Element) {
    peer.send(Message::Text(String::from(&element).into()))
        .await
        .expect("send frame");
}

async fn receive(peer: &mut Peer) -> Element {
    match timeout(TIMEOUT, peer.next()).await.expect("frame timeout") {
        Some(Ok(Message::Text(text))) => text.parse().expect("XML frame"),
        other => panic!("expected text frame, got {other:?}"),
    }
}

/// Open the stream and return its pre-authentication features.
async fn start_stream(peer: &mut Peer) -> Element {
    send(
        peer,
        Element::builder("open", NS_FRAMING)
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), "example.com")
            .attr(minidom::rxml::xml_ncname!("version").to_owned(), "1.0")
            .build(),
    )
    .await;
    assert_eq!(receive(peer).await.name(), "open");
    let features = receive(peer).await;
    assert_eq!(
        (features.name(), features.ns()),
        ("features", NS_STREAM.to_owned())
    );
    features
}

fn bearer(token: &str) -> Vec<u8> {
    let mut payload = b"n,,".to_vec();
    payload.push(1);
    payload.extend_from_slice(b"auth=Bearer ");
    payload.extend_from_slice(token.as_bytes());
    payload.extend_from_slice(b"\x01\x01");
    payload
}

/// Authenticate with OAUTHBEARER, carrying `inline` and the `<user-agent/>`
/// of the client installation `user_agent`, and return the next frame.
async fn authenticate_as(
    peer: &mut Peer,
    token: &str,
    inline: &Sasl2InlineRequests,
    user_agent: Option<&str>,
) -> Element {
    let mut request = Sasl2Authenticate::new(
        Sasl2Mechanism::OAuthBearer,
        Some(Sasl2Data::new(bearer(token))),
    );
    if let Some(id) = user_agent {
        request
            .push_additional_child(
                Element::builder("user-agent", "urn:xmpp:sasl:2")
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
                    .build(),
            )
            .expect("one user agent");
    }
    inline
        .append_to(&mut request)
        .expect("inline requests fit the authenticate request");
    send(peer, request.to_element()).await;
    receive(peer).await
}

fn bind_request(bind: Bind2Request) -> Sasl2InlineRequests {
    Sasl2InlineRequests {
        resume: None,
        bind: Some(bind),
    }
}

fn carbons_enable() -> Element {
    Element::builder("enable", CARBONS_NS).build()
}

fn csi_inactive() -> Element {
    Element::builder("inactive", "urn:xmpp:csi:0").build()
}

fn bound_of(frame: &Element) -> Bind2Bound {
    let success = Sasl2Success::from_element(frame).expect("SASL2 success");
    Sasl2InlineResults::from_success(&success)
        .expect("typed inline results")
        .bound
        .expect("a <bound/> result")
}

fn metadata_of(bound: &Bind2Bound) -> &Element {
    bound
        .other_results
        .iter()
        .find(|result| result.is("metadata", NS_MAM))
        .expect("<bound/> carries the XEP-0313 <metadata/>")
}

async fn expect_post_auth_features(peer: &mut Peer) {
    let features = receive(peer).await;
    assert_eq!(
        (features.name(), features.ns()),
        ("features", NS_STREAM.to_owned())
    );
}

/// Bind with `bind`, expect `<bound/>`, and return the resource the server
/// registered for the connection.
async fn bind(
    state: &WebSocketState,
    peer: &mut Peer,
    token: &str,
    bind: Bind2Request,
    user_agent: Option<&str>,
) -> (FullJid, Bind2Bound) {
    let before = resources(state);
    let frame = authenticate_as(peer, token, &bind_request(bind), user_agent).await;
    let bound = bound_of(&frame);
    let identifier = Sasl2Success::from_element(&frame)
        .expect("SASL2 success")
        .authorization_identifier;
    expect_post_auth_features(peer).await;
    let mut after = resources(state);
    after.retain(|jid| !before.contains(jid));
    assert_eq!(after.len(), 1, "exactly one new resource: {after:?}");
    let jid = after.remove(0);
    // XEP-0386: the authorization identifier is the client's full JID.
    assert_eq!(identifier.to_string(), jid.to_string());
    (jid, bound)
}

fn resources(state: &WebSocketState) -> Vec<FullJid> {
    state.deps.protocol.connection_registry.list_connections()
}

fn alice() -> BareJid {
    "alice@example.com".parse().expect("bare jid")
}

fn offered(features: &Element) -> Option<Sasl2InlineOffer> {
    let authentication = features
        .children()
        .find(|child| child.is("authentication", "urn:xmpp:sasl:2"))
        .expect("SASL2 is offered");
    let feature = Sasl2Feature::from_element(authentication).expect("SASL2 feature");
    feature
        .inline
        .map(|inline| Sasl2InlineOffer::from_inline_features(&inline).expect("typed inline offer"))
}

#[tokio::test]
async fn bind2_is_advertised_inside_the_sasl2_inline_element_with_its_inline_features() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let (mut peer, server) = connect(state).await;
    let features = start_stream(&mut peer).await;

    // XEP-0386: a feature named 'bind' in 'urn:xmpp:bind:0' inside <inline/>,
    // listing the features that can be enabled with the bind. XEP-0198
    // resumption is offered beside it.
    let offer = offered(&features).expect("an <inline/> element");
    assert_eq!(
        offer,
        Sasl2InlineOffer {
            stream_resumption: true,
            bind2: Some(Bind2Feature {
                inline_features: vec![Feature::new(CARBONS_NS), Feature::new(NS_SM)],
            }),
        }
    );
    let authentication = features
        .children()
        .find(|child| child.is("authentication", "urn:xmpp:sasl:2"))
        .expect("SASL2");
    let inline = authentication
        .children()
        .find(|child| child.is("inline", "urn:xmpp:sasl:2"))
        .expect("inline");
    assert!(inline.children().any(|child| child.is("bind", NS_BIND2)));
    server.abort();
}

#[tokio::test]
async fn bind2_is_not_advertised_where_it_is_not_processed() {
    let state = websocket_state_with_sasl2_without_durable_sm().await;
    let (mut peer, server) = connect(state).await;
    let features = start_stream(&mut peer).await;

    // Without the durable authority an inline <bind/> is a malformed
    // request, so the server does not offer it.
    assert_eq!(offered(&features), None);
    server.abort();
}

#[tokio::test]
async fn bind2_enables_message_carbons_when_requested() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state.clone()).await;
    start_stream(&mut peer).await;

    let request = Bind2Request {
        other_features: vec![carbons_enable()],
        ..Bind2Request::default()
    };
    let (jid, _) = bind(&state, &mut peer, &session.id, request, None).await;
    let entry = state
        .deps
        .protocol
        .connection_registry
        .entry(&jid)
        .expect("registered binding");
    assert!(
        entry.carbons_enabled.load(Ordering::Relaxed),
        "XEP-0386: Message Carbons MUST be enabled at the bind"
    );
    server.abort();
}

#[tokio::test]
async fn bind2_without_a_carbons_request_leaves_carbons_off() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state.clone()).await;
    start_stream(&mut peer).await;

    let (jid, _) = bind(
        &state,
        &mut peer,
        &session.id,
        Bind2Request::default(),
        None,
    )
    .await;
    let entry = state
        .deps
        .protocol
        .connection_registry
        .entry(&jid)
        .expect("registered binding");
    assert!(!entry.carbons_enabled.load(Ordering::Relaxed));
    server.abort();
}

#[tokio::test]
async fn bind2_refuses_features_it_does_not_offer_inline() {
    for features in [
        // XEP-0352 is not offered inline.
        vec![csi_inactive()],
        // An unknown extension.
        vec![Element::builder("enable", "urn:example:unknown").build()],
        // XEP-0280 <enable/> is empty and appears once.
        vec![carbons_enable(), carbons_enable()],
        vec![Element::builder("enable", CARBONS_NS)
            .attr(minidom::rxml::xml_ncname!("x").to_owned(), "y")
            .build()],
    ] {
        let state = websocket_state_with_sasl2_and_durable_sm().await;
        let session = create_test_session(&state, "alice").await;
        let (mut peer, server) = connect(state.clone()).await;
        start_stream(&mut peer).await;

        let request = Bind2Request {
            other_features: features.clone(),
            ..Bind2Request::default()
        };
        let frame = authenticate_as(&mut peer, &session.id, &bind_request(request), None).await;
        let failure = Sasl2Failure::from_element(&frame).expect("SASL2 failure");
        assert_eq!(
            failure.condition,
            Sasl2FailureCondition::MalformedRequest,
            "{features:?}"
        );
        assert!(resources(&state).is_empty(), "nothing was bound");
        server.abort();
    }
}

#[tokio::test]
async fn bind2_includes_the_client_tag_in_the_resource_as_a_prefix() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state.clone()).await;
    start_stream(&mut peer).await;

    let request = Bind2Request {
        tag: Some(Bind2Tag::new("AwesomeXMPP")),
        ..Bind2Request::default()
    };
    let (jid, _) = bind(&state, &mut peer, &session.id, request, None).await;
    // XEP-0386: "[client tag]/[server generated identifier]".
    let (tag, server_id) = jid.resource().as_str().split_once('/').expect("tag/id");
    assert_eq!(tag, "AwesomeXMPP");
    assert!(!server_id.is_empty());
    server.abort();
}

#[tokio::test]
async fn bind2_generates_the_resource_alone_without_a_usable_tag() {
    let too_long = "t".repeat(2000);
    for tag in [None, Some(String::new()), Some(too_long)] {
        let state = websocket_state_with_sasl2_and_durable_sm().await;
        let session = create_test_session(&state, "alice").await;
        let (mut peer, server) = connect(state.clone()).await;
        start_stream(&mut peer).await;

        let request = Bind2Request {
            tag: tag.clone().map(Bind2Tag::new),
            ..Bind2Request::default()
        };
        let (jid, _) = bind(&state, &mut peer, &session.id, request, None).await;
        assert!(
            !jid.resource().as_str().contains('/'),
            "no tag prefix for {:?}: {jid}",
            tag.as_ref().map(String::len)
        );
        server.abort();
    }
}

fn archived_row(id: &str) -> PendingRow {
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: alice(),
        original_receipt_at: Utc::now(),
        payload: PendingPayload::Archived(StanzaId::new(id, alice().into())),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

fn whole_row() -> PendingRow {
    let mut message = XmppMessage::new(Some(alice().into()));
    message.type_ = MessageType::Chat;
    message.from = Some("bob@example.com".parse().expect("jid"));
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: alice(),
        original_receipt_at: Utc::now(),
        payload: PendingPayload::Transient(Box::new(message)),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

#[tokio::test]
async fn bind2_clears_offline_messages_the_archive_provides_without_sending_them() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let session = create_test_session(&state, "alice").await;
    let storage = state.deps.protocol.pending_delivery_storage.clone();
    let kept = whole_row();
    let kept_id = kept.id.clone();
    for row in [archived_row("a1"), archived_row("a2"), kept] {
        storage.insert(row).await.expect("store offline row");
    }
    let (mut peer, server) = connect(state.clone()).await;
    start_stream(&mut peer).await;

    bind(
        &state,
        &mut peer,
        &session.id,
        Bind2Request::default(),
        None,
    )
    .await;

    // XEP-0386: "Clear the offline messages for this user, if any, without
    // sending them (as they will be provided by MAM)." What the archive does
    // not hold is not provided by it, so that stays for the XEP-0160 flush.
    let rows = storage.list(&alice()).await.expect("list offline rows");
    assert_eq!(
        rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
        vec![kept_id],
        "only the message the archive does not hold remains"
    );
    // Nothing was sent to the client.
    assert!(
        timeout(Duration::from_millis(300), peer.next())
            .await
            .is_err(),
        "no stored message is sent"
    );
    server.abort();
}

#[tokio::test]
async fn bound_carries_the_archive_metadata_as_of_the_bind() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let session = create_test_session(&state, "alice").await;
    let mam = state.deps.protocol.mam_storage.clone();

    // An empty archive reports an empty <metadata/>.
    let (mut first, first_server) = connect(state.clone()).await;
    start_stream(&mut first).await;
    let (jid, bound) = bind(
        &state,
        &mut first,
        &session.id,
        Bind2Request::default(),
        None,
    )
    .await;
    assert!(metadata_of(&bound).children().next().is_none());
    let _ = jid;
    first_server.abort();

    for (index, id) in ["m1", "m2", "m3"].into_iter().enumerate() {
        let mut message =
            ArchivedMessage::for_test("bob@example.com".parse().expect("jid"), alice().into());
        message.id = id.to_owned();
        message.timestamp = Utc::now() + chrono::Duration::seconds(index as i64);
        message.body = Some(id.to_owned());
        message.stanza_id = Some(StanzaId::new(id, alice().into()));
        mam.store_message(&alice(), &message)
            .await
            .expect("archive a message");
    }
    let expected = mam
        .archive_bounds(&alice(), MamArchiveKind::Personal, None)
        .await
        .expect("archive bounds")
        .expect("a non-empty archive");
    assert_ne!(expected.start.id, expected.end.id);

    let (mut second, second_server) = connect(state.clone()).await;
    start_stream(&mut second).await;
    let (_, bound) = bind(
        &state,
        &mut second,
        &session.id,
        Bind2Request::default(),
        None,
    )
    .await;
    let metadata = metadata_of(&bound);
    let start = metadata.get_child("start", NS_MAM).expect("<start/>");
    let end = metadata.get_child("end", NS_MAM).expect("<end/>");
    assert_eq!(start.attr("id"), Some(expected.start.id.as_str()));
    assert_eq!(end.attr("id"), Some(expected.end.id.as_str()));
    assert!(start.attr("timestamp").is_some() && end.attr("timestamp").is_some());
    second_server.abort();
}

/// The frames a peer receives until its stream ends.
async fn frames_until_closed(peer: &mut Peer) -> Vec<Element> {
    let mut frames = Vec::new();
    loop {
        match timeout(TIMEOUT, peer.next()).await.expect("frame timeout") {
            Some(Ok(Message::Text(text))) => frames.push(text.parse().expect("XML frame")),
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return frames,
            Some(Ok(_)) => {}
        }
    }
}

fn is_conflict(frames: &[Element]) -> bool {
    frames.iter().any(|frame| {
        frame.is("error", NS_STREAM) && frame.children().any(|child| child.name() == "conflict")
    })
}

#[tokio::test]
async fn bind2_ends_the_earlier_sessions_of_the_same_client() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let session = create_test_session(&state, "alice").await;

    let (mut earlier, earlier_server) = connect(state.clone()).await;
    start_stream(&mut earlier).await;
    let (earlier_jid, _) = bind(
        &state,
        &mut earlier,
        &session.id,
        Bind2Request::default(),
        Some(CLIENT_A),
    )
    .await;

    // Another client of the same account is not the same client.
    let (mut other, other_server) = connect(state.clone()).await;
    start_stream(&mut other).await;
    let (other_jid, _) = bind(
        &state,
        &mut other,
        &session.id,
        Bind2Request::default(),
        Some(CLIENT_B),
    )
    .await;
    assert!(resources(&state).contains(&earlier_jid));

    // The same client binds again: its earlier session ends, and no
    // other does.
    let (mut later, later_server) = connect(state.clone()).await;
    start_stream(&mut later).await;
    let (later_jid, _) = bind(
        &state,
        &mut later,
        &session.id,
        Bind2Request::default(),
        Some(CLIENT_A),
    )
    .await;

    let frames = frames_until_closed(&mut earlier).await;
    assert!(is_conflict(&frames), "the earlier session got {frames:?}");
    let live = resources(&state);
    assert!(!live.contains(&earlier_jid), "{live:?}");
    assert!(live.contains(&other_jid), "another client is left alone");
    assert!(live.contains(&later_jid));
    earlier_server.abort();
    other_server.abort();
    later_server.abort();
}

#[tokio::test]
async fn bind2_without_a_user_agent_id_ends_no_session() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let session = create_test_session(&state, "alice").await;

    let (mut first, first_server) = connect(state.clone()).await;
    start_stream(&mut first).await;
    let (first_jid, _) = bind(
        &state,
        &mut first,
        &session.id,
        Bind2Request::default(),
        None,
    )
    .await;
    let (mut second, second_server) = connect(state.clone()).await;
    start_stream(&mut second).await;
    let (second_jid, _) = bind(
        &state,
        &mut second,
        &session.id,
        Bind2Request::default(),
        None,
    )
    .await;

    let live = resources(&state);
    assert!(live.contains(&first_jid) && live.contains(&second_jid));
    first_server.abort();
    second_server.abort();
}
