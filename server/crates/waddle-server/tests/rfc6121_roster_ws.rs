//! RFC 6121 roster behavior over the active WebSocket C2S transport.

mod ws_common;

use std::time::Duration;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";

async fn connect_alice_bob() -> (TestServer, WsXmppClient, WsXmppClient) {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

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

async fn send_roster_get(client: &mut WsXmppClient, id: &str) -> String {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><query xmlns="jabber:iq:roster"/></iq>"#
        ))
        .await
        .expect("send roster get");
    client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("roster get result")
}

async fn send_roster_get_with_ver(client: &mut WsXmppClient, id: &str, ver: &str) -> String {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><query xmlns="jabber:iq:roster" ver="{ver}"/></iq>"#
        ))
        .await
        .expect("send versioned roster get");
    client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("versioned roster get result")
}

async fn send_roster_set(client: &mut WsXmppClient, id: &str, item: &str) -> (String, String) {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="{id}"><query xmlns="jabber:iq:roster">{item}</query></iq>"#
        ))
        .await
        .expect("send roster set");
    let push = client
        .recv_matching(|frame| frame.contains("jabber:iq:roster") && frame.contains("type='set'"))
        .await
        .expect("roster push");
    let result = client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("roster set result");
    (push, result)
}

fn roster_version(frame: &str) -> String {
    // xmpp-parsers 0.22 serializes attribute values with single quotes,
    // but earlier minidom used double — accept either at parse time.
    let (marker, terminator) = if frame.contains("ver=\"") {
        ("ver=\"", '"')
    } else if frame.contains("ver='") {
        ("ver='", '\'')
    } else {
        panic!("missing roster version: {frame}");
    };
    let start = frame.find(marker).expect("marker present") + marker.len();
    let end = frame[start..]
        .find(terminator)
        .unwrap_or_else(|| panic!("unterminated roster version: {frame}"));
    frame[start..start + end].to_string()
}

