//! Waddle 1:1 DM pinned-message integration tests over WebSocket.

mod ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const NS_WADDLE_PIN: &str = "urn:waddle:pin:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn user_client(
    server: &TestServer,
    username: &str,
    password: &str,
    resource: &str,
) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, username, password, resource)
        .await
        .expect("connect and auth")
}

fn frame_has_iq_id(frame: &str, id: &str) -> bool {
    frame.contains(&format!(r#"id='{id}'"#)) || frame.contains(&format!(r#"id="{id}""#))
}

fn extract_pin_event_target(frame: &str) -> String {
    extract_attr_after(frame, "pin-event", "target")
        .unwrap_or_else(|| panic!("pin event must carry a target attribute: {frame}"))
}

async fn fetch_dm_pins(client: &mut WsXmppClient, peer: &str, id: &str) -> String {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" to="{peer}" id="{id}">
                <query xmlns="{NS_WADDLE_PIN}"/>
            </iq>"#
        ))
        .await
        .expect("send DM pin query");
    client
        .recv_matching(|frame| frame.contains("<iq") && frame_has_iq_id(frame, id))
        .await
        .expect("pin query response")
}

fn pin_entry_count(frame: &str) -> usize {
    frame.matches("<pin ").count()
}

async fn block_peer(client: &mut WsXmppClient, peer: &str, id: &str) {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="{id}">
                <block xmlns="urn:xmpp:blocking">
                    <item jid="{peer}"/>
                </block>
            </iq>"#
        ))
        .await
        .expect("send blocking IQ");
    let response = client
        .recv_matching(|frame| frame.contains("<iq") && frame_has_iq_id(frame, id))
        .await
        .expect("blocking IQ response");
    assert!(
        response.contains("type='result'") || response.contains(r#"type="result""#),
        "blocking IQ should succeed: {response}"
    );
}

#[tokio::test]
async fn dm_pin_updates_peer_live_and_survives_reload() {
    let _guard = TEST_SERIAL.lock().await;
    let server =
        TestServer::start_with_extra_accounts(&[("alice", "alice-pass"), ("bob", "bob-pass")]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-pin-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "dm-pin-bob").await;

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-pin-target">
                <body>pin this direct message</body>
            </message>"#,
        )
        .await
        .expect("send target DM");
    let delivered = bob
        .recv_matching(|frame| frame.contains("pin this direct message"))
        .await
        .expect("Bob receives target DM");
    let target_stanza_id = extract_attr_after(&delivered, "stanza-id", "id")
        .unwrap_or_else(|| panic!("DM delivery must carry XEP-0359 stanza-id: {delivered}"));

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-pin-request">
                <pinned xmlns="{NS_WADDLE_PIN}" target="{target_stanza_id}"/>
            </message>"#
        ))
        .await
        .expect("send DM pin request");

    let live_pin = bob
        .recv_matching(|frame| {
            frame.contains("<pin-event")
                && frame.contains("action='pinned'")
                && frame.contains("target=")
        })
        .await
        .expect("Bob receives live DM pin event");
    let canonical_pin_target = extract_pin_event_target(&live_pin);
    assert!(
        live_pin.contains("by='alice@localhost'") && live_pin.contains("pin this direct message"),
        "live DM pin event must identify the pinner and carry a preview: {live_pin}"
    );

    let alice_pins = fetch_dm_pins(&mut alice, "bob@localhost", "alice-dm-pins").await;
    assert!(
        alice_pins.contains("<query")
            && alice_pins.contains(&canonical_pin_target)
            && alice_pins.contains("pin this direct message"),
        "Alice's server-fetched DM pin list must include the pin: {alice_pins}"
    );
    let bob_pins = fetch_dm_pins(&mut bob, "alice@localhost", "bob-dm-pins").await;
    assert!(
        bob_pins.contains("<query")
            && bob_pins.contains(&canonical_pin_target)
            && bob_pins.contains("pin this direct message"),
        "Bob's server-fetched DM pin list must include the same pair pin: {bob_pins}"
    );

    let _ = bob.close().await;
    let mut bob_reloaded = user_client(&server, "bob", "bob-pass", "dm-pin-bob-reloaded").await;
    let bob_reloaded_pins =
        fetch_dm_pins(&mut bob_reloaded, "alice@localhost", "bob-dm-pins-reload").await;
    assert!(
        bob_reloaded_pins.contains(&canonical_pin_target)
            && bob_reloaded_pins.contains("pin this direct message"),
        "Bob must fetch the same DM pin list after reconnect: {bob_reloaded_pins}"
    );
}

