use super::*;
use crate::ingress_shadow::IngressEffectCapture;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use waddle_xmpp::inbox::storage::InboxStorageError;
use waddle_xmpp::inbox::InboxEntry;
use waddle_xmpp::pending_delivery::storage::{
    InMemoryPendingDeliveryStorage, PendingDeliveryStorage,
};
use waddle_xmpp::xep::CallThreadDuration;
use waddle_xmpp_core::xep0359::{build_stanza_id_element, StanzaId as XepStanzaId};

fn capture_snapshot(
    capture: &IngressEffectCapture,
) -> crate::ingress_shadow::IngressEffectCaptureSnapshot {
    capture.snapshot()
}

#[tokio::test]
async fn direct_send_stanza_captures_serialized_xep_0191_block_error() {
    let registry = ConnectionRegistry::new();
    let capture = IngressEffectCapture::new(None);
    let deps = Deps::registry_only(&registry).with_ingress_effect_capture(Some(capture.clone()));
    let original = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com/mobile"),
        "blocked message",
    );
    let reply = waddle_xmpp::protocol::handlers::errors::outgoing_block_error_reply(&original);

    let outcome = interpret(
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Message(reply)))],
        &deps,
    )
    .await;

    assert_eq!(
        outcome.frames.len(),
        1,
        "the reply must reach the wire first"
    );
    assert!(capture_snapshot(&capture)
        .intents
        .iter()
        .any(|intent| matches!(
            intent,
            IngressEffectIntent::ErrorReply { recipient, error }
                if recipient.as_str() == "alice@example.com/web"
                    && matches!(
                        error.condition_payload,
                        Some(waddle_xmpp::ingress::FrozenStanzaErrorConditionPayload::Blocked)
                    )
        )));
}

struct FailingInboxStorage;

#[async_trait]
impl InboxStorage for FailingInboxStorage {
    async fn list(&self, _user: &jid::BareJid) -> Result<Vec<InboxEntry>, InboxStorageError> {
        Ok(Vec::new())
    }

    async fn list_threads(
        &self,
        _user: &jid::BareJid,
        _room: &jid::BareJid,
    ) -> Result<Vec<InboxEntry>, InboxStorageError> {
        Ok(Vec::new())
    }

    async fn upsert(
        &self,
        _user: &jid::BareJid,
        _entry: InboxEntry,
        _increment_unread: bool,
    ) -> Result<InboxEntry, InboxStorageError> {
        Err(InboxStorageError::Other(
            "forced upsert failure".to_string(),
        ))
    }

    async fn mark_read(
        &self,
        _user: &jid::BareJid,
        _partner: &jid::BareJid,
        _thread_id: Option<&str>,
    ) -> Result<Option<InboxEntry>, InboxStorageError> {
        Ok(None)
    }

    async fn total_unread(&self, _user: &jid::BareJid) -> Result<u64, InboxStorageError> {
        Ok(0)
    }

    async fn mark_call_thread_ended(
        &self,
        _room: &jid::BareJid,
        _thread_id: &str,
        _ended: DateTime<Utc>,
        _duration: &CallThreadDuration,
    ) -> Result<(), InboxStorageError> {
        Ok(())
    }

    async fn mark_direct_call_thread_ended(
        &self,
        _user: &jid::BareJid,
        _partner: &jid::BareJid,
        _thread_id: &str,
        _ended: DateTime<Utc>,
        _duration: &CallThreadDuration,
    ) -> Result<(), InboxStorageError> {
        Ok(())
    }
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
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::InboxProject {
            owner: intent_owner,
            mutation: waddle_xmpp::ingress::InboxProjectionMutation::Direct {
                entry,
                increment_unread: true,
            },
        } if *intent_owner == owner
            && entry.partner == "bob@example.com".parse::<jid::BareJid>().expect("peer")
            && entry.last_stanza_id == "archive-1"
            && entry.preview.as_deref() == Some("hi")
            && entry.unread == 1
    )));
}

#[tokio::test]
async fn direct_inbox_boundary_skips_inbox_project_intent_when_upsert_fails() {
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(FailingInboxStorage);
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
    assert!(
        !snapshot.intents.iter().any(|intent| matches!(
            intent,
            IngressEffectIntent::InboxProject { owner: intent_owner, .. }
                if *intent_owner == owner
        )),
        "failed direct inbox projection must not record InboxProject",
    );
}

