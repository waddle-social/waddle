//! XEP-0503 version 0.3.0 across S2S, inbound, where an enrolled host-grant
//! scope is involved (ADR-0020 D5, D8 and D10; xsf/xeps
//! 4132c531487fe9450ae90b905b8df950d0d478b2).
//!
//! - A Space lists its rooms, and a room an enrolled scope manages is told
//!   to a remote asker only where the room's own host would describe it:
//!   to a principal its canonical grants admit to view it, decided on the
//!   evidence of the stream that carried the request. A stranger is not
//!   told of it, in the Space's items as in the room's own discovery.
//! - The Space node of an enrolled scope is served to nobody: whatever its
//!   `federated` setting says, it answers as a node that does not exist, in
//!   every operation a remote principal may ask of a node, with the error a
//!   node that does not exist gets.
//!
//! The deployment is the revocation fixture's on PostgreSQL, with one
//! replica that runs an S2S snapshot admitting `home-b.test` and serves its
//! Spaces to it. The cases need PostgreSQL only. Without it they return
//! early, as every PostgreSQL case of the workspace does; a lane that sets
//! `WADDLE_TEST_REQUIRE_POSTGRES=1` fails them instead, since the fixture
//! reads the test database through
//! `waddle_ws_test_support::require_test_postgres_url`, which panics there.
//! No SpiceDB or second process is involved.
#![cfg(feature = "test-support")]

// The shared fixtures serve several suites; this one uses a subset.
#[allow(dead_code)]
mod host_grant_revocation_support;
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;
#[allow(dead_code)]
#[path = "s2s_inbound_muc_support/managed.rs"]
mod managed;
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

use jid::BareJid;
use managed::{Managed, GUEST_PHONE, STRANGER_PHONE};
use minidom::Element;
use s2s_inbound_muc_support::{error_condition, jid};
use waddle_server::db::s2s_spaces::FederatedSpaces;
use waddle_server::test_support::host_spaces_for_remote_principals;
use waddle_xmpp::pubsub::{NodeConfig, PubSubItem};
use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};
use xmpp_parsers::iq::Iq;

/// The Spaces service of the fixture's deployment.
const SPACES: &str = "spaces.example.com";
/// A Space no scope enrolls.
const LOBBY: &str = "lobby";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NS_DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";

fn spaces() -> BareJid {
    SPACES.parse().expect("the Spaces service")
}

/// The fixture's managed room on a replica that also serves its Spaces to
/// `home-b.test`.
async fn serving_spaces(label: &str) -> Option<Managed> {
    let managed = Managed::start(label, |spec| spec.local.push(SPACES)).await?;
    host_spaces_for_remote_principals(&managed.hosted.state, &managed.hosted.router);
    Some(managed)
}

