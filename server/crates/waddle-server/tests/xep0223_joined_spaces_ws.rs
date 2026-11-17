//! Integration tests for the private `urn:waddle:joined-spaces:0` PEP
//! node (XEP-0223 Persistent Storage of Private Data via PubSub) over
//! WebSocket C2S.
//!
//! The node holds one item per Space on another server that the account
//! shows. The server pins it owner-only whatever the client asks for:
//!
//! - several Spaces are kept side by side (no single-item eviction);
//! - publish-options asking for `open` access do not open it;
//! - an owner configure asking for `open` access does not open it;
//! - only the owner publishes to it, and every item names its Space;
//! - a retract removes one Space and leaves the others.

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const NODE: &str = "urn:waddle:joined-spaces:0";
const NS: &str = "urn:waddle:joined-spaces:0";
const DEV_ITEM: &str = "xmpp:spaces.remote.example?;node=dev";
const OPS_ITEM: &str = "xmpp:spaces.remote.example?;node=ops";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect_two_accounts() -> (TestServer, WsXmppClient, WsXmppClient) {
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
    .expect("alice connect");
    let bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connect");
    (server, alice, bob)
}

/// Publish-options that ask for the weakest privacy a client could
/// request. The server must ignore the access model it names.
fn open_publish_options() -> &'static str {
    r#"<publish-options>
      <x xmlns="jabber:x:data" type="submit">
        <field var="FORM_TYPE" type="hidden">
          <value>http://jabber.org/protocol/pubsub#publish-options</value>
        </field>
        <field var="pubsub#persist_items"><value>true</value></field>
        <field var="pubsub#access_model"><value>open</value></field>
      </x>
    </publish-options>"#
}

