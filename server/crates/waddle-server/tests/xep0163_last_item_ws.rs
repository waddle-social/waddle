//! XEP-0163 "Sending the Last Published Item" over WebSocket.
//!
//! A PEP service MUST send the last published item to newly-available
//! resources of each subscriber, including the account owner, when the
//! resource's XEP-0115 caps show interest (`<node>+notify`) and the node's
//! `pubsub#send_last_published_item` is `on_sub_and_presence`.

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NODE: &str = "http://jabber.org/protocol/mood";

fn caps_ver(name: &str, features: &[&str]) -> String {
    use waddle_xmpp::disco::info::{Feature, Identity};
    let identities = vec![Identity::new("client", "pc", Some(name))];
    let features: Vec<Feature> = features.iter().map(|f| Feature::new(f)).collect();
    waddle_xmpp::xep::xep0115::compute_caps_hash(&identities, &features)
}

async fn iq_set_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq set");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("iq set response")
}

async fn wait_for_event(client: &mut WsXmppClient, dur: Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match client.recv_timeout(remaining).await {
            Ok(frame) => {
                if frame.contains("<message")
                    && frame.contains(NS_PUBSUB_EVENT)
                    && frame.contains(&format!(r#"node='{NODE}'"#))
                {
                    return Some(frame);
                }
            }
            Err(_) => return None,
        }
    }
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

/// Go available with caps advertising `features`, answering the server's
/// disco#info query. The last-item replay may arrive at any point after.
async fn go_available_with_caps(client: &mut WsXmppClient, name: &str, features: &[&str]) {
    let full = client.full_jid.clone().expect("full jid");
    let caps_node = format!("https://{name}.example/caps");
    let ver = caps_ver(name, features);
    client
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("presence with caps");
    let query = client
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries caps");
    let iq_id = ws_common::extract_attr_after(&query, "<iq", "id").expect("iq id");
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="{name}"/>{feature_xml}</query></iq>"#
        ))
        .await
        .expect("disco reply");
}

struct Pair {
    server: TestServer,
    alice_password: String,
    _alice: WsXmppClient,
    bob: WsXmppClient,
    alice_bare: String,
}

/// Alice (available) has published `item-1` to the mood node; bob holds a
/// `from` subscription to her presence but is not yet available.
async fn setup(send_last: Option<&str>) -> Pair {
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
        "alice-r1",
    )
    .await
    .expect("alice connect");
    let mut bob =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "bob", &bob_password, "bob-r1")
            .await
            .expect("bob connect");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");

    for (client, id) in [(&mut alice, "roster-init-a"), (&mut bob, "roster-init-b")] {
        client
            .send(&format!(
                r#"<iq xmlns="jabber:client" type="get" id="{id}"><query xmlns="jabber:iq:roster"/></iq>"#
            ))
            .await
            .expect("roster get");
        client
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
    alice
        .recv_matching(|f| f.contains(r#"type='subscribe'"#))
        .await
        .expect("alice receives subscribe");
    alice
        .send(&format!(
            r#"<presence xmlns="jabber:client" type="subscribed" to="{bob_bare}"/>"#
        ))
        .await
        .expect("alice approves");
    bob.recv_matching(|f| f.contains(r#"type='subscribed'"#))
        .await
        .expect("bob receives approval");

    let resp = iq_set_to(
        &mut alice,
        "pub-1",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{NODE}"><item id="item-1"><mood xmlns="{NODE}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(resp.contains(r#"type='result'"#), "publish: {resp}");

    if let Some(value) = send_last {
        let resp = iq_set_to(
            &mut alice,
            "cfg-1",
            &alice_bare,
            &format!(
                r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{NODE}"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/pubsub#node_config</value></field><field var="pubsub#send_last_published_item"><value>{value}</value></field></x></configure></pubsub>"#
            ),
        )
        .await;
        assert!(resp.contains(r#"type='result'"#), "configure: {resp}");
    }
    Pair {
        server,
        alice_password,
        _alice: alice,
        bob,
        alice_bare,
    }
}

fn notify_features() -> [String; 2] {
    [NS_DISCO_INFO.to_string(), format!("{NODE}+notify")]
}

#[tokio::test]
async fn contact_coming_online_with_notify_receives_last_item() {
    let _serial = TEST_SERIAL.lock().await;
    let mut p = setup(None).await;
    let f = notify_features();
    go_available_with_caps(&mut p.bob, "bob", &[&f[0], &f[1]]).await;
    let event = wait_for_event(&mut p.bob, Duration::from_secs(3))
        .await
        .expect("XEP-0163: last published item MUST be sent to a newly-available +notify resource");
    assert!(
        event.contains(&format!(r#"from='{}'"#, p.alice_bare)),
        "{event}"
    );
    assert!(event.contains(r#"id='item-1'"#), "{event}");
    assert!(event.contains("urn:xmpp:delay"), "delay stamp: {event}");
    // A later presence update is not initial presence: no second replay.
    p.bob
        .send(r#"<presence xmlns="jabber:client"><show>away</show></presence>"#)
        .await
        .expect("presence update");
    ping_anchor(&mut p.bob, "anchor-1").await;
    assert!(
        wait_for_event(&mut p.bob, Duration::from_millis(300))
            .await
            .is_none(),
        "subsequent presence must not replay"
    );
}

#[tokio::test]
async fn contact_without_notify_receives_no_last_item() {
    let _serial = TEST_SERIAL.lock().await;
    let mut p = setup(None).await;
    go_available_with_caps(&mut p.bob, "bob", &[NS_DISCO_INFO]).await;
    ping_anchor(&mut p.bob, "anchor-1").await;
    assert!(
        wait_for_event(&mut p.bob, Duration::from_millis(500))
            .await
            .is_none(),
        "resource without +notify MUST NOT receive the last item"
    );
}

#[tokio::test]
async fn node_with_send_last_published_item_never_is_not_replayed() {
    let _serial = TEST_SERIAL.lock().await;
    let mut p = setup(Some("never")).await;
    let f = notify_features();
    go_available_with_caps(&mut p.bob, "bob", &[&f[0], &f[1]]).await;
    ping_anchor(&mut p.bob, "anchor-1").await;
    assert!(
        wait_for_event(&mut p.bob, Duration::from_millis(500))
            .await
            .is_none(),
        "send_last_published_item=never MUST NOT replay"
    );
}

#[tokio::test]
async fn owner_second_resource_receives_own_last_item() {
    let _serial = TEST_SERIAL.lock().await;
    let p = setup(None).await;
    let mut alice_b = WsXmppClient::connect_and_auth(
        &p.server.ws_url(),
        DOMAIN,
        "alice",
        &p.alice_password,
        "alice-r2",
    )
    .await
    .expect("alice second resource");
    let f = notify_features();
    go_available_with_caps(&mut alice_b, "alice-r2", &[&f[0], &f[1]]).await;
    let event = wait_for_event(&mut alice_b, Duration::from_secs(3))
        .await
        .expect("account owner's newly-available resource MUST receive the last item");
    assert!(event.contains(r#"id='item-1'"#), "{event}");
}
