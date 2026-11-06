//! Required PostgreSQL 17 tests for the unselected durable repository.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::clustering::claims::PostgresClaimStore;
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{
    AttestedLineage, DeploymentUuid, LineageUuid, PgDatabaseIdentity, PgDatabaseName, PgIdentity,
    PgSchemaIdentity, PgSchemaName, PgSystemIdentifier,
};
use waddle_server::db::physical_attestation::{
    attested_pool, PgRoleName, PhysicalAttestationConfig,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, DatabasePool};
use waddle_server::ingress_uow::{IngressUnitOfWork, SendLease};
use waddle_server::sm_custody::{
    candidate_postgres_ddl, CustodyCapture, CustodyError, CustodyLimits, CustodyRecoveryAuthority,
    CustodyResolution, CustodyTerminal, PreparedDirectFrame, SessionGeneration,
    SmCustodyRepository,
};
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ownership::{
    BindingRef, ClaimEpoch, ClaimStore, ConnectionRef, EntityType, NodeIdentity, NodeLeaseIdentity,
    SharedNodeIdentity,
};
use waddle_xmpp::pending_delivery::SmSessionId;

use super::super::super::{
    url_with_schema, wait_for_blocked_application, Fixture, CATALOG_GRANT_LOCK,
};
use super::super::{direct_pin, now, pg_seed_transaction, postgres_fixture, seed_pg_direct};
use super::{installed_v1009, pg_attempt, request};

struct Candidate {
    fixture: Fixture,
    config: PhysicalAttestationConfig,
    uow: IngressUnitOfWork,
    capture: CustodyCapture,
    frame: PreparedDirectFrame,
    lease: SendLease,
}

impl Candidate {
    async fn new() -> Self {
        // Unlike the older optional fixtures, this suite cannot silently skip.
        let url = std::env::var("WADDLE_TEST_POSTGRES_URL")
            .expect("SM custody requires WADDLE_TEST_POSTGRES_URL and PostgreSQL 17");
        let fixture = Fixture::new(url).await;
        let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
            .fetch_one(&fixture.admin)
            .await
            .expect("version");
        assert!(
            (170000..180000).contains(&version),
            "PostgreSQL 17 required"
        );
        let (config, retention) = postgres_fixture(&fixture).await;
        drop(retention);
        installed_v1009(&fixture).await;
        let owner_db = Database::from_config(
            "custody owner",
            &DatabaseConfig::new(
                DatabaseDriver::Postgres,
                url_with_schema(&fixture.base_url, &fixture.target),
            ),
        )
        .await
        .expect("owner DB");
        PostgresClaimStore::new(owner_db)
            .ensure_schema()
            .await
            .expect("claim schema");
        sqlx::raw_sql(&candidate_postgres_ddl(&config).expect("candidate DDL"))
            .execute(&fixture.admin)
            .await
            .expect("unregistered schema");
        for table in [
            "sm_custody_quota",
            "sm_custody_gates",
            "sm_custody_operations",
            "sm_custody_slots",
            "sm_custody_terminals",
            "sm_custody_transfers",
        ] {
            sqlx::query(&format!(
                "GRANT SELECT, INSERT, UPDATE ON {}.{table} TO {}",
                fixture.target, fixture.runtime
            ))
            .execute(&fixture.admin)
            .await
            .expect("candidate DML grants");
        }
        for table in ["sessions", "clustering_claims"] {
            sqlx::query(&format!(
                "GRANT SELECT, UPDATE ON {}.{table} TO {}",
                fixture.target, fixture.runtime
            ))
            .execute(&fixture.admin)
            .await
            .expect("assertion grants");
        }
        let principal = AuthenticatedPrincipalRef::new(
            "bob@example.test".parse().expect("JID"),
            AuthContextId::new(Uuid::new_v4()),
            AuthContextVersion::new(3),
            PrincipalAuthEpoch::new(5),
        );
        let q = &fixture.target;
        sqlx::query(&format!("INSERT INTO {q}.users (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) VALUES ($1,'bob','bob','bob',$2,$2)"))
            .bind(principal.bare_jid().to_string()).bind(now().to_rfc3339()).execute(&fixture.admin).await.expect("user");
        sqlx::query(&format!("INSERT INTO {q}.sessions (id,user_jid,token_hash,auth_context_id,auth_context_version,principal_auth_epoch,created_at,last_used_at) VALUES ('custody',$1,'custody-token',$2,3,5,$3,$3)"))
            .bind(principal.bare_jid().to_string()).bind(principal.auth_context_id().as_uuid().to_string())
            .bind(now().to_rfc3339()).execute(&fixture.admin).await.expect("principal");
        let (pin, envelope, intent) = direct_pin(Uuid::new_v4(), "bob@example.test/laptop");
        seed_pg_direct(&fixture, &pin, envelope, intent).await;
        let mut admission = request(pin);
        let lease = pg_attempt(&fixture, &config, &mut admission).await;
        let owner = admission.owner.clone();
        let capture = CustodyCapture {
            gate_id: Uuid::new_v4(),
            token: Uuid::new_v4(),
            resource: admission.pin.key.resource.clone(),
            binding: BindingRef::fresh(ConnectionRef::fresh(
                NodeLeaseIdentity::from_node_identity(&owner),
            )),
            session: SmSessionId::new("durable-custody-session"),
            generation: SessionGeneration::new(7).expect("generation"),
            principal,
            owner: owner.clone(),
            claim: ClaimEpoch(41),
        };
        sqlx::query(&format!("INSERT INTO {q}.clustering_claims (entity,entity_type,node_id,node_epoch,claim_epoch) VALUES ($1,$2,$3,$4,$5)"))
            .bind(format!("{}:{}",EntityType::SmSession.as_db_str(),capture.session.as_str()))
            .bind(EntityType::SmSession.as_db_str()).bind(&owner.node_id).bind(&owner.node_epoch)
            .bind(capture.claim.0).execute(&fixture.admin).await.expect("exact claim");
        let uow = uow(
            &fixture.runtime_options,
            &config,
            fixture.lineage.deployment_uuid.0,
            &owner,
        )
        .await;
        let frame = PreparedDirectFrame::for_lease(
            admission.pin.admission_id,
            admission.pin.key,
            admission.pin.payload,
            admission.pin.received_at,
            &lease,
        )
        .expect("prepared");
        let candidate = Self {
            fixture,
            config,
            uow,
            capture,
            frame,
            lease,
        };
        candidate
            .repository()
            .install_captured_session()
            .await
            .expect("inert capture");
        candidate
    }

