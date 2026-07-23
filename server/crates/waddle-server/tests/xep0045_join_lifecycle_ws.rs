//! XEP-0045 join-lifecycle integration tests over WebSocket (PR #1207).
//!
//! Covers the wire-observable MUC occupancy-lifecycle behaviors added by
//! the presence/SM/MUC reliability bundle:
//!
//! 1. Second-nick refusal (XEP-0045 §7.6, issue #1107): a full JID that
//!    already occupies the room under one nick and asks to join under a
//!    second nick is refused with a presence error `type='cancel'`
//!    carrying `<not-acceptable
//!    xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>`, and the original
//!    occupancy stays intact (no ghost second occupancy, so broadcast
//!    fan-out reaches the session exactly once).
//!
//! 2. Ghost-occupancy cleanup on unclean disconnect (XEP-0045 §7.14):
//!    when a non-resumable session's socket drops with no `</close>`
//!    and no unavailable presence, the room broadcasts
//!    `<presence type='unavailable'>` from the occupant's room-nick
//!    (with `<x xmlns='http://jabber.org/protocol/muc#user'>` and
//!    `<item role='none'/>`) to the remaining occupants.
//!
//! 3. Fresh-bind occupancy cleanup (codex P1 on PR #1207): an
//!    XEP-0198-resumable session that drops uncleanly DETACHES — the
//!    occupancy is preserved awaiting resume and no leave is broadcast.
//!    When the SAME full JID then reconnects and fresh-binds (no
//!    resume), the invalidation of the dead detached session must clean
//!    its MUC occupancy: remaining occupants receive the room-nick
//!    unavailable, and the fresh (never-joined) stream must not inherit
//!    room fan-out.
//!
//! 4. Portable startup in a cluster-capable binary: when the
//!    `clustering` feature is compiled but clustering is disabled at
//!    runtime, a first join stays on the local XEP-0045 path. It must
//!    not be rejected with a retryable `<resource-constraint/>`, and
//!    readiness is confirmed by self-presence status code 110.
//!
//! Wire shapes are parsed via `minidom::Element` and asserted
//! structurally so child ordering and attribute quoting cannot flake
//! the tests.

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const ALICE: &str = "alice";
const BOB: &str = "bob";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_XMPP_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

// Each test spawns a fresh waddle-server binary; serialize so the harness
// temp-port slot does not race when several tests run in parallel.
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect(server: &TestServer, user: &str, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .expect("connect and auth")
}

/// Send a XEP-0045 join presence to `room/nick` and drain frames until
/// the historical-subject ack arrives (last frame in the join sequence).
async fn join_room(client: &mut WsXmppClient, room: &str, nick: &str) {
    client
        .send(&format!(
            r#"<presence to="{room}/{nick}"><x xmlns="{NS_MUC}"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

/// Find a descendant element with the given local-name and namespace.
fn find_descendant<'a>(root: &'a Element, name: &str, ns: &str) -> Option<&'a Element> {
    for child in root.children() {
        if child.name() == name && child.ns() == ns {
            return Some(child);
        }
        if let Some(found) = find_descendant(child, name, ns) {
            return Some(found);
        }
    }
    None
}

/// Assert that NO frame matching `predicate` arrives within `window`.
/// Non-matching frames are drained; a matching frame fails the test.
async fn assert_no_frame_matching<F: Fn(&str) -> bool>(
    client: &mut WsXmppClient,
    window: Duration,
    predicate: F,
    why: &str,
) {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return;
        }
        match client.recv_timeout(deadline - now).await {
            Ok(frame) => {
                assert!(!predicate(&frame), "{why}: unexpected frame {frame}");
            }
            Err(err) if err.contains("Timeout") => return,
            Err(err) => panic!("connection failed while asserting frame absence ({why}): {err}"),
        }
    }
}

