//! PG17 schedules for the unselected pinned receipt authority.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::clustering::singleton_lease::{
    prepare_owner_schema, GuardBackend, LeaseGrant, LeaseOutcome, ProcessIdentity,
    SingletonLeaseStore,
};
use waddle_server::db::physical_attestation::{
    pinned_attested_pool, GuardedReceiptOperationPins, PgRoleName, PhysicalAttestationConfig,
    PinnedCatalogError, PinnedReceiptAuthority, PinnedReceiptCommit, PinnedReceiptError,
    PinnedRuntimeAuthority, ReceiptOperation, ReceiptOperationPins, ReceiptReadback,
    CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::db::{DarkMigrationLedgerPins, Database, DatabaseConfig, DatabaseDriver};
use waddle_server::ingress_substrate::{EffectReceiptKind, EPOCH_GUARDED_TABLES};
use waddle_server::pending_delivery::bootstrap_pending_delivery_owner_schema;
use waddle_xmpp::ingress::MessageKey;

use super::{
    dark_attestor_fixture, prepare_candidate_application_fixture, proxy_runtime_options, test_url,
    url_with_schema, Fixture, CATALOG_GRANT_LOCK,
};

const INGRESS_API: &str =
    include_str!("../../src/db/migrations/owner/ingress_definer_api_candidate.sql");
const LOOKUP: &str =
    include_str!("../../src/db/migrations/owner/p21_receipt_journal_lookup_candidate.sql");
const GUARDED_RECEIPT: &str =
    include_str!("../../src/db/migrations/owner/p21_guarded_receipt_candidate.sql");

async fn sql(fixture: &Fixture, statement: &str) {
    sqlx::raw_sql(statement)
        .execute(&fixture.admin)
        .await
        .unwrap_or_else(|error| panic!("fixture SQL failed: {statement}: {error}"));
}

async fn relation(fixture: &Fixture, name: &str) -> u32 {
    sqlx::query_scalar::<_, Oid>("SELECT $1::pg_catalog.regclass::pg_catalog.oid")
        .bind(format!("{}.{}", fixture.target, name))
        .fetch_one(&fixture.admin)
        .await
        .expect("fixture relation OID")
        .0
}

async fn function(fixture: &Fixture, signature: &str) -> u32 {
    sqlx::query_scalar::<_, Oid>("SELECT $1::pg_catalog.regprocedure::pg_catalog.oid")
        .bind(format!("{}.{}", fixture.target, signature))
        .fetch_one(&fixture.admin)
        .await
        .expect("fixture function OID")
        .0
}

async fn role(fixture: &Fixture, name: &str) -> u32 {
    sqlx::query_scalar::<_, Oid>("SELECT oid FROM pg_catalog.pg_roles WHERE rolname=$1")
        .bind(name)
        .fetch_one(&fixture.admin)
        .await
        .expect("fixture role OID")
        .0
}

async fn wait_backend_terminated(fixture: &Fixture, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let live: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",
            )
            .bind(pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("inspect discarded backend");
            if !live {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("uncertain backend is eventually discarded");
}

async fn assert_same_transaction(fixture: &Fixture, before: GuardBackend, after: GuardBackend) {
    assert_eq!(
        before, after,
        "preflight and operation use the same transaction"
    );
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
         WHERE pid=$1 AND backend_start=$2 AND xact_start=$3 AND datid=$4 \
           AND backend_xid::text=$5 AND pg_catalog.pg_postmaster_start_time()=$6)",
    )
    .bind(after.pid)
    .bind(after.backend_start)
    .bind(after.transaction_start)
    .bind(Oid(after.database_oid))
    .bind((after.transaction_id & u64::from(u32::MAX)).to_string())
    .bind(after.postmaster_start)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect exact transaction");
    assert!(live, "the observed PID and XID are still live at the pause");
}

async fn expire_lease(fixture: &Fixture, grant: &LeaseGrant) {
    let changed = sqlx::query(&format!(
        "UPDATE {}.singleton_candidate_leases \
         SET expires_at=pg_catalog.clock_timestamp()-interval '1 second' \
         WHERE incarnation=$1 AND node_id=$2 AND generation=$3",
        fixture.target
    ))
    .bind(grant.identity().incarnation())
    .bind(grant.identity().node_id())
    .bind(grant.generation())
    .execute(&fixture.admin)
    .await
    .expect("expire exact lease")
    .rows_affected();
    assert_eq!(changed, 1);
}

