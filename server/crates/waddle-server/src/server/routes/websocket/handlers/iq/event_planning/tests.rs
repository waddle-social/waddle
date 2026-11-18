use jid::{BareJid, FullJid};
use waddle_xmpp::muc::room_actor::{ChangeAffiliation, JoinAffiliationGrant, JoinWithAffiliation};
use waddle_xmpp::muc::RoomConfig;
use waddle_xmpp::pubsub::{PubSubItem, PubSubRequest};
use waddle_xmpp::Affiliation;
use waddle_xmpp_core::event_planning::{PlanningItem, PlanningKind, NODE_EVENT_PLANNING};
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::iq::Iq;

use super::{can_publish_kind, handle_room_pubsub, Standing};
use crate::server::routes::websocket::cleanup::get_or_create_room_actor;
use crate::server::routes::websocket::test_state::{
    create_test_websocket_state_with_extension_manager, empty_extension_manager, TestStateOverrides,
};
use crate::server::routes::websocket::WebSocketState;

fn iq(room: &BareJid, sender: &FullJid, set: bool) -> Iq {
    if set {
        Iq::Set {
            from: Some(sender.clone().into()),
            to: Some(room.clone().into()),
            id: "planning".into(),
            payload: minidom::Element::builder("pubsub", waddle_xmpp_core::pubsub::NS_PUBSUB)
                .build(),
        }
    } else {
        Iq::Get {
            from: Some(sender.clone().into()),
            to: Some(room.clone().into()),
            id: "planning".into(),
            payload: minidom::Element::builder("pubsub", waddle_xmpp_core::pubsub::NS_PUBSUB)
                .build(),
        }
    }
}

fn is_result(frame: &str) -> bool {
    let element: minidom::Element = frame.parse().expect("valid IQ reply");
    matches!(Iq::try_from(element), Ok(Iq::Result { .. }))
}

fn publish(item: &PlanningItem) -> PubSubRequest {
    PubSubRequest::Publish {
        node: NODE_EVENT_PLANNING.into(),
        item: PubSubItem {
            id: Some(item.id.clone()),
            publisher: item.author.clone(),
            payload: Some(item.to_element()),
        },
        publish_options: None,
    }
}
fn read() -> PubSubRequest {
    PubSubRequest::Items {
        node: NODE_EVENT_PLANNING.into(),
        max_items: None,
        item_ids: Vec::new(),
    }
}

async fn workspace(
    state: &WebSocketState,
    room: &BareJid,
    owner: &FullJid,
) -> kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor> {
    let actor = get_or_create_room_actor(
        state,
        room,
        RoomConfig {
            members_only: true,
            ..Default::default()
        },
        "default".into(),
        "planning".into(),
    )
    .await
    .expect("workspace actor")
    .actor_ref;
    actor
        .ask(JoinWithAffiliation {
            sender_jid: owner.clone(),
            nick: "organizer".into(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Owner),
            local_domain: "example.com".into(),
            admission_revision: 0,
            session: OccupancySessionGeneration::mint(),
        })
        .await
        .expect("owner joins");
    actor
}

#[test]
fn metadata_and_personal_identifiers_cannot_be_impersonated() {
    let alice: BareJid = "alice@example.com".parse().expect("alice");
    let metadata = PlanningItem::new("event", PlanningKind::Event, "London");
    assert!(!can_publish_kind(
        &metadata,
        &alice,
        Standing::of(Affiliation::Member)
    ));
    assert!(can_publish_kind(
        &metadata,
        &alice,
        Standing::of(Affiliation::Owner)
    ));
    let forged = PlanningItem::new("attendance:bob@example.com", PlanningKind::Attendance, "");
    assert!(!can_publish_kind(
        &forged,
        &alice,
        Standing::of(Affiliation::Owner)
    ));
    let reserved = PlanningItem::new("event", PlanningKind::Link, "Link");
    assert!(!can_publish_kind(
        &reserved,
        &alice,
        Standing::of(Affiliation::Owner)
    ));
}