#[tokio::test]
async fn dm_pin_deduplicates_archive_local_target_ids() {
    let _guard = TEST_SERIAL.lock().await;
    let server =
        TestServer::start_with_extra_accounts(&[("alice", "alice-pass"), ("bob", "bob-pass")]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-pin-dual-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "dm-pin-dual-bob").await;

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-dual-target">
                <body>one logical DM, two archive ids</body>
            </message>"#,
        )
        .await
        .expect("send target DM");
    let delivered = bob
        .recv_matching(|frame| frame.contains("one logical DM, two archive ids"))
        .await
        .expect("Bob receives target DM");
    let bob_archive_id = extract_attr_after(&delivered, "stanza-id", "id")
        .unwrap_or_else(|| panic!("DM delivery must carry XEP-0359 stanza-id: {delivered}"));

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-dual-pin-a">
                <pinned xmlns="{NS_WADDLE_PIN}" target="{bob_archive_id}"/>
            </message>"#
        ))
        .await
        .expect("Alice pins with Bob archive id");
    let alice_pin_event = bob
        .recv_matching(|frame| frame.contains("<pin-event") && frame.contains("action='pinned'"))
        .await
        .expect("Bob receives Alice pin");
    let canonical_target = extract_pin_event_target(&alice_pin_event);

    bob.send(&format!(
        r#"<message xmlns="jabber:client" type="chat" to="alice@localhost" id="dm-dual-pin-b">
            <pinned xmlns="{NS_WADDLE_PIN}" target="dm-dual-target"/>
        </message>"#
    ))
    .await
    .expect("Bob pins with Alice wire id");
    let bob_pin_event = alice
        .recv_matching(|frame| {
            frame.contains("<pin-event")
                && frame.contains("action='pinned'")
                && frame.contains(&canonical_target)
        })
        .await
        .expect("Alice receives Bob pin");
    assert!(
        bob_pin_event.contains("one logical DM, two archive ids"),
        "replacement pin event should keep the preview: {bob_pin_event}"
    );

    let alice_pins = fetch_dm_pins(&mut alice, "bob@localhost", "alice-dual-pins").await;
    assert_eq!(
        pin_entry_count(&alice_pins),
        1,
        "both archive-local target ids must converge to one pair pin: {alice_pins}"
    );
    assert!(
        alice_pins.contains(&canonical_target),
        "pin list should use the canonical pair-stable target: {alice_pins}"
    );
}

#[tokio::test]
async fn dm_unpin_resolves_archive_local_target_ids() {
    let _guard = TEST_SERIAL.lock().await;
    let server =
        TestServer::start_with_extra_accounts(&[("alice", "alice-pass"), ("bob", "bob-pass")]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-unpin-dual-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "dm-unpin-dual-bob").await;

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-unpin-dual-target">
                <body>unpin through the other archive id</body>
            </message>"#,
        )
        .await
        .expect("send target DM");
    let delivered = bob
        .recv_matching(|frame| frame.contains("unpin through the other archive id"))
        .await
        .expect("Bob receives target DM");
    let bob_archive_id = extract_attr_after(&delivered, "stanza-id", "id")
        .unwrap_or_else(|| panic!("DM delivery must carry XEP-0359 stanza-id: {delivered}"));

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-unpin-dual-pin">
                <pinned xmlns="{NS_WADDLE_PIN}" target="{bob_archive_id}"/>
            </message>"#
        ))
        .await
        .expect("Alice pins with Bob archive id");
    let pin_event = bob
        .recv_matching(|frame| frame.contains("<pin-event") && frame.contains("action='pinned'"))
        .await
        .expect("Bob receives Alice pin");
    let canonical_target = extract_pin_event_target(&pin_event);

    bob.send(&format!(
        r#"<message xmlns="jabber:client" type="chat" to="alice@localhost" id="dm-unpin-dual-request">
            <unpinned xmlns="{NS_WADDLE_PIN}" target="dm-unpin-dual-target"/>
        </message>"#
    ))
    .await
    .expect("Bob unpins with Alice wire id");
    alice
        .recv_matching(|frame| {
            frame.contains("<pin-event")
                && frame.contains("action='unpinned'")
                && frame.contains(&canonical_target)
        })
        .await
        .expect("Alice receives canonical unpin event");

    let alice_pins = fetch_dm_pins(&mut alice, "bob@localhost", "alice-dual-unpin-pins").await;
    assert!(
        !alice_pins.contains(&canonical_target),
        "alternate-id unpin must remove the canonical stored pin: {alice_pins}"
    );
}