/// Assert the XEP-0045 §7.14 room-broadcast leave shape: `<presence
/// type='unavailable' from='room@service/nick'>` with `<x
/// xmlns='http://jabber.org/protocol/muc#user'>` carrying `<item
/// role='none'/>`.
fn assert_leave_presence(frame: &str, expected_from: &str) {
    let element = frame
        .parse::<Element>()
        .unwrap_or_else(|err| panic!("frame must parse as XML: {err}; frame={frame}"));
    assert_eq!(element.name(), "presence", "expected <presence>: {frame}");
    assert_eq!(
        element.attr("type"),
        Some("unavailable"),
        "leave presence must have type='unavailable': {frame}"
    );
    assert_eq!(
        element.attr("from"),
        Some(expected_from),
        "leave presence must come from the departed occupant's room-nick: {frame}"
    );
    let muc_user = find_descendant(&element, "x", NS_MUC_USER).unwrap_or_else(|| {
        panic!("leave presence missing <x xmlns='{NS_MUC_USER}'>: {frame}");
    });
    let item = muc_user
        .children()
        .find(|child| child.name() == "item" && child.ns() == NS_MUC_USER)
        .unwrap_or_else(|| panic!("leave presence missing <item> in muc#user: {frame}"));
    assert_eq!(
        item.attr("role"),
        Some("none"),
        "XEP-0045 §7.14: a departed occupant's leave presence carries role='none': {frame}"
    );
}

/// Assert the XEP-0045 §7.6 second-nick refusal shape: `<presence
/// type='error' from='room@service/requested-nick'>` with `<error
/// type='cancel'><not-acceptable
/// xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error>`.
fn assert_not_acceptable_presence_error(frame: &str, expected_from: &str, expected_to: &str) {
    let element = frame
        .parse::<Element>()
        .unwrap_or_else(|err| panic!("frame must parse as XML: {err}; frame={frame}"));
    assert_eq!(element.name(), "presence", "expected <presence>: {frame}");
    assert_eq!(
        element.attr("type"),
        Some("error"),
        "refusal must be a presence error: {frame}"
    );
    assert_eq!(
        element.attr("from"),
        Some(expected_from),
        "XEP-0045 §7.6: the error is returned from the REQUESTED room-nick: {frame}"
    );
    assert_eq!(
        element.attr("to"),
        Some(expected_to),
        "the error must be addressed to the requesting full JID: {frame}"
    );
    let error = element
        .children()
        .find(|child| child.name() == "error")
        .unwrap_or_else(|| panic!("presence error missing <error> child: {frame}"));
    assert_eq!(
        error.attr("type"),
        Some("cancel"),
        "XEP-0045 §7.6 refusal uses error type='cancel': {frame}"
    );
    assert!(
        find_descendant(error, "not-acceptable", NS_XMPP_STANZAS).is_some(),
        "XEP-0045 §7.6 refusal carries <not-acceptable xmlns='{NS_XMPP_STANZAS}'/>: {frame}"
    );
}

