//! Source-aware alias laboratory under the attested restricted runtime role.

use super::Fixture;
use crate::config::LineageConfig;
use crate::db::physical_attestation::attested_pool;
use crate::db::DatabasePool;
use crate::ingress_substrate::{
    classify_source_alias, CanonicalSourceBinding, MessageEnvelope, SourceAliasClassification,
    SourceAliasOffer,
};
use crate::ingress_uow::IngressUnitOfWork;
use futures::FutureExt;
use jid::FullJid;
use std::time::Duration;
use uuid::Uuid;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, IngressOrdinal, MessageKey, NormalizedTarget, SemanticDigest,
    SmIngressId,
};
use waddle_xmpp_core::xep0359::OriginId;
use xmpp_parsers::message::Message;

#[derive(Clone)]
struct Input {
    sender: FullJid,
    target: NormalizedTarget,
    origin: OriginId,
    digest: SemanticDigest,
    source: CanonicalSourceBinding,
    envelope: MessageEnvelope,
}

impl Input {
    fn offer(&self) -> SourceAliasOffer<'_> {
        SourceAliasOffer {
            sender: &self.sender,
            target: &self.target,
            origin: &self.origin,
            digest: &self.digest,
            source: &self.source,
            envelope: &self.envelope,
        }
    }
}

fn input(principal: &AuthenticatedPrincipalRef, resource: &str, origin: &str) -> Input {
    let sender: FullJid = format!("alice@example.test/{resource}")
        .parse()
        .expect("sender");
    let target = NormalizedTarget::Bare("room@muc.example.test".parse().expect("target"));
    let origin = OriginId::new(origin);
    let mut message = Message::new(Some("room@muc.example.test".parse().expect("recipient")));
    message.from = Some(sender.clone().into());
    message.bodies.insert(Default::default(), "body".into());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, origin.as_str());
    let digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: target.clone(),
            server_authorities: vec![principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("digest input");
    Input {
        sender,
        target,
        origin,
        digest: waddle_xmpp::ingress::digest::v1::digest(&digest_input),
        source: CanonicalSourceBinding::new(principal.clone(), None),
        envelope: MessageEnvelope::new(message),
    }
}

