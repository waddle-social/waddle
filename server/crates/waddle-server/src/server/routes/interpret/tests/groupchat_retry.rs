use super::room_subject::{
    persist_subject_event, spawn_subject_mutation_test_room, subject_change_message,
};
use super::*;

fn groupchat_retry_message(
    room: &jid::BareJid,
    occupant: &jid::FullJid,
    archive_id: &str,
    origin_id: &str,
) -> xmpp_parsers::message::Message {
    use waddle_xmpp_core::xep0359::{build_origin_id_element, build_stanza_id_element};

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room.clone())));
    message.from = Some(jid::Jid::from(occupant.clone()));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "retry me".to_string());
    message.payloads.push(build_origin_id_element(origin_id));
    message.payloads.push(build_stanza_id_element(
        archive_id,
        &jid::Jid::from(room.clone()),
    ));
    message
}

fn groupchat_retry_sender_item(
    real_jid: &jid::FullJid,
) -> waddle_xmpp_core::mam::ArchivedMucSender {
    waddle_xmpp_core::mam::ArchivedMucSender {
        jid: jid::Jid::from(real_jid.clone()),
        affiliation: waddle_xmpp_core::types::Affiliation::Member,
        role: waddle_xmpp_core::types::Role::Participant,
    }
}

fn archived_groupchat_retry_fixture(
    room: &jid::BareJid,
    occupant: &jid::FullJid,
    real_jid: &jid::FullJid,
    archive_id: &str,
    origin_id: &str,
) -> waddle_xmpp::mam::ArchivedMessage {
    use waddle_xmpp_core::mam::ArchivedRichMessage;
    use waddle_xmpp_core::xep0359::OriginId;

    waddle_xmpp::mam::ArchivedMessage {
        id: archive_id.to_string(),
        body: Some("retry me".to_string()),
        origin_id: Some(OriginId::new(origin_id)),
        message_type: xmpp_parsers::message::MessageType::Groupchat,
        nickname_generation: Some(7),
        rich: Some(ArchivedRichMessage {
            muc_sender: Some(groupchat_retry_sender_item(real_jid)),
            ..ArchivedRichMessage::default()
        }),
        ..waddle_xmpp::mam::ArchivedMessage::for_test(
            jid::Jid::from(occupant.clone()),
            jid::Jid::from(room.clone()),
        )
    }
}

fn groupchat_retry_batch(
    room: &jid::BareJid,
    sender: &jid::FullJid,
    other: &jid::FullJid,
    message: &xmpp_parsers::message::Message,
) -> Vec<OutboundEvent> {
    let mut sender_reflection = message.clone();
    sender_reflection.to = Some(jid::Jid::from(sender.clone()));
    let mut other_reflection = message.clone();
    other_reflection.to = Some(jid::Jid::from(other.clone()));

    vec![
        OutboundEvent::ArchiveGroupchat {
            room: room.clone(),
            sender: sender.clone(),
            message: Box::new(message.clone()),
            sender_nickname_generation: 8,
            sender_item: Some(groupchat_retry_sender_item(sender)),
        },
        OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(sender.clone()),
            stanza: Box::new(Stanza::Message(sender_reflection)),
            call_setup: None,
        },
        OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(other.clone()),
            stanza: Box::new(Stanza::Message(other_reflection)),
            call_setup: None,
        },
        OutboundEvent::ProjectGroupchatInbox {
            owner: sender.to_bare(),
            room: room.clone(),
            message: Box::new(message.clone()),
            is_recipient: false,
            is_durable_recipient: false,
            is_live_occupant: true,
            room_members_only: true,
            sender_can_broadcast_channel_mention: false,
            thread: None,
            dispatch_timestamp: 1_752_768_000,
        },
        OutboundEvent::ProjectGroupchatInbox {
            owner: other.to_bare(),
            room: room.clone(),
            message: Box::new(message.clone()),
            is_recipient: true,
            is_durable_recipient: true,
            is_live_occupant: true,
            room_members_only: true,
            sender_can_broadcast_channel_mention: false,
            thread: None,
            dispatch_timestamp: 1_752_768_000,
        },
        OutboundEvent::SendStanza(Box::new(Stanza::Message(message.clone()))),
    ]
}

