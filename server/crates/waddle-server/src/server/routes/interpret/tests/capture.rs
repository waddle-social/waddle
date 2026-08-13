use super::*;
use crate::ingress_shadow::IngressEffectCapture;
use waddle_xmpp::pending_delivery::storage::{
    InMemoryPendingDeliveryStorage, PendingDeliveryStorage,
};
use waddle_xmpp_core::xep0359::{build_stanza_id_element, StanzaId as XepStanzaId};

fn capture_snapshot(
    capture: &IngressEffectCapture,
) -> crate::ingress_shadow::IngressEffectCaptureSnapshot {
    capture.snapshot()
}

#[tokio::test]
async fn direct_inbox_boundary_records_inbox_project_intent() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let capture = IngressEffectCapture::new(None);
    let deps = Deps::test_with_storage(&registry, &mam, &inbox)
        .with_ingress_effect_capture(Some(capture.clone()));

    let owner: jid::BareJid = "alice@example.com".parse().expect("owner");
    let peer: jid::BareJid = "bob@example.com".parse().expect("peer");
    let archive_ref = XepStanzaId::new("archive-1", jid::Jid::from(owner.clone()));
    let _ = interpret(
        vec![OutboundEvent::ProjectInbox {
            owner: owner.clone(),
            peer,
            message: Box::new(chat_msg(
                jid("alice@example.com/web"),
                jid("bob@example.com"),
                "hi",
            )),
            archive_ref,
            increment_unread: true,
        }],
        &deps,
    )
    .await;

    let snapshot = capture_snapshot(&capture);
    assert!(snapshot
        .intents
        .contains(&IngressEffectIntent::InboxProject {
            owner,
            increment_unread: true,
        }));
}

#[tokio::test]
async fn groupchat_inbox_boundary_records_inbox_and_notification_intents() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let capture = IngressEffectCapture::new(None);
    let deps = Deps::test_with_storage(&registry, &mam, &inbox)
        .with_ingress_effect_capture(Some(capture.clone()));

    let owner: jid::BareJid = "bob@example.com".parse().expect("owner");
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    let occupant: jid::FullJid = "room@conference.example.com/alice"
        .parse()
        .expect("occupant");
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room.clone())));
    message.from = Some(jid::Jid::from(occupant));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "hello room".to_string());
    message.payloads.push(build_stanza_id_element(
        "room-archive-1",
        &jid::Jid::from(room.clone()),
    ));

    let _ = interpret(
        vec![OutboundEvent::ProjectGroupchatInbox {
            owner: owner.clone(),
            room: room.clone(),
            message: Box::new(message),
            is_recipient: true,
            is_durable_recipient: true,
            is_live_occupant: true,
            room_members_only: true,
            sender_can_broadcast_channel_mention: false,
            thread: None,
            dispatch_timestamp: 1_752_768_000,
        }],
        &deps,
    )
    .await;

    let snapshot = capture_snapshot(&capture);
    assert!(snapshot
        .intents
        .contains(&IngressEffectIntent::InboxProject {
            owner: owner.clone(),
            increment_unread: true,
        }));
    assert!(snapshot
        .intents
        .contains(&IngressEffectIntent::NotificationActivityPreview { owner }));
}

#[tokio::test]
async fn offline_delivery_boundary_records_notification_preview_intent() {
    let registry = ConnectionRegistry::new();
    let pending: Arc<dyn PendingDeliveryStorage> = Arc::new(InMemoryPendingDeliveryStorage::new(
        waddle_xmpp::pending_delivery::QuotaPolicy::default_policy(),
    ));
    let capture = IngressEffectCapture::new(None);
    let deps = Deps {
        connection_registry: &registry,
        user_registry: None,
        sm_session_registry: None,
        mam_storage: None,
        inbox_storage: None,
        extension_manager: None,
        room_registry: None,
        web_socket_state: None,
        authenticated_principal: None,
        local_domain: "example.com",
        blocking_storage: None,
        message_dispatcher: None,
        pending_delivery_storage: Some(&pending),
        ordered_relay_origin: None,
        sfu: None,
        ingress_effect_capture: Some(capture.clone()),
    };
    let recipient: jid::BareJid = "bob@example.com".parse().expect("recipient");
    let archive_jid: jid::BareJid = "bob@example.com".parse().expect("archive");
    let _ = interpret(
        vec![OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(XepStanzaId::new(
                "offline-archive-1",
                jid::Jid::from(archive_jid),
            )),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(chat_msg(
                jid("alice@example.com/web"),
                jid("bob@example.com"),
                "offline hi",
            )),
        }],
        &deps,
    )
    .await;

    let snapshot = capture_snapshot(&capture);
    assert!(snapshot
        .intents
        .contains(&IngressEffectIntent::NotificationActivityPreview { owner: recipient }));
}

