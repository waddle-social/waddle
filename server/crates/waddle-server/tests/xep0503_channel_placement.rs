//! XEP-0503 (v0.3.0): who may place an existing channel in a Space.
//!
//! Publishing a room's XEP-0402 bookmark to a Space node lists the room in
//! that Space. Waddle also moves the channel's Space link to the node and
//! writes the channel's parent relationship to the Space, so the Space's
//! admins gain the channel's management and deleting the Space destroys
//! the channel. Authority over the Space node alone is therefore not
//! enough to place a channel there. The decision is taken from explicit
//! relationships, the same on every permission backend:
//!
//! - a publish that moves nothing is the Space's own: the channel's link
//!   and parents name that Space and no other;
//! - otherwise the publisher owns, administers or manages the channel, owns
//!   or administers a Space that is the channel's parent now, or is the
//!   deployment owner, who can already place any channel through the
//!   channel administration commands.
//!
//! The cases drive the production router over a loopback WebSocket against
//! an in-memory state whose catalog rows, links and permission
//! relationships they write directly. They need neither PostgreSQL nor
//! SpiceDB.
#![cfg(feature = "test-support")]

mod ws_loopback_support;

use jid::BareJid;
use waddle_server::channel_space_links::ChannelSpaceLink;
use waddle_server::permissions::{
    ListSubjects, Object, ObjectType, Relation, Subject, SubjectType, Tuple, WriteTuple,
};
use waddle_server::space_identity::{space_jid_for_node, SpaceNode};
use waddle_server::test_support::websocket_state_with_test_spaces_service;
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::stanza::{get, is_error, is_result, pubsub_items, pubsub_publish, set};
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::muc::room_registry_actor::CreateRoom;
use waddle_xmpp::pubsub::{Affiliation, PubSubItem};
use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};
use ws_loopback_support::Loopback;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::DefinedCondition;

/// The test state's MUC service.
const MUC: &str = "muc.example.com";
/// The test state's Spaces service.
const SPACES: &str = "spaces.example.com";
const NS_MUC: &str = "http://jabber.org/protocol/muc";

/// The community whose channel is the target.
const VICTIM_SPACE: &str = "victim-space";
/// A Space the publisher owns.
const OTHER_SPACE: &str = "other-space";
const CHANNEL: &str = "victim-room";
/// A channel no Space lists and no Space parents.
const LOOSE_CHANNEL: &str = "loose-room";
/// A third Space, which the fixture links the channel to in one case.
const LINKED_SPACE: &str = "linked-space";

struct Fixture {
    server: Loopback,
    /// Owns `OTHER_SPACE` and nothing of the channel.
    mallory: BareJid,
    /// Owns the channel and `VICTIM_SPACE`, and also `OTHER_SPACE`.
    victor: BareJid,
}

impl Fixture {
    async fn start() -> Self {
        let server = Loopback::serve(websocket_state_with_test_spaces_service().await).await;
        let mallory = server.account("mallory").await;
        let victor = server.account("victor").await;
        let fixture = Self {
            server,
            mallory,
            victor,
        };
        let spaces: BareJid = SPACES.parse().expect("Spaces service");
        for node in [VICTIM_SPACE, OTHER_SPACE, LINKED_SPACE] {
            fixture
                .server
                .state
                .deps
                .protocol
                .pubsub_storage
                .get_or_create_node(&spaces, node)
                .await
                .expect("Space node");
        }
        fixture.server.channel(CHANNEL, false).await;
        fixture.server.channel(LOOSE_CHANNEL, false).await;
        fixture
            .relate(
                Object::new(ObjectType::Space, OTHER_SPACE),
                "owner",
                Subject::user(fixture.mallory.as_str()),
            )
            .await;
        for node in [VICTIM_SPACE, OTHER_SPACE] {
            fixture
                .relate(
                    Object::new(ObjectType::Space, node),
                    "owner",
                    Subject::user(fixture.victor.as_str()),
                )
                .await;
        }
        fixture
            .relate(
                Object::new(ObjectType::Channel, CHANNEL),
                "owner",
                Subject::user(fixture.victor.as_str()),
            )
            .await;
        fixture
            .relate(
                Object::new(ObjectType::Channel, CHANNEL),
                "parent",
                Subject::userset(SubjectType::Space, VICTIM_SPACE, ""),
            )
            .await;
        fixture.link(VICTIM_SPACE).await;
        fixture
    }