#[test]
fn archive_id_rewrite_updates_groupchat_inbox_projection_message() {
    use waddle_xmpp_core::xep0359::extract_stanza_id_by;

    let room: jid::BareJid = "room@conference.example.com".parse().expect("room JID");
    let occupant: jid::FullJid = "room@conference.example.com/alice"
        .parse()
        .expect("occupant JID");
    let owner: jid::BareJid = "alice@example.com".parse().expect("owner JID");
    let mut event = OutboundEvent::ProjectGroupchatInbox {
        owner,
        room: room.clone(),
        message: Box::new(groupchat_retry_message(
            &room,
            &occupant,
            "fresh-retry-id",
            "stable-origin",
        )),
        is_recipient: false,
        is_durable_recipient: false,
        is_live_occupant: true,
        room_members_only: true,
        sender_can_broadcast_channel_mention: false,
        thread: None,
        dispatch_timestamp: 1_752_768_000,
    };
    let rewrite = ArchiveIdRewrite::from_store_result(
        jid::Jid::from(room.clone()),
        "fresh-retry-id".to_string(),
        "original-archive-id".to_string(),
    )
    .expect("different archive ids produce a rewrite");

    apply_archive_id_rewrites(&mut event, &[rewrite]);

    let OutboundEvent::ProjectGroupchatInbox { message, .. } = event else {
        panic!("expected groupchat inbox projection");
    };
    assert_eq!(
        extract_stanza_id_by(&message, &jid::Jid::from(room)).as_deref(),
        Some("original-archive-id")
    );
}

#[tokio::test]
async fn groupchat_origin_retry_suppresses_non_sender_fanout_and_rewrites_sender_copy() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::{InMemoryMamStorage, StoreOutcome};
    use waddle_xmpp::registry::{DeliveryKind, UserRegistryActor};
    use waddle_xmpp_core::xep0359::extract_stanza_id_by;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let sender: jid::FullJid = "alice@example.com/new-session".parse().expect("sender JID");
    let sender_second: jid::FullJid = "alice@example.com/second-session"
        .parse()
        .expect("sender JID");
    let old_sender: jid::FullJid = "alice@example.com/old-session".parse().expect("sender JID");
    let other: jid::FullJid = "bob@example.com/phone".parse().expect("other JID");
    let occupant: jid::FullJid = "room@conference.example.com/alice"
        .parse()
        .expect("occupant JID");
    let room = occupant.to_bare();
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
    let (sender_second_tx, mut sender_second_rx) = tokio::sync::mpsc::channel(8);
    let (other_tx, mut other_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &sender, sender_tx).await;
    register_into_both_tiers(&registry, &user_registry, &sender_second, sender_second_tx).await;
    register_into_both_tiers(&registry, &user_registry, &other, other_tx).await;

    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let mut deps = Deps::test_with_storage(&registry, &mam, &inbox);
    deps.user_registry = Some(&user_registry);
    let original_id = "original-room-archive-id";
    let origin_id = "stable-groupchat-origin";
    assert_eq!(
        mam_concrete
            .store_message(
                &room,
                &archived_groupchat_retry_fixture(
                    &room,
                    &occupant,
                    &old_sender,
                    original_id,
                    origin_id,
                ),
            )
            .await
            .expect("seed original row"),
        StoreOutcome::Stored(original_id.to_string())
    );
    let retry = groupchat_retry_message(&room, &occupant, "fresh-retry-id", origin_id);
    let mut second_reflection = retry.clone();
    second_reflection.to = Some(jid::Jid::from(sender_second.clone()));
    let mut batch = groupchat_retry_batch(&room, &sender, &other, &retry);
    batch.insert(
        2,
        OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(sender_second),
            stanza: Box::new(Stanza::Message(second_reflection)),
            call_setup: None,
        },
    );

    let outcome = interpret(batch, &deps).await;

    assert!(outcome.frames.is_empty(), "dedupe emits no error frame");
    assert_eq!(
        outcome.retry_suppression,
        Some(GroupchatRetrySuppression::Deduplicated)
    );
    assert_eq!(
        mam_concrete.count_messages(&room).await.expect("count"),
        1,
        "dedupe must not create a second room archive row"
    );
    assert!(
        drain_inbound(&mut other_rx).is_empty(),
        "non-sender reflection and inbox push must both be suppressed"
    );
    let sender_deliveries = drain_inbound(&mut sender_rx);
    assert_eq!(sender_deliveries.len(), 1, "sender reflection survives");
    assert_eq!(sender_deliveries[0].kind, DeliveryKind::PeerStanza);
    let Stanza::Message(sender_copy) = &sender_deliveries[0].stanza else {
        panic!("expected sender message reflection");
    };
    assert_eq!(
        extract_stanza_id_by(sender_copy, &jid::Jid::from(room.clone())).as_deref(),
        Some(original_id),
        "sender reflection must use the retained archive stanza-id"
    );
    let second_sender_deliveries = drain_inbound(&mut sender_second_rx);
    assert_eq!(
        second_sender_deliveries.len(),
        1,
        "every session of the sender's bare JID receives the reflection"
    );
    let Stanza::Message(second_sender_copy) = &second_sender_deliveries[0].stanza else {
        panic!("expected second sender message reflection");
    };
    assert_eq!(
        extract_stanza_id_by(second_sender_copy, &jid::Jid::from(room.clone())).as_deref(),
        Some(original_id)
    );
    let sender_inbox = inbox_concrete
        .list(&sender.to_bare())
        .await
        .expect("sender inbox");
    assert_eq!(sender_inbox.len(), 1, "sender inbox projection survives");
    assert_eq!(sender_inbox[0].preview.as_deref(), Some("retry me"));
    assert!(
        inbox_concrete
            .list(&other.to_bare())
            .await
            .expect("other inbox")
            .is_empty(),
        "non-sender inbox projection must be suppressed"
    );
}

