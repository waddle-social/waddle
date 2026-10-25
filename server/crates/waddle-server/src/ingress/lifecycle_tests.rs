use super::contract::{test_lineage_config, RETIREMENT_BATCH_SIZE};
#[cfg(feature = "clustering")]
use super::SharedNodeIdentity;
use super::{
    Arc, Database, DatabaseConfig, DatabaseDriver, Duration, IngressAuthority, IngressConfig,
    IngressRetirementOutcome, IngressUnitOfWork, IngressUowError, SmIngressId, SmIngressRepository,
    SmSessionId, WireHandledCount,
};

async fn authority() -> IngressAuthority {
    let database = Database::in_memory("ingress-lifecycle")
        .await
        .expect("open test database");
    IngressAuthority::for_test(database).await
}

#[tokio::test]
async fn stream_idle_waits_for_commit_and_releases_on_cancellation() {
    let authority = authority().await;
    let stream = SmSessionId::new("pending-commit");
    let held = authority.hold_test_commit(&stream).await;
    assert!(
        !authority
            .wait_for_stream_idle(&stream, Duration::from_millis(1))
            .await
    );
    assert!(
        authority
            .wait_for_stream_idle(&SmSessionId::new("other"), Duration::from_millis(10))
            .await
    );
    drop(held);
    assert!(
        authority
            .wait_for_stream_idle(&stream, Duration::from_millis(10))
            .await
    );
}

