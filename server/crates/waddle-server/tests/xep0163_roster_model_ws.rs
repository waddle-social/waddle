//! XEP-0163 / XEP-0060 `roster` access model over WebSocket.
//!
//! XEP-0060 §4.5.1: "roster: Access is limited to those on the roster
//! of the node owner ... in the groups specified by
//! pubsub#roster_groups_allowed." XEP-0060 §6.5.9 / XEP-0163 §4 refuse
//! with `<not-authorized/>` + `<not-in-roster-group/>`. The same decision
//! gates item retrieval, subscribe, live notification fan-out and the
//! last-item replay; XEP-0163 §4 additionally forbids notifying a
//! subscriber the user has blocked (XEP-0191), whatever the access model.

use waddle_ws_test_support as ws_common;

mod pep_caps_support;
use pep_caps_support::{go_available_with_caps, wait_for_event};

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NODE: &str = "http://jabber.org/protocol/mood";

async fn iq(client: &mut WsXmppClient, kind: &str, id: &str, to: &str, body: &str) -> String {
    let to_attr = if to.is_empty() {
        String::new()
    } else {
        format!(r#" to="{to}""#)
    };
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="{kind}" id="{id}"{to_attr}>{body}</iq>"#
        ))
        .await
        .expect("send iq");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("iq response")
}

struct World {
    _server: TestServer,
    alice: WsXmppClient,
    bob: WsXmppClient,
    carol: WsXmppClient,
}

const ALICE: &str = "alice@localhost";
const BOB: &str = "bob@localhost";
const CAROL: &str = "carol@localhost";

/// Alice grants bob and carol a presence subscription (roster
/// subscription `from`), files bob under "Friends" and carol under "Work",
/// publishes `item-1` to the mood node and restricts it to `roster` with
/// `roster_groups_allowed = [Friends]`. Nobody but alice is available.
async fn world(access: &str, groups: &[&str]) -> World {
    let pw = |n: &str| format!("{n}-{}", uuid::Uuid::new_v4());
    let (a, b, c) = (pw("alice"), pw("bob"), pw("carol"));
    let server =
        TestServer::start_with_extra_accounts(&[("alice", &a), ("bob", &b), ("carol", &c)]);
    let connect = |user: &'static str, pass: String, res: &'static str| {
        let url = server.ws_url();
        async move {
            WsXmppClient::connect_and_auth(&url, DOMAIN, user, &pass, res)
                .await
                .expect("connect")
        }
    };
    let mut alice = connect("alice", a, "alice-r1").await;
    let mut bob = connect("bob", b, "bob-r1").await;
    let mut carol = connect("carol", c, "carol-r1").await;

    for (client, id) in [
        (&mut alice, "ri-a"),
        (&mut bob, "ri-b"),
        (&mut carol, "ri-c"),
    ] {
        let r = iq(
            client,
            "get",
            id,
            "",
            r#"<query xmlns="jabber:iq:roster"/>"#,
        )
        .await;
        assert!(r.contains("type='result'"), "{r}");
    }
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice presence");
    for (contact, bare) in [(&mut bob, BOB), (&mut carol, CAROL)] {
        contact
            .send(&format!(
                r#"<presence xmlns="jabber:client" type="subscribe" to="{ALICE}"/>"#
            ))
            .await
            .expect("subscribe");
        alice
            .recv_matching(|f| f.contains("type='subscribe'") && f.contains(bare))
            .await
            .expect("alice receives subscribe");
        alice
            .send(&format!(
                r#"<presence xmlns="jabber:client" type="subscribed" to="{bare}"/>"#
            ))
            .await
            .expect("approve");
        contact
            .recv_matching(|f| f.contains("type='subscribed'"))
            .await
            .expect("contact receives approval");
    }
    file_contact(&mut alice, "grp-bob", BOB, "Friends").await;
    file_contact(&mut alice, "grp-carol", CAROL, "Work").await;

    let r = iq(
        &mut alice,
        "set",
        "pub-1",
        ALICE,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{NODE}"><item id="item-1"><mood xmlns="{NODE}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r.contains("type='result'"), "publish: {r}");
    configure(&mut alice, "cfg-1", access, Some(groups)).await;
    World {
        _server: server,
        alice,
        bob,
        carol,
    }
}

async fn file_contact(owner: &mut WsXmppClient, id: &str, contact: &str, group: &str) {
    let r = iq(
        owner,
        "set",
        id,
        "",
        &format!(
            r#"<query xmlns="jabber:iq:roster"><item jid="{contact}"><group>{group}</group></item></query>"#
        ),
    )
    .await;
    assert!(r.contains("type='result'"), "roster set: {r}");
}

async fn configure(
    owner: &mut WsXmppClient,
    id: &str,
    access: &str,
    groups: Option<&[&str]>,
) -> String {
    let groups_field = groups
        .map(|groups| {
            let values: String = groups
                .iter()
                .map(|g| format!("<value>{g}</value>"))
                .collect();
            format!(
                r#"<field var="pubsub#roster_groups_allowed" type="list-multi">{values}</field>"#
            )
        })
        .unwrap_or_default();
    iq(
        owner,
        "set",
        id,
        ALICE,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{NODE}"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/pubsub#node_config</value></field><field var="pubsub#access_model"><value>{access}</value></field>{groups_field}</x></configure></pubsub>"#
        ),
    )
    .await
}

