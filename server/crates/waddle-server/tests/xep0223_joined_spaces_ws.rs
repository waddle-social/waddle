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
//! - no affiliation shares it: granting `member`, `publisher`,
//!   `publish-only` or `owner` is answered with `not-acceptable`, which
//!   returns only those entities with their existing affiliation, while
//!   the permitted changes of the same request are applied (XEP-0060
//!   §8.9.2.4);
//! - only the owner publishes to it or administers it, even against an
//!   affiliation row or an open publish model, and every item names its
//!   Space;
//! - a retract removes one Space and leaves the others.
//!
//! The server under test is a separate `waddle-server` process with
//! in-memory SQLite stores; no PostgreSQL or other service is needed. The
//! publish and administration gate is also driven directly against an
//! in-memory PubSub store holding the rows no request can write.

use waddle_ws_test_support as ws_common;

use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use tokio::sync::Mutex;
use waddle_server::pubsub_authz::{can_administer, can_publish};
use waddle_xmpp::pubsub::{
    Affiliation, InMemoryPubSubStorage, NodeConfig, PubSubStorage, PublishModel,
    PEP_BOOKMARK_MAX_ITEMS,
};
use waddle_xmpp::xep::xep0004::find_data_form;
use waddle_xmpp::xep::xep0357::NS_PUBSUB_PUBLISH_OPTIONS;
use waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_JOINED_SPACES as NODE;
use ws_common::iq::{round_trip, send_iq, send_stanza};
use ws_common::stanza::{
    get, is_error, is_result, own_get, own_set, owner_affiliations_of, parse_frame, pubsub_items,
    pubsub_owner_affiliations, result_payload,
};
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::caps::Caps;
use xmpp_parsers::data_forms::{DataForm, DataFormType, Field};
use xmpp_parsers::disco::{DiscoInfoResult, Feature, Identity};
use xmpp_parsers::hashes::{Algo, Hash};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jid::Jid;
use xmpp_parsers::minidom::rxml::NcName;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::ns;
use xmpp_parsers::ping::Ping;
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::pubsub::owner::{Owner, Payload as OwnerPayload};
use xmpp_parsers::pubsub::pubsub::{Item, Publish, PublishOptions, Retract};
use xmpp_parsers::pubsub::{ItemId, NodeName, PubSub};
use xmpp_parsers::roster::Roster;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const DOMAIN: &str = "localhost";
/// The namespace of a joined-spaces item's `<space/>` payload, which is
/// the node's own name.
const NS: &str = NODE;
const DEV_ITEM: &str = "xmpp:spaces.remote.example?;node=dev";
const OPS_ITEM: &str = "xmpp:spaces.remote.example?;node=ops";
/// An ordinary PEP node of the same account, used as a control: with
/// its default access model it reaches roster contacts advertising
/// `+notify`, and once its owner opens it any account may read it.
const CONTROL_NODE: &str = ns::MOOD;

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn account(localpart: &str) -> BareJid {
    format!("{localpart}@{DOMAIN}")
        .parse()
        .expect("account JID")
}

fn attribute(name: &str) -> NcName {
    NcName::try_from(name).expect("valid XML attribute name")
}

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

/// Every RFC 7395 frame carries one complete XML element.
fn frame_element(frame: &str) -> Element {
    parse_frame(frame).unwrap_or_else(|| panic!("server frame is not one XML element: {frame}"))
}

/// Whether `frame` is a presence of `presence_type`.
fn is_presence(frame: &str, presence_type: &str) -> bool {
    parse_frame(frame).is_some_and(|element| {
        element.is("presence", ns::JABBER_CLIENT) && element.attr("type") == Some(presence_type)
    })
}

/// The node of an XEP-0060 §7.1.2.1 event notification and the ids of
/// the items or retractions it carries, if `frame` is one.
fn event_of(frame: &str) -> Option<(String, Vec<String>)> {
    let message = parse_frame(frame).filter(|element| element.is("message", ns::JABBER_CLIENT))?;
    let items = message
        .get_child("event", ns::PUBSUB_EVENT)?
        .get_child("items", ns::PUBSUB_EVENT)?;
    let ids = items
        .children()
        .filter_map(|child| child.attr("id").map(str::to_owned))
        .collect();
    Some((items.attr("node")?.to_owned(), ids))
}