async fn seed_retirement_message(
    authority: &IngressAuthority,
    stream: SmIngressId,
    position: u32,
    receipt_complete: bool,
) -> waddle_xmpp::ingress::MessageKey {
    use crate::ingress_uow::{
        CanonicalMessageRepository, EffectIntentRepository, EffectReceiptRepository,
    };
    use waddle_xmpp::ingress::{IngressEffectIntent, IngressOrdinal, MessageKey, SemanticDigest};
    let key = MessageKey::new();
    let intent = IngressEffectIntent::RouteOccupantPm {
        recipient: "juliet@example.com/phone".parse().expect("recipient"),
        sender: "romeo@example.com/phone".parse().expect("sender"),
    };
    let mut transaction = authority
        .uow
        .begin()
        .await
        .expect("seed message transaction");
    CanonicalMessageRepository::record_message(
        &mut transaction,
        key,
        &SemanticDigest::from_storage(1, [1; 32]).expect("digest"),
        None,
    )
    .await
    .expect("record canonical message");
    EffectIntentRepository::reconcile(&mut transaction, key, std::slice::from_ref(&intent), false)
        .await
        .expect("record pending intent");
    if receipt_complete {
        let receipt = super::durable::receipt_key(&intent).expect("receipt identity");
        EffectReceiptRepository::record_receipt(
            &mut transaction,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("record completed effect");
    }
    SmIngressRepository::insert_sm_ref(
        &mut transaction,
        stream,
        IngressOrdinal::from_storage(u64::from(position)).expect("ordinal"),
        WireHandledCount::from_storage(position),
        key,
    )
    .await
    .expect("record stream reference");
    transaction
        .commit()
        .await
        .expect("commit retirement fixture");
    key
}

async fn assert_retired_message(
    authority: &IngressAuthority,
    key: waddle_xmpp::ingress::MessageKey,
    terminal: bool,
) {
    let guard = authority.database.guard().await.expect("database guard");
    let mut rows = guard.query(
            "SELECT terminal_at IS NOT NULL FROM ingress_messages WHERE CAST(message_key AS TEXT) = ?",
            crate::db_params![key.to_storage().to_string()],
        ).await.expect("read retired canonical message");
    assert_eq!(
        rows.next()
            .await
            .expect("row read")
            .expect("canonical message retained")
            .get::<bool>(0)
            .expect("terminal state"),
        terminal
    );
}

async fn enrollment_checkpoint_and_retirement_round_trip(authority: &IngressAuthority) {
    let stream = SmSessionId::new("enrolled-stream");
    let id = authority.enroll_stream(&stream).await.expect("enroll");
    assert_eq!(
        authority
            .enroll_stream(&stream)
            .await
            .expect("enroll twice"),
        id
    );
    assert_eq!(
        authority.lookup_stream(&stream).await.expect("lookup"),
        Some(id)
    );
    assert_eq!(
        authority
            .next_retirement_candidates()
            .await
            .expect("candidate page"),
        vec![stream.clone()]
    );
    authority.mark_retirement_candidate_attempted(&stream).await;
    assert_eq!(
        authority
            .next_retirement_candidates()
            .await
            .expect("candidate cursor wraps"),
        vec![stream.clone()]
    );
    authority
        .flush_checkpoint(id, WireHandledCount::from_storage(3))
        .await
        .expect("flush checkpoint");
    assert_eq!(
        authority
            .load_resume_checkpoint(&stream)
            .await
            .expect("checkpoint"),
        Some(WireHandledCount::from_storage(3))
    );
    #[cfg(feature = "clustering")]
    if authority.database.driver() == DatabaseDriver::Postgres {
        authority.database.execute("INSERT INTO clustering_claims (entity, entity_type) VALUES ('sm_session:enrolled-stream', 'sm_session')").await.expect("retain promotion claim");
        assert_eq!(
            authority
                .forget_stream(&stream)
                .await
                .expect("retirement while claimed"),
            IngressRetirementOutcome::DeferredClaim
        );
        assert_eq!(
            authority
                .lookup_stream(&stream)
                .await
                .expect("retained ingress stream"),
            Some(id)
        );
        authority
            .database
            .execute("DELETE FROM clustering_claims WHERE entity = 'sm_session:enrolled-stream'")
            .await
            .expect("confirm promotion and release claim");
    }
    let pending = seed_retirement_message(authority, id, 1, false).await;
    let complete = seed_retirement_message(authority, id, 2, true).await;
    assert_eq!(
        authority.forget_stream(&stream).await.expect("retire"),
        IngressRetirementOutcome::Deleted
    );
    assert_retired_message(authority, pending, false).await;
    assert_retired_message(authority, complete, true).await;
    let mut transaction = authority.uow.begin().await.expect("read retired refs");
    assert!(SmIngressRepository::refs_for_stream_batch(
        &mut transaction,
        id,
        RETIREMENT_BATCH_SIZE
    )
    .await
    .expect("retired refs")
    .is_empty());
    transaction.commit().await.expect("close retired refs read");
    assert_eq!(
        authority
            .lookup_stream(&stream)
            .await
            .expect("lookup retired"),
        None
    );
    assert_eq!(
        authority
            .forget_stream(&stream)
            .await
            .expect("retire absent"),
        IngressRetirementOutcome::StreamMissing
    );
}

async fn retirement_preserves_committed_batches(authority: &IngressAuthority) {
    let stream = SmSessionId::new("batched-retirement");
    let id = authority.enroll_stream(&stream).await.expect("enroll");
    let mut keys = Vec::new();
    for position in 1..=RETIREMENT_BATCH_SIZE * 2 + 3 {
        keys.push(seed_retirement_message(authority, id, position, true).await);
    }
    // A same-origin retry can bind another position to the same canonical
    // key. Its reference must survive until its own bounded page is retired.
    let position = RETIREMENT_BATCH_SIZE * 2 + 4;
    let mut transaction = authority
        .uow
        .begin()
        .await
        .expect("duplicate ref transaction");
    SmIngressRepository::insert_sm_ref(
        &mut transaction,
        id,
        waddle_xmpp::ingress::IngressOrdinal::from_storage(u64::from(position))
            .expect("duplicate ordinal"),
        WireHandledCount::from_storage(position),
        keys[0],
    )
    .await
    .expect("duplicate canonical ref");
    transaction.commit().await.expect("commit duplicate ref");
    let mut transaction = authority.uow.begin().await.expect("inspect first page");
    let first_page =
        SmIngressRepository::refs_for_stream_batch(&mut transaction, id, RETIREMENT_BATCH_SIZE)
            .await
            .expect("first page");
    assert!(first_page.iter().any(|(_, key)| *key == keys[0]));
    assert!(first_page
        .iter()
        .all(|(ordinal, _)| ordinal.to_storage() != u64::from(position)));
    transaction.commit().await.expect("close first page read");
    // Pause after the first transaction committed so the deadline cancels
    // the actual multi-batch operation at a deterministic boundary.
    let gate = Arc::new(tokio::sync::Notify::new());
    *authority
        .retirement_batch_gate
        .lock()
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = Some(gate.clone());
    {
        let retirement = authority.forget_stream(&stream);
        tokio::pin!(retirement);
        tokio::select! {
            outcome = &mut retirement => panic!("retirement passed the batch gate: {outcome:?}"),
            () = gate.notified() => {},
        }
        assert!(tokio::time::timeout(Duration::from_millis(1), retirement)
            .await
            .is_err());
    }
    *authority
        .retirement_batch_gate
        .lock()
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = None;
    assert_eq!(
        authority
            .lookup_stream(&stream)
            .await
            .expect("retained stream"),
        Some(id)
    );
    let mut transaction = authority.uow.begin().await.expect("inspect progress");
    let remaining =
        SmIngressRepository::refs_for_stream_batch(&mut transaction, id, RETIREMENT_BATCH_SIZE * 3)
            .await
            .expect("remaining refs");
    assert_eq!(remaining.len(), RETIREMENT_BATCH_SIZE as usize + 4);
    assert!(
        remaining
            .iter()
            .any(|(ordinal, key)| ordinal.to_storage() == u64::from(position) && *key == keys[0]),
        "duplicate reference outside the selected page must survive"
    );
    assert!(remaining.iter().all(|entry| !first_page.contains(entry)));
    transaction.commit().await.expect("close progress read");
    for key in &keys {
        assert_retired_message(
            authority,
            *key,
            first_page
                .iter()
                .any(|(_, selected_key)| selected_key == key),
        )
        .await;
    }
    assert_eq!(
        authority
            .forget_stream(&stream)
            .await
            .expect("resume retirement"),
        IngressRetirementOutcome::Deleted
    );
    assert_eq!(
        authority
            .lookup_stream(&stream)
            .await
            .expect("deleted stream"),
        None
    );
    let mut transaction = authority.uow.begin().await.expect("inspect final refs");
    assert!(SmIngressRepository::refs_for_stream_batch(
        &mut transaction,
        id,
        RETIREMENT_BATCH_SIZE,
    )
    .await
    .expect("final refs")
    .is_empty());
    transaction.commit().await.expect("close final read");
    for key in keys {
        assert_retired_message(authority, key, true).await;
    }
}

#[tokio::test]
async fn sqlite_retirement_preserves_committed_batches_after_cancellation() {
    let authority = authority().await;
    crate::db::MigrationRunner::global()
        .run(&authority.database)
        .await
        .expect("migrate ingress");
    retirement_preserves_committed_batches(&authority).await;
}

#[tokio::test]
async fn retirement_scan_pages_past_live_streams_and_wraps() {
    let authority = authority().await;
    crate::db::MigrationRunner::global()
        .run(&authority.database)
        .await
        .expect("migrate");
    for index in 0..65 {
        authority
            .enroll_stream(&SmSessionId::new(format!("stream-{index:03}")))
            .await
            .expect("enroll");
    }
    let first = authority
        .next_retirement_candidates()
        .await
        .expect("first page");
    assert_eq!(first.len(), 64);
    authority
        .mark_retirement_candidate_attempted(first.last().expect("first page tail"))
        .await;

    assert_eq!(
        authority
            .next_retirement_candidates()
            .await
            .expect("tail page"),
        vec![SmSessionId::new("stream-064")]
    );
    authority
        .mark_retirement_candidate_attempted(&SmSessionId::new("stream-064"))
        .await;
    assert_eq!(
        authority
            .next_retirement_candidates()
            .await
            .expect("wrapped page"),
        first
    );
}

#[tokio::test]
async fn sqlite_enrollment_checkpoint_and_retirement() {
    let authority = authority().await;
    crate::db::MigrationRunner::global()
        .run(&authority.database)
        .await
        .expect("migrate ingress");
    enrollment_checkpoint_and_retirement_round_trip(&authority).await;
}

#[tokio::test]
async fn postgres_enrollment_checkpoint_and_retirement() {
    postgres_retirement_test(false).await;
}

#[tokio::test]
async fn postgres_retirement_preserves_committed_batches_after_cancellation() {
    postgres_retirement_test(true).await;
}

async fn postgres_retirement_test(batches: bool) {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping postgres retirement test: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("postgres admin");
    let schema = format!("ingress_lifecycle_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create schema");
    let mut url = url::Url::parse(&database_url).expect("database URL");
    url.query_pairs_mut()
        .append_pair("options", &format!("-c search_path={schema}"));
    let config = DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, url.to_string());
    let db = Database::from_config("ingress-lifecycle", &config)
        .await
        .expect("database");
    crate::db::MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrate");
    let lineage = test_lineage_config();
    let mut authority = IngressAuthority::for_test(db.clone()).await;
    #[cfg(feature = "clustering")]
    {
        db.execute("CREATE TABLE IF NOT EXISTS clustering_claims (entity TEXT NOT NULL, entity_type TEXT NOT NULL, PRIMARY KEY (entity, entity_type))").await.expect("claims schema");
        authority.uow = IngressUnitOfWork::open_with_node_identity(
            db,
            lineage,
            SharedNodeIdentity::new(waddle_xmpp::ownership::NodeIdentity::new(
                "lifecycle",
                "test",
            )),
        )
        .expect("unit of work");
    }
    #[cfg(not(feature = "clustering"))]
    {
        authority.uow = IngressUnitOfWork::open(db, lineage).expect("unit of work");
    }
    if batches {
        retirement_preserves_committed_batches(&authority).await;
    } else {
        enrollment_checkpoint_and_retirement_round_trip(&authority).await;
    }
    drop(authority);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
}

