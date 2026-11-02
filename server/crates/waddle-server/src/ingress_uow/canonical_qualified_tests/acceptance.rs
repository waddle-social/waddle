//! Real direct-message Phase B under an attested transaction and hostile schema.

use super::{with_database, with_schema, Fixture};
use crate::config::LineageConfig;
use crate::db::physical_attestation::attested_pool;
use crate::db::{Database, DatabaseConfig, DatabaseDriver, DatabasePool};
use crate::ingress::commit::{commit_hooks, commit_submission};
use crate::ingress::effects::delivery::{ExternalDeliveryEffect, PeerDeliveryKind};
use crate::ingress::effects::direct::DurableDirectEffect;
use crate::ingress::effects::{Effect, IngressPlan, RoomExecutionPath};
use crate::ingress::ExternalEffect;
use crate::ingress::{
    DurableEffect, IngressPrincipal, IngressStreamIdentity, IngressSubmission, PlannedEffect,
};
use crate::ingress_uow::IngressUnitOfWork;
use futures::FutureExt;
use jid::BareJid;
use std::time::Duration;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::ingress::{
    ConnectionGeneration, DigestContext, DigestInput, EffectMessageIdentity,
    InboxProjectionMutation, IngressEffectIntent, NormalizedTarget, SmIngressId,
    TransportGeneration, WireHandledCount,
};
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp_core::xep0359::StanzaId;

#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimStore, EntityType, NodeIdentity, SharedNodeIdentity,
};

fn submission(
    principal: &AuthenticatedPrincipalRef,
    stream: &SmSessionId,
    sm: SmIngressId,
    origin: &str,
    id: &str,
) -> IngressSubmission {
    let recipient: BareJid = "bob@example.test".parse().expect("recipient");
    let mut message = xmpp_parsers::message::Message::new(Some(recipient.clone().into()));
    message.from = Some("alice@example.test/phone".parse().expect("sender"));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Default::default(), "ordinary message".to_owned());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, origin);
    let target = NormalizedTarget::Bare(recipient.clone());
    let digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: target.clone(),
            server_authorities: vec![principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let stamp = StanzaId::new(id, recipient.clone().into());
    let mut archived = ArchivedMessage::for_test(
        message.from.clone().expect("from"),
        message.to.clone().expect("to"),
    );
    archived.id = id.to_owned();
    archived.body = Some("ordinary message".to_owned());
    archived.message_type = xmpp_parsers::message::MessageType::Chat;
    archived.stanza_id = Some(stamp.clone());
    archived.origin_id = digest_input.origin().cloned();
    let sender_stamp = StanzaId::new(format!("sender-{id}"), principal.bare_jid().clone().into());
    let mut sender_archive = archived.clone();
    sender_archive.id = sender_stamp.id.clone();
    sender_archive.stanza_id = Some(sender_stamp.clone());
    let entry = InboxEntry::new(
        principal.bare_jid().clone(),
        ConversationKind::Direct,
        id,
        archived.timestamp.timestamp(),
    );
    let intents = vec![
        IngressEffectIntent::ArchiveAuthoritative {
            archive: principal.bare_jid().clone(),
            stanza_id: sender_stamp,
            by: principal.bare_jid().clone(),
            archived_at: sender_archive.timestamp,
            ordinal: None,
        },
        IngressEffectIntent::ArchiveAuthoritative {
            archive: recipient.clone(),
            stanza_id: stamp.clone(),
            by: recipient.clone(),
            archived_at: archived.timestamp,
            ordinal: None,
        },
        IngressEffectIntent::InboxProject {
            owner: recipient.clone(),
            mutation: InboxProjectionMutation::Direct {
                entry: entry.clone(),
                increment_unread: true,
            },
        },
        IngressEffectIntent::RouteDirect {
            recipient: recipient.clone(),
            fanout: vec!["bob@example.test/laptop".parse().expect("resource")],
            route_identity: EffectMessageIdentity::StanzaId(stamp.clone()),
            prepared: None,
        },
    ];
    let plan = vec![
        PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ArchiveDirect {
                archive: principal.bare_jid().clone(),
                message: Box::new(sender_archive),
                archive_expectation: ArchiveExpectation::Fresh,
            },
        ))),
        PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ArchiveDirect {
                archive: recipient.clone(),
                message: Box::new(archived),
                archive_expectation: ArchiveExpectation::Fresh,
            },
        ))),
        PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ProjectInbox {
                owner: recipient,
                entry: Box::new(entry),
                increment_unread: true,
            },
        ))),
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(EffectMessageIdentity::StanzaId(stamp)),
                jid: "bob@example.test/laptop".parse().expect("resource"),
                stanza: Box::new(waddle_xmpp::Stanza::Message(message.clone())),
                kind: PeerDeliveryKind::PeerStanza,
                call_setup: None,
            },
        ))),
    ];
    IngressSubmission {
        identity: IngressStreamIdentity::Resumable {
            stream_id: stream.clone(),
            sm_ingress_id: sm,
            #[cfg(feature = "clustering")]
            owner: NodeIdentity::new("accept-owner", "accept-incarnation"),
            #[cfg(feature = "clustering")]
            claim_epoch: ClaimEpoch(7),
            reserved_wire_position: WireHandledCount::from_storage(1),
            checkpoint_h: WireHandledCount::from_storage(1),
        },
        principal: IngressPrincipal::Authenticated(principal.clone()),
        sender: "alice@example.test/phone".parse().expect("sender"),
        target,
        plan: IngressPlan {
            failure: None,
            plan,
            intents,
            room_canonical_message: None,
            sanitized_message: message,
            error_reply: None,
            rejection: None,
            room_execution: RoomExecutionPath::None,
        },
        digest_input,
        connection_generation: TransportGeneration::Connection(ConnectionGeneration::INITIAL),
    }
}