async fn seed(
    fixture: &Fixture,
    ingress_budget: u32,
) -> (
    IngressUnitOfWork,
    AuthenticatedPrincipalRef,
    AuthenticatedPrincipalRef,
) {
    // Narrow the broad canonical fixture to the existing shared-facade ACL.
    for sql in [
        format!(
            "REVOKE UPDATE, DELETE ON {}.ingress_origin_aliases FROM {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "REVOKE DELETE ON {}.ingress_messages FROM {}",
            fixture.target, fixture.runtime
        ),
        format!("REVOKE UPDATE ON {}.ingress_messages FROM {}", fixture.target, fixture.runtime),
        format!("GRANT UPDATE (envelope_version, envelope, terminal_at, retention_eligible_at) ON {}.ingress_messages TO {}", fixture.target, fixture.runtime),
        format!("REVOKE INSERT, UPDATE, DELETE ON {}.sessions FROM {}", fixture.target, fixture.runtime),
        format!("GRANT UPDATE (last_used_at) ON {}.sessions TO {}", fixture.target, fixture.runtime),
        format!("REVOKE INSERT, UPDATE, DELETE ON {}.ingress_protocol_epoch FROM {}", fixture.target, fixture.runtime),
        format!("GRANT UPDATE (id) ON {}.ingress_protocol_epoch TO {}", fixture.target, fixture.runtime),
        format!("REVOKE INSERT, UPDATE, DELETE ON {}._lineage, {}._migrations FROM {}", fixture.target, fixture.target, fixture.runtime),
        format!("REVOKE UPDATE, DELETE ON {}.ingress_sm_refs, {}.ingress_sm_streams FROM {}", fixture.target, fixture.target, fixture.runtime),
    ] {
        sqlx::query(&sql)
            .execute(&fixture.admin)
            .await
            .expect("restrict runtime");
    }
    let alias_table = format!("{}.ingress_origin_aliases", fixture.target);
    let message_table = format!("{}.ingress_messages", fixture.target);
    let (alias_update, alias_delete, message_delete): (bool, bool, bool) = sqlx::query_as(
        "SELECT pg_catalog.has_any_column_privilege($1, $2, 'UPDATE'), pg_catalog.has_table_privilege($1, $2, 'DELETE'), pg_catalog.has_table_privilege($1, $3, 'DELETE')",
    ).bind(&fixture.runtime).bind(&alias_table).bind(&message_table).fetch_one(&fixture.admin).await.expect("runtime ACL");
    assert!(!alias_update && !alias_delete && !message_delete);
    let session_table = format!("{}.sessions", fixture.target);
    let (message_table_update, message_envelope_update, message_source_update, session_table_update, session_last_used_update, session_identity_update, session_delete):
        (bool, bool, bool, bool, bool, bool, bool) = sqlx::query_as(
            "SELECT pg_catalog.has_table_privilege($1, $2, 'UPDATE'), pg_catalog.has_column_privilege($1, $2, 'envelope', 'UPDATE'), pg_catalog.has_column_privilege($1, $2, 'source_bare_jid', 'UPDATE'), pg_catalog.has_table_privilege($1, $3, 'UPDATE'), pg_catalog.has_column_privilege($1, $3, 'last_used_at', 'UPDATE'), pg_catalog.has_column_privilege($1, $3, 'user_jid', 'UPDATE'), pg_catalog.has_table_privilege($1, $3, 'DELETE')",
        )
        .bind(&fixture.runtime).bind(&message_table).bind(&session_table)
        .fetch_one(&fixture.admin).await.expect("effective column grants");
    assert!(!message_table_update && message_envelope_update && !message_source_update);
    assert!(
        !session_table_update
            && session_last_used_update
            && !session_identity_update
            && !session_delete
    );
    let user = "alice@example.test";
    sqlx::query(&format!("INSERT INTO {}.users (jid, username, xmpp_localpart, created_at, updated_at) VALUES ($1, 'alice', 'alice', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')", fixture.target))
        .bind(user).execute(&fixture.admin).await.expect("user");
    let principals = [
        AuthenticatedPrincipalRef::new(
            user.parse().expect("bare"),
            AuthContextId::new(Uuid::new_v4()),
            AuthContextVersion::new(3),
            PrincipalAuthEpoch::new(5),
        ),
        AuthenticatedPrincipalRef::new(
            user.parse().expect("bare"),
            AuthContextId::new(Uuid::new_v4()),
            AuthContextVersion::new(7),
            PrincipalAuthEpoch::new(9),
        ),
    ];
    for principal in &principals {
        let id = Uuid::new_v4().to_string();
        sqlx::query(&format!("INSERT INTO {}.sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES ($1, $2, $1, $3, $4, $5, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')", fixture.target))
            .bind(id).bind(user).bind(principal.auth_context_id().as_uuid().to_string())
            .bind(i64::try_from(principal.auth_context_version().get()).expect("version"))
            .bind(i64::try_from(principal.auth_epoch().get()).expect("epoch"))
            .execute(&fixture.admin).await.expect("session");
    }
    let attested = attested_pool(
        fixture.runtime_options.clone(),
        fixture.config.clone(),
        ingress_budget + 1,
        Duration::from_secs(5),
    )
    .expect("attested pool");
    let pool =
        DatabasePool::from_attested_postgres_pool("source alias lab", attested, 1, ingress_budget)
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
    (uow, principals[0].clone(), principals[1].clone())
}

async fn wait_alias_insert(fixture: &Fixture, pid: i64) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT COALESCE((SELECT state = 'active' AND wait_event_type = 'Lock' AND query LIKE '%INSERT INTO%ingress_origin_aliases%' FROM pg_catalog.pg_stat_activity WHERE pid = $1), false)")
                .bind(i32::try_from(pid).expect("pid")).fetch_one(&fixture.admin).await.expect("wait state");
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("candidate waiting on alias conflict");
}

#[tokio::test]
async fn restricted_postgres_first_insert_races_preserve_the_first_source() {
    tokio::task::LocalSet::new()
        .run_until(restricted_postgres_first_insert_races_preserve_the_first_source_case())
        .await;
}