#[tokio::test]
async fn archive_direct_boundary_records_notification_activity_preview_intent() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let capture = IngressEffectCapture::new(None);
    let deps = Deps::test_with_storage(&registry, &mam, &inbox)
        .with_ingress_effect_capture(Some(capture.clone()));
    let owner: jid::BareJid = "alice@example.com".parse().expect("owner");

    let _ = interpret(
        vec![OutboundEvent::ArchiveDirect {
            archive_jid: owner.clone(),
            from: jid("alice@example.com/web"),
            to: jid("bob@example.com"),
            message: Box::new(chat_msg(
                jid("alice@example.com/web"),
                jid("bob@example.com"),
                "hello",
            )),
        }],
        &deps,
    )
    .await;

    let snapshot = capture_snapshot(&capture);
    assert!(snapshot
        .intents
        .contains(&IngressEffectIntent::NotificationActivityPreview { owner }));
}

#[tokio::test]
async fn direct_archive_capture_uses_the_deduplicated_authoritative_id() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp_core::xep0359::{build_origin_id_element, OriginId};

    let registry = ConnectionRegistry::new();
    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let capture = IngressEffectCapture::new(None);
    let deps = Deps::test_with_storage(&registry, &mam, &inbox)
        .with_ingress_effect_capture(Some(capture.clone()));
    let owner: jid::BareJid = "alice@example.com".parse().expect("owner");
    let peer: jid::BareJid = "bob@example.com".parse().expect("peer");
    let origin = OriginId::new("retry-origin");

    mam_concrete
        .store_message(
            &owner,
            &ArchivedMessage {
                id: "authoritative-archive-id".to_string(),
                body: Some("hello".to_string()),
                origin_id: Some(origin.clone()),
                message_type: xmpp_parsers::message::MessageType::Chat,
                ..ArchivedMessage::for_test(
                    "alice@example.com/old".parse().expect("sender"),
                    peer.clone().into(),
                )
            },
        )
        .await
        .expect("seed archive row");

    let mut retry = chat_msg(
        jid("alice@example.com/new"),
        jid("bob@example.com"),
        "hello",
    );
    retry.id = Some(xmpp_parsers::message::Id("fresh-retry-id".to_string()));
    retry
        .payloads
        .push(build_origin_id_element(origin.as_str()));
    let _ = interpret(
        vec![OutboundEvent::ArchiveDirect {
            archive_jid: owner.clone(),
            from: jid("alice@example.com/new"),
            to: jid("bob@example.com"),
            message: Box::new(retry),
        }],
        &deps,
    )
    .await;

    assert!(capture_snapshot(&capture).intents.iter().any(|intent| {
        matches!(
            intent,
            IngressEffectIntent::ArchiveAuthoritative { archive, stanza_id, .. }
                if archive == &owner && stanza_id.id == "authoritative-archive-id"
        )
    }));
}

#[tokio::test]
async fn shared_recipient_pass_records_recipient_side_effects_once_across_multi_resource_fanout() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let (desk_tx, _desk_rx) = tokio::sync::mpsc::channel(8);
    let (phone_tx, _phone_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_phone, phone_tx).await;
    registry.update_presence(&bob_desk, true, 5);
    registry.update_presence(&bob_phone, true, 5);

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let capture = IngressEffectCapture::new(None);
    let deps = offline_pass_deps_with_user_registry(
        &registry,
        &user_registry,
        &mam,
        &inbox,
        &blocking,
        &dispatcher,
    )
    .with_ingress_effect_capture(Some(capture.clone()));

    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(chat_msg(
                jid("alice@example.com/web"),
                jid("bob@example.com"),
                "fanout once",
            ))),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    let snapshot = capture_snapshot(&capture);
    let recipient_archive_count = snapshot
        .intents
        .iter()
        .filter(|intent| {
            matches!(
                intent,
                IngressEffectIntent::ArchiveAuthoritative { archive, .. }
                    if archive == &"bob@example.com".parse::<jid::BareJid>().expect("bare")
            )
        })
        .count();
    let recipient_inbox_count = snapshot
        .intents
        .iter()
        .filter(|intent| {
            matches!(
                intent,
                IngressEffectIntent::InboxProject { owner, .. }
                    if owner == &"bob@example.com".parse::<jid::BareJid>().expect("bare")
            )
        })
        .count();

    assert_eq!(recipient_archive_count, 1);
    assert_eq!(recipient_inbox_count, 1);
}
