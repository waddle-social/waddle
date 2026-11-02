//! Prepared repository statements use the attested schema, without activating
//! the still-open live-delivery operation.
use super::{digest, envelope, Fixture};
use crate::config::LineageConfig;
use crate::db::physical_attestation::attested_pool;
use crate::db::DatabasePool;
use crate::ingress_uow::{
    ArchiveDispatchRepository, CanonicalMessageRepository, EffectIntentRepository,
    IngressUnitOfWork, IngressUowTransaction, SendAttemptRepository, SendClaim, SendLease,
    SendObligation,
};
use futures::FutureExt;
use std::time::Duration;
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent, MessageKey};
use waddle_xmpp::ownership::NodeIdentity;

#[tokio::test]
async fn attested_send_attempt_repositories_ignore_hostile_schema() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::open(&url).await;
    let result = std::panic::AssertUnwindSafe(run(&fixture))
        .catch_unwind()
        .await;
    fixture.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn hostile<'a>(uow: &'a IngressUnitOfWork, path: &str) -> IngressUowTransaction<'a> {
    let mut tx = uow.begin().await.expect("prepared transaction");
    tx.transaction_mut()
        .execute(&format!("SET LOCAL search_path = {path}"), ())
        .await
        .expect("hostile path");
    tx
}

async fn seed(uow: &IngressUnitOfWork, schema: &str, byte: u8) -> SendObligation {
    let key = MessageKey::new();
    let recipient: jid::FullJid = "bob@example.test/laptop".parse().expect("recipient");
    let intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.to_bare(),
        fanout: vec![recipient.clone()],
        route_identity: EffectMessageIdentity::capture_ordinal(1),
        prepared: None,
    };
    let receipt = crate::ingress::receipt_key(&intent).expect("receipt");
    let mut tx = uow.begin().await.expect("seed transaction");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &digest(byte),
        Some(&envelope("direct")),
    )
    .await
    .expect("canonical message");
    EffectIntentRepository::reconcile(&mut tx, key, &[intent], false)
        .await
        .expect("intent");
    tx.transaction_mut().execute(&format!("INSERT INTO {schema}.ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource) VALUES ('bob@example.test', 1, ?::uuid, ?, ?, ?)"), crate::db_params![key.to_storage().to_string(), receipt.kind.to_storage(), receipt.semantic_identity_hash.to_vec(), recipient.to_string()]).await.expect("archive dispatch");
    tx.commit().await.expect("seed commit");
    SendObligation {
        message: key,
        receipt,
        recipient,
    }
}

async fn claim(uow: &IngressUnitOfWork, path: &str, obligation: &SendObligation) -> SendLease {
    let mut tx = hostile(uow, path).await;
    let result = SendAttemptRepository::claim(
        &mut tx,
        obligation,
        &NodeIdentity::new("node", "incarnation"),
        Duration::from_secs(10),
    )
    .await
    .expect("claim");
    let SendClaim::Acquired(lease) = result else {
        panic!("expected acquired claim: {result:?}")
    };
    tx.commit().await.expect("claim commit");
    lease
}

async fn state(fixture: &Fixture, schema: &str, obligation: &SendObligation) -> Option<i32> {
    sqlx::query_scalar(&format!(
        "SELECT state FROM {schema}.ingress_send_attempts WHERE message_key = $1"
    ))
    .bind(obligation.message.to_storage())
    .fetch_optional(&fixture.admin)
    .await
    .expect("attempt state")
}

async fn stored_attempt(fixture: &Fixture, schema: &str, obligation: &SendObligation) -> String {
    sqlx::query_scalar(&format!(
        "SELECT pg_catalog.row_to_json(a)::text FROM {schema}.ingress_send_attempts a WHERE message_key = $1"
    ))
    .bind(obligation.message.to_storage())
    .fetch_one(&fixture.admin)
    .await
    .expect("stored attempt")
}