async fn restricted_postgres_first_insert_races_preserve_the_first_source_case() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::open(&url).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        let (uow, first_principal, sibling_principal) = seed(&fixture, 2).await;
        let mut denied = uow.begin().await.expect("role-denial UOW");
        assert!(denied.transaction_mut().query(
            &format!("SELECT a.message_key FROM {}.ingress_origin_aliases a FOR UPDATE OF a", fixture.target),
            (),
        ).await.is_err(), "runtime has no alias row-lock privilege");
        drop(denied);
        for (winner_commits, digest_conflict) in [(true, false), (false, false), (true, true)] {
            let alias = if digest_conflict { "race-digest" } else if winner_commits { "race-commit" } else { "race-abort" };
            let first_sm = SmIngressId::new();
            let second_sm = SmIngressId::new();
            // Admin seeds stream metadata solely for stored-linkage inspection;
            // this is not evidence of runtime SM-lane privileges or authority.
            for (id, suffix) in [(first_sm, "first"), (second_sm, "second")] {
                sqlx::query(&format!("INSERT INTO {}.ingress_sm_streams (sm_ingress_id, stream_id) VALUES ($1::uuid, $2)", fixture.target))
                    .bind(id.to_storage().to_string()).bind(format!("{alias}-{suffix}"))
                    .execute(&fixture.admin).await.expect("seed stream metadata");
            }
            let mut first = input(&first_principal, "phone", alias);
            first.source = CanonicalSourceBinding::new(first_principal.clone(), Some((first_sm, IngressOrdinal::FIRST)));
            let mut second = input(&sibling_principal, "tablet", alias);
            second.source = CanonicalSourceBinding::new(sibling_principal.clone(), Some((second_sm, IngressOrdinal::FIRST)));
            assert_eq!(first.digest, second.digest);
            if digest_conflict { second.digest = SemanticDigest::from_storage(1, [9; 32]).expect("conflicting digest"); }
            let first_key = MessageKey::new();
            let second_key = MessageKey::new();
            let first_tx = uow.begin().await.expect("first UOW");
            let (first_tx, first_result) = classify_source_alias(first_tx, first.offer(), || first_key).await.expect("first candidate");
            assert_eq!(first_result, SourceAliasClassification::NewBound(first_key));
            let second_uow = uow.clone();
            let second_input = second.clone();
            let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
            let waiter = tokio::task::spawn_local(async move {
                let mut tx = second_uow.begin().await.expect("second UOW");
                tx.set_local_timeouts(10_000, 10_000).await.expect("test wait bounds");
                let pid = super::count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
                pid_tx.send(pid).expect("send pid");
                let (tx, result) = classify_source_alias(tx, second_input.offer(), || second_key).await.expect("second candidate");
                tx.commit().await.expect("commit second");
                result
            });
            let pid = pid_rx.await.expect("pid");
            wait_alias_insert(&fixture, pid).await;
            if winner_commits { first_tx.commit().await.expect("commit winner"); } else { drop(first_tx); }
            let result = tokio::time::timeout(Duration::from_secs(5), waiter).await.expect("second completes").expect("second task");
            let expected_key = if winner_commits { first_key } else { second_key };
            if digest_conflict {
                assert_eq!(result, SourceAliasClassification::Conflict);
            } else if winner_commits {
                assert_eq!(result, SourceAliasClassification::ExistingBound { key: first_key, first_source: first.source.clone() });
            } else {
                assert_eq!(result, SourceAliasClassification::NewBound(second_key));
            }
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}.ingress_messages WHERE message_key IN ($1::uuid, $2::uuid)", fixture.target))
                .bind(first_key.to_storage().to_string()).bind(second_key.to_storage().to_string()).fetch_one(&fixture.admin).await.expect("candidate count");
            assert_eq!(count, 1, "losing candidate must not survive");
            if !digest_conflict {
                sqlx::query(&format!("INSERT INTO {}.ingress_sm_refs (sm_ingress_id, ingress_ordinal, wire_h, wire_generation, message_key) VALUES ($1::uuid, 1, 1, 0, $2::uuid)", fixture.target))
                    .bind(second_sm.to_storage().to_string()).bind(expected_key.to_storage().to_string())
                    .execute(&fixture.admin).await.expect("seed later reference");
                let linked: String = sqlx::query_scalar(&format!("SELECT message_key::text FROM {}.ingress_sm_refs WHERE sm_ingress_id = $1::uuid AND ingress_ordinal = 1", fixture.target))
                    .bind(second_sm.to_storage().to_string()).fetch_one(&fixture.admin).await.expect("read later reference");
                assert_eq!(linked, expected_key.to_storage().to_string());
            }
            let replay = input(&sibling_principal, "tablet", alias);
            let (mut tx, replay_result) = classify_source_alias(uow.begin().await.expect("replay UOW"), replay.offer(), MessageKey::new).await.expect("replay");
            let expected_source = if winner_commits { first.source.clone() } else { second.source.clone() };
            assert_eq!(replay_result, SourceAliasClassification::ExistingBound { key: expected_key, first_source: expected_source });
            let stored = crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, expected_key).await.expect("load").expect("envelope");
            assert_eq!(stored, if winner_commits { first.envelope.clone() } else { second.envelope.clone() });
            tx.commit().await.expect("end readback");
        }
        // A key collision cannot attach another alias to an unrelated row.
        let original = input(&first_principal, "phone", "key-base");
        let original_key = MessageKey::new();
        let (tx, result) = classify_source_alias(uow.begin().await.expect("base UOW"), original.offer(), || original_key).await.expect("base");
        assert_eq!(result, SourceAliasClassification::NewBound(original_key));
        tx.commit().await.expect("commit base");
        let collision = input(&first_principal, "phone", "key-collision");
        assert!(matches!(classify_source_alias(uow.begin().await.expect("collision UOW"), collision.offer(), || original_key).await,
            Err(crate::ingress_substrate::SourceAliasError::Substrate(crate::ingress_substrate::IngressSubstrateError::MessageContentConflict))));
        let collisions: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}.ingress_origin_aliases WHERE origin_id = 'key-collision'", fixture.target)).fetch_one(&fixture.admin).await.expect("collision count");
        assert_eq!(collisions, 0);

        // A hash/tuple inconsistency must not classify as a valid alias.
        sqlx::query(&format!("UPDATE {}.ingress_origin_aliases SET sender_bare_jid = 'mallory@example.test' WHERE message_key = $1::uuid", fixture.target))
            .bind(original_key.to_storage().to_string()).execute(&fixture.admin).await.expect("corrupt alias tuple");
        assert!(matches!(classify_source_alias(uow.begin().await.expect("corrupt alias UOW"), original.offer(), MessageKey::new).await,
            Err(crate::ingress_substrate::SourceAliasError::Substrate(crate::ingress_substrate::IngressSubstrateError::AliasIdentityConflict))));
        sqlx::query(&format!("UPDATE {}.ingress_origin_aliases SET sender_bare_jid = 'alice@example.test' WHERE message_key = $1::uuid", fixture.target))
            .bind(original_key.to_storage().to_string()).execute(&fixture.admin).await.expect("restore alias tuple");

        // Even a conflicting digest must not hide an invalid first source.
        sqlx::query(&format!("ALTER TABLE {}.ingress_messages DISABLE TRIGGER ingress_messages_source_binding_guard", fixture.target)).execute(&fixture.admin).await.expect("disable fixture guard");
        sqlx::query(&format!("UPDATE {}.ingress_messages SET source_bare_jid = 'mallory@example.test' WHERE message_key = $1::uuid", fixture.target))
            .bind(original_key.to_storage().to_string()).execute(&fixture.admin).await.expect("corrupt source");
        sqlx::query(&format!("ALTER TABLE {}.ingress_messages ENABLE TRIGGER ingress_messages_source_binding_guard", fixture.target)).execute(&fixture.admin).await.expect("restore fixture guard");
        let mut differing = original.clone();
        differing.digest = SemanticDigest::from_storage(1, [6; 32]).expect("different digest");
        assert!(matches!(classify_source_alias(uow.begin().await.expect("bad source UOW"), differing.offer(), MessageKey::new).await,
            Err(crate::ingress_substrate::SourceAliasError::MalformedSource)));
        sqlx::query(&format!("ALTER TABLE {}.ingress_messages DROP CONSTRAINT ingress_messages_source_binding_check", fixture.target)).execute(&fixture.admin).await.expect("remove fixture constraint");
        sqlx::query(&format!("ALTER TABLE {}.ingress_messages DISABLE TRIGGER ingress_messages_source_binding_guard", fixture.target)).execute(&fixture.admin).await.expect("disable fixture guard");
        sqlx::query(&format!("UPDATE {}.ingress_messages SET source_auth_context_id = NULL WHERE message_key = $1::uuid", fixture.target))
            .bind(original_key.to_storage().to_string()).execute(&fixture.admin).await.expect("partial source tuple");
        sqlx::query(&format!("ALTER TABLE {}.ingress_messages ENABLE TRIGGER ingress_messages_source_binding_guard", fixture.target)).execute(&fixture.admin).await.expect("restore fixture guard");
        assert!(matches!(classify_source_alias(uow.begin().await.expect("malformed source UOW"), differing.offer(), MessageKey::new).await,
            Err(crate::ingress_substrate::SourceAliasError::MalformedSource)));

        // A historical bound row may have no envelope; replay cannot fill it.
        let bound = input(&first_principal, "phone", "bound-empty");
        let bound_key = MessageKey::new();
        let mut tx = uow.begin().await.expect("historical bound UOW");
        let result = crate::ingress_uow::CanonicalMessageRepository::resolve_and_record_alias_with_source(
            &mut tx, &bound.sender.to_bare(), &bound.target, &bound.origin, &bound.digest, &bound.source, || bound_key,
        ).await.expect("seed bound empty");
        assert!(matches!(result, waddle_xmpp::ingress::AliasResolution::Aliased(waddle_xmpp::ingress::AliasOutcome::Inserted(key)) if key == bound_key));
        tx.commit().await.expect("commit bound empty");
        let bound_sibling = input(&sibling_principal, "tablet", "bound-empty");
        let (mut tx, result) = classify_source_alias(uow.begin().await.expect("bound empty replay"), bound_sibling.offer(), MessageKey::new).await.expect("bound replay");
        assert_eq!(result, SourceAliasClassification::ExistingBound { key: bound_key, first_source: bound.source.clone() });
        assert_eq!(crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, bound_key).await.expect("bound envelope"), None);
        tx.commit().await.expect("end bound replay");
        let legacy = input(&first_principal, "phone", "legacy-empty");
        let legacy_key = MessageKey::new();
        let mut tx = uow.begin().await.expect("legacy UOW");
        let result = crate::ingress_uow::CanonicalMessageRepository::resolve_and_record_alias(
            &mut tx, &legacy.sender.to_bare(), &legacy.target, &legacy.origin, &legacy.digest, || legacy_key,
        ).await.expect("seed legacy");
        assert!(matches!(result, waddle_xmpp::ingress::AliasResolution::Aliased(waddle_xmpp::ingress::AliasOutcome::Inserted(key)) if key == legacy_key));
        tx.commit().await.expect("commit legacy");
        let (mut tx, result) = classify_source_alias(uow.begin().await.expect("legacy replay"), legacy.offer(), MessageKey::new).await.expect("legacy classification");
        assert_eq!(result, SourceAliasClassification::LegacyUnbound(legacy_key));
        assert_eq!(crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, legacy_key).await.expect("legacy envelope"), None);
        tx.commit().await.expect("end legacy replay");
        let shadow = input(&first_principal, "phone", "hostile-shadow");
        let mut tx = uow.begin().await.expect("shadow UOW");
        fixture.seed_shadows(&mut tx).await;
        fixture.grant_temp(true).await;
        tx.transaction_mut().execute("CREATE TEMP TABLE sessions (user_jid text, auth_context_id text, auth_context_version bigint, principal_auth_epoch bigint, expires_at text)", ()).await.expect("shadow sessions");
        fixture.grant_temp(false).await;
        let key = MessageKey::new();
        let (mut tx, result) = classify_source_alias(tx, shadow.offer(), || key).await.expect("qualified shadow classification");
        assert_eq!(result, SourceAliasClassification::NewBound(key));
        assert_eq!(super::count(&mut tx, "SELECT count(*) FROM pg_temp.ingress_messages").await, 0);
        tx.commit().await.expect("commit qualified shadow");
    }).catch_unwind().await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn wait_savepoint_sleep(fixture: &Fixture, pid: i64, command: &str) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT COALESCE((SELECT state = 'active' AND wait_event = 'PgSleep' AND query LIKE $2 FROM pg_catalog.pg_stat_activity WHERE pid = $1), false)",
            )
            .bind(i32::try_from(pid).expect("pid"))
            .bind(format!("%{command}%pg_sleep%"))
            .fetch_one(&fixture.admin).await.expect("savepoint wait");
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("savepoint command executed but completion is withheld");
}