/// Whether `element` or any element inside it carries a joined-spaces
/// `<space/>` payload or names one of the Spaces' items.
fn discloses_a_space(element: &Element) -> bool {
    element.ns() == NS
        || element
            .attr("id")
            .is_some_and(|id| id == DEV_ITEM || id == OPS_ITEM)
        || element.children().any(discloses_a_space)
}

/// Whether `element` or any element inside it names the joined-spaces
/// node or discloses one of its Spaces.
fn mentions_joined_spaces(element: &Element) -> bool {
    element.attr("node") == Some(NODE)
        || discloses_a_space(element)
        || element.children().any(mentions_joined_spaces)
}

/// Send a ping IQ and wait for its reply: every frame the server sent
/// this connection before the reply has been received by then.
async fn ping_anchor(client: &mut WsXmppClient, id: &str) {
    let _ = round_trip(client, own_get(id, Ping.into())).await;
}

/// Subscribe bob to alice's presence, approved by alice, so alice's
/// roster holds bob with `subscription = from`: the contact XEP-0163 §3
/// notifies and the relationship XEP-0060 §4.5 `presence` and `roster`
/// access models admit.
async fn establish_bob_subscribes_to_alice(alice: &mut WsXmppClient, bob: &mut WsXmppClient) {
    let roster_get = || -> Element {
        Roster {
            ver: None,
            items: Vec::new(),
        }
        .into()
    };
    let _ = round_trip(alice, own_get("roster-init-a", roster_get())).await;
    let _ = round_trip(bob, own_get("roster-init-b", roster_get())).await;

    send_stanza(alice, Presence::new(PresenceType::None)).await;
    send_stanza(
        bob,
        Presence::new(PresenceType::Subscribe).with_to(account("alice")),
    )
    .await;
    let _subscribe = alice
        .recv_matching(|frame| is_presence(frame, "subscribe"))
        .await
        .expect("alice receives subscribe");
    send_stanza(
        alice,
        Presence::new(PresenceType::Subscribed).with_to(account("bob")),
    )
    .await;
    let _subscribed = bob
        .recv_matching(|frame| is_presence(frame, "subscribed"))
        .await
        .expect("bob receives approval");
}

/// Send bob's presence with XEP-0115 caps advertising `+notify` for each
/// of `nodes`, answer the server's disco#info for them, and wait until
/// the server has processed the answer.
async fn advertise_notify_caps(bob: &mut WsXmppClient, nodes: &[&str]) {
    let notify_vars: Vec<String> = nodes.iter().map(|node| format!("{node}+notify")).collect();
    let mut features = vec![ns::DISCO_INFO];
    features.extend(notify_vars.iter().map(String::as_str));
    let caps_node = "https://bob.example/joined-spaces-caps";
    let identity_name = "Bob's Client";
    let ver = caps_verification_string("client", "pc", identity_name, &features);
    let bob_full: Jid = bob
        .full_jid
        .as_deref()
        .expect("bob full jid")
        .parse()
        .expect("bob full JID");
    let caps = Caps::new(
        caps_node,
        Hash::from_base64(Algo::Sha_1, &ver).expect("caps ver"),
    );
    send_stanza(bob, Presence::new(PresenceType::None).with_payload(caps)).await;

    let disco_query = bob
        .recv_matching(|frame| {
            parse_frame(frame).is_some_and(|element| {
                element.is("iq", ns::JABBER_CLIENT)
                    && element.attr("type") == Some("get")
                    && element.has_child("query", ns::DISCO_INFO)
            })
        })
        .await
        .expect("server caps disco to bob");
    let iq_id = frame_element(&disco_query)
        .attr("id")
        .expect("iq id")
        .to_owned();
    let disco = DiscoInfoResult {
        node: Some(format!("{caps_node}#{ver}")),
        identities: vec![Identity {
            category: "client".to_owned(),
            type_: "pc".to_owned(),
            lang: None,
            name: Some(identity_name.to_owned()),
        }],
        features: features.iter().map(|var| Feature::new(*var)).collect(),
        extensions: Vec::new(),
    };
    send_iq(
        bob,
        Iq::Result {
            from: Some(bob_full),
            to: None,
            id: iq_id,
            payload: Some(disco.into()),
        },
    )
    .await;
    ping_anchor(bob, "joined-spaces-caps-anchor").await;
}