#[tokio::test]
async fn dm_unpin_converges_both_participants() {
    let _guard = TEST_SERIAL.lock().await;
    let server =
        TestServer::start_with_extra_accounts(&[("alice", "alice-pass"), ("bob", "bob-pass")]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-unpin-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "dm-unpin-bob").await;

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-unpin-target">
                <body>pin then unpin this direct message</body>
            </message>"#,
        )
        .await
        .expect("send target DM");
    let delivered = bob
        .recv_matching(|frame| frame.contains("pin then unpin this direct message"))
        .await
        .expect("Bob receives target DM");
    let target_stanza_id = extract_attr_after(&delivered, "stanza-id", "id")
        .unwrap_or_else(|| panic!("DM delivery must carry XEP-0359 stanza-id: {delivered}"));

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-unpin-pin">
                <pinned xmlns="{NS_WADDLE_PIN}" target="{target_stanza_id}"/>
            </message>"#
        ))
        .await
        .expect("send DM pin request");
    let pin_event = bob
        .recv_matching(|frame| frame.contains("<pin-event") && frame.contains("action='pinned'"))
        .await
        .expect("Bob receives live DM pin event");
    let canonical_target = extract_pin_event_target(&pin_event);

    bob.send(&format!(
        r#"<message xmlns="jabber:client" type="chat" to="alice@localhost" id="dm-unpin-request">
            <unpinned xmlns="{NS_WADDLE_PIN}" target="{target_stanza_id}"/>
        </message>"#
    ))
    .await
    .expect("send DM unpin request");

    let alice_unpin = alice
        .recv_matching(|frame| {
            frame.contains("<pin-event")
                && frame.contains("action='unpinned'")
                && frame.contains(&canonical_target)
                && frame.contains("by='bob@localhost'")
        })
        .await
        .expect("Alice receives live DM unpin event");
    assert!(
        !alice_unpin.contains("<preview"),
        "unpin event must not carry a stale preview: {alice_unpin}"
    );

    let alice_pins = fetch_dm_pins(&mut alice, "bob@localhost", "alice-dm-pins-after-unpin").await;
    assert!(
        !alice_pins.contains(&canonical_target),
        "Alice's DM pin list must remove the unpinned entry: {alice_pins}"
    );
    let bob_pins = fetch_dm_pins(&mut bob, "alice@localhost", "bob-dm-pins-after-unpin").await;
    assert!(
        !bob_pins.contains(&canonical_target),
        "Bob's DM pin list must remove the unpinned entry: {bob_pins}"
    );
}