#[tokio::test]
async fn displayed_marker_boundary_records_read_timestamp_and_push_route() {
    use waddle_xmpp::inbox::ConversationKind;
    use waddle_xmpp::mam::ArchivedMessage;
    use xmpp_parsers::message::MessageType;

    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let registry = state.deps.protocol.connection_registry.as_ref();
    let user_registry = &state.deps.protocol.user_registry;
    let mam = Arc::clone(&state.deps.protocol.mam_storage);
    let inbox = Arc::clone(&state.deps.protocol.inbox_storage);
    let capture = IngressEffectCapture::new(None);
    let deps = Deps::test_with_storage(registry, &mam, &inbox)
        .with_ingress_effect_capture(Some(capture.clone()));
    let mut deps = deps;
    deps.user_registry = Some(user_registry);
    deps.web_socket_state = Some(state.as_ref());

    let owner: jid::BareJid = "alice@example.com".parse().expect("owner");
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    let owner_phone: jid::FullJid = "alice@example.com/phone".parse().expect("resource");
    let (owner_phone_tx, _owner_phone_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(registry, user_registry, &owner_phone, owner_phone_tx).await;

    inbox
        .upsert(
            &owner,
            InboxEntry::new(room.clone(), ConversationKind::MucRoom, "seed-1", 1).with_unread(1),
            true,
        )
        .await
        .expect("seed unread inbox row");
    mam.store_message(
        &room,
        &ArchivedMessage {
            id: "msg-1".to_string(),
            body: Some("hello".to_string()),
            message_type: MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                "room@conference.example.com/alice"
                    .parse()
                    .expect("occupant"),
                jid::Jid::from(room.clone()),
            )
        },
    )
    .await
    .expect("seed displayed target");

    let _ = interpret(
        vec![OutboundEvent::MarkInboxReadFromDisplayed {
            owner: owner.clone(),
            room: room.clone(),
            displayed_message_id: "msg-1".to_string(),
        }],
        &deps,
    )
    .await;

    let snapshot = capture_snapshot(&capture);
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::NotificationActivityPreview {
            owner: intent_owner,
            mutation: waddle_xmpp::ingress::NotificationActivityMutation::ReadMarker {
                conversation,
                committed_at_ms,
            },
        } if *intent_owner == owner && *conversation == room && *committed_at_ms > 0
    )));
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::InboxProject {
            owner: intent_owner,
            mutation: waddle_xmpp::ingress::InboxProjectionMutation::GroupchatChannelRead {
                room: intent_room,
            },
        } if *intent_owner == owner && *intent_room == room
    )));
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::RouteDirect {
            recipient,
            fanout,
            ..
        } if *recipient == owner && *fanout == vec![owner_phone.clone()]
    )));
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
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::InboxProject { owner: intent_owner, .. }
            if *intent_owner == owner
    )));
    assert!(
        !snapshot.intents.iter().any(|intent| matches!(
            intent,
            IngressEffectIntent::NotificationActivityPreview { owner: intent_owner, .. }
                if *intent_owner == owner
        )),
        "candidate retry without websocket state must not capture preview intent",
    );
}

#[tokio::test]
async fn groupchat_inbox_boundary_records_notification_intent_after_candidate_acceptance() {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let registry = state.deps.protocol.connection_registry.as_ref();
    let mam = Arc::clone(&state.deps.protocol.mam_storage);
    let inbox = Arc::clone(&state.deps.protocol.inbox_storage);
    let capture = IngressEffectCapture::new(None);
    let deps = Deps::test_with_storage(registry, &mam, &inbox)
        .with_ingress_effect_capture(Some(capture.clone()));
    let mut deps = deps;
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.web_socket_state = Some(state.as_ref());

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
        "room-archive-accepted",
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
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::InboxProject { owner: intent_owner, .. }
            if *intent_owner == owner
    )));
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::NotificationActivityPreview { owner: intent_owner, .. }
            if *intent_owner == owner
    )));
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::GroupchatNotificationRecovery { mutation }
            if mutation.recipient == owner
                && mutation.action
                    == waddle_xmpp::ingress::GroupchatNotificationRecoveryAction::Recorded
    )));
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::GroupchatNotificationRecovery { mutation }
            if mutation.recipient == owner
                && mutation.action
                    == waddle_xmpp::ingress::GroupchatNotificationRecoveryAction::Completed
    )));
}

