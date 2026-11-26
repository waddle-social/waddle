use jid::{BareJid, Jid};
use kameo::actor::ActorRef;
use waddle_server::notification_outbox::{PushDisplayNameRequest, PushDisplayNameResolver};
use waddle_server::test_support::{
    published_push_display_name_resolver, seed_local_account, websocket_state, WebSocketState,
};
use waddle_xmpp::muc::room_actor::{Join, RoomActor};
use waddle_xmpp::muc::room_registry_actor::CreateRoom;
use waddle_xmpp::muc::RoomConfig;
use waddle_xmpp::pubsub::{NodeConfig, PubSubItem};
use waddle_xmpp::push::envelope::NotificationClass;
use waddle_xmpp::push::names::PushNames;
use waddle_xmpp::xep::xep0292::{build_vcard4_element, VCard4, PEP_NODE_VCARD4};
use waddle_xmpp::{Affiliation, Role};
use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedMucSender, ArchivedRichMessage};
use waddle_xmpp_core::xep0359::StanzaId;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::message::MessageType;

use crate::support::bare;

const SENDER: &str = "bob@example.com";
const RECIPIENT: &str = "alice@example.com";
const ROOM: &str = "park@muc.example.com";

fn parse_jid(address: &str) -> Jid {
    address.parse().expect("JID")
}

// Seed at the storage seam so tests can also represent an item with the
// wrong publisher and an otherwise readable remote PEP node.
async fn publish_vcard(
    state: &WebSocketState,
    owner: &BareJid,
    publisher: &BareJid,
    vcard: VCard4,
) {
    let item = PubSubItem::new(Some("current".into()), Some(build_vcard4_element(&vcard)));
    state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(owner, PEP_NODE_VCARD4, &item, Some(publisher), true)
        .await
        .expect("publish vCard");
}

async fn resolve_direct(state: &WebSocketState, sender: &Jid) -> PushNames {
    published_push_display_name_resolver(state)
        .resolve(PushDisplayNameRequest {
            recipient: &bare(RECIPIENT),
            conversation: &sender.to_bare(),
            sender,
            class: NotificationClass::Dm,
            archive_stanza_id: None,
        })
        .await
}

#[tokio::test]
async fn readable_vcard_supplies_the_published_full_name() {
    let state = websocket_state().await;
    seed_local_account(&state, "bob").await;
    let owner = bare(SENDER);
    let vcard = VCard4::new()
        .with_full_name("Bob Builder")
        .with_nickname("Bobby");
    publish_vcard(&state, &owner, &owner, vcard).await;

    let names = resolve_direct(&state, &parse_jid("bob@example.com/phone")).await;
    assert_eq!(
        names.sender.as_ref().map(|name| name.as_str()),
        Some("Bob Builder")
    );
    assert!(names.room.is_none());
}

#[tokio::test]
async fn invisible_vcard_full_name_falls_back_to_nickname() {
    let state = websocket_state().await;
    seed_local_account(&state, "bob").await;
    let owner = bare(SENDER);
    let vcard = VCard4::new()
        .with_full_name(" \u{200b}\u{2060} ")
        .with_nickname("Bobby");
    publish_vcard(&state, &owner, &owner, vcard).await;

    let names = resolve_direct(&state, &parse_jid(SENDER)).await;
    assert_eq!(
        names.sender.as_ref().map(|name| name.as_str()),
        Some("Bobby")
    );
}

#[tokio::test]
async fn vcard_read_denial_omits_the_sender_name() {
    let state = websocket_state().await;
    seed_local_account(&state, "bob").await;
    let owner = bare(SENDER);
    publish_vcard(
        &state,
        &owner,
        &owner,
        VCard4::new().with_full_name("Bob Builder"),
    )
    .await;
    state
        .deps
        .protocol
        .pubsub_storage
        .update_node_config(&owner, PEP_NODE_VCARD4, &NodeConfig::whitelist())
        .await
        .expect("restrict vCard reads");

    assert!(resolve_direct(&state, &parse_jid(SENDER))
        .await
        .sender
        .is_none());
}

#[tokio::test]
async fn vcard_published_by_another_account_omits_the_sender_name() {
    let state = websocket_state().await;
    seed_local_account(&state, "bob").await;
    publish_vcard(
        &state,
        &bare(SENDER),
        &bare(RECIPIENT),
        VCard4::new().with_full_name("Forged Bob"),
    )
    .await;

    assert!(resolve_direct(&state, &parse_jid(SENDER))
        .await
        .sender
        .is_none());
}