#[tokio::test]
async fn dm_retraction_removes_pinned_message() {
    let _guard = TEST_SERIAL.lock().await;
    let server =
        TestServer::start_with_extra_accounts(&[("alice", "alice-pass"), ("bob", "bob-pass")]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-pin-retract-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "dm-pin-retract-bob").await;

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-retract-pin-target">
                <body>pin then retract this direct message</body>
            </message>"#,
        )
        .await
        .expect("send target DM");
    let delivered = bob
        .recv_matching(|frame| frame.contains("pin then retract this direct message"))
        .await
        .expect("Bob receives target DM");
    let target_stanza_id = extract_attr_after(&delivered, "stanza-id", "id")
        .unwrap_or_else(|| panic!("DM delivery must carry XEP-0359 stanza-id: {delivered}"));

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-retract-pin-request">
                <pinned xmlns="{NS_WADDLE_PIN}" target="{target_stanza_id}"/>
            </message>"#
        ))
        .await
        .expect("send DM pin request");
    let pin_event = bob
        .recv_matching(|frame| frame.contains("<pin-event") && frame.contains("action='pinned'"))
        .await
        .expect("Bob receives live DM pin event");
    let canonical_target = extract_pin_event_target(&pin_event);

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-pin-retraction">
                <retract xmlns="urn:xmpp:message-retract:1" id="{target_stanza_id}"/>
                <body>/me retracted a previous message</body>
            </message>"#
        ))
        .await
        .expect("send DM retraction");
    let bob_unpin = bob
        .recv_matching(|frame| {
            frame.contains("<pin-event")
                && frame.contains("action='unpinned'")
                && frame.contains("reason='retracted'")
                && frame.contains(&canonical_target)
        })
        .await
        .expect("Bob receives pin removal caused by retraction");
    assert!(
        bob_unpin.contains("by='alice@localhost'"),
        "retraction-caused unpin should be attributed to the retractor: {bob_unpin}"
    );

    let bob_pins = fetch_dm_pins(&mut bob, "alice@localhost", "bob-dm-pins-after-retract").await;
    assert!(
        !bob_pins.contains(&canonical_target),
        "retracted DM must not remain pinned: {bob_pins}"
    );
}

#[tokio::test]
async fn dm_pin_after_retraction_is_rejected() {
    let _guard = TEST_SERIAL.lock().await;
    let server =
        TestServer::start_with_extra_accounts(&[("alice", "alice-pass"), ("bob", "bob-pass")]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-pin-tombstone-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "dm-pin-tombstone-bob").await;

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-tombstone-target">
                <body>this direct message will be tombstoned</body>
            </message>"#,
        )
        .await
        .expect("send target DM");
    let delivered = bob
        .recv_matching(|frame| frame.contains("this direct message will be tombstoned"))
        .await
        .expect("Bob receives target DM");
    let _target_stanza_id = extract_attr_after(&delivered, "stanza-id", "id")
        .unwrap_or_else(|| panic!("DM delivery must carry XEP-0359 stanza-id: {delivered}"));

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-tombstone-retract">
                <retract xmlns="urn:xmpp:message-retract:1" id="dm-tombstone-target"/>
                <body>/me retracted a previous message</body>
            </message>"#,
        )
        .await
        .expect("send DM retraction");
    tokio::time::sleep(Duration::from_millis(500)).await;

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-tombstone-pin">
                <pinned xmlns="{NS_WADDLE_PIN}" target="dm-tombstone-target"/>
            </message>"#
        ))
        .await
        .expect("send stale DM pin request");
    let error = alice
        .recv_matching(|frame| {
            frame.contains("dm-tombstone-pin") && frame.contains("<item-not-found")
        })
        .await
        .expect("pinning a tombstoned DM is rejected");
    assert!(
        error.contains("Pinned DM target was not found"),
        "tombstone rejection should not expose the original preview: {error}"
    );
    let bob_pins = fetch_dm_pins(&mut bob, "alice@localhost", "bob-tombstone-pins").await;
    assert!(
        !bob_pins.contains("this direct message will be tombstoned"),
        "tombstoned DM must not be stored as a pin: {bob_pins}"
    );
}

