//! RFC 6121 presence behavior over the active WebSocket C2S transport.

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";

async fn assert_no_frame_matching<F>(
    client: &mut WsXmppClient,
    duration: Duration,
    predicate: F,
    description: &str,
) where
    F: Fn(&str) -> bool,
{
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        let Ok(frame) = client.recv_timeout(remaining).await else {
            return;
        };
        assert!(!predicate(&frame), "{description}: {frame}");
    }
}

async fn connect_alice_bob() -> (TestServer, WsXmppClient, WsXmppClient) {
    let _start_default_server: fn() -> TestServer = TestServer::start;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let _admin_password = server.fixed_account_password();

    let alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice connection");
    let bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    (server, alice, bob)
}

async fn send_roster_get(client: &mut WsXmppClient, id: &str) {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><query xmlns="jabber:iq:roster"/></iq>"#
        ))
        .await
        .expect("send roster get");
    let _ = client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("roster get result");
}

async fn establish_subscription_to_alice(alice: &mut WsXmppClient, bob: &mut WsXmppClient) {
    send_roster_get(alice, "alice-subscription-roster").await;
    send_roster_get(bob, "bob-subscription-roster").await;
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice is available for subscription request delivery");
    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("bob subscribes to alice");
    let subscribe = alice
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("alice receives subscribe");
    assert!(
        subscribe.contains("from='bob@localhost'"),
        "expected bob subscribe: {subscribe}"
    );
    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("alice approves bob");
    let subscribed = bob
        .recv_matching(|frame| frame.contains("type='subscribed'"))
        .await
        .expect("bob receives approval");
    assert!(
        subscribed.contains("from='alice@localhost'"),
        "expected alice approval: {subscribed}"
    );
    bob.send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("bob is available for presence broadcasts");
}

