//! Issue #1831 Phase B: the `extension_job_outbox` row and the archive row
//! it accompanies must commit or roll back together — a real transactional
//! outbox, not a fire-and-forget side effect. These tests exercise the two
//! repositories exactly as `ingress::durable::apply_durable` calls them
//! (same transaction, same order), rather than re-deriving the SQL.
use super::{ExtensionJobOutboxRepository, MamArchiveRepository};
use crate::extension_job_outbox::PendingJobInput;
use crate::ingress::test_support::IngressFixture;
use jid::BareJid;
use waddle_extensions::{JobKind, PluginId, RoomJid, WaddleId};
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage, MamTxStoreOutcome};

fn archive_and_message() -> (BareJid, ArchivedMessage) {
    let archive: BareJid = "romeo@example.com".parse().expect("archive owner");
    let mut message = ArchivedMessage::for_test(
        archive.clone().into(),
        "juliet@example.com".parse().expect("recipient"),
    );
    message.id = "extension-job-outbox-atomicity".into();
    message.body = Some("is this durable?".to_string());
    (archive, message)
}

fn job_input(outcome: &MamTxStoreOutcome) -> PendingJobInput {
    let MamTxStoreOutcome::Inserted { stanza_id, .. } = outcome else {
        panic!("expected a fresh archive insert, got {outcome:?}");
    };
    PendingJobInput {
        extension_id: PluginId::new("community-safety-judge").expect("plugin id"),
        job_kind: JobKind::new("message-judge").expect("job kind"),
        waddle_id: WaddleId::new("default").expect("waddle id"),
        room: None,
        target_stanza_id: stanza_id.clone(),
        body: "is this durable?".to_string(),
        now_ms: crate::time::now_ms(),
    }
}

#[tokio::test]
async fn commit_persists_both_the_archive_row_and_the_extension_job_outbox_row() {
    let fixture = IngressFixture::sqlite().await;
    crate::extension_job_outbox::initialize(&fixture.db)
        .await
        .expect("extension job outbox schema");
    let (archive, message) = archive_and_message();

    let mut tx = fixture.uow.begin().await.expect("begin");
    MamArchiveRepository::lock_sequences(&mut tx, std::slice::from_ref(&archive))
        .await
        .expect("lock sequences");
    let outcome =
        MamArchiveRepository::store(&mut tx, &archive, &message, ArchiveExpectation::Fresh)
            .await
            .expect("archive store");
    let input = job_input(&outcome);
    ExtensionJobOutboxRepository::enqueue_in_tx(&mut tx, input)
        .await
        .expect("enqueue extension job outbox row");
    tx.commit().await.expect("commit");

    assert_eq!(
        fixture.count("mam_messages").await,
        1,
        "the archive row must be durable after commit"
    );
    assert_eq!(
        fixture.count("extension_job_outbox").await,
        1,
        "the job outbox row committed alongside the archive row must also be durable"
    );
    fixture.close().await;
}

#[tokio::test]
async fn dropping_the_transaction_rolls_back_both_rows_together() {
    let fixture = IngressFixture::sqlite().await;
    crate::extension_job_outbox::initialize(&fixture.db)
        .await
        .expect("extension job outbox schema");
    let (archive, message) = archive_and_message();

    let mut tx = fixture.uow.begin().await.expect("begin");
    MamArchiveRepository::lock_sequences(&mut tx, std::slice::from_ref(&archive))
        .await
        .expect("lock sequences");
    let outcome =
        MamArchiveRepository::store(&mut tx, &archive, &message, ArchiveExpectation::Fresh)
            .await
            .expect("archive store");
    let input = job_input(&outcome);
    ExtensionJobOutboxRepository::enqueue_in_tx(&mut tx, input)
        .await
        .expect("enqueue extension job outbox row");
    // No `tx.commit()`: dropping rolls both writes back together.
    drop(tx);

    assert_eq!(
        fixture.count("mam_messages").await,
        0,
        "a rolled-back transaction must leave no archive row"
    );
    assert_eq!(
        fixture.count("extension_job_outbox").await,
        0,
        "a rolled-back transaction must leave no job outbox row either — the two can never \
         observably diverge"
    );
    fixture.close().await;
}

#[tokio::test]
async fn room_variant_records_the_room_and_a_direct_message_records_none() {
    let fixture = IngressFixture::sqlite().await;
    crate::extension_job_outbox::initialize(&fixture.db)
        .await
        .expect("extension job outbox schema");
    let (archive, message) = archive_and_message();

    let mut tx = fixture.uow.begin().await.expect("begin");
    MamArchiveRepository::lock_sequences(&mut tx, std::slice::from_ref(&archive))
        .await
        .expect("lock sequences");
    let outcome =
        MamArchiveRepository::store(&mut tx, &archive, &message, ArchiveExpectation::Fresh)
            .await
            .expect("archive store");
    let MamTxStoreOutcome::Inserted { stanza_id, .. } = &outcome else {
        panic!("expected a fresh archive insert");
    };
    let room = RoomJid::new("room@conference.example.test").expect("room jid");
    ExtensionJobOutboxRepository::enqueue_in_tx(
        &mut tx,
        PendingJobInput {
            extension_id: PluginId::new("community-safety-judge").expect("plugin id"),
            job_kind: JobKind::new("message-judge").expect("job kind"),
            waddle_id: WaddleId::new("space").expect("waddle id"),
            room: Some(room),
            target_stanza_id: stanza_id.clone(),
            body: "hello room".to_string(),
            now_ms: crate::time::now_ms(),
        },
    )
    .await
    .expect("enqueue");
    tx.commit().await.expect("commit");

    let claimed = crate::extension_job_outbox::claim_due_batch(&fixture.db, 10, i64::MAX)
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        claimed[0].room.as_ref().map(|room| room.as_str()),
        Some("room@conference.example.test")
    );
    fixture.close().await;
}