    fn repository(&self) -> SmCustodyRepository {
        SmCustodyRepository::new(
            self.uow.clone(),
            &self.config,
            self.capture.clone(),
            CustodyLimits::new(16, 64 * 1024).expect("limits"),
        )
        .expect("shared candidate")
    }

    fn authority(&self) -> CustodyRecoveryAuthority {
        CustodyRecoveryAuthority {
            owner: self.capture.owner.clone(),
            claim: self.capture.claim,
        }
    }

    async fn settle_executor(&self) {
        let q = &self.fixture.target;
        let mut tx = pg_seed_transaction(&self.fixture).await;
        sqlx::query(&format!(
            "UPDATE {q}.ingress_send_attempts SET state = 2 WHERE message_key = $1"
        ))
        .bind(self.frame.key.message_key.to_storage())
        .execute(&mut *tx)
        .await
        .expect("executor completion");
        sqlx::query(&format!("INSERT INTO {q}.ingress_effect_receipts (message_key,kind,semantic_identity_hash) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING"))
            .bind(self.frame.key.message_key.to_storage()).bind(self.frame.key.kind.to_storage())
            .bind(self.frame.key.semantic_identity_hash.as_slice()).execute(&mut *tx).await.expect("executor receipt");
        sqlx::query(&format!("UPDATE {q}.ingress_messages SET terminal_at = clock_timestamp() WHERE message_key = $1"))
            .bind(self.frame.key.message_key.to_storage()).execute(&mut *tx).await.expect("canonical terminal");
        tx.commit().await.expect("executor commit");
    }
}

async fn uow(
    options: &PgConnectOptions,
    config: &PhysicalAttestationConfig,
    deployment: Uuid,
    owner: &NodeIdentity,
) -> IngressUnitOfWork {
    let pool = attested_pool(options.clone(), config.clone(), 3, Duration::from_secs(2))
        .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("SM custody", pool, 2, 1)
        .await
        .expect("shared pool");
    IngressUnitOfWork::open_with_node_identity(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: Some(DeploymentUuid(deployment)),
            action: None,
        },
        SharedNodeIdentity::new(owner.clone()),
    )
    .expect("fenced UOW")
}

