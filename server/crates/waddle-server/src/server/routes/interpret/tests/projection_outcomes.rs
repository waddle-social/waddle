//! Committed inbox values, rather than Phase-A inputs, determine push contents.
use super::super::effects::{
    room::DurableRoomEffect, AppliedDurableEffects, DurableEffect, DurableOutcome, Effect,
    EffectOutcome, ExternalEffect, ImmediateSink, PlanSink, ProjectionRef,
};
use super::super::groupchat_archive::{project_groupchat_inbox, GroupchatInboxProjectionInputs};
use super::*;
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::protocol::event::GroupchatThreadProjection;
use waddle_xmpp::xep::xep0430::{parse_inbox_entry_with_metadata, NS_INBOX, NS_WADDLE_INBOX};

#[derive(Clone, Copy)]
enum Conversation {
    Fresh,
    Existing,
    Thread,
}

async fn assert_committed_projection_push(database_url: &str, conversation: Conversation) {
    let storage = crate::inbox::DatabaseInboxStorage::open(Some(database_url))
        .await
        .expect("inbox storage");
    let owner: jid::BareJid = format!("projection-{}@example.com", uuid::Uuid::new_v4())
        .parse()
        .expect("owner");
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    if !matches!(conversation, Conversation::Fresh) {
        for timestamp in 1..=3 {
            let mut entry = InboxEntry::new(
                room.clone(),
                ConversationKind::MucRoom,
                "previous",
                timestamp,
            );
            if matches!(conversation, Conversation::Thread) {
                entry = entry
                    .with_thread("thread-root")
                    .with_thread_title("Original title");
            }
            storage
                .upsert(&owner, entry, true)
                .await
                .expect("seed prior messages");
        }
    }
    let registry = test_registry();
    let user_registry = waddle_xmpp::registry::UserRegistryActor::spawn(
        waddle_xmpp::registry::UserRegistryActor::new(),
    );
    let resource = owner.with_resource_str("phone").expect("resource");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &resource, tx).await;
    let sink = PlanSink::new();
    let inbox: Arc<dyn InboxStorage> = Arc::new(storage.clone());
    let deps = Deps {
        inbox_storage: Some(&inbox),
        user_registry: Some(&user_registry),
        effects: &sink,
        ..Deps::registry_only(&registry)
    };
    let mut message = Message::new(Some(room.clone().into()));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.from = Some(
        "room@conference.example.com/alice"
            .parse()
            .expect("occupant"),
    );
    // The client wire id differs from the room-assigned archive identity.
    // Both channel and thread projections must use the trusted room stamp.
    message.id = Some(xmpp_parsers::message::Id("client-wire-id".to_owned()));
    message
        .payloads
        .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
            "committed-message",
            &room.clone().into(),
        ));
    let thread = matches!(conversation, Conversation::Thread).then(|| GroupchatThreadProjection {
        thread_id: "thread-root".into(),
        title: None,
        author_nick: None,
        call_thread_kind: None,
        call_thread_media: None,
    });
    project_groupchat_inbox(GroupchatInboxProjectionInputs {
        deps: &deps,
        owner: &owner,
        room: &room,
        message: &message,
        is_recipient: true,
        thread: &thread,
        dispatch_timestamp: 10,
        notification_recovery: None,
    })
    .await;
    assert!(rx.try_recv().is_err(), "Phase A must not push");
    let plan = sink.snapshot();
    let mut applied = AppliedDurableEffects::default();
    let database = storage.database();
    let mut transaction = database.begin().await.expect("Phase B transaction");
    for (index, effect) in plan.iter().enumerate() {
        if let Effect::Durable(DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
            owner,
            entry,
            is_recipient,
            ..
        })) = &effect.effect
        {
            assert_eq!(entry.last_stanza_id, "committed-message");
            assert_eq!(
                entry.unread, 0,
                "uncommitted input has no authoritative unread count"
            );
            let updated = crate::inbox::upsert_in_transaction(
                &mut transaction,
                owner,
                entry.as_ref().clone(),
                *is_recipient,
            )
            .await
            .expect("apply durable projection");
            applied.insert(ProjectionRef(index), DurableOutcome::Inbox(updated));
        }
    }
    transaction
        .commit()
        .await
        .expect("commit projections before push");
    let mut pushes = 0;
    for effect in plan {
        if let Effect::External(ExternalEffect::Direct(
            super::super::effects::direct::ExternalDirectEffect::PushInboxUpdate {
                projection, ..
            },
        )) = &effect.effect
        {
            assert!(
                !effect.dependencies.is_empty(),
                "push inherits archive dependency"
            );
            assert!(
                applied.inbox(*projection).is_some(),
                "push references an applied projection"
            );
            assert!(matches!(
                ImmediateSink
                    .execute_with_applied(effect, &deps, &applied)
                    .await,
                EffectOutcome::Completed
            ));
            pushes += 1;
        }
    }
    assert_eq!(pushes, if thread.is_some() { 2 } else { 1 });
    for _ in 0..pushes {
        let outbound = rx.try_recv().expect("committed inbox push");
        let Stanza::Message(message) = outbound.stanza else {
            panic!("message push")
        };
        let push = message
            .payloads
            .iter()
            .find(|payload| payload.is("push", NS_WADDLE_INBOX))
            .expect("push wrapper");
        let entry = parse_inbox_entry_with_metadata(
            push.get_child("entry", NS_INBOX).expect("XEP-0430 entry"),
            push.get_child("metadata", NS_WADDLE_INBOX),
        )
        .expect("decode actual pushed contents");
        assert_eq!(entry.last_stanza_id, "committed-message");
        if entry.thread_id.is_some() {
            assert_eq!(entry.unread, 4);
            assert_eq!(entry.reply_count, 3);
            assert_eq!(entry.thread_title.as_deref(), Some("Original title"));
        } else {
            assert_eq!(
                entry.unread,
                if matches!(conversation, Conversation::Existing) {
                    4
                } else {
                    1
                }
            );
        }
    }
    // Keep shared Postgres test databases free of this run's projection rows.
    database
        .guard()
        .await
        .expect("cleanup connection")
        .execute(
            "DELETE FROM inbox_entries WHERE user_jid = ?",
            crate::db_params![owner.to_string()],
        )
        .await
        .expect("remove test projections");
}

