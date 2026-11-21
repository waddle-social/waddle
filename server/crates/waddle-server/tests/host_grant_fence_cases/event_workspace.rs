//! The private event workspace of a managed room: the XEP-0060 storage a
//! session addresses to the room's JID. It is decided from the room's
//! canonical grants (`read` to retrieve it, `send_message` to write one's
//! own objects, `manage` for the organizer's), whatever affiliation the
//! room actor's list still mirrors, and fails closed while the grants
//! cannot be read. A room no enrolled scope manages keeps deciding it from
//! the room actor's list.

use jid::BareJid;
use waddle_server::permissions::host_grant::RoomAffiliation;
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::stanza::{
    get, is_error, is_result, pubsub_items, pubsub_publish, pubsub_retract, result_payload, set,
};
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::muc::room_actor::ChangeAffiliation;
use waddle_xmpp::muc::room_registry_actor::{CreateRoom, GetRoom};
use waddle_xmpp_core::event_planning::{PlanningItem, PlanningKind, NODE_EVENT_PLANNING};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::DefinedCondition;

use super::host_grant_support::{bare, OWNER, PEER, SENDER};
use super::{error_condition, join, ManagedRoom, CAROL, MUC};

/// Retrieve the workspace of `room`.
async fn read(client: &mut WsXmppClient, id: &str, room: &BareJid) -> Iq {
    round_trip(
        client,
        get(id, room.as_str(), pubsub_items(NODE_EVENT_PLANNING)),
    )
    .await
}

/// Publish `item` to the workspace of `room`.
async fn publish(client: &mut WsXmppClient, id: &str, room: &BareJid, item: &PlanningItem) -> Iq {
    round_trip(
        client,
        set(
            id,
            room.as_str(),
            pubsub_publish(NODE_EVENT_PLANNING, &item.id, item.to_element()),
        ),
    )
    .await
}

/// Retract the object `item_id` from the workspace of `room`.
async fn retract(client: &mut WsXmppClient, id: &str, room: &BareJid, item_id: &str) -> Iq {
    round_trip(
        client,
        set(
            id,
            room.as_str(),
            pubsub_retract(NODE_EVENT_PLANNING, item_id),
        ),
    )
    .await
}

/// The identifiers of the objects a workspace read returned.
fn item_ids(reply: &Iq) -> Vec<String> {
    let Some(pubsub) = result_payload(reply) else {
        return Vec::new();
    };
    pubsub
        .children()
        .filter(|child| child.name() == "items")
        .flat_map(|items| items.children())
        .filter_map(|item| item.attr("id").map(str::to_owned))
        .collect()
}

/// A shared task object with the identifier `id`.
fn task(id: &str) -> PlanningItem {
    PlanningItem::new(id, PlanningKind::Task, "Book dinner")
}

/// Give `account` an affiliation in the room actor's list of `room`, served
/// by the fixture's state, as a mirror that is out of date would hold it.
async fn mirror(
    fixture: &ManagedRoom,
    room: &BareJid,
    account: &str,
    affiliation: waddle_xmpp::Affiliation,
) {
    let actor = fixture
        .server
        .state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("room lookup")
        .expect("room actor");
    actor
        .ask(ChangeAffiliation {
            jid: bare(account),
            affiliation,
        })
        .await
        .expect("mirror affiliation");
}