#[tokio::test]
async fn postgres_sm_custody_commits_before_acceptance_and_reclaims_only_after_ack() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let mut foreign = c.fixture.lineage.clone();
    foreign
        .postgres_identity
        .as_mut()
        .expect("PG identity")
        .database
        .oid += 1;
    let foreign = PhysicalAttestationConfig::new(
        foreign,
        PgRoleName::new(&c.fixture.installer).expect("role"),
        PgRoleName::new(&c.fixture.runtime).expect("role"),
        PgRoleName::new(&c.fixture.definer).expect("role"),
        PgSchemaName(c.fixture.trusted.clone()),
    )
    .expect("foreign config with same schema name");
    assert!(
        SmCustodyRepository::new(
            c.uow.clone(),
            &foreign,
            c.capture.clone(),
            CustodyLimits::new(16, 65536).expect("limits")
        )
        .is_err(),
        "full physical identity must match the UOW"
    );
    let mut forged = c.frame.clone();
    forged.payload.bodies.clear();
    assert!(repo.admit(&forged, &c.lease).await.is_err());
    let admission = repo
        .admit(&c.frame, &c.lease)
        .await
        .expect("committed payload admission");
    assert_eq!(
        admission,
        repo.admit(&c.frame, &c.lease).await.expect("exact replay")
    );
    let slot = repo
        .bind(&admission)
        .await
        .expect("durable slot before memory acceptance");
    assert_eq!(slot.ordinal(), 1);
    assert_eq!(slot, repo.bind(&admission).await.expect("same slot"));
    // A future adapter may now enqueue, but neither admission nor bind did so.
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    sender
        .send(slot.clone())
        .await
        .expect("memory enqueue only after known bind");
    assert_eq!(receiver.recv().await, Some(slot.clone()));
    assert!(
        repo.acknowledge(&slot, slot.h()).await.is_err(),
        "binding is not exposure"
    );
    repo.expose(&slot).await.expect("committed exposure");
    assert!(repo.expose(&slot).await.is_err(), "one transport attempt");
    assert!(repo.acknowledge(&slot, 2).await.is_err());
    let mut delete = pg_seed_transaction(&c.fixture).await;
    assert!(
        sqlx::query(&format!(
            "DELETE FROM {}.ingress_messages WHERE message_key = $1",
            c.fixture.target
        ))
        .bind(c.frame.key.message_key.to_storage())
        .execute(&mut *delete)
        .await
        .is_err(),
        "raw deletion cannot erase pending custody"
    );
    delete.rollback().await.expect("rollback failed delete");
    c.settle_executor().await;
    repo.acknowledge(&slot, slot.h())
        .await
        .expect("positive ACK");
    let seal = repo.seal().await.expect("idempotent seal");
    assert!(!repo
        .collect_terminal(&seal, c.frame.key.message_key)
        .await
        .expect("fresh retention tail"));
    let mut tx = pg_seed_transaction(&c.fixture).await;
    sqlx::query(&format!("UPDATE {}.ingress_messages SET retention_eligible_at = clock_timestamp() - interval '9 days' WHERE message_key = $1",c.fixture.target))
        .bind(c.frame.key.message_key.to_storage()).execute(&mut *tx).await.expect("advance fixture tail");
    tx.commit().await.expect("tail commit");
    assert!(repo
        .collect_terminal(&seal, c.frame.key.message_key)
        .await
        .expect("candidate collection"));
    let recovered = repo
        .reconcile(&seal, &c.authority(), c.frame.operation_id)
        .await
        .expect("journal outlives canonical");
    let CustodyResolution::Committed(recovered) = recovered else {
        panic!("committed journal lost")
    };
    assert_eq!(recovered.frame(), &c.frame);
    assert_eq!(recovered.terminal(), Some(CustodyTerminal::Ack));
    assert!(
        repo.bind(&admission).await.is_err(),
        "late original cannot revive after collection"
    );
    let mut raw = PgConnection::connect_with(&c.fixture.runtime_options)
        .await
        .expect("raw runtime late writer");
    let late_gate = Uuid::new_v4();
    sqlx::query(&format!("INSERT INTO {}.sm_custody_gates (gate_id,token,resource,session_id,generation,endpoint,node_id,node_incarnation,claim_epoch) SELECT $1,token,resource,'late-new-session',generation,endpoint,node_id,node_incarnation,claim_epoch FROM {}.sm_custody_gates WHERE gate_id = $2",c.fixture.target,c.fixture.target))
        .bind(late_gate).bind(c.capture.gate_id).execute(&mut raw).await.expect("independent late gate");
    assert!(sqlx::query(&format!("INSERT INTO {}.sm_custody_operations (operation_id,gate_id,message_key,receipt_kind,semantic_identity_hash,resource,attempt_token,payload,received_at) VALUES ($1,$2,$3,$4,$5,$6,$7,'late',$8)",c.fixture.target))
        .bind(Uuid::new_v4()).bind(late_gate).bind(c.frame.key.message_key.to_storage())
        .bind(c.frame.key.kind.to_storage()).bind(c.frame.key.semantic_identity_hash.as_slice())
        .bind(c.frame.key.resource.to_string()).bind(Uuid::new_v4()).bind(c.frame.received_at)
        .execute(&mut raw).await.is_err(),"even a raw delayed candidate insertion cannot follow canonical collection");
    drop(raw);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_sm_custody_seal_proves_absence_and_rejects_generation_replacement() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let mut replacement = c.capture.clone();
    replacement.gate_id = Uuid::new_v4();
    replacement.token = Uuid::new_v4();
    replacement.generation = SessionGeneration::new(8).expect("generation");
    let replacement = SmCustodyRepository::new(
        c.uow.clone(),
        &c.config,
        replacement,
        CustodyLimits::new(16, 65536).expect("limits"),
    )
    .expect("candidate replacement");
    assert!(
        replacement.install_captured_session().await.is_err(),
        "this tranche forbids a second generation for one session"
    );
    let seal = repo.seal().await.expect("seal before original enrollment");
    assert!(matches!(
        repo.reconcile(&seal, &c.authority(), c.frame.operation_id)
            .await
            .expect("sealed exact absence"),
        CustodyResolution::ProvenAbsent
    ));
    assert!(
        repo.admit(&c.frame, &c.lease).await.is_err(),
        "delayed first attempt cannot enter after negative proof"
    );
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.sm_custody_operations",
        c.fixture.target
    ))
    .fetch_one(&c.fixture.admin)
    .await
    .expect("journal count");
    assert_eq!(count, 0);
    drop(repo);
    drop(replacement);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_sm_custody_seal_fences_delayed_admission_and_successor_reads_inventory() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let admission = repo.admit(&c.frame, &c.lease).await.expect("admit");
    let next_owner = NodeIdentity::new("successor-node", "successor-incarnation");
    sqlx::query(&format!(
        "UPDATE {}.clustering_claims SET node_id = $1, node_epoch = $2, claim_epoch = 42",
        c.fixture.target
    ))
    .bind(&next_owner.node_id)
    .bind(&next_owner.node_epoch)
    .execute(&c.fixture.admin)
    .await
    .expect("claim turnover");
    sqlx::query(&format!("DELETE FROM {}.sessions", c.fixture.target))
        .execute(&c.fixture.admin)
        .await
        .expect("old principal revoked");
    assert!(repo.bind(&admission).await.is_err());
    let next_uow = uow(
        &c.fixture.runtime_options,
        &c.config,
        c.fixture.lineage.deployment_uuid.0,
        &next_owner,
    )
    .await;
    let authority = CustodyRecoveryAuthority {
        owner: next_owner,
        claim: ClaimEpoch(42),
    };
    let successor = SmCustodyRepository::recover_session(
        next_uow,
        &c.config,
        &SmSessionId::new("durable-custody-session"),
        &"bob@example.test/laptop".parse().expect("resource"),
        &authority,
        CustodyLimits::new(16, 65536).expect("limits"),
    )
    .await
    .expect("discover old capture from durable storage");
    let sealed = successor
        .seal_for_recovery(&authority)
        .await
        .expect("successor seal");
    let page = successor
        .inventory(&sealed, &authority, None)
        .await
        .expect("read without old principal");
    assert_eq!(page.operations.len(), 1);
    assert!(page.next.is_none());
    assert_eq!(page.operations[0].frame(), &c.frame);
    assert!(matches!(
        successor
            .reconcile(&sealed, &authority, Uuid::new_v4())
            .await
            .expect("exact ID absence"),
        CustodyResolution::ProvenAbsent
    ));
    assert!(matches!(
        successor
            .reconcile(&sealed, &authority, c.frame.operation_id)
            .await
            .expect("exact committed ID"),
        CustodyResolution::Committed(_)
    ));
    successor
        .transfer_recovered(
            &sealed,
            &authority,
            &page.operations[0]
                .admission(&sealed)
                .expect("exact sealed capability"),
        )
        .await
        .expect("successor owns full-payload recovery without old principal");
    drop(successor);
    drop(repo);
    c.fixture.cleanup().await;
}

