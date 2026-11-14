//! XEP-0060 on a room-bound extension route node: the room decides.
//!
//! An extension declares a route whose state node names a room
//! (`urn:example:board:{room}`). Reading that node through the extensions
//! service hands the reader a PubSub `member` affiliation on it, and that
//! row outlives a later ban from the room. Subscribing, receiving event
//! notifications and publishing are therefore decided by the room as it
//! stands, not by the affiliation: a banned account keeps its stale row and
//! gets nothing through it.
//!
//! The cases drive the production router over a loopback WebSocket against
//! an in-memory state with an extension manager that declares the route and
//! loads no extension. They need neither PostgreSQL nor SpiceDB.
#![cfg(feature = "test-support")]

mod ws_loopback_support;

use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use waddle_extensions::{
    DisplayText, ExtensionManager, ExtensionRouteDescriptor, ExtensionRouteScope,
    ExtensionRouteSurface, PayloadNamespace, PluginId, PubSubNode, RouteId,
};
use waddle_server::permissions::{
    DeleteTuple, Object, ObjectType, Relation, Subject, Tuple, WriteTuple,
};
use waddle_server::test_support::websocket_state_with_extension_manager;
use waddle_xmpp::pubsub::{Affiliation, NodeConfig};
use ws_loopback_support::{
    get, is_error, is_result, items, publish, room, round_trip, set, subscribe, Loopback,
    EXTENSIONS,
};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::DefinedCondition;

const CHANNEL: &str = "board-room";
const PAYLOAD_NS: &str = "urn:example:board";

fn route() -> ExtensionRouteDescriptor {
    ExtensionRouteDescriptor {
        plugin: PluginId::try_new("board").expect("plugin id"),
        id: RouteId::try_new("board").expect("route id"),
        label: DisplayText::try_new("Board").expect("label"),
        scope: ExtensionRouteScope::Channel,
        surface: ExtensionRouteSurface::List,
        state_node: PubSubNode::try_new("urn:example:board:{room}").expect("state node"),
        payload_namespace: PayloadNamespace::try_new(PAYLOAD_NS).expect("payload namespace"),
    }
}

/// The route's state node for the fixture room.
fn node() -> String {
    format!("urn:example:board:{}", room(CHANNEL))
}

fn card(text: &str) -> Element {
    Element::builder("card", PAYLOAD_NS).append(text).build()
}

struct Fixture {
    server: Loopback,
    bob: BareJid,
}

impl Fixture {
    async fn start() -> Self {
        let manager = Arc::new(ExtensionManager::with_route_descriptors_for_test(vec![
            route(),
        ]));
        let server = Loopback::serve(websocket_state_with_extension_manager(manager).await).await;
        let alice = server.account("alice").await;
        let bob = server.account("bob").await;
        let carol = server.account("carol").await;
        server.channel(CHANNEL, true).await;
        let fixture = Self { server, bob };
        for member in [&alice, &fixture.bob, &carol] {
            fixture.member_relation(member, true).await;
        }
        let extensions: BareJid = EXTENSIONS.parse().expect("extensions service");
        let storage = &fixture.server.state.deps.protocol.pubsub_storage;
        storage
            .get_or_create_node(&extensions, &node())
            .await
            .expect("route node");
        storage
            .update_node_config(&extensions, &node(), &NodeConfig::spaces_private())
            .await
            .expect("route node configuration");
        storage
            .set_affiliation(&extensions, &node(), &alice, Affiliation::Publisher)
            .await
            .expect("publisher affiliation");
        fixture
    }

    async fn member_relation(&self, account: &BareJid, present: bool) {
        let tuple = Tuple::new(
            Object::new(ObjectType::Channel, CHANNEL),
            Relation::new("member"),
            Subject::user(account.as_str()),
        );
        let actor = &self.server.state.deps.app_state.permission_actor;
        if present {
            actor.ask(WriteTuple { tuple }).await.expect("member");
        } else {
            actor
                .ask(DeleteTuple { tuple })
                .await
                .expect("member removed");
        }
    }