async fn expire(uow: &IngressUnitOfWork, path: &str, target: &str, obligation: &SendObligation) {
    let mut tx = hostile(uow, path).await;
    tx.transaction_mut()
        .execute(
            &format!("UPDATE {target}.ingress_send_attempts SET expires_at_ms = 0 WHERE message_key = ?::uuid"),
            crate::db_params![obligation.message.to_storage().to_string()],
        )
        .await
        .expect("expire attempt");
    tx.commit().await.expect("expiry commit");
}

async fn run(fixture: &Fixture) {
    let target = &fixture.target;
    for sql in [
        format!("REVOKE INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {target} FROM {}", fixture.runtime),
        format!("GRANT INSERT, UPDATE, DELETE ON {target}.ingress_messages, {target}.ingress_effect_intents, {target}.ingress_archive_dispatch, {target}.ingress_send_attempts TO {}", fixture.runtime),
        format!("GRANT UPDATE (id) ON {target}.ingress_protocol_epoch TO {}", fixture.runtime),
    ] { sqlx::query(&sql).execute(&fixture.admin).await.expect("restricted writer grants"); }
    let lineage_write: bool = sqlx::query_scalar("SELECT pg_catalog.has_any_column_privilege($1, $2, 'INSERT') OR pg_catalog.has_any_column_privilege($1, $2, 'UPDATE') OR pg_catalog.has_table_privilege($1, $2, 'DELETE')")
        .bind(&fixture.runtime).bind(format!("{target}._lineage")).fetch_one(&fixture.admin).await.expect("lineage privileges");
    assert!(!lineage_write);
    let attested = attested_pool(
        fixture.runtime_options.clone(),
        fixture.config.clone(),
        2,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool =
        DatabasePool::from_attested_postgres_pool("post-commit repositories", attested, 1, 1)
            .await
            .expect("pool");
    let uow = IngressUnitOfWork::open(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
    )
    .expect("UOW");
    let first = seed(&uow, target, 1).await;
    let rolled_back = seed(&uow, target, 2).await;
    let released = seed(&uow, target, 3).await;
    let recovered_lease = seed(&uow, target, 4).await;
    let recovered_start = seed(&uow, target, 5).await;
    let attacker = format!("postcommit_shadow_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {attacker}"))
        .execute(&fixture.admin)
        .await
        .expect("attacker schema");
    for table in [
        "ingress_messages",
        "ingress_archive_dispatch",
        "ingress_carbon_receipts",
        "ingress_send_attempts",
    ] {
        sqlx::query(&format!(
            "CREATE TABLE {attacker}.{table} (LIKE {target}.{table} INCLUDING ALL)"
        ))
        .execute(&fixture.admin)
        .await
        .expect("shadow table");
    }
    for sql in [
        format!("GRANT USAGE ON SCHEMA {attacker} TO {}", fixture.runtime),
        format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {attacker} TO {}",
            fixture.runtime
        ),
    ] {
        sqlx::query(&sql)
            .execute(&fixture.admin)
            .await
            .expect("attacker grants");
    }
    let key = first.message.to_storage();
    let kind = first.receipt.kind.to_storage();
    let hash = first.receipt.semantic_identity_hash.to_vec();
    sqlx::query(&format!("INSERT INTO {attacker}.ingress_carbon_receipts (message_key, kind, semantic_identity_hash, recipient) VALUES ($1, $2, $3, $4)"))
        .bind(key).bind(kind).bind(&hash).bind(first.recipient.to_string()).execute(&fixture.admin).await.expect("forged carbon receipt");
    sqlx::query(&format!("INSERT INTO {attacker}.ingress_send_attempts (message_key, kind, semantic_identity_hash, recipient, node_id, node_incarnation, lease_token, expires_at_ms, state, recovered) VALUES ($1, $2, $3, $4, 'attacker', 'attacker', 'attacker', 0, 2, 0)"))
        .bind(key).bind(kind).bind(&hash).bind(first.recipient.to_string()).execute(&fixture.admin).await.expect("forged attempt");
    sqlx::query(&format!("INSERT INTO {attacker}.ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource) VALUES ('forged@example.test', 9, $1, $2, $3, $4)"))
        .bind(key).bind(kind).bind(&hash).bind(first.recipient.to_string()).execute(&fixture.admin).await.expect("forged position");
    let path = format!("{attacker}, {target}");
    let mut tx = hostile(&uow, &path).await;
    let positions = ArchiveDispatchRepository::positions(&mut tx, first.message, &first.receipt)
        .await
        .expect("trusted positions");
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].archive.to_string(), "bob@example.test");
    assert!(
        !SendAttemptRepository::has_resource_receipt(&mut tx, &first)
            .await
            .expect("trusted carbon receipt")
    );
    tx.commit().await.expect("read commit");

    sqlx::query(&format!("INSERT INTO {target}.ingress_carbon_receipts (message_key, kind, semantic_identity_hash, recipient) VALUES ($1, $2, $3, $4)"))
        .bind(key).bind(kind).bind(&hash).bind(first.recipient.to_string())
        .execute(&fixture.admin).await.expect("trusted carbon receipt");
    let mut tx = hostile(&uow, &path).await;
    assert!(SendAttemptRepository::has_resource_receipt(&mut tx, &first)
        .await
        .expect("trusted positive receipt"));
    tx.commit().await.expect("positive receipt read");
    sqlx::query(&format!(
        "DELETE FROM {target}.ingress_carbon_receipts WHERE message_key = $1"
    ))
    .bind(key)
    .execute(&fixture.admin)
    .await
    .expect("remove trusted fixture receipt");

    for table in ["ingress_archive_dispatch", "ingress_carbon_receipts"] {
        sqlx::query(&format!(
            "ALTER TABLE {target}.{table} RENAME TO hidden_postcommit_relation"
        ))
        .execute(&fixture.admin)
        .await
        .expect("hide trusted table");
        let mut tx = hostile(&uow, &path).await;
        let failed = if table == "ingress_archive_dispatch" {
            ArchiveDispatchRepository::positions(&mut tx, first.message, &first.receipt)
                .await
                .is_err()
        } else {
            SendAttemptRepository::has_resource_receipt(&mut tx, &first)
                .await
                .is_err()
        };
        assert!(failed, "missing trusted {table} must fail");
        drop(tx);
        sqlx::query(&format!(
            "ALTER TABLE {target}.hidden_postcommit_relation RENAME TO {table}"
        ))
        .execute(&fixture.admin)
        .await
        .expect("restore trusted table");
    }

    let lease = claim(&uow, &path, &first).await;
    assert_eq!(state(fixture, target, &first).await, Some(0));
    assert_eq!(state(fixture, &attacker, &first).await, Some(2));
    // An unqualified update could otherwise succeed against the exact shadow token.
    sqlx::query(&format!(
        "DELETE FROM {attacker}.ingress_send_attempts WHERE message_key = $1"
    ))
    .bind(key)
    .execute(&fixture.admin)
    .await
    .expect("remove forged row");
    sqlx::query(&format!("INSERT INTO {attacker}.ingress_send_attempts SELECT * FROM {target}.ingress_send_attempts WHERE message_key = $1")).bind(key).execute(&fixture.admin).await.expect("forge matching lease");
    sqlx::query(&format!(
        "ALTER TABLE {target}.ingress_send_attempts RENAME TO hidden_postcommit_relation"
    ))
    .execute(&fixture.admin)
    .await
    .expect("hide attempts");
    for operation in ["claim", "start", "complete", "release"] {
        let mut tx = hostile(&uow, &path).await;
        let failed = match operation {
            "claim" => SendAttemptRepository::claim(
                &mut tx,
                &first,
                &NodeIdentity::new("node", "incarnation"),
                Duration::from_secs(10),
            )
            .await
            .is_err(),
            "start" => SendAttemptRepository::start(&mut tx, &lease).await.is_err(),
            "complete" => SendAttemptRepository::complete(&mut tx, &lease)
                .await
                .is_err(),
            _ => SendAttemptRepository::release_proven_not_enqueued(&mut tx, &lease)
                .await
                .is_err(),
        };
        assert!(failed, "missing trusted attempts: {operation}");
        drop(tx);
    }
    sqlx::query(&format!(
        "ALTER TABLE {target}.hidden_postcommit_relation RENAME TO ingress_send_attempts"
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore attempts");
    assert_eq!(state(fixture, target, &first).await, Some(0));
    assert_eq!(state(fixture, &attacker, &first).await, Some(0));
    let mut tx = hostile(&uow, &path).await;
    assert!(SendAttemptRepository::start(&mut tx, &lease)
        .await
        .expect("start"));
    tx.commit().await.expect("start commit");
    let mut tx = hostile(&uow, &path).await;
    assert!(SendAttemptRepository::complete(&mut tx, &lease)
        .await
        .expect("complete"));
    tx.commit().await.expect("completion commit");
    let mut tx = hostile(&uow, &path).await;
    assert!(matches!(
        SendAttemptRepository::claim(
            &mut tx,
            &first,
            &NodeIdentity::new("node", "incarnation"),
            Duration::from_secs(10)
        )
        .await
        .expect("replay"),
        SendClaim::Completed
    ));
    tx.commit().await.expect("replay commit");
    assert_eq!(state(fixture, target, &first).await, Some(2));
    assert_eq!(state(fixture, &attacker, &first).await, Some(0));

    let mut tx = hostile(&uow, &path).await;
    let SendClaim::Acquired(stale) = SendAttemptRepository::claim(
        &mut tx,
        &rolled_back,
        &NodeIdentity::new("node", "incarnation"),
        Duration::from_secs(10),
    )
    .await
    .expect("rollback claim") else {
        panic!("claim")
    };
    drop(tx);
    let mut tx = hostile(&uow, &path).await;
    assert!(!SendAttemptRepository::start(&mut tx, &stale)
        .await
        .expect("rolled-back token"));
    tx.commit().await.expect("rollback verification");
    assert_eq!(state(fixture, target, &rolled_back).await, None);
    let lease = claim(&uow, &path, &released).await;
    let mut tx = hostile(&uow, &path).await;
    assert!(SendAttemptRepository::start(&mut tx, &lease)
        .await
        .expect("start released"));
    tx.commit().await.expect("start released commit");
    let mut tx = hostile(&uow, &path).await;
    assert!(
        SendAttemptRepository::release_proven_not_enqueued(&mut tx, &lease)
            .await
            .expect("release")
    );
    tx.commit().await.expect("release commit");
    assert_eq!(state(fixture, target, &released).await, None);

    // Recovery uses the conflict UPDATE and both retained-evidence release
    // branches. A definite failure of the new attempt must not erase an older
    // ambiguous start, and a stale token must not affect either schema.
    for (obligation, previously_started, expected_state) in
        [(&recovered_lease, false, 0), (&recovered_start, true, 1)]
    {
        let old = claim(&uow, &path, obligation).await;
        if previously_started {
            let mut tx = hostile(&uow, &path).await;
            assert!(SendAttemptRepository::start(&mut tx, &old)
                .await
                .expect("old start"));
            tx.commit().await.expect("old start commit");
        }
        expire(&uow, &path, target, obligation).await;
        sqlx::query(&format!("INSERT INTO {attacker}.ingress_send_attempts SELECT * FROM {target}.ingress_send_attempts WHERE message_key = $1"))
            .bind(obligation.message.to_storage()).execute(&fixture.admin).await.expect("shadow recovered token");
        let shadow_before = stored_attempt(fixture, &attacker, obligation).await;
        let replacement = claim(&uow, &path, obligation).await;
        let mut tx = hostile(&uow, &path).await;
        assert!(!SendAttemptRepository::start(&mut tx, &old)
            .await
            .expect("stale start"));
        assert!(SendAttemptRepository::start(&mut tx, &replacement)
            .await
            .expect("replacement start"));
        assert!(
            SendAttemptRepository::release_proven_not_enqueued(&mut tx, &replacement)
                .await
                .expect("recovered release")
        );
        tx.commit().await.expect("recovered release commit");
        assert_eq!(
            state(fixture, target, obligation).await,
            Some(expected_state)
        );
        assert_eq!(
            state(fixture, &attacker, obligation).await,
            Some(expected_state)
        );
        assert_eq!(
            stored_attempt(fixture, &attacker, obligation).await,
            shadow_before
        );
    }
}
