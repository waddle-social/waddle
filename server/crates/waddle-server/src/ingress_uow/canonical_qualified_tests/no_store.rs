//! A no-store direct acceptance must use the attested schema through terminalization.

use super::{acceptance, with_database, with_schema, Fixture};
use crate::config::LineageConfig;
use crate::db::physical_attestation::attested_pool;
use crate::db::{Database, DatabaseConfig, DatabaseDriver, DatabasePool};
use crate::ingress::commit::{commit_hooks, commit_submission};
use crate::ingress::effects::delivery::ExternalDeliveryEffect;
use crate::ingress::effects::Effect;
use crate::ingress::{
    ExternalEffect, IngressDecisionClass, IngressStreamIdentity, IngressSubmission,
};
use crate::ingress_uow::{CanonicalMessageRepository, IngressUnitOfWork};
use futures::FutureExt;
use sqlx::PgPool;
use std::time::Duration;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, EffectMessageIdentity, IngressEffectIntent, MessageKey,
    NormalizedTarget, SmIngressId, WireHandledCount,
};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::xep::xep0334::{add_hint, has_hint, Hint};

#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::{ClaimStore, EntityType, NodeIdentity, SharedNodeIdentity};

fn submission(
    principal: &AuthenticatedPrincipalRef,
    stream: &SmSessionId,
    sm: SmIngressId,
    origin: &str,
) -> IngressSubmission {
    let mut offered = acceptance::submission(principal, stream, sm, origin, "no-store-stamp");
    let resource: jid::FullJid = "bob@example.test/laptop".parse().expect("full recipient");
    let mut message = offered.plan.sanitized_message.clone();
    message.to = Some(resource.clone().into());
    add_hint(&mut message, Hint::NoStore);
    offered.target = NormalizedTarget::Full(resource);
    offered.digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: offered.target.clone(),
            server_authorities: vec![principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("no-store digest");
    offered
        .plan
        .intents
        .retain(|intent| matches!(intent, IngressEffectIntent::RouteDirect { .. }));
    let IngressEffectIntent::RouteDirect {
        route_identity: EffectMessageIdentity::StanzaId(stamp),
        ..
    } = &offered.plan.intents[0]
    else {
        panic!("stamped route identity");
    };
    let mut routed = message.clone();
    waddle_xmpp_core::xep0359::add_stanza_id(&mut routed, stamp);
    offered.plan.plan.retain(|planned| {
        matches!(
            &planned.effect,
            Effect::External(ExternalEffect::Delivery(
                ExternalDeliveryEffect::RouteToPeer { .. }
            ))
        )
    });
    let Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
        stanza,
        ..
    })) = &mut offered.plan.plan[0].effect
    else {
        panic!("direct route")
    };
    **stanza = waddle_xmpp::Stanza::Message(routed);
    offered.plan.sanitized_message = message;
    offered
}

async fn execute(db: &PgPool, sql: &str) {
    sqlx::query(sql).execute(db).await.expect("fixture SQL");
}

async fn count(db: &PgPool, schema: &str, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {schema}.{table}"))
        .fetch_one(db)
        .await
        .expect("row count")
}

async fn terminal(db: &PgPool, schema: &str, key: MessageKey) -> (bool, bool) {
    sqlx::query_as(&format!("SELECT terminal_at IS NOT NULL, retention_eligible_at IS NOT NULL FROM {schema}.ingress_messages WHERE message_key = $1"))
        .bind(key.to_storage()).fetch_one(db).await.expect("terminal state")
}

async fn reason(db: &PgPool, schema: &str, key: MessageKey) -> Option<String> {
    sqlx::query_scalar(&format!(
        "SELECT policy_discard_reason FROM {schema}.ingress_effect_receipts WHERE message_key = $1"
    ))
    .bind(key.to_storage())
    .fetch_one(db)
    .await
    .expect("receipt reason")
}

async fn reopen(db: &PgPool, schema: &str, key: MessageKey) {
    sqlx::query(&format!("UPDATE {schema}.ingress_messages SET terminal_at = NULL, retention_eligible_at = NULL WHERE message_key = $1"))
        .bind(key.to_storage()).execute(db).await.expect("reopen fixture row");
}

async fn replay(uow: &IngressUnitOfWork, path: &str, offered: &IngressSubmission) {
    let decision = commit_hooks::SEARCH_PATH
        .scope(path.to_owned(), commit_submission(uow, offered, 1))
        .await
        .expect("no-store replay");
    assert!(
        decision.external.is_empty(),
        "no-store replay cannot repeat the delivery"
    );
}

