use super::*;
use kameo::actor::Spawn;
use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
use waddle_xmpp::muc::{MucRoom, RoomConfig};
use waddle_xmpp::registry::ConnectionRegistry;

#[tokio::test]
async fn correction_authority_is_the_validated_archive_row_with_nickname_continuity() {
    let room: BareJid = "room@muc.example.test".parse().expect("room");
    let nick: Jid = room.with_resource_str("alice").expect("nick").into();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    for id in ["older-room-stanza", "latest-room-stanza"] {
        let mut original = ArchivedMessage::for_test(nick.clone(), room.clone().into());
        original.id = id.into();
        original.stanza_id = Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "reused-wire-id",
            room.clone().into(),
        ));
        original.nickname_generation = Some(7);
        original.message_type = XmppMessageType::Groupchat;
        mam.store_message(&room, &original).await.expect("archive");
    }
    let actor = RoomActor::spawn(RoomActor::new(
        MucRoom::new(room.clone(), "w".into(), "c".into(), RoomConfig::default()),
        waddle_xmpp::xep::xep0421::OccupantIdSecret::new(vec![1; 32]).expect("secret"),
    ));
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&registry);
    deps.mam_storage = Some(&mam);
    let mut correction = Message::new(Some(room.clone().into()));
    correction.from = Some("alice@example.test/phone".parse().expect("sender"));
    correction.type_ = XmppMessageType::Groupchat;
    correction
        .payloads
        .push(waddle_xmpp::xep::xep0308::build_replace_element(
            "reused-wire-id",
        ));
    let target =
        validate_groupchat_rich_targets(&deps, &room, &correction, Some(&nick), &actor, Some(7))
            .await
            .expect("valid correction")
            .expect("validated target");
    assert_eq!(
        target,
        waddle_xmpp_core::xep0359::StanzaId::new("latest-room-stanza", room.clone().into())
    );
    assert!(
        validate_groupchat_rich_targets(&deps, &room, &correction, Some(&nick), &actor, Some(8))
            .await
            .is_err(),
        "leave/rejoin cannot borrow a previous nickname generation"
    );
    let different_nick = room.with_resource_str("other").expect("other nick").into();
    assert!(
        validate_groupchat_rich_targets(
            &deps,
            &room,
            &correction,
            Some(&different_nick),
            &actor,
            Some(7)
        )
        .await
        .is_err(),
        "a different MUC sender cannot acquire correction authority"
    );
}