#[tokio::test]
async fn roster_get_add_update_remove_uses_durable_state() {
    let (_server, mut alice, bob) = connect_alice_bob().await;

    let empty = send_roster_get(&mut alice, "roster-get-empty").await;
    assert!(empty.contains("type='result'"), "expected result: {empty}");
    assert!(
        empty.contains("jabber:iq:roster"),
        "expected roster query: {empty}"
    );
    assert!(empty.contains("ver="), "expected roster version: {empty}");
    assert!(
        !empty.contains("<item"),
        "new user roster should be empty: {empty}"
    );

    let (add_push, add_result) = send_roster_set(
        &mut alice,
        "roster-add-bob",
        r#"<item jid="bob@localhost" name="Bob"><group>Friends</group></item>"#,
    )
    .await;
    assert!(add_push.contains("jid='bob@localhost'"), "push: {add_push}");
    assert!(
        add_push.contains("subscription='none'"),
        "server controls subscription state: {add_push}"
    );
    let add_version = roster_version(&add_push);
    assert!(
        add_result.contains("type='result'"),
        "expected set result: {add_result}"
    );

    let after_add = send_roster_get(&mut alice, "roster-get-after-add").await;
    assert_eq!(
        roster_version(&after_add),
        add_version,
        "roster get after add should return current version"
    );
    let unchanged =
        send_roster_get_with_ver(&mut alice, "roster-get-unchanged", &add_version).await;
    assert!(
        unchanged.contains("type='result'") && !unchanged.contains("jabber:iq:roster"),
        "matching roster ver should return an empty unchanged result: {unchanged}"
    );
    let stale = send_roster_get_with_ver(&mut alice, "roster-get-stale", "stale-version").await;
    assert!(
        stale.contains("jabber:iq:roster") && stale.contains("bob@localhost"),
        "stale roster ver should return full roster state: {stale}"
    );
    assert!(
        after_add.contains("jid='bob@localhost'") && after_add.contains("name='Bob'"),
        "added item should be durable: {after_add}"
    );
    assert!(
        after_add.contains("<group xmlns='jabber:iq:roster'>Friends</group>")
            || after_add.contains("<group>Friends</group>"),
        "group should be durable: {after_add}"
    );

    let (update_push, _) = send_roster_set(
        &mut alice,
        "roster-update-bob",
        r#"<item jid="bob@localhost" name="Robert" subscription="both"><group>Work</group></item>"#,
    )
    .await;
    assert!(
        update_push.contains("name='Robert'") && update_push.contains("subscription='none'"),
        "update must ignore client subscription attribute: {update_push}"
    );
    let update_version = roster_version(&update_push);
    assert_ne!(
        add_version, update_version,
        "roster version should change after update"
    );

    let after_update = send_roster_get(&mut alice, "roster-get-after-update").await;
    assert_eq!(
        roster_version(&after_update),
        update_version,
        "roster get after update should return current version"
    );
    assert!(
        after_update.contains("name='Robert'")
            && after_update.contains("subscription='none'")
            && after_update.contains("Work"),
        "updated item should preserve server-owned subscription state: {after_update}"
    );

    let (remove_push, _) = send_roster_set(
        &mut alice,
        "roster-remove-bob",
        r#"<item jid="bob@localhost" subscription="remove"/>"#,
    )
    .await;
    assert!(
        remove_push.contains("jid='bob@localhost'")
            && remove_push.contains("subscription='remove'"),
        "remove push should carry subscription=remove: {remove_push}"
    );
    let remove_version = roster_version(&remove_push);
    assert_ne!(
        update_version, remove_version,
        "roster version should change after remove"
    );

    let after_remove = send_roster_get(&mut alice, "roster-get-after-remove").await;
    assert_eq!(
        roster_version(&after_remove),
        remove_version,
        "roster get after remove should return current version"
    );
    assert!(
        !after_remove.contains("jid='bob@localhost'"),
        "removed item should not be returned: {after_remove}"
    );

    alice
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="roster-remove-missing"><query xmlns="jabber:iq:roster"><item jid="bob@localhost" subscription="remove"/></query></iq>"#,
        )
        .await
        .expect("send missing remove");
    let missing = alice
        .recv_matching(|frame| frame.contains("roster-remove-missing"))
        .await
        .expect("missing remove error");
    assert!(
        missing.contains("item-not-found"),
        "missing remove should return item-not-found: {missing}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn roster_set_pushes_only_to_interested_connected_user_resources() {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let mut desktop = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("desktop-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("desktop connection");
    let mut phone = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("phone-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("phone connection");
    let mut tablet = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("tablet-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("tablet connection");

    let _desktop_roster = send_roster_get(&mut desktop, "desktop-initial-roster").await;
    let _phone_roster = send_roster_get(&mut phone, "phone-initial-roster").await;

    desktop
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="roster-fanout"><query xmlns="jabber:iq:roster"><item jid="bob@localhost" name="Bob"/></query></iq>"#,
        )
        .await
        .expect("send roster set");

    let desktop_push = desktop
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster") && frame.contains("bob@localhost")
        })
        .await
        .expect("desktop roster push");
    let result = desktop
        .recv_matching(|frame| frame.contains("roster-fanout"))
        .await
        .expect("set result");
    let phone_push = phone
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster") && frame.contains("bob@localhost")
        })
        .await
        .expect("phone roster push");

    assert!(
        desktop_push.contains("type='set'"),
        "desktop push: {desktop_push}"
    );
    assert!(
        phone_push.contains("type='set'"),
        "phone push: {phone_push}"
    );
    assert!(result.contains("type='result'"), "set result: {result}");
    let tablet_frame = tablet.recv_timeout(Duration::from_millis(250)).await;
    assert!(
        tablet_frame.is_err(),
        "uninterested resource must not receive roster push: {tablet_frame:?}"
    );

    tablet
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="roster-uninterested-source"><query xmlns="jabber:iq:roster"><item jid="carol@localhost" name="Carol"/></query></iq>"#,
        )
        .await
        .expect("send roster set from uninterested resource");
    let tablet_result = tablet
        .recv_timeout(Duration::from_secs(2))
        .await
        .expect("uninterested source set result");
    assert!(
        tablet_result.contains("roster-uninterested-source")
            && tablet_result.contains("type='result'"),
        "uninterested source should receive only the IQ result first: {tablet_result}"
    );
    let tablet_after_set = tablet.recv_timeout(Duration::from_millis(250)).await;
    assert!(
        tablet_after_set.is_err(),
        "uninterested source must not receive its own roster push: {tablet_after_set:?}"
    );
    let desktop_carol_push = desktop
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster") && frame.contains("carol@localhost")
        })
        .await
        .expect("desktop carol roster push");
    let phone_carol_push = phone
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster") && frame.contains("carol@localhost")
        })
        .await
        .expect("phone carol roster push");
    assert!(
        desktop_carol_push.contains("type='set'") && phone_carol_push.contains("type='set'"),
        "interested siblings should receive uninterested-source pushes: {desktop_carol_push}; {phone_carol_push}"
    );
    assert_eq!(
        roster_version(&desktop_carol_push),
        roster_version(&phone_carol_push),
        "sibling fanout pushes should carry the same current version"
    );
    let phone_after_carol = send_roster_get(&mut phone, "phone-roster-after-carol").await;
    assert_eq!(
        roster_version(&phone_after_carol),
        roster_version(&phone_carol_push),
        "subsequent roster get should return the same version as the sibling push"
    );
    assert!(
        phone_after_carol.contains("carol@localhost"),
        "subsequent roster get should include the pushed item: {phone_after_carol}"
    );
    let current_version = roster_version(&phone_after_carol);
    let tablet_unchanged =
        send_roster_get_with_ver(&mut tablet, "tablet-roster-current-ver", &current_version).await;
    assert!(
        tablet_unchanged.contains("type='result'")
            && !tablet_unchanged.contains("jabber:iq:roster"),
        "matching-ver roster get should return empty result: {tablet_unchanged}"
    );
    desktop
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="roster-after-tablet-interest"><query xmlns="jabber:iq:roster"><item jid="dave@localhost" name="Dave"/></query></iq>"#,
        )
        .await
        .expect("send roster set after tablet interest");
    let _desktop_dave_result = desktop
        .recv_matching(|frame| frame.contains("roster-after-tablet-interest"))
        .await
        .expect("desktop dave set result");
    let tablet_dave_push = tablet
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster") && frame.contains("dave@localhost")
        })
        .await
        .expect("matching-version roster get should mark tablet interested");
    assert!(
        tablet_dave_push.contains("type='set'"),
        "tablet should receive later roster push after unchanged get: {tablet_dave_push}"
    );

    let _ = tablet.close().await;
    let _ = phone.close().await;
    let _ = desktop.close().await;
}