#[tokio::test]
async fn dm_pin_lists_are_scoped_per_pair() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", "alice-pass"),
        ("bob", "bob-pass"),
        ("charlie", "charlie-pass"),
    ]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-scope-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "dm-scope-bob").await;
    let mut charlie = user_client(&server, "charlie", "charlie-pass", "dm-scope-charlie").await;

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-scope-target">
                <body>Alice and Bob only</body>
            </message>"#,
        )
        .await
        .expect("send target DM");
    let delivered = bob
        .recv_matching(|frame| frame.contains("Alice and Bob only"))
        .await
        .expect("Bob receives target DM");
    let target_stanza_id = extract_attr_after(&delivered, "stanza-id", "id")
        .unwrap_or_else(|| panic!("DM delivery must carry XEP-0359 stanza-id: {delivered}"));

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-scope-pin">
                <pinned xmlns="{NS_WADDLE_PIN}" target="{target_stanza_id}"/>
            </message>"#
        ))
        .await
        .expect("send DM pin request");
    let pin_event = bob
        .recv_matching(|frame| frame.contains("<pin-event") && frame.contains("action='pinned'"))
        .await
        .expect("Bob receives live DM pin event");
    let canonical_target = extract_pin_event_target(&pin_event);

    let bob_pins = fetch_dm_pins(&mut bob, "alice@localhost", "bob-alice-pins").await;
    assert!(
        bob_pins.contains(&canonical_target),
        "Bob/Alice pair should contain the pin: {bob_pins}"
    );
    let charlie_bob_pins = fetch_dm_pins(&mut charlie, "bob@localhost", "charlie-bob-pins").await;
    assert!(
        !charlie_bob_pins.contains(&canonical_target)
            && !charlie_bob_pins.contains("Alice and Bob only"),
        "Charlie/Bob pair must not leak Alice/Bob pins: {charlie_bob_pins}"
    );
}

#[tokio::test]
async fn dm_pin_target_must_belong_to_the_pair() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", "alice-pass"),
        ("bob", "bob-pass"),
        ("charlie", "charlie-pass"),
    ]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-cross-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "dm-cross-bob").await;
    let mut charlie = user_client(&server, "charlie", "charlie-pass", "dm-cross-charlie").await;

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="charlie@localhost" id="dm-cross-target">
                <body>Alice and Charlie only</body>
            </message>"#,
        )
        .await
        .expect("send cross-pair target DM");
    let delivered = charlie
        .recv_matching(|frame| frame.contains("Alice and Charlie only"))
        .await
        .expect("Charlie receives target DM");
    let target_stanza_id = extract_attr_after(&delivered, "stanza-id", "id")
        .unwrap_or_else(|| panic!("DM delivery must carry XEP-0359 stanza-id: {delivered}"));

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-cross-pin">
                <pinned xmlns="{NS_WADDLE_PIN}" target="{target_stanza_id}"/>
            </message>"#
        ))
        .await
        .expect("send cross-pair DM pin request");

    let error = alice
        .recv_matching(|frame| frame.contains("dm-cross-pin") && frame.contains("<item-not-found"))
        .await
        .expect("cross-pair DM pin is rejected");
    assert!(
        error.contains("Pinned DM target was not found"),
        "cross-pair pin rejection should not leak the target preview: {error}"
    );
    let bob_unexpected_pin = bob
        .recv_timeout(Duration::from_millis(250))
        .await
        .unwrap_or_default();
    assert!(
        !bob_unexpected_pin.contains("<pin-event"),
        "Bob must not receive a pin event for Alice/Charlie content: {bob_unexpected_pin}"
    );
    let bob_pins = fetch_dm_pins(&mut bob, "alice@localhost", "bob-cross-pins").await;
    assert!(
        !bob_pins.contains(&target_stanza_id) && !bob_pins.contains("Alice and Charlie only"),
        "Alice/Bob pair must not store Alice/Charlie pins: {bob_pins}"
    );
}