#[tokio::test]
async fn groupchat_origin_retry_after_tombstone_silently_suppresses_entire_batch() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::{ArchivedTombstone, InMemoryMamStorage};
    use waddle_xmpp::registry::UserRegistryActor;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let sender: jid::FullJid = "alice@example.com/new-session".parse().expect("sender JID");
    let old_sender: jid::FullJid = "alice@example.com/old-session".parse().expect("sender JID");
    let other: jid::FullJid = "bob@example.com/phone".parse().expect("other JID");
    let occupant: jid::FullJid = "room@conference.example.com/alice"
        .parse()
        .expect("occupant JID");
    let room = occupant.to_bare();
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
    let (other_tx, mut other_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &sender, sender_tx).await;
    register_into_both_tiers(&registry, &user_registry, &other, other_tx).await;

    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let mut deps = Deps::test_with_storage(&registry, &mam, &inbox);
    deps.user_registry = Some(&user_registry);
    let original_id = "retracted-room-archive-id";
    let origin_id = "retracted-groupchat-origin";
    mam_concrete
        .store_message(
            &room,
            &archived_groupchat_retry_fixture(
                &room,
                &occupant,
                &old_sender,
                original_id,
                origin_id,
            ),
        )
        .await
        .expect("seed original row");
    assert!(mam_concrete
        .replace_with_tombstone(
            original_id,
            ArchivedTombstone {
                retraction_id: None,
                stamp: chrono::Utc::now(),
                moderation: None,
                sender_scope: None,
            },
        )
        .await
        .expect("replace with tombstone"));
    let retry = groupchat_retry_message(&room, &occupant, "fresh-retry-id", origin_id);

    let outcome = interpret(groupchat_retry_batch(&room, &sender, &other, &retry), &deps).await;

    assert_eq!(
        outcome.retry_suppression,
        Some(GroupchatRetrySuppression::TombstoneSwallowed)
    );
    assert!(
        outcome.frames.is_empty(),
        "tombstone hit is a silent swallow"
    );
    assert_eq!(
        mam_concrete.count_messages(&room).await.expect("count"),
        1,
        "tombstone retry must not create a new archive row"
    );
    assert!(drain_inbound(&mut sender_rx).is_empty());
    assert!(drain_inbound(&mut other_rx).is_empty());
    assert!(inbox_concrete
        .list(&sender.to_bare())
        .await
        .expect("sender inbox")
        .is_empty());
    assert!(inbox_concrete
        .list(&other.to_bare())
        .await
        .expect("other inbox")
        .is_empty());
}