#[tokio::test]
async fn groupchat_inbox_boundary_skips_notification_intent_when_t0_policy_suppresses_candidate() {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let registry = state.deps.protocol.connection_registry.as_ref();
    let mam = Arc::clone(&state.deps.protocol.mam_storage);
    let inbox = Arc::clone(&state.deps.protocol.inbox_storage);
    let capture = IngressEffectCapture::new(None);
    let deps = Deps::test_with_storage(registry, &mam, &inbox)
        .with_ingress_effect_capture(Some(capture.clone()));
    let mut deps = deps;
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.web_socket_state = Some(state.as_ref());

    let owner: jid::BareJid = "bob@example.com".parse().expect("owner");
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    state
        .deps
        .protocol
        .notification_settings_projection
        .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
            owner_bare_jid: owner.clone(),
            conversation_jid: room.clone(),
            conversation_kind:
                crate::notification_settings_projection::ConversationKind::PrivateGroup,
            mode: waddle_xmpp::xep::NotificationLevel::Never,
            rich_payload_opt_in: false,
            source_version: 1,
            updated_at_ms: 1,
            source: crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: owner.clone(),
        })
        .await
        .expect("seed notification settings");
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
        "room-archive-suppressed",
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
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::InboxProject { owner: intent_owner, .. }
            if *intent_owner == owner
    )));
    assert!(
        !snapshot.intents.iter().any(|intent| matches!(
            intent,
            IngressEffectIntent::NotificationActivityPreview { owner: intent_owner, .. }
                if *intent_owner == owner
        )),
        "T0 policy suppression must leave no preview intent",
    );
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
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::NotificationActivityPreview { owner, .. }
            if *owner == recipient
    )));
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::PendingDelivery {
            mutation: waddle_xmpp::ingress::PendingDeliveryMutation::Archived {
                recipient: intent_recipient,
                ..
            }
        } if *intent_recipient == recipient
    )));
}

#[tokio::test]
async fn transient_offline_delivery_records_pending_delivery_intent() {
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
    let transient = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "transient offline hi",
    );
    let _ = interpret(
        vec![OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(Box::new(
                transient.clone(),
            )),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(transient),
        }],
        &deps,
    )
    .await;

    assert!(capture_snapshot(&capture)
        .intents
        .iter()
        .any(|intent| matches!(
            intent,
            IngressEffectIntent::PendingDelivery {
                mutation: waddle_xmpp::ingress::PendingDeliveryMutation::Transient {
                    recipient: intent_recipient,
                    ..
                }
            } if *intent_recipient == recipient
        )));
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
    assert!(
        !snapshot.intents.iter().any(|intent| matches!(
            intent,
            IngressEffectIntent::NotificationActivityPreview { owner: intent_owner, .. }
                if *intent_owner == owner
        )),
        "missing websocket state must leave no notification-activity preview intent",
    );
}

#[tokio::test]
async fn archive_direct_boundary_records_notification_activity_preview_after_projection_write() {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let registry = state.deps.protocol.connection_registry.as_ref();
    let mam = Arc::clone(&state.deps.protocol.mam_storage);
    let inbox = Arc::clone(&state.deps.protocol.inbox_storage);
    let capture = IngressEffectCapture::new(None);
    let deps = Deps::test_with_storage(registry, &mam, &inbox)
        .with_ingress_effect_capture(Some(capture.clone()));
    let mut deps = deps;
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.web_socket_state = Some(state.as_ref());
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
    assert!(snapshot.intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::NotificationActivityPreview { owner: intent_owner, .. }
            if *intent_owner == owner
    )));
}

#[tokio::test]
async fn direct_archive_capture_uses_the_new_authoritative_id_for_an_origin_retry() {
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

    assert_eq!(
        mam_concrete
            .count_messages(&owner)
            .await
            .expect("count archive"),
        2
    );
    let snapshot = capture_snapshot(&capture);
    let (stanza_id, archived_at) = snapshot
        .intents
        .iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::ArchiveAuthoritative {
                archive,
                stanza_id,
                archived_at,
                ..
            } if archive == &owner => Some((stanza_id, archived_at)),
            _ => None,
        })
        .expect("captured archive identity");
    assert_ne!(stanza_id.id, "authoritative-archive-id");
    let row = mam_concrete
        .get_message(stanza_id.as_str())
        .await
        .expect("read archive")
        .expect("captured row exists");
    assert_eq!(row.timestamp, *archived_at);
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
