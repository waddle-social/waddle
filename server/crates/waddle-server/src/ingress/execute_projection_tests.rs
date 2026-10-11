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