#[tokio::test]
async fn restricted_postgres_savepoint_cancellation_drops_uow_and_recovers_one_slot() {
    tokio::task::LocalSet::new()
        .run_until(
            restricted_postgres_savepoint_cancellation_drops_uow_and_recovers_one_slot_case(),
        )
        .await;
}

async fn restricted_postgres_savepoint_cancellation_drops_uow_and_recovers_one_slot_case() {
    use crate::ingress_substrate::{SavepointPhase, STALL_SAVEPOINT_PHASE};
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::open(&url).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        let (uow, first_principal, sibling_principal) = seed(&fixture, 2).await;
        for phase in [
            SavepointPhase::Begin,
            SavepointPhase::Release,
            SavepointPhase::Rollback,
        ] {
            let alias = match phase {
                SavepointPhase::Begin => "cancel-begin",
                SavepointPhase::Release => "cancel-release",
                SavepointPhase::Rollback => "cancel-rollback",
            };
            let candidate = MessageKey::new();
            let winner_input = input(&first_principal, "phone", alias);
            let contender = input(&sibling_principal, "tablet", alias);
            let (winner, mut held_slot) = if phase == SavepointPhase::Rollback {
                let winner_tx = uow.begin().await.expect("winner UOW");
                let (tx, result) =
                    classify_source_alias(winner_tx, winner_input.offer(), MessageKey::new)
                        .await
                        .expect("winner alias");
                assert!(matches!(result, SourceAliasClassification::NewBound(_)));
                (Some(tx), None)
            } else {
                (
                    None,
                    Some(uow.begin().await.expect("reserve other ingress slot")),
                )
            };
            let task_uow = uow.clone();
            let task_input = contender.clone();
            let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
            let task = tokio::task::spawn_local(async move {
                let mut tx = task_uow.begin().await.expect("candidate UOW");
                tx.set_local_timeouts(10_000, 10_000)
                    .await
                    .expect("test wait bounds");
                let pid = super::count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
                pid_tx.send(pid).expect("send pid");
                STALL_SAVEPOINT_PHASE
                    .scope(phase, async move {
                        let (tx, result) =
                            classify_source_alias(tx, task_input.offer(), || candidate).await?;
                        drop(tx);
                        Ok::<_, crate::ingress_substrate::SourceAliasError>(result)
                    })
                    .await
            });
            let pid = pid_rx.await.expect("pid");
            if let Some(winner) = winner {
                wait_alias_insert(&fixture, pid).await;
                winner.commit().await.expect("commit winner");
                held_slot = Some(uow.begin().await.expect("reserve freed ingress slot"));
            }
            let command = match phase {
                SavepointPhase::Begin => "SAVEPOINT waddle_source_alias_candidate",
                SavepointPhase::Rollback => "ROLLBACK TO SAVEPOINT waddle_source_alias_candidate",
                SavepointPhase::Release => "RELEASE SAVEPOINT waddle_source_alias_candidate",
            };
            wait_savepoint_sleep(&fixture, pid, command).await;
            task.abort();
            assert!(task.await.expect_err("cancelled classifier").is_cancelled());
            assert!(
                tokio::time::timeout(Duration::from_millis(100), uow.begin())
                    .await
                    .is_err(),
                "only free lane must remain occupied during cleanup"
            );
            let mut reused = tokio::time::timeout(Duration::from_secs(8), uow.begin())
                .await
                .expect("lane recovers")
                .expect("fresh UOW");
            let count: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM {}.ingress_messages WHERE message_key = $1::uuid",
                fixture.target
            ))
            .bind(candidate.to_storage().to_string())
            .fetch_one(&fixture.admin)
            .await
            .expect("candidate absence");
            assert_eq!(count, 0);
            let _ = super::count(&mut reused, "SELECT 1::bigint").await;
            reused
                .commit()
                .await
                .expect("reused transaction is healthy");
            drop(held_slot);
        }
    })
    .catch_unwind()
    .await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn wait_backend_query(fixture: &Fixture, pid: i64, needle: &str, event_type: &str) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT COALESCE((SELECT state = 'active' AND wait_event_type = $2 AND query LIKE $3 FROM pg_catalog.pg_stat_activity WHERE pid = $1), false)",
            ).bind(i32::try_from(pid).expect("pid")).bind(event_type).bind(format!("%{needle}%"))
                .fetch_one(&fixture.admin).await.expect("backend query state");
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("backend reached expected wait");
}