#[tokio::test]
async fn remote_sender_omits_the_sender_name_even_with_readable_pep() {
    let state = websocket_state().await;
    seed_local_account(&state, "bob").await;
    let sender = parse_jid("bob@remote.example/phone");
    let owner = sender.to_bare();
    publish_vcard(
        &state,
        &owner,
        &owner,
        VCard4::new().with_full_name("Remote Bob"),
    )
    .await;

    assert!(resolve_direct(&state, &sender).await.sender.is_none());
}

async fn create_room(state: &WebSocketState) -> ActorRef<RoomActor> {
    state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: bare(ROOM),
            waddle_id: "parks".into(),
            channel_id: "park".into(),
            config: RoomConfig {
                name: "The Park".into(),
                ..RoomConfig::default()
            },
        })
        .await
        .expect("room actor")
}

async fn archive_room_message(state: &WebSocketState, sender: Jid, id: &StanzaId) {
    let room = bare(ROOM);
    let mut message = ArchivedMessage::for_test(sender, Jid::from(room.clone()));
    message.id = id.id.clone();
    message.stanza_id = Some(StanzaId::new("client-message-id", Jid::from(room.clone())));
    message.body = Some("Hello park".into());
    message.message_type = MessageType::Groupchat;
    message.rich = Some(ArchivedRichMessage {
        muc_sender: Some(ArchivedMucSender {
            jid: parse_jid("bob@example.com/phone"),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }),
        ..ArchivedRichMessage::default()
    });
    state
        .deps
        .protocol
        .mam_storage
        .store_message(&room, &message)
        .await
        .expect("archive original sender");
}

async fn resolve_room(
    state: &WebSocketState,
    sender: &Jid,
    archive_stanza_id: StanzaId,
) -> PushNames {
    published_push_display_name_resolver(state)
        .resolve(PushDisplayNameRequest {
            recipient: &bare(RECIPIENT),
            conversation: &bare(ROOM),
            sender,
            class: NotificationClass::PersonalMention,
            archive_stanza_id: Some(archive_stanza_id),
        })
        .await
}

#[tokio::test]
async fn room_uses_config_and_the_archived_account_after_nick_reuse() {
    let state = websocket_state().await;
    let actor = create_room(&state).await;
    seed_local_account(&state, "bob").await;
    seed_local_account(&state, "mallory").await;
    for (account, name) in [(SENDER, "Bob Builder"), ("mallory@example.com", "Mallory")] {
        let owner = bare(account);
        publish_vcard(&state, &owner, &owner, VCard4::new().with_full_name(name)).await;
    }
    let occupant = parse_jid("park@muc.example.com/shared-nick");
    let id = StanzaId::new("room-issued-id", parse_jid(ROOM));
    archive_room_message(&state, occupant.clone(), &id).await;
    actor
        .ask(Join {
            session: OccupancySessionGeneration::mint(),
            nick: "shared-nick".into(),
            real_jid: "mallory@example.com/laptop".parse().expect("real JID"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("new occupant takes the old nickname");

    let names = resolve_room(&state, &occupant, id).await;
    assert_eq!(
        names.room.as_ref().map(|name| name.as_str()),
        Some("The Park")
    );
    assert_eq!(
        names.sender.as_ref().map(|name| name.as_str()),
        Some("Bob Builder")
    );
}

#[tokio::test]
async fn unknown_nick_keeps_the_room_name_without_a_sender_name() {
    let state = websocket_state().await;
    create_room(&state).await;
    let names = resolve_room(
        &state,
        &parse_jid("park@muc.example.com/unknown"),
        StanzaId::new("missing-id", parse_jid(ROOM)),
    )
    .await;
    assert_eq!(
        names.room.as_ref().map(|name| name.as_str()),
        Some("The Park")
    );
    assert!(names.sender.is_none());
}

#[tokio::test]
async fn archive_row_for_a_different_occupant_omits_the_sender_name() {
    let state = websocket_state().await;
    create_room(&state).await;
    seed_local_account(&state, "bob").await;
    let owner = bare(SENDER);
    publish_vcard(
        &state,
        &owner,
        &owner,
        VCard4::new().with_full_name("Bob Builder"),
    )
    .await;
    let id = StanzaId::new("room-issued-id", parse_jid(ROOM));
    archive_room_message(&state, parse_jid("park@muc.example.com/bob"), &id).await;

    let names = resolve_room(&state, &parse_jid("park@muc.example.com/other"), id).await;
    assert_eq!(
        names.room.as_ref().map(|name| name.as_str()),
        Some("The Park")
    );
    assert!(names.sender.is_none());
}