/// SQLite has one writer: the authority shares the global handle, so even
/// a private in-memory database (the test fleet and dev mode) boots.
#[tokio::test]
async fn boot_shares_the_global_sqlite_handle() {
    let database = Database::in_memory("ingress-boot")
        .await
        .expect("open test database");
    crate::db::MigrationRunner::single()
        .run(&database)
        .await
        .expect("migrate");
    let lineage = test_lineage_config();
    crate::db::lineage::enroll(&database, &lineage)
        .await
        .expect("enroll lineage");
    let authority = IngressAuthority::try_new(
        IngressConfig::default(),
        database.clone(),
        lineage,
        #[cfg(feature = "clustering")]
        None,
    )
    .await
    .expect("boot on a shared in-memory database");
    authority.drain_and_join(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn stopped_authority_rejects_checkpoint_operations_before_database_access() {
    let authority = authority().await;
    assert!(authority.drain_and_join(Duration::from_secs(1)).await);
    assert!(matches!(
        authority
            .flush_checkpoint(SmIngressId::new(), WireHandledCount::from_storage(1))
            .await,
        Err(IngressUowError::AuthorityStopped)
    ));
    assert!(matches!(
        authority
            .load_resume_checkpoint(&SmSessionId::new("stopped-stream"))
            .await,
        Err(IngressUowError::AuthorityStopped)
    ));
}

#[tokio::test]
async fn authority_drain_waits_for_admitted_work_and_can_be_rejoined() {
    let authority = authority().await;
    let admitted = authority.admission.read().await;
    assert!(!authority.drain_and_join(Duration::from_millis(1)).await);
    assert!(authority.cancellation.is_cancelled());
    assert!(authority.force_stop.is_cancelled());
    drop(admitted);
    assert!(authority.drain_and_join(Duration::from_secs(1)).await);
}
