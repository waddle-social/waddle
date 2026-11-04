//! Prepared SM writes target their attested relations; retirement is excluded.

use super::{count, digest, Fixture};
use crate::config::LineageConfig;
use crate::db::physical_attestation::attested_pool;
use crate::db::{DatabaseConfig, DatabaseDriver, DatabasePool, MigrationRunner, PoolConfig};
use crate::ingress_substrate::{FrontierOutcome, IngressSubstrateError, MessageWriteOutcome};
use crate::ingress_uow::{
    CanonicalMessageRepository, IngressUnitOfWork, IngressUowError, IngressUowTransaction,
    SmIngressRepository, SmIngressStreamRepository,
};
use futures::FutureExt;
use std::time::Duration;
use waddle_xmpp::ingress::{IngressOrdinal, MessageKey, SmIngressId, WireHandledCount};
use waddle_xmpp::pending_delivery::SmSessionId;

async fn seed_sm_shadows(
    fixture: &Fixture,
    tx: &mut IngressUowTransaction<'_>,
    stream: &SmSessionId,
    id: SmIngressId,
    key: MessageKey,
    shadow_key: MessageKey,
) {
    fixture.seed_shadows(tx).await;
    fixture.grant_temp(true).await;
    for sql in [
        "CREATE TEMP TABLE ingress_sm_streams (sm_ingress_id uuid PRIMARY KEY, stream_id text UNIQUE NOT NULL, handled_ordinal numeric NOT NULL DEFAULT 0, checkpoint_h bigint NOT NULL DEFAULT 0, wire_generation bigint NOT NULL DEFAULT 0, row_revision bigint NOT NULL DEFAULT 0, updated_at timestamptz NOT NULL DEFAULT now()) ON COMMIT PRESERVE ROWS",
        "CREATE TEMP TABLE ingress_sm_refs (sm_ingress_id uuid NOT NULL, ingress_ordinal numeric NOT NULL, wire_h bigint NOT NULL, wire_generation bigint NOT NULL, message_key uuid NOT NULL, PRIMARY KEY (sm_ingress_id, ingress_ordinal), UNIQUE (sm_ingress_id, wire_generation, wire_h)) ON COMMIT PRESERVE ROWS",
    ] {
        tx.transaction_mut().execute(sql, ()).await.expect("create SM shadow");
    }
    fixture.grant_temp(false).await;
    for message in [key, shadow_key] {
        tx.transaction_mut()
            .execute(
                "INSERT INTO pg_temp.ingress_messages (message_key, retention_eligible_at) VALUES (?::uuid, now())",
                crate::db_params![message.to_storage().to_string()],
            )
            .await
            .expect("shadow parent");
    }
    tx.transaction_mut()
        .execute(
            "INSERT INTO pg_temp.ingress_sm_streams (sm_ingress_id, stream_id, wire_generation) VALUES (?::uuid, ?, 3)",
            crate::db_params![id.to_storage().to_string(), stream.as_str()],
        )
        .await
        .expect("shadow stream");
    tx.transaction_mut()
        .execute(
            "INSERT INTO pg_temp.ingress_sm_refs (sm_ingress_id, ingress_ordinal, wire_h, wire_generation, message_key) VALUES (?::uuid, 1, 7, 3, ?::uuid)",
            crate::db_params![id.to_storage().to_string(), shadow_key.to_storage().to_string()],
        )
        .await
        .expect("shadow binding");
}

