use super::*;
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::stream_management::persistence::{
    IngressCustodyDisposition, SmPersistenceStorage,
};
use waddle_xmpp::stream_management::{SmIngressAppendKey, SmIngressReceiptKind};

async fn fixture(
    quota: QuotaPolicy,
) -> (
    DatabasePendingDeliveryStorage,
    crate::sm_persistence::DatabaseSmPersistence,
    PendingRow,
    PersistedIngressAppend,
) {
    fixture_at(quota, None).await
}

async fn fixture_at(
    quota: QuotaPolicy,
    database_url: Option<&str>,
) -> (
    DatabasePendingDeliveryStorage,
    crate::sm_persistence::DatabaseSmPersistence,
    PendingRow,
    PersistedIngressAppend,
) {
    let sm = crate::sm_persistence::DatabaseSmPersistence::open(database_url)
        .await
        .expect("SM store");
    let pending = DatabasePendingDeliveryStorage::from_database(sm.database(), quota)
        .await
        .expect("co-located pending store");
    let resource: FullJid = "alice@example.com/phone".parse().expect("resource");
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(resource.clone())));
    message.id = Some(xmpp_parsers::message::Id("custody-atomic".to_owned()));
    message.from = Some("bob@example.com/web".parse().expect("sender JID"));
    let receipt = chrono::Utc::now();
    let append = PersistedIngressAppend {
        key: SmIngressAppendKey {
            message_key: MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [1; 32],
            resource: resource.clone(),
        },
        accepting_stream: SmSessionId::new(format!("custody-transfer-{}", uuid::Uuid::new_v4())),
        sequence: 7,
        payload: Stanza::Message(message.clone()),
        original_receipt_at: receipt,
        disposition: IngressCustodyDisposition::Pending,
        appended_at: receipt,
    };
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: resource.to_bare(),
        original_receipt_at: receipt,
        payload: PendingPayload::Transient(Box::new(message)),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let database = sm.database();
    let mut tx = database.begin_immediate().await.expect("seed transaction");
    crate::sm_persistence::ingress_append::insert(&mut tx, &append)
        .await
        .expect("seed custody");
    tx.commit().await.expect("seed commit");
    (pending, sm, row, append)
}

#[tokio::test]
async fn custody_transfer_commits_once_with_its_pending_payload() {
    let (pending, sm, row, append) = fixture(QuotaPolicy::Unlimited).await;
    assert_eq!(
        pending
            .insert_ingress_custody(row.clone(), &append)
            .await
            .expect("transfer"),
        CustodyInsertOutcome::Inserted
    );
    assert_eq!(
        sm.get_ingress_append(&append.key)
            .await
            .expect("proof")
            .expect("allocation")
            .disposition,
        IngressCustodyDisposition::Promoted
    );
    assert_eq!(
        pending
            .insert_ingress_custody(row.clone(), &append)
            .await
            .expect("retry"),
        CustodyInsertOutcome::AlreadyCompleted
    );
    assert_eq!(
        pending
            .list(&row.recipient)
            .await
            .expect("pending rows")
            .len(),
        1
    );
}

#[tokio::test]
async fn custody_transfer_cannot_resurrect_a_foreign_tombstone() {
    let (pending, sm, row, append) = fixture(QuotaPolicy::Unlimited).await;
    // The recovery worker retains a stale Pending observation while a
    // different worker finishes the tombstone and pending-row scrub first.
    assert!(sm
        .complete_ingress_append(
            &append.key,
            &append.accepting_stream,
            append.sequence,
            IngressCustodyDisposition::Tombstoned
        )
        .await
        .expect("foreign tombstone"));
    assert_eq!(
        pending
            .insert_ingress_custody(row.clone(), &append)
            .await
            .expect("stale handoff"),
        CustodyInsertOutcome::AlreadyCompleted
    );
    assert!(pending
        .list(&row.recipient)
        .await
        .expect("pending rows")
        .is_empty());
    assert_eq!(
        sm.get_ingress_append(&append.key)
            .await
            .expect("proof")
            .expect("allocation")
            .disposition,
        IngressCustodyDisposition::Tombstoned
    );
}