#[tokio::test]
async fn committed_inbox_push_fresh_recipient_sqlite() {
    assert_committed_projection_push("sqlite::memory:", Conversation::Fresh).await;
}
#[tokio::test]
async fn committed_inbox_push_existing_conversation_sqlite() {
    assert_committed_projection_push("sqlite::memory:", Conversation::Existing).await;
}
#[tokio::test]
async fn committed_inbox_push_existing_thread_reply_count_sqlite() {
    assert_committed_projection_push("sqlite::memory:", Conversation::Thread).await;
}

async fn postgres_push(conversation: Conversation) {
    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (committed inbox push contents)");
        return;
    };
    assert_committed_projection_push(&url, conversation).await;
}
#[tokio::test]
async fn committed_inbox_push_fresh_recipient_postgres() {
    postgres_push(Conversation::Fresh).await;
}
#[tokio::test]
async fn committed_inbox_push_existing_conversation_postgres() {
    postgres_push(Conversation::Existing).await;
}
#[tokio::test]
async fn committed_inbox_push_existing_thread_reply_count_postgres() {
    postgres_push(Conversation::Thread).await;
}

#[tokio::test]
async fn committed_inbox_push_requires_the_projection_outcome() {
    let registry = test_registry();
    let deps = Deps::registry_only(&registry);
    let effect =
        super::super::effects::PlannedEffect::new(Effect::External(ExternalEffect::Direct(
            super::super::effects::direct::ExternalDirectEffect::PushInboxUpdate {
                owner: "alice@example.com".parse().expect("owner"),
                projection: ProjectionRef(0),
            },
        )));
    assert!(matches!(
        ImmediateSink
            .execute_with_applied(effect, &deps, &AppliedDurableEffects::default())
            .await,
        EffectOutcome::Unavailable
    ));
}