/// XEP-0045 §7.6 (issue #1107): a full JID already in the room under one
/// nick must be refused when it asks to join the SAME room under a
/// second nick — nicknames are locked to identity, so the service
/// answers with `<not-acceptable/>` (type='cancel') from the requested
/// room-nick. The existing occupancy must survive untouched: no ghost
/// second occupancy is admitted, so room fan-out still reaches the
/// session exactly once and messages it sends are still reflected from
/// the ORIGINAL nick.
#[tokio::test]
async fn xep_0045_second_nick_join_is_refused_with_not_acceptable() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "lifecycle-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "lifecycle-alice").await;
    let mut bob = connect(&server, BOB, &bob_pass, "lifecycle-bob").await;
    let alice_full = alice.full_jid.clone().expect("alice full jid");

    let room = format!("join-lc-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    // Admin joins first — opening the room makes admin the owner per
    // XEP-0045 §10.1.3 (instant room). Then alice and bob join as
    // participants.
    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;
    join_room(&mut bob, &room, BOB).await;

    // Drain bob's join broadcast from alice's queue so the next frame
    // alice sees is the refusal.
    alice
        .recv_matching(|frame| frame.contains(&format!("{room}/{BOB}")))
        .await
        .expect("alice sees bob join");

    // Alice, already joined as "alice", asks to join the same room as
    // "impostor" — same account, same resource, second nick.
    let second_nick = "impostor";
    alice
        .send(&format!(
            r#"<presence to="{room}/{second_nick}"><x xmlns="{NS_MUC}"/></presence>"#
        ))
        .await
        .expect("send second-nick join");
    let refusal = alice
        .recv_matching(|frame| frame.contains("<presence") && frame.contains("type='error'"))
        .await
        .expect("alice receives second-nick refusal");
    assert_not_acceptable_presence_error(&refusal, &format!("{room}/{second_nick}"), &alice_full);

    // No occupant may observe a join under the second nick.
    assert_no_frame_matching(
        &mut bob,
        Duration::from_millis(700),
        |frame| frame.contains(&format!("{room}/{second_nick}")),
        "a refused second-nick join must not be broadcast to other occupants",
    )
    .await;

    // Alice is still joined under her ORIGINAL nick: a message she sends
    // is reflected to the room from room/alice.
    let alice_body = format!("still-alice-{}", uuid::Uuid::new_v4());
    alice
        .send(&format!(
            r#"<message to="{room}" type="groupchat" id="msg-{}"><body>{alice_body}</body></message>"#,
            uuid::Uuid::new_v4()
        ))
        .await
        .expect("alice sends groupchat message");
    let bob_copy = bob
        .recv_matching(|frame| frame.contains(&alice_body))
        .await
        .expect("bob receives alice's message");
    assert!(
        bob_copy.contains(&format!("from='{room}/{ALICE}'")),
        "alice's message must be reflected from her original nick: {bob_copy}"
    );

    // Exactly ONE occupancy for alice's full JID: a room broadcast
    // reaches her session once, not once per ghost occupancy.
    let bob_body = format!("ping-from-bob-{}", uuid::Uuid::new_v4());
    bob.send(&format!(
        r#"<message to="{room}" type="groupchat" id="msg-{}"><body>{bob_body}</body></message>"#,
        uuid::Uuid::new_v4()
    ))
    .await
    .expect("bob sends groupchat message");
    let first_copy = alice
        .recv_matching(|frame| frame.contains(&bob_body))
        .await
        .expect("alice receives bob's message");
    assert!(
        first_copy.contains(&format!("from='{room}/{BOB}'")),
        "bob's message must be reflected from his room-nick: {first_copy}"
    );
    assert_no_frame_matching(
        &mut alice,
        Duration::from_millis(700),
        |frame| frame.contains(&bob_body),
        "a single occupancy must receive room fan-out exactly once (duplicate = ghost occupancy)",
    )
    .await;

    let _ = admin.close().await;
    let _ = alice.close().await;
    let _ = bob.close().await;
}

/// XEP-0045 §7.14 on the unclean-disconnect path: when an occupant's
/// socket drops abruptly (no `</close>`, no unavailable presence, no
/// XEP-0198 resumption to wait for), the service must inform the
/// remaining occupants by broadcasting `<presence type='unavailable'>`
/// from the departed occupant's room-nick — otherwise the occupant
/// ghosts in every remaining client's roster forever.
#[tokio::test]
async fn xep_0045_unclean_disconnect_broadcasts_leave_to_remaining_occupants() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "drop-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "drop-alice").await;
    let mut bob = connect(&server, BOB, &bob_pass, "drop-bob").await;

    let room = format!("drop-lc-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;
    join_room(&mut bob, &room, BOB).await;

    // Abrupt drop: no `</close>`, no unavailable — the TCP/WebSocket
    // stream is simply torn down. Alice never enabled XEP-0198, so the
    // session is not resumable and must be cleaned up immediately.
    drop(alice);

    let leave = bob
        .recv_matching(|frame| {
            frame.contains("type='unavailable'") && frame.contains(&format!("{room}/{ALICE}"))
        })
        .await
        .expect("bob receives the room's leave broadcast for alice");
    assert_leave_presence(&leave, &format!("{room}/{ALICE}"));

    let _ = admin.close().await;
    let _ = bob.close().await;
}