async fn get_items(client: &mut WsXmppClient, id: &str) -> String {
    iq(
        client,
        "get",
        id,
        ALICE,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{NODE}"/></pubsub>"#),
    )
    .await
}

async fn subscribe(client: &mut WsXmppClient, id: &str, bare: &str) -> String {
    iq(
        client,
        "set",
        id,
        ALICE,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="{NODE}" jid="{bare}"/></pubsub>"#),
    )
    .await
}

async fn publish(client: &mut WsXmppClient, id: &str, item: &str) {
    let r = iq(
        client,
        "set",
        id,
        ALICE,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{NODE}"><item id="{item}"><mood xmlns="{NODE}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r.contains("type='result'"), "publish: {r}");
}

fn assert_not_in_roster_group(response: &str) {
    assert!(
        response.contains("type='error'")
            && response.contains("not-authorized")
            && response.contains("not-in-roster-group"),
        "XEP-0060 §6.5.9: not-authorized + not-in-roster-group expected: {response}"
    );
}

#[tokio::test]
async fn roster_model_item_retrieval_follows_allowed_groups() {
    let _serial = TEST_SERIAL.lock().await;
    let mut w = world("roster", &["Friends"]).await;

    let bob = get_items(&mut w.bob, "get-bob").await;
    assert!(
        bob.contains("type='result'") && bob.contains("id='item-1'"),
        "contact in an allowed group reads items: {bob}"
    );
    assert_not_in_roster_group(&get_items(&mut w.carol, "get-carol").await);
    let owner = get_items(&mut w.alice, "get-alice").await;
    assert!(owner.contains("id='item-1'"), "owner always reads: {owner}");

    // Moving carol into the allowed group admits her at once (XEP-0163 §4:
    // access controls are recalculated when a roster item changes).
    file_contact(&mut w.alice, "grp-carol-2", CAROL, "Friends").await;
    let carol = get_items(&mut w.carol, "get-carol-2").await;
    assert!(
        carol.contains("id='item-1'"),
        "carol now in Friends: {carol}"
    );

    // With no allowed group configured nobody but the owner qualifies.
    let r = configure(&mut w.alice, "cfg-empty", "roster", Some(&[])).await;
    assert!(r.contains("type='result'"), "{r}");
    assert_not_in_roster_group(&get_items(&mut w.bob, "get-bob-2").await);
    let owner = get_items(&mut w.alice, "get-alice-2").await;
    assert!(owner.contains("id='item-1'"), "{owner}");
}

#[tokio::test]
async fn roster_model_subscribe_follows_allowed_groups() {
    let _serial = TEST_SERIAL.lock().await;
    let mut w = world("roster", &["Friends"]).await;
    assert_not_in_roster_group(&subscribe(&mut w.carol, "sub-carol", CAROL).await);
    let bob = subscribe(&mut w.bob, "sub-bob", BOB).await;
    assert!(
        bob.contains("type='result'") && bob.contains("subscription='subscribed'"),
        "allowed-group contact subscribes: {bob}"
    );
}

#[tokio::test]
async fn roster_model_notifications_and_last_item_reach_only_allowed_groups() {
    let _serial = TEST_SERIAL.lock().await;
    let mut w = world("roster", &["Friends"]).await;
    let notify = format!("{NODE}+notify");

    // Last-item replay on becoming available (XEP-0163 §4).
    go_available_with_caps(&mut w.bob, "bob", &[NS_DISCO_INFO, &notify]).await;
    go_available_with_caps(&mut w.carol, "carol", &[NS_DISCO_INFO, &notify]).await;
    let replay = wait_for_event(&mut w.bob, Duration::from_secs(3))
        .await
        .expect("allowed-group contact receives the last item");
    assert!(replay.contains("id='item-1'"), "{replay}");
    assert!(
        wait_for_event(&mut w.carol, Duration::from_millis(800))
            .await
            .is_none(),
        "contact outside the allowed groups MUST NOT receive the replayed item"
    );

    // Live fan-out.
    publish(&mut w.alice, "pub-2", "item-2").await;
    let live = wait_for_event(&mut w.bob, Duration::from_secs(3))
        .await
        .expect("allowed-group contact is notified");
    assert!(live.contains("id='item-2'"), "{live}");
    assert!(
        wait_for_event(&mut w.carol, Duration::from_millis(800))
            .await
            .is_none(),
        "contact outside the allowed groups MUST NOT be notified"
    );

    // Recalculation: filing carol under Friends admits her to later events.
    file_contact(&mut w.alice, "grp-carol-3", CAROL, "Friends").await;
    publish(&mut w.alice, "pub-3", "item-3").await;
    let live = wait_for_event(&mut w.carol, Duration::from_secs(3))
        .await
        .expect("carol is notified once in an allowed group");
    assert!(live.contains("id='item-3'"), "{live}");
}