/// Publish-options that ask for the weakest privacy a client could
/// request. The server must ignore the access model they name.
fn open_publish_options() -> PublishOptions {
    PublishOptions {
        form: Some(DataForm::new(
            DataFormType::Submit,
            NS_PUBSUB_PUBLISH_OPTIONS,
            vec![
                Field::text_single("pubsub#persist_items", "true"),
                Field::text_single("pubsub#access_model", "open"),
            ],
        )),
    }
}

fn space_payload(node: &str) -> Element {
    Element::builder("space", NS)
        .attr(attribute("service"), "spaces.remote.example")
        .attr(attribute("node"), node)
        .attr(attribute("follow"), "true")
        .build()
}

/// A XEP-0060 §7.1 publish of one item to `node`, with an item id unless
/// `item_id` is `None`.
fn publish_payload(
    node: &str,
    item_id: Option<&str>,
    payload: Element,
    publish_options: Option<PublishOptions>,
) -> Element {
    PubSub::Publish {
        publish: Publish {
            node: NodeName(node.to_owned()),
            items: vec![Item {
                id: item_id.map(|id| ItemId(id.to_owned())),
                publisher: None,
                payload: Some(payload),
            }],
        },
        publish_options,
    }
    .into()
}

/// A publish to the joined-spaces node carrying [`open_publish_options`],
/// to the sender's own account or to `to`.
fn publish_iq(id: &str, to: Option<&BareJid>, item_id: Option<&str>, payload: Element) -> Iq {
    let payload = publish_payload(NODE, item_id, payload, Some(open_publish_options()));
    match to {
        Some(to) => Iq::Set {
            from: None,
            to: Some(to.clone().into()),
            id: id.to_owned(),
            payload,
        },
        None => own_set(id, payload),
    }
}

/// The owner configure form of a `<configure/>` result.
fn configure_form(reply: &Iq) -> waddle_xmpp::xep::xep0004::DataForm {
    let configure = result_payload(reply)
        .and_then(|payload| payload.get_child("configure", ns::PUBSUB_OWNER))
        .unwrap_or_else(|| panic!("owner configure result expected: {reply:?}"));
    find_data_form(configure)
        .expect("configure form")
        .expect("valid configure form")
}

/// The `node` attribute of each `<space/>` payload of an items result.
fn space_nodes(reply: &Iq) -> Vec<String> {
    let items = result_payload(reply)
        .and_then(|payload| payload.get_child("items", ns::PUBSUB))
        .unwrap_or_else(|| panic!("items result expected: {reply:?}"));
    items
        .children()
        .filter(|item| item.is("item", ns::PUBSUB))
        .filter_map(|item| item.get_child("space", NS))
        .filter_map(|space| space.attr("node").map(str::to_owned))
        .collect()
}

/// Publish one item to alice's control node (see [`CONTROL_NODE`]).
async fn publish_control(alice: &mut WsXmppClient, id: &str, item_id: &str) {
    let mood = Element::builder("mood", ns::MOOD)
        .append(Element::builder("happy", ns::MOOD).build())
        .build();
    let result = round_trip(
        alice,
        own_set(id, publish_payload(CONTROL_NODE, Some(item_id), mood, None)),
    )
    .await;
    assert!(
        is_result(&result),
        "control publish {item_id} failed: {result:?}"
    );
}

/// The owner's `pubsub#owner` configure form of the joined-spaces node.
async fn owner_configure_form(
    client: &mut WsXmppClient,
    id: &str,
) -> waddle_xmpp::xep::xep0004::DataForm {
    let reply = round_trip(
        client,
        own_get(
            id,
            Owner {
                payload: OwnerPayload::Configure {
                    node: Some(NodeName(NODE.to_owned())),
                    form: None,
                },
            }
            .into(),
        ),
    )
    .await;
    configure_form(&reply)
}

/// Submit one owner configure form field for `node`.
async fn owner_configure_set(
    client: &mut WsXmppClient,
    id: &str,
    node: &str,
    fields: &[(&str, &str)],
) -> Iq {
    let form = DataForm::new(
        DataFormType::Submit,
        ns::PUBSUB_CONFIGURE,
        fields
            .iter()
            .map(|(var, value)| Field::text_single(var, value))
            .collect(),
    );
    round_trip(
        client,
        own_set(
            id,
            Owner {
                payload: OwnerPayload::Configure {
                    node: Some(NodeName(node.to_owned())),
                    form: Some(form),
                },
            }
            .into(),
        ),
    )
    .await
}

