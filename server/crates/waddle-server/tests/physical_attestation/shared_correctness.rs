//! The dark shared facade, exercised on the actual owner-created MAM schema.

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use sqlx::Connection;
use uuid::Uuid;
use waddle_server::config::{IngressConfig, LineageConfig};
use waddle_server::db::physical_attestation::{attested_pool, install};
use waddle_server::db::{
    bootstrap_dark_postgres_mam_owner, DarkMigrationOwnerConfig, Database, DatabaseConfig,
    DatabaseDriver, DatabasePool, MigrationRunner, PoolConfig,
};
use waddle_server::ingress::effects::delivery::{ExternalDeliveryEffect, PeerDeliveryKind};
use waddle_server::ingress::effects::direct::DurableDirectEffect;
use waddle_server::ingress::effects::Effect;
use waddle_server::ingress::{
    DurableEffect, ExternalEffect, IngressDecisionClass, IngressPlan, IngressPrincipal,
    IngressStreamIdentity, IngressSubmission, PlannedEffect, RoomExecutionPath,
};
use waddle_server::test_support::{SharedCorrectnessFixture, SharedCorrectnessSubmitError};
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ingress::{
    ConnectionGeneration, DigestContext, DigestInput, EffectMessageIdentity, IngressEffectIntent,
    NormalizedTarget, TransportGeneration,
};
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};
use waddle_xmpp_core::xep0359::{add_origin_id, StanzaId};

use super::{test_url, Fixture, CATALOG_GRANT_LOCK};

#[path = "pooled_archive_resolver.rs"]
mod pooled_archive_resolver;

fn facade(pool: &DatabasePool, lineage: LineageConfig) -> SharedCorrectnessFixture {
    SharedCorrectnessFixture::from_pool(
        pool,
        &IngressConfig {
            pool_size: 1,
            retry_attempts: 2,
        },
        lineage,
        #[cfg(feature = "clustering")]
        None,
    )
    .expect("closed facade")
}

fn principal() -> AuthenticatedPrincipalRef {
    AuthenticatedPrincipalRef::new(
        "alice@example.test".parse().expect("sender"),
        AuthContextId::new(Uuid::new_v4()),
        AuthContextVersion::new(1),
        PrincipalAuthEpoch::new(1),
    )
}

async fn seed_principal(db: &Database, principal: &AuthenticatedPrincipalRef) {
    let guard = db.guard().await.expect("owner guard");
    let now = "2020-01-01T00:00:00Z".to_owned();
    guard.execute(
        "INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        waddle_server::db_params![principal.bare_jid().to_string(), "alice".to_owned(), "alice".to_owned(), now.clone(), now.clone()],
    ).await.expect("seed account");
    guard.execute(
        "INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        waddle_server::db_params!["session".to_owned(), principal.bare_jid().to_string(), "token".to_owned(), principal.auth_context_id().as_uuid().to_string(), 1_i64, 1_i64, now.clone(), now],
    ).await.expect("seed session");
}

fn submission(
    principal: &AuthenticatedPrincipalRef,
    origin: &str,
    id: &str,
) -> (IngressSubmission, StanzaId) {
    let recipient: jid::BareJid = "bob@example.test".parse().expect("recipient");
    let mut message = xmpp_parsers::message::Message::new(Some(recipient.clone().into()));
    message.from = Some("alice@example.test/phone".parse().expect("sender"));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.bodies.insert(Default::default(), id.to_owned());
    add_origin_id(&mut message, origin);
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
    archived.body = Some(id.to_owned());
    archived.message_type = xmpp_parsers::message::MessageType::Chat;
    archived.stanza_id = Some(stamp.clone());
    archived.origin_id = digest_input.origin().cloned();
    let archive_intent = IngressEffectIntent::ArchiveAuthoritative {
        archive: recipient.clone(),
        stanza_id: stamp.clone(),
        by: recipient.clone(),
        archived_at: archived.timestamp,
        ordinal: None,
    };
    let effect = Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
        archive: recipient.clone(),
        message: Box::new(archived),
        archive_expectation: ArchiveExpectation::Fresh,
    }));
    let route_intent = IngressEffectIntent::RouteDirect {
        recipient,
        fanout: vec!["bob@example.test/laptop".parse().expect("resource")],
        route_identity: EffectMessageIdentity::StanzaId(stamp.clone()),
        prepared: None,
    };
    let route = Effect::External(ExternalEffect::Delivery(
        ExternalDeliveryEffect::RouteToPeer {
            route_identity: Some(EffectMessageIdentity::StanzaId(stamp.clone())),
            jid: "bob@example.test/laptop".parse().expect("resource"),
            stanza: Box::new(waddle_xmpp::Stanza::Message(message.clone())),
            kind: PeerDeliveryKind::PeerStanza,
            call_setup: None,
        },
    ));
    (
        IngressSubmission {
            identity: IngressStreamIdentity::Ephemeral {
                principal: principal.clone(),
            },
            principal: IngressPrincipal::Authenticated(principal.clone()),
            sender: "alice@example.test/phone".parse().expect("sender"),
            target,
            plan: IngressPlan {
                failure: None,
                plan: vec![PlannedEffect::new(effect), PlannedEffect::new(route)],
                intents: vec![archive_intent, route_intent],
                room_canonical_message: None,
                sanitized_message: message,
                error_reply: None,
                rejection: None,
                room_execution: RoomExecutionPath::None,
            },
            digest_input,
            connection_generation: TransportGeneration::Connection(ConnectionGeneration::INITIAL),
        },
        stamp,
    )
}