    /// Ban `account` from the room the way a channel ban is stored: the
    /// member relation is replaced by `outcast`.
    async fn ban(&self, account: &BareJid) {
        self.member_relation(account, false).await;
        self.server
            .state
            .deps
            .app_state
            .permission_actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Channel, CHANNEL),
                    Relation::new("outcast"),
                    Subject::user(account.as_str()),
                ),
            })
            .await
            .expect("outcast");
    }

    async fn node_affiliation(&self, account: &BareJid) -> Affiliation {
        let extensions: BareJid = EXTENSIONS.parse().expect("extensions service");
        self.server
            .state
            .deps
            .protocol
            .pubsub_storage
            .affiliation(&extensions, &node(), account)
            .await
            .expect("node affiliation")
    }
}

/// Whether `client` receives an event notification for the fixture node
/// within `wait`.
async fn receives_event(client: &mut waddle_ws_test_support::WsXmppClient, wait: Duration) -> bool {
    let node = node();
    client
        .recv_matching_within(wait, |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "message"
                    && element.children().any(|child| {
                        child.name() == "event"
                            && child
                                .children()
                                .any(|items| items.attr("node") == Some(node.as_str()))
                    })
            })
        })
        .await
        .is_ok()
}

#[tokio::test]
async fn a_banned_member_keeps_a_stale_affiliation_and_cannot_subscribe_through_it() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.server.client("bob", "laptop").await;

    // Reading the room's extension state hands bob a member affiliation.
    let reply = round_trip(&mut bob, get("read", EXTENSIONS, items(&node()))).await;
    assert!(is_result(&reply), "member read refused: {reply:?}");
    assert_eq!(
        fixture.node_affiliation(&fixture.bob).await,
        Affiliation::Member
    );

    fixture.ban(&fixture.bob).await;
    // The affiliation row outlives the ban ...
    assert_eq!(
        fixture.node_affiliation(&fixture.bob).await,
        Affiliation::Member
    );
    // ... and admits nothing.
    let reply = round_trip(
        &mut bob,
        set(
            "subscribe",
            EXTENSIONS,
            subscribe(&node(), fixture.bob.as_str()),
        ),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a banned account subscribed: {reply:?}"
    );
    let reply = round_trip(&mut bob, get("reread", EXTENSIONS, items(&node()))).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a banned account read the room's state: {reply:?}"
    );
}

#[tokio::test]
async fn a_subscription_made_before_a_ban_receives_no_event_after_it() {
    let fixture = Fixture::start().await;
    let mut alice = fixture.server.client("alice", "laptop").await;
    let mut bob = fixture.server.client("bob", "laptop").await;
    let mut carol = fixture.server.client("carol", "laptop").await;

    for (client, account, id) in [
        (&mut bob, "bob", "bob-read"),
        (&mut carol, "carol", "carol-read"),
    ] {
        let reply = round_trip(client, get(id, EXTENSIONS, items(&node()))).await;
        assert!(is_result(&reply), "{account} read refused: {reply:?}");
        let jid = format!("{account}@{}", ws_loopback_support::DOMAIN);
        let reply = round_trip(client, set(id, EXTENSIONS, subscribe(&node(), &jid))).await;
        assert!(
            is_result(&reply),
            "{account} subscription refused: {reply:?}"
        );
    }

    fixture.ban(&fixture.bob).await;
    let reply = round_trip(
        &mut alice,
        set(
            "post",
            EXTENSIONS,
            publish(&node(), "card-1", card("hello")),
        ),
    )
    .await;
    assert!(is_result(&reply), "member publish refused: {reply:?}");

    // The member who may still read the room receives the event; the banned
    // subscriber does not.
    assert!(receives_event(&mut carol, Duration::from_secs(5)).await);
    assert!(!receives_event(&mut bob, Duration::from_secs(1)).await);
}

#[tokio::test]
async fn a_banned_member_cannot_publish_through_a_publisher_affiliation() {
    let fixture = Fixture::start().await;
    let extensions: BareJid = EXTENSIONS.parse().expect("extensions service");
    fixture
        .server
        .state
        .deps
        .protocol
        .pubsub_storage
        .set_affiliation(&extensions, &node(), &fixture.bob, Affiliation::Publisher)
        .await
        .expect("publisher affiliation");
    fixture.ban(&fixture.bob).await;
    let mut bob = fixture.server.client("bob", "laptop").await;
    let reply = round_trip(
        &mut bob,
        set("post", EXTENSIONS, publish(&node(), "card-1", card("spam"))),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a banned account published: {reply:?}"
    );
}