#[tokio::test]
async fn groupchat_retraction_retry_finishes_tombstone_after_archive_deduplication() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::{
        ArchivedMessage, ArchivedRichPayload, InMemoryMamStorage, StoreOutcome,
    };
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp_core::xep0359::{build_origin_id_element, build_stanza_id_element, OriginId};

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let sender: jid::FullJid = "alice@example.com/new-session".parse().expect("sender JID");
    let other: jid::FullJid = "bob@example.com/phone".parse().expect("other JID");
    let occupant: jid::FullJid = "room@conference.example.com/alice"
        .parse()
        .expect("occupant JID");
    let room = occupant.to_bare();
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
    let (other_tx, mut other_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &sender, sender_tx).await;
    register_into_both_tiers(&registry, &user_registry, &other, other_tx).await;

    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let mut deps = Deps::test_with_storage(&registry, &mam, &inbox);
    deps.user_registry = Some(&user_registry);
    let target_id = "target-room-stanza-id";
    seed_groupchat_archive_row(&mam, &room, target_id, "target-wire-id").await;

    let origin_id = "stable-retraction-origin";
    let original_retraction_id = "original-retraction-archive-id";
    let mut retraction = Message::new(Some(jid::Jid::from(room.clone())));
    retraction.from = Some(jid::Jid::from(occupant.clone()));
    retraction.type_ = XmppMessageType::Groupchat;
    retraction.id = Some(xmpp_parsers::message::Id("retraction-wire-id".to_string()));
    retraction
        .payloads
        .push(waddle_xmpp::xep::xep0424::build_retract_element(target_id));
    retraction.payloads.push(build_origin_id_element(origin_id));
    retraction.payloads.push(build_stanza_id_element(
        "fresh-retraction-archive-id",
        &jid::Jid::from(room.clone()),
    ));
    let sender_item = groupchat_retry_sender_item(&sender);
    let archived_retraction = ArchivedMessage {
        id: original_retraction_id.to_string(),
        body: None,
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "retraction-wire-id",
            jid::Jid::from(room.clone()),
        )),
        origin_id: Some(OriginId::new(origin_id)),
        message_type: XmppMessageType::Groupchat,
        rich: groupchat_archive::rich_archive_payload(&retraction, Some(&sender_item)),
        nickname_generation: Some(7),
        ..ArchivedMessage::for_test(
            jid::Jid::from(occupant.clone()),
            jid::Jid::from(room.clone()),
        )
    };
    assert!(matches!(
        archived_retraction
            .rich
            .as_ref()
            .and_then(|rich| rich.payload.as_ref()),
        Some(ArchivedRichPayload::Retraction(_))
    ));
    assert_eq!(
        mam_concrete
            .store_message(&room, &archived_retraction)
            .await
            .expect("seed archived retraction request"),
        StoreOutcome::Stored(original_retraction_id.to_string())
    );

    let mut other_reflection = retraction.clone();
    other_reflection.to = Some(jid::Jid::from(other.clone()));
    let mut sender_reflection = retraction.clone();
    sender_reflection.to = Some(jid::Jid::from(sender.clone()));
    let outcome = interpret(
        vec![
            OutboundEvent::ArchiveGroupchat {
                room: room.clone(),
                sender: sender.clone(),
                message: Box::new(retraction.clone()),
                sender_nickname_generation: 8,
                sender_item: Some(sender_item),
            },
            OutboundEvent::ApplyGroupchatRetractionTombstone {
                room: room.clone(),
                target_message_id: target_id.to_string(),
                retraction_message: Box::new(retraction),
            },
            OutboundEvent::RouteToConnection {
                jid: jid::Jid::from(other),
                stanza: Box::new(Stanza::Message(other_reflection)),
                call_setup: None,
            },
            OutboundEvent::RouteToConnection {
                jid: jid::Jid::from(sender),
                stanza: Box::new(Stanza::Message(sender_reflection)),
                call_setup: None,
            },
        ],
        &deps,
    )
    .await;

    assert_eq!(
        outcome.retry_suppression,
        Some(GroupchatRetrySuppression::Deduplicated)
    );
    let target = mam_concrete
        .get_message(target_id)
        .await
        .expect("target lookup")
        .expect("target remains as tombstone row");
    assert!(matches!(
        target.rich.as_ref().and_then(|rich| rich.payload.as_ref()),
        Some(ArchivedRichPayload::Tombstone(_))
    ));
    assert!(drain_inbound(&mut other_rx).is_empty());
    assert_eq!(drain_inbound(&mut sender_rx).len(), 1);
}