#[tokio::test]
async fn dm_retraction_from_non_author_does_not_clear_pin() {
    let _guard = TEST_SERIAL.lock().await;
    let server =
        TestServer::start_with_extra_accounts(&[("alice", "alice-pass"), ("bob", "bob-pass")]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-invalid-retract-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "dm-invalid-retract-bob").await;

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-invalid-retract-target">
                <body>Alice authored this pinned direct message</body>
            </message>"#,
        )
        .await
        .expect("send target DM");
    let delivered = bob
        .recv_matching(|frame| frame.contains("Alice authored this pinned direct message"))
        .await
        .expect("Bob receives target DM");
    let target_stanza_id = extract_attr_after(&delivered, "stanza-id", "id")
        .unwrap_or_else(|| panic!("DM delivery must carry XEP-0359 stanza-id: {delivered}"));

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-invalid-retract-pin">
                <pinned xmlns="{NS_WADDLE_PIN}" target="{target_stanza_id}"/>
            </message>"#
        ))
        .await
        .expect("send DM pin request");
    let pin_event = bob
        .recv_matching(|frame| frame.contains("<pin-event") && frame.contains("action='pinned'"))
        .await
        .expect("Bob receives live DM pin event");
    let canonical_target = extract_pin_event_target(&pin_event);

    bob.send(&format!(
        r#"<message xmlns="jabber:client" type="chat" to="alice@localhost" id="dm-invalid-retract">
            <retract xmlns="urn:xmpp:message-retract:1" id="{target_stanza_id}"/>
            <body>/me retracted a previous message</body>
        </message>"#
    ))
    .await
    .expect("send invalid DM retraction");

    let alice_unexpected_unpin = alice
        .recv_timeout(Duration::from_millis(250))
        .await
        .unwrap_or_default();
    assert!(
        !alice_unexpected_unpin.contains("<pin-event")
            || !alice_unexpected_unpin.contains("action='unpinned'"),
        "invalid retraction must not fan out a pin removal: {alice_unexpected_unpin}"
    );
    let bob_pins = fetch_dm_pins(&mut bob, "alice@localhost", "bob-invalid-retract-pins").await;
    assert!(
        bob_pins.contains(&canonical_target)
            && bob_pins.contains("Alice authored this pinned direct message"),
        "pin must remain after rejected non-author retraction: {bob_pins}"
    );
}

#[tokio::test]
async fn dm_pin_to_remote_domain_returns_forbidden() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[("alice", "alice-pass")]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-pin-remote-alice").await;

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="mallory@example.test" id="dm-pin-remote">
                <pinned xmlns="{NS_WADDLE_PIN}" target="remote-target"/>
            </message>"#
        ))
        .await
        .expect("send remote-domain DM pin request");

    let error = alice
        .recv_matching(|frame| frame.contains("dm-pin-remote") && frame.contains("<forbidden"))
        .await
        .expect("remote-domain DM pin is rejected");
    assert!(
        error.contains("type='error'") || error.contains(r#"type="error""#),
        "remote-domain pin must return a stanza error: {error}"
    );
}

#[tokio::test]
async fn dm_pin_event_respects_recipient_blocklist() {
    let _guard = TEST_SERIAL.lock().await;
    let server =
        TestServer::start_with_extra_accounts(&[("alice", "alice-pass"), ("bob", "bob-pass")]);
    let mut alice = user_client(&server, "alice", "alice-pass", "dm-pin-block-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "dm-pin-block-bob").await;

    alice
        .send(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-block-target">
                <body>blocked recipient should not see pin event</body>
            </message>"#,
        )
        .await
        .expect("send target DM");
    let delivered = bob
        .recv_matching(|frame| frame.contains("blocked recipient should not see pin event"))
        .await
        .expect("Bob receives target DM before blocking Alice");
    let target_stanza_id = extract_attr_after(&delivered, "stanza-id", "id")
        .unwrap_or_else(|| panic!("DM delivery must carry XEP-0359 stanza-id: {delivered}"));

    block_peer(&mut bob, "alice@localhost", "dm-pin-block-bob-blocks-alice").await;

    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@localhost" id="dm-block-pin">
                <pinned xmlns="{NS_WADDLE_PIN}" target="{target_stanza_id}"/>
            </message>"#
        ))
        .await
        .expect("send DM pin request after block");

    alice
        .recv_matching(|frame| {
            frame.contains("<pin-event")
                && frame.contains("action='pinned'")
                && frame.contains("blocked recipient should not see pin event")
        })
        .await
        .expect("sender still receives local pin event");
    let bob_unexpected_pin = bob
        .recv_timeout(Duration::from_millis(250))
        .await
        .unwrap_or_default();
    assert!(
        !bob_unexpected_pin.contains("<pin-event"),
        "blocked recipient must not receive live DM pin events: {bob_unexpected_pin}"
    );
}