#[tokio::test]
async fn room_membership_protects_storage_and_revocation_takes_effect() {
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides::default(),
    )
    .await;
    let room: BareJid = "london@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let bob: FullJid = "bob@example.com/android".parse().expect("bob");
    let actor = workspace(&state, &room, &alice).await;
    let backing_node =
        super::super::event_planning_access::local_event_planning_access(&actor, &alice)
            .await
            .expect("access scope")
            .storage_node();
    let metadata = PlanningItem::new("event", PlanningKind::Event, "London meetup");
    let owner_result = handle_room_pubsub(
        &iq(&room, &alice, true),
        &state,
        &room,
        &alice,
        publish(&metadata),
    )
    .await;
    assert!(is_result(&owner_result[0]), "{owner_result:?}");
    let denied = handle_room_pubsub(&iq(&room, &bob, false), &state, &room, &bob, read()).await;
    assert!(denied[0].contains("forbidden"));
    assert!(!denied[0].contains("London meetup"));
    actor
        .ask(ChangeAffiliation {
            jid: bob.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("invite member");
    let allowed = handle_room_pubsub(&iq(&room, &bob, false), &state, &room, &bob, read()).await;
    assert!(allowed[0].contains("London meetup"), "{allowed:?}");
    let mut task = PlanningItem::new("book-dinner", PlanningKind::Task, "Book dinner");
    task.author = Some(alice.to_bare());
    let written =
        handle_room_pubsub(&iq(&room, &bob, true), &state, &room, &bob, publish(&task)).await;
    assert!(is_result(&written[0]), "{written:?}");
    let stored = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&room, &backing_node, None, std::slice::from_ref(&task.id))
        .await
        .expect("persisted task");
    let parsed = PlanningItem::from_element(
        stored[0]
            .to_pubsub_item()
            .payload
            .as_ref()
            .expect("payload"),
    )
    .expect("typed task");
    assert_eq!(
        parsed.author,
        Some(bob.to_bare()),
        "publisher must come from authenticated sender"
    );
    actor
        .ask(ChangeAffiliation {
            jid: bob.to_bare(),
            affiliation: Affiliation::None,
        })
        .await
        .expect("revoke member");
    let denied = handle_room_pubsub(&iq(&room, &bob, false), &state, &room, &bob, read()).await;
    assert!(denied[0].contains("forbidden"));
    let denied_write =
        handle_room_pubsub(&iq(&room, &bob, true), &state, &room, &bob, publish(&task)).await;
    assert!(denied_write[0].contains("forbidden"));
}

#[tokio::test]
async fn members_cannot_overwrite_or_retract_another_publishers_items() {
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides::default(),
    )
    .await;
    let room: BareJid = "london@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let bob: FullJid = "bob@example.com/android".parse().expect("bob");
    let actor = workspace(&state, &room, &alice).await;
    let backing_node =
        super::super::event_planning_access::local_event_planning_access(&actor, &alice)
            .await
            .expect("access scope")
            .storage_node();
    actor
        .ask(ChangeAffiliation {
            jid: bob.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("invite member");
    let item = PlanningItem::new("dinner", PlanningKind::Task, "Book dinner");
    let owner_result = handle_room_pubsub(
        &iq(&room, &alice, true),
        &state,
        &room,
        &alice,
        publish(&item),
    )
    .await;
    assert!(is_result(&owner_result[0]));
    let overwrite =
        handle_room_pubsub(&iq(&room, &bob, true), &state, &room, &bob, publish(&item)).await;
    assert!(overwrite[0].contains("forbidden"), "{overwrite:?}");
    let retract = PubSubRequest::Retract {
        node: NODE_EVENT_PLANNING.into(),
        item_id: item.id.clone(),
        notify: false,
    };
    let denied = handle_room_pubsub(&iq(&room, &bob, true), &state, &room, &bob, retract).await;
    assert!(denied[0].contains("error"));
    let items = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&room, &backing_node, None, &[item.id])
        .await
        .expect("retained item");
    assert_eq!(items.len(), 1);
    let configuration = PubSubRequest::ConfigureNode {
        node: NODE_EVENT_PLANNING.into(),
    };
    let denied =
        handle_room_pubsub(&iq(&room, &bob, false), &state, &room, &bob, configuration).await;
    assert!(denied[0].contains("forbidden"));
}