#[tokio::test]
async fn tombstoned_retraction_retry_still_heals_target_tombstone_silently() {
    // The pathological double-crash window (Greptile review on PR #1412):
    // the retraction-request row was archived, the process died before the
    // target tombstone applied, and the request row itself was later
    // tombstoned. The retry then hits TombstoneSwallow — which must still
    // let the terminal-guarded ApplyGroupchatRetractionTombstone heal the
    // live target while delivering nothing to anyone.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::{
        ArchivedMessage, ArchivedRichPayload, ArchivedTombstone, InMemoryMamStorage, StoreOutcome,
    };
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp_core::xep0359::{build_origin_id_element, build_stanza_id_element, OriginId};

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let sender: jid::FullJid = "alice@example.com/new-session".parse().expect("sender JID");
    let other: jid::FullJid = "bob@example.com/phone".parse().expect("other JID");
    let occupant: jid::FullJid = "room@conference.example.com/alice"
        .parse()
        .expect("occupant JID");
    let room = occupant.to_bare();
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
    let (other_tx, mut other_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &sender, sender_tx).await;
    register_into_both_tiers(&registry, &user_registry, &other, other_tx).await;

    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let mut deps = Deps::test_with_storage(&registry, &mam, &inbox);
    deps.user_registry = Some(&user_registry);
    let target_id = "swallow-target-stanza-id";
    seed_groupchat_archive_row(&mam, &room, target_id, "swallow-target-wire-id").await;

    let origin_id = "swallowed-retraction-origin";
    let original_retraction_id = "swallowed-retraction-archive-id";
    let mut retraction = Message::new(Some(jid::Jid::from(room.clone())));
    retraction.from = Some(jid::Jid::from(occupant.clone()));
    retraction.type_ = XmppMessageType::Groupchat;
    retraction.id = Some(xmpp_parsers::message::Id(
        "swallow-retraction-wire-id".to_string(),
    ));
    retraction
        .payloads
        .push(waddle_xmpp::xep::xep0424::build_retract_element(target_id));
    retraction.payloads.push(build_origin_id_element(origin_id));
    retraction.payloads.push(build_stanza_id_element(
        "fresh-swallow-retraction-archive-id",
        &jid::Jid::from(room.clone()),
    ));
    let sender_item = groupchat_retry_sender_item(&sender);
    let archived_retraction = ArchivedMessage {
        id: original_retraction_id.to_string(),
        body: None,
        origin_id: Some(OriginId::new(origin_id)),
        message_type: XmppMessageType::Groupchat,
        rich: groupchat_archive::rich_archive_payload(&retraction, Some(&sender_item)),
        nickname_generation: Some(7),
        ..ArchivedMessage::for_test(
            jid::Jid::from(occupant.clone()),
            jid::Jid::from(room.clone()),
        )
    };
    assert_eq!(
        mam_concrete
            .store_message(&room, &archived_retraction)
            .await
            .expect("seed archived retraction request"),
        StoreOutcome::Stored(original_retraction_id.to_string())
    );
    assert!(mam_concrete
        .replace_with_tombstone(
            original_retraction_id,
            ArchivedTombstone {
                retraction_id: None,
                stamp: chrono::Utc::now(),
                moderation: None,
                sender_scope: Some(sender.to_bare()),
            },
        )
        .await
        .expect("tombstone the retraction-request row itself"));

    let mut other_reflection = retraction.clone();
    other_reflection.to = Some(jid::Jid::from(other.clone()));
    let mut sender_reflection = retraction.clone();
    sender_reflection.to = Some(jid::Jid::from(sender.clone()));
    let outcome = interpret(
        vec![
            OutboundEvent::ArchiveGroupchat {
                room: room.clone(),
                sender: sender.clone(),
                message: Box::new(retraction.clone()),
                sender_nickname_generation: 8,
                sender_item: Some(sender_item),
            },
            OutboundEvent::ApplyGroupchatRetractionTombstone {
                room: room.clone(),
                target_message_id: target_id.to_string(),
                retraction_message: Box::new(retraction),
            },
            OutboundEvent::RouteToConnection {
                jid: jid::Jid::from(other),
                stanza: Box::new(Stanza::Message(other_reflection)),
                call_setup: None,
            },
            OutboundEvent::RouteToConnection {
                jid: jid::Jid::from(sender),
                stanza: Box::new(Stanza::Message(sender_reflection)),
                call_setup: None,
            },
        ],
        &deps,
    )
    .await;

    assert_eq!(
        outcome.retry_suppression,
        Some(GroupchatRetrySuppression::TombstoneSwallowed)
    );
    assert!(outcome.frames.is_empty(), "swallow emits no error frame");
    let target = mam_concrete
        .get_message(target_id)
        .await
        .expect("target lookup")
        .expect("target row exists");
    assert!(
        matches!(
            target.rich.as_ref().and_then(|rich| rich.payload.as_ref()),
            Some(ArchivedRichPayload::Tombstone(_))
        ),
        "the heal event must still tombstone the live target"
    );
    assert!(drain_inbound(&mut other_rx).is_empty());
    assert!(
        drain_inbound(&mut sender_rx).is_empty(),
        "tombstone swallow delivers nothing, even to the sender"
    );
}