async fn wait_for_blocked_registration(fixture: &Fixture, blocker_pid: i32, relation: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE wait_event_type='Lock' AND $1=ANY(pg_catalog.pg_blocking_pids(pid)) \
                   AND query LIKE $2)",
            )
            .bind(blocker_pid)
            .bind(format!("%{}%{relation}%FOR UPDATE%", fixture.target))
            .fetch_one(&fixture.admin)
            .await
            .expect("inspect registration waiter");
            if waiting {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("registration waits on the expected blocker");
}

async fn setup(
    fixture: &Fixture,
) -> (
    PinnedReceiptAuthority,
    SingletonLeaseStore,
    LeaseGrant,
    Vec<ReceiptOperation>,
    String,
    PhysicalAttestationConfig,
    PinnedRuntimeAuthority,
    GuardedReceiptOperationPins,
) {
    prepare_candidate_application_fixture(fixture).await;
    let owner = Database::from_config(
        "pinned receipt fixture owner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &fixture.target),
        ),
    )
    .await
    .expect("owner database");
    bootstrap_pending_delivery_owner_schema(&owner)
        .await
        .expect("prepare pending delivery");
    prepare_owner_schema(&owner, &fixture.target)
        .await
        .expect("prepare singleton slot");
    let leases = SingletonLeaseStore::new_unverified(owner, &fixture.target).expect("lease store");
    let identity = ProcessIdentity::new("pinned-receipt-node", Uuid::new_v4()).expect("identity");
    let grant = match leases
        .register(Uuid::new_v4(), identity, Duration::from_secs(300))
        .await
        .expect("register fixture lease")
    {
        LeaseOutcome::Acquired { grant, .. } => grant,
        other => panic!("unexpected lease outcome: {other:?}"),
    };

    let s = &fixture.target;
    let r = &fixture.runtime;
    let api = format!("p21_api_{}", Uuid::new_v4().simple());
    sql(
        fixture,
        &format!(
            "CREATE ROLE {api} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT; \
             GRANT USAGE,CREATE ON SCHEMA {s} TO {api}; \
             GRANT SELECT ON {s}.ingress_protocol_epoch,{s}.sessions,{s}.pending_delivery TO {api}; \
             GRANT UPDATE(id) ON {s}.ingress_protocol_epoch,{s}.sessions TO {api}; \
             GRANT SELECT ON {s}.ingress_epoch_guard_manifest TO {r}; \
             GRANT SELECT ON {s}.singleton_candidate_slot,{s}.singleton_candidate_leases TO {api}; \
             GRANT UPDATE(slot) ON {s}.singleton_candidate_slot TO {api}"
        ),
    )
    .await;
    for table in EPOCH_GUARDED_TABLES {
        sql(
            fixture,
            &format!(
                "REVOKE ALL ON {s}.{table} FROM {r}; \
                 GRANT SELECT,INSERT,UPDATE,DELETE ON {s}.{table} TO {api}"
            ),
        )
        .await;
    }
    let ingress = INGRESS_API
        .replace("__SCHEMA_PATH__", &format!("\"{s}\""))
        .replace("__SCHEMA__", &format!("\"{s}\""))
        .replace("__API_OWNER_LITERAL__", &api)
        .replace("__API_OWNER_IDENT__", &api)
        .replace("__RUNTIME_ROLE_LITERAL__", r)
        .replace("__RUNTIME_ROLE_IDENT__", r);
    sql(fixture, &ingress).await;

    let message = MessageKey::new();
    let mut seed = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("seed as runtime");
    let mut tx = seed.begin().await.expect("seed transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}.ingress_protocol_epoch IN ACCESS SHARE MODE"
    ))
    .execute(&mut *tx)
    .await
    .expect("seed epoch lock");
    let inserted: bool = sqlx::query_scalar(&format!(
        "SELECT {s}.waddle_ingress_insert_message($1,1, \
         pg_catalog.decode(pg_catalog.repeat('5a',32),'hex'),NULL::smallint,NULL::bytea, \
         NULL::text,NULL::uuid,NULL::numeric,NULL::numeric,NULL::uuid,NULL::numeric)"
    ))
    .bind(message.to_storage())
    .fetch_one(&mut *tx)
    .await
    .expect("seed canonical message");
    assert!(inserted);
    let mut operations = Vec::new();
    for index in 1..=9 {
        let operation = ReceiptOperation {
            operation_id: Uuid::new_v4(),
            message,
            kind: EffectReceiptKind::from_storage(index),
            semantic_identity_hash: [u8::try_from(index).expect("small index"); 32],
        };
        let inserted: bool = sqlx::query_scalar(&format!(
            "SELECT {s}.waddle_ingress_insert_effect_intent($1,$2::integer::numeric,$3,$4,$5)"
        ))
        .bind(message.to_storage())
        .bind(index)
        .bind(operation.kind.to_storage())
        .bind(operation.semantic_identity_hash.as_slice())
        .bind([1u8].as_slice())
        .fetch_one(&mut *tx)
        .await
        .expect("seed effect intent");
        assert!(inserted);
        operations.push(operation);
    }
    tx.commit().await.expect("commit seed");
    drop(seed);

    let journal_function = format!("{s}.waddle_p21_record_effect_receipt(uuid,uuid,integer,bytea)");
    let lookup = LOOKUP
        .replace("__SCHEMA__", &format!("\"{s}\""))
        .replace("__SCHEMA_LITERAL__", &format!("'{s}'"))
        .replace("__RUNTIME_ROLE__", r);
    let guarded = GUARDED_RECEIPT
        .replace("__SCHEMA__", &format!("\"{s}\""))
        .replace("__SCHEMA_PATH__", &format!("\"{s}\""))
        .replace("__API_OWNER_IDENT__", &api)
        .replace("__RUNTIME_ROLE_IDENT__", r);
    sql(
        fixture,
        &format!(
            "ALTER FUNCTION {journal_function} OWNER TO {api}; \
             ALTER FUNCTION {journal_function} SECURITY DEFINER; \
             GRANT SELECT,INSERT ON {s}.p21_receipt_operation_results TO {api}; \
             REVOKE ALL ON FUNCTION {s}.waddle_ingress_record_effect_receipt(uuid,integer,bytea) FROM PUBLIC,{r}; \
             REVOKE ALL ON FUNCTION {s}.waddle_ingress_record_policy_discard(uuid,integer,bytea,text) FROM PUBLIC,{r}; \
             SET ROLE {api}; {lookup}; RESET ROLE; \
             REVOKE CREATE ON SCHEMA {s} FROM {api}; {guarded}"
        ),
    )
    .await;

    let (config, attestor) = dark_attestor_fixture(fixture).await;
    sql(fixture, &format!("GRANT SELECT ON {s}._migrations TO {r}")).await;
    let schema_oid = fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("physical identity")
        .schema
        .oid;
    let runtime_oid = role(fixture, r).await;
    let epoch_oid = relation(fixture, "ingress_protocol_epoch").await;
    let ledger = DarkMigrationLedgerPins {
        schema_oid,
        schema_owner_role_oid: attestor.schema_owner_role_oid,
        runtime_role_oid: runtime_oid,
        ledger_relation_oid: relation(fixture, "_migrations").await,
        ledger_primary_index_oid: relation(fixture, "_migrations_pkey").await,
    };
    let pinned = PinnedRuntimeAuthority::new(
        &config,
        attestor,
        ledger,
        epoch_oid,
        relation(fixture, "ingress_protocol_epoch_pkey").await,
        relation(fixture, "ingress_epoch_guard_manifest").await,
    )
    .expect("independently captured pinned pool identities");
    let pins = GuardedReceiptOperationPins {
        receipt: ReceiptOperationPins {
            schema_oid,
            schema_owner_role_oid: attestor.schema_owner_role_oid,
            runtime_role_oid: runtime_oid,
            api_owner_role_oid: role(fixture, &api).await,
            prepare_function_oid: function(fixture, "waddle_ingress_definer_prepare()").await,
            epoch_relation_oid: epoch_oid,
            journal_relation_oid: relation(fixture, "p21_receipt_operation_results").await,
            effect_receipt_function_oid: function(
                fixture,
                "waddle_ingress_record_effect_receipt(uuid,integer,bytea)",
            )
            .await,
            policy_discard_function_oid: function(
                fixture,
                "waddle_ingress_record_policy_discard(uuid,integer,bytea,text)",
            )
            .await,
            journal_function_oid: function(
                fixture,
                "waddle_p21_record_effect_receipt(uuid,uuid,integer,bytea)",
            )
            .await,
            journal_lookup_function_oid: function(
                fixture,
                "waddle_p21_lookup_receipt_operation(uuid)",
            )
            .await,
        },
        singleton_slot_relation_oid: relation(fixture, "singleton_candidate_slot").await,
        singleton_slot_primary_index_oid: relation(fixture, "singleton_candidate_slot_pkey").await,
        singleton_lease_relation_oid: relation(fixture, "singleton_candidate_leases").await,
        singleton_lease_primary_index_oid: relation(fixture, "singleton_candidate_leases_pkey")
            .await,
        singleton_lease_generation_index_oid: relation(
            fixture,
            "singleton_candidate_leases_generation_key",
        )
        .await,
        singleton_lease_identity_index_oid: relation(
            fixture,
            "singleton_candidate_leases_node_id_incarnation_generation_key",
        )
        .await,
        guarded_receipt_function_oid: function(
            fixture,
            "waddle_p21_guarded_record_effect_receipt(text,uuid,bigint,uuid,uuid,integer,bytea)",
        )
        .await,
    };
    let pool = pinned_attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        pinned.clone(),
        2,
        Duration::from_secs(3),
    )
    .expect("pinned pool");
    let authority = PinnedReceiptAuthority::new(pool, PgRoleName::new(&api).expect("API"), pins)
        .expect("matching independent pins");
    (
        authority, leases, grant, operations, api, config, pinned, pins,
    )
}