fn space_payload(node: &str) -> String {
    format!(r#"<space xmlns="{NS}" service="spaces.remote.example" node="{node}" follow="true"/>"#)
}

fn publish_iq(id: &str, to: Option<&str>, item_id: Option<&str>, payload: &str) -> String {
    let to_attr = to.map(|to| format!(r#" to="{to}""#)).unwrap_or_default();
    let item_open = item_id
        .map(|item_id| format!(r#"<item id="{item_id}">"#))
        .unwrap_or_else(|| "<item>".to_string());
    let options = open_publish_options();
    format!(
        r#"<iq type="set" id="{id}"{to_attr}>
          <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="{NODE}">{item_open}{payload}</item></publish>
            {options}
          </pubsub>
        </iq>"#
    )
}

async fn send_and_wait(client: &mut WsXmppClient, xml: &str, id: &str) -> String {
    client.send(xml).await.expect("send");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("response")
}

async fn publish_space(client: &mut WsXmppClient, id: &str, item_id: &str, node: &str) {
    let result = send_and_wait(
        client,
        &publish_iq(id, None, Some(item_id), &space_payload(node)),
        id,
    )
    .await;
    assert!(
        result.contains(r#"type='result'"#),
        "publish of {item_id} failed: {result}"
    );
}

async fn owner_items(client: &mut WsXmppClient, id: &str) -> String {
    send_and_wait(
        client,
        &format!(
            r#"<iq type="get" id="{id}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{NODE}"/>
              </pubsub>
            </iq>"#
        ),
        id,
    )
    .await
}

async fn assert_bob_cannot_read(bob: &mut WsXmppClient, id: &str) {
    let result = send_and_wait(
        bob,
        &format!(
            r#"<iq type="get" id="{id}" to="alice@{DOMAIN}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{NODE}"/>
              </pubsub>
            </iq>"#
        ),
        id,
    )
    .await;
    assert!(
        result.contains(r#"type='error'"#),
        "a non-owner read of the joined-spaces node must fail: {result}"
    );
    assert!(
        result.contains("forbidden") || result.contains("item-not-found"),
        "expected forbidden or item-not-found, got: {result}"
    );
    assert!(
        !result.contains("node=dev") && !result.contains("node=ops"),
        "a refused read must not disclose the list: {result}"
    );
}

#[tokio::test]
async fn several_spaces_are_kept_side_by_side() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, _bob) = connect_two_accounts().await;

    publish_space(&mut alice, "pub-dev", DEV_ITEM, "dev").await;
    publish_space(&mut alice, "pub-ops", OPS_ITEM, "ops").await;

    let items = owner_items(&mut alice, "items-both").await;
    assert!(
        items.contains(r#"node="dev""#) || items.contains("node='dev'"),
        "dev missing: {items}"
    );
    assert!(
        items.contains(r#"node="ops""#) || items.contains("node='ops'"),
        "ops missing: {items}"
    );

    let _ = alice.close().await;
}

#[tokio::test]
async fn open_publish_options_do_not_open_the_node() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    publish_space(&mut alice, "pub-open", DEV_ITEM, "dev").await;
    assert_bob_cannot_read(&mut bob, "bob-read").await;

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn owner_configure_cannot_open_the_node() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    publish_space(&mut alice, "pub-before-config", DEV_ITEM, "dev").await;

    let configure = send_and_wait(
        &mut alice,
        &format!(
            r#"<iq type="set" id="cfg-open">
              <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
                <configure node="{NODE}">
                  <x xmlns="jabber:x:data" type="submit">
                    <field var="FORM_TYPE" type="hidden">
                      <value>http://jabber.org/protocol/pubsub#node_config</value>
                    </field>
                    <field var="pubsub#access_model"><value>open</value></field>
                    <field var="pubsub#max_items"><value>1</value></field>
                  </x>
                </configure>
              </pubsub>
            </iq>"#
        ),
        "cfg-open",
    )
    .await;
    // Whether the server accepts the form or refuses it, the node stays
    // owner-only and multi-item.
    let _ = configure;

    let read_back = send_and_wait(
        &mut alice,
        &format!(
            r#"<iq type="get" id="cfg-read">
              <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
                <configure node="{NODE}"/>
              </pubsub>
            </iq>"#
        ),
        "cfg-read",
    )
    .await;
    assert!(
        read_back.contains("whitelist"),
        "the joined-spaces node must stay whitelist: {read_back}"
    );

    publish_space(&mut alice, "pub-after-config", OPS_ITEM, "ops").await;
    let items = owner_items(&mut alice, "items-after-config").await;
    assert!(
        (items.contains(r#"node="dev""#) || items.contains("node='dev'"))
            && (items.contains(r#"node="ops""#) || items.contains("node='ops'")),
        "a configure must not reduce the node to one item: {items}"
    );

    assert_bob_cannot_read(&mut bob, "bob-read-after-config").await;

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn only_the_owner_publishes_and_every_item_names_its_space() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    publish_space(&mut alice, "pub-owner", DEV_ITEM, "dev").await;

    let foreign = send_and_wait(
        &mut bob,
        &publish_iq(
            "bob-pub",
            Some(&format!("alice@{DOMAIN}")),
            Some(OPS_ITEM),
            &space_payload("ops"),
        ),
        "bob-pub",
    )
    .await;
    assert!(
        foreign.contains(r#"type='error'"#),
        "a non-owner publish to the joined-spaces node must fail: {foreign}"
    );

    let anonymous_item = send_and_wait(
        &mut alice,
        &publish_iq("pub-no-id", None, None, &space_payload("ops")),
        "pub-no-id",
    )
    .await;
    assert!(
        anonymous_item.contains(r#"type='error'"#) && anonymous_item.contains("bad-request"),
        "a joined-spaces item without an id must be refused: {anonymous_item}"
    );

    let items = owner_items(&mut alice, "items-owner").await;
    assert!(
        !items.contains("node=\"ops\"") && !items.contains("node='ops'"),
        "no refused publish may be stored: {items}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn retract_removes_one_space() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, _bob) = connect_two_accounts().await;

    publish_space(&mut alice, "pub-dev-r", DEV_ITEM, "dev").await;
    publish_space(&mut alice, "pub-ops-r", OPS_ITEM, "ops").await;

    let retract = send_and_wait(
        &mut alice,
        &format!(
            r#"<iq type="set" id="retract-dev">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <retract node="{NODE}" notify="true"><item id="{DEV_ITEM}"/></retract>
              </pubsub>
            </iq>"#
        ),
        "retract-dev",
    )
    .await;
    assert!(
        retract.contains(r#"type='result'"#),
        "retract failed: {retract}"
    );

    // The retraction notice to the owner's own resource may arrive
    // first; drain briefly before reading the list again.
    for _ in 0..2 {
        if alice
            .recv_timeout(Duration::from_millis(200))
            .await
            .is_err()
        {
            break;
        }
    }

    let items = owner_items(&mut alice, "items-after-retract").await;
    assert!(
        !items.contains("node=\"dev\"") && !items.contains("node='dev'"),
        "the retracted Space must be gone: {items}"
    );
    assert!(
        items.contains("node=\"ops\"") || items.contains("node='ops'"),
        "the other Space must stay: {items}"
    );

    let _ = alice.close().await;
}