#[tokio::test]
async fn fresh_groupchat_origin_fans_out_to_every_recipient() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::InMemoryMamStorage;
    use waddle_xmpp::registry::UserRegistryActor;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let sender: jid::FullJid = "alice@example.com/session".parse().expect("sender JID");
    let other: jid::FullJid = "bob@example.com/phone".parse().expect("other JID");
    let occupant: jid::FullJid = "room@conference.example.com/alice"
        .parse()
        .expect("occupant JID");
    let room = occupant.to_bare();
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
    let (other_tx, mut other_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &sender, sender_tx).await;
    register_into_both_tiers(&registry, &user_registry, &other, other_tx).await;

    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let mut deps = Deps::test_with_storage(&registry, &mam, &inbox);
    deps.user_registry = Some(&user_registry);
    let fresh_id = "fresh-room-archive-id";
    let message = groupchat_retry_message(&room, &occupant, fresh_id, "fresh-origin");

    let outcome = interpret(
        groupchat_retry_batch(&room, &sender, &other, &message),
        &deps,
    )
    .await;

    assert_eq!(outcome.retry_suppression, None);
    assert_eq!(
        outcome.frames.len(),
        1,
        "fresh canary event is not suppressed"
    );
    assert_eq!(mam_concrete.count_messages(&room).await.expect("count"), 1);
    assert_eq!(drain_inbound(&mut sender_rx).len(), 1);
    assert!(
        drain_inbound(&mut other_rx)
            .iter()
            .any(|delivery| matches!(delivery.stanza, Stanza::Message(ref message) if message.type_ == xmpp_parsers::message::MessageType::Groupchat)),
        "non-sender receives the groupchat reflection"
    );
    let sender_inbox = inbox_concrete
        .list(&sender.to_bare())
        .await
        .expect("sender inbox");
    let other_inbox = inbox_concrete
        .list(&other.to_bare())
        .await
        .expect("other inbox");
    assert_eq!(sender_inbox.len(), 1);
    assert_eq!(other_inbox.len(), 1);
}

