//! #1831 Phase 2: the `message_judgment_outbox` row and the archive row it
//! accompanies must commit or roll back together — a real transactional
//! outbox, not a fire-and-forget side effect. These tests exercise the two
//! repositories exactly as `ingress::durable::apply_durable` calls them
//! (same transaction, same order), rather than re-deriving the SQL.
use super::{MamArchiveRepository, MessageJudgmentOutboxRepository};
use crate::ingress::test_support::IngressFixture;
use crate::message_judgment_outbox::PendingJudgmentInput;
use jid::BareJid;
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage, MamTxStoreOutcome};

fn archive_and_message() -> (BareJid, ArchivedMessage) {
    let archive: BareJid = "romeo@example.com".parse().expect("archive owner");
    let mut message = ArchivedMessage::for_test(
        archive.clone().into(),
        "juliet@example.com".parse().expect("recipient"),
    );
    message.id = "judgment-outbox-atomicity".into();
    message.body = Some("is this durable?".to_string());
    (archive, message)
}

async fn judgment_input(archive: &BareJid, outcome: &MamTxStoreOutcome) -> PendingJudgmentInput {
    let MamTxStoreOutcome::Inserted { stanza_id, .. } = outcome else {
        panic!("expected a fresh archive insert, got {outcome:?}");
    };
    PendingJudgmentInput {
        waddle_id: waddle_xmpp::muc::durable::WaddleId::new(archive.to_string()),
        stanza_id: stanza_id.clone(),
        body: "is this durable?".to_string(),
        now_ms: crate::time::now_ms(),
    }
}

#[tokio::test]
async fn commit_persists_both_the_archive_row_and_the_judgment_outbox_row() {
    let fixture = IngressFixture::sqlite().await;
    crate::message_judgment_outbox::initialize(&fixture.db)
        .await
        .expect("judgment outbox schema");
    let (archive, message) = archive_and_message();

    let mut tx = fixture.uow.begin().await.expect("begin");
    MamArchiveRepository::lock_sequences(&mut tx, std::slice::from_ref(&archive))
        .await
        .expect("lock sequences");
    let outcome =
        MamArchiveRepository::store(&mut tx, &archive, &message, ArchiveExpectation::Fresh)
            .await
            .expect("archive store");
    let input = judgment_input(&archive, &outcome).await;
    MessageJudgmentOutboxRepository::enqueue_in_tx(&mut tx, input)
        .await
        .expect("enqueue judgment outbox row");
    tx.commit().await.expect("commit");

    assert_eq!(
        fixture.count("mam_messages").await,
        1,
        "the archive row must be durable after commit"
    );
    assert_eq!(
        fixture.count("message_judgment_outbox").await,
        1,
        "the judgment outbox row committed alongside the archive row must also be durable"
    );
    fixture.close().await;
}

#[tokio::test]
async fn dropping_the_transaction_rolls_back_both_rows_together() {
    let fixture = IngressFixture::sqlite().await;
    crate::message_judgment_outbox::initialize(&fixture.db)
        .await
        .expect("judgment outbox schema");
    let (archive, message) = archive_and_message();

    let mut tx = fixture.uow.begin().await.expect("begin");
    MamArchiveRepository::lock_sequences(&mut tx, std::slice::from_ref(&archive))
        .await
        .expect("lock sequences");
    let outcome =
        MamArchiveRepository::store(&mut tx, &archive, &message, ArchiveExpectation::Fresh)
            .await
            .expect("archive store");
    let input = judgment_input(&archive, &outcome).await;
    MessageJudgmentOutboxRepository::enqueue_in_tx(&mut tx, input)
        .await
        .expect("enqueue judgment outbox row");
    // No `tx.commit()`: per `IngressUowTransaction`'s own docs, dropping an
    // uncommitted transaction rolls back the underlying database
    // transaction. A real crash between the archive write and commit is
    // exactly this — no code path exists where one row survives without
    // the other, because both writes share this one transaction.
    drop(tx);

    assert_eq!(
        fixture.count("mam_messages").await,
        0,
        "the archive row must not survive an uncommitted transaction"
    );
    assert_eq!(
        fixture.count("message_judgment_outbox").await,
        0,
        "the judgment outbox row must not survive either — it can never \
         outlive the archive row it was enqueued alongside"
    );
    fixture.close().await;
}

/// Sanity check on the reverse direction: with the feature left disabled
/// (the default `IngressUnitOfWork`), `apply_durable`'s own gate would
/// never even reach `MessageJudgmentOutboxRepository`. This test instead
/// pins down `IngressUowTransaction::judgment_outbox_enabled`'s default so
/// a future refactor cannot silently flip this feature on.
#[tokio::test]
async fn a_fresh_unit_of_work_defaults_to_judgment_outbox_disabled() {
    let fixture = IngressFixture::sqlite().await;
    let tx = fixture.uow.begin().await.expect("begin");
    assert!(!tx.judgment_outbox_enabled());
    drop(tx);
    fixture.close().await;
}