#[tokio::test]
async fn attested_sm_pair_ignores_hostile_shadows_and_missing_relations() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::open(&url).await;
    let result = std::panic::AssertUnwindSafe(postgres_case(&fixture))
        .catch_unwind()
        .await;
    fixture.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn postgres_case(fixture: &Fixture) {
    let attested = attested_pool(
        fixture.runtime_options.clone(),
        fixture.config.clone(),
        2,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("qualified SM fixture", attested, 1, 1)
        .await
        .expect("shared pool with one ingress slot");
    let mut main = pool.global().begin().await.expect("hold main slot");
    let mut rows = main
        .query("SELECT pg_catalog.pg_backend_pid()::bigint", ())
        .await
        .expect("main pid");
    let main_pid: i64 = rows
        .next()
        .await
        .expect("main row")
        .expect("main pid row")
        .get(0)
        .expect("main pid value");
    drop(rows);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), pool.global().begin())
            .await
            .is_err(),
        "the main slot is occupied"
    );
    let uow = IngressUnitOfWork::open(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
    )
    .expect("ingress UoW");
    let stream = SmSessionId::new("qualified-sm-stream");
    let id = SmIngressId::new();
    let key = MessageKey::new();
    let other = MessageKey::new();
    let shadow_key = MessageKey::new();
    let first = IngressOrdinal::FIRST;
    let second = IngressOrdinal::from_storage(2).expect("second ordinal");
    let h = WireHandledCount::from_storage(7);
    let mut tx = uow.begin().await.expect("prepared transaction");
    assert!(tx.receipt_schema().is_some());
    let pid = count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
    assert_ne!(pid, main_pid);
    seed_sm_shadows(fixture, &mut tx, &stream, id, key, shadow_key).await;
    let raw =
        crate::ingress_substrate::insert_sm_ref(tx.transaction_mut(), id, second, h, key).await;
    assert!(matches!(
        raw,
        Err(IngressSubstrateError::UnsupportedLiveEpoch)
    ));

    SmIngressStreamRepository::mint_reserved(&mut tx, &stream, id)
        .await
        .expect("mint reserved");
    assert_eq!(
        SmIngressStreamRepository::mint(&mut tx, &stream)
            .await
            .expect("mint replay"),
        id
    );
    let new_stream = SmSessionId::new("qualified-sm-fresh");
    let fresh_id = SmIngressStreamRepository::mint(&mut tx, &new_stream)
        .await
        .expect("mint fresh");
    assert_ne!(id, fresh_id);
    assert_eq!(
        SmIngressStreamRepository::lock_single_node(&mut tx, &stream)
            .await
            .expect("stream lock"),
        Some((id, 0))
    );
    for message in [key, other] {
        CanonicalMessageRepository::record_message(&mut tx, message, &digest(7), None)
            .await
            .expect("trusted parent");
    }
    tx.transaction_mut().execute(
        &format!("UPDATE {}.ingress_messages SET retention_eligible_at = now() WHERE message_key = ?::uuid", fixture.target),
        crate::db_params![key.to_storage().to_string()],
    ).await.expect("eligible trusted parent");
    assert_eq!(
        SmIngressRepository::insert_sm_ref(&mut tx, id, first, h, key)
            .await
            .expect("qualified ref"),
        MessageWriteOutcome::Recorded
    );
    assert_eq!(
        SmIngressRepository::insert_sm_ref(&mut tx, id, first, h, key)
            .await
            .expect("exact replay"),
        MessageWriteOutcome::AlreadyRecorded
    );
    assert!(matches!(
        SmIngressRepository::insert_sm_ref(&mut tx, id, first, h, other).await,
        Err(IngressUowError::Substrate(
            IngressSubstrateError::SmOrdinalConflict
        ))
    ));
    assert!(matches!(
        SmIngressRepository::insert_sm_ref(
            &mut tx,
            id,
            first,
            WireHandledCount::from_storage(8),
            key
        )
        .await,
        Err(IngressUowError::Substrate(
            IngressSubstrateError::SmOrdinalConflict
        ))
    ));
    assert_eq!(
        SmIngressRepository::insert_sm_ref(
            &mut tx,
            id,
            second,
            WireHandledCount::from_storage(9),
            shadow_key
        )
        .await
        .expect("missing trusted parent"),
        MessageWriteOutcome::MessageVanished
    );
    assert_eq!(
        SmIngressRepository::lookup_wire_binding(&mut tx, id, h)
            .await
            .expect("qualified lookup"),
        Some((key, first))
    );
    assert_eq!(
        crate::ingress_substrate::lookup_wire_binding(tx.transaction_mut(), id, h)
            .await
            .expect("raw shadow lookup"),
        Some((shadow_key, first))
    );
    assert_eq!(
        SmIngressStreamRepository::advance_frontier(&mut tx, id, first, h)
            .await
            .expect("frontier"),
        FrontierOutcome::Advanced
    );
    assert_eq!(
        SmIngressStreamRepository::advance_frontier(&mut tx, id, first, h)
            .await
            .expect("frontier replay"),
        FrontierOutcome::Idempotent
    );
    SmIngressStreamRepository::flush_checkpoint(&mut tx, id, WireHandledCount::from_storage(8))
        .await
        .expect("checkpoint");
    SmIngressStreamRepository::flush_checkpoint(&mut tx, id, h)
        .await
        .expect("no rewind");
    assert_eq!(
        SmIngressStreamRepository::load_stream_checkpoint(&mut tx, id)
            .await
            .expect("checkpoint read"),
        Some(WireHandledCount::from_storage(8))
    );
    assert_eq!(
        SmIngressStreamRepository::lock_single_node(&mut tx, &stream)
            .await
            .expect("frontier lock"),
        Some((id, 1))
    );
    // A hostile name lookup points stream X to the real ingress id for Y.
    // Resume must select X from the trusted table before loading a checkpoint.
    SmIngressStreamRepository::flush_checkpoint(
        &mut tx,
        fresh_id,
        WireHandledCount::from_storage(99),
    )
    .await
    .expect("different stream checkpoint");
    tx.transaction_mut()
        .execute(
            "UPDATE pg_temp.ingress_sm_streams SET sm_ingress_id = ?::uuid WHERE stream_id = ?",
            crate::db_params![fresh_id.to_storage().to_string(), stream.as_str()],
        )
        .await
        .expect("spoof stream mapping");
    assert_eq!(
        SmIngressStreamRepository::lookup_unclaimed_for_retirement(&mut tx, &stream)
            .await
            .expect("raw shadow mapping"),
        Some(fresh_id)
    );
    let resume_id = SmIngressStreamRepository::lookup_unclaimed(&mut tx, &stream)
        .await
        .expect("qualified stream mapping")
        .expect("trusted stream exists");
    assert_eq!(resume_id, id);
    assert_eq!(
        SmIngressStreamRepository::load_stream_checkpoint(&mut tx, resume_id)
            .await
            .expect("qualified resume checkpoint"),
        Some(WireHandledCount::from_storage(8))
    );
    assert_eq!(
        SmIngressStreamRepository::load_stream_checkpoint(&mut tx, fresh_id)
            .await
            .expect("other checkpoint"),
        Some(WireHandledCount::from_storage(99))
    );
    assert_eq!(count(&mut tx, &format!("SELECT count(*) FROM {}.ingress_messages WHERE message_key = '{}'::uuid AND retention_eligible_at IS NULL", fixture.target, key.to_storage())).await, 1);
    assert_eq!(
        count(
            &mut tx,
            "SELECT count(*) FROM pg_temp.ingress_messages WHERE retention_eligible_at IS NOT NULL"
        )
        .await,
        2
    );
    assert_eq!(
        count(&mut tx, "SELECT count(*) FROM pg_temp.ingress_sm_refs").await,
        1
    );
    assert_eq!(count(&mut tx, "SELECT count(*) FROM pg_temp.ingress_sm_streams WHERE handled_ordinal = 0 AND checkpoint_h = 0").await, 1);
    assert_eq!(
        count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await,
        pid
    );
    tx.commit().await.expect("commit");

    let mut replay = uow.begin().await.expect("replay transaction");
    assert_eq!(
        count(&mut replay, "SELECT pg_catalog.pg_backend_pid()::bigint").await,
        pid
    );
    seed_sm_shadows(fixture, &mut replay, &stream, id, key, shadow_key).await;
    assert_eq!(
        count(&mut replay, &format!("SELECT count(*) FROM {}.ingress_sm_refs WHERE sm_ingress_id = '{}'::uuid AND ingress_ordinal = 1 AND wire_generation = 0", fixture.target, id.to_storage())).await,
        1,
        "the committed reference uses the trusted stream generation, not the shadow's generation 3"
    );
    assert_eq!(
        SmIngressStreamRepository::mint(&mut replay, &stream)
            .await
            .expect("mint replay"),
        id
    );
    assert_eq!(
        SmIngressRepository::insert_sm_ref(&mut replay, id, first, h, key)
            .await
            .expect("durable ref replay"),
        MessageWriteOutcome::AlreadyRecorded
    );
    assert_eq!(
        SmIngressRepository::lookup_wire_binding(&mut replay, id, h)
            .await
            .expect("durable binding"),
        Some((key, first))
    );
    replay.commit().await.expect("replay commit");

    let mut rollback = uow.begin().await.expect("rollback transaction");
    seed_sm_shadows(fixture, &mut rollback, &stream, id, key, shadow_key).await;
    let rolled_stream = SmSessionId::new("rollback-sm");
    let rolled_id = SmIngressStreamRepository::mint(&mut rollback, &rolled_stream)
        .await
        .expect("rollback mint");
    assert_eq!(
        SmIngressRepository::insert_sm_ref(&mut rollback, rolled_id, first, h, key)
            .await
            .expect("rollback ref"),
        MessageWriteOutcome::Recorded
    );
    drop(rollback);
    let mut check = uow.begin().await.expect("post rollback");
    assert_eq!(
        SmIngressStreamRepository::lock_single_node(&mut check, &rolled_stream)
            .await
            .expect("rolled back stream"),
        None
    );
    assert_eq!(
        count(
            &mut check,
            &format!("SELECT count(*) FROM {}.ingress_sm_refs", fixture.target)
        )
        .await,
        1
    );
    check.commit().await.expect("check");

    for (relation, operation) in [
        ("ingress_sm_streams", "mint"),
        ("ingress_sm_streams", "lookup"),
        ("ingress_sm_refs", "insert"),
    ] {
        let mut missing = uow.begin().await.expect("missing table transaction");
        seed_sm_shadows(fixture, &mut missing, &stream, id, key, shadow_key).await;
        missing
            .transaction_mut()
            .execute("UPDATE pg_temp.ingress_protocol_epoch SET epoch = 1", ())
            .await
            .expect("raw epoch");
        let renamed = format!("{relation}_held");
        sqlx::query(&format!(
            "ALTER TABLE {}.{relation} RENAME TO {renamed}",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("hide trusted SM table");
        assert_eq!(
            crate::ingress_substrate::insert_sm_ref(
                missing.transaction_mut(),
                id,
                second,
                WireHandledCount::from_storage(19),
                key
            )
            .await
            .expect("raw writes shadow"),
            MessageWriteOutcome::Recorded
        );
        assert_eq!(
            count(&mut missing, "SELECT count(*) FROM pg_temp.ingress_sm_refs").await,
            2
        );
        if operation == "mint" {
            assert!(
                SmIngressStreamRepository::mint(&mut missing, &SmSessionId::new("missing"))
                    .await
                    .is_err(),
                "missing trusted streams must fail closed"
            );
        } else if operation == "lookup" {
            missing.transaction_mut()
                .execute(
                    "UPDATE pg_temp.ingress_sm_streams SET sm_ingress_id = ?::uuid WHERE stream_id = ?",
                    crate::db_params![fresh_id.to_storage().to_string(), stream.as_str()],
                )
                .await
                .expect("spoof missing-table mapping");
            assert_eq!(
                SmIngressStreamRepository::lookup_unclaimed_for_retirement(&mut missing, &stream)
                    .await
                    .expect("raw missing-table mapping"),
                Some(fresh_id)
            );
            assert!(
                SmIngressStreamRepository::lookup_unclaimed(&mut missing, &stream)
                    .await
                    .is_err(),
                "missing trusted stream lookup must fail closed"
            );
        } else {
            assert!(
                SmIngressRepository::insert_sm_ref(
                    &mut missing,
                    id,
                    second,
                    WireHandledCount::from_storage(19),
                    key
                )
                .await
                .is_err(),
                "missing trusted refs must fail closed"
            );
        }
        drop(missing);
        sqlx::query(&format!(
            "ALTER TABLE {}.{renamed} RENAME TO {relation}",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("restore trusted SM table");
    }
    drop(main);
    drop(uow);
    drop(pool);
}

#[tokio::test]
async fn sqlite_shared_memory_sm_pair_replay_and_rollback() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, ":memory:");
    config.pool_size = 1;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("shared SQLite pool");
    MigrationRunner::single()
        .run(pool.global())
        .await
        .expect("migrations");
    let uow = IngressUnitOfWork::open(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: None,
            action: None,
        },
    )
    .expect("SQLite UoW");
    let stream = SmSessionId::new("sqlite-qualified-sm");
    let id = SmIngressId::new();
    let key = MessageKey::new();
    let other = MessageKey::new();
    let h = WireHandledCount::from_storage(7);
    let mut tx = uow.begin().await.expect("begin");
    assert!(tx.receipt_schema().is_none());
    SmIngressStreamRepository::mint_reserved(&mut tx, &stream, id)
        .await
        .expect("reserved");
    assert_eq!(
        SmIngressStreamRepository::mint(&mut tx, &stream)
            .await
            .expect("mint replay"),
        id
    );
    for message in [key, other] {
        CanonicalMessageRepository::record_message(&mut tx, message, &digest(1), None)
            .await
            .expect("parent");
    }
    assert_eq!(
        SmIngressRepository::insert_sm_ref(&mut tx, id, IngressOrdinal::FIRST, h, key)
            .await
            .expect("record"),
        MessageWriteOutcome::Recorded
    );
    assert!(matches!(
        SmIngressRepository::insert_sm_ref(&mut tx, id, IngressOrdinal::FIRST, h, other).await,
        Err(IngressUowError::Substrate(
            IngressSubstrateError::SmOrdinalConflict
        ))
    ));
    assert_eq!(
        SmIngressStreamRepository::advance_frontier(&mut tx, id, IngressOrdinal::FIRST, h)
            .await
            .expect("advance"),
        FrontierOutcome::Advanced
    );
    SmIngressStreamRepository::flush_checkpoint(&mut tx, id, WireHandledCount::from_storage(8))
        .await
        .expect("flush");
    tx.commit().await.expect("commit");
    let mut replay = uow.begin().await.expect("replay");
    assert_eq!(
        SmIngressRepository::insert_sm_ref(&mut replay, id, IngressOrdinal::FIRST, h, key)
            .await
            .expect("exact replay"),
        MessageWriteOutcome::AlreadyRecorded
    );
    assert_eq!(
        SmIngressRepository::lookup_wire_binding(&mut replay, id, h)
            .await
            .expect("wire binding"),
        Some((key, IngressOrdinal::FIRST))
    );
    assert_eq!(
        SmIngressStreamRepository::load_stream_checkpoint(&mut replay, id)
            .await
            .expect("checkpoint"),
        Some(WireHandledCount::from_storage(8))
    );
    assert_eq!(
        SmIngressStreamRepository::lock_single_node(&mut replay, &stream)
            .await
            .expect("stream"),
        Some((id, 1))
    );
    assert_eq!(
        SmIngressStreamRepository::lookup_unclaimed(&mut replay, &stream)
            .await
            .expect("SQLite stream mapping"),
        Some(id)
    );
    let rolled_stream = SmSessionId::new("sqlite-rollback");
    let rolled_id = SmIngressStreamRepository::mint(&mut replay, &rolled_stream)
        .await
        .expect("mint rollback");
    assert_eq!(
        SmIngressRepository::insert_sm_ref(&mut replay, rolled_id, IngressOrdinal::FIRST, h, key)
            .await
            .expect("rollback ref"),
        MessageWriteOutcome::Recorded
    );
    drop(replay);
    let mut check = uow.begin().await.expect("post rollback");
    assert_eq!(
        SmIngressStreamRepository::lock_single_node(&mut check, &rolled_stream)
            .await
            .expect("rolled back"),
        None
    );
    assert_eq!(
        count(&mut check, "SELECT count(*) FROM ingress_sm_refs").await,
        1
    );
    check.commit().await.expect("finish");
}
