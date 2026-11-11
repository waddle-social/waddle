//! Disposable PostgreSQL 17 custody composition; no serving path is enabled.

use super::super::super::{url_with_schema, Fixture};
use super::{
    attested_pool, direct_pin, installed_v1009, now, pg_attempt, pg_seed_transaction,
    postgres_fixture, request, seed_pg_direct, test_url, ChronoDuration, Database, DatabasePool,
    DirectAdmission, DirectAdmissionLimits, DirectAdmissionOutcome, Duration, IngressUnitOfWork,
    LineageConfig, ALIAS_RETENTION, CATALOG_GRANT_LOCK,
};
use waddle_server::clustering::claims::PostgresClaimStore;
use waddle_server::db::physical_attestation::{
    custody_lab_postgres_ddl, CustodyBinding, CustodyDisposition, CustodyLab, CustodyLabError,
    CustodyLabIdentity, CustodyRetention,
};
use waddle_server::db::{DatabaseConfig, DatabaseDriver};
use waddle_server::ingress_uow::IngressUowError;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ownership::{ClaimEpoch, ClaimStore, EntityType, SharedNodeIdentity};

#[tokio::test]
async fn postgres_custody_uses_real_fences_before_binding_and_gc() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
        .fetch_one(&fixture.admin)
        .await
        .expect("server version");
    assert!(
        (170000..180000).contains(&version),
        "PostgreSQL 17 required"
    );
    let (config, retention) = postgres_fixture(&fixture).await;
    drop(retention);
    installed_v1009(&fixture).await;
    let admin_db = Database::from_config(
        "custody claim schema",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &fixture.target),
        ),
    )
    .await
    .expect("owner database");
    PostgresClaimStore::new(admin_db.clone())
        .ensure_schema()
        .await
        .expect("real claim schema");
    drop(admin_db);
    sqlx::raw_sql(&custody_lab_postgres_ddl(&config))
        .execute(&fixture.admin)
        .await
        .expect("disposable custody schema");
    let schema = &fixture.target;
    for table in [
        "custody_lab_quota",
        "custody_lab_gate",
        "custody_lab_admission",
        "custody_lab_binding",
        "custody_lab_counted",
        "custody_lab_terminal",
        "custody_lab_transfer",
    ] {
        sqlx::query(&format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.{table} TO {}",
            fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("fixture grants");
    }
    for statement in [
        format!(
            "GRANT SELECT, UPDATE (id) ON {schema}.sessions TO {}",
            fixture.runtime
        ),
        format!(
            "GRANT SELECT, UPDATE (entity) ON {schema}.clustering_claims TO {}",
            fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("real assertion grants");
    }
    let principal = AuthenticatedPrincipalRef::new(
        "bob@example.test".parse().expect("recipient"),
        AuthContextId::new(uuid::Uuid::new_v4()),
        AuthContextVersion::new(3),
        PrincipalAuthEpoch::new(5),
    );
    sqlx::query(&format!("INSERT INTO {schema}.users (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) VALUES ($1, 'bob', 'bob', 'bob', $2, $2)"))
        .bind(principal.bare_jid().to_string()).bind(now().to_rfc3339()).execute(&fixture.admin).await.expect("recipient user");
    sqlx::query(&format!("INSERT INTO {schema}.sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES ('fixture', $1, 'fixture-token', $2, 3, 5, $3, $3)"))
        .bind(principal.bare_jid().to_string()).bind(principal.auth_context_id().as_uuid().to_string()).bind(now().to_rfc3339()).execute(&fixture.admin).await.expect("durable principal");
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let mut admission = request(pin);
    let lease = pg_attempt(&fixture, &config, &mut admission).await;
    let owner = admission.owner.clone();
    let identity = SharedNodeIdentity::new(owner.clone());
    let claim_epoch = ClaimEpoch(41);
    sqlx::query(&format!("INSERT INTO {schema}.clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES ($1, $2, $3, $4, $5)"))
        .bind(format!("{}:{}", EntityType::SmSession.as_db_str(), admission.connection.sm_session.as_str()))
        .bind(EntityType::SmSession.as_db_str()).bind(&owner.node_id).bind(&owner.node_epoch).bind(claim_epoch.0).execute(&fixture.admin).await.expect("exact SM claim");
    let token = uuid::Uuid::new_v4();
    sqlx::query(&format!("INSERT INTO {schema}.custody_lab_gate (resource, connection_generation, sm_session_id, capability, ordinal) VALUES ($1, $2, $3, $4, 4294967295)"))
        .bind(admission.connection.resource.to_string()).bind(admission.connection.generation).bind(admission.connection.sm_session.as_str()).bind(token).execute(&fixture.admin).await.expect("synthetic gate");
    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        3,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("custody UOW", pool, 2, 1)
        .await
        .expect("database pool");
    let uow = IngressUnitOfWork::open_with_node_identity(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
        identity,
    )
    .expect("clustered UOW");
    let limits = DirectAdmissionLimits::new(8, 4096).expect("limits");
    let identity = CustodyLabIdentity {
        principal: principal.clone(),
        owner: owner.clone(),
        claim_epoch,
        connection: admission.connection.clone(),
        token,
    };
    let lab = CustodyLab::new(uow.clone(), &config, identity.clone(), limits).expect("lab");
    let wrong_principal = AuthenticatedPrincipalRef::new(
        principal.bare_jid().clone(),
        AuthContextId::new(uuid::Uuid::new_v4()),
        AuthContextVersion::new(3),
        PrincipalAuthEpoch::new(5),
    );
    let wrong = CustodyLab::new(
        uow.clone(),
        &config,
        CustodyLabIdentity {
            principal: wrong_principal,
            ..identity.clone()
        },
        limits,
    )
    .expect("lab");
    assert!(matches!(
        wrong.admit(&admission, &lease).await,
        Err(CustodyLabError::Rejected)
    ));
    sqlx::query(&format!(
        "UPDATE {schema}.clustering_claims SET claim_epoch = $1"
    ))
    .bind(claim_epoch.0 + 1)
    .execute(&fixture.admin)
    .await
    .expect("replace claim");
    assert!(matches!(
        lab.admit(&admission, &lease).await,
        Err(CustodyLabError::Uow(IngressUowError::ClaimFenceMissing))
    ));
    sqlx::query(&format!(
        "UPDATE {schema}.clustering_claims SET claim_epoch = $1"
    ))
    .bind(claim_epoch.0)
    .execute(&fixture.admin)
    .await
    .expect("restore claim");
    let mut forged = admission.clone();
    forged.pin.payload = waddle_xmpp::parser::message_from_string("<message xmlns='jabber:client' from='alice@example.test/phone' to='bob@example.test' type='chat'><body>changed</body></message>").expect("message");
    assert!(lab.admit(&forged, &lease).await.is_err());
    let before: (i64, i64) = sqlx::query_as(&format!("SELECT (SELECT count(*) FROM {schema}.custody_lab_admission), (SELECT count(*) FROM {schema}.ingress_send_attempts WHERE state = 0)"))
        .fetch_one(&fixture.admin).await.expect("atomic precondition");
    assert_eq!(before, (0, 1));
    assert_eq!(
        lab.admit(&admission, &lease)
            .await
            .expect("atomic admission"),
        DirectAdmissionOutcome::Recorded
    );
    let after: (String, i64, bool) = sqlx::query_as(&format!("SELECT a.payload, s.state::bigint, m.retention_eligible_at IS NULL FROM {schema}.custody_lab_admission a JOIN {schema}.ingress_send_attempts s ON s.message_key = a.message_key JOIN {schema}.ingress_messages m ON m.message_key = a.message_key"))
        .fetch_one(&fixture.admin).await.expect("stored admission");
    assert_eq!(
        waddle_xmpp::parser::message_from_string(&after.0).expect("payload"),
        admission.pin.payload
    );
    assert_eq!((after.1, after.2), (1, true));
    let (phone_pin, phone_envelope, phone_intent) =
        direct_pin(uuid::Uuid::new_v4(), "bob@example.test/phone");
    seed_pg_direct(&fixture, &phone_pin, phone_envelope, phone_intent).await;
    let mut phone = request(phone_pin);
    let phone_lease = pg_attempt(&fixture, &config, &mut phone).await;
    let wrong_resource = DirectAdmission::for_lease(
        phone.pin.clone(),
        &phone_lease,
        admission.connection.clone(),
    )
    .expect("the lease itself is valid for the phone obligation");
    assert_eq!(wrong_resource.connection, admission.connection);
    assert_ne!(
        wrong_resource.pin.key.resource,
        admission.connection.resource
    );
    assert!(matches!(
        lab.admit(&wrong_resource, &phone_lease).await,
        Err(CustodyLabError::Rejected)
    ));
    let phone_state: (i64, i64) = sqlx::query_as(&format!(
        "SELECT (SELECT count(*) FROM {schema}.custody_lab_admission WHERE message_key = $1), \
         (SELECT state::bigint FROM {schema}.ingress_send_attempts WHERE message_key = $1)"
    ))
    .bind(phone.pin.key.message_key.to_storage())
    .fetch_one(&fixture.admin)
    .await
    .expect("wrong-resource admission leaves the real attempt untouched");
    assert_eq!(phone_state, (0, 0));
    let unbound = CustodyBinding {
        admission_id: admission.pin.admission_id,
        connection: admission.connection.clone(),
        ordinal: 4294967296,
        h: 0,
    };
    assert!(lab.count(&admission, &unbound).await.is_err());
    let binding = lab.bind(&admission).await.expect("durable binding");
    assert_eq!(binding, unbound);
    let append: (String, String, i64, i64) = sqlx::query_as(&format!("SELECT accepting_stream_id, custody_payload, sequence, disposition::bigint FROM {schema}.sm_ingress_appends"))
        .fetch_one(&fixture.admin).await.expect("durable SM append");
    assert_eq!(append.0, admission.connection.sm_session.as_str());
    assert_eq!(
        waddle_xmpp::parser::message_from_string(&append.1).expect("SM payload"),
        admission.pin.payload
    );
    assert_eq!((append.2, append.3), (0, 0));
    assert!(lab
        .settle(&admission, &binding, CustodyDisposition::Ack, now())
        .await
        .is_err());
    assert_eq!(
        lab.collect(admission.pin.key.message_key, now())
            .await
            .expect("blocked"),
        CustodyRetention::Blocked
    );
    lab.count(&admission, &binding)
        .await
        .expect("count after bind");
    lab.settle(&admission, &binding, CustodyDisposition::Ack, now())
        .await
        .expect("fixture ACK");
    assert_eq!(
        lab.collect(
            admission.pin.key.message_key,
            now() + ALIAS_RETENTION - ChronoDuration::seconds(1)
        )
        .await
        .expect("fresh tail"),
        CustodyRetention::Retained
    );
    assert_eq!(
        lab.collect(admission.pin.key.message_key, now() + ALIAS_RETENTION)
            .await
            .expect("expired tail"),
        CustodyRetention::Deleted
    );
    let mut late = pg_seed_transaction(&fixture).await;
    let error = sqlx::query(&format!("INSERT INTO {schema}.sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES ($1, $2, $3, $4, $5, 0, 0, 'late', 0, 0)"))
        .bind(admission.pin.key.message_key.to_storage().to_string()).bind(admission.pin.key.kind.to_storage()).bind(admission.pin.key.semantic_identity_hash.as_slice()).bind(admission.connection.resource.to_string()).bind(admission.connection.sm_session.as_str()).execute(&mut *late).await.expect_err("late append rejected");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23503")
    );
    late.rollback().await.expect("rollback");

    for disposition in [CustodyDisposition::Transfer, CustodyDisposition::Tombstone] {
        let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
        seed_pg_direct(&fixture, &pin, envelope, intent).await;
        let mut candidate = request(pin);
        candidate.connection = admission.connection.clone();
        let lease = pg_attempt(&fixture, &config, &mut candidate).await;
        assert_eq!(
            lab.admit(&candidate, &lease)
                .await
                .expect("admit terminal case"),
            DirectAdmissionOutcome::Recorded
        );
        let bound = lab.bind(&candidate).await.expect("bind terminal case");
        lab.count(&candidate, &bound)
            .await
            .expect("count terminal case");
        let settled_at = now();
        lab.settle(&candidate, &bound, disposition, settled_at)
            .await
            .expect("settle terminal case");
        let transfer_payload: Option<String> = sqlx::query_scalar(&format!(
            "SELECT payload FROM {schema}.custody_lab_transfer WHERE admission_id = $1"
        ))
        .bind(candidate.pin.admission_id)
        .fetch_optional(&fixture.admin)
        .await
        .expect("transfer custody query");
        assert_eq!(
            transfer_payload.is_some(),
            disposition == CustodyDisposition::Transfer
        );
        if let Some(payload) = &transfer_payload {
            assert_eq!(
                waddle_xmpp::parser::message_from_string(payload).expect("transfer payload"),
                candidate.pin.payload
            );
        }
        assert_eq!(
            lab.collect(candidate.pin.key.message_key, settled_at + ALIAS_RETENTION)
                .await
                .expect("terminal retention"),
            CustodyRetention::Deleted
        );
        if disposition == CustodyDisposition::Transfer {
            let retained: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM {schema}.custody_lab_transfer WHERE admission_id = $1)"
            ))
            .bind(candidate.pin.admission_id)
            .fetch_one(&fixture.admin)
            .await
            .expect("transfer survives canonical collection");
            assert!(retained);
        }
    }

    let (old_pin, old_envelope, old_intent) =
        direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &old_pin, old_envelope, old_intent).await;
    let mut old_pending = request(old_pin);
    old_pending.connection = admission.connection.clone();
    let old_lease = pg_attempt(&fixture, &config, &mut old_pending).await;
    assert_eq!(
        lab.admit(&old_pending, &old_lease)
            .await
            .expect("pending admission before replacement"),
        DirectAdmissionOutcome::Recorded
    );
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let mut replacement = request(pin);
    replacement.owner = owner.clone();
    replacement.connection.sm_session = admission.connection.sm_session.clone();
    let new_token = uuid::Uuid::new_v4();
    sqlx::query(&format!("UPDATE {schema}.custody_lab_gate SET connection_generation = $2, capability = $3, ordinal = 0 WHERE resource = $1"))
        .bind(replacement.connection.resource.to_string()).bind(replacement.connection.generation).bind(new_token).execute(&fixture.admin).await.expect("same JID replacement");
    assert!(matches!(
        lab.bind(&old_pending).await,
        Err(CustodyLabError::Rejected)
    ));
    let pending_after_replacement: (i64, i64) = sqlx::query_as(&format!(
        "SELECT (SELECT count(*) FROM {schema}.ingress_messages WHERE message_key = $1), \
         (SELECT count(*) FROM {schema}.custody_lab_admission WHERE message_key = $1)"
    ))
    .bind(old_pending.pin.key.message_key.to_storage())
    .fetch_one(&fixture.admin)
    .await
    .expect("stale gate preserves pending custody");
    assert_eq!(pending_after_replacement, (1, 1));
    let replacement_lease = pg_attempt(&fixture, &config, &mut replacement).await;
    assert!(lab.admit(&replacement, &replacement_lease).await.is_err());
    let replacement_lab = CustodyLab::new(
        uow,
        &config,
        CustodyLabIdentity {
            principal,
            owner,
            claim_epoch,
            connection: replacement.connection.clone(),
            token: new_token,
        },
        limits,
    )
    .expect("replacement lab");
    let sealed = replacement_lab
        .seal(uuid::Uuid::new_v4())
        .await
        .expect("seal absence barrier");
    assert!(replacement_lab
        .lookup_sealed(&sealed, &replacement)
        .await
        .expect("post-fence lookup")
        .is_none());
    assert!(replacement_lab
        .admit(&replacement, &replacement_lease)
        .await
        .is_err());
    drop(replacement_lab);
    drop(lab);
    drop(wrong);
    drop(pool);
    fixture.cleanup().await;
}