#[tokio::test]
async fn restricted_postgres_gc_order_and_stale_alias_probe_fail_closed() {
    tokio::task::LocalSet::new()
        .run_until(restricted_postgres_gc_order_and_stale_alias_probe_fail_closed_case())
        .await;
}

async fn restricted_postgres_gc_order_and_stale_alias_probe_fail_closed_case() {
    use crate::ingress_substrate::{SourceAliasError, STALL_AFTER_PROBE};
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::open(&url).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        let (uow, principal, _) = seed(&fixture, 2).await;
        let first = input(&principal, "phone", "gc-classifier-first");
        let first_key = MessageKey::new();
        let (tx, result) = classify_source_alias(uow.begin().await.expect("first UOW"), first.offer(), || first_key).await.expect("first");
        assert_eq!(result, SourceAliasClassification::NewBound(first_key));
        tx.commit().await.expect("seed first");
        let (locked, result) = classify_source_alias(uow.begin().await.expect("locked UOW"), first.offer(), MessageKey::new).await.expect("lock alias");
        assert!(matches!(result, SourceAliasClassification::ExistingBound { key, .. } if key == first_key));
        let admin = fixture.admin.clone();
        let schema = fixture.target.clone();
        let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
        let gc = tokio::task::spawn_local(async move {
            let mut tx = admin.begin().await.expect("GC tx");
            let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()").fetch_one(&mut *tx).await.expect("pid");
            pid_tx.send(i64::from(pid)).expect("send pid");
            sqlx::query(&format!("SELECT 1 FROM {schema}.ingress_messages WHERE message_key = $1::uuid FOR UPDATE"))
                .bind(first_key.to_storage().to_string()).fetch_one(&mut *tx).await.expect("canonical lock");
            sqlx::query(&format!("DELETE FROM {schema}.ingress_origin_aliases WHERE message_key = $1::uuid"))
                .bind(first_key.to_storage().to_string()).execute(&mut *tx).await.expect("delete alias");
            sqlx::query(&format!("DELETE FROM {schema}.ingress_messages WHERE message_key = $1::uuid"))
                .bind(first_key.to_storage().to_string()).execute(&mut *tx).await.expect("delete message");
            tx.commit().await.expect("commit GC");
        });
        let pid = pid_rx.await.expect("GC pid");
        wait_backend_query(&fixture, pid, "FOR UPDATE", "Lock").await;
        locked.commit().await.expect("release classifier lock");
        gc.await.expect("GC completes");

        let second = input(&principal, "phone", "gc-probe-first");
        let second_key = MessageKey::new();
        let (tx, result) = classify_source_alias(uow.begin().await.expect("second UOW"), second.offer(), || second_key).await.expect("second");
        assert_eq!(result, SourceAliasClassification::NewBound(second_key));
        tx.commit().await.expect("seed second");
        let task_uow = uow.clone();
        let task_input = second.clone();
        let candidate = MessageKey::new();
        let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_local(async move {
            let mut tx = task_uow.begin().await.expect("probe UOW");
            tx.set_local_timeouts(10_000, 10_000).await.expect("test wait bounds");
            let pid = super::count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
            pid_tx.send(pid).expect("pid");
            STALL_AFTER_PROBE.scope((), async move {
                let (tx, result) = classify_source_alias(tx, task_input.offer(), || candidate).await?;
                drop(tx);
                Ok::<_, SourceAliasError>(result)
            }).await
        });
        let pid = pid_rx.await.expect("probe pid");
        wait_backend_query(&fixture, pid, "pg_sleep", "Timeout").await;
        let mut gc = fixture.admin.begin().await.expect("GC first tx");
        sqlx::query(&format!("SELECT 1 FROM {}.ingress_messages WHERE message_key = $1::uuid FOR UPDATE", fixture.target))
            .bind(second_key.to_storage().to_string()).fetch_one(&mut *gc).await.expect("lock canonical");
        sqlx::query(&format!("DELETE FROM {}.ingress_origin_aliases WHERE message_key = $1::uuid", fixture.target))
            .bind(second_key.to_storage().to_string()).execute(&mut *gc).await.expect("delete alias");
        sqlx::query(&format!("DELETE FROM {}.ingress_messages WHERE message_key = $1::uuid", fixture.target))
            .bind(second_key.to_storage().to_string()).execute(&mut *gc).await.expect("delete message");
        wait_backend_query(&fixture, pid, "FOR UPDATE", "Lock").await;
        gc.commit().await.expect("commit GC after classifier waits");
        assert!(matches!(task.await.expect("probe task"), Err(SourceAliasError::AliasChanged)));
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}.ingress_messages WHERE message_key = $1::uuid", fixture.target))
            .bind(candidate.to_storage().to_string()).fetch_one(&fixture.admin).await.expect("candidate count");
        assert_eq!(count, 0, "stale alias must not mint a replacement");
    }).catch_unwind().await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn restricted_postgres_delayed_commit_has_only_positive_readback_evidence() {
    tokio::task::LocalSet::new()
        .run_until(restricted_postgres_delayed_commit_has_only_positive_readback_evidence_case())
        .await;
}