#[tokio::test]
async fn groupchat_subject_retry_is_stored_and_fans_out_normally() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage, StoreOutcome};
    use waddle_xmpp::muc::room_actor::GetSnapshot;
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp_core::xep0359::{build_origin_id_element, build_stanza_id_element, OriginId};

    let (room_registry, room_actor, room, _claim_store, claim_fence, _store) =
        spawn_subject_mutation_test_room().await;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let recipient: jid::FullJid = "bob@example.com/phone".parse().expect("recipient JID");
    let (recipient_tx, mut recipient_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &recipient, recipient_tx).await;
    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let mut deps = Deps::test_with_storage(&registry, &mam, &inbox);
    deps.room_registry = Some(&room_registry);
    deps.user_registry = Some(&user_registry);

    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender JID");
    let occupant: jid::FullJid = format!("{room}/alice-nick").parse().expect("occupant JID");
    let sender_item = groupchat_retry_sender_item(&sender);
    let origin_id = "stable-subject-origin";
    let subject = "Do not resurrect this subject";
    let mut retry = subject_change_message(&room, &sender, subject);
    retry.from = Some(jid::Jid::from(occupant.clone()));
    retry.id = Some(xmpp_parsers::message::Id("subject-wire-id".to_string()));
    retry.payloads.push(build_origin_id_element(origin_id));
    retry.payloads.push(build_stanza_id_element(
        "fresh-subject-archive-id",
        &jid::Jid::from(room.clone()),
    ));
    let archived = ArchivedMessage {
        id: "original-subject-archive-id".to_string(),
        body: None,
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "subject-wire-id",
            jid::Jid::from(room.clone()),
        )),
        origin_id: Some(OriginId::new(origin_id)),
        message_type: XmppMessageType::Groupchat,
        rich: groupchat_archive::rich_archive_payload(&retry, Some(&sender_item)),
        nickname_generation: Some(7),
        ..ArchivedMessage::for_test(jid::Jid::from(occupant), jid::Jid::from(room.clone()))
    };
    assert_eq!(
        mam_concrete
            .store_message(&room, &archived)
            .await
            .expect("seed subject row"),
        StoreOutcome::Stored("original-subject-archive-id".to_string())
    );
    let mut recipient_reflection = retry.clone();
    recipient_reflection.to = Some(jid::Jid::from(recipient.clone()));

    let events = super::super::room_dispatch::order_subject_persistence_after_archive(vec![
        persist_subject_event(&room, &sender, subject, claim_fence),
        OutboundEvent::ArchiveGroupchat {
            room: room.clone(),
            sender: sender.clone(),
            message: Box::new(retry),
            sender_nickname_generation: 8,
            sender_item: Some(sender_item),
        },
        OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(recipient),
            stanza: Box::new(Stanza::Message(recipient_reflection)),
            call_setup: None,
        },
        OutboundEvent::CloseTransport,
    ]);
    let outcome = interpret(events, &deps).await;

    assert_eq!(outcome.retry_suppression, None);
    assert!(outcome.frames.is_empty());
    assert!(outcome.close, "post-subject canary must not be suppressed");
    assert_eq!(
        mam_concrete.count_messages(&room).await.expect("count"),
        2,
        "room-state subject retries remain outside timeline retry dedupe"
    );
    let deliveries = drain_inbound(&mut recipient_rx);
    assert_eq!(deliveries.len(), 1, "subject retry fans out normally");
    let Stanza::Message(reflection) = &deliveries[0].stanza else {
        panic!("expected subject reflection");
    };
    assert_eq!(
        reflection.subjects.get("").map(String::as_str),
        Some(subject)
    );
    let stored_subject = room_actor
        .ask(GetSnapshot)
        .await
        .expect("room snapshot")
        .room
        .subject
        .expect("subject retry applies room state");
    assert_eq!(stored_subject.texts.get(""), Some(subject));
    assert_eq!(stored_subject.setter, sender.to_bare());
}