#[tokio::test]
async fn attested_no_store_acceptance_qualifies_terminalization_and_rolls_back() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::open(&url).await;
    let result = std::panic::AssertUnwindSafe(run(&fixture, &url))
        .catch_unwind()
        .await;
    fixture.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn run(fixture: &Fixture, url: &str) {
    let target = &fixture.target;
    let boundary = with_schema(&with_database(url, &fixture.database_name), target);
    let owner = Database::from_config(
        "no-store-owner",
        &DatabaseConfig::new(DatabaseDriver::Postgres, &boundary),
    )
    .await
    .expect("owner database");
    let mam = waddle_xmpp::mam::SqlxMamStorage::open(&boundary)
        .await
        .expect("MAM schema");
    crate::inbox::bootstrap_inbox_owner_schema(&owner)
        .await
        .expect("inbox schema");
    #[cfg(feature = "clustering")]
    crate::clustering::claims::PostgresClaimStore::new(owner.clone())
        .ensure_schema()
        .await
        .expect("claim schema");
    let principal = AuthenticatedPrincipalRef::new(
        "alice@example.test".parse().expect("alice"),
        AuthContextId::new(uuid::Uuid::new_v4()),
        AuthContextVersion::new(1),
        PrincipalAuthEpoch::new(1),
    );
    let stream = SmSessionId::new("no-store-stream");
    let sm = SmIngressId::new();
    sqlx::query(&format!("INSERT INTO {target}.users (jid, username, xmpp_localpart, created_at, updated_at) VALUES ($1, 'alice', 'alice', $2, $2)"))
        .bind(principal.bare_jid().to_string()).bind("2020-01-01T00:00:00Z").execute(&fixture.admin).await.expect("user");
    sqlx::query(&format!("INSERT INTO {target}.sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES ('s', $1, 'token', $2, 1, 1, $3, $3)"))
        .bind(principal.bare_jid().to_string()).bind(principal.auth_context_id().as_uuid().to_string()).bind("2020-01-01T00:00:00Z").execute(&fixture.admin).await.expect("session");
    #[cfg(feature = "clustering")]
    sqlx::query(&format!("INSERT INTO {target}.clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES ($1, $2, 'accept-owner', 'accept-incarnation', 7)"))
        .bind(format!("{}:{}", EntityType::SmSession.as_db_str(), stream.as_str())).bind(EntityType::SmSession.as_db_str())
        .execute(&fixture.admin).await.expect("claim");

    let attacker = format!("no_store_shadow_{}", uuid::Uuid::new_v4().simple());
    execute(&fixture.admin, &format!("CREATE SCHEMA {attacker}")).await;
    let tables = [
        "sessions",
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_sm_streams",
        "ingress_sm_refs",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_delivery_receipts",
        "ingress_effect_descendants",
        "ingress_send_attempts",
        "sm_ingress_appends",
        "ingress_archive_dispatch",
        "mam_messages",
        "inbox_entries",
    ];
    for table in tables {
        execute(
            &fixture.admin,
            &format!("CREATE TABLE {attacker}.{table} (LIKE {target}.{table} INCLUDING ALL)"),
        )
        .await;
        execute(
            &fixture.admin,
            &format!("GRANT SELECT ON {target}.{table} TO {}", fixture.runtime),
        )
        .await;
    }
    execute(
        &fixture.admin,
        &format!("CREATE TABLE {attacker}.pending_delivery (row_id text PRIMARY KEY)"),
    )
    .await;
    execute(
        &fixture.admin,
        &format!("GRANT USAGE ON SCHEMA {attacker} TO {}", fixture.runtime),
    )
    .await;
    execute(
        &fixture.admin,
        &format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {attacker} TO {}",
            fixture.runtime
        ),
    )
    .await;
    execute(
        &fixture.admin,
        &format!(
            "REVOKE INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {target} FROM {}",
            fixture.runtime
        ),
    )
    .await;
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_sm_streams",
        "ingress_sm_refs",
        "ingress_effect_intents",
        "ingress_effect_receipts",
    ] {
        execute(
            &fixture.admin,
            &format!(
                "GRANT INSERT, UPDATE, DELETE ON {target}.{table} TO {}",
                fixture.runtime
            ),
        )
        .await;
    }
    for (table, column) in [
        ("ingress_protocol_epoch", "id"),
        ("sessions", "last_used_at"),
        #[cfg(feature = "clustering")]
        ("clustering_claims", "entity_type"),
    ] {
        execute(
            &fixture.admin,
            &format!(
                "GRANT UPDATE ({column}) ON {target}.{table} TO {}",
                fixture.runtime
            ),
        )
        .await;
    }
    #[cfg(feature = "clustering")]
    execute(
        &fixture.admin,
        &format!(
            "GRANT SELECT ON {target}.clustering_claims TO {}",
            fixture.runtime
        ),
    )
    .await;
    let lineage_write: bool = sqlx::query_scalar("SELECT pg_catalog.has_any_column_privilege($1, $2, 'INSERT') OR pg_catalog.has_any_column_privilege($1, $2, 'UPDATE') OR pg_catalog.has_table_privilege($1, $2, 'DELETE')")
        .bind(&fixture.runtime).bind(format!("{target}._lineage")).fetch_one(&fixture.admin).await.expect("lineage privilege");
    assert!(!lineage_write);
    drop(mam);
    drop(owner);
    let pool = DatabasePool::from_attested_postgres_pool(
        "no-store",
        attested_pool(
            fixture.runtime_options.clone(),
            fixture.config.clone(),
            2,
            Duration::from_secs(2),
        )
        .expect("attested pool"),
        1,
        1,
    )
    .await
    .expect("database pool");
    let lineage = LineageConfig {
        deployment_uuid: Some(fixture.lineage.deployment_uuid),
        action: None,
    };
    #[cfg(feature = "clustering")]
    let uow = IngressUnitOfWork::open_with_node_identity(
        pool.ingress(),
        lineage,
        SharedNodeIdentity::new(NodeIdentity::new("accept-owner", "accept-incarnation")),
    )
    .expect("cluster UOW");
    #[cfg(not(feature = "clustering"))]
    let uow = IngressUnitOfWork::open(pool.ingress(), lineage).expect("UOW");
    let path = format!("{attacker}, {target}");
    let offered = submission(&principal, &stream, sm, "no-store-origin");
    let first = commit_hooks::SEARCH_PATH
        .scope(path.clone(), commit_submission(&uow, &offered, 1))
        .await
        .expect("no-store acceptance");
    assert!(first.class.advances());
    assert_eq!(first.external.len(), 1);
    let key = first.message_key.expect("canonical key");
    assert_eq!(
        reason(&fixture.admin, target, key).await.as_deref(),
        Some("storage_hint_forbids_handoff")
    );
    assert_eq!(terminal(&fixture.admin, target, key).await, (true, false));
    assert_eq!(count(&fixture.admin, target, "ingress_sm_refs").await, 1);
    assert_eq!(count(&fixture.admin, target, "mam_messages").await, 0);
    assert_eq!(count(&fixture.admin, target, "inbox_entries").await, 0);
    let mut tx = uow.begin().await.expect("read envelope");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("load envelope")
        .expect("envelope");
    assert!(envelope.message().bodies.is_empty());
    assert!(has_hint(envelope.message(), Hint::NoStore));
    tx.commit().await.expect("close read");
    replay(&uow, &path, &offered).await;

    // Fake custody and completed-attempt rows must not turn policy discard into delivery proof.
    sqlx::query(&format!("INSERT INTO {attacker}.sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) SELECT message_key::text, kind, semantic_identity_hash, 'bob@example.test/laptop', 'forged', 1, 0, 'forged', 0, 0 FROM {target}.ingress_effect_intents WHERE message_key = $1"))
        .bind(key.to_storage()).execute(&fixture.admin).await.expect("shadow custody");
    sqlx::query(&format!("INSERT INTO {attacker}.ingress_send_attempts (message_key, kind, semantic_identity_hash, recipient, node_id, node_incarnation, lease_token, expires_at_ms, state, recovered) SELECT message_key, kind, semantic_identity_hash, 'bob@example.test/laptop', 'forged', 'forged', 'forged', 0, 2, 0 FROM {target}.ingress_effect_intents WHERE message_key = $1"))
        .bind(key.to_storage()).execute(&fixture.admin).await.expect("shadow attempt");
    sqlx::query(&format!(
        "DELETE FROM {target}.ingress_effect_receipts WHERE message_key = $1"
    ))
    .bind(key.to_storage())
    .execute(&fixture.admin)
    .await
    .expect("clear receipt");
    replay(&uow, &path, &offered).await;
    assert_eq!(
        reason(&fixture.admin, target, key).await.as_deref(),
        Some("storage_hint_forbids_handoff")
    );

    // Real same-schema custody, and separately a completed attempt, are delivery proof.
    sqlx::query(&format!(
        "DELETE FROM {target}.ingress_effect_receipts WHERE message_key = $1"
    ))
    .bind(key.to_storage())
    .execute(&fixture.admin)
    .await
    .expect("clear receipt");
    sqlx::query(&format!("INSERT INTO {target}.sm_ingress_appends SELECT * FROM {attacker}.sm_ingress_appends WHERE message_key = $1::text"))
        .bind(key.to_storage()).execute(&fixture.admin).await.expect("trusted custody");
    replay(&uow, &path, &offered).await;
    assert_eq!(reason(&fixture.admin, target, key).await, None);
    sqlx::query(&format!(
        "DELETE FROM {target}.ingress_effect_receipts WHERE message_key = $1"
    ))
    .bind(key.to_storage())
    .execute(&fixture.admin)
    .await
    .expect("clear receipt");
    sqlx::query(&format!(
        "DELETE FROM {target}.sm_ingress_appends WHERE message_key = $1::text"
    ))
    .bind(key.to_storage())
    .execute(&fixture.admin)
    .await
    .expect("clear custody");
    sqlx::query(&format!("INSERT INTO {target}.ingress_send_attempts SELECT * FROM {attacker}.ingress_send_attempts WHERE message_key = $1"))
        .bind(key.to_storage()).execute(&fixture.admin).await.expect("trusted attempt");
    replay(&uow, &path, &offered).await;
    assert_eq!(reason(&fixture.admin, target, key).await, None);

    // The optional pending table and descendants must be resolved in the target schema.
    let mut transient = submission(&principal, &stream, sm, "retention-origin");
    transient.identity = IngressStreamIdentity::Ephemeral {
        principal: principal.clone(),
    };
    let accepted = commit_hooks::SEARCH_PATH
        .scope(path.clone(), commit_submission(&uow, &transient, 1))
        .await
        .expect("ephemeral acceptance");
    let transient_key = accepted.message_key.expect("ephemeral key");
    assert_eq!(
        terminal(&fixture.admin, target, transient_key).await,
        (true, true)
    );
    sqlx::query(&format!("INSERT INTO {attacker}.ingress_effect_descendants (message_key, kind, semantic_identity_hash, descendant_key) SELECT message_key, kind, semantic_identity_hash, 'forged-child' FROM {target}.ingress_effect_intents WHERE message_key = $1"))
        .bind(transient_key.to_storage()).execute(&fixture.admin).await.expect("shadow descendant");
    sqlx::query(&format!("INSERT INTO {attacker}.ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id) SELECT 'bob@example.test', 1, message_key, kind, semantic_identity_hash, '', 'forged-pending' FROM {target}.ingress_effect_intents WHERE message_key = $1"))
        .bind(transient_key.to_storage()).execute(&fixture.admin).await.expect("shadow dispatch");
    execute(
        &fixture.admin,
        &format!("INSERT INTO {attacker}.pending_delivery VALUES ('forged-pending')"),
    )
    .await;
    reopen(&fixture.admin, target, transient_key).await;
    replay(&uow, &path, &transient).await;
    assert_eq!(
        terminal(&fixture.admin, target, transient_key).await,
        (true, true),
        "shadow descendants and optional pending cannot delay retention"
    );
    sqlx::query(&format!("INSERT INTO {target}.ingress_effect_descendants (message_key, kind, semantic_identity_hash, descendant_key) SELECT message_key, kind, semantic_identity_hash, 'trusted-child' FROM {target}.ingress_effect_intents WHERE message_key = $1"))
        .bind(transient_key.to_storage()).execute(&fixture.admin).await.expect("trusted descendant");
    reopen(&fixture.admin, target, transient_key).await;
    replay(&uow, &path, &transient).await;
    assert_eq!(
        terminal(&fixture.admin, target, transient_key).await,
        (true, false),
        "trusted descendant delays retention"
    );
    sqlx::query(&format!(
        "DELETE FROM {target}.ingress_effect_descendants WHERE message_key = $1"
    ))
    .bind(transient_key.to_storage())
    .execute(&fixture.admin)
    .await
    .expect("clear descendant");
    execute(
        &fixture.admin,
        &format!("DELETE FROM {attacker}.pending_delivery"),
    )
    .await;
    assert_eq!(
        count(&fixture.admin, &attacker, "pending_delivery").await,
        0
    );
    execute(
        &fixture.admin,
        &format!("CREATE TABLE {target}.pending_delivery (row_id text PRIMARY KEY)"),
    )
    .await;
    execute(
        &fixture.admin,
        &format!(
            "GRANT SELECT ON {target}.pending_delivery TO {}",
            fixture.runtime
        ),
    )
    .await;
    execute(
        &fixture.admin,
        &format!("INSERT INTO {target}.pending_delivery VALUES ('trusted-pending')"),
    )
    .await;
    sqlx::query(&format!("INSERT INTO {target}.ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id) SELECT 'bob@example.test', 1, message_key, kind, semantic_identity_hash, '', 'trusted-pending' FROM {target}.ingress_effect_intents WHERE message_key = $1"))
        .bind(transient_key.to_storage()).execute(&fixture.admin).await.expect("trusted dispatch");
    reopen(&fixture.admin, target, transient_key).await;
    replay(&uow, &path, &transient).await;
    assert_eq!(
        terminal(&fixture.admin, target, transient_key).await,
        (true, false),
        "trusted pending delays retention"
    );
    execute(
        &fixture.admin,
        &format!("DELETE FROM {target}.pending_delivery WHERE row_id = 'trusted-pending'"),
    )
    .await;
    reopen(&fixture.admin, target, transient_key).await;
    replay(&uow, &path, &transient).await;
    assert_eq!(
        terminal(&fixture.admin, target, transient_key).await,
        (true, true),
        "a removed pending row no longer delays retention"
    );

    // A committed acceptance with a lost acknowledgement still owns its
    // one-shot policy receipt; retry cannot mint another live delivery.
    let mut ambiguous = submission(&principal, &stream, sm, "ambiguous-origin");
    ambiguous.identity = IngressStreamIdentity::Ephemeral {
        principal: principal.clone(),
    };
    let lost = commit_hooks::AMBIGUOUS_COMMIT
        .scope(
            true,
            commit_hooks::SEARCH_PATH.scope(path.clone(), commit_submission(&uow, &ambiguous, 1)),
        )
        .await
        .expect_err("commit acknowledgement is lost");
    assert_eq!(lost.class(), IngressDecisionClass::AmbiguousCommit);
    replay(&uow, &path, &ambiguous).await;

    // Losing a required target relation must roll back the whole offered h=2 acceptance.
    for (index, table) in [
        "sm_ingress_appends",
        "ingress_send_attempts",
        "ingress_effect_descendants",
        "ingress_effect_receipts",
    ]
    .iter()
    .enumerate()
    {
        execute(
            &fixture.admin,
            &format!("ALTER TABLE {target}.{table} RENAME TO hidden_relation"),
        )
        .await;
        let mut next = submission(&principal, &stream, sm, &format!("missing-{index}"));
        if let IngressStreamIdentity::Resumable {
            reserved_wire_position,
            checkpoint_h,
            ..
        } = &mut next.identity
        {
            *reserved_wire_position = WireHandledCount::from_storage(2);
            *checkpoint_h = WireHandledCount::from_storage(2);
        }
        let error = commit_hooks::SEARCH_PATH
            .scope(path.clone(), commit_submission(&uow, &next, 1))
            .await
            .expect_err("missing target relation");
        assert_eq!(
            error.class(),
            IngressDecisionClass::Storage,
            "missing {table}"
        );
        execute(
            &fixture.admin,
            &format!("ALTER TABLE {target}.hidden_relation RENAME TO {table}"),
        )
        .await;
        assert_eq!(
            count(&fixture.admin, target, "ingress_messages").await,
            3,
            "canonical rollback for {table}"
        );
        assert_eq!(
            count(&fixture.admin, target, "ingress_origin_aliases").await,
            3,
            "alias rollback for {table}"
        );
        assert_eq!(
            count(&fixture.admin, target, "ingress_effect_intents").await,
            3,
            "intent rollback for {table}"
        );
        assert_eq!(
            count(&fixture.admin, target, "ingress_effect_receipts").await,
            3,
            "receipt rollback for {table}"
        );
        assert_eq!(
            count(&fixture.admin, target, "ingress_sm_refs").await,
            1,
            "SM rollback for {table}"
        );
        let handled: String = sqlx::query_scalar(&format!(
            "SELECT handled_ordinal::text FROM {target}.ingress_sm_streams WHERE stream_id = $1"
        ))
        .bind(stream.as_str())
        .fetch_one(&fixture.admin)
        .await
        .expect("SM frontier");
        assert_eq!(handled, "1", "SM frontier rollback for {table}");
    }
    drop(uow);
    drop(pool);
}