fn child_input(c: &Candidate) -> serde_json::Value {
    let pg = c
        .fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("PG identity");
    serde_json::json!({
        "url":c.fixture.base_url, "schema":c.fixture.target,
        "runtime":c.fixture.runtime, "installer":c.fixture.installer,
        "definer":c.fixture.definer, "trusted":c.fixture.trusted,
        "lineage":c.fixture.lineage.lineage_uuid.0, "deployment":c.fixture.lineage.deployment_uuid.0,
        "system":pg.system_identifier.0, "db_name":pg.database.name.0, "db_oid":pg.database.oid,
        "schema_oid":pg.schema.oid, "gate":c.capture.gate_id,"token":c.capture.token,
        "resource":c.capture.resource.to_string(),"binding":c.capture.binding,
        "principal":c.capture.principal,"session":c.capture.session.as_str(),
        "generation":c.capture.generation.get(),"node":c.capture.owner.node_id,
        "incarnation":c.capture.owner.node_epoch,"claim":c.capture.claim.0,
        "operation":c.frame.operation_id,
    })
}

fn text_field<'a>(input: &'a serde_json::Value, key: &str) -> &'a str {
    input[key].as_str().expect("child text field")
}

fn uuid_field(input: &serde_json::Value, key: &str) -> Uuid {
    text_field(input, key).parse().expect("child UUID")
}