#[tokio::test]
async fn presence_subscription_state_is_reflected_in_roster_queries() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    let _alice_initial = send_roster_get(&mut alice, "alice-initial-roster").await;
    let _bob_initial = send_roster_get(&mut bob, "bob-initial-roster").await;
    alice
        .send(r#"<presence xmlns="jabber:client"><show>chat</show><status>ready to approve</status><priority>7</priority></presence>"#)
        .await
        .expect("alice available");
    bob.send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("bob available");

    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("send subscribe");
    let bob_outbound_push = bob
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("alice@localhost")
                && frame.contains("ask='subscribe'")
        })
        .await
        .expect("bob outbound subscribe roster push");
    assert!(
        bob_outbound_push.contains("subscription='none'"),
        "outbound subscribe starts as pending none: {bob_outbound_push}"
    );
    let _alice_subscribe = alice
        .recv_matching(|frame| {
            frame.contains("type='subscribe'") || frame.contains("type='subscribe'")
        })
        .await
        .expect("alice receives subscribe presence");
    let alice_pending_roster = send_roster_get(&mut alice, "alice-roster-before-approval").await;
    assert!(
        !alice_pending_roster.contains("jid='bob@localhost'"),
        "inbound subscribe must not create a recipient roster item before approval: {alice_pending_roster}"
    );

    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("send subscribed");
    let alice_push = alice
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("bob@localhost")
                && frame.contains("subscription='from'")
        })
        .await
        .expect("alice subscribed roster push");
    let bob_push = bob
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("alice@localhost")
                && frame.contains("subscription='to'")
        })
        .await
        .expect("bob subscribed roster push");
    assert!(
        !bob_push.contains("ask='subscribe'"),
        "subscribed should clear pending ask: {bob_push}"
    );
    let bob_subscribed = bob
        .recv_matching(|frame| {
            frame.contains("type='subscribed'") || frame.contains("type='subscribed'")
        })
        .await
        .expect("bob receives subscribed presence after roster push");
    assert!(
        bob_subscribed.contains("from='alice@localhost'")
            || bob_subscribed.contains("from='alice@localhost'"),
        "subscribed presence should follow roster push: {bob_subscribed}"
    );
    let bob_alice_available = bob
        .recv_matching(|frame| {
            (frame.contains("from='alice@localhost/") || frame.contains("from='alice@localhost/"))
                && (frame.contains("to='bob@localhost") || frame.contains("to='bob@localhost"))
                && frame.contains("<show>chat</show>")
                && frame.contains("ready to approve")
                && frame.contains("<priority>7</priority>")
        })
        .await
        .expect("bob receives alice current presence after roster push");
    assert!(
        !bob_alice_available.contains("type='unavailable'"),
        "approval should send current available presence: {bob_alice_available}"
    );

    let alice_roster = send_roster_get(&mut alice, "alice-roster-after-subscription").await;
    let bob_roster = send_roster_get(&mut bob, "bob-roster-after-subscription").await;
    assert_eq!(
        roster_version(&alice_roster),
        roster_version(&alice_push),
        "alice roster get after subscription should return pushed version"
    );
    assert_eq!(
        roster_version(&bob_roster),
        roster_version(&bob_push),
        "bob roster get after subscription should return pushed version"
    );
    assert!(
        alice_roster.contains("jid='bob@localhost'")
            && alice_roster.contains("subscription='from'"),
        "alice roster should reflect subscription state: {alice_roster}; push was {alice_push}"
    );
    assert!(
        bob_roster.contains("jid='alice@localhost'") && bob_roster.contains("subscription='to'"),
        "bob roster should reflect subscription state: {bob_roster}"
    );

    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("send duplicate subscribe");
    let duplicate_ack = bob
        .recv_matching(|frame| frame.contains("type='subscribed'"))
        .await
        .expect("duplicate subscribe is auto-acknowledged");
    assert!(
        duplicate_ack.contains("from='alice@localhost'"),
        "duplicate subscribe should be acknowledged by existing contact: {duplicate_ack}"
    );
    let bob_after_duplicate = send_roster_get(&mut bob, "bob-after-duplicate-subscribe").await;
    assert!(
        bob_after_duplicate.contains("jid='alice@localhost'")
            && bob_after_duplicate.contains("subscription='to'")
            && !bob_after_duplicate.contains("ask='subscribe'"),
        "duplicate subscribe must not recreate pending ask: {bob_after_duplicate}"
    );

    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribe" to="bob@localhost"/>"#)
        .await
        .expect("send reciprocal subscribe");
    let _alice_reciprocal_push = alice
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("bob@localhost")
                && frame.contains("ask='subscribe'")
        })
        .await
        .expect("alice reciprocal pending push");
    let _bob_reciprocal_subscribe = bob
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("bob receives reciprocal subscribe");
    bob.send(r#"<presence xmlns="jabber:client" type="subscribed" to="alice@localhost"/>"#)
        .await
        .expect("approve reciprocal subscribe");
    let alice_both_push = alice
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("bob@localhost")
                && frame.contains("subscription='both'")
        })
        .await
        .expect("alice both roster push");
    let bob_both_push = bob
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("alice@localhost")
                && frame.contains("subscription='both'")
        })
        .await
        .expect("bob both roster push");
    let _alice_reciprocal_subscribed = alice
        .recv_matching(|frame| frame.contains("type='subscribed'"))
        .await
        .expect("alice receives reciprocal subscribed after roster push");
    assert!(
        !alice_both_push.contains("ask='subscribe'") && !bob_both_push.contains("ask='subscribe'"),
        "reciprocal approval should clear pending asks: {alice_both_push}; {bob_both_push}"
    );
    let alice_after_both = send_roster_get(&mut alice, "alice-roster-after-both").await;
    let bob_after_both = send_roster_get(&mut bob, "bob-roster-after-both").await;
    assert!(
        alice_after_both.contains("jid='bob@localhost'")
            && alice_after_both.contains("subscription='both'"),
        "alice roster should reach both: {alice_after_both}"
    );
    assert!(
        bob_after_both.contains("jid='alice@localhost'")
            && bob_after_both.contains("subscription='both'"),
        "bob roster should reach both: {bob_after_both}"
    );

    bob.send(r#"<presence xmlns="jabber:client" type="unsubscribe" to="alice@localhost"/>"#)
        .await
        .expect("send unsubscribe");
    let bob_unsubscribe_push = bob
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("alice@localhost")
                && frame.contains("subscription='from'")
        })
        .await
        .expect("bob unsubscribe roster push");
    let alice_unsubscribe_push = alice
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("bob@localhost")
                && frame.contains("subscription='to'")
        })
        .await
        .expect("alice unsubscribe roster push");
    let alice_unsubscribe = alice
        .recv_matching(|frame| {
            frame.contains("type='unsubscribe'") || frame.contains("type='unsubscribe'")
        })
        .await
        .expect("alice receives unsubscribe presence after roster push");
    let bob_alice_unavailable = bob
        .recv_matching(|frame| {
            frame.contains("type='unavailable'") && frame.contains("from='alice@localhost/")
        })
        .await
        .expect("bob receives alice unavailable after unsubscribe");
    assert!(
        bob_alice_unavailable.contains("to='bob@localhost"),
        "unsubscribe should send unavailable presence from contact to user: {bob_alice_unavailable}"
    );
    assert!(
        roster_version(&bob_unsubscribe_push) != roster_version(&bob_push),
        "bob roster version should change after unsubscribe: {bob_unsubscribe_push}"
    );
    assert!(
        roster_version(&alice_unsubscribe_push) != roster_version(&alice_push),
        "alice roster version should change after unsubscribe: {alice_unsubscribe_push}"
    );
    let alice_after_unsubscribe =
        send_roster_get(&mut alice, "alice-roster-after-unsubscribe").await;
    let bob_after_unsubscribe = send_roster_get(&mut bob, "bob-roster-after-unsubscribe").await;
    assert_eq!(
        roster_version(&alice_after_unsubscribe),
        roster_version(&alice_unsubscribe_push),
        "alice roster get after unsubscribe should return pushed version"
    );
    assert_eq!(
        roster_version(&bob_after_unsubscribe),
        roster_version(&bob_unsubscribe_push),
        "bob roster get after unsubscribe should return pushed version"
    );
    assert!(
        alice_after_unsubscribe.contains("jid='bob@localhost'")
            && alice_after_unsubscribe.contains("subscription='to'"),
        "alice roster should reflect unsubscribe state after {alice_unsubscribe}: {alice_after_unsubscribe}"
    );
    assert!(
        bob_after_unsubscribe.contains("jid='alice@localhost'")
            && bob_after_unsubscribe.contains("subscription='from'"),
        "bob roster should reflect unsubscribe state: {bob_after_unsubscribe}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn subscription_denial_clears_pending_without_creating_recipient_item() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    let _alice_initial = send_roster_get(&mut alice, "alice-denial-initial").await;
    let _bob_initial = send_roster_get(&mut bob, "bob-denial-initial").await;
    alice
        .send(r#"<presence xmlns="jabber:client"><status>available for denial test</status></presence>"#)
        .await
        .expect("alice available");
    bob.send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("bob available");

    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("send subscribe");
    let bob_pending_push = bob
        .recv_matching(|frame| frame.contains("ask='subscribe'"))
        .await
        .expect("bob pending subscribe push");
    assert!(
        bob_pending_push.contains("subscription='none'"),
        "pending subscribe should be none+ask: {bob_pending_push}"
    );
    let _alice_subscribe = alice
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("alice receives subscribe");

    alice
        .send(r#"<presence xmlns="jabber:client" type="unsubscribed" to="bob@localhost"/>"#)
        .await
        .expect("send denial");
    let bob_denial_push = bob
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("alice@localhost")
                && frame.contains("subscription='none'")
        })
        .await
        .expect("bob denial roster push");
    assert!(
        !bob_denial_push.contains("ask='subscribe'"),
        "denial should clear pending ask: {bob_denial_push}"
    );
    let bob_denial = bob
        .recv_timeout(Duration::from_secs(2))
        .await
        .expect("bob receives denial after roster push");
    assert!(
        bob_denial.contains("type='unsubscribed'"),
        "pure denial must send roster push before unsubscribed: {bob_denial}"
    );
    let alice_roster = send_roster_get(&mut alice, "alice-after-denial").await;
    let bob_roster = send_roster_get(&mut bob, "bob-after-denial").await;
    assert!(
        !alice_roster.contains("jid='bob@localhost'"),
        "denial must not create recipient roster item: {alice_roster}"
    );
    assert!(
        bob_roster.contains("jid='alice@localhost'")
            && bob_roster.contains("subscription='none'")
            && !bob_roster.contains("ask='subscribe'"),
        "denial should leave requester item without pending ask: {bob_roster}"
    );
    let after_denial_frame = bob.recv_timeout(Duration::from_millis(250)).await;
    assert!(
        after_denial_frame
            .as_ref()
            .map(|frame| !frame.contains("type='unavailable'"))
            .unwrap_or(true),
        "pure denial must not send unavailable after the denial push: {after_denial_frame:?}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn unsolicited_subscription_responses_do_not_create_roster_items() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    let _alice_initial = send_roster_get(&mut alice, "alice-unsolicited-initial").await;
    let _bob_initial = send_roster_get(&mut bob, "bob-unsolicited-initial").await;

    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("send unsolicited subscribed");
    let bob_frame = bob.recv_timeout(Duration::from_millis(250)).await;
    assert!(
        bob_frame.is_err(),
        "pre-approval must not notify the contact until they subscribe: {bob_frame:?}"
    );
    let alice_roster = send_roster_get(&mut alice, "alice-after-preapproval").await;
    let bob_roster = send_roster_get(&mut bob, "bob-after-preapproval").await;
    assert!(
        alice_roster.contains("jid='bob@localhost'")
            && alice_roster.contains("subscription='none'")
            && alice_roster.contains("approved='true'"),
        "pre-approval must record sender roster item: {alice_roster}"
    );
    assert!(
        !bob_roster.contains("jid='alice@localhost'"),
        "pre-approval alone must not create recipient roster item: {bob_roster}"
    );
    while bob.recv_timeout(Duration::from_millis(50)).await.is_ok() {}
    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("send subscribe after preapproval");
    let auto_ack = bob
        .recv_matching(|frame| frame.contains("type='subscribed'"))
        .await
        .expect("pre-approved subscribe is auto-acknowledged");
    assert!(
        auto_ack.contains("from='alice@localhost'"),
        "pre-approved subscribe should be acknowledged by contact: {auto_ack}"
    );
    let bob_roster = send_roster_get(&mut bob, "bob-after-preapproved-subscribe").await;
    let alice_roster = send_roster_get(&mut alice, "alice-after-preapproved-subscribe").await;
    assert!(
        bob_roster.contains("jid='alice@localhost'")
            && bob_roster.contains("subscription='to'")
            && !bob_roster.contains("ask='subscribe'"),
        "pre-approved subscribe must complete without pending ask: {bob_roster}"
    );
    assert!(
        alice_roster.contains("jid='bob@localhost'")
            && alice_roster.contains("subscription='from'")
            && !alice_roster.contains("approved='true'"),
        "pre-approved subscribe must consume the pre-approval: {alice_roster}"
    );
    while alice.recv_timeout(Duration::from_millis(50)).await.is_ok() {}
    while bob.recv_timeout(Duration::from_millis(50)).await.is_ok() {}

    let (_server, mut alice, mut bob) = connect_alice_bob().await;
    let _alice_initial = send_roster_get(&mut alice, "alice-unsolicited-unsubscribe-initial").await;
    let _bob_initial = send_roster_get(&mut bob, "bob-unsolicited-unsubscribe-initial").await;

    bob.send(r#"<presence xmlns="jabber:client" type="unsubscribe" to="alice@localhost"/>"#)
        .await
        .expect("send unsolicited unsubscribe");
    let alice_frame = alice.recv_timeout(Duration::from_millis(250)).await;
    assert!(
        alice_frame.is_err(),
        "unsolicited unsubscribe must be silently ignored: {alice_frame:?}"
    );
    let alice_roster = send_roster_get(&mut alice, "alice-after-unsolicited-unsubscribe").await;
    let bob_roster = send_roster_get(&mut bob, "bob-after-unsolicited-unsubscribe").await;
    assert!(
        !alice_roster.contains("jid='bob@localhost'"),
        "unsolicited unsubscribe must not create recipient roster item: {alice_roster}"
    );
    assert!(
        !bob_roster.contains("jid='alice@localhost'"),
        "unsolicited unsubscribe must not create sender roster item: {bob_roster}"
    );

    let (_alice_add_push, _alice_add_result) = send_roster_set(
        &mut alice,
        "alice-add-bob-none",
        r#"<item jid="bob@localhost" name="Bob"/>"#,
    )
    .await;
    let (_bob_add_push, _bob_add_result) = send_roster_set(
        &mut bob,
        "bob-add-alice-none",
        r#"<item jid="alice@localhost" name="Alice"/>"#,
    )
    .await;
    alice
        .send(r#"<presence xmlns="jabber:client" type="unsubscribed" to="bob@localhost"/>"#)
        .await
        .expect("send unsolicited unsubscribed");
    let bob_frame = bob.recv_timeout(Duration::from_millis(250)).await;
    assert!(
        bob_frame.is_err(),
        "unsolicited unsubscribed without pending ask/subscription must be silently ignored: {bob_frame:?}"
    );
    let alice_roster = send_roster_get(&mut alice, "alice-after-unsolicited-unsubscribed").await;
    let bob_roster = send_roster_get(&mut bob, "bob-after-unsolicited-unsubscribed").await;
    assert!(
        alice_roster.contains("jid='bob@localhost'")
            && alice_roster.contains("subscription='none'"),
        "invalid unsubscribed must not mutate sender roster item: {alice_roster}"
    );
    assert!(
        bob_roster.contains("jid='alice@localhost'") && bob_roster.contains("subscription='none'"),
        "invalid unsubscribed must not mutate recipient roster item: {bob_roster}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn preapproval_preserves_existing_to_state_and_can_be_cancelled() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    let _alice_initial = send_roster_get(&mut alice, "alice-preapproval-to-initial").await;
    let _bob_initial = send_roster_get(&mut bob, "bob-preapproval-to-initial").await;

    bob.send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("bob is available for subscription request delivery");
    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribe" to="bob@localhost"/>"#)
        .await
        .expect("alice subscribes to bob");
    bob.recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("bob receives subscribe");
    bob.send(r#"<presence xmlns="jabber:client" type="subscribed" to="alice@localhost"/>"#)
        .await
        .expect("bob approves alice");
    let _alice_to_push = alice
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("bob@localhost")
                && frame.contains("subscription='to'")
        })
        .await
        .expect("alice receives approval roster push");
    alice
        .recv_matching(|frame| frame.contains("type='subscribed'"))
        .await
        .expect("alice receives subscribed after roster push");

    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("alice preapproves bob");
    let alice_roster = send_roster_get(&mut alice, "alice-preapproval-preserves-to").await;
    assert!(
        alice_roster.contains("jid='bob@localhost'")
            && alice_roster.contains("subscription='to'")
            && alice_roster.contains("approved='true'"),
        "pre-approval must preserve existing to subscription: {alice_roster}"
    );

    alice
        .send(r#"<presence xmlns="jabber:client" type="unsubscribed" to="bob@localhost"/>"#)
        .await
        .expect("alice cancels preapproval");
    let alice_roster = send_roster_get(&mut alice, "alice-preapproval-cancelled").await;
    assert!(
        alice_roster.contains("jid='bob@localhost'")
            && alice_roster.contains("subscription='to'")
            && !alice_roster.contains("approved='true'"),
        "unsubscribed must cancel pre-approval without changing to subscription: {alice_roster}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn offline_subscribe_is_removed_when_requester_unsubscribes_before_delivery() {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");
    let _bob_initial = send_roster_get(&mut bob, "bob-offline-subscribe-roster").await;

    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("send offline subscribe");
    let _bob_pending_push = bob
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("alice@localhost")
                && frame.contains("ask='subscribe'")
        })
        .await
        .expect("bob pending subscribe push");
    bob.send(r#"<presence xmlns="jabber:client" type="unsubscribe" to="alice@localhost"/>"#)
        .await
        .expect("cancel offline subscribe");
    let _bob_cancel_push = bob
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("alice@localhost")
                && frame.contains("subscription='none'")
                && !frame.contains("ask='subscribe'")
        })
        .await
        .expect("bob unsubscribe roster push");

    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice connection");
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available");
    let stale = alice.recv_timeout(Duration::from_millis(250)).await;
    assert!(
        stale.is_err(),
        "cancelled offline subscribe must not be delivered when Alice becomes available: {stale:?}"
    );
    let alice_roster = send_roster_get(&mut alice, "alice-after-cancelled-offline-subscribe").await;
    assert!(
        !alice_roster.contains("jid='bob@localhost'"),
        "cancelled offline subscribe must not create Alice roster item: {alice_roster}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn offline_subscribe_is_redelivered_until_answered() {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");
    let _bob_initial = send_roster_get(&mut bob, "bob-redelivery-roster").await;

    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("send offline subscribe");
    let _bob_pending_push = bob
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("alice@localhost")
                && frame.contains("ask='subscribe'")
        })
        .await
        .expect("bob pending subscribe push");

    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice connection");
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available");
    let first = alice
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("alice receives pending subscribe");
    assert!(
        first.contains("from='bob@localhost'"),
        "pending subscribe should identify requester: {first}"
    );

    alice
        .send(r#"<presence xmlns="jabber:client" type="unavailable"/>"#)
        .await
        .expect("alice unavailable");
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available again");
    let second = alice
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("unanswered pending subscribe is redelivered");
    assert!(
        second.contains("from='bob@localhost'"),
        "redelivered subscribe should preserve requester: {second}"
    );

    alice
        .send(r#"<presence xmlns="jabber:client" type="unsubscribed" to="bob@localhost"/>"#)
        .await
        .expect("deny subscription");
    let _bob_denial_push = bob
        .recv_matching(|frame| {
            frame.contains("alice@localhost") && !frame.contains("ask='subscribe'")
        })
        .await
        .expect("bob denial roster push");
    let _bob_denial = bob
        .recv_matching(|frame| frame.contains("type='unsubscribed'"))
        .await
        .expect("bob receives denial after roster push");
    alice
        .send(r#"<presence xmlns="jabber:client" type="unavailable"/>"#)
        .await
        .expect("alice unavailable after denial");
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available after denial");
    let after_denial = alice.recv_timeout(Duration::from_millis(250)).await;
    assert!(
        after_denial.is_err(),
        "answered pending subscribe must not be redelivered: {after_denial:?}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn live_subscribe_is_redelivered_until_answered() {
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
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available");

    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");
    let _bob_initial = send_roster_get(&mut bob, "bob-live-redelivery-roster").await;

    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("send live subscribe");
    let first = alice
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("alice receives live subscribe");
    assert!(
        first.contains("from='bob@localhost'"),
        "live subscribe should identify requester: {first}"
    );

    alice
        .send(r#"<presence xmlns="jabber:client" type="unavailable"/>"#)
        .await
        .expect("alice unavailable");
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available again");
    let second = alice
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("unanswered live subscribe is redelivered");
    assert!(
        second.contains("from='bob@localhost'"),
        "redelivered live subscribe should preserve requester: {second}"
    );

    alice
        .send(r#"<presence xmlns="jabber:client" type="unsubscribed" to="bob@localhost"/>"#)
        .await
        .expect("deny live subscription");
    let _bob_denial = bob
        .recv_matching(|frame| frame.contains("type='unsubscribed'"))
        .await
        .expect("bob receives live denial");

    let _ = alice.close().await;
    let _ = bob.close().await;
}
