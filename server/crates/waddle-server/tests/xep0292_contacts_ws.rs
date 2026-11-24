//! XEP-0292 private contacts node (`urn:xmpp:contacts`) wire tests.
//!
//! xep-0292.xml:390: the node "MUST have an access type of \"whitelist\"
//! by default". The node holds one item per contact (ItemID = contact
//! bare JID), so a publish without `<publish-options>` must neither
//! evict earlier contacts nor leak them to roster `+notify` contacts.

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use waddle_xmpp::xep::xep0292::PEP_NODE_CONTACTS;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const NS_VCARD4: &str = "urn:ietf:params:xml:ns:vcard-4.0";
const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";

async fn iq(client: &mut WsXmppClient, kind: &str, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="{kind}" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("iq response")
}

fn publish_contact_body(contact: &str, name: &str) -> String {
    format!(
        r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{PEP_NODE_CONTACTS}"><item id="{contact}"><vcard xmlns="{NS_VCARD4}"><fn><text>{name}</text></fn></vcard></item></publish></pubsub>"#
    )
}

fn items_body() -> String {
    format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{PEP_NODE_CONTACTS}"/></pubsub>"#)
}

async fn ping_anchor(client: &mut WsXmppClient, id: &str) {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><ping xmlns="urn:xmpp:ping"/></iq>"#
        ))
        .await
        .expect("send ping");
    let _ = client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("ping result");
}

#[tokio::test]
async fn xep0292_contacts_node_defaults_to_whitelist_and_keeps_multiple_items() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let mut admin =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "admin", &password, "c-admin")
            .await
            .expect("admin connect");
    let admin_bare = format!("admin@{DOMAIN}");

    for (id, jid, name) in [
        ("c-pub-1", "romeo@montague.example", "Romeo"),
        ("c-pub-2", "juliet@capulet.example", "Juliet"),
    ] {
        let resp = iq(
            &mut admin,
            "set",
            id,
            &admin_bare,
            &publish_contact_body(jid, name),
        )
        .await;
        assert!(resp.contains(r#"type='result'"#), "publish {jid}: {resp}");
    }

    let cfg = iq(
        &mut admin,
        "get",
        "c-cfg",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{PEP_NODE_CONTACTS}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        cfg.contains("<value>whitelist</value>"),
        "urn:xmpp:contacts MUST default to whitelist: {cfg}"
    );

    let items = iq(&mut admin, "get", "c-items", &admin_bare, &items_body()).await;
    assert!(items.contains("Romeo"), "first contact retained: {items}");
    assert!(items.contains("Juliet"), "second contact retained: {items}");
    assert_eq!(items.match_indices("<item ").count(), 2, "{items}");

    let _ = admin.close().await;
}

#[tokio::test]
async fn xep0292_contacts_node_hidden_from_roster_contact_with_notify() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "ct-alice",
    )
    .await
    .expect("alice connect");
    let mut bob =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "bob", &bob_password, "ct-bob")
            .await
            .expect("bob connect");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let bob_full = bob.full_jid.clone().expect("bob full jid");

    for (c, id) in [(&mut alice, "ct-roster-a"), (&mut bob, "ct-roster-b")] {
        c.send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><query xmlns="jabber:iq:roster"/></iq>"#
        ))
        .await
        .expect("roster get");
        let _ = c
            .recv_matching(|f| f.contains(id))
            .await
            .expect("roster result");
    }
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice presence");
    bob.send(&format!(
        r#"<presence xmlns="jabber:client" type="subscribe" to="{alice_bare}"/>"#
    ))
    .await
    .expect("bob subscribes");
    let _ = alice
        .recv_matching(|f| f.contains(r#"type='subscribe'"#))
        .await
        .expect("alice receives subscribe");
    alice
        .send(&format!(
            r#"<presence xmlns="jabber:client" type="subscribed" to="{bob_bare}"/>"#
        ))
        .await
        .expect("alice approves");
    let _ = bob
        .recv_matching(|f| f.contains(r#"type='subscribed'"#))
        .await
        .expect("bob receives approval");

    // bob advertises urn:xmpp:contacts+notify via XEP-0115 caps.
    use waddle_xmpp::disco::info::{Feature, Identity};
    let notify_var = format!("{PEP_NODE_CONTACTS}+notify");
    let features = ["http://jabber.org/protocol/disco#info", notify_var.as_str()];
    let ver = waddle_xmpp::xep::xep0115::compute_caps_hash(
        &[Identity::new("client", "pc", Some("Bob's Client"))],
        &features.iter().map(|f| Feature::new(f)).collect::<Vec<_>>(),
    );
    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="https://bob.example/caps" ver="{ver}"/></presence>"#
    ))
    .await
    .expect("bob caps presence");
    let disco_query = bob
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server resolves bob's caps");
    let iq_id = extract_attr_after(&disco_query, "<iq", "id").expect("iq id");
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="https://bob.example/caps#{ver}"><identity category="client" type="pc" name="Bob's Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob disco reply");
    ping_anchor(&mut bob, "ct-anchor-1").await;

    let resp = iq(
        &mut alice,
        "set",
        "ct-pub",
        &alice_bare,
        &publish_contact_body("romeo@montague.example", "Secret Romeo"),
    )
    .await;
    assert!(resp.contains(r#"type='result'"#), "publish: {resp}");

    // Bob must not get a notification for the whitelist node.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match bob.recv_timeout(remaining).await {
            Ok(frame) => assert!(
                !(frame.contains(NS_PUBSUB_EVENT) && frame.contains(PEP_NODE_CONTACTS))
                    && !frame.contains("Secret Romeo"),
                "roster contact with +notify MUST NOT receive whitelist contacts: {frame}"
            ),
            Err(_) => break,
        }
    }

    // Nor may he fetch items.
    let fetch = iq(&mut bob, "get", "ct-fetch", &alice_bare, &items_body()).await;
    assert!(
        fetch.contains(r#"type='error'"#) && !fetch.contains("Secret Romeo"),
        "roster contact MUST NOT read whitelist contacts node: {fetch}"
    );

    // The owner can.
    let own = iq(&mut alice, "get", "ct-own", &alice_bare, &items_body()).await;
    assert!(own.contains("Secret Romeo"), "owner fetch: {own}");

    let _ = bob.close().await;
    let _ = alice.close().await;
}