    /// Link the channel to the Space `node`, replacing any link it has.
    async fn link(&self, node: &str) {
        self.server
            .state
            .deps
            .app_state
            .channel_space_link_store
            .set(&ChannelSpaceLink {
                channel_jid: room(CHANNEL),
                space_jid: space_jid(node),
                space_node: SpaceNode::from(node),
                created_at: 0,
            })
            .await
            .expect("channel-space link");
    }

    async fn relate(&self, object: Object, relation: &str, subject: Subject) {
        self.server
            .state
            .deps
            .app_state
            .permission_actor
            .ask(WriteTuple {
                tuple: Tuple::new(object, Relation::new(relation), subject),
            })
            .await
            .expect("permission relationship");
    }

    /// Spawn the channel's members-only room actor and make its catalog row
    /// members-only, so that a join is admitted by a channel right only.
    async fn room_actor(&self, channel: &str) {
        self.server.channel(channel, true).await;
        let config = waddle_xmpp::muc::RoomConfig {
            members_only: true,
            ..Default::default()
        };
        self.server
            .state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room(channel),
                waddle_id: "test".to_owned(),
                channel_id: channel.to_owned(),
                config,
            })
            .await
            .expect("room actor");
    }

    /// Make `account` an owner of the Space node `node` in PubSub, so that
    /// it may read the node's items.
    async fn space_affiliation(&self, node: &str, account: &BareJid) {
        let spaces: BareJid = SPACES.parse().expect("Spaces service");
        self.server
            .state
            .deps
            .protocol
            .pubsub_storage
            .set_affiliation(&spaces, node, account, Affiliation::Owner)
            .await
            .expect("Space node affiliation");
    }

    /// Store the channel's bookmark in the Space node `node` directly,
    /// without a publish and its placement.
    async fn bookmark_in(&self, node: &str) {
        let spaces: BareJid = SPACES.parse().expect("Spaces service");
        let channel = room(CHANNEL);
        let bookmark = Bookmark::new(channel.clone()).with_name(CHANNEL);
        let item = PubSubItem::new(
            Some(channel.to_string()),
            Some(build_bookmark_element(&bookmark)),
        );
        self.server
            .state
            .deps
            .protocol
            .pubsub_storage
            .publish_item(&spaces, node, &item, None, false)
            .await
            .expect("stored bookmark");
    }

    /// The Space node the channel's link names.
    async fn linked_space(&self) -> Option<String> {
        self.linked_space_of(CHANNEL).await
    }

    /// The Space node the link of `channel` names.
    async fn linked_space_of(&self, channel: &str) -> Option<String> {
        self.server
            .state
            .deps
            .app_state
            .channel_space_link_store
            .get(&room(channel))
            .await
            .expect("channel-space link read")
            .map(|link| link.space_node.as_str().to_owned())
    }

    /// The Space nodes the channel's parent relationships name.
    async fn parents(&self) -> Vec<String> {
        self.parents_of(CHANNEL).await
    }

    /// The Space nodes the parent relationships of `channel` name.
    async fn parents_of(&self, channel: &str) -> Vec<String> {
        let mut parents: Vec<String> = self
            .server
            .state
            .deps
            .app_state
            .permission_actor
            .ask(ListSubjects {
                object: Object::new(ObjectType::Channel, channel),
                relation: Relation::new("parent"),
            })
            .await
            .expect("parent relationships")
            .into_iter()
            .map(|subject| subject.id)
            .collect();
        parents.sort();
        parents
    }
}

/// The room JID of the channel `channel_id`.
fn room(channel_id: &str) -> BareJid {
    format!("{channel_id}@{MUC}").parse().expect("room JID")
}

fn space_jid(node: &str) -> BareJid {
    space_jid_for_node(
        &SPACES.parse::<BareJid>().expect("Spaces service"),
        &SpaceNode::from(node),
    )
    .expect("projectable Space node")
}

fn bookmark_publish(id: &str, node: &str) -> xmpp_parsers::iq::Iq {
    channel_bookmark_publish(id, node, CHANNEL)
}

fn channel_bookmark_publish(id: &str, node: &str, channel: &str) -> xmpp_parsers::iq::Iq {
    let room = room(channel);
    let bookmark = Bookmark::new(room.clone()).with_name(channel);
    set(
        id,
        SPACES,
        pubsub_publish(node, room.as_str(), build_bookmark_element(&bookmark)),
    )
}