/// Native subprocess entry, invoked only by the parent concurrency test. An
/// ordinary test run ignores this helper rather than silently skipping PG work.
#[tokio::test]
#[ignore = "native subprocess entry for postgres_sm_custody_delayed_commit_survives_origin_death"]
async fn sm_custody_process_child() {
    let input: serde_json::Value = serde_json::from_str(
        &std::env::var("WADDLE_SM_CUSTODY_CHILD").expect("parent descriptor required"),
    )
    .expect("descriptor");
    let lineage = AttestedLineage {
        lineage_uuid: LineageUuid(uuid_field(&input, "lineage")),
        deployment_uuid: DeploymentUuid(uuid_field(&input, "deployment")),
        postgres_identity: Some(PgIdentity {
            system_identifier: PgSystemIdentifier(input["system"].as_u64().expect("system id")),
            database: PgDatabaseIdentity {
                oid: input["db_oid"].as_u64().expect("database oid") as u32,
                name: PgDatabaseName(text_field(&input, "db_name").to_owned()),
            },
            schema: PgSchemaIdentity {
                oid: input["schema_oid"].as_u64().expect("schema oid") as u32,
                name: PgSchemaName(text_field(&input, "schema").to_owned()),
            },
        }),
    };
    let config = PhysicalAttestationConfig::new(
        lineage,
        PgRoleName::new(text_field(&input, "installer")).expect("role"),
        PgRoleName::new(text_field(&input, "runtime")).expect("role"),
        PgRoleName::new(text_field(&input, "definer")).expect("role"),
        PgSchemaName(text_field(&input, "trusted").to_owned()),
    )
    .expect("physical config");
    let options = PgConnectOptions::from_str(text_field(&input, "url"))
        .expect("URL")
        .username(text_field(&input, "runtime"))
        .options([("search_path", text_field(&input, "schema"))])
        .application_name("sm-custody-child");
    let owner = NodeIdentity::new(
        text_field(&input, "node"),
        text_field(&input, "incarnation"),
    );
    let capture = CustodyCapture {
        gate_id: uuid_field(&input, "gate"),
        token: uuid_field(&input, "token"),
        resource: text_field(&input, "resource").parse().expect("resource"),
        binding: serde_json::from_value(input["binding"].clone()).expect("binding"),
        principal: serde_json::from_value(input["principal"].clone()).expect("principal"),
        session: SmSessionId::new(text_field(&input, "session")),
        generation: SessionGeneration::new(input["generation"].as_u64().expect("generation"))
            .expect("generation"),
        owner: owner.clone(),
        claim: ClaimEpoch(input["claim"].as_i64().expect("claim")),
    };
    let uow = uow(&options, &config, uuid_field(&input, "deployment"), &owner).await;
    let repo = SmCustodyRepository::new(
        uow,
        &config,
        capture,
        CustodyLimits::new(16, 65536).expect("limits"),
    )
    .expect("repository");
    let admission = repo
        .committed_admission(uuid_field(&input, "operation"))
        .await
        .expect("positive readback");
    repo.bind(&admission).await.expect("child durable bind");
}