/// An XEP-0060 §8.9.2 owner affiliations set on the joined-spaces node.
async fn owner_set_affiliations(
    client: &mut WsXmppClient,
    id: &str,
    changes: &[(&str, &str)],
) -> Iq {
    round_trip(
        client,
        own_set(id, pubsub_owner_affiliations(NODE, changes)),
    )
    .await
}

/// The rows of an XEP-0060 §8.9.1 owner affiliations get on the
/// joined-spaces node.
async fn owner_affiliation_rows(client: &mut WsXmppClient, id: &str) -> Vec<(String, String)> {
    let reply = round_trip(client, own_get(id, pubsub_owner_affiliations(NODE, &[]))).await;
    result_payload(&reply)
        .and_then(|payload| owner_affiliations_of(payload, NODE))
        .unwrap_or_else(|| panic!("owner affiliations result expected: {reply:?}"))
}

/// The affiliations an XEP-0060 §8.9.2.4 refusal returns: the reply must
/// be a `modify`/`not-acceptable` error carrying the joined-spaces node's
/// owner `<affiliations/>`.
fn refused_affiliations(reply: &Iq) -> Vec<(String, String)> {
    let Iq::Error { error, payload, .. } = reply else {
        panic!("an unsupported affiliation must be refused: {reply:?}");
    };
    assert_eq!(
        (&error.type_, &error.defined_condition),
        (&ErrorType::Modify, &DefinedCondition::NotAcceptable),
        "XEP-0060 §8.9.2.4: the refusal SHOULD be modify/not-acceptable: {reply:?}"
    );
    payload
        .as_ref()
        .and_then(|payload| owner_affiliations_of(payload, NODE))
        .unwrap_or_else(|| panic!("the refusal must carry the refused affiliations: {reply:?}"))
}

fn row(jid: &BareJid, affiliation: &str) -> (String, String) {
    (jid.to_string(), affiliation.to_owned())
}

async fn publish_space(client: &mut WsXmppClient, id: &str, item_id: &str, node: &str) {
    let result = round_trip(
        client,
        publish_iq(id, None, Some(item_id), space_payload(node)),
    )
    .await;
    assert!(
        is_result(&result),
        "publish of {item_id} failed: {result:?}"
    );
}

async fn owner_items(client: &mut WsXmppClient, id: &str) -> Iq {
    round_trip(client, own_get(id, pubsub_items(NODE))).await
}

async fn retract_space(client: &mut WsXmppClient, id: &str, item_id: &str) {
    let retract: Element = PubSub::Retract(Retract {
        node: NodeName(NODE.to_owned()),
        notify: true,
        items: vec![Item {
            id: Some(ItemId(item_id.to_owned())),
            publisher: None,
            payload: None,
        }],
    })
    .into();
    let result = round_trip(client, own_set(id, retract)).await;
    assert!(is_result(&result), "retract failed: {result:?}");
}

async fn assert_bob_cannot_read(bob: &mut WsXmppClient, id: &str) {
    let alice = account("alice").to_string();
    let result = round_trip(bob, get(id, &alice, pubsub_items(NODE))).await;
    assert!(
        is_error(&result, DefinedCondition::Forbidden)
            || is_error(&result, DefinedCondition::ItemNotFound),
        "a non-owner read of the joined-spaces node must fail with forbidden or \
         item-not-found: {result:?}"
    );
    let element: Element = result.clone().into();
    assert!(
        !discloses_a_space(&element),
        "a refused read must not disclose the list: {result:?}"
    );
}