/// Open the room actor of `room` on the fixture's state without a join.
async fn open_room(fixture: &ManagedRoom, room: &BareJid, channel_id: &str) {
    fixture
        .server
        .state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "test".to_owned(),
            channel_id: channel_id.to_owned(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("room actor");
}

#[tokio::test]
async fn an_event_workspace_in_a_managed_room_follows_the_canonical_grants() {
    let Some(fixture) = ManagedRoom::start("hg_event_workspace").await else {
        return;
    };
    let room = fixture.room().clone();
    let mut owner = fixture.server.client("owner", "phone").await;
    let mut romeo = fixture.server.client("romeo", "phone").await;
    let mut alice = fixture.server.client("alice", "phone").await;
    let mut carol = fixture.server.client("carol", "phone").await;
    for (client, nick) in [(&mut owner, "owner"), (&mut romeo, "romeo")] {
        let joined = join(client, &room, nick).await;
        assert_eq!(
            error_condition(&joined),
            None,
            "a grantee's join: {joined:?}"
        );
    }

    // The canonical owner maintains the event object.
    let event = PlanningItem::new("event", PlanningKind::Event, "Summit");
    let reply = publish(&mut owner, "owner-event", &room, &event).await;
    assert!(is_result(&reply), "the owner's event object: {reply:?}");

    // An account the grants do not name is shown nothing and writes
    // nothing, though the room actor's list names it an owner.
    fixture
        .mirror_affiliation(CAROL, waddle_xmpp::Affiliation::Owner)
        .await;
    let reply = read(&mut carol, "carol-read", &room).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a mirrored owner without a grant read the workspace: {reply:?}"
    );
    for (id, item) in [
        ("carol-task", task("carol-task")),
        (
            "carol-event",
            PlanningItem::new("event", PlanningKind::Event, "Hijacked"),
        ),
    ] {
        let reply = publish(&mut carol, id, &room, &item).await;
        assert!(
            is_error(&reply, DefinedCondition::Forbidden),
            "a mirrored owner without a grant wrote {}: {reply:?}",
            item.id
        );
    }
    let reply = retract(&mut carol, "carol-retract", &room, "event").await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a mirrored owner without a grant retracted the event object: {reply:?}"
    );

    // A canonical member writes its own objects and reads the workspace;
    // an owner affiliation the list mirrors gives it no organizer right.
    fixture
        .mirror_affiliation(SENDER, waddle_xmpp::Affiliation::Owner)
        .await;
    let reply = publish(&mut romeo, "romeo-task", &room, &task("romeo-task")).await;
    assert!(is_result(&reply), "a member's own object: {reply:?}");
    let reply = publish(
        &mut romeo,
        "romeo-event",
        &room,
        &PlanningItem::new("event", PlanningKind::Event, "Renamed"),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a member with a mirrored owner affiliation edited the event object: {reply:?}"
    );
    let reply = retract(&mut romeo, "romeo-retract-event", &room, "event").await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a member with a mirrored owner affiliation retracted the event object: {reply:?}"
    );
    let reply = read(&mut romeo, "romeo-read", &room).await;
    assert!(is_result(&reply), "a member's read: {reply:?}");
    let listed = item_ids(&reply);
    assert!(
        listed.contains(&"event".to_owned()) && listed.contains(&task("romeo-task").id),
        "a member's read: {listed:?}"
    );

    // A canonical member the list does not name reads it all the same: the
    // list decides nothing in either direction.
    fixture
        .mirror_affiliation(PEER, waddle_xmpp::Affiliation::None)
        .await;
    let reply = read(&mut alice, "alice-read", &room).await;
    assert!(
        is_result(&reply),
        "a member the mirror does not name was refused: {reply:?}"
    );

    // Once the grant is revoked the member reads and writes nothing,
    // whatever the list still mirrors.
    fixture
        .grants
        .set(OWNER, SENDER, RoomAffiliation::None)
        .await
        .expect("revoke");
    let reply = read(&mut romeo, "romeo-read-revoked", &room).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a revoked member read the workspace: {reply:?}"
    );
    let reply = retract(
        &mut romeo,
        "romeo-retract-revoked",
        &room,
        &task("romeo-task").id,
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a revoked member retracted its object: {reply:?}"
    );

    // The canonical owner retracts another member's object.
    let reply = retract(&mut owner, "owner-retract", &room, &task("romeo-task").id).await;
    assert!(is_result(&reply), "the owner's retraction: {reply:?}");
    fixture.close().await;
}

#[tokio::test]
async fn an_event_workspace_refuses_while_the_grants_cannot_be_read() {
    let Some(fixture) = ManagedRoom::start_unattached("hg_event_workspace_unread").await else {
        return;
    };
    let room = fixture.room().clone();
    open_room(&fixture, &room, &fixture.grants.scope.channel_id).await;
    mirror(&fixture, &room, SENDER, waddle_xmpp::Affiliation::Owner).await;
    let mut romeo = fixture.server.client("romeo", "phone").await;
    let reply = read(&mut romeo, "managed-read", &room).await;
    assert!(
        is_error(&reply, DefinedCondition::InternalServerError),
        "a managed room's workspace was decided without its grants: {reply:?}"
    );
    let reply = publish(&mut romeo, "managed-task", &room, &task("romeo-task")).await;
    assert!(
        is_error(&reply, DefinedCondition::InternalServerError),
        "a managed room's workspace was written without its grants: {reply:?}"
    );

    // A room no scope enrolls keeps deciding its workspace from the room
    // actor's list on the same replica.
    const PLAIN_ROOM: &str = "plain-event";
    let plain_room: BareJid = format!("{PLAIN_ROOM}@{MUC}")
        .parse()
        .expect("plain room address");
    fixture.server.channel(PLAIN_ROOM, false).await;
    open_room(&fixture, &plain_room, PLAIN_ROOM).await;
    let reply = read(&mut romeo, "plain-read-outsider", &plain_room).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "an unmanaged room's workspace was read by a non-member: {reply:?}"
    );
    mirror(
        &fixture,
        &plain_room,
        SENDER,
        waddle_xmpp::Affiliation::Member,
    )
    .await;
    let reply = publish(&mut romeo, "plain-task", &plain_room, &task("romeo-task")).await;
    assert!(
        is_result(&reply),
        "an unmanaged room's member wrote: {reply:?}"
    );
    let reply = read(&mut romeo, "plain-read", &plain_room).await;
    assert_eq!(
        item_ids(&reply),
        vec![task("romeo-task").id],
        "an unmanaged room's member read: {reply:?}"
    );
    fixture.close().await;
}