#[tokio::test]
async fn websocket_directed_presence_routes_to_target_resource() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;
    let alice_jid = alice.full_jid.clone().expect("alice full jid");

    bob.send(&format!(
        r#"<presence xmlns="jabber:client" to="{alice_jid}"><show>away</show><status>directed hello</status><priority>5</priority></presence>"#
    ))
    .await
    .expect("send directed presence");

    let delivered = alice
        .recv_matching(|frame| frame.contains("directed hello"))
        .await
        .expect("directed presence delivery");
    let has_from =
        delivered.contains("from='bob@localhost/") || delivered.contains("from='bob@localhost/");
    let has_to =
        delivered.contains("to='alice@localhost/") || delivered.contains("to='alice@localhost/");
    assert!(
        has_from && has_to,
        "expected directed presence routed with full JIDs, got: {delivered}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_full_jid_probe_returns_rich_resource_presence() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;
    let alice_jid = alice.full_jid.clone().expect("alice full jid");

    establish_subscription_to_alice(&mut alice, &mut bob).await;
    alice
        .send(
            r#"<presence xmlns="jabber:client"><show>away</show><status>debugging</status><priority>7</priority></presence>"#,
        )
        .await
        .expect("alice sends rich presence");
    bob.recv_matching(|frame| frame.contains("debugging"))
        .await
        .expect("bob receives broadcast rich presence");

    bob.send(&probe_presence_xml(&alice_jid))
        .await
        .expect("bob probes alice full jid");
    let probe = bob
        .recv_matching(|frame| {
            frame.contains("from='alice@localhost/") || frame.contains("from='alice@localhost/")
        })
        .await
        .expect("probe response");
    assert!(
        probe.contains("<show>away</show>")
            && probe.contains("<status>debugging</status>")
            && probe.contains("<priority>7</priority>"),
        "full-JID probe must preserve rich resource presence: {probe}"
    );
    // Alice sent no <c/>: the probe response must not add one either
    // (issue #1101 — server caps were previously injected here).
    assert!(
        !probe.contains("http://jabber.org/protocol/caps"),
        "server must not attach caps to a caps-less probe response: {probe}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

/// Count the `<c` XEP-0115 caps elements in a raw frame. Relayed user
/// presence must carry exactly the caps the client sent — never an extra
/// server-injected one (caps belong to the generating entity, XEP-0115 §1).
fn caps_element_count(frame: &str) -> usize {
    frame
        .matches("<c xmlns='http://jabber.org/protocol/caps'")
        .count()
}

/// Serialize a `<presence type="probe" to=…/>` via minidom — stanzas with
/// interpolated values (JIDs) are built with the XML builder, never `format!`.
fn probe_presence_xml(to: &str) -> String {
    let attr = |name: &str| {
        <minidom::rxml::NcName as std::convert::TryFrom<&str>>::try_from(name)
            .expect("static ncname is valid")
    };
    let element = minidom::Element::builder("presence", "jabber:client")
        .attr(attr("type"), "probe")
        .attr(attr("to"), to)
        .build();
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize element");
    String::from_utf8(bytes).expect("serializer emits utf-8")
}

#[tokio::test]
async fn websocket_presence_broadcast_relays_contacts_own_caps_not_servers() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    establish_subscription_to_alice(&mut alice, &mut bob).await;
    alice
        .send(
            r#"<presence xmlns="jabber:client"><c xmlns="http://jabber.org/protocol/caps" hash="sha-1" node="https://example.com/client" ver="zHyEOgxTrkpSdGcQKH8EFPLsriY="/></presence>"#,
        )
        .await
        .expect("alice sends presence with her own caps");
    let broadcast = bob
        .recv_matching(|frame| {
            frame.contains("from='alice@localhost/")
                && frame.contains("http://jabber.org/protocol/caps")
        })
        .await
        .expect("bob receives alice's broadcast presence with a caps element");
    assert!(
        broadcast.contains("zHyEOgxTrkpSdGcQKH8EFPLsriY=")
            && broadcast.contains("https://example.com/client"),
        "broadcast must carry the contact's own caps hash: {broadcast}"
    );
    assert_eq!(
        caps_element_count(&broadcast),
        1,
        "broadcast must carry exactly the client's caps element, nothing extra: {broadcast}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_presence_broadcast_relays_arbitrary_extensions_verbatim() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    establish_subscription_to_alice(&mut alice, &mut bob).await;
    alice
        .send(
            r#"<presence xmlns="jabber:client"><show>away</show><idle xmlns="urn:xmpp:idle:1" since="2026-07-06T10:00:00Z"/><x xmlns="urn:example:future-extension:0"><detail>opaque</detail></x></presence>"#,
        )
        .await
        .expect("alice sends presence with idle and an unknown extension");
    let broadcast = bob
        .recv_matching(|frame| {
            frame.contains("from='alice@localhost/")
                && frame.contains("urn:example:future-extension:0")
        })
        .await
        .expect("bob receives the relayed presence with the unknown extension");
    assert!(
        broadcast.contains("<detail>opaque</detail>"),
        "extension content must survive relay verbatim: {broadcast}"
    );
    assert!(
        broadcast.contains("urn:xmpp:idle:1") && broadcast.contains("2026-07-06T10:00:00"),
        "XEP-0319 idle must survive relay without hand re-attachment: {broadcast}"
    );
    // Alice sent no <c/>: the relay must not add one either — the historical
    // bug (ensure_caps_payload) only decorated caps-less presence, so this
    // negative assertion is what actually pins it down.
    assert!(
        !broadcast.contains("http://jabber.org/protocol/caps"),
        "server must not attach any caps to a caps-less relayed presence: {broadcast}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_error_typed_presence_is_not_broadcast_to_subscribers() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    establish_subscription_to_alice(&mut alice, &mut bob).await;
    alice
        .send(
            r#"<presence xmlns="jabber:client" type="error"><status>bogus error relay</status></presence>"#,
        )
        .await
        .expect("alice sends an error-typed presence");
    assert_no_frame_matching(
        &mut bob,
        Duration::from_millis(250),
        |frame| frame.contains("bogus error relay"),
        "an error-typed presence must be dropped, not relayed to subscribers",
    )
    .await;

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_presence_probe_returns_contacts_own_caps_and_extensions() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;
    let alice_jid = alice.full_jid.clone().expect("alice full jid");

    establish_subscription_to_alice(&mut alice, &mut bob).await;
    alice
        .send(
            r#"<presence xmlns="jabber:client"><show>away</show><c xmlns="http://jabber.org/protocol/caps" hash="sha-1" node="https://example.com/client" ver="zHyEOgxTrkpSdGcQKH8EFPLsriY="/><x xmlns="urn:example:future-extension:0"/></presence>"#,
        )
        .await
        .expect("alice sends presence with her own caps and an extension");
    bob.recv_matching(|frame| frame.contains("urn:example:future-extension:0"))
        .await
        .expect("bob receives alice's broadcast");

    bob.send(&probe_presence_xml(&alice_jid))
        .await
        .expect("bob probes alice");
    let probe = bob
        .recv_matching(|frame| {
            frame.contains("from='alice@localhost/") && frame.contains("<show>away</show>")
        })
        .await
        .expect("probe response");
    assert!(
        probe.contains("zHyEOgxTrkpSdGcQKH8EFPLsriY=")
            && probe.contains("https://example.com/client")
            && probe.contains("urn:example:future-extension:0"),
        "probe response must carry the contact's stored caps and extensions: {probe}"
    );
    assert_eq!(
        caps_element_count(&probe),
        1,
        "probe response must carry exactly the client's caps element, nothing extra: {probe}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_subscription_approval_push_carries_contacts_own_payloads() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    send_roster_get(&mut alice, "alice-approval-roster").await;
    send_roster_get(&mut bob, "bob-approval-roster").await;
    alice
        .send(
            r#"<presence xmlns="jabber:client"><c xmlns="http://jabber.org/protocol/caps" hash="sha-1" node="https://example.com/client" ver="zHyEOgxTrkpSdGcQKH8EFPLsriY="/><x xmlns="urn:example:future-extension:0"/></presence>"#,
        )
        .await
        .expect("alice is available with her own caps and an extension");
    bob.send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("bob is available");
    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("bob subscribes to alice");
    alice
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("alice receives subscribe");
    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("alice approves bob");

    let pushed = bob
        .recv_matching(|frame| {
            frame.contains("from='alice@localhost/")
                && frame.contains("http://jabber.org/protocol/caps")
        })
        .await
        .expect("bob receives alice's current presence after approval");
    assert!(
        pushed.contains("zHyEOgxTrkpSdGcQKH8EFPLsriY=")
            && pushed.contains("https://example.com/client")
            && pushed.contains("urn:example:future-extension:0"),
        "approval push must carry the contact's stored caps and extensions: {pushed}"
    );
    assert_eq!(
        caps_element_count(&pushed),
        1,
        "approval push must carry exactly the client's caps element, nothing extra: {pushed}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_blocking_prevents_presence_probe_response() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    alice
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="presence-block-bob"><block xmlns="urn:xmpp:blocking"><item jid="bob@localhost"/></block></iq>"#,
        )
        .await
        .expect("send block set");
    let block_response = alice
        .recv_matching(|frame| frame.contains("presence-block-bob"))
        .await
        .expect("blocking set response");
    assert!(
        block_response.contains("type='result'") || block_response.contains("type='result'"),
        "expected blocking result, got: {block_response}"
    );

    bob.send(r#"<presence xmlns="jabber:client" type="probe" to="alice@localhost"/>"#)
        .await
        .expect("send blocked probe");
    assert_no_frame_matching(
        &mut bob,
        Duration::from_millis(250),
        |frame| frame.contains("from='alice@localhost") || frame.contains("from='alice@localhost"),
        "bob should not receive alice presence after being blocked",
    )
    .await;

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_blocking_filters_presence_broadcast_to_subscriber() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    send_roster_get(&mut bob, "bob-presence-roster").await;
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available");

    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("send subscribe");
    let subscribe = alice
        .recv_matching(|frame| {
            frame.contains("type='subscribe'") || frame.contains("type='subscribe'")
        })
        .await
        .expect("alice subscribe request");
    assert!(
        subscribe.contains("from='bob@localhost") || subscribe.contains("from='bob@localhost"),
        "expected bob subscribe request, got: {subscribe}"
    );

    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("send subscribed");
    let subscribed = bob
        .recv_matching(|frame| {
            frame.contains("type='subscribed'") || frame.contains("type='subscribed'")
        })
        .await
        .expect("bob subscribed response");
    assert!(
        subscribed.contains("from='alice@localhost")
            || subscribed.contains("from='alice@localhost"),
        "expected alice subscribed response, got: {subscribed}"
    );

    alice
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="presence-broadcast-block-bob"><block xmlns="urn:xmpp:blocking"><item jid="bob@localhost"/></block></iq>"#,
        )
        .await
        .expect("send block set");
    let block_response = alice
        .recv_matching(|frame| frame.contains("presence-broadcast-block-bob"))
        .await
        .expect("blocking set response");
    assert!(
        block_response.contains("type='result'") || block_response.contains("type='result'"),
        "expected blocking result, got: {block_response}"
    );

    alice
        .send(
            r#"<presence xmlns="jabber:client"><show>chat</show><status>secret status</status><priority>7</priority></presence>"#,
        )
        .await
        .expect("send available presence");
    assert_no_frame_matching(
        &mut bob,
        Duration::from_millis(250),
        |frame| frame.contains("secret status"),
        "bob should not receive blocked alice broadcast presence",
    )
    .await;

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_blocking_filters_presence_broadcast_when_subscriber_blocks_sender() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    send_roster_get(&mut bob, "bob-presence-roster").await;
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available");

    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("send subscribe");
    let subscribe = alice
        .recv_matching(|frame| {
            frame.contains("type='subscribe'") || frame.contains("type='subscribe'")
        })
        .await
        .expect("alice subscribe request");
    assert!(
        subscribe.contains("from='bob@localhost") || subscribe.contains("from='bob@localhost"),
        "expected bob subscribe request, got: {subscribe}"
    );

    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("send subscribed");
    let subscribed = bob
        .recv_matching(|frame| {
            frame.contains("type='subscribed'") || frame.contains("type='subscribed'")
        })
        .await
        .expect("bob subscribed response");
    assert!(
        subscribed.contains("from='alice@localhost")
            || subscribed.contains("from='alice@localhost"),
        "expected alice subscribed response, got: {subscribed}"
    );

    bob.send(
        r#"<iq xmlns="jabber:client" type="set" id="presence-broadcast-block-alice"><block xmlns="urn:xmpp:blocking"><item jid="alice@localhost"/></block></iq>"#,
    )
    .await
    .expect("send block set");
    let block_response = bob
        .recv_matching(|frame| frame.contains("presence-broadcast-block-alice"))
        .await
        .expect("blocking set response");
    assert!(
        block_response.contains("type='result'") || block_response.contains("type='result'"),
        "expected blocking result, got: {block_response}"
    );

    alice
        .send(
            r#"<presence xmlns="jabber:client"><show>chat</show><status>blocked by subscriber</status><priority>7</priority></presence>"#,
        )
        .await
        .expect("send available presence");
    assert_no_frame_matching(
        &mut bob,
        Duration::from_millis(250),
        |frame| frame.contains("blocked by subscriber"),
        "bob should not receive alice broadcast presence after blocking alice",
    )
    .await;

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_unclean_disconnect_broadcasts_unavailable_to_subscribers() {
    // RFC 6121 §4.5.2: when a session ends ungracefully (connection drop,
    // no </stream> close, no self-sent unavailable), the SERVER must
    // generate <presence type='unavailable'/> from that full JID to the
    // user's presence subscribers. A resource that never sent initial
    // available presence must NOT trigger such a broadcast.
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice connection");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");
    establish_subscription_to_alice(&mut alice, &mut bob).await;
    let alice_full = alice.full_jid.clone().expect("alice full jid");

    // A second alice resource that binds but NEVER sends available presence.
    let silent_resource = format!("alice-silent-{}", uuid::Uuid::new_v4());
    let alice_silent = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &silent_resource,
    )
    .await
    .expect("silent alice connection");
    let alice_silent_full = alice_silent
        .full_jid
        .clone()
        .expect("silent alice full jid");

    // Abrupt drop: no <close/>, no </stream>, no unavailable presence —
    // the TCP/WebSocket stream is simply torn down.
    drop(alice_silent);
    assert_no_frame_matching(
        &mut bob,
        Duration::from_millis(700),
        |frame| {
            frame.contains("type='unavailable'")
                && frame.contains(&format!("from='{alice_silent_full}'"))
        },
        "a session that never sent available presence must not produce a server-generated unavailable broadcast",
    )
    .await;

    // Abrupt drop of the presence-available resource: subscribers MUST
    // receive a server-generated unavailable from that full JID.
    drop(alice);
    let unavailable = bob
        .recv_matching(|frame| {
            frame.contains("type='unavailable'") && frame.contains(&format!("from='{alice_full}'"))
        })
        .await
        .expect("bob receives server-generated unavailable after alice's unclean disconnect");
    assert!(
        unavailable.contains("to='bob@localhost"),
        "server-generated unavailable must be addressed to the subscriber: {unavailable}"
    );

    let _ = bob.close().await;
}

#[tokio::test]
async fn websocket_probe_returns_xep0319_idle_for_live_and_detached_resource() {
    // XEP-0319 §2.2 + RFC 6121 §4.3: a probe response reports the contact's
    // last broadcast presence, including the <idle xmlns='urn:xmpp:idle:1'
    // since='…'/> stamp the client advertised. Issue #1103 acceptance covers
    // "live AND detached": the stored payloads must answer a probe while the
    // resource is still LIVE, and must survive into the detached snapshot
    // once the XEP-0198 session is awaiting resume.
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice connection");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");
    establish_subscription_to_alice(&mut alice, &mut bob).await;
    let alice_full = alice.full_jid.clone().expect("alice full jid");

    // Make alice's session resumable so the abrupt drop below detaches the
    // session instead of tearing it down.
    alice
        .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
        .await
        .expect("alice enables stream management");
    let _enabled = alice
        .recv_matching(|frame| frame.contains("<enabled"))
        .await
        .expect("alice receives <enabled/>");

    const IDLE_SINCE: &str = "2024-06-01T12:00:00Z";
    alice
        .send(&format!(
            r#"<presence xmlns="jabber:client"><show>away</show><idle xmlns="urn:xmpp:idle:1" since="{IDLE_SINCE}"/></presence>"#
        ))
        .await
        .expect("alice sends away presence with XEP-0319 idle");
    let live_broadcast = bob
        .recv_matching(|frame| {
            frame.contains(&format!("from='{alice_full}'")) && frame.contains("<show>away</show>")
        })
        .await
        .expect("bob receives alice's live away broadcast");
    assert!(
        live_broadcast.contains("urn:xmpp:idle:1"),
        "live broadcast relays the idle stamp verbatim (issue #1101): {live_broadcast}"
    );

    // Probe the LIVE resource's full JID before any disconnect: the
    // stored last-broadcast presence must already carry the idle stamp
    // (RFC 6121 §4.3.2, XEP-0319 §2.2 — issue #1103 "live" half).
    bob.send(&probe_presence_xml(&alice_full))
        .await
        .expect("bob probes alice's live resource");
    let live_probe = bob
        .recv_matching(|frame| {
            frame.contains(&format!("from='{alice_full}'")) && frame.contains("<show>away</show>")
        })
        .await
        .expect("probe response for the live resource");
    assert!(
        live_probe.contains("urn:xmpp:idle:1") && live_probe.contains(IDLE_SINCE),
        "probe response for a live resource must carry the stored XEP-0319 idle stamp: {live_probe}"
    );

    // Abrupt drop: the resumable session detaches, awaiting resume.
    drop(alice);
    tokio::time::sleep(Duration::from_millis(800)).await;

    bob.send(&probe_presence_xml("alice@localhost"))
        .await
        .expect("bob probes alice");
    let probe = bob
        .recv_matching(|frame| {
            frame.contains(&format!("from='{alice_full}'")) && frame.contains("<show>away</show>")
        })
        .await
        .expect("probe response for the detached resource");
    assert!(
        probe.contains("urn:xmpp:idle:1") && probe.contains(IDLE_SINCE),
        "probe response for a detached resource must carry the stored XEP-0319 idle stamp: {probe}"
    );

    let _ = bob.close().await;
}
