//! PG17 operation journal: role boundary, atomic result and catalog drift.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::{Connection, PgConnection};
use tokio::sync::oneshot;
use uuid::Uuid;
use waddle_server::db::physical_attestation::{
    attested_pool, PgRoleName, PhysicalAttestationConfig, ReceiptAuthorityError, ReceiptCommit,
    ReceiptOperation, ReceiptOperationAuthority, ReceiptOperationPins, ReceiptReadback,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::test_support::database_from_attested_postgres_pool;

use super::super::{proxy_runtime_options, Fixture};
use super::receipt_transaction;

pub(super) async fn exercise(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    epoch_oid: u32,
) {
    let receipts = receipt_transaction::seed(fixture, 6).await;
    let s = &fixture.target;
    let r = &fixture.runtime;
    let api_name = api.as_str();
    let journal = format!("{s}.p21_receipt_operation_results");
    let function = format!("{s}.waddle_p21_record_effect_receipt(uuid,uuid,integer,bytea)");

    // Reapply V1030 through the registered runner with hostile default ACLs.
    // This is a disposable, exclusively owned schema with an empty journal.
    super::execute(&fixture.admin, &format!(
        "DROP FUNCTION {function}; DROP TABLE {journal}; DELETE FROM {s}._migrations WHERE version=1030; \
         ALTER DEFAULT PRIVILEGES IN SCHEMA {s} GRANT ALL ON TABLES TO {r}; \
         ALTER DEFAULT PRIVILEGES IN SCHEMA {s} GRANT EXECUTE ON FUNCTIONS TO {r}"
    )).await;
    let owner = Database::from_config(
        "receipt migration owner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            super::super::url_with_schema(&fixture.base_url, s),
        ),
    )
    .await
    .expect("owner database");
    assert_eq!(
        MigrationRunner::single()
            .run(&owner)
            .await
            .expect("registered V1030"),
        vec![1030]
    );
    drop(owner);
    super::execute(
        &fixture.admin,
        &format!(
            "ALTER DEFAULT PRIVILEGES IN SCHEMA {s} REVOKE ALL ON TABLES FROM {r}; \
         ALTER DEFAULT PRIVILEGES IN SCHEMA {s} REVOKE EXECUTE ON FUNCTIONS FROM {r}"
        ),
    )
    .await;
    let strict_before = attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        1,
        Duration::from_secs(2),
    )
    .expect("strict profile pool");
    strict_before
        .inspect_restricted_ingress_profile(api, epoch_oid)
        .await
        .expect("V1030 preserves strict v2 before provisioning");
    strict_before.close().await;

    // The operator must fence/drain old writers and check an empty, owner-only
    // journal before provisioning. This fixture is exclusively owned.
    let empty: bool = sqlx::query_scalar(&format!("SELECT NOT EXISTS(SELECT 1 FROM {journal})"))
        .fetch_one(&fixture.admin)
        .await
        .expect("empty journal");
    assert!(empty);
    let before: (bool, bool, bool) = sqlx::query_as(
        "SELECT pg_catalog.has_table_privilege($1,$2,'SELECT,INSERT,UPDATE,DELETE'), \
         pg_catalog.has_function_privilege($1,$3,'EXECUTE'), \
         (SELECT prosecdef FROM pg_catalog.pg_proc WHERE oid=$3::pg_catalog.regprocedure)",
    )
    .bind(r)
    .bind(&journal)
    .bind(&function)
    .fetch_one(&fixture.admin)
    .await
    .expect("inert ACL");
    assert_eq!(before, (false, false, false));
    super::execute(
        &fixture.admin,
        &format!(
            "GRANT CREATE ON SCHEMA {s} TO {api_name}; \
         ALTER FUNCTION {function} OWNER TO {api_name}; \
         ALTER FUNCTION {function} SECURITY DEFINER; \
         REVOKE CREATE ON SCHEMA {s} FROM {api_name}; \
         GRANT SELECT,INSERT ON {journal} TO {api_name}; \
         GRANT SELECT ON {journal} TO {r}; \
         GRANT EXECUTE ON FUNCTION {function} TO {r}"
        ),
    )
    .await;
    let schema_oid = config_identity_schema_oid(fixture);
    let schema_owner: Oid =
        sqlx::query_scalar("SELECT nspowner FROM pg_catalog.pg_namespace WHERE oid=$1")
            .bind(Oid(schema_oid))
            .fetch_one(&fixture.admin)
            .await
            .expect("schema owner");
    let role_oid = |name: String| async move {
        sqlx::query_scalar::<_, Oid>("SELECT oid FROM pg_catalog.pg_roles WHERE rolname=$1")
            .bind(name)
            .fetch_one(&fixture.admin)
            .await
            .expect("role OID")
            .0
    };
    let journal_oid: Oid =
        sqlx::query_scalar("SELECT oid FROM pg_catalog.pg_class WHERE oid=$1::pg_catalog.regclass")
            .bind(&journal)
            .fetch_one(&fixture.admin)
            .await
            .expect("journal OID");
    let function_oid: Oid = sqlx::query_scalar(
        "SELECT oid FROM pg_catalog.pg_proc WHERE oid=$1::pg_catalog.regprocedure",
    )
    .bind(&function)
    .fetch_one(&fixture.admin)
    .await
    .expect("function OID");
    let pins = ReceiptOperationPins {
        schema_oid,
        schema_owner_role_oid: schema_owner.0,
        runtime_role_oid: role_oid(r.to_owned()).await,
        api_owner_role_oid: role_oid(api_name.to_owned()).await,
        epoch_relation_oid: epoch_oid,
        journal_relation_oid: journal_oid.0,
        journal_function_oid: function_oid.0,
    };
    let db = receipt_transaction::database(fixture, config.clone(), "receipt-journal");
    let authority = ReceiptOperationAuthority::postgres(db.clone(), api.clone(), pins)
        .expect("bound authority");
    let operation = ReceiptOperation {
        operation_id: Uuid::new_v4(),
        message: receipts[0].message,
        kind: receipts[0].kind,
        semantic_identity_hash: receipts[0].semantic_identity_hash,
    };
    assert_eq!(
        authority.readback(&operation).await.expect("absence"),
        ReceiptReadback::Indeterminate
    );
    assert_eq!(
        authority.record(&operation).await.expect("record"),
        ReceiptCommit::Committed {
            operation_id: operation.operation_id,
            inserted: true
        }
    );
    assert_eq!(
        authority.record(&operation).await.expect("replay"),
        ReceiptCommit::Committed {
            operation_id: operation.operation_id,
            inserted: true
        }
    );
    assert_eq!(
        authority.readback(&operation).await.expect("readback"),
        ReceiptReadback::Committed { inserted: true }
    );
    let conflicting = ReceiptOperation {
        kind: receipts[1].kind,
        semantic_identity_hash: receipts[1].semantic_identity_hash,
        ..operation
    };
    assert!(matches!(
        authority.record(&conflicting).await,
        Err(ReceiptAuthorityError::OperationConflict)
    ));
    assert!(matches!(
        authority.readback(&conflicting).await,
        Err(ReceiptAuthorityError::OperationConflict)
    ));

    let same = ReceiptOperation {
        operation_id: Uuid::new_v4(),
        kind: receipts[2].kind,
        semantic_identity_hash: receipts[2].semantic_identity_hash,
        ..operation
    };
    let (first, second) = tokio::join!(authority.record(&same), authority.record(&same));
    let expected = ReceiptCommit::Committed {
        operation_id: same.operation_id,
        inserted: true,
    };
    assert_eq!(first.expect("first concurrent duplicate"), expected);
    assert_eq!(second.expect("second concurrent duplicate"), expected);

    let concurrent_id = Uuid::new_v4();
    let one = ReceiptOperation {
        operation_id: concurrent_id,
        kind: receipts[3].kind,
        semantic_identity_hash: receipts[3].semantic_identity_hash,
        ..operation
    };
    let two = ReceiptOperation {
        kind: receipts[4].kind,
        semantic_identity_hash: receipts[4].semantic_identity_hash,
        ..one
    };
    let (one_result, two_result) = tokio::join!(authority.record(&one), authority.record(&two));
    let one_won = matches!(
        one_result,
        Ok(ReceiptCommit::Committed { inserted: true, .. })
    );
    let two_won = matches!(
        two_result,
        Ok(ReceiptCommit::Committed { inserted: true, .. })
    );
    assert_ne!(one_won, two_won, "one conflicting tuple commits");
    if one_won {
        assert!(matches!(
            two_result,
            Err(ReceiptAuthorityError::OperationConflict)
        ));
        assert_eq!(
            authority.readback(&one).await.expect("winning identity"),
            ReceiptReadback::Committed { inserted: true }
        );
    } else {
        assert!(matches!(
            one_result,
            Err(ReceiptAuthorityError::OperationConflict)
        ));
        assert_eq!(
            authority.readback(&two).await.expect("winning identity"),
            ReceiptReadback::Committed { inserted: true }
        );
    }
    let counts: (i64, i64) = sqlx::query_as(&format!(
        "SELECT (SELECT count(*) FROM {s}.p21_receipt_operation_results WHERE operation_id=$1), \
         (SELECT count(*) FROM {s}.ingress_effect_receipts WHERE message_key=$2 AND kind IN ($3,$4))"
    )).bind(concurrent_id).bind(one.message.to_storage()).bind(one.kind.to_storage()).bind(two.kind.to_storage())
      .fetch_one(&fixture.admin).await.expect("atomic conflict state");
    assert_eq!(
        counts,
        (1, 1),
        "losing receipt rolls back with its mismatched journal attempt"
    );

    let mut direct = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime connection");
    assert!(
        sqlx::query(&format!("INSERT INTO {journal} VALUES ($1,$2,$3,$4,true)"))
            .bind(Uuid::new_v4())
            .bind(operation.message.to_storage())
            .bind(operation.kind.to_storage())
            .bind(operation.semantic_identity_hash.as_slice())
            .execute(&mut direct)
            .await
            .is_err()
    );
    drop(direct);
    for (label, change, restore) in [
        (
            "runtime insert",
            format!("GRANT INSERT ON {journal} TO {r}"),
            format!("REVOKE INSERT ON {journal} FROM {r}"),
        ),
        (
            "API update",
            format!("GRANT UPDATE ON {journal} TO {api_name}"),
            format!("REVOKE UPDATE ON {journal} FROM {api_name}"),
        ),
        (
            "function invoker",
            format!("ALTER FUNCTION {function} SECURITY INVOKER"),
            format!("ALTER FUNCTION {function} SECURITY DEFINER"),
        ),
        (
            "function search path",
            format!("ALTER FUNCTION {function} SET search_path=public"),
            format!("ALTER FUNCTION {function} SET search_path=pg_catalog,pg_temp"),
        ),
        (
            "extra index",
            format!("CREATE INDEX p21_journal_hostile ON {journal}(receipt_kind)"),
            format!("DROP INDEX {s}.p21_journal_hostile"),
        ),
    ] {
        super::execute(&fixture.admin, &change).await;
        let rejected = authority
            .record(&ReceiptOperation {
                operation_id: Uuid::new_v4(),
                ..operation
            })
            .await;
        super::execute(&fixture.admin, &restore).await;
        assert!(rejected.is_err(), "accepted hostile {label}: {rejected:?}");
        assert_eq!(
            authority
                .readback(&operation)
                .await
                .expect("restored profile"),
            ReceiptReadback::Committed { inserted: true }
        );
    }
    for wrong in [
        ReceiptOperationPins {
            schema_owner_role_oid: pins.schema_owner_role_oid + 1,
            ..pins
        },
        ReceiptOperationPins {
            runtime_role_oid: pins.runtime_role_oid + 1,
            ..pins
        },
        ReceiptOperationPins {
            api_owner_role_oid: pins.api_owner_role_oid + 1,
            ..pins
        },
        ReceiptOperationPins {
            epoch_relation_oid: pins.epoch_relation_oid + 1,
            ..pins
        },
        ReceiptOperationPins {
            journal_relation_oid: pins.journal_relation_oid + 1,
            ..pins
        },
        ReceiptOperationPins {
            journal_function_oid: pins.journal_function_oid + 1,
            ..pins
        },
    ] {
        let wrong = ReceiptOperationAuthority::postgres(db.clone(), api.clone(), wrong)
            .expect("syntactically valid pins");
        assert!(
            wrong.readback(&operation).await.is_err(),
            "unmatched independent OID must fail"
        );
    }
    let strict = attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        1,
        Duration::from_secs(2),
    )
    .expect("strict pool");
    assert!(
        strict
            .inspect_restricted_ingress_profile(api, epoch_oid)
            .await
            .is_err(),
        "strict v2 refuses separately provisioned extra API function"
    );
    strict.close().await;

    let lost = ReceiptOperation {
        operation_id: Uuid::new_v4(),
        kind: receipts[5].kind,
        semantic_identity_hash: receipts[5].semantic_identity_hash,
        ..operation
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy listener");
    let port = listener.local_addr().expect("proxy address").port();
    let (held, waiting) = oneshot::channel();
    let (release, disconnect) = oneshot::channel();
    let proxy = tokio::spawn(receipt_transaction::lose_receipt_commit_reply(
        listener,
        fixture.admin_options.clone(),
        held,
        disconnect,
    ));
    let options = proxy_runtime_options(fixture, port).application_name("p21-journal-lost-commit");
    let pool = attested_pool(options, config.clone(), 1, Duration::from_secs(2))
        .expect("proxied attested pool");
    let proxied_db = database_from_attested_postgres_pool("p21-journal-lost-commit", pool);
    let proxied = ReceiptOperationAuthority::postgres(proxied_db, api.clone(), pins)
        .expect("proxied authority");
    let task = tokio::spawn(async move { proxied.record(&lost).await });
    tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("COMMIT reached server")
        .expect("held reply");
    assert_eq!(
        authority
            .readback(&lost)
            .await
            .expect("positive journal evidence"),
        ReceiptReadback::Committed { inserted: true }
    );
    release.send(()).expect("discard acknowledgement");
    assert_eq!(
        task.await.expect("task").expect("indeterminate outcome"),
        ReceiptCommit::Indeterminate {
            operation_id: lost.operation_id
        }
    );
    proxy.await.expect("proxy task").expect("proxy finished");
}

fn config_identity_schema_oid(fixture: &Fixture) -> u32 {
    fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("PG identity")
        .schema
        .oid
}