/// Codex P1 on PR #1207: an XEP-0198-resumable session that drops
/// uncleanly DETACHES — its MUC occupancy is preserved awaiting resume
/// and no leave is broadcast. When the SAME full JID (same account AND
/// same resource) then reconnects and fresh-binds instead of resuming,
/// the fresh bind invalidates the dead detached session, and that
/// invalidation MUST clean the dead session's MUC occupancy: the new
/// stream has not joined anything yet, so the occupancy is certainly
/// stale. Remaining occupants receive the room-nick unavailable at
/// rebind time, and the fresh stream must not inherit room fan-out it
/// never subscribed to.
#[tokio::test]
async fn xep_0045_fresh_bind_after_unclean_drop_cleans_detached_occupancy() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "rebind-admin").await;
    // Fixed resource: the fresh bind below must reuse the EXACT full JID.
    let alice_resource = "rebind-phone";
    let mut alice = connect(&server, ALICE, &alice_pass, alice_resource).await;
    let mut bob = connect(&server, BOB, &bob_pass, "rebind-bob").await;

    // Make alice's session resumable so the abrupt drop below detaches
    // the session (occupancy preserved) instead of tearing it down.
    alice
        .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
        .await
        .expect("alice enables stream management");
    alice
        .recv_matching(|frame| frame.contains("<enabled"))
        .await
        .expect("alice receives <enabled/>");

    let room = format!("rebind-lc-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut bob, &room, BOB).await;
    join_room(&mut alice, &room, ALICE).await;

    // Drain alice's join broadcast from bob's queue so every later
    // frame bob sees about room/alice is leave-related.
    bob.recv_matching(|frame| frame.contains(&format!("{room}/{ALICE}")))
        .await
        .expect("bob sees alice join");

    // Abrupt drop: the resumable session detaches awaiting resume —
    // the occupancy is intentionally preserved, so NO leave broadcast
    // may reach bob yet.
    drop(alice);
    assert_no_frame_matching(
        &mut bob,
        Duration::from_millis(1000),
        |frame| frame.contains("type='unavailable'") && frame.contains(&format!("{room}/{ALICE}")),
        "a detached resumable session must keep its occupancy until resumed or invalidated",
    )
    .await;

    // The same full JID reconnects and FRESH-BINDS (no <resume/>). The
    // bind invalidates the dead detached session, whose MUC occupancy
    // must now be cleaned even though a live registry entry (the fresh
    // bind itself) exists for the JID.
    let mut alice_fresh = connect(&server, ALICE, &alice_pass, alice_resource).await;

    let leave = bob
        .recv_matching(|frame| {
            frame.contains("type='unavailable'") && frame.contains(&format!("{room}/{ALICE}"))
        })
        .await
        .expect("bob receives alice's leave when the fresh bind invalidates the detached session");
    assert_leave_presence(&leave, &format!("{room}/{ALICE}"));

    // The fresh stream never joined the room, so it must NOT inherit
    // fan-out addressed via the dead session's occupancy.
    let bob_body = format!("after-rebind-{}", uuid::Uuid::new_v4());
    bob.send(&format!(
        r#"<message to="{room}" type="groupchat" id="msg-{}"><body>{bob_body}</body></message>"#,
        uuid::Uuid::new_v4()
    ))
    .await
    .expect("bob sends groupchat message");
    bob.recv_matching(|frame| frame.contains(&bob_body))
        .await
        .expect("bob receives his own reflection");
    assert_no_frame_matching(
        &mut alice_fresh,
        Duration::from_millis(1000),
        |frame| frame.contains(&bob_body),
        "a fresh bind that never joined the room must not receive room fan-out",
    )
    .await;

    let _ = admin.close().await;
    let _ = bob.close().await;
    let _ = alice_fresh.close().await;
}

