use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::PendingReceiptRepository;
use waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal;

fn archived(id: PendingRowId, label: &str) -> PendingRow {
    let recipient: BareJid = "alice@example.com".parse().expect("recipient");
    PendingRow {
        id,
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
            label,
            recipient.into(),
        )),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

async fn raw_insert(tx: &mut crate::db::Transaction<'_>, row: &PendingRow, ignore: bool) {
    let PendingPayload::Archived(stamp) = &row.payload else {
        panic!("archived row")
    };
    let mut sql = String::from("INSERT INTO pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id) VALUES (?, ?, ?, 'archived', ?, ?)");
    if ignore {
        sql.push_str(" ON CONFLICT (row_id) DO NOTHING");
    }
    tx.execute(
        &sql,
        crate::db_params![
            row.id.as_str(),
            row.recipient.to_string(),
            row.original_receipt_at.timestamp_millis(),
            stamp.by.to_string(),
            stamp.id.to_string()
        ],
    )
    .await
    .expect("raw physical insert");
}

async fn ordinal(db: &Database, id: &PendingRowId) -> PendingNotificationRecoveryOrdinal {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT notification_recovery_ordinal FROM pending_delivery WHERE row_id = ?",
            crate::db_params![id.as_str()],
        )
        .await
        .expect("ordinal read");
    PendingNotificationRecoveryOrdinal::from_storage(
        rows.next()
            .await
            .expect("row")
            .expect("physical row")
            .get::<i64>(0)
            .expect("positive integer"),
    )
    .expect("valid ordinal")
}

async fn legacy_schema(fixture: &IngressFixture) -> [PendingRow; 2] {
    fixture.execute("CREATE TABLE pending_delivery (row_id TEXT PRIMARY KEY, recipient_jid TEXT NOT NULL, original_receipt_at BIGINT NOT NULL, payload_kind TEXT NOT NULL, archive_stanza_by TEXT, archive_stanza_id TEXT, transient_xml TEXT, flushed_in_session TEXT, legacy_note TEXT NOT NULL DEFAULT 'preserve')", ()).await;
    let rows = [
        archived(PendingRowId::new(hex::encode([255_u8; 32])), "legacy-high"),
        archived(PendingRowId::fresh(), "legacy-uuid"),
    ];
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("legacy transaction");
    for row in &rows {
        raw_insert(&mut tx, row, false).await;
    }
    tx.commit().await.expect("legacy commit");
    rows
}