#[tokio::test]
async fn postgres_sm_custody_delayed_commit_survives_origin_death() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    for committed in [false, true] {
        delayed_commit_schedule(committed).await;
    }
}

async fn delayed_commit_schedule(committed: bool) {
    let c = Candidate::new().await;
    let repo = Arc::new(c.repository());
    repo.admit(&c.frame, &c.lease)
        .await
        .expect("admission survives origin");
    let mut blocker = PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("independent blocker backend");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await
        .expect("PID");
    sqlx::query("SELECT pg_advisory_lock(2218001)")
        .execute(&mut blocker)
        .await
        .expect("hold commit barrier");
    let q = &c.fixture.target;
    let outcome = if committed {
        "RETURN NEW;"
    } else {
        "RAISE EXCEPTION 'controlled custody commit rollback';"
    };
    // The attested UOW deliberately tightens ordinary lock waits to 100ms.
    // This disposable deferred trigger needs a longer wait for OS scheduling;
    // no repository or production timeout is changed.
    sqlx::raw_sql(&format!("CREATE FUNCTION {q}.pause_custody_commit() RETURNS trigger LANGUAGE plpgsql SET lock_timeout = '3s' AS $$ BEGIN PERFORM pg_catalog.pg_advisory_xact_lock(2218001); {outcome} END $$; CREATE CONSTRAINT TRIGGER pause_custody_commit AFTER INSERT ON {q}.sm_custody_slots DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {q}.pause_custody_commit();"))
        .execute(&c.fixture.admin).await.expect("controlled deferred COMMIT barrier");
    let mut child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
        .arg("--ignored").arg("--exact")
        .arg("retention_lab_tests::direct_admission_tests::sm_custody_tests::sm_custody_process_child")
        .arg("--nocapture")
        .env("WADDLE_SM_CUSTODY_CHILD",child_input(&c).to_string())
        .kill_on_drop(true).spawn().expect("independent origin process");
    let original_pid =
        wait_for_blocked_application(&c.fixture.admin, "sm-custody-child", blocker_pid).await;
    let committing: bool =
        sqlx::query_scalar("SELECT query = 'COMMIT' FROM pg_stat_activity WHERE pid = $1")
            .bind(original_pid)
            .fetch_one(&c.fixture.admin)
            .await
            .expect("backend inside COMMIT");
    assert!(
        committing,
        "the barrier must be inside COMMIT, not a precommit statement"
    );
    if committed {
        let pid = child.id().expect("origin PID").to_string();
        assert!(tokio::process::Command::new("/bin/kill")
            .arg("-STOP")
            .arg(&pid)
            .status()
            .await
            .expect("stop origin before receiving COMMIT reply")
            .success());
    } else {
        child
            .start_kill()
            .expect("kill origin after COMMIT started");
        child.wait().await.expect("origin exited");
    }
    let before: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {q}.sm_custody_slots"))
        .fetch_one(&c.fixture.admin)
        .await
        .expect("ordinary absent read");
    assert_eq!(before, 0, "absence during delayed COMMIT is not rollback");
    let recovery = Arc::clone(&repo);
    let authority = c.authority();
    let seal_task = tokio::spawn(async move { recovery.seal_for_recovery(&authority).await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        !seal_task.is_finished(),
        "seal must wait behind the unresolved original gate lock"
    );
    sqlx::query("SELECT pg_advisory_unlock(2218001)")
        .execute(&mut blocker)
        .await
        .expect("allow original terminal outcome");
    if committed {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let visible: i64 =
                    sqlx::query_scalar(&format!("SELECT count(*) FROM {q}.sm_custody_slots"))
                        .fetch_one(&c.fixture.admin)
                        .await
                        .expect("independent commit observation");
                if visible == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("original commits while caller is stopped");
        child
            .start_kill()
            .expect("kill origin before it can observe COMMIT reply");
        child.wait().await.expect("stopped origin exited");
    }
    let seal = tokio::time::timeout(Duration::from_secs(5), seal_task)
        .await
        .expect("bounded recovery")
        .expect("seal task")
        .expect("known committed seal");
    let page = repo
        .inventory(&seal, &c.authority(), None)
        .await
        .expect("autonomous restart inventory");
    assert_eq!(page.operations.len(), 1);
    assert_eq!(page.operations[0].frame(), &c.frame);
    assert_eq!(
        page.operations[0].slot().map(|slot| slot.ordinal()),
        committed.then_some(1)
    );
    let result = repo
        .reconcile(&seal, &c.authority(), c.frame.operation_id)
        .await
        .expect("exact operation read");
    assert!(matches!(result, CustodyResolution::Committed(_)));
    assert!(
        repo.committed_admission(c.frame.operation_id)
            .await
            .is_err(),
        "sealed original cannot re-enter"
    );
    repo.transfer_recovered(
        &seal,
        &c.authority(),
        &page.operations[0]
            .admission(&seal)
            .expect("exact gate capability"),
    )
    .await
    .expect("retained payload has a recovery sink");
    drop(blocker);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_sm_custody_transfer_wrap_and_capacity_are_closed() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    {
        let c = Candidate::new().await;
        let mut wrong = c.capture.clone();
        wrong.generation = SessionGeneration::new(8).expect("generation");
        let wrong = SmCustodyRepository::new(
            c.uow.clone(),
            &c.config,
            wrong,
            CustodyLimits::new(16, 65536).expect("limits"),
        )
        .expect("wrong capture");
        assert!(wrong.admit(&c.frame, &c.lease).await.is_err());
        let tiny = SmCustodyRepository::new(
            c.uow.clone(),
            &c.config,
            c.capture.clone(),
            CustodyLimits::new(1, 1).expect("tiny limits"),
        )
        .expect("tiny repo");
        assert!(matches!(
            tiny.admit(&c.frame, &c.lease).await,
            Err(CustodyError::Capacity)
        ));
        let repo = c.repository();
        sqlx::query(&format!("UPDATE {}.sm_custody_gates SET appended=4294967295,exposed=4294967295,peer_acked=4294967295",c.fixture.target))
            .execute(&c.fixture.admin).await.expect("settled pre-wrap prefix");
        let admission = repo.admit(&c.frame, &c.lease).await.expect("admit");
        let slot = repo.bind(&admission).await.expect("wrap slot");
        assert_eq!((slot.ordinal(), slot.h()), (4294967296, 0));
        c.settle_executor().await;
        assert!(
            sqlx::query(&format!(
                "INSERT INTO {}.sm_custody_terminals (operation_id,disposition) VALUES ($1,3)",
                c.fixture.target
            ))
            .bind(c.frame.operation_id)
            .execute(&c.fixture.admin)
            .await
            .is_err(),
            "no unproven tombstone disposition is supported"
        );
        repo.transfer(&admission)
            .await
            .expect("atomic full payload transfer");
        assert!(repo.expose(&slot).await.is_err());
        assert!(repo.acknowledge(&slot, slot.h()).await.is_err());
        let retained: Option<(String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(&format!(
            "SELECT payload,received_at FROM {}.sm_custody_transfers",
            c.fixture.target
        ))
        .fetch_optional(&c.fixture.admin)
        .await
        .expect("transfer payload");
        assert!(retained.is_some());
        if let Some((payload, time)) = retained {
            assert_eq!(
                waddle_xmpp::parser::message_from_string(&payload).expect("typed transfer"),
                c.frame.payload
            );
            assert_eq!(time, c.frame.received_at);
        }
        let seal = repo.seal().await.expect("seal");
        assert!(repo.admit(&c.frame, &c.lease).await.is_err());
        assert!(matches!(
            repo.reconcile(&seal, &c.authority(), c.frame.operation_id)
                .await
                .expect("terminal journal"),
            CustodyResolution::Committed(_)
        ));
        drop(wrong);
        drop(tiny);
        drop(repo);
        c.fixture.cleanup().await;
    }
}