#[tokio::test]
async fn poll_votes_require_live_open_poll_and_known_choices() {
    use waddle_xmpp_core::event_planning::PlanningOption;
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides::default(),
    )
    .await;
    let room: BareJid = "london@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let bob: FullJid = "bob@example.com/android".parse().expect("bob");
    let actor = workspace(&state, &room, &alice).await;
    actor
        .ask(ChangeAffiliation {
            jid: bob.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("invite member");
    let mut poll = PlanningItem::new("dinner", PlanningKind::Poll, "Dinner?");
    poll.options = vec![
        PlanningOption {
            id: "a".into(),
            label: "Friday".into(),
        },
        PlanningOption {
            id: "b".into(),
            label: "Saturday".into(),
        },
    ];
    let mut vote = PlanningItem::new("vote:dinner:bob@example.com", PlanningKind::Vote, "");
    vote.subject = Some("dinner".into());
    vote.selections = vec!["a".into()];
    let missing =
        handle_room_pubsub(&iq(&room, &bob, true), &state, &room, &bob, publish(&vote)).await;
    assert!(missing[0].contains("item-not-found"));
    let created = handle_room_pubsub(
        &iq(&room, &alice, true),
        &state,
        &room,
        &alice,
        publish(&poll),
    )
    .await;
    assert!(is_result(&created[0]), "{created:?}");
    vote.selections = vec!["unknown".into()];
    let unknown =
        handle_room_pubsub(&iq(&room, &bob, true), &state, &room, &bob, publish(&vote)).await;
    assert!(unknown[0].contains("invalid-payload"));
    vote.selections = vec!["a".into()];
    let accepted =
        handle_room_pubsub(&iq(&room, &bob, true), &state, &room, &bob, publish(&vote)).await;
    assert!(is_result(&accepted[0]), "{accepted:?}");
    poll.completed = true;
    let closed = handle_room_pubsub(
        &iq(&room, &alice, true),
        &state,
        &room,
        &alice,
        publish(&poll),
    )
    .await;
    assert!(is_result(&closed[0]));
    vote.selections = vec!["b".into()];
    let denied =
        handle_room_pubsub(&iq(&room, &bob, true), &state, &room, &bob, publish(&vote)).await;
    assert!(denied[0].contains("invalid-payload"));
    let mut forged = vote.clone();
    forged.id = "vote:dinner:alice@example.com".into();
    assert!(handle_room_pubsub(
        &iq(&room, &bob, true),
        &state,
        &room,
        &bob,
        publish(&forged)
    )
    .await[0]
        .contains("error"));
}

#[tokio::test]
async fn assignee_and_organizer_edits_preserve_original_author_and_payer() {
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides::default(),
    )
    .await;
    let room: BareJid = "london@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let bob: FullJid = "bob@example.com/android".parse().expect("bob");
    let actor = workspace(&state, &room, &alice).await;
    let backing_node =
        super::super::event_planning_access::local_event_planning_access(&actor, &alice)
            .await
            .expect("scope")
            .storage_node();
    actor
        .ask(ChangeAffiliation {
            jid: bob.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("invite member");
    let mut task = PlanningItem::new("dinner", PlanningKind::Task, "Book dinner");
    task.assignee = Some(bob.to_bare());
    assert!(is_result(
        &handle_room_pubsub(
            &iq(&room, &alice, true),
            &state,
            &room,
            &alice,
            publish(&task)
        )
        .await[0]
    ));
    task.completed = true;
    task.description = Some("Booked for six".into());
    let completed =
        handle_room_pubsub(&iq(&room, &bob, true), &state, &room, &bob, publish(&task)).await;
    assert!(is_result(&completed[0]), "{completed:?}");
    let stored = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&room, &backing_node, None, std::slice::from_ref(&task.id))
        .await
        .expect("task");
    assert_eq!(stored[0].publisher, Some(alice.to_bare()));
    let parsed = PlanningItem::from_element(
        stored[0]
            .to_pubsub_item()
            .payload
            .as_ref()
            .expect("payload"),
    )
    .expect("task");
    assert_eq!(parsed.author, Some(alice.to_bare()));
    assert!(parsed.completed);
    task.assignee = Some("carol@example.com".parse().expect("carol"));
    assert!(
        handle_room_pubsub(&iq(&room, &bob, true), &state, &room, &bob, publish(&task)).await[0]
            .contains("forbidden")
    );
    let mut expense = PlanningItem::new("hotel", PlanningKind::Expense, "Hotel");
    expense.amount_minor = Some(15000);
    expense.currency = Some("GBP".into());
    assert!(is_result(
        &handle_room_pubsub(
            &iq(&room, &bob, true),
            &state,
            &room,
            &bob,
            publish(&expense)
        )
        .await[0]
    ));
    expense.description = Some("Two nights".into());
    assert!(is_result(
        &handle_room_pubsub(
            &iq(&room, &alice, true),
            &state,
            &room,
            &alice,
            publish(&expense)
        )
        .await[0]
    ));
    let stored = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&room, &backing_node, None, &[expense.id])
        .await
        .expect("expense");
    assert_eq!(stored[0].publisher, Some(bob.to_bare()));
    let parsed = PlanningItem::from_element(
        stored[0]
            .to_pubsub_item()
            .payload
            .as_ref()
            .expect("payload"),
    )
    .expect("expense");
    assert_eq!(parsed.author, Some(bob.to_bare()));
}