#[tokio::test]
async fn attested_direct_acceptance_targets_one_schema_and_rolls_back() {
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
    let boundary = with_schema(&with_database(url, &fixture.database_name), &fixture.target);
    let owner = Database::from_config(
        "acceptance-owner",
        &DatabaseConfig::new(DatabaseDriver::Postgres, &boundary),
    )
    .await
    .expect("owner database");
    let mam = waddle_xmpp::mam::SqlxMamStorage::open(&boundary)
        .await
        .expect("MAM owner schema");
    crate::inbox::bootstrap_inbox_owner_schema(&owner)
        .await
        .expect("inbox owner schema");
    #[cfg(feature = "clustering")]
    crate::clustering::claims::PostgresClaimStore::new(owner.clone())
        .ensure_schema()
        .await
        .expect("claim owner schema");
    let principal = AuthenticatedPrincipalRef::new(
        "alice@example.test".parse().expect("alice"),
        AuthContextId::new(uuid::Uuid::new_v4()),
        AuthContextVersion::new(1),
        PrincipalAuthEpoch::new(1),
    );
    let stream = SmSessionId::new("accept-stream");
    let sm = SmIngressId::new();
    sqlx::query(&format!("INSERT INTO {}.users (jid, username, xmpp_localpart, created_at, updated_at) VALUES ($1, 'alice', 'alice', $2, $2)", fixture.target)).bind(principal.bare_jid().to_string()).bind("2020-01-01T00:00:00Z").execute(&fixture.admin).await.expect("user");
    sqlx::query(&format!("INSERT INTO {}.sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES ('s', $1, 'token', $2, 1, 1, $3, $3)", fixture.target)).bind(principal.bare_jid().to_string()).bind(principal.auth_context_id().as_uuid().to_string()).bind("2020-01-01T00:00:00Z").execute(&fixture.admin).await.expect("session");
    #[cfg(feature = "clustering")]
    sqlx::query(&format!("INSERT INTO {}.clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES ($1, $2, 'accept-owner', 'accept-incarnation', 7)", fixture.target)).bind(format!("{}:{}", EntityType::SmSession.as_db_str(), stream.as_str())).bind(EntityType::SmSession.as_db_str()).execute(&fixture.admin).await.expect("claim");
    let attacker = format!("accept_shadow_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {attacker}"))
        .execute(&fixture.admin)
        .await
        .expect("attacker schema");
    let tables = [
        "sessions",
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_sm_streams",
        "ingress_sm_refs",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_deliveries",
        "ingress_delivery_receipts",
        "ingress_archive_dispatch",
        "mam_messages",
        "mam_archive_sequences",
        "inbox_entries",
    ];
    for table in &tables {
        sqlx::query(&format!(
            "CREATE TABLE {attacker}.{table} (LIKE {}.{table} INCLUDING ALL)",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("shadow relation");
    }
    #[cfg(feature = "clustering")]
    sqlx::query(&format!(
        "CREATE TABLE {attacker}.clustering_claims (LIKE {}.clustering_claims INCLUDING ALL)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("shadow claims relation");
    // An unqualified principal or claim read must produce the opposite result.
    sqlx::query(&format!(
        "INSERT INTO {attacker}.sessions SELECT * FROM {}.sessions",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("shadow session");
    sqlx::query(&format!(
        "UPDATE {attacker}.sessions SET expires_at = '2000-01-01T00:00:00Z'"
    ))
    .execute(&fixture.admin)
    .await
    .expect("expired shadow session");
    #[cfg(feature = "clustering")]
    {
        sqlx::query(&format!(
            "INSERT INTO {attacker}.clustering_claims SELECT * FROM {}.clustering_claims",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("shadow claim");
        sqlx::query(&format!(
            "UPDATE {attacker}.clustering_claims SET claim_epoch = 8"
        ))
        .execute(&fixture.admin)
        .await
        .expect("stale shadow claim");
    }
    for sql in [
        format!("GRANT USAGE ON SCHEMA {attacker} TO {}", fixture.runtime),
        format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {attacker} TO {}",
            fixture.runtime
        ),
        format!(
            "REVOKE INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {} FROM {}",
            fixture.target, fixture.runtime
        ),
    ] {
        sqlx::query(&sql)
            .execute(&fixture.admin)
            .await
            .expect("writer grants");
    }
    for table in &tables {
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("data-plane reader grant");
        if matches!(*table, "sessions" | "ingress_delivery_receipts") {
            continue;
        }
        sqlx::query(&format!(
            "GRANT INSERT, UPDATE, DELETE ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("data-plane writer grant");
    }
    // PostgreSQL requires UPDATE on at least one column for row share locks.
    for (table, column) in [
        ("ingress_protocol_epoch", "id"),
        ("sessions", "last_used_at"),
        #[cfg(feature = "clustering")]
        ("clustering_claims", "entity_type"),
    ] {
        sqlx::query(&format!(
            "GRANT UPDATE ({column}) ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("row lock grant");
    }
    #[cfg(feature = "clustering")]
    sqlx::query(&format!(
        "GRANT SELECT ON {}.clustering_claims TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("claim reader grant");
    let (lineage_select, lineage_write): (bool, bool) = sqlx::query_as(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT'), \
                pg_catalog.has_any_column_privilege($1, $2, 'INSERT') OR \
                pg_catalog.has_any_column_privilege($1, $2, 'UPDATE') OR \
                pg_catalog.has_table_privilege($1, $2, 'DELETE')",
    )
    .bind(&fixture.runtime)
    .bind(format!("{}._lineage", fixture.target))
    .fetch_one(&fixture.admin)
    .await
    .expect("runtime lineage privileges");
    assert!(lineage_select);
    assert!(!lineage_write, "runtime cannot mutate physical lineage");
    drop(mam);
    drop(owner);
    let attested = attested_pool(
        fixture.runtime_options.clone(),
        fixture.config.clone(),
        2,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("acceptance", attested, 1, 1)
        .await
        .expect("pool");
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
    .expect("clustered UOW");
    #[cfg(not(feature = "clustering"))]
    let uow = IngressUnitOfWork::open(pool.ingress(), lineage).expect("UOW");
    let path = format!("{attacker}, {}", fixture.target);
    let first_submission = submission(&principal, &stream, sm, "origin-1", "archive-1");
    let first = commit_hooks::SEARCH_PATH
        .scope(path.clone(), commit_submission(&uow, &first_submission, 1))
        .await
        .expect("attested acceptance");
    assert!(first.class.advances());
    let forged_progress = sqlx::query(&format!(
        "INSERT INTO {attacker}.ingress_delivery_receipts (message_key, kind, semantic_identity_hash, resource) SELECT message_key, kind, semantic_identity_hash, resource FROM {}.ingress_archive_dispatch",
        fixture.target
    )).execute(&fixture.admin).await.expect("forged shadow progress");
    assert_eq!(forged_progress.rows_affected(), 1);
    let replay = commit_hooks::SEARCH_PATH
        .scope(path.clone(), commit_submission(&uow, &first_submission, 1))
        .await
        .expect("same wire replay");
    assert_eq!(first.message_key, replay.message_key);
    assert_eq!(replay.route_progress.len(), 1);
    assert!(
        replay.route_progress[0].completed.is_empty(),
        "shadow progress cannot suppress the live recipient"
    );
    for (table, expected) in [
        ("ingress_messages", 1_i64),
        ("ingress_origin_aliases", 1),
        ("ingress_sm_refs", 1),
        ("ingress_effect_intents", 4),
        ("ingress_effect_receipts", 3),
        ("ingress_deliveries", 1),
        ("ingress_archive_dispatch", 1),
        ("mam_messages", 2),
        ("mam_archive_sequences", 2),
        ("inbox_entries", 1),
    ] {
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {}.{table}", fixture.target))
                .fetch_one(&fixture.admin)
                .await
                .expect("target count");
        assert_eq!(count, expected, "target {table}");
        let shadow: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {attacker}.{table}"))
            .fetch_one(&fixture.admin)
            .await
            .expect("shadow count");
        assert_eq!(shadow, 0, "shadow {table} must stay untouched");
    }
    let unread: i64 = sqlx::query_scalar(&format!(
        "SELECT unread::bigint FROM {}.inbox_entries",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("unread");
    assert_eq!(unread, 1, "replay must not increment unread");
    let (handled, checkpoint): (String, i64) = sqlx::query_as(&format!(
        "SELECT handled_ordinal::text, checkpoint_h FROM {}.ingress_sm_streams WHERE stream_id = $1", fixture.target
    )).bind(stream.as_str()).fetch_one(&fixture.admin).await.expect("SM checkpoint");
    assert_eq!((handled.as_str(), checkpoint), ("1", 1));
    let archive_and_dispatch_match: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.mam_messages m JOIN {}.ingress_archive_dispatch d ON d.archive_jid = m.room_jid AND d.archive_seq = m.archive_seq WHERE m.id = 'archive-1'", fixture.target, fixture.target
    )).fetch_one(&fixture.admin).await.expect("archive dispatch link");
    assert_eq!(archive_and_dispatch_match, 1);
    #[cfg(feature = "clustering")]
    {
        sqlx::query(&format!(
            "UPDATE {}.clustering_claims SET claim_epoch = 8",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("retire trusted claim");
        sqlx::query(&format!(
            "UPDATE {attacker}.clustering_claims SET claim_epoch = 7"
        ))
        .execute(&fixture.admin)
        .await
        .expect("matching shadow claim");
        let mut offered = submission(&principal, &stream, sm, "stale-owner", "stale-archive");
        if let IngressStreamIdentity::Resumable {
            reserved_wire_position,
            checkpoint_h,
            ..
        } = &mut offered.identity
        {
            *reserved_wire_position = WireHandledCount::from_storage(2);
            *checkpoint_h = WireHandledCount::from_storage(2);
        }
        let error = commit_hooks::SEARCH_PATH
            .scope(path.clone(), commit_submission(&uow, &offered, 1))
            .await
            .expect_err("shadow claim cannot restore authority");
        assert_eq!(
            error.class(),
            crate::ingress::IngressDecisionClass::ClaimFenceMissing
        );
        sqlx::query(&format!(
            "UPDATE {}.clustering_claims SET claim_epoch = 7",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("restore trusted claim");
    }
    for (index, table) in [
        "ingress_effect_intents",
        "mam_archive_sequences",
        "mam_messages",
        "ingress_deliveries",
        "inbox_entries",
        "ingress_archive_dispatch",
        "ingress_delivery_receipts",
        "ingress_effect_receipts",
    ]
    .iter()
    .enumerate()
    {
        sqlx::query(&format!(
            "ALTER TABLE {}.{table} RENAME TO hidden_relation",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("hide trusted relation");
        let mut offered = submission(
            &principal,
            &stream,
            sm,
            &format!("failed-{index}"),
            &format!("failed-archive-{index}"),
        );
        if let IngressStreamIdentity::Resumable {
            reserved_wire_position,
            checkpoint_h,
            ..
        } = &mut offered.identity
        {
            *reserved_wire_position = WireHandledCount::from_storage(2);
            *checkpoint_h = WireHandledCount::from_storage(2);
        }
        let error = commit_hooks::SEARCH_PATH
            .scope(path.clone(), commit_submission(&uow, &offered, 1))
            .await
            .expect_err("missing trusted relation must fail closed");
        assert_eq!(
            error.class(),
            crate::ingress::IngressDecisionClass::Storage,
            "missing trusted {table}"
        );
        sqlx::query(&format!(
            "ALTER TABLE {}.hidden_relation RENAME TO {table}",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("restore trusted relation");
        let messages: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {}.ingress_messages",
            fixture.target
        ))
        .fetch_one(&fixture.admin)
        .await
        .expect("canonical count");
        assert_eq!(messages, 1, "missing {table} must roll back acceptance");
        for (protected, expected) in [
            ("ingress_origin_aliases", 1_i64),
            ("ingress_sm_refs", 1),
            ("ingress_effect_intents", 4),
            ("ingress_effect_receipts", 3),
            ("ingress_deliveries", 1),
            ("ingress_archive_dispatch", 1),
            ("mam_messages", 2),
            ("mam_archive_sequences", 2),
            ("inbox_entries", 1),
        ] {
            let count: i64 = sqlx::query_scalar(&format!(
                "SELECT count(*) FROM {}.{protected}",
                fixture.target
            ))
            .fetch_one(&fixture.admin)
            .await
            .expect("rollback count");
            assert_eq!(count, expected, "missing {table}: partial {protected}");
        }
    }
    drop(uow);
    drop(pool);
}
