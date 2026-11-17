//! Integration tests for the private `urn:waddle:joined-spaces:0` PEP
//! node (XEP-0223 Persistent Storage of Private Data via PubSub) over
//! WebSocket C2S.
//!
//! The node holds one item per Space on another server that the account
//! shows. The server pins it owner-only whatever the client asks for:
//!
//! - several Spaces are kept side by side (no single-item eviction);
//! - the node it auto-creates is whitelist, bounded at
//!   `PEP_BOOKMARK_MAX_ITEMS` items and never sends its last item;
//! - publish-options asking for `open` access do not open it, even to a
//!   contact subscribed to the owner's presence;
//! - a roster contact advertising `+notify` for it receives no event;
//! - an owner configure asking for `open` access does not open it, and a
//!   `max_items` the owner chose within the bound survives a publish;
//! - no affiliation shares it: granting `member` or `publisher` is
//!   refused (XEP-0060 §8.9.2.4);
//! - only the owner publishes to it, and every item names its Space;
//! - a retract removes one Space and leaves the others.
//!
//! The server under test is a separate `waddle-server` process with
//! in-memory SQLite stores; no PostgreSQL or other service is needed.

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use waddle_xmpp::pubsub::PEP_BOOKMARK_MAX_ITEMS;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const NODE: &str = "urn:waddle:joined-spaces:0";
const NS: &str = "urn:waddle:joined-spaces:0";
const DEV_ITEM: &str = "xmpp:spaces.remote.example?;node=dev";
const OPS_ITEM: &str = "xmpp:spaces.remote.example?;node=ops";
const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
/// An ordinary PEP node of the same account, used as a control: with
/// its default access model it reaches roster contacts advertising
/// `+notify`, and once its owner opens it any account may read it.
const CONTROL_NODE: &str = "http://jabber.org/protocol/mood";

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

/// Compute the XEP-0115 ver hash for the advertised feature set, as
/// `xep0163_pep_ws.rs` does.
fn caps_verification_string(
    identity_category: &str,
    identity_type: &str,
    identity_name: &str,
    features: &[&str],
) -> String {
    use waddle_xmpp::disco::info::{Feature, Identity};
    use waddle_xmpp::xep::xep0115::compute_caps_hash;
    let identities = vec![Identity::new(
        identity_category,
        identity_type,
        Some(identity_name),
    )];
    let features: Vec<Feature> = features.iter().map(|f| Feature::new(f)).collect();
    compute_caps_hash(&identities, &features)
}

/// Send a ping IQ and wait for its result: every frame the server sent
/// this connection before the result has been received by then.
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

