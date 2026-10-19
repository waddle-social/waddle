use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::interpret::effects::{Effect, PlannedEffect};
use waddle_xmpp::{
    ingress::{
        LinkPreviewMediaRefMutation, LinkPreviewMediaRefState, NotificationActivityMutation,
    },
    mam::RichMessageId,
};
use waddle_xmpp_core::xep0359::StanzaId;

async fn sealed_inbox_push_audience(empty_original_audience: bool) {
    use crate::ingress::{execute::uow::execute_with_uow, IngressEffectCapture};
    use crate::server::routes::{
        interpret::{effects::PlanSink, interpret},
        websocket::{interpret_loop::build_interpret_deps, tests as socket_tests},
    };
    use waddle_xmpp::{
        ingress::{DigestContext, DigestInput, EffectMessageIdentity, NormalizedTarget},
        muc::{
            room_actor::{ChangeAffiliation, Join},
            room_registry_actor::CreateRoom,
        },
        protocol::OutboundEvent,
        Stanza,
    };
    let fixture = IngressFixture::sqlite().await;
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "frozen-inbox@muc.example.com".parse().expect("room");
    let phone: jid::FullJid = "alice@example.com/phone".parse().expect("phone");
    let laptop: jid::FullJid = "alice@example.com/laptop".parse().expect("laptop");
    let owner = phone.to_bare();
    let mut submission =
        fixture.submission(Some("frozen-inbox-audience"), "frozen room projection");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "frozen-inbox".into(),
            channel_id: "frozen-inbox".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    for (jid, nick) in [
        (submission.sender.clone(), "sender"),
        (phone.clone(), "alice"),
    ] {
        // Inbox audiences come from persistent affiliations, not the live
        // occupant's Join label. Exercise a real durable recipient projection.
        actor
            .ask(ChangeAffiliation {
                jid: jid.to_bare(),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("grant inbox membership");
        actor
            .ask(Join {
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                nick: nick.into(),
                real_jid: jid,
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
    }
    let (phone_sender, mut phone_receiver) = tokio::sync::mpsc::channel(16);
    if !empty_original_audience {
        socket_tests::register_test_connection(&state, &phone, phone_sender.clone()).await;
    }
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("room digest");
    let deps = build_interpret_deps(&state, None);
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let mut planned = deps.clone();
    planned.effects = &sink;
    planned.ingress_effect_capture = Some(capture.clone());
    interpret(
        vec![OutboundEvent::DispatchToRoom {
            room,
            message: Box::new(submission.plan.sanitized_message.clone()),
        }],
        &planned,
    )
    .await;
    submission.plan.room_canonical_message = sink.room_canonical_message();
    let (plan, execution) = sink.take();
    submission.plan.plan = plan;
    submission.plan.room_execution = execution;
    submission.plan.intents = capture.snapshot().intents;
    assert!(submission.plan.plan.iter().any(|planned| matches!(&planned.effect,
        Effect::External(ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate { owner: saved, .. })) if saved == &owner
    )), "durable-member planning must produce the inbox refresh before acceptance");
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("accept captured room projection");
    let (index, effect, projection) = decision.external.iter().enumerate().find_map(|(index, effect)| {
        match effect { ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate { owner: saved, projection, receipt }) if saved == &owner => {
            assert!(matches!(receipt.as_deref(), Some(IngressEffectIntent::RouteDirect { recipient, fanout, route_identity: EffectMessageIdentity::InboxPush(_), .. })
                if recipient == &owner && (if empty_original_audience { fanout.is_empty() } else { fanout.as_slice() == std::slice::from_ref(&phone) })), "the complete original audience is recorded, including empty");
            Some((index, effect, *projection))
        }, _ => None }
    }).expect("captured inbox push");
    let entry = decision
        .applied_durable
        .inbox(projection)
        .expect("committed Phase-B projection")
        .clone();
    if empty_original_audience {
        socket_tests::register_test_connection(&state, &phone, phone_sender).await;
    }
    let (laptop_sender, mut laptop_receiver) = tokio::sync::mpsc::channel(16);
    socket_tests::register_test_connection(&state, &laptop, laptop_sender).await;
    let outcome = execute_with_uow(
        &fixture.uow,
        &fixture.db,
        &decision,
        index,
        effect,
        &deps,
        tokio::time::Instant::now() + std::time::Duration::from_secs(2),
    )
    .await
    .expect("sealed hook owns the refresh");
    let EffectOutcome::InboxPush(resources) = outcome else {
        panic!("best-effort refresh policy remains InboxPush");
    };
    if empty_original_audience {
        assert!(resources.is_empty());
        assert!(
            phone_receiver.try_recv().is_err(),
            "a new resource cannot expand an empty original audience"
        );
    } else {
        assert_eq!(resources, vec![phone.clone()]);
        let Stanza::Message(message) = phone_receiver
            .try_recv()
            .expect("phone gets one inbox refresh")
            .stanza
        else {
            panic!("message");
        };
        assert_eq!(
            message,
            waddle_xmpp::xep::xep0430::build_inbox_push(phone.clone().into(), &entry),
            "push carries the original committed projection"
        );
        assert!(phone_receiver.try_recv().is_err(), "one captured push");
    }
    assert!(
        laptop_receiver.try_recv().is_err(),
        "a laptop connected after h cannot receive the frozen push"
    );
    let mut changed = effect.clone();
    if let ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate { receipt, .. }) =
        &mut changed
    {
        if let Some(IngressEffectIntent::RouteDirect { fanout, .. }) = receipt.as_deref_mut() {
            fanout.push(laptop.clone());
        }
    }
    assert!(
        execute_sealed_inbox_push(&fixture.uow, &decision, index, &changed, &deps)
            .await
            .is_err(),
        "a retained effect cannot alter the sealed audience"
    );
    assert_eq!(
        fixture.count("ingress_send_attempts").await,
        0,
        "projection refresh creates no durable message send ledger"
    );
    assert_eq!(fixture.count("sm_ingress_appends").await, 0);
    // Mark-read and other unsealed callers intentionally refresh today's devices.
    let mut read_entry = entry;
    read_entry.unread = 0;
    let mut current = crate::server::routes::interpret::push_inbox_update(
        &state.deps.protocol.connection_registry,
        Some(&state.deps.protocol.user_registry),
        &owner,
        &read_entry,
    )
    .await;
    current.sort();
    let mut expected = vec![phone, laptop];
    expected.sort();
    assert_eq!(
        current, expected,
        "the existing mark-read helper remains dynamic"
    );
    assert!(phone_receiver.try_recv().is_ok());
    assert!(laptop_receiver.try_recv().is_ok());
    fixture.close().await;
}

#[tokio::test]
async fn xep0045_sealed_inbox_push_keeps_pre_h_phone_and_excludes_late_laptop() {
    sealed_inbox_push_audience(false).await;
}

#[tokio::test]
async fn xep0045_sealed_inbox_push_preserves_an_empty_original_audience() {
    sealed_inbox_push_audience(true).await;
}

async fn activity_atomicity(fixture: IngressFixture) {
    let owner = fixture.principal.bare_jid().clone();
    let conversation: jid::BareJid = "juliet@example.com".parse().expect("conversation");
    let store = crate::notification_activity::NotificationActivityStore::new(fixture.db.clone())
        .await
        .expect("activity schema");
    let mut submission = fixture.submission(Some("atomic-activity"), "activity");
    let mutation = NotificationActivityMutation::ChatStateGone {
        conversation: conversation.clone(),
        committed_at_ms: 200,
    };
    submission.plan.intents = vec![IngressEffectIntent::NotificationActivityPreview {
        owner: owner.clone(),
        mutation: mutation.clone(),
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Direct(ExternalDirectEffect::NotificationActivity {
            owner: owner.clone(),
            mutation,
        }),
    ))];
    store
        .record_outbound_message(&owner, &conversation, 100)
        .await
        .expect("old activity");
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    let mut foreign = decision.external[0].clone();
    let ExternalEffect::Direct(ExternalDirectEffect::NotificationActivity {
        owner: foreign_owner,
        ..
    }) = &mut foreign
    else {
        panic!("activity effect");
    };
    *foreign_owner = "other@example.com".parse().expect("foreign owner");
    assert!(store_projection(&fixture.uow, &decision, 0, &foreign)
        .await
        .is_err());
    assert_eq!(fixture.count("notification_activity").await, 1);
    assert_eq!(fixture.count("ingress_deliveries").await, 0);
    fail_after_projection_update(key);
    assert!(
        store_projection(&fixture.uow, &decision, 0, &decision.external[0])
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .count("notification_activity WHERE last_active_at_ms = 100 AND updated_at_ms = 100")
            .await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("ingress_deliveries").await, 0);
    let registry = waddle_xmpp::registry::ConnectionRegistry::new();
    let deps = crate::server::routes::interpret::Deps::registry_only(&registry);
    assert!(super::super::owns(
        &decision.external[0],
        &decision.route_progress
    ));
    let settled = super::super::execute_with_uow(
        &fixture.uow,
        &fixture.db,
        &decision,
        0,
        &decision.external[0],
        &deps,
        tokio::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .await
    .expect("canonical projection routing");
    assert!(matches!(
        settled,
        EffectOutcome::Settled(SettledOutcome {
            completion: SettledCompletion::Complete,
            ..
        })
    ));

    assert_eq!(
        fixture
            .count("notification_activity WHERE last_active_at_ms = 0 AND updated_at_ms = 200")
            .await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(fixture.count("ingress_deliveries").await, 1);
    // A settled duplicate cannot change the projection, even after a newer signal.
    store
        .record_outbound_message(&owner, &conversation, 300)
        .await
        .expect("new activity");
    store_projection(&fixture.uow, &decision, 0, &decision.external[0])
        .await
        .expect("same-key replay");
    assert_eq!(
        fixture
            .count("notification_activity WHERE last_active_at_ms = 300 AND updated_at_ms = 300")
            .await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(fixture.count("ingress_deliveries").await, 1);
    // A different historical obligation must not resurrect engagement after gone.
    store
        .record_chat_state_gone(&owner, &conversation, 400)
        .await
        .expect("new gone");
    store
        .record_outbound_message(&owner, &conversation, 350)
        .await
        .expect("old outbound");
    store
        .record_chat_state(
            &owner,
            &conversation,
            crate::notification_activity::NotificationChatState::Active,
            350,
        )
        .await
        .expect("old state");
    store
        .record_read_marker(&owner, &conversation, 350)
        .await
        .expect("old marker");
    assert_eq!(fixture.count("notification_activity WHERE last_active_at_ms = 0 AND last_chat_state = 'gone' AND updated_at_ms = 400").await, 1);
    store
        .record_outbound_message(&owner, &conversation, 400)
        .await
        .expect("same millisecond historical activity");
    assert_eq!(fixture.count("notification_activity WHERE last_active_at_ms = 0 AND last_chat_state = 'gone' AND updated_at_ms = 400").await, 1);
    fixture.close().await;
}

async fn preview_atomicity(fixture: IngressFixture) {
    let archive = fixture.principal.bare_jid().clone();
    let slot = uuid::Uuid::new_v4();
    fixture.execute("INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, expires_at) VALUES (?, ?, ?, ?, ?, ?)", crate::db_params![slot.to_string(), archive.to_string(), "preview.png".to_owned(), 1_i64, "image/png".to_owned(), "2099-01-01T00:00:00Z".to_owned()]).await;
    let mutation = LinkPreviewMediaRefMutation {
        upload_slot_id: slot,
        archive: archive.clone(),
        message_id: RichMessageId::new("original".to_owned()).expect("message id"),
        current_archive_stanza_id: StanzaId::new("original-archive", archive.into()),
        state: LinkPreviewMediaRefState::Current,
    };
    let mut submission = fixture.submission(Some("atomic-preview"), "preview");
    submission.plan.intents = vec![IngressEffectIntent::LinkPreviewMediaRef {
        mutation: mutation.clone(),
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs {
            mutations: vec![mutation],
        }),
    ))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    fail_after_projection_update(decision.message_key.expect("key"));
    assert!(
        store_projection(&fixture.uow, &decision, 0, &decision.external[0])
            .await
            .is_err()
    );
    assert_eq!(fixture.count("link_preview_media_refs").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("ingress_deliveries").await, 0);
    let registry = waddle_xmpp::registry::ConnectionRegistry::new();
    let deps = crate::server::routes::interpret::Deps::registry_only(&registry);
    assert!(super::super::owns(
        &decision.external[0],
        &decision.route_progress
    ));
    let settled = super::super::execute_with_uow(
        &fixture.uow,
        &fixture.db,
        &decision,
        0,
        &decision.external[0],
        &deps,
        tokio::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .await
    .expect("canonical projection routing");
    assert!(matches!(
        settled,
        EffectOutcome::Settled(SettledOutcome {
            completion: SettledCompletion::Complete,
            ..
        })
    ));

    assert_eq!(fixture.count("link_preview_media_refs WHERE current_archive_id = 'original-archive' AND state = 'current'").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(fixture.count("ingress_deliveries").await, 1);
    let timestamp = fixture
        .optional_text("SELECT updated_at FROM link_preview_media_refs")
        .await;
    store_projection(&fixture.uow, &decision, 0, &decision.external[0])
        .await
        .expect("same-key replay");
    assert_eq!(
        fixture
            .optional_text("SELECT updated_at FROM link_preview_media_refs")
            .await,
        timestamp
    );
    fixture.execute("UPDATE link_preview_media_refs SET current_archive_id = 'newer', updated_at = '2099-01-01T00:00:00.000000Z'", ()).await;
    // Even missing receipt evidence must not clobber a newer projection.
    fixture
        .execute("DELETE FROM ingress_effect_receipts", ())
        .await;
    store_projection(&fixture.uow, &decision, 0, &decision.external[0])
        .await
        .expect("old outstanding replay");
    assert_eq!(
        fixture
            .count("link_preview_media_refs WHERE current_archive_id = 'newer'")
            .await,
        1
    );
    fixture
        .execute(
            "UPDATE link_preview_media_refs SET updated_at = created_at",
            (),
        )
        .await;
    fixture
        .execute("DELETE FROM ingress_effect_receipts", ())
        .await;
    store_projection(&fixture.uow, &decision, 0, &decision.external[0])
        .await
        .expect("unknown equal-time revision replay");
    assert_eq!(
        fixture
            .count("link_preview_media_refs WHERE current_archive_id = 'newer'")
            .await,
        1
    );
    // Canonical archive order resolves equal timestamp precision on SQLite.
    // A newer correction must survive an old outstanding Current mutation.
    let seed_archive = match fixture.db.driver() {
        crate::db::DatabaseDriver::Sqlite => "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, archive_seq) VALUES (?, ?, ?, ?, ?, ?, ?)",
        crate::db::DatabaseDriver::Postgres => "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, archive_seq) VALUES (?, ?, ?::timestamptz, ?, ?, ?, ?)",
    };
    for (id, ordinal) in [("original-archive", 1_i64), ("newer", 2_i64)] {
        fixture
            .execute(
                seed_archive,
                crate::db_params![
                    id.to_owned(),
                    fixture.principal.bare_jid().to_string(),
                    "2026-01-01T00:00:00Z".to_owned(),
                    fixture.principal.bare_jid().to_string(),
                    "juliet@example.com".to_owned(),
                    "message".to_owned(),
                    ordinal
                ],
            )
            .await;
    }
    fixture
        .execute(
            "UPDATE link_preview_media_refs SET updated_at = created_at",
            (),
        )
        .await;
    fixture
        .execute("DELETE FROM ingress_effect_receipts", ())
        .await;
    store_projection(&fixture.uow, &decision, 0, &decision.external[0])
        .await
        .expect("equal timestamp replay");
    assert_eq!(
        fixture
            .count("link_preview_media_refs WHERE current_archive_id = 'newer'")
            .await,
        1
    );
    fixture.execute("UPDATE link_preview_media_refs SET current_archive_id = 'original-archive', state = 'unreferenced', updated_at = created_at", ()).await;
    fixture
        .execute("DELETE FROM ingress_effect_receipts", ())
        .await;
    store_projection(&fixture.uow, &decision, 0, &decision.external[0])
        .await
        .expect("cleared revision replay");
    assert_eq!(
        fixture
            .count("link_preview_media_refs WHERE state = 'unreferenced'")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_activity_projection_and_receipt_commit_or_rollback_together() {
    activity_atomicity(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_activity_projection_and_receipt_commit_or_rollback_together() {
    if let Some(fixture) = IngressFixture::postgres("atomic_activity").await {
        activity_atomicity(fixture).await;
    }
}
#[tokio::test]
async fn sqlite_preview_projection_and_receipt_commit_or_rollback_together() {
    preview_atomicity(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_preview_projection_and_receipt_commit_or_rollback_together() {
    if let Some(fixture) = IngressFixture::postgres("atomic_preview").await {
        preview_atomicity(fixture).await;
    }
}