fn element_to_xml(element: Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize XML");
    String::from_utf8(buf).expect("xmpp_parsers serializes UTF-8")
}

fn disco_info_get_xml(to: &str, id: &str) -> String {
    element_to_xml(
        Element::builder("iq", "jabber:client")
            .attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("id").to_owned(),
                id,
            )
            .attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("type").to_owned(),
                "get",
            )
            .attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("to").to_owned(),
                to,
            )
            .append(Element::builder("query", "http://jabber.org/protocol/disco#info").build())
            .build(),
    )
}

fn groupchat_message_xml(to: &str, id: &str, body: &str) -> String {
    element_to_xml(
        Element::builder("message", "jabber:client")
            .attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("id").to_owned(),
                id,
            )
            .attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("type").to_owned(),
                "groupchat",
            )
            .attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("to").to_owned(),
                to,
            )
            .append(
                Element::builder("body", "jabber:client")
                    .append(body)
                    .build(),
            )
            .build(),
    )
}

/// XEP-0045 §7.2.2: a binary compiled with cluster support must
/// retain portable single-node semantics when clustering is disabled at
/// runtime. In that mode there is no ordered-relay bridge, so the first
/// join must be handled locally rather than bounced with the
/// `<resource-constraint/>` that tells the client to retry a genuinely
/// unavailable remote owner. A completed join is proven by the
/// room-authored self-presence carrying status code 110, followed by the
/// room subject.
#[cfg(feature = "clustering")]
#[tokio::test]
async fn xep_0045_cluster_capable_binary_with_runtime_clustering_disabled_joins_locally() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_envs(&[], &[("WADDLE_CLUSTERING_ENABLED", "false")]);
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "runtime-clustering-disabled").await;
    let room = format!(
        "runtime-clustering-disabled-{}@muc.{DOMAIN}",
        uuid::Uuid::new_v4()
    );
    let room_nick = format!("{room}/{ADMIN}");
    let join = element_to_xml(
        Element::builder("presence", "jabber:client")
            .attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("to").to_owned(),
                room_nick.as_str(),
            )
            .append(Element::builder("x", NS_MUC).build())
            .build(),
    );

    admin.send(&join).await.expect("send first local MUC join");
    let join_frames = admin
        .recv_until(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                find_descendant(&element, "subject", "jabber:client").is_some()
                    || (element.name() == "presence" && element.attr("type") == Some("error"))
            })
        })
        .await
        .expect("first join must complete or return a typed presence error");

    let parsed = join_frames
        .iter()
        .map(|frame| {
            frame
                .parse::<Element>()
                .unwrap_or_else(|error| panic!("join frame must parse as XML: {error}; {frame}"))
        })
        .collect::<Vec<_>>();
    assert!(
        parsed
            .iter()
            .all(|frame| find_descendant(frame, "resource-constraint", NS_XMPP_STANZAS).is_none()),
        "runtime-disabled clustering must not turn a local first join into a retryable \
         resource-constraint: {join_frames:?}"
    );
    assert!(
        parsed.iter().any(|frame| {
            frame.name() == "presence"
                && frame.attr("from") == Some(room_nick.as_str())
                && frame
                    .children()
                    .find(|child| child.name() == "x" && child.ns() == NS_MUC_USER)
                    .is_some_and(|muc_user| {
                        muc_user.children().any(|child| {
                            child.name() == "status" && child.attr("code") == Some("110")
                        })
                    })
        }),
        "XEP-0045 self-presence status 110 must confirm local join readiness: {join_frames:?}"
    );
    assert!(
        parsed
            .iter()
            .any(|frame| find_descendant(frame, "subject", "jabber:client").is_some()),
        "a successful first join must finish with the room subject: {join_frames:?}"
    );

    let _ = admin.close().await;
}