#[tokio::test]
async fn roster_model_without_allowed_groups_notifies_nobody_but_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let mut w = world("roster", &[]).await;
    let notify = format!("{NODE}+notify");
    go_available_with_caps(&mut w.bob, "bob", &[NS_DISCO_INFO, &notify]).await;
    assert!(
        wait_for_event(&mut w.bob, Duration::from_millis(800))
            .await
            .is_none(),
        "empty roster_groups_allowed MUST NOT replay to contacts"
    );
    publish(&mut w.alice, "pub-2", "item-2").await;
    assert!(
        wait_for_event(&mut w.bob, Duration::from_millis(800))
            .await
            .is_none(),
        "empty roster_groups_allowed MUST NOT notify contacts"
    );
}

#[tokio::test]
async fn roster_groups_allowed_is_exposed_in_the_configuration_form() {
    let _serial = TEST_SERIAL.lock().await;
    let mut w = world("roster", &["Friends", "Family"]).await;
    let form = iq(
        &mut w.alice,
        "get",
        "cfg-get",
        ALICE,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{NODE}"/></pubsub>"#),
    )
    .await;
    assert!(
        form.contains("pubsub#roster_groups_allowed")
            && form.contains("list-multi")
            && form.contains(">Friends</value>")
            && form.contains(">Family</value>"),
        "XEP-0060 §8.2.1 form lists the stored groups: {form}"
    );
    assert!(form.contains(">roster</value>"), "{form}");
}

#[tokio::test]
async fn pep_node_cannot_be_configured_with_unsupported_authorize_model() {
    let _serial = TEST_SERIAL.lock().await;
    let mut w = world("roster", &["Friends"]).await;
    let r = configure(&mut w.alice, "cfg-auth", "authorize", None).await;
    assert!(
        r.contains("type='error'")
            && r.contains("not-acceptable")
            && r.contains("unsupported-access-model"),
        "XEP-0060 §8.1.3: not-acceptable + unsupported-access-model: {r}"
    );
}

#[tokio::test]
async fn explicit_subscribers_honour_xep0191_blocking_on_open_nodes() {
    let _serial = TEST_SERIAL.lock().await;
    let mut w = world("open", &[]).await;

    for (client, id, bare) in [
        (&mut w.bob, "sub-bob", BOB),
        (&mut w.carol, "sub-carol", CAROL),
    ] {
        let r = subscribe(client, id, bare).await;
        assert!(r.contains("type='result'"), "open node subscribe: {r}");
    }
    publish(&mut w.alice, "pub-2", "item-2").await;
    for client in [&mut w.bob, &mut w.carol] {
        assert!(
            wait_for_event(client, Duration::from_secs(3))
                .await
                .is_some(),
            "explicit subscribers are notified before any block"
        );
    }

    // The owner blocks bob; carol blocks the owner (both directions).
    let r = iq(
        &mut w.alice,
        "set",
        "blk-bob",
        "",
        &format!(r#"<block xmlns="urn:xmpp:blocking"><item jid="{BOB}"/></block>"#),
    )
    .await;
    assert!(r.contains("type='result'"), "{r}");
    let r = iq(
        &mut w.carol,
        "set",
        "blk-alice",
        "",
        &format!(r#"<block xmlns="urn:xmpp:blocking"><item jid="{ALICE}"/></block>"#),
    )
    .await;
    assert!(r.contains("type='result'"), "{r}");

    publish(&mut w.alice, "pub-3", "item-3").await;
    for (client, who) in [
        (&mut w.bob, "blocked by the owner"),
        (&mut w.carol, "blocking the owner"),
    ] {
        assert!(
            wait_for_event(client, Duration::from_millis(800))
                .await
                .is_none(),
            "XEP-0163 §4: subscriber {who} MUST NOT be notified"
        );
    }

    // A blocked entity cannot subscribe afresh either.
    let r = subscribe(&mut w.bob, "sub-bob-2", BOB).await;
    assert!(r.contains("type='error'"), "blocked subscribe refused: {r}");
}

/// XEP-0163 §4 (xep-0163.xml:465) requires the open, roster and whitelist
/// access models; the account advertises each one it supports.
#[tokio::test]
async fn pep_account_advertises_supported_access_models() {
    let _serial = TEST_SERIAL.lock().await;
    let mut w = world("roster", &["Friends"]).await;
    let r = iq(
        &mut w.alice,
        "get",
        "disco-access",
        ALICE,
        &format!(r#"<query xmlns="{NS_DISCO_INFO}"/>"#),
    )
    .await;
    for feature in [
        "http://jabber.org/protocol/pubsub#access-open",
        "http://jabber.org/protocol/pubsub#access-roster",
        "http://jabber.org/protocol/pubsub#access-whitelist",
    ] {
        assert!(
            r.contains(&format!("var='{feature}'")),
            "account disco#info must advertise {feature}: {r}"
        );
    }
    assert!(
        !r.contains("pubsub#access-authorize"),
        "authorize is not supported and must not be advertised: {r}"
    );
}