async fn count(db: &Database, relation: &str) -> i64 {
    let guard = db.guard().await.expect("count guard");
    let mut rows = guard
        .query(&format!("SELECT count(*) FROM {relation}"), ())
        .await
        .expect("count");
    rows.next()
        .await
        .expect("row")
        .expect("count row")
        .get(0)
        .expect("integer")
}

async fn pg_snapshot(fixture: &Fixture) -> (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64) {
    let schema = &fixture.target;
    sqlx::query_as(&format!(
        "SELECT (SELECT count(*) FROM {schema}.ingress_messages), \
         (SELECT count(*) FROM {schema}.ingress_origin_aliases), \
         (SELECT count(*) FROM {schema}.ingress_effect_intents), \
         (SELECT count(*) FROM {schema}.ingress_effect_receipts), \
         (SELECT count(*) FROM {schema}.mam_messages), \
         (SELECT count(*) FROM {schema}.mam_archive_sequences), \
         (SELECT coalesce(sum(next_seq), 0)::bigint FROM {schema}.mam_archive_sequences), \
         (SELECT count(*) FROM {schema}.ingress_archive_dispatch), \
         (SELECT count(*) FROM {schema}.ingress_deliveries), \
         (SELECT count(*) FROM {schema}.ingress_sm_refs), \
         (SELECT count(*) FROM {schema}.ingress_sm_streams)"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("complete transaction snapshot")
}

#[tokio::test]
async fn sqlite_shared_facade_uses_exact_single_connection_and_replays() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
    config.pool_size = 1;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("single SQLite pool");
    MigrationRunner::single()
        .run(pool.global())
        .await
        .expect("migrations");
    waddle_server::test_support::initialize_mam_on_sqlite_test_pool(&pool)
        .await
        .expect("MAM on same pool");
    let principal = principal();
    seed_principal(pool.global(), &principal).await;
    let facade = facade(&pool, LineageConfig::default());
    let (offered, stamp) = submission(&principal, "sqlite-origin", "sqlite-archive");
    let mut resumable = offered.clone();
    resumable.identity = IngressStreamIdentity::Resumable {
        stream_id: waddle_xmpp::pending_delivery::SmSessionId::new("unbound-stream"),
        sm_ingress_id: waddle_xmpp::ingress::SmIngressId::new(),
        reserved_wire_position: waddle_xmpp::ingress::WireHandledCount::from_storage(1),
        checkpoint_h: waddle_xmpp::ingress::WireHandledCount::from_storage(1),
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("fixture", "incarnation"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
    };
    assert!(matches!(
        facade.submit(&resumable).await,
        Err(SharedCorrectnessSubmitError::UnsupportedStreamIdentity)
    ));
    let mut no_origin = offered.clone();
    let mut no_origin_message = no_origin.plan.sanitized_message.clone();
    no_origin_message
        .payloads
        .retain(|element| !waddle_xmpp_core::xep0359::is_origin_id_element(element));
    no_origin.digest_input = DigestInput::from_parsed(
        &no_origin_message,
        &DigestContext {
            target: no_origin.target.clone(),
            server_authorities: vec![principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("originless digest");
    no_origin.plan.sanitized_message = no_origin_message;
    assert!(matches!(
        facade.submit(&no_origin).await,
        Err(SharedCorrectnessSubmitError::MissingOriginId)
    ));
    assert_eq!(count(pool.global(), "ingress_messages").await, 0);
    let first = facade
        .submit(&offered)
        .await
        .expect("accept SQLite message");
    assert!(first.class.advances());
    let found = facade
        .lookup_archive_or_stanza_id(&stamp)
        .await
        .expect("lookup")
        .expect("archive");
    assert_eq!(found.id, "sqlite-archive");
    let replay = facade.submit(&offered).await.expect("immutable replay");
    assert_eq!(first.message_key, replay.message_key);
    assert_eq!(count(pool.global(), "mam_messages").await, 1);
    assert_eq!(count(pool.global(), "ingress_effect_receipts").await, 1);

    let guard = pool.global().guard().await.expect("same connection");
    guard
        .execute("CREATE TEMP TABLE mam_messages (id TEXT)", ())
        .await
        .expect("create SQLite shadow");
    drop(guard);
    assert_eq!(
        facade
            .lookup_archive_or_stanza_id(&stamp)
            .await
            .expect("main-qualified lookup")
            .expect("durable row")
            .id,
        "sqlite-archive"
    );
    let guard = pool.global().guard().await.expect("same connection");
    guard
        .execute("DROP TABLE temp.mam_messages", ())
        .await
        .expect("drop SQLite shadow");
    guard
        .execute("ALTER TABLE mam_messages RENAME TO hidden_mam_messages", ())
        .await
        .expect("hide MAM relation");
    drop(guard);
    let (failed, _) = submission(&principal, "sqlite-failed", "sqlite-failed-archive");
    assert!(facade.submit(&failed).await.is_err());
    let guard = pool.global().guard().await.expect("same connection");
    guard
        .execute("ALTER TABLE hidden_mam_messages RENAME TO mam_messages", ())
        .await
        .expect("restore MAM relation");
    drop(guard);
    assert_eq!(count(pool.global(), "mam_messages").await, 1);
    assert_eq!(count(pool.global(), "ingress_effect_receipts").await, 1);
    assert_eq!(count(pool.global(), "ingress_messages").await, 1);
    let (second, _) = submission(&principal, "sqlite-second-origin", "sqlite-second");
    facade.submit(&second).await.expect("second archive");
    pooled_archive_resolver::exercise_sqlite(&pool, &stamp).await;
}

#[tokio::test]
async fn sqlite_memory_pool_with_two_checkouts_is_refused() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
    config.pool_size = 2;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("two-connection SQLite pool");
    let first = pool.global().begin().await.expect("first checkout");
    let second = pool.global().begin().await.expect("second checkout");
    assert!(
        SharedCorrectnessFixture::from_pool(
            &pool,
            &IngressConfig {
                pool_size: 1,
                retry_attempts: 1
            },
            LineageConfig::default(),
            #[cfg(feature = "clustering")]
            None,
        )
        .is_err(),
        "multi-connection memory cannot establish one backend"
    );
    first.rollback().await.expect("release first checkout");
    second.rollback().await.expect("release second checkout");
}

#[tokio::test]
async fn restricted_v1009_facade_preserves_atomicity_and_reserved_capacity() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url.clone()).await;
    let suffix = Uuid::new_v4().simple().to_string();
    let owner = format!("shared_owner_{suffix}");
    let migrator = format!("shared_migrator_{suffix}");
    let mut created_roles = Vec::new();
    let result = std::panic::AssertUnwindSafe(exercise_postgres(
        &fixture,
        &owner,
        &migrator,
        &suffix,
        &mut created_roles,
    ))
    .catch_unwind()
    .await;
    fixture.cleanup().await;
    let cluster = sqlx::PgPool::connect(&url).await.expect("cleanup cluster");
    for role in created_roles.iter().rev() {
        sqlx::query(&format!("DROP ROLE {role}"))
            .execute(&cluster)
            .await
            .expect("drop test-owned role");
    }
    cluster.close().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn exercise_postgres(
    fixture: &Fixture,
    owner: &str,
    migrator: &str,
    suffix: &str,
    created_roles: &mut Vec<String>,
) {
    sqlx::query(&format!(
        "CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"
    ))
    .execute(&fixture.admin)
    .await
    .expect("create owner");
    created_roles.push(owner.to_owned());
    sqlx::query(&format!("CREATE ROLE {migrator} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"))
        .execute(&fixture.admin).await.expect("create migrator");
    created_roles.push(migrator.to_owned());
    for statement in [
        format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
        format!("ALTER SCHEMA {} OWNER TO {owner}", fixture.target),
        format!("GRANT SELECT ON {}._lineage TO {owner}", fixture.target),
        format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("provision owner handoff");
    }
    let mut owner_url = url::Url::parse(&fixture.base_url).expect("fixture URL");
    let retained: Vec<_> = owner_url
        .query_pairs()
        .filter(|(key, _)| key != "user" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    owner_url
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(k, v)| (k, v)))
        .append_pair("user", migrator)
        .append_pair(
            "options",
            &format!("-c role={owner} -c search_path={}", fixture.target),
        );
    let owner_db = Database::from_config(
        "shared owner",
        &DatabaseConfig::new(DatabaseDriver::Postgres, owner_url.to_string()),
    )
    .await
    .expect("migrator assumes owner");
    let owner_guard = owner_db.guard().await.expect("migrator session");
    let mut roles = owner_guard
        .query("SELECT session_user::text, current_user::text", ())
        .await
        .expect("authenticated and effective roles");
    let roles = roles.next().await.expect("roles").expect("role row");
    assert_eq!(roles.get::<String>(0).expect("session role"), migrator);
    assert_eq!(roles.get::<String>(1).expect("effective role"), owner);
    drop(owner_guard);
    MigrationRunner::single()
        .run(&owner_db)
        .await
        .expect("owner migrations");
    let owner_config = DarkMigrationOwnerConfig::new(
        fixture.lineage.clone(),
        waddle_server::db::physical_attestation::PgRoleName::new(owner).expect("owner role"),
        waddle_server::db::physical_attestation::PgRoleName::new(&fixture.runtime)
            .expect("runtime role"),
    )
    .expect("owner config");
    let migrator_role =
        waddle_server::db::physical_attestation::PgRoleName::new(migrator).expect("migrator role");
    let migrator_options = fixture.admin_options.clone().username(migrator);
    bootstrap_dark_postgres_mam_owner(&migrator_options, &owner_config, &migrator_role)
        .await
        .expect("separate owner MAM bootstrap");
    waddle_server::pending_delivery::bootstrap_pending_delivery_owner_schema(&owner_db)
        .await
        .expect("separate pending owner bootstrap");
    let principal = principal();
    seed_principal(&owner_db, &principal).await;
    drop(owner_db);
    let (relation_owner, owner_login): (String, bool) = sqlx::query_as(
        "SELECT r.rolname::text, r.rolcanlogin FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace JOIN pg_catalog.pg_roles r ON r.oid=c.relowner WHERE n.nspname=$1 AND c.relname='mam_messages'"
    ).bind(&fixture.target).fetch_one(&fixture.admin).await.expect("observe MAM owner");
    assert_eq!(relation_owner, owner);
    assert!(!owner_login);
    let graph_ok: bool = sqlx::query_scalar(
        "SELECT m.rolcanlogin AND NOT o.rolcanlogin AND r.rolcanlogin \
         AND x.set_option AND NOT x.inherit_option AND NOT x.admin_option \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members y WHERE y.member=r.oid) \
         AND NOT pg_catalog.pg_has_role(r.oid, o.oid, 'SET') \
         FROM pg_catalog.pg_roles m JOIN pg_catalog.pg_roles o ON o.rolname=$2 \
         JOIN pg_catalog.pg_roles r ON r.rolname=$3 \
         JOIN pg_catalog.pg_auth_members x ON x.member=m.oid AND x.roleid=o.oid \
         WHERE m.rolname=$1",
    )
    .bind(migrator)
    .bind(owner)
    .bind(&fixture.runtime)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect role graph");
    assert!(graph_ok);
    for statement in [
        format!(
            "GRANT SELECT ON {}.ingress_protocol_epoch TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT UPDATE (id) ON {}.ingress_protocol_epoch TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT SELECT ON {}.sessions TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT UPDATE (last_used_at) ON {}.sessions TO {}",
            fixture.target, fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("restricted runtime grant");
    }
    for (table, privileges) in [
        ("ingress_messages", "SELECT, INSERT"),
        ("ingress_origin_aliases", "SELECT, INSERT"),
        ("ingress_effect_intents", "SELECT, INSERT"),
        ("ingress_effect_receipts", "SELECT, INSERT"),
        ("ingress_archive_dispatch", "SELECT, INSERT"),
        ("ingress_deliveries", "SELECT"),
        ("ingress_delivery_receipts", "SELECT"),
        ("mam_archive_sequences", "SELECT, INSERT"),
        ("mam_messages", "SELECT, INSERT"),
        ("pending_delivery", "SELECT, INSERT, UPDATE, DELETE"),
        ("_lineage", "SELECT"),
        ("_migrations", "SELECT"),
    ] {
        sqlx::query(&format!(
            "GRANT {privileges} ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("scoped application grant");
    }
    for (table, columns) in [
        (
            "ingress_messages",
            "envelope_version, envelope, terminal_at, retention_eligible_at",
        ),
        ("ingress_effect_intents", "payload"),
        ("mam_archive_sequences", "next_seq"),
    ] {
        sqlx::query(&format!(
            "GRANT UPDATE ({columns}) ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("scoped application column grant");
    }
    let shadow = format!("shared_shadow_{suffix}");
    for statement in [
        format!("CREATE SCHEMA {shadow}"),
        format!(
            "CREATE TABLE {shadow}.mam_messages (LIKE {}.mam_messages INCLUDING ALL)",
            fixture.target
        ),
        format!("GRANT USAGE ON SCHEMA {shadow} TO {}", fixture.runtime),
        format!(
            "GRANT SELECT ON {shadow}.mam_messages TO {}",
            fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("provision hostile shadow");
    }
    let attestation = install(&fixture.installer_options, fixture.config(), Uuid::new_v4())
        .await
        .expect("install attestor");
    // The checkout attestor requires the target to be current before it
    // normalizes the path. The helper's separate shadow test establishes
    // qualification independently of this target-first compatibility case.
    let hostile_path = format!("{},{shadow},pg_temp", fixture.target);
    let attested = attested_pool(
        fixture
            .runtime_options
            .clone()
            .options([("search_path", hostile_path.as_str())]),
        attestation,
        2,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("shared facade", attested, 1, 1)
        .await
        .expect("two admitted lanes");
    assert!(
        SharedCorrectnessFixture::from_pool(
            &pool,
            &IngressConfig {
                pool_size: 2,
                retry_attempts: 1,
            },
            LineageConfig {
                deployment_uuid: Some(fixture.lineage.deployment_uuid),
                action: None,
            },
            #[cfg(feature = "clustering")]
            None,
        )
        .is_err(),
        "incorrect reserved-lane budget must be refused"
    );
    let ingress_config = IngressConfig {
        pool_size: 1,
        retry_attempts: 2,
    };
    let lineage_config = LineageConfig {
        deployment_uuid: Some(fixture.lineage.deployment_uuid),
        action: None,
    };
    let mut wrong_lineage = fixture.lineage.clone();
    wrong_lineage.deployment_uuid.0 = Uuid::new_v4();
    let wrong_config = DarkMigrationOwnerConfig::new(
        wrong_lineage,
        waddle_server::db::physical_attestation::PgRoleName::new(owner).expect("owner role"),
        waddle_server::db::physical_attestation::PgRoleName::new(&fixture.runtime)
            .expect("runtime role"),
    )
    .expect("wrong config");
    assert!(
        SharedCorrectnessFixture::attach_postgres_mam_from_pool(
            &pool,
            &ingress_config,
            lineage_config.clone(),
            &wrong_config,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .is_err(),
        "mismatched lineage must be refused"
    );
    let mut malformed_lineage = fixture.lineage.clone();
    malformed_lineage
        .postgres_identity
        .as_mut()
        .expect("PostgreSQL identity")
        .schema
        .name
        .0 = "invalid\"schema".to_owned();
    let malformed_config = DarkMigrationOwnerConfig::new(
        malformed_lineage,
        waddle_server::db::physical_attestation::PgRoleName::new(owner).expect("owner role"),
        waddle_server::db::physical_attestation::PgRoleName::new(&fixture.runtime)
            .expect("runtime role"),
    )
    .expect("malformed config");
    assert!(
        SharedCorrectnessFixture::attach_postgres_mam_from_pool(
            &pool,
            &ingress_config,
            lineage_config.clone(),
            &malformed_config,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .is_err(),
        "malformed schema must be refused before SQL"
    );
    sqlx::query(&format!("ALTER ROLE {owner} LOGIN"))
        .execute(&fixture.admin)
        .await
        .expect("temporarily unsafe owner");
    assert!(
        SharedCorrectnessFixture::attach_postgres_mam_from_pool(
            &pool,
            &ingress_config,
            lineage_config.clone(),
            &owner_config,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .is_err(),
        "owner LOGIN must be refused"
    );
    sqlx::query(&format!("ALTER ROLE {owner} NOLOGIN"))
        .execute(&fixture.admin)
        .await
        .expect("restore NOLOGIN owner");
    sqlx::query(&format!(
        "DROP INDEX {}.idx_mam_room_archive_seq",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove MAM ordinal index");
    assert!(
        SharedCorrectnessFixture::attach_postgres_mam_from_pool(
            &pool,
            &ingress_config,
            lineage_config.clone(),
            &owner_config,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .is_err(),
        "missing MAM ordinal index must be refused"
    );
    sqlx::query(&format!(
        "CREATE UNIQUE INDEX idx_mam_room_archive_seq ON {}.mam_messages(room_jid,archive_seq)",
        fixture.target,
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore MAM ordinal index");
    let facade = Arc::new(
        SharedCorrectnessFixture::attach_postgres_mam_from_pool(
            &pool,
            &ingress_config,
            lineage_config.clone(),
            &owner_config,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .expect("read-only MAM attachment"),
    );

    // Role-specific grants affect only this disposable database.
    for (table, privilege) in [
        ("mam_messages", "UPDATE"),
        ("mam_messages", "DELETE"),
        ("mam_messages", "TRUNCATE"),
        ("mam_messages", "TRIGGER"),
        ("mam_messages", "REFERENCES"),
        ("_lineage", "UPDATE"),
        ("_migrations", "INSERT"),
    ] {
        sqlx::query(&format!(
            "GRANT {privilege} ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant negative privilege");
        assert!(
            SharedCorrectnessFixture::attach_postgres_mam_from_pool(
                &pool,
                &ingress_config,
                lineage_config.clone(),
                &owner_config,
                #[cfg(feature = "clustering")]
                None,
            )
            .await
            .is_err(),
            "{privilege} on {table} must be refused"
        );
        sqlx::query(&format!(
            "REVOKE {privilege} ON {}.{table} FROM {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("revoke negative privilege");
    }
    for (table, column) in [
        ("mam_archive_sequences", "archive_jid"),
        ("_lineage", "deployment_uuid"),
    ] {
        sqlx::query(&format!(
            "GRANT UPDATE ({column}) ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant negative column privilege");
        assert!(
            SharedCorrectnessFixture::attach_postgres_mam_from_pool(
                &pool,
                &ingress_config,
                lineage_config.clone(),
                &owner_config,
                #[cfg(feature = "clustering")]
                None,
            )
            .await
            .is_err(),
            "UPDATE({column}) on {table} must be refused"
        );
        sqlx::query(&format!(
            "REVOKE UPDATE ({column}) ON {}.{table} FROM {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("revoke negative column privilege");
    }
    sqlx::query(&format!(
        "GRANT TRUNCATE ON {}.mam_messages TO {}",
        fixture.target, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant inherited privilege");
    sqlx::query(&format!(
        "GRANT {} TO {} WITH INHERIT TRUE, SET TRUE, ADMIN FALSE",
        fixture.outsider, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant runtime membership");
    assert!(
        SharedCorrectnessFixture::attach_postgres_mam_from_pool(
            &pool,
            &ingress_config,
            lineage_config.clone(),
            &owner_config,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .is_err(),
        "inherited privileges must be refused"
    );
    sqlx::query(&format!(
        "REVOKE {} FROM {}",
        fixture.outsider, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke runtime membership");
    sqlx::query(&format!(
        "REVOKE TRUNCATE ON {}.mam_messages FROM {}",
        fixture.target, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke inherited privilege");
    sqlx::query(&format!(
        "GRANT SELECT ON {}.mam_messages TO {} WITH GRANT OPTION",
        fixture.target, fixture.runtime,
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant delegation option");
    assert!(
        SharedCorrectnessFixture::attach_postgres_mam_from_pool(
            &pool,
            &ingress_config,
            lineage_config.clone(),
            &owner_config,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .is_err(),
        "grant options must be refused"
    );
    sqlx::query(&format!(
        "REVOKE GRANT OPTION FOR SELECT ON {}.mam_messages FROM {}",
        fixture.target, fixture.runtime,
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke delegation option");
    let version: i32 =
        sqlx::query_scalar("SELECT pg_catalog.current_setting('server_version_num')::integer")
            .fetch_one(&fixture.admin)
            .await
            .expect("PostgreSQL version");
    if version >= 170000 {
        sqlx::query(&format!(
            "GRANT MAINTAIN ON {}.mam_messages TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant MAINTAIN");
        assert!(
            SharedCorrectnessFixture::attach_postgres_mam_from_pool(
                &pool,
                &ingress_config,
                lineage_config.clone(),
                &owner_config,
                #[cfg(feature = "clustering")]
                None,
            )
            .await
            .is_err(),
            "MAINTAIN must be refused"
        );
        sqlx::query(&format!(
            "REVOKE MAINTAIN ON {}.mam_messages FROM {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("revoke MAINTAIN");
    }
    let admin_db = Database::from_config(
        "rollback-only PUBLIC ACL probe",
        &DatabaseConfig::new(DatabaseDriver::Postgres, &fixture.base_url),
    )
    .await
    .expect("fixture admin database");
    assert!(
        waddle_server::test_support::probe_public_mam_acl_rollback(&admin_db, &owner_config)
            .await
            .expect("PUBLIC ACL probe")
    );
    assert!(
        waddle_server::test_support::probe_public_mam_acl_rollback(&admin_db, &malformed_config)
            .await
            .is_err(),
        "PUBLIC probe must reject malformed schema before SQL"
    );
    drop(admin_db);
    let public_acl: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
         CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) a \
         WHERE n.nspname=$1 AND c.relname='mam_messages' AND a.grantee=0)",
    ).bind(&fixture.target).fetch_one(&fixture.admin).await.expect("PUBLIC ACL after rollback");
    assert!(!public_acl, "PUBLIC probe left no committed grant");

    for role in [fixture.runtime.as_str(), owner] {
        sqlx::query(&format!(
            "REVOKE ALL PRIVILEGES ON {}.mam_messages FROM {role}",
            fixture.target,
        ))
        .execute(&fixture.admin)
        .await
        .expect("remove relation ACL entry");
    }
    let empty_acl: bool = sqlx::query_scalar(
        "SELECT pg_catalog.cardinality(c.relacl) = 0 FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = 'mam_messages'",
    )
    .bind(&fixture.target)
    .fetch_one(&fixture.admin)
    .await
    .expect("observe explicit empty ACL");
    assert!(empty_acl);
    let error = SharedCorrectnessFixture::attach_postgres_mam_from_pool(
        &pool,
        &ingress_config,
        lineage_config.clone(),
        &owner_config,
        #[cfg(feature = "clustering")]
        None,
    )
    .await
    .err()
    .expect("empty ACL must be refused");
    assert!(matches!(
        error.downcast_ref::<waddle_server::db::bootstrap::RuntimeAttachmentError>(),
        Some(waddle_server::db::bootstrap::RuntimeAttachmentError::UnsafeMamRuntime)
    ));
    sqlx::query(&format!(
        "GRANT ALL PRIVILEGES ON {}.mam_messages TO {owner}",
        fixture.target,
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore owner ACL");
    sqlx::query(&format!(
        "GRANT SELECT, INSERT ON {}.mam_messages TO {}",
        fixture.target, fixture.runtime,
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore runtime ACL");

    let mut runtime = sqlx::PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("restricted login");
    assert!(sqlx::query(&format!("SET ROLE {owner}"))
        .execute(&mut runtime)
        .await
        .is_err());
    assert!(sqlx::query(&format!(
        "CREATE TABLE {}.forbidden (id integer)",
        fixture.target
    ))
    .execute(&mut runtime)
    .await
    .is_err());
    assert!(sqlx::query("CREATE TEMP TABLE forbidden (id integer)")
        .execute(&mut runtime)
        .await
        .is_err());
    assert!(sqlx::query(&format!(
        "ALTER TABLE {}.mam_messages ADD COLUMN forbidden integer",
        fixture.target,
    ))
    .execute(&mut runtime)
    .await
    .is_err());
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_archive_dispatch",
        "ingress_deliveries",
        "mam_archive_sequences",
        "mam_messages",
    ] {
        assert!(
            sqlx::query(&format!(
                "DELETE FROM {}.{table} WHERE false",
                fixture.target
            ))
            .execute(&mut runtime)
            .await
            .is_err(),
            "runtime DELETE unexpectedly allowed on {table}"
        );
    }
    for (table, column) in [
        ("ingress_origin_aliases", "sender_bare_jid"),
        ("ingress_effect_receipts", "kind"),
        ("ingress_archive_dispatch", "resource"),
        ("mam_messages", "id"),
    ] {
        assert!(
            sqlx::query(&format!(
                "UPDATE {}.{table} SET {column} = {column} WHERE false",
                fixture.target
            ))
            .execute(&mut runtime)
            .await
            .is_err(),
            "runtime UPDATE unexpectedly allowed on {table}"
        );
    }
    let mut tx = runtime.begin().await.expect("runtime transaction");
    sqlx::query(&format!(
        "SELECT id FROM {}.ingress_protocol_epoch FOR SHARE",
        fixture.target
    ))
    .execute(&mut *tx)
    .await
    .expect("column grant permits V1009 row lock");
    tx.rollback().await.expect("release row lock");
    let mut tx = runtime.begin().await.expect("lock test");
    assert!(sqlx::query(&format!(
        "LOCK TABLE {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *tx)
    .await
    .is_err());
    tx.rollback().await.expect("release failed lock");
    assert!(sqlx::query(&format!(
        "UPDATE {}.ingress_protocol_epoch SET epoch=1",
        fixture.target
    ))
    .execute(&mut runtime)
    .await
    .is_err());
    assert!(sqlx::query(&format!(
        "UPDATE {}.ingress_protocol_epoch SET id=id",
        fixture.target
    ))
    .execute(&mut runtime)
    .await
    .is_err());
    drop(runtime);

    let (first, stamp) = submission(&principal, "pg-origin", "pg-archive");
    sqlx::query(&format!(
        "REVOKE UPDATE (id) ON {}.ingress_protocol_epoch FROM {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove V1009 row-lock privilege");
    assert!(facade.submit(&first).await.is_err());
    assert_eq!(count(pool.global(), "ingress_messages").await, 0);
    sqlx::query(&format!(
        "GRANT UPDATE (id) ON {}.ingress_protocol_epoch TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore V1009 row-lock privilege");
    let decision = facade
        .submit(&first)
        .await
        .expect("V1009 direct acceptance");
    assert!(decision.class.advances());
    assert_eq!(
        facade
            .lookup_archive_or_stanza_id(&stamp)
            .await
            .expect("MAM lookup")
            .expect("archived")
            .id,
        "pg-archive"
    );
    let retry = facade
        .submit(&first)
        .await
        .expect("same committed submission");
    assert_eq!(decision.message_key, retry.message_key);
    assert_eq!(count(pool.global(), "mam_messages").await, 1);
    assert_eq!(count(pool.global(), "ingress_effect_receipts").await, 1);
    let committed = pg_snapshot(fixture).await;
    // Routing remains an external effect; committing the intent does not
    // create a downstream delivery or stream-management checkpoint.
    assert_eq!(committed, (1, 1, 2, 1, 1, 1, 1, 1, 0, 0, 0));

    sqlx::query(&format!(
        "UPDATE {}.mam_archive_sequences SET next_seq=0",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("make counter stale");
    assert!(
        SharedCorrectnessFixture::attach_postgres_mam_from_pool(
            &pool,
            &ingress_config,
            lineage_config.clone(),
            &owner_config,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .is_err(),
        "stale counter must be refused"
    );
    sqlx::query(&format!(
        "DELETE FROM {}.mam_archive_sequences",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove counter");
    assert!(
        SharedCorrectnessFixture::attach_postgres_mam_from_pool(
            &pool,
            &ingress_config,
            lineage_config.clone(),
            &owner_config,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .is_err(),
        "missing counter must be refused"
    );
    sqlx::query(&format!(
        "INSERT INTO {}.mam_archive_sequences (archive_jid,next_seq) VALUES ($1,1)",
        fixture.target,
    ))
    .bind("bob@example.test")
    .execute(&fixture.admin)
    .await
    .expect("restore counter");

    sqlx::query(&format!(
        "REVOKE INSERT ON {}.mam_messages FROM {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("inject late write failure");
    let (failed, failed_stamp) = submission(&principal, "failed-origin", "failed-archive");
    let late_error = facade
        .submit(&failed)
        .await
        .expect_err("MAM INSERT is denied");
    let SharedCorrectnessSubmitError::Commit(late_error) = late_error else {
        panic!("expected a commit failure from MAM");
    };
    let waddle_server::ingress_uow::IngressUowError::MamStore(
        waddle_xmpp::mam::MamTxStoreError::Database(sqlx::Error::Database(late_error)),
    ) = late_error.source
    else {
        panic!("expected the original MAM database permission failure");
    };
    assert_eq!(late_error.code().as_deref(), Some("42501"));
    assert!(facade
        .lookup_archive_or_stanza_id(&failed_stamp)
        .await
        .expect("lookup after failure")
        .is_none());
    assert_eq!(count(pool.global(), "mam_messages").await, 1);
    assert_eq!(count(pool.global(), "ingress_effect_receipts").await, 1);
    assert_eq!(
        pg_snapshot(fixture).await,
        committed,
        "late failure rolls back all writes"
    );
    sqlx::query(&format!(
        "GRANT INSERT ON {}.mam_messages TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore MAM insert");

    let (lost, lost_stamp) = submission(&principal, "lost-origin", "lost-archive");
    let error = facade
        .submit_with_simulated_lost_commit_ack(&lost)
        .await
        .expect_err("simulated lost acknowledgement");
    let SharedCorrectnessSubmitError::Commit(error) = error else {
        panic!("expected an ambiguous commit error");
    };
    assert_eq!(error.class(), IngressDecisionClass::AmbiguousCommit);
    assert!(!error.class().advances());
    let after_lost_ack = pg_snapshot(fixture).await;
    assert_eq!(after_lost_ack, (2, 2, 4, 2, 2, 1, 2, 2, 0, 0, 0));
    let replay = facade
        .submit(&lost)
        .await
        .expect("replay after known test commit");
    assert!(replay.class.advances());
    assert_eq!(
        facade
            .lookup_archive_or_stanza_id(&lost_stamp)
            .await
            .expect("lost lookup")
            .expect("row")
            .id,
        "lost-archive"
    );
    assert_eq!(count(pool.global(), "mam_messages").await, 2);
    assert_eq!(count(pool.global(), "ingress_effect_receipts").await, 2);
    assert_eq!(
        pg_snapshot(fixture).await,
        after_lost_ack,
        "replay cannot duplicate or advance state"
    );

    let main_hold = pool.global().begin().await.expect("occupy main");
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            SharedCorrectnessFixture::attach_postgres_mam_from_pool(
                &pool,
                &ingress_config,
                lineage_config.clone(),
                &owner_config,
                #[cfg(feature = "clustering")]
                None,
            )
        )
        .await
        .is_err(),
        "MAM attachment must wait for main admission"
    );
    assert!(tokio::time::timeout(
        Duration::from_millis(100),
        facade.lookup_archive_or_stanza_id(&stamp)
    )
    .await
    .is_err());
    let (reserved, _) = submission(&principal, "reserved-origin", "reserved-archive");
    assert!(facade
        .submit(&reserved)
        .await
        .expect("reserved ingress is available")
        .class
        .advances());
    drop(main_hold);
    assert!(facade
        .lookup_archive_or_stanza_id(&stamp)
        .await
        .expect("main recovered")
        .is_some());
    let lane = pool.ingress();
    let ingress_hold = lane.begin().await.expect("occupy ingress");
    let second_attachment = SharedCorrectnessFixture::attach_postgres_mam_from_pool(
        &pool,
        &ingress_config,
        lineage_config.clone(),
        &owner_config,
        #[cfg(feature = "clustering")]
        None,
    )
    .await
    .expect("MAM attachment uses main while ingress is occupied");
    drop(second_attachment);
    assert!(facade
        .lookup_archive_or_stanza_id(&stamp)
        .await
        .expect("main while ingress occupied")
        .is_some());
    let (blocked, _) = submission(&principal, "blocked-origin", "blocked-archive");
    assert!(
        tokio::time::timeout(Duration::from_millis(40), facade.submit(&blocked))
            .await
            .is_err()
    );
    drop(ingress_hold);
    assert!(facade
        .submit(&blocked)
        .await
        .expect("cancelled admission recovered")
        .class
        .advances());

    // Cancel after checkout and BEGIN while a real statement waits on a lock.
    let mut blocker = fixture.admin.begin().await.expect("MAM lock holder");
    sqlx::query(&format!(
        "LOCK TABLE {}.mam_messages IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *blocker)
    .await
    .expect("block MAM lookup");
    let lookup = {
        let facade = Arc::clone(&facade);
        let stamp = stamp.clone();
        tokio::spawn(async move { facade.lookup_archive_or_stanza_id(&stamp).await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE datname=$1 AND usename=$2 AND wait_event_type='Lock' \
                   AND query LIKE '%mam_messages%')",
            )
            .bind(&fixture.database_name)
            .bind(&fixture.runtime)
            .fetch_one(&fixture.admin)
            .await
            .expect("observe blocked runtime query");
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lookup is blocked after BEGIN");
    lookup.abort();
    assert!(lookup.await.expect_err("cancelled task").is_cancelled());
    blocker.rollback().await.expect("release MAM lock");
    assert!(tokio::time::timeout(
        Duration::from_secs(3),
        facade.lookup_archive_or_stanza_id(&stamp)
    )
    .await
    .expect("cancelled connection cleanup is bounded")
    .expect("replacement lookup")
    .is_some());

    let old_pids: Vec<i32> = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=$1 AND usename=$2",
    )
    .bind(&fixture.database_name)
    .bind(&fixture.runtime)
    .fetch_all(&fixture.admin)
    .await
    .expect("enumerate fixture runtime sessions");
    assert!(!old_pids.is_empty());
    for pid in &old_pids {
        let terminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
            .bind(pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("terminate fixture backend");
        assert!(terminated);
    }
    assert!(facade
        .lookup_archive_or_stanza_id(&stamp)
        .await
        .expect("attested reconnect")
        .is_some());
    let replacement_pids: Vec<i32> = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=$1 AND usename=$2",
    )
    .bind(&fixture.database_name)
    .bind(&fixture.runtime)
    .fetch_all(&fixture.admin)
    .await
    .expect("replacement sessions");
    assert!(replacement_pids.iter().any(|pid| !old_pids.contains(pid)));

    pooled_archive_resolver::exercise_postgres(
        &pool,
        &owner_config,
        &ingress_config,
        &lineage_config,
        fixture,
        &stamp,
    )
    .await;
    drop(facade);
    drop(pool);
}