#[tokio::test]
async fn pinned_receipt_holds_fences_and_recovers_only_positive_results() {
    let _guard = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (authority, leases, grant, operations, api, config, pinned, pins) = setup(&fixture).await;

    for wrong in [
        GuardedReceiptOperationPins {
            receipt: ReceiptOperationPins {
                runtime_role_oid: pins.receipt.api_owner_role_oid,
                ..pins.receipt
            },
            ..pins
        },
        GuardedReceiptOperationPins {
            singleton_slot_primary_index_oid: pins.receipt.journal_relation_oid,
            ..pins
        },
        GuardedReceiptOperationPins {
            guarded_receipt_function_oid: pins.receipt.journal_lookup_function_oid,
            ..pins
        },
    ] {
        let pool = pinned_attested_pool(
            fixture.runtime_options.clone(),
            config.clone(),
            pinned.clone(),
            1,
            Duration::from_secs(3),
        )
        .expect("independent pool");
        assert!(matches!(
            PinnedReceiptAuthority::new(pool, PgRoleName::new(&api).expect("API"), wrong),
            Err(PinnedReceiptError::Binding)
        ));
    }
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("raw runtime login");
    for query in [
        format!(
            "SELECT * FROM {}.p21_receipt_operation_results",
            fixture.target
        ),
        format!(
            "SELECT {}.waddle_p21_record_effect_receipt(NULL::uuid,NULL::uuid,NULL::integer,NULL::bytea)",
            fixture.target
        ),
    ] {
        let error = sqlx::query(&query)
            .execute(&mut runtime)
            .await
            .expect_err("runtime must not bypass the guarded journal path");
        assert_eq!(
            error.as_database_error().and_then(|database| database.code()).as_deref(),
            Some("42501"),
            "the database must deny access rather than reject invalid arguments"
        );
    }
    drop(runtime);

    let trigger = format!("{}.waddle_ingress_protocol_epoch_guard()", fixture.target);
    let (original_source, compression, physical, logical): (String, Option<String>, i32, i32) =
        sqlx::query_as(
            "SELECT prosrc,pg_catalog.pg_column_compression(prosrc)::text, \
                    pg_catalog.pg_column_size(prosrc),pg_catalog.octet_length(prosrc) \
             FROM pg_catalog.pg_proc WHERE oid=$1::pg_catalog.regprocedure",
        )
        .bind(&trigger)
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect legitimate trigger source");
    assert_eq!(compression.as_deref(), Some("pglz"));
    assert!(physical <= 2048 && logical <= 8192);
    assert_eq!(
        authority.readback(&operations[0]).await.expect("missing"),
        ReceiptReadback::Indeterminate
    );
    // Simulate hostile catalog contents only in the disposable fixture. A
    // compact compressed source may pass the physical bound but must fail
    // the separate logical bound before any later catalog consumer uses it.
    sqlx::query(
        "UPDATE pg_catalog.pg_proc SET prosrc=pg_catalog.repeat('x',10000) \
         WHERE oid=$1::pg_catalog.regprocedure",
    )
    .bind(&trigger)
    .execute(&fixture.admin)
    .await
    .expect("install oversized logical trigger source");
    let (compression, physical, logical): (Option<String>, i32, i32) = sqlx::query_as(
        "SELECT pg_catalog.pg_column_compression(prosrc)::text, \
                pg_catalog.pg_column_size(prosrc),pg_catalog.octet_length(prosrc) \
         FROM pg_catalog.pg_proc WHERE oid=$1::pg_catalog.regprocedure",
    )
    .bind(&trigger)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect oversized logical trigger source");
    assert_eq!(compression.as_deref(), Some("pglz"));
    assert!(physical <= 2048 && logical > 8192);
    assert!(matches!(
        authority.readback(&operations[0]).await,
        Err(PinnedReceiptError::Catalog(PinnedCatalogError::Preflight))
    ));
    sqlx::query(
        "UPDATE pg_catalog.pg_proc SET prosrc=\
         (SELECT pg_catalog.string_agg(pg_catalog.md5(n::text),'') || pg_catalog.repeat('abc',10000) \
          FROM pg_catalog.generate_series(1,300) n) WHERE oid=$1::pg_catalog.regprocedure",
    )
    .bind(&trigger)
    .execute(&fixture.admin)
    .await
    .expect("install oversized physical trigger source");
    let (compression, physical): (Option<String>, i32) = sqlx::query_as(
        "SELECT pg_catalog.pg_column_compression(prosrc)::text, \
                pg_catalog.pg_column_size(prosrc) \
         FROM pg_catalog.pg_proc WHERE oid=$1::pg_catalog.regprocedure",
    )
    .bind(&trigger)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect oversized physical trigger source");
    assert_eq!(compression.as_deref(), Some("pglz"));
    assert!(physical > 2048);
    assert!(matches!(
        authority.readback(&operations[0]).await,
        Err(PinnedReceiptError::Catalog(PinnedCatalogError::Preflight))
    ));
    sqlx::query("UPDATE pg_catalog.pg_proc SET prosrc=$1 WHERE oid=$2::pg_catalog.regprocedure")
        .bind(original_source)
        .bind(&trigger)
        .execute(&fixture.admin)
        .await
        .expect("restore trigger source");
    assert_eq!(
        authority
            .readback(&operations[0])
            .await
            .expect("restored profile"),
        ReceiptReadback::Indeterminate
    );
    let expected = PinnedReceiptCommit::Committed {
        operation_id: operations[0].operation_id,
        inserted: true,
    };
    assert_eq!(
        authority
            .record(&grant, &operations[0])
            .await
            .expect("record"),
        expected
    );
    assert_eq!(
        authority
            .record(&grant, &operations[0])
            .await
            .expect("replay"),
        expected
    );
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (resume, released) = tokio::sync::oneshot::channel();
    let mut read = Box::pin(authority.readback_paused_for_test(&operations[0], ready, released));
    let (before, after) = tokio::select! {
        result = &mut read => panic!("readback ended before pause: {result:?}"),
        observation = waiting => observation.expect("paused readback"),
    };
    assert_same_transaction(&fixture, before, after).await;
    resume.send(()).expect("release readback");
    assert_eq!(
        read.await.expect("positive readback"),
        ReceiptReadback::Committed { inserted: true }
    );
    let duplicate = ReceiptOperation {
        operation_id: Uuid::new_v4(),
        ..operations[0]
    };
    assert!(matches!(
        authority
            .record(&grant, &duplicate)
            .await
            .expect("existing receipt"),
        PinnedReceiptCommit::Committed {
            inserted: false,
            ..
        }
    ));
    assert_eq!(
        authority
            .readback(&duplicate)
            .await
            .expect("duplicate readback"),
        ReceiptReadback::Committed { inserted: false }
    );
    let conflict = ReceiptOperation {
        semantic_identity_hash: [99; 32],
        ..operations[0]
    };
    assert!(matches!(
        authority.readback(&conflict).await,
        Err(PinnedReceiptError::OperationConflict)
    ));
    assert!(matches!(
        authority.record(&grant, &conflict).await,
        Err(PinnedReceiptError::OperationConflict)
    ));

    // Pause after the guarded function, while the same transaction still owns
    // its slot, epoch, ledger and catalog barriers and has not committed.
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (resume, released) = tokio::sync::oneshot::channel();
    let mut write = Box::pin(authority.record_paused_for_test(
        &grant,
        &operations[1],
        Duration::from_secs(15),
        ready,
        released,
    ));
    let (before, after) = tokio::select! {
        result = &mut write => panic!("write ended before pause: {result:?}"),
        observation = waiting => observation.expect("paused backend"),
    };
    assert_same_transaction(&fixture, before, after).await;
    let pid = after.pid;
    let protected: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 \
         AND locktype='advisory' AND classid=(($2::bigint >> 32) & 4294967295)::oid \
         AND objid=($2::bigint & 4294967295)::oid AND objsubid=1 \
         AND mode='ShareLock' AND granted) \
         AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND relation=$3 \
         AND mode='RowShareLock' AND granted)",
    )
    .bind(pid)
    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
    .bind(Oid(relation(&fixture, "singleton_candidate_slot").await))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect held same-backend fences");
    assert!(protected);
    let mut mutator = fixture.admin.begin().await.expect("catalog mutator");
    let mutator_acquired: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&mut *mutator)
            .await
            .expect("try exclusive catalog fence");
    assert!(
        !mutator_acquired,
        "the receipt transaction excludes mutators"
    );
    mutator
        .rollback()
        .await
        .expect("end failed mutator attempt");
    assert_eq!(
        authority
            .readback(&operations[1])
            .await
            .expect("uncommitted readback"),
        ReceiptReadback::Indeterminate
    );
    resume.send(()).expect("release write");
    assert!(matches!(
        write.await.expect("commit"),
        PinnedReceiptCommit::Committed { inserted: true, .. }
    ));
    assert_eq!(
        authority
            .readback(&operations[1])
            .await
            .expect("committed readback"),
        ReceiptReadback::Committed { inserted: true }
    );

    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (_resume, held) = tokio::sync::oneshot::channel();
    let mut cancelled = Box::pin(authority.record_paused_for_test(
        &grant,
        &operations[2],
        Duration::from_secs(15),
        ready,
        held,
    ));
    let (_, after) = tokio::select! {
        result = &mut cancelled => panic!("write ended before cancellation: {result:?}"),
        observation = waiting => observation.expect("cancelled backend"),
    };
    let pid = after.pid;
    drop(cancelled);
    wait_backend_terminated(&fixture, pid).await;
    assert_eq!(
        authority
            .readback(&operations[2])
            .await
            .expect("cancelled readback"),
        ReceiptReadback::Indeterminate
    );

    // A timeout returns the observed backend incarnation when preparation
    // reached it, but the absence of a row is still not rollback evidence.
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (_resume, held) = tokio::sync::oneshot::channel();
    let mut timed_out = Box::pin(authority.record_paused_for_test(
        &grant,
        &operations[4],
        Duration::from_secs(5),
        ready,
        held,
    ));
    let (_, after) = tokio::select! {
        result = &mut timed_out => panic!("write ended before timeout pause: {result:?}"),
        observation = waiting => observation.expect("timed-out backend"),
    };
    let pid = after.pid;
    assert!(matches!(
        timed_out.await.expect("unknown timeout"),
        PinnedReceiptCommit::Indeterminate {
            backend_pid: Some(observed_pid), backend: Some(backend), ..
        } if observed_pid == pid && backend.pid == pid && backend.transaction_id > 0
    ));
    wait_backend_terminated(&fixture, pid).await;
    assert_eq!(
        authority
            .readback(&operations[4])
            .await
            .expect("timeout readback"),
        ReceiptReadback::Indeterminate
    );

    // Let PostgreSQL commit, hide its reply, then prove the exact operation
    // independently. A failed client acknowledgement stays indeterminate.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let (held, waiting) = tokio::sync::oneshot::channel();
    let (disconnect, released) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(lose_commit_reply(
        listener,
        fixture.admin_options.clone(),
        held,
        released,
    ));
    let pool = pinned_attested_pool(
        proxy_runtime_options(&fixture, port),
        config,
        pinned,
        1,
        Duration::from_secs(3),
    )
    .expect("proxied pinned pool");
    let proxied = PinnedReceiptAuthority::new(pool, PgRoleName::new(&api).expect("API"), pins)
        .expect("proxied receipt authority");
    let lost = operations[5];
    let proxy_grant = grant.clone();
    let write = tokio::spawn(async move { proxied.record(&proxy_grant, &lost).await });
    tokio::time::timeout(Duration::from_secs(15), waiting)
        .await
        .expect("COMMIT reached server")
        .expect("COMMIT reply held");
    assert_eq!(
        authority
            .readback(&lost)
            .await
            .expect("positive journal evidence"),
        ReceiptReadback::Committed { inserted: true }
    );
    disconnect.send(()).expect("discard COMMIT reply");
    assert!(matches!(
        write.await.expect("proxy task").expect("unknown COMMIT"),
        PinnedReceiptCommit::Indeterminate {
            operation_id, backend_pid: Some(pid), backend: Some(backend)
        } if operation_id == lost.operation_id && pid == backend.pid
    ));
    proxy.await.expect("proxy task").expect("proxy completed");

    sql(
        &fixture,
        &format!(
            "GRANT SELECT ON {}.p21_receipt_operation_results TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    assert!(
        authority.record(&grant, &operations[3]).await.is_err(),
        "journal ACL drift must refuse the write"
    );
    sql(
        &fixture,
        &format!(
            "REVOKE SELECT ON {}.p21_receipt_operation_results FROM {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    assert_eq!(
        authority
            .readback(&operations[3])
            .await
            .expect("no receipt after rejected profile"),
        ReceiptReadback::Indeterminate
    );

    // The admitted writer wins first: expiration does not allow a successor
    // to replace its slot until the receipt transaction commits.
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (resume, released) = tokio::sync::oneshot::channel();
    let mut write = Box::pin(authority.record_paused_for_test(
        &grant,
        &operations[6],
        Duration::from_secs(15),
        ready,
        released,
    ));
    let (before, after) = tokio::select! {
        result = &mut write => panic!("receipt ended before takeover race: {result:?}"),
        observation = waiting => observation.expect("admitted receipt"),
    };
    assert_same_transaction(&fixture, before, after).await;
    expire_lease(&fixture, &grant).await;
    let successor_identity =
        ProcessIdentity::new("pinned-receipt-successor", Uuid::new_v4()).expect("identity");
    let contender = leases.clone();
    let contender_identity = successor_identity.clone();
    let pending = tokio::spawn(async move {
        contender
            .register(Uuid::new_v4(), contender_identity, Duration::from_secs(300))
            .await
    });
    wait_for_blocked_registration(&fixture, after.pid, "singleton_candidate_slot").await;
    assert!(
        !pending.is_finished(),
        "successor waits for the admitted receipt"
    );
    resume.send(()).expect("finish admitted receipt");
    assert!(matches!(
        write.await.expect("receipt commit"),
        PinnedReceiptCommit::Committed { inserted: true, .. }
    ));
    let successor = match pending.await.expect("join successor").expect("takeover") {
        LeaseOutcome::Acquired { grant, .. } => grant,
        other => panic!("unexpected takeover result: {other:?}"),
    };
    assert_eq!(successor.identity(), &successor_identity);
    assert_eq!(successor.generation(), grant.generation() + 1);
    assert_eq!(
        authority
            .readback(&operations[6])
            .await
            .expect("receipt readback"),
        ReceiptReadback::Committed { inserted: true }
    );

    // The successor wins first: hold the old lease row to pause registration
    // after it acquires the slot. The old receipt's NOWAIT guard must fail;
    // it cannot proceed under the slot that the contender is replacing.
    expire_lease(&fixture, &successor).await;
    let mut lease_blocker = fixture.admin.begin().await.expect("hold lease row");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *lease_blocker)
        .await
        .expect("lease blocker PID");
    sqlx::query(&format!(
        "SELECT incarnation FROM {}.singleton_candidate_leases WHERE incarnation=$1 FOR UPDATE",
        fixture.target
    ))
    .bind(successor.identity().incarnation())
    .fetch_one(&mut *lease_blocker)
    .await
    .expect("lock exact old lease");
    let final_identity =
        ProcessIdentity::new("pinned-receipt-final", Uuid::new_v4()).expect("identity");
    let contender = leases.clone();
    let contender_identity = final_identity.clone();
    let pending = tokio::spawn(async move {
        contender
            .register(Uuid::new_v4(), contender_identity, Duration::from_secs(300))
            .await
    });
    wait_for_blocked_registration(&fixture, blocker_pid, "singleton_candidate_leases").await;
    let blocked = authority
        .record(&successor, &operations[7])
        .await
        .expect_err("old receipt must fail while successor owns the slot");
    assert!(
        matches!(&blocked, PinnedReceiptError::Postgres(sqlx::Error::Database(database))
            if database.code().as_deref() == Some("55P03")),
        "the guarded function must fail NOWAIT on the slot: {blocked:?}"
    );
    lease_blocker.rollback().await.expect("release lease row");
    let final_grant = match pending.await.expect("join successor").expect("takeover") {
        LeaseOutcome::Acquired { grant, .. } => grant,
        other => panic!("unexpected takeover result: {other:?}"),
    };
    assert_eq!(final_grant.identity(), &final_identity);
    assert_eq!(final_grant.generation(), successor.generation() + 1);
    assert_eq!(
        authority
            .readback(&operations[7])
            .await
            .expect("rejected receipt readback"),
        ReceiptReadback::Indeterminate
    );
    let stale = authority
        .record(&successor, &operations[7])
        .await
        .expect_err("replaced singleton grant must not write");
    assert!(
        matches!(&stale, PinnedReceiptError::Postgres(sqlx::Error::Database(database))
            if database.code().as_deref() == Some("55000")),
        "the guarded function must reject the replaced grant: {stale:?}"
    );
    assert!(matches!(
        authority
            .record(&final_grant, &operations[8])
            .await
            .expect("new owner receipt"),
        PinnedReceiptCommit::Committed { inserted: true, .. }
    ));

    assert!(matches!(
        leases
            .revoke(Uuid::new_v4(), final_grant.clone())
            .await
            .expect("revoke"),
        LeaseOutcome::Revoked
    ));
    let stale = authority
        .record(&final_grant, &operations[3])
        .await
        .expect_err("stale singleton grant must not write");
    assert!(
        matches!(&stale, PinnedReceiptError::Postgres(sqlx::Error::Database(database))
            if database.code().as_deref() == Some("55000")),
        "the guarded function must reject the stale grant: {stale:?}"
    );

    drop(authority);
    drop(leases);
    sql(&fixture, &format!("DROP OWNED BY {api}; DROP ROLE {api}")).await;
    fixture.cleanup().await;
}

async fn lose_commit_reply(
    listener: tokio::net::TcpListener,
    upstream: sqlx::postgres::PgConnectOptions,
    held: tokio::sync::oneshot::Sender<()>,
    disconnect: tokio::sync::oneshot::Receiver<()>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (client, _) = listener.accept().await?;
    let server = crate::postgres_proxy::connect_local_postgres(&upstream).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let forward = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
    });
    loop {
        let tag = server_read.read_u8().await?;
        let length = server_read.read_u32().await?;
        if !(4..=1_048_576).contains(&length) {
            return Err(std::io::Error::other("invalid backend frame"));
        }
        let mut body = vec![0; (length - 4) as usize];
        server_read.read_exact(&mut body).await?;
        if tag == b'C' && body == b"COMMIT\0" {
            held.send(())
                .map_err(|_| std::io::Error::other("holder dropped"))?;
            disconnect
                .await
                .map_err(|_| std::io::Error::other("disconnect dropped"))?;
            break;
        }
        client_write.write_u8(tag).await?;
        client_write.write_u32(length).await?;
        client_write.write_all(&body).await?;
    }
    forward.abort();
    let _ = forward.await;
    client_write.shutdown().await
}
