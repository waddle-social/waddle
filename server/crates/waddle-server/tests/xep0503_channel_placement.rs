//! XEP-0503 (v0.3.0): who may place an existing channel in a Space.
//!
//! Publishing a room's XEP-0402 bookmark to a Space node lists the room in
//! that Space. Waddle also moves the channel's Space link to the node and
//! writes the channel's parent relationship to the Space, so the Space's
//! admins gain the channel's management and deleting the Space destroys
//! the channel. Authority over the Space node alone is therefore not
//! enough to place a channel there: the publisher must manage the channel,
//! the channel must already belong to that Space, or the publisher must be
//! the deployment owner, who can already place any channel through the
//! channel administration commands.
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
use waddle_server::test_support::websocket_state;
use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};
use ws_loopback_support::{is_error, is_result, publish, room, round_trip, set, Loopback, SPACES};
use xmpp_parsers::stanza_error::DefinedCondition;

/// The community whose channel is the target.
const VICTIM_SPACE: &str = "victim-space";
/// A Space the publisher owns.
const OTHER_SPACE: &str = "other-space";
const CHANNEL: &str = "victim-room";

struct Fixture {
    server: Loopback,
    /// Owns `OTHER_SPACE` and nothing of the channel.
    mallory: BareJid,
    /// Owns the channel and `VICTIM_SPACE`, and also `OTHER_SPACE`.
    victor: BareJid,
}

impl Fixture {
    async fn start() -> Self {
        let server = Loopback::serve(websocket_state().await).await;
        let mallory = server.account("mallory").await;
        let victor = server.account("victor").await;
        let fixture = Self {
            server,
            mallory,
            victor,
        };
        let spaces: BareJid = SPACES.parse().expect("Spaces service");
        for node in [VICTIM_SPACE, OTHER_SPACE] {
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
        fixture
            .server
            .state
            .deps
            .app_state
            .channel_space_link_store
            .set(&ChannelSpaceLink {
                channel_jid: room(CHANNEL),
                space_jid: space_jid(VICTIM_SPACE),
                space_node: SpaceNode::from(VICTIM_SPACE),
                created_at: 0,
            })
            .await
            .expect("channel-space link");
        fixture
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

    /// The Space node the channel's link names.
    async fn linked_space(&self) -> Option<String> {
        self.server
            .state
            .deps
            .app_state
            .channel_space_link_store
            .get(&room(CHANNEL))
            .await
            .expect("channel-space link read")
            .map(|link| link.space_node.as_str().to_owned())
    }

    /// The Space nodes the channel's parent relationships name.
    async fn parents(&self) -> Vec<String> {
        let mut parents: Vec<String> = self
            .server
            .state
            .deps
            .app_state
            .permission_actor
            .ask(ListSubjects {
                object: Object::new(ObjectType::Channel, CHANNEL),
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

fn space_jid(node: &str) -> BareJid {
    space_jid_for_node(
        &SPACES.parse::<BareJid>().expect("Spaces service"),
        &SpaceNode::from(node),
    )
    .expect("projectable Space node")
}

fn bookmark_publish(id: &str, node: &str) -> xmpp_parsers::iq::Iq {
    let room = room(CHANNEL);
    let bookmark = Bookmark::new(room.clone()).with_name(CHANNEL);
    set(
        id,
        SPACES,
        publish(node, room.as_str(), build_bookmark_element(&bookmark)),
    )
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
    assert!(fixture.parents().await.contains(&OTHER_SPACE.to_owned()));
}

#[tokio::test]
async fn a_space_owner_may_republish_a_channel_already_in_their_space() {
    let fixture = Fixture::start().await;
    // Mallory now also owns the channel's own Space, without managing the
    // channel: the channel already belongs there, so the publish only
    // refreshes its bookmark.
    fixture
        .relate(
            Object::new(ObjectType::Space, VICTIM_SPACE),
            "owner",
            Subject::user(fixture.mallory.as_str()),
        )
        .await;
    let mut mallory = fixture.server.client("mallory", "laptop").await;
    let reply = round_trip(&mut mallory, bookmark_publish("refresh", VICTIM_SPACE)).await;
    assert!(is_result(&reply), "republish refused: {reply:?}");
    assert_eq!(fixture.linked_space().await.as_deref(), Some(VICTIM_SPACE));
    assert_eq!(fixture.parents().await, vec![VICTIM_SPACE.to_owned()]);

    let reply = round_trip(&mut mallory, bookmark_publish("steal", OTHER_SPACE)).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "moving out of the Space still needs the channel: {reply:?}"
    );
    assert_eq!(fixture.linked_space().await.as_deref(), Some(VICTIM_SPACE));
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
}