async fn restricted_postgres_delayed_commit_has_only_positive_readback_evidence_case() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::open(&url).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        let (uow, principal, _) = seed(&fixture, 2).await;
        let random = Uuid::new_v4();
        let bytes = random.as_bytes();
        let gate = 0x6d61_6d61_0000_0000_i64
            | i64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        sqlx::query(&format!("CREATE FUNCTION {}.source_alias_commit_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_catalog.pg_advisory_xact_lock({gate}); RETURN NEW; END $$", fixture.target))
            .execute(&fixture.admin).await.expect("commit gate function");
        sqlx::query(&format!("CREATE CONSTRAINT TRIGGER source_alias_commit_gate AFTER INSERT ON {}.ingress_messages DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {}.source_alias_commit_gate()", fixture.target, fixture.target))
            .execute(&fixture.admin).await.expect("deferred commit gate");
        let mut gate_conn = fixture.admin.acquire().await.expect("gate session");
        sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)").bind(gate).execute(&mut *gate_conn).await.expect("hold gate");
        let offered = input(&principal, "phone", "uncertain-commit");
        let task_input = offered.clone();
        let task_uow = uow.clone();
        let key = MessageKey::new();
        let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_local(async move {
            let mut tx = task_uow.begin().await.expect("commit UOW");
            tx.set_local_timeouts(10_000, 10_000).await.expect("test wait bounds");
            let pid = super::count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
            let (tx, result) = classify_source_alias(tx, task_input.offer(), || key).await.expect("classify");
            assert_eq!(result, SourceAliasClassification::NewBound(key));
            pid_tx.send(pid).expect("pid");
            tx.commit().await
        });
        let pid = pid_rx.await.expect("pid");
        wait_backend_query(&fixture, pid, "COMMIT", "Lock").await;
        task.abort();
        assert!(task.await.expect_err("cancelled COMMIT").is_cancelled());
        let before: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}.ingress_messages WHERE message_key = $1::uuid", fixture.target))
            .bind(key.to_storage().to_string()).fetch_one(&fixture.admin).await.expect("pre-release observation");
        assert_eq!(before, 0, "absence while COMMIT is blocked is not rollback proof");
        sqlx::query("SELECT pg_catalog.pg_advisory_unlock($1)").bind(gate).execute(&mut *gate_conn).await.expect("release commit gate");
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let visible: bool = sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {}.ingress_messages WHERE message_key = $1::uuid)", fixture.target))
                    .bind(key.to_storage().to_string()).fetch_one(&fixture.admin).await.expect("observe committed row");
                if visible { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("commit became positively observable");
        // Fence the exact old backend before the independent replay/readback.
        let _terminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
            .bind(i32::try_from(pid).expect("pid")).fetch_one(&fixture.admin).await.expect("terminate old backend");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)")
                    .bind(i32::try_from(pid).expect("pid")).fetch_one(&fixture.admin).await.expect("backend status");
                if !exists { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("old backend fenced");
        let replay = input(&principal, "phone", "uncertain-commit");
        let (mut tx, result) = classify_source_alias(uow.begin().await.expect("readback UOW"), replay.offer(), MessageKey::new).await.expect("positive exact alias readback");
        assert_eq!(result, SourceAliasClassification::ExistingBound { key, first_source: offered.source.clone() });
        assert_eq!(crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, key).await.expect("read envelope"), Some(offered.envelope));
        tx.commit().await.expect("end readback");
    }).catch_unwind().await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn restricted_postgres_cancelled_unique_wait_is_fenced_before_orphan_check() {
    tokio::task::LocalSet::new()
        .run_until(restricted_postgres_cancelled_unique_wait_case())
        .await;
}