async fn durable_insertion_order_contract(fixture: IngressFixture) {
    let old = legacy_schema(&fixture).await;
    let store =
        DatabasePendingDeliveryStorage::from_database(fixture.db.clone(), QuotaPolicy::Unlimited)
            .await
            .expect("initialize legacy schema");
    let initial = [
        ordinal(&fixture.db, &old[0].id).await,
        ordinal(&fixture.db, &old[1].id).await,
    ];
    assert_ne!(initial[0], initial[1]);
    assert!(initial.iter().all(|value| value.to_storage() > 0));
    assert_eq!(
        fixture
            .count("pending_delivery WHERE legacy_note = 'preserve'")
            .await,
        2
    );
    fixture.execute("UPDATE pending_delivery SET original_receipt_at = 0, flushed_in_session = 'claimed', notification_outboxed_at_ms = 1 WHERE row_id = ?", crate::db_params![old[0].id.as_str()]).await;
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("duplicate transaction");
    raw_insert(&mut tx, &old[0], true).await;
    tx.commit().await.expect("duplicate commit");
    let reopened =
        DatabasePendingDeliveryStorage::from_database(fixture.db.clone(), QuotaPolicy::Unlimited)
            .await
            .expect("idempotent initialization");
    for (row, expected) in old.iter().zip(initial) {
        assert_eq!(ordinal(&fixture.db, &row.id).await, expected);
    }
    let raw = archived(PendingRowId::fresh(), "new raw row");
    let mut tx = fixture.db.begin_immediate().await.expect("raw transaction");
    raw_insert(&mut tx, &raw, false).await;
    tx.commit().await.expect("raw commit");
    let raw_order = ordinal(&fixture.db, &raw.id).await;
    assert!(initial.iter().all(|old| raw_order > *old));
    let uow = archived(PendingRowId::new(hex::encode([0_u8; 32])), "new UoW row");
    let mut tx = fixture.uow.begin().await.expect("UoW transaction");
    PendingReceiptRepository::insert(&mut tx, &uow, QuotaPolicy::Unlimited)
        .await
        .expect("actual canonical pending writer");
    tx.commit().await.expect("UoW commit");
    let uow_order = ordinal(&fixture.db, &uow.id).await;
    assert!(
        uow_order > raw_order,
        "insertion order ignores the lexically smaller ID"
    );
    let page = reopened
        .list_unoutboxed_archived_after(Some(raw_order), Some(uow_order), 1)
        .await
        .expect("ordinal page");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].row.id, uow.id);
    let failed_update = fixture.db.guard().await.expect("guard").execute("UPDATE pending_delivery SET notification_recovery_ordinal = notification_recovery_ordinal + 100 WHERE row_id = ?", crate::db_params![uow.id.as_str()]).await;
    assert!(
        failed_update.is_err(),
        "physical ordinals cannot be rewritten"
    );
    assert_eq!(ordinal(&fixture.db, &uow.id).await, uow_order);
    let mut allocated = uow_order;
    if fixture.db.driver() == DatabaseDriver::Postgres {
        let gap = archived(PendingRowId::fresh(), "rollback gap");
        let mut tx = fixture.db.begin().await.expect("rollback transaction");
        raw_insert(&mut tx, &gap, false).await;
        let mut rows = tx
            .query(
                "SELECT notification_recovery_ordinal FROM pending_delivery WHERE row_id = ?",
                crate::db_params![gap.id.as_str()],
            )
            .await
            .expect("allocated position");
        allocated = PendingNotificationRecoveryOrdinal::from_storage(
            rows.next()
                .await
                .expect("row")
                .expect("gap row")
                .get::<i64>(0)
                .expect("position"),
        )
        .expect("valid gap");
        drop(rows);
        tx.rollback().await.expect("rollback");
        assert_eq!(
            store
                .notification_recovery_high_water()
                .await
                .expect("committed horizon"),
            Some(uow_order)
        );
    }
    fixture.execute("DELETE FROM pending_delivery", ()).await;
    let empty =
        DatabasePendingDeliveryStorage::from_database(fixture.db.clone(), QuotaPolicy::Unlimited)
            .await
            .expect("reopen after last-row deletion");
    assert_eq!(
        empty
            .notification_recovery_high_water()
            .await
            .expect("empty horizon"),
        None
    );
    empty
        .insert(uow.clone())
        .await
        .expect("same ID physically reinserted");
    assert!(
        ordinal(&fixture.db, &uow.id).await > allocated,
        "deletion/restart/rollback never reuse insertion positions"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_pending_recovery_ordinal_schema_contracts() {
    durable_insertion_order_contract(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_pending_recovery_ordinal_schema_contracts() {
    if let Some(fixture) = IngressFixture::postgres("pendingord").await {
        durable_insertion_order_contract(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_pending_recovery_ordinal_overflow_rolls_back_insert() {
    let fixture = IngressFixture::sqlite().await;
    let store =
        DatabasePendingDeliveryStorage::from_database(fixture.db.clone(), QuotaPolicy::Unlimited)
            .await
            .expect("store");
    fixture
        .execute(
            "UPDATE pending_notification_recovery_counter SET last_ordinal = ? WHERE singleton = 1",
            crate::db_params![i64::MAX],
        )
        .await;
    assert!(store
        .insert(archived(PendingRowId::fresh(), "overflow"))
        .await
        .is_err());
    assert_eq!(fixture.count("pending_delivery").await, 0);
    assert_eq!(
        store
            .notification_recovery_high_water()
            .await
            .expect("horizon"),
        None
    );
    assert_eq!(
        fixture
            .count("pending_notification_recovery_counter WHERE last_ordinal = 9223372036854775807")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn postgres_pending_recovery_initializer_pins_read_committed() {
    let Some(fixture) = IngressFixture::postgres("pendingordrr").await else {
        return;
    };
    legacy_schema(&fixture).await;
    let mut url = url::Url::parse(fixture.db.database_url()).expect("fixture URL");
    let options = url
        .query_pairs()
        .find(|(key, _)| key == "options")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default();
    let retained = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair(
            "options",
            &format!("{options} -c default_transaction_isolation=repeatable\\ read"),
        );
    let db = Database::from_config(
        "pending-ordinal-repeatable-read",
        &crate::db::DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string()),
    )
    .await
    .expect("fixture-local pool");
    {
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query("SHOW default_transaction_isolation", ())
            .await
            .expect("default isolation");
        assert_eq!(
            rows.next()
                .await
                .expect("row")
                .expect("setting")
                .get::<String>(0)
                .expect("value"),
            "repeatable read"
        );
    }
    let store = DatabasePendingDeliveryStorage::from_database(db.clone(), QuotaPolicy::Unlimited)
        .await
        .expect("RR initialization");
    let mut tx = begin(&store).await.expect("metadata transaction");
    let mut rows = tx
        .query("SHOW transaction_isolation", ())
        .await
        .expect("isolation");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("setting")
            .get::<String>(0)
            .expect("value"),
        "read committed"
    );
    drop(rows);
    tx.commit().await.expect("transaction commit");
    assert_eq!(
        fixture
            .count("pending_delivery WHERE notification_recovery_ordinal > 0")
            .await,
        2
    );
    drop(store);
    drop(db);
    fixture.close().await;
}