/// Join `room` as `nick` and return the first presence the room sends from
/// that occupant address: the self-presence or the join error.
async fn join(client: &mut WsXmppClient, room: &BareJid, nick: &str) -> Element {
    let occupant = format!("{room}/{nick}");
    let presence = Element::builder("presence", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            occupant.as_str(),
        )
        .append(Element::builder("x", NS_MUC).build())
        .build();
    client
        .send(&String::from(&presence))
        .await
        .expect("send join");
    let frame = client
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "presence" && element.attr("from") == Some(occupant.as_str())
            })
        })
        .await
        .expect("join presence");
    frame.parse().expect("presence XML")
}

/// The items the Space `node` lists.
async fn listed(fixture: &Fixture, node: &str) -> usize {
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    fixture
        .server
        .state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces, node, None, &[])
        .await
        .expect("Space items")
        .len()
}

#[tokio::test]
async fn a_space_owner_cannot_place_another_communitys_channel_in_their_space() {
    let fixture = Fixture::start().await;
    let mut mallory = fixture.server.client("mallory", "laptop").await;

    let reply = round_trip(&mut mallory, bookmark_publish("steal", OTHER_SPACE)).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "the publish must be refused: {reply:?}"
    );
    // Nothing moved: the channel keeps its link and its only parent, and
    // the other Space lists nothing.
    assert_eq!(fixture.linked_space().await.as_deref(), Some(VICTIM_SPACE));
    assert_eq!(fixture.parents().await, vec![VICTIM_SPACE.to_owned()]);
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    let listed = fixture
        .server
        .state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces, OTHER_SPACE, None, &[])
        .await
        .expect("Space items");
    assert!(listed.is_empty(), "the other Space lists {listed:?}");
}

#[tokio::test]
async fn the_channel_manager_places_the_channel_and_republishes_it_in_its_space() {
    let fixture = Fixture::start().await;
    let mut victor = fixture.server.client("victor", "laptop").await;

    // Republishing the bookmark in the Space the channel belongs to only
    // updates the bookmark.
    let reply = round_trip(&mut victor, bookmark_publish("refresh", VICTIM_SPACE)).await;
    assert!(is_result(&reply), "republish refused: {reply:?}");
    assert_eq!(fixture.linked_space().await.as_deref(), Some(VICTIM_SPACE));

    // The channel's owner may move it to another Space they own.
    let reply = round_trip(&mut victor, bookmark_publish("move", OTHER_SPACE)).await;
    assert!(is_result(&reply), "move refused: {reply:?}");
    assert_eq!(fixture.linked_space().await.as_deref(), Some(OTHER_SPACE));
    assert_eq!(fixture.parents().await, vec![OTHER_SPACE.to_owned()]);
}

#[tokio::test]
async fn the_owner_of_the_channels_parent_space_republishes_and_moves_it() {
    let fixture = Fixture::start().await;
    // Mallory now also owns the channel's own Space, without any
    // relationship on the channel itself.
    fixture
        .relate(
            Object::new(ObjectType::Space, VICTIM_SPACE),
            "owner",
            Subject::user(fixture.mallory.as_str()),
        )
        .await;
    let mut mallory = fixture.server.client("mallory", "laptop").await;
    // The channel already belongs to that Space alone: the publish only
    // refreshes its bookmark.
    let reply = round_trip(&mut mallory, bookmark_publish("refresh", VICTIM_SPACE)).await;
    assert!(is_result(&reply), "republish refused: {reply:?}");
    assert_eq!(fixture.linked_space().await.as_deref(), Some(VICTIM_SPACE));
    assert_eq!(fixture.parents().await, vec![VICTIM_SPACE.to_owned()]);

    // The owner of the channel's current parent Space holds the authority
    // the channel inherits from it, and may move it to another Space.
    let reply = round_trip(&mut mallory, bookmark_publish("move", OTHER_SPACE)).await;
    assert!(is_result(&reply), "move refused: {reply:?}");
    assert_eq!(fixture.linked_space().await.as_deref(), Some(OTHER_SPACE));
    assert_eq!(fixture.parents().await, vec![OTHER_SPACE.to_owned()]);
}

#[tokio::test]
async fn a_space_owner_cannot_place_a_channel_no_space_parents() {
    let fixture = Fixture::start().await;
    let mut mallory = fixture.server.client("mallory", "laptop").await;
    // Mallory owns the destination Space; the loose channel has no link, no
    // parent and no relationship of hers.
    let reply = round_trip(
        &mut mallory,
        channel_bookmark_publish("claim", OTHER_SPACE, LOOSE_CHANNEL),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "the publish must be refused: {reply:?}"
    );
    assert_eq!(fixture.linked_space_of(LOOSE_CHANNEL).await, None);
    assert!(fixture.parents_of(LOOSE_CHANNEL).await.is_empty());
    assert_eq!(listed(&fixture, OTHER_SPACE).await, 0);
}