#[tokio::test]
async fn custody_transfer_preserves_pending_custody_on_quota_and_missing_identity() {
    let (pending, sm, row, append) = fixture(QuotaPolicy::CountCap { max_rows: 0 }).await;
    assert_eq!(
        pending
            .insert_ingress_custody(row.clone(), &append)
            .await
            .expect("quota refusal"),
        CustodyInsertOutcome::QuotaExceeded
    );
    let mut mismatch = append.clone();
    mismatch.sequence += 1;
    assert!(pending
        .insert_ingress_custody(row.clone(), &mismatch)
        .await
        .is_err());
    assert!(pending
        .list(&row.recipient)
        .await
        .expect("pending rows")
        .is_empty());
    assert_eq!(
        sm.get_ingress_append(&append.key)
            .await
            .expect("proof")
            .expect("allocation")
            .disposition,
        IngressCustodyDisposition::Pending
    );
}

#[tokio::test]
async fn custody_transfer_rolls_back_pending_insert_if_completion_fails() {
    let (pending, sm, row, append) = fixture(QuotaPolicy::Unlimited).await;
    sm.database().guard().await.expect("database").execute(
        "CREATE TRIGGER fail_custody_completion BEFORE UPDATE ON sm_ingress_appends WHEN NEW.disposition = 2 BEGIN SELECT RAISE(ABORT, 'injected custody completion failure'); END", (),
    ).await.expect("completion failure trigger");
    assert!(pending
        .insert_ingress_custody(row.clone(), &append)
        .await
        .is_err());
    assert!(pending
        .list(&row.recipient)
        .await
        .expect("pending rows")
        .is_empty());
    assert_eq!(
        sm.get_ingress_append(&append.key)
            .await
            .expect("proof")
            .expect("allocation")
            .disposition,
        IngressCustodyDisposition::Pending
    );
}

#[tokio::test]
async fn postgres_custody_transfer_serializes_both_tombstone_orders() {
    use std::time::Duration;
    let Some(fixture) =
        crate::ingress::test_support::IngressFixture::postgres("custody_transfer").await
    else {
        return;
    };
    let (pending, sm, row, append) =
        fixture_at(QuotaPolicy::Unlimited, Some(fixture.db.database_url())).await;
    let database = sm.database();
    let mut tombstone = database
        .begin()
        .await
        .expect("foreign tombstone transaction");
    tombstone.execute("UPDATE sm_ingress_appends SET disposition = 3 WHERE message_key = ? AND disposition = 0", crate::db_params![append.key.message_key.to_storage().to_string()]).await.expect("hold custody tombstone lock");
    let mut handoff = tokio::spawn({
        let pending = pending.clone();
        let row = row.clone();
        let append = append.clone();
        async move { pending.insert_ingress_custody(row, &append).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut handoff)
            .await
            .is_err(),
        "handoff waits for foreign custody row lock"
    );
    tombstone.commit().await.expect("foreign tombstone commit");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), handoff)
            .await
            .expect("handoff completes")
            .expect("handoff task")
            .expect("handoff outcome"),
        CustodyInsertOutcome::AlreadyCompleted
    );
    assert!(pending
        .list(&row.recipient)
        .await
        .expect("pending rows")
        .is_empty());
    drop(sm);
    drop(pending);
    drop(database);

    // Reverse the row-lock order. The foreign scrub sees the old Pending
    // snapshot, then waits for transfer; its pending scrub must see the
    // inserted row after the atomic transfer commits.
    let (pending, sm, row, append) =
        fixture_at(QuotaPolicy::Unlimited, Some(fixture.db.database_url())).await;
    let database = pending.database();
    let mut handoff = database.begin().await.expect("handoff transaction");
    assert_eq!(
        transfer(&mut handoff, &row, &append, QuotaPolicy::Unlimited)
            .await
            .expect("atomic transfer"),
        CustodyInsertOutcome::Inserted
    );
    let recipient = row.recipient.clone();
    let mut scrub = tokio::spawn({
        let pending = pending.clone();
        async move {
            let target = waddle_xmpp::tombstone::TombstoneTarget::Direct {
                wire_id: "custody-atomic".to_owned(),
                author: "bob@example.com".parse().expect("author"),
                archive: recipient,
            };
            sm.scrub_ingress_custody(&target, chrono::Utc::now())
                .await
                .expect("custody scrub");
            pending
                .scrub_for_tombstone(&target)
                .await
                .expect("pending scrub");
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut scrub)
            .await
            .is_err(),
        "foreign scrub waits for custody transfer lock"
    );
    handoff.commit().await.expect("handoff commit");
    tokio::time::timeout(Duration::from_secs(5), scrub)
        .await
        .expect("scrub completes")
        .expect("scrub task");
    assert!(pending
        .list(&row.recipient)
        .await
        .expect("pending rows")
        .is_empty());
    drop(database);
    drop(pending);
    fixture.close().await;
}