#[tokio::test]
async fn several_spaces_are_kept_side_by_side() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, _bob) = connect_two_accounts().await;

    publish_space(&mut alice, "pub-dev", DEV_ITEM, "dev").await;
    publish_space(&mut alice, "pub-ops", OPS_ITEM, "ops").await;

    let items = owner_items(&mut alice, "items-both").await;
    let nodes = space_nodes(&items);
    assert!(
        nodes.iter().any(|node| node == "dev"),
        "dev missing: {items:?}"
    );
    assert!(
        nodes.iter().any(|node| node == "ops"),
        "ops missing: {items:?}"
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
    assert_eq!(
        form.value("pubsub#access_model"),
        Some("whitelist"),
        "the auto-created node must be whitelist: {form:?}"
    );
    assert_eq!(
        form.value("pubsub#max_items"),
        Some(PEP_BOOKMARK_MAX_ITEMS.to_string().as_str()),
        "the auto-created node must hold PEP_BOOKMARK_MAX_ITEMS items: {form:?}"
    );
    assert_eq!(
        form.value("pubsub#send_last_published_item"),
        Some("never"),
        "the auto-created node must never send its last item: {form:?}"
    );
    assert_eq!(
        form.value("pubsub#persist_items"),
        Some("1"),
        "the auto-created node must persist its items: {form:?}"
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
        &[("pubsub#access_model", "open")],
    )
    .await;
    assert!(
        is_result(&opened),
        "opening the control node failed: {opened:?}"
    );
    let control_read = round_trip(
        &mut bob,
        get(
            "bob-read-control",
            &account("alice").to_string(),
            pubsub_items(CONTROL_NODE),
        ),
    )
    .await;
    let control_items: Vec<String> = result_payload(&control_read)
        .and_then(|payload| payload.get_child("items", ns::PUBSUB))
        .map(|items| {
            items
                .children()
                .filter_map(|item| item.attr("id").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        control_items.iter().any(|id| id == "control-1"),
        "bob must read alice's open control node: {control_read:?}"
    );

    publish_space(&mut alice, "pub-open", DEV_ITEM, "dev").await;
    let form = owner_configure_form(&mut alice, "cfg-after-open-options").await;
    assert_eq!(
        form.value("pubsub#access_model"),
        Some("whitelist"),
        "publish-options must not change the access model: {form:?}"
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
        .recv_until(|frame| event_of(frame).is_some_and(|(node, _)| node == CONTROL_NODE))
        .await
        .expect("bob, a roster contact advertising +notify, must receive the control event");
    let leaked: Vec<&String> = frames
        .iter()
        .filter(|frame| mentions_joined_spaces(&frame_element(frame)))
        .collect();
    assert!(
        leaked.is_empty(),
        "a roster contact must receive no joined-spaces event: {leaked:?}"
    );

    // A retraction notifies no contact either.
    retract_space(&mut alice, "retract-notify", DEV_ITEM).await;
    publish_control(&mut alice, "pub-notify-control-2", "control-notify-2").await;
    let frames = bob
        .recv_until(|frame| {
            event_of(frame).is_some_and(|(node, ids)| {
                node == CONTROL_NODE && ids.iter().any(|id| id == "control-notify-2")
            })
        })
        .await
        .expect("bob must receive the second control event");
    let leaked: Vec<&String> = frames
        .iter()
        .filter(|frame| mentions_joined_spaces(&frame_element(frame)))
        .collect();
    assert!(
        leaked.is_empty(),
        "a roster contact must receive no joined-spaces retraction: {leaked:?}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn owner_cannot_share_the_node_through_affiliations() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;
    let alice_bare = account("alice");
    let bob_bare = account("bob");
    let admin_bare = account("admin");
    let (alice_jid, bob_jid, admin_jid) = (
        alice_bare.to_string(),
        bob_bare.to_string(),
        admin_bare.to_string(),
    );

    publish_space(&mut alice, "pub-aff", DEV_ITEM, "dev").await;

    for (id, affiliation) in [
        ("aff-member", "member"),
        ("aff-publisher", "publisher"),
        ("aff-publish-only", "publish-only"),
        ("aff-owner", "owner"),
    ] {
        let refused =
            owner_set_affiliations(&mut alice, id, &[(bob_jid.as_str(), affiliation)]).await;
        assert_eq!(
            refused_affiliations(&refused),
            vec![row(&bob_bare, "none")],
            "granting {affiliation} must be refused, returning bob with his existing \
             affiliation: {refused:?}"
        );
    }

    // XEP-0060 §8.9.2.4: a request mixing a permitted change with a
    // refused one returns only the refused entity, with its existing
    // affiliation; the entity left out counts as modified, so its change
    // is applied.
    let mixed = owner_set_affiliations(
        &mut alice,
        "aff-mixed",
        &[
            (admin_jid.as_str(), "outcast"),
            (bob_jid.as_str(), "member"),
        ],
    )
    .await;
    assert_eq!(
        refused_affiliations(&mixed),
        vec![row(&bob_bare, "none")],
        "only the refused entity may be returned: {mixed:?}"
    );

    // The account cannot make itself an outcast of its own node; its
    // existing affiliation is the ownership XEP-0163 §1 derives.
    let self_outcast = owner_set_affiliations(
        &mut alice,
        "aff-self-outcast",
        &[(alice_jid.as_str(), "outcast")],
    )
    .await;
    assert_eq!(
        refused_affiliations(&self_outcast),
        vec![row(&alice_bare, "owner")],
        "the account must not be made an outcast of its own node: {self_outcast:?}"
    );

    assert_eq!(
        owner_affiliation_rows(&mut alice, "aff-get").await,
        vec![row(&admin_bare, "outcast")],
        "the permitted change alone is stored; the account keeps its derived ownership"
    );

    assert_bob_cannot_read(&mut bob, "bob-read-after-aff").await;
    let items = owner_items(&mut alice, "items-after-aff").await;
    assert!(
        space_nodes(&items).iter().any(|node| node == "dev"),
        "the owner must still read the node: {items:?}"
    );

    // Banning bob is permitted and stored; a later refused grant returns
    // him with that ban as his existing affiliation and leaves it.
    let banned =
        owner_set_affiliations(&mut alice, "aff-outcast", &[(bob_jid.as_str(), "outcast")]).await;
    assert!(
        is_result(&banned),
        "banning another account must be permitted: {banned:?}"
    );
    let regrant = owner_set_affiliations(
        &mut alice,
        "aff-regrant",
        &[(bob_jid.as_str(), "publisher")],
    )
    .await;
    assert_eq!(
        refused_affiliations(&regrant),
        vec![row(&bob_bare, "outcast")],
        "a refused grant returns the stored ban: {regrant:?}"
    );
    let mut rows = owner_affiliation_rows(&mut alice, "aff-get-outcast").await;
    rows.sort();
    let mut expected = vec![row(&admin_bare, "outcast"), row(&bob_bare, "outcast")];
    expected.sort();
    assert_eq!(rows, expected, "both bans must be stored");
    assert_bob_cannot_read(&mut bob, "bob-read-after-ban").await;

    let _ = alice.close().await;
    let _ = bob.close().await;
}

/// Neither an affiliation row nor an open publish model lets another
/// account publish to or administer the joined-spaces node, though the
/// same rows do on an ordinary PEP node. No request can write such a row
/// (see `owner_cannot_share_the_node_through_affiliations`), so the gate
/// is driven against a store that holds one.
#[tokio::test]
async fn affiliation_rows_grant_another_account_nothing_on_the_node() {
    let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
    let alice = account("alice");
    let bob = account("bob");
    let open_publishing = NodeConfig {
        publish_model: PublishModel::Open,
        ..NodeConfig::default()
    };
    for node in [NODE, CONTROL_NODE] {
        storage
            .get_or_create_node(&alice, node)
            .await
            .expect("node");
        storage
            .update_node_config(&alice, node, &open_publishing)
            .await
            .expect("open publish model");
    }

    // Control: on an ordinary PEP node an owner row lets bob publish and
    // administer.
    storage
        .set_affiliation(&alice, CONTROL_NODE, &bob, Affiliation::Owner)
        .await
        .expect("control owner row");
    assert!(can_publish(&storage, &alice, CONTROL_NODE, &bob, true)
        .await
        .expect("control publish gate"));
    assert!(can_administer(&storage, &alice, CONTROL_NODE, &bob, true)
        .await
        .expect("control administration gate"));

    assert!(
        !can_publish(&storage, &alice, NODE, &bob, true)
            .await
            .expect("publish gate"),
        "an open publish model must not let another account publish"
    );
    for affiliation in [
        Affiliation::Owner,
        Affiliation::Publisher,
        Affiliation::PublishOnly,
        Affiliation::Member,
    ] {
        storage
            .set_affiliation(&alice, NODE, &bob, affiliation)
            .await
            .expect("affiliation row");
        assert!(
            !can_publish(&storage, &alice, NODE, &bob, true)
                .await
                .expect("publish gate"),
            "a {affiliation} row must not let another account publish"
        );
        assert!(
            !can_administer(&storage, &alice, NODE, &bob, true)
                .await
                .expect("administration gate"),
            "a {affiliation} row must not let another account administer"
        );
    }

    assert!(can_publish(&storage, &alice, NODE, &alice, true)
        .await
        .expect("owner publish gate"));
    assert!(can_administer(&storage, &alice, NODE, &alice, true)
        .await
        .expect("owner administration gate"));
}

#[tokio::test]
async fn publish_keeps_an_owner_chosen_item_bound() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, _bob) = connect_two_accounts().await;

    publish_space(&mut alice, "pub-bound-dev", DEV_ITEM, "dev").await;

    let configured =
        owner_configure_set(&mut alice, "cfg-bound", NODE, &[("pubsub#max_items", "64")]).await;
    assert!(
        is_result(&configured),
        "configuring max_items 64 failed: {configured:?}"
    );
    let form = owner_configure_form(&mut alice, "cfg-bound-read").await;
    assert_eq!(
        form.value("pubsub#max_items"),
        Some("64"),
        "max_items 64 is within the node's bound and must be kept: {form:?}"
    );

    // The publish reconciles the node's stored config; a permitted owner
    // choice is not reset to the node's defaults.
    publish_space(&mut alice, "pub-bound-ops", OPS_ITEM, "ops").await;
    let form = owner_configure_form(&mut alice, "cfg-bound-after-publish").await;
    assert_eq!(
        form.value("pubsub#max_items"),
        Some("64"),
        "a publish must keep the owner's max_items: {form:?}"
    );
    assert_eq!(
        form.value("pubsub#access_model"),
        Some("whitelist"),
        "the node must stay whitelist: {form:?}"
    );

    let items = owner_items(&mut alice, "items-bound").await;
    let nodes = space_nodes(&items);
    assert!(
        nodes.iter().any(|node| node == "dev") && nodes.iter().any(|node| node == "ops"),
        "both Spaces must be kept: {items:?}"
    );

    let _ = alice.close().await;
}

#[tokio::test]
async fn owner_configure_cannot_open_the_node() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    publish_space(&mut alice, "pub-before-config", DEV_ITEM, "dev").await;

    // Whether the server accepts the form or refuses it, the node stays
    // owner-only and multi-item.
    let _ = owner_configure_set(
        &mut alice,
        "cfg-open",
        NODE,
        &[("pubsub#access_model", "open"), ("pubsub#max_items", "1")],
    )
    .await;

    let read_back = owner_configure_form(&mut alice, "cfg-read").await;
    assert_eq!(
        read_back.value("pubsub#access_model"),
        Some("whitelist"),
        "the joined-spaces node must stay whitelist: {read_back:?}"
    );
    assert_ne!(
        read_back.value("pubsub#max_items"),
        Some("1"),
        "the joined-spaces node must keep more than one item: {read_back:?}"
    );

    publish_space(&mut alice, "pub-after-config", OPS_ITEM, "ops").await;
    let items = owner_items(&mut alice, "items-after-config").await;
    let nodes = space_nodes(&items);
    assert!(
        nodes.iter().any(|node| node == "dev") && nodes.iter().any(|node| node == "ops"),
        "a configure must not reduce the node to one item: {items:?}"
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

    let foreign = round_trip(
        &mut bob,
        publish_iq(
            "bob-pub",
            Some(&account("alice")),
            Some(OPS_ITEM),
            space_payload("ops"),
        ),
    )
    .await;
    assert!(
        matches!(foreign, Iq::Error { .. }),
        "a non-owner publish to the joined-spaces node must fail: {foreign:?}"
    );

    let anonymous_item = round_trip(
        &mut alice,
        publish_iq("pub-no-id", None, None, space_payload("ops")),
    )
    .await;
    assert!(
        is_error(&anonymous_item, DefinedCondition::BadRequest),
        "a joined-spaces item without an id must be refused: {anonymous_item:?}"
    );

    let items = owner_items(&mut alice, "items-owner").await;
    assert!(
        !space_nodes(&items).iter().any(|node| node == "ops"),
        "no refused publish may be stored: {items:?}"
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

    retract_space(&mut alice, "retract-dev", DEV_ITEM).await;

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
    let nodes = space_nodes(&items);
    assert!(
        !nodes.iter().any(|node| node == "dev"),
        "the retracted Space must be gone: {items:?}"
    );
    assert!(
        nodes.iter().any(|node| node == "ops"),
        "the other Space must stay: {items:?}"
    );

    let _ = alice.close().await;
}