#[tokio::test]
async fn a_link_that_disagrees_with_the_parent_is_moved_only_with_authority() {
    let fixture = Fixture::start().await;
    // The channel is linked to one Space and parented by another.
    fixture.link(LINKED_SPACE).await;
    fixture
        .relate(
            Object::new(ObjectType::Space, LINKED_SPACE),
            "owner",
            Subject::user(fixture.mallory.as_str()),
        )
        .await;
    let mut mallory = fixture.server.client("mallory", "laptop").await;

    // The owner of the linked Space, which is not the channel's parent,
    // holds no authority over the channel: republishing there would make
    // that Space its parent.
    let reply = round_trip(&mut mallory, bookmark_publish("relink", LINKED_SPACE)).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "the publish must be refused: {reply:?}"
    );
    assert_eq!(fixture.linked_space().await.as_deref(), Some(LINKED_SPACE));
    assert_eq!(fixture.parents().await, vec![VICTIM_SPACE.to_owned()]);
    assert_eq!(listed(&fixture, LINKED_SPACE).await, 0);

    // Reading the linked Space's items backfills the bookmarks of the
    // channels linked to it. The channel already has a parent elsewhere, so
    // the read lists nothing new and neither adds the linked Space as a
    // parent nor removes the parent the channel has.
    fixture.room_actor(CHANNEL).await;
    fixture
        .space_affiliation(LINKED_SPACE, &fixture.mallory)
        .await;
    let reply = round_trip(
        &mut mallory,
        get("backfill", SPACES, pubsub_items(LINKED_SPACE)),
    )
    .await;
    assert!(is_result(&reply), "the items read was refused: {reply:?}");
    assert_eq!(listed(&fixture, LINKED_SPACE).await, 0);
    assert_eq!(fixture.parents().await, vec![VICTIM_SPACE.to_owned()]);

    // A bookmark of the channel left in the linked Space does not make
    // that Space its parent when an account without access joins: the join
    // repairs a missing parent only, and this channel has one.
    fixture.bookmark_in(LINKED_SPACE).await;
    let joined = join(&mut mallory, &room(CHANNEL), "mallory").await;
    assert_eq!(
        joined.attr("type"),
        Some("error"),
        "the join was admitted: {joined:?}"
    );
    assert_eq!(fixture.parents().await, vec![VICTIM_SPACE.to_owned()]);

    // The owner of the parent Space, who has no relationship on the channel
    // itself, holds the authority the channel inherits from it, so
    // republishing in the parent Space moves the link there.
    let olivia = fixture.server.account("olivia").await;
    fixture
        .relate(
            Object::new(ObjectType::Space, VICTIM_SPACE),
            "owner",
            Subject::user(olivia.as_str()),
        )
        .await;
    let mut olivia = fixture.server.client("olivia", "laptop").await;
    let reply = round_trip(&mut olivia, bookmark_publish("realign", VICTIM_SPACE)).await;
    assert!(is_result(&reply), "the parent owner was refused: {reply:?}");
    assert_eq!(fixture.linked_space().await.as_deref(), Some(VICTIM_SPACE));
    assert_eq!(fixture.parents().await, vec![VICTIM_SPACE.to_owned()]);
    assert_eq!(listed(&fixture, LINKED_SPACE).await, 0);
}

#[tokio::test]
async fn the_deployment_owner_places_a_channel_it_does_not_manage() {
    let fixture = Fixture::start().await;
    let operator = fixture.server.account("operator").await;
    // The deployment's server object, as account bootstrap names it.
    fixture
        .relate(
            Object::new(ObjectType::Server, "deployment"),
            "owner",
            Subject::user(operator.as_str()),
        )
        .await;
    let mut operator = fixture.server.client("operator", "laptop").await;
    let reply = round_trip(&mut operator, bookmark_publish("place", OTHER_SPACE)).await;
    assert!(is_result(&reply), "deployment owner refused: {reply:?}");
    assert_eq!(fixture.linked_space().await.as_deref(), Some(OTHER_SPACE));
    // The Space the channel left held no bookmark of it; the move still
    // removes that Space as the channel's parent.
    assert_eq!(listed(&fixture, VICTIM_SPACE).await, 0);
    assert_eq!(fixture.parents().await, vec![OTHER_SPACE.to_owned()]);
}