/// XEP-0045 §7.4 stable-id (#1265 item 14): the service and rooms
/// advertise `http://jabber.org/protocol/muc#stable_id`, and the
/// reflected groupchat message keeps the sender's original `id`.
#[tokio::test]
async fn xep_0045_stable_id_advertised_and_reflected_id_preserved() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "stable-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "stable-alice").await;
    let mut bob = connect(&server, BOB, &bob_pass, "stable-bob").await;

    let room = format!("stable-id-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;
    join_room(&mut bob, &room, BOB).await;

    // Service-level advertisement.
    alice
        .send(&disco_info_get_xml(
            &format!("muc.{DOMAIN}"),
            "disco-svc-stable",
        ))
        .await
        .expect("send service disco");
    let service_disco = alice
        .recv_matching(|frame| frame.contains("disco-svc-stable"))
        .await
        .expect("service disco result");
    assert!(
        service_disco.contains("http://jabber.org/protocol/muc#stable_id"),
        "MUC service must advertise stable_id (§7.4): {service_disco}"
    );

    // Room-level advertisement.
    alice
        .send(&disco_info_get_xml(&room, "disco-room-stable"))
        .await
        .expect("send room disco");
    let room_disco = alice
        .recv_matching(|frame| frame.contains("disco-room-stable"))
        .await
        .expect("room disco result");
    assert!(
        room_disco.contains("http://jabber.org/protocol/muc#stable_id"),
        "room must advertise stable_id (§7.4): {room_disco}"
    );

    // Behavior backing the advertisement: the reflected groupchat
    // message keeps the sender's original id.
    let original_id = format!("stable-{}", uuid::Uuid::new_v4());
    let body = format!("stable-id-body-{}", uuid::Uuid::new_v4());
    alice
        .send(&groupchat_message_xml(&room, &original_id, &body))
        .await
        .expect("alice sends groupchat message");
    let bob_copy = bob
        .recv_matching(|frame| frame.contains(&body))
        .await
        .expect("bob receives reflection");
    assert!(
        bob_copy.contains(&format!("id='{original_id}'")),
        "reflected message must keep the sender's original id (§7.4): {bob_copy}"
    );

    let _ = admin.close().await;
    let _ = alice.close().await;
    let _ = bob.close().await;
}

/// XEP-0045 §7.4 (#1263): a groupchat message to a room that does not
/// exist must be answered with a message error carrying
/// `<item-not-found/>` — the previous behavior silently dropped the
/// message and the sender never learned it was lost.
#[tokio::test]
async fn xep_0045_groupchat_to_nonexistent_room_bounces_item_not_found() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "ghost-room-sender").await;

    let ghost_room = format!("never-created-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let msg_id = format!("ghost-{}", uuid::Uuid::new_v4());
    let message = Element::builder("message", "jabber:client")
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("to").to_owned(),
            ghost_room.clone(),
        )
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("type").to_owned(),
            "groupchat",
        )
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("id").to_owned(),
            msg_id.clone(),
        )
        .append(
            Element::builder("body", "jabber:client")
                .append("hello?")
                .build(),
        )
        .build();
    let mut bytes = Vec::new();
    message.write_to(&mut bytes).expect("serialize message");
    admin
        .send(&String::from_utf8(bytes).expect("utf8 message"))
        .await
        .expect("send groupchat to nonexistent room");

    let frame = admin
        .recv_matching(|frame| frame.contains(&msg_id))
        .await
        .expect("error reply for message to nonexistent room");
    let element = frame
        .parse::<Element>()
        .unwrap_or_else(|err| panic!("frame must parse as XML: {err}; frame={frame}"));
    assert_eq!(element.name(), "message", "expected <message>: {frame}");
    assert_eq!(
        element.attr("type"),
        Some("error"),
        "reply must be a message error: {frame}"
    );
    assert_eq!(
        element.attr("from"),
        Some(ghost_room.as_str()),
        "the error comes from the room bare JID: {frame}"
    );
    assert!(
        find_descendant(&element, "item-not-found", NS_XMPP_STANZAS).is_some(),
        "XEP-0045 §7.4: nonexistent room bounces <item-not-found/>: {frame}"
    );

    let _ = admin.close().await;
}