async fn restricted_postgres_cancelled_unique_wait_case() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::open(&url).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        let (uow, principal, sibling) = seed(&fixture, 2).await;
        let first = input(&principal, "phone", "cancel-unique-wait");
        let second = input(&sibling, "tablet", "cancel-unique-wait");
        let winner_key = MessageKey::new();
        let loser_key = MessageKey::new();
        let (winner, result) = classify_source_alias(uow.begin().await.expect("winner UOW"), first.offer(), || winner_key).await.expect("winner");
        assert_eq!(result, SourceAliasClassification::NewBound(winner_key));
        let task_uow = uow.clone();
        let task_input = second.clone();
        let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
        let loser = tokio::task::spawn_local(async move {
            let mut tx = task_uow.begin().await.expect("loser UOW");
            tx.set_local_timeouts(10_000, 10_000).await.expect("test wait bounds");
            let pid = super::count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
            pid_tx.send(pid).expect("pid");
            let (tx, result) = classify_source_alias(tx, task_input.offer(), || loser_key).await?;
            drop(tx);
            Ok::<_, crate::ingress_substrate::SourceAliasError>(result)
        });
        let pid = pid_rx.await.expect("pid");
        wait_alias_insert(&fixture, pid).await;
        loser.abort();
        assert!(loser.await.expect_err("cancelled loser").is_cancelled());
        winner.commit().await.expect("complete winning transaction");
        // Absence before this fence could merely mean the candidate is still
        // uncommitted on the blocked backend. Terminate that exact backend and
        // wait for its session to disappear before making any no-orphan claim.
        let _terminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
            .bind(i32::try_from(pid).expect("pid")).fetch_one(&fixture.admin).await.expect("fence loser");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)")
                    .bind(i32::try_from(pid).expect("pid")).fetch_one(&fixture.admin).await.expect("backend status");
                if !exists { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("loser backend fenced");
        let rows: Vec<String> = sqlx::query_scalar(&format!("SELECT message_key::text FROM {}.ingress_messages WHERE message_key IN ($1::uuid, $2::uuid)", fixture.target))
            .bind(winner_key.to_storage().to_string()).bind(loser_key.to_storage().to_string())
            .fetch_all(&fixture.admin).await.expect("post-fence rows");
        assert_eq!(rows, vec![winner_key.to_storage().to_string()]);
        let hold = uow.begin().await.expect("hold one lane slot");
        let mut reused = tokio::time::timeout(Duration::from_secs(8), uow.begin()).await.expect("other slot recovers").expect("fresh UOW");
        assert_eq!(super::count(&mut reused, "SELECT 1::bigint").await, 1);
        reused.commit().await.expect("healthy reused slot");
        drop(hold);
    }).catch_unwind().await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