/// Create the public Space `node` and turn its `federated` setting on.
async fn federated_space(managed: &Managed, node: &str) {
    let state = &managed.hosted.state;
    let storage = &state.deps.protocol.pubsub_storage;
    let (stored, _) = storage
        .get_or_create_node(&spaces(), node)
        .await
        .expect("the Space node");
    storage
        .update_node_config(&spaces(), node, &NodeConfig::spaces_public())
        .await
        .expect("the Space's configuration");
    let enabled = FederatedSpaces::new(state.deps.app_state.db_pool.global().clone())
        .enable(
            &spaces(),
            node,
            stored.created_at.timestamp_millis(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .expect("the setting is written");
    assert!(enabled);
}

fn items(from: &str, id: &str, node: &str) -> Iq {
    Iq::Get {
        from: Some(jid(from)),
        to: Some(jid(SPACES)),
        id: id.to_owned(),
        payload: Element::builder("pubsub", NS_PUBSUB)
            .append(
                Element::builder("items", NS_PUBSUB)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                    .build(),
            )
            .build(),
    }
}

async fn ask(managed: &Managed, iq: Iq) -> Element {
    let id = iq.id().to_owned();
    managed.hosted.peer_sends_to(SPACES, iq).await;
    managed.hosted.answered(&id).await
}

fn item_ids(answer: &Element) -> Vec<String> {
    let mut ids = Vec::new();
    for child in answer.children() {
        if child.name() == "item" {
            ids.extend(child.attr("id").map(ToOwned::to_owned));
        }
        ids.extend(item_ids(child));
    }
    ids
}

#[tokio::test]
async fn a_managed_rooms_bookmark_is_told_only_to_a_principal_its_grants_admit() {
    let Some(managed) = serving_spaces("s2s_spaces_managed").await else {
        return;
    };
    let room = managed.room();
    federated_space(&managed, LOBBY).await;
    // A bookmark of the managed room that a Space no scope enrolls still
    // holds, as one published before the room was enrolled.
    let bookmark = PubSubItem::new(
        Some(room.to_string()),
        Some(build_bookmark_element(
            &Bookmark::new(room.clone()).with_name("managed"),
        )),
    );
    managed
        .hosted
        .state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces(), LOBBY, &bookmark, None, false)
        .await
        .expect("the stored bookmark");

    // The guest holds a membership of the room: its grants admit it to
    // view the room on the evidence of its stream.
    let told = ask(&managed, items(GUEST_PHONE, "managed-1", LOBBY)).await;
    assert_eq!(told.attr("type"), Some("result"), "{told:?}");
    assert_eq!(item_ids(&told), [room.to_string()]);

    // A stranger reads the Space, which is public, and is not told of the
    // room.
    let untold = ask(&managed, items(STRANGER_PHONE, "managed-2", LOBBY)).await;
    assert_eq!(untold.attr("type"), Some("result"), "{untold:?}");
    assert!(item_ids(&untold).is_empty(), "{untold:?}");

    // Nor is anybody while the room is not open to other servers.
    managed.deployment.replicas[0]
        .federate(&managed.deployment.grants.scope.channel_id, false)
        .await;
    let closed = ask(&managed, items(GUEST_PHONE, "managed-3", LOBBY)).await;
    assert_eq!(closed.attr("type"), Some("result"), "{closed:?}");
    assert!(item_ids(&closed).is_empty(), "{closed:?}");
    managed.deployment.close().await;
}

/// An element `name` in `namespace` naming `node`.
fn naming(name: &str, namespace: &str, node: &str) -> minidom::element::ElementBuilder {
    Element::builder(name, namespace).attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
}

/// A `pubsub` element of `namespace` holding `child`.
fn pubsub(namespace: &str, child: Element) -> Element {
    Element::builder("pubsub", namespace).append(child).build()
}

/// Every operation a remote principal may ask of the node `node`, each
/// with whether it is a `set`: discovery, reading, subscribing and leaving,
/// placing and removing a room, and each request of the owner namespace.
fn operations(node: &str, account: &str) -> Vec<(&'static str, bool, Element)> {
    let room = "lounge@muc.example.com";
    vec![
        (
            "disco-info",
            false,
            naming("query", NS_DISCO_INFO, node).build(),
        ),
        (
            "disco-items",
            false,
            naming("query", NS_DISCO_ITEMS, node).build(),
        ),
        (
            "items",
            false,
            pubsub(NS_PUBSUB, naming("items", NS_PUBSUB, node).build()),
        ),
        (
            "subscribe",
            true,
            pubsub(
                NS_PUBSUB,
                naming("subscribe", NS_PUBSUB, node)
                    .attr(minidom::rxml::xml_ncname!("jid").to_owned(), account)
                    .build(),
            ),
        ),
        (
            "unsubscribe",
            true,
            pubsub(
                NS_PUBSUB,
                naming("unsubscribe", NS_PUBSUB, node)
                    .attr(minidom::rxml::xml_ncname!("jid").to_owned(), account)
                    .build(),
            ),
        ),
        (
            "publish",
            true,
            pubsub(
                NS_PUBSUB,
                naming("publish", NS_PUBSUB, node)
                    .append(
                        Element::builder("item", NS_PUBSUB)
                            .attr(minidom::rxml::xml_ncname!("id").to_owned(), room)
                            .build(),
                    )
                    .build(),
            ),
        ),
        (
            "retract",
            true,
            pubsub(
                NS_PUBSUB,
                naming("retract", NS_PUBSUB, node)
                    .append(
                        Element::builder("item", NS_PUBSUB)
                            .attr(minidom::rxml::xml_ncname!("id").to_owned(), room)
                            .build(),
                    )
                    .build(),
            ),
        ),
        (
            "configure",
            false,
            pubsub(
                NS_PUBSUB_OWNER,
                naming("configure", NS_PUBSUB_OWNER, node).build(),
            ),
        ),
        (
            "delete",
            true,
            pubsub(
                NS_PUBSUB_OWNER,
                naming("delete", NS_PUBSUB_OWNER, node).build(),
            ),
        ),
        (
            "purge",
            true,
            pubsub(
                NS_PUBSUB_OWNER,
                naming("purge", NS_PUBSUB_OWNER, node).build(),
            ),
        ),
        (
            "affiliations",
            false,
            pubsub(
                NS_PUBSUB_OWNER,
                naming("affiliations", NS_PUBSUB_OWNER, node).build(),
            ),
        ),
        (
            "subscriptions",
            false,
            pubsub(
                NS_PUBSUB_OWNER,
                naming("subscriptions", NS_PUBSUB_OWNER, node).build(),
            ),
        ),
    ]
}

/// `payload` from `from` to the Spaces service, as a `get` or a `set`.
fn request(from: &str, id: String, set: bool, payload: Element) -> Iq {
    let (from, to) = (Some(jid(from)), Some(jid(SPACES)));
    if set {
        Iq::Set {
            from,
            to,
            id,
            payload,
        }
    } else {
        Iq::Get {
            from,
            to,
            id,
            payload,
        }
    }
}

#[tokio::test]
async fn the_space_of_an_enrolled_scope_is_served_to_nobody() {
    let Some(managed) = serving_spaces("s2s_spaces_enrolled").await else {
        return;
    };
    let node = managed.deployment.grants.scope.space_node.clone();
    // Even with a PubSub node of that name whose setting is on.
    federated_space(&managed, &node).await;
    let account = jid(GUEST_PHONE).to_bare().to_string();

    // Every operation is answered as for a node that does not exist, with
    // the very error such a node gets.
    let enrolled = operations(&node, &account);
    let missing = operations("nowhere", &account);
    for ((name, set, asked), (_, _, control)) in enrolled.into_iter().zip(missing) {
        let answer = ask(
            &managed,
            request(GUEST_PHONE, format!("enrolled-{name}"), set, asked),
        )
        .await;
        let expected = ask(
            &managed,
            request(GUEST_PHONE, format!("missing-{name}"), set, control),
        )
        .await;
        assert_eq!(
            error_condition(&answer).as_deref(),
            Some("item-not-found"),
            "{name}: {answer:?}"
        );
        assert_eq!(
            answer.children().find(|child| child.name() == "error"),
            expected.children().find(|child| child.name() == "error"),
            "{name}"
        );
    }
    let listed = ask(
        &managed,
        Iq::Get {
            from: Some(jid(GUEST_PHONE)),
            to: Some(jid(SPACES)),
            id: "enrolled-listing".to_owned(),
            payload: Element::builder("query", NS_DISCO_ITEMS).build(),
        },
    )
    .await;
    assert_eq!(listed.attr("type"), Some("result"), "{listed:?}");
    assert!(item_ids(&listed).is_empty(), "{listed:?}");
    managed.deployment.close().await;
}