/// Subscribe bob to alice's presence, approved by alice, so alice's
/// roster holds bob with `subscription = from`: the contact XEP-0163 §3
/// notifies and the relationship XEP-0060 §4.5 `presence` and `roster`
/// access models admit.
async fn establish_bob_subscribes_to_alice(alice: &mut WsXmppClient, bob: &mut WsXmppClient) {
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    alice
        .send(r#"<iq xmlns="jabber:client" type="get" id="roster-init-a"><query xmlns="jabber:iq:roster"/></iq>"#)
        .await
        .expect("alice roster get");
    let _ = alice
        .recv_matching(|f| f.contains("roster-init-a"))
        .await
        .expect("alice roster result");
    bob.send(r#"<iq xmlns="jabber:client" type="get" id="roster-init-b"><query xmlns="jabber:iq:roster"/></iq>"#)
        .await
        .expect("bob roster get");
    let _ = bob
        .recv_matching(|f| f.contains("roster-init-b"))
        .await
        .expect("bob roster result");

    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice presence");
    bob.send(&format!(
        r#"<presence xmlns="jabber:client" type="subscribe" to="{alice_bare}"/>"#
    ))
    .await
    .expect("bob subscribes");
    let _subscribe = alice
        .recv_matching(|f| f.contains(r#"type='subscribe'"#))
        .await
        .expect("alice receives subscribe");
    alice
        .send(&format!(
            r#"<presence xmlns="jabber:client" type="subscribed" to="{bob_bare}"/>"#
        ))
        .await
        .expect("alice approves");
    let _subscribed = bob
        .recv_matching(|f| f.contains(r#"type='subscribed'"#))
        .await
        .expect("bob receives approval");
}

/// Send bob's presence with XEP-0115 caps advertising `+notify` for each
/// of `nodes`, answer the server's disco#info for them, and wait until
/// the server has processed the answer.
async fn advertise_notify_caps(bob: &mut WsXmppClient, nodes: &[&str]) {
    let notify_vars: Vec<String> = nodes.iter().map(|node| format!("{node}+notify")).collect();
    let mut features = vec![NS_DISCO_INFO];
    features.extend(notify_vars.iter().map(String::as_str));
    let caps_node = "https://bob.example/joined-spaces-caps";
    let ver = caps_verification_string("client", "pc", "Bob's Client", &features);
    let bob_full = bob.full_jid.clone().expect("bob full jid");
    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
    ))
    .await
    .expect("bob caps presence");
    let disco_query = bob
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server caps disco to bob");
    let iq_id = ws_common::extract_attr_after(&disco_query, "<iq", "id").expect("iq id");
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Bob's Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob disco reply");
    ping_anchor(bob, "joined-spaces-caps-anchor").await;
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

/// The serialized `<field/>` of a configure form with this `var`, or "".
fn form_field<'a>(form: &'a str, var: &str) -> &'a str {
    form.split(var)
        .nth(1)
        .and_then(|rest| rest.split("</field>").next())
        .unwrap_or_default()
}

async fn send_and_wait(client: &mut WsXmppClient, xml: &str, id: &str) -> String {
    client.send(xml).await.expect("send");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("response")
}

/// Publish one item to alice's control node (see [`CONTROL_NODE`]).
async fn publish_control(alice: &mut WsXmppClient, id: &str, item_id: &str) {
    let result = send_and_wait(
        alice,
        &format!(
            r#"<iq type="set" id="{id}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{CONTROL_NODE}">
                  <item id="{item_id}"><mood xmlns="{CONTROL_NODE}"><happy/></mood></item>
                </publish>
              </pubsub>
            </iq>"#
        ),
        id,
    )
    .await;
    assert!(
        result.contains(r#"type='result'"#),
        "control publish {item_id} failed: {result}"
    );
}

/// The owner's `pubsub#owner` configure form of the joined-spaces node.
async fn owner_configure_form(client: &mut WsXmppClient, id: &str) -> String {
    send_and_wait(
        client,
        &format!(
            r#"<iq type="get" id="{id}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
                <configure node="{NODE}"/>
              </pubsub>
            </iq>"#
        ),
        id,
    )
    .await
}

/// Submit one owner configure form field for `node`.
async fn owner_configure_set(
    client: &mut WsXmppClient,
    id: &str,
    node: &str,
    var: &str,
    value: &str,
) -> String {
    send_and_wait(
        client,
        &format!(
            r#"<iq type="set" id="{id}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
                <configure node="{node}">
                  <x xmlns="jabber:x:data" type="submit">
                    <field var="FORM_TYPE" type="hidden">
                      <value>http://jabber.org/protocol/pubsub#node_config</value>
                    </field>
                    <field var="{var}"><value>{value}</value></field>
                  </x>
                </configure>
              </pubsub>
            </iq>"#
        ),
        id,
    )
    .await
}

/// An XEP-0060 §8.9.2 owner affiliations set on the joined-spaces node.
async fn owner_set_affiliations(
    client: &mut WsXmppClient,
    id: &str,
    changes: &[(&str, &str)],
) -> String {
    let affiliations: String = changes
        .iter()
        .map(|(jid, affiliation)| {
            format!(r#"<affiliation jid="{jid}" affiliation="{affiliation}"/>"#)
        })
        .collect();
    send_and_wait(
        client,
        &format!(
            r#"<iq type="set" id="{id}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
                <affiliations node="{NODE}">{affiliations}</affiliations>
              </pubsub>
            </iq>"#
        ),
        id,
    )
    .await
}

/// An XEP-0060 §8.9.1 owner affiliations get on the joined-spaces node.
async fn owner_get_affiliations(client: &mut WsXmppClient, id: &str) -> String {
    send_and_wait(
        client,
        &format!(
            r#"<iq type="get" id="{id}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
                <affiliations node="{NODE}"/>
              </pubsub>
            </iq>"#
        ),
        id,
    )
    .await
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
async fn auto_created_node_is_whitelist_bounded_and_never_sends_last_item() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, _bob) = connect_two_accounts().await;

    // The first publish auto-creates the node; the publish-options it
    // carries ask for `open` access and are not what the node gets.
    publish_space(&mut alice, "pub-auto", DEV_ITEM, "dev").await;

    let form = owner_configure_form(&mut alice, "cfg-auto").await;
    assert!(
        form.contains(r#"type='result'"#),
        "owner configure get failed: {form}"
    );
    assert!(
        form_field(&form, "pubsub#access_model").contains(">whitelist<"),
        "the auto-created node must be whitelist: {form}"
    );
    assert!(
        form_field(&form, "pubsub#max_items").contains(&format!(">{PEP_BOOKMARK_MAX_ITEMS}<")),
        "the auto-created node must hold PEP_BOOKMARK_MAX_ITEMS items: {form}"
    );
    assert!(
        form_field(&form, "pubsub#send_last_published_item").contains(">never<"),
        "the auto-created node must never send its last item: {form}"
    );
    assert!(
        form_field(&form, "pubsub#persist_items").contains(">1<"),
        "the auto-created node must persist its items: {form}"
    );

    let _ = alice.close().await;
}

#[tokio::test]
async fn open_publish_options_do_not_open_the_node() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    // Bob is subscribed to alice's presence: had the publish-options set
    // the `open` or `presence` access model, XEP-0060 §4.5 would admit him.
    establish_bob_subscribes_to_alice(&mut alice, &mut bob).await;

    // Control: bob can read a node of alice's that she opened, so the
    // refusal below is the joined-spaces node's privacy, not a request
    // the harness cannot make.
    publish_control(&mut alice, "pub-control", "control-1").await;
    let opened = owner_configure_set(
        &mut alice,
        "cfg-control-open",
        CONTROL_NODE,
        "pubsub#access_model",
        "open",
    )
    .await;
    assert!(
        opened.contains(r#"type='result'"#),
        "opening the control node failed: {opened}"
    );
    let control_read = send_and_wait(
        &mut bob,
        &format!(
            r#"<iq type="get" id="bob-read-control" to="alice@{DOMAIN}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{CONTROL_NODE}"/>
              </pubsub>
            </iq>"#
        ),
        "bob-read-control",
    )
    .await;
    assert!(
        control_read.contains(r#"type='result'"#) && control_read.contains("control-1"),
        "bob must read alice's open control node: {control_read}"
    );

    publish_space(&mut alice, "pub-open", DEV_ITEM, "dev").await;
    let form = owner_configure_form(&mut alice, "cfg-after-open-options").await;
    assert!(
        form_field(&form, "pubsub#access_model").contains(">whitelist<"),
        "publish-options must not change the access model: {form}"
    );
    assert_bob_cannot_read(&mut bob, "bob-read").await;

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn roster_contact_with_notify_caps_receives_no_event() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    establish_bob_subscribes_to_alice(&mut alice, &mut bob).await;
    advertise_notify_caps(&mut bob, &[NODE, CONTROL_NODE]).await;

    publish_space(&mut alice, "pub-notify", DEV_ITEM, "dev").await;
    // The control publish follows; its XEP-0163 §3 event reaches bob, so
    // the roster and caps setup works, and any event of the joined-spaces
    // publish would have been queued for bob before it.
    publish_control(&mut alice, "pub-notify-control", "control-notify-1").await;

    let frames = bob
        .recv_until(|frame| {
            frame.contains("<message")
                && frame.contains(NS_PUBSUB_EVENT)
                && frame.contains(CONTROL_NODE)
        })
        .await
        .expect("bob, a roster contact advertising +notify, must receive the control event");
    let leaked: Vec<&String> = frames
        .iter()
        .filter(|frame| frame.contains(NODE) || frame.contains(DEV_ITEM))
        .collect();
    assert!(
        leaked.is_empty(),
        "a roster contact must receive no joined-spaces event: {leaked:?}"
    );

    // A retraction notifies no contact either.
    let retract = send_and_wait(
        &mut alice,
        &format!(
            r#"<iq type="set" id="retract-notify">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <retract node="{NODE}" notify="true"><item id="{DEV_ITEM}"/></retract>
              </pubsub>
            </iq>"#
        ),
        "retract-notify",
    )
    .await;
    assert!(
        retract.contains(r#"type='result'"#),
        "retract failed: {retract}"
    );
    publish_control(&mut alice, "pub-notify-control-2", "control-notify-2").await;
    let frames = bob
        .recv_until(|frame| {
            frame.contains("<message")
                && frame.contains(NS_PUBSUB_EVENT)
                && frame.contains("control-notify-2")
        })
        .await
        .expect("bob must receive the second control event");
    let leaked: Vec<&String> = frames.iter().filter(|frame| frame.contains(NODE)).collect();
    assert!(
        leaked.is_empty(),
        "a roster contact must receive no joined-spaces retraction: {leaked:?}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}

/// Whether an affiliations error returns `entity` with `affiliation`
/// inside an owner `<affiliations/>` element, whichever quotes the
/// serializer used.
fn returns_unmodified_affiliation(response: &str, entity: &str, affiliation: &str) -> bool {
    let Some(start) = response.find("<affiliations") else {
        return false;
    };
    let affiliations = &response[start..];
    affiliations.split("<affiliation ").skip(1).any(|element| {
        let element = element.split('>').next().unwrap_or_default();
        (element.contains(&format!("jid='{entity}'"))
            || element.contains(&format!("jid=\"{entity}\"")))
            && (element.contains(&format!("affiliation='{affiliation}'"))
                || element.contains(&format!("affiliation=\"{affiliation}\"")))
    })
}

#[tokio::test]
async fn owner_cannot_share_the_node_through_affiliations() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let admin_bare = format!("admin@{DOMAIN}");

    publish_space(&mut alice, "pub-aff", DEV_ITEM, "dev").await;

    for (id, affiliation) in [
        ("aff-member", "member"),
        ("aff-publisher", "publisher"),
        ("aff-publish-only", "publish-only"),
        ("aff-owner", "owner"),
    ] {
        let refused = owner_set_affiliations(&mut alice, id, &[(&bob_bare, affiliation)]).await;
        assert!(
            refused.contains(r#"type='error'"#) && refused.contains("not-allowed"),
            "granting {affiliation} on the joined-spaces node must be refused: {refused}"
        );
    }

    // A request mixing a permitted change with a refused one is refused
    // whole: neither entity gets a row.
    let mixed = owner_set_affiliations(
        &mut alice,
        "aff-mixed",
        &[(&admin_bare, "outcast"), (&bob_bare, "member")],
    )
    .await;
    assert!(
        mixed.contains(r#"type='error'"#) && mixed.contains("not-allowed"),
        "a request granting member must be refused whole: {mixed}"
    );
    // XEP-0060 §8.9.2.4: an entity left out of the error counts as
    // modified, so the refusal returns both, each with its affiliation as
    // stored (none: neither has a row).
    for entity in [&admin_bare, &bob_bare] {
        assert!(
            returns_unmodified_affiliation(&mixed, entity, "none"),
            "the refusal must return {entity} with its existing affiliation: {mixed}"
        );
    }

    // The account cannot make itself an outcast of its own node.
    let self_outcast =
        owner_set_affiliations(&mut alice, "aff-self-outcast", &[(&alice_bare, "outcast")]).await;
    assert!(
        self_outcast.contains(r#"type='error'"#) && self_outcast.contains("not-allowed"),
        "the account must not be made an outcast of its own node: {self_outcast}"
    );

    let rows = owner_get_affiliations(&mut alice, "aff-get").await;
    assert!(
        rows.contains(r#"type='result'"#),
        "owner affiliations get failed: {rows}"
    );
    assert!(
        !rows.contains(&bob_bare) && !rows.contains(&admin_bare),
        "no refused change may be stored: {rows}"
    );
    assert!(
        !rows.contains("outcast"),
        "the account must keep its derived ownership: {rows}"
    );

    assert_bob_cannot_read(&mut bob, "bob-read-after-aff").await;
    let items = owner_items(&mut alice, "items-after-aff").await;
    assert!(
        items.contains(r#"type='result'"#)
            && (items.contains(r#"node="dev""#) || items.contains("node='dev'")),
        "the owner must still read the node: {items}"
    );

    // Control: the affiliations request itself is served; banning bob is
    // permitted and stored.
    let banned = owner_set_affiliations(&mut alice, "aff-outcast", &[(&bob_bare, "outcast")]).await;
    assert!(
        banned.contains(r#"type='result'"#),
        "banning another account must be permitted: {banned}"
    );
    let rows = owner_get_affiliations(&mut alice, "aff-get-outcast").await;
    assert!(
        rows.contains(&bob_bare) && rows.contains("outcast"),
        "the ban must be stored: {rows}"
    );
    assert_bob_cannot_read(&mut bob, "bob-read-after-ban").await;

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn publish_keeps_an_owner_chosen_item_bound() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, _bob) = connect_two_accounts().await;

    publish_space(&mut alice, "pub-bound-dev", DEV_ITEM, "dev").await;

    let configured =
        owner_configure_set(&mut alice, "cfg-bound", NODE, "pubsub#max_items", "64").await;
    assert!(
        configured.contains(r#"type='result'"#),
        "configuring max_items 64 failed: {configured}"
    );
    let form = owner_configure_form(&mut alice, "cfg-bound-read").await;
    assert!(
        form_field(&form, "pubsub#max_items").contains(">64<"),
        "max_items 64 is within the node's bound and must be kept: {form}"
    );

    // The publish reconciles the node's stored config; a permitted owner
    // choice is not reset to the node's defaults.
    publish_space(&mut alice, "pub-bound-ops", OPS_ITEM, "ops").await;
    let form = owner_configure_form(&mut alice, "cfg-bound-after-publish").await;
    assert!(
        form_field(&form, "pubsub#max_items").contains(">64<"),
        "a publish must keep the owner's max_items: {form}"
    );
    assert!(
        form_field(&form, "pubsub#access_model").contains(">whitelist<"),
        "the node must stay whitelist: {form}"
    );

    let items = owner_items(&mut alice, "items-bound").await;
    assert!(
        (items.contains(r#"node="dev""#) || items.contains("node='dev'"))
            && (items.contains(r#"node="ops""#) || items.contains("node='ops'")),
        "both Spaces must be kept: {items}"
    );

    let _ = alice.close().await;
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
        form_field(&read_back, "pubsub#access_model").contains(">whitelist<"),
        "the joined-spaces node must stay whitelist: {read_back}"
    );
    assert!(
        !form_field(&read_back, "pubsub#max_items").contains(">1<"),
        "the joined-spaces node must keep more than one item: {read_back}"
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
