//! PG17 operation journal: role boundary, atomic result and catalog drift.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::{Connection, PgConnection};
use tokio::sync::oneshot;
use uuid::Uuid;
use waddle_server::db::physical_attestation::{
    attested_pool, PgRoleName, PhysicalAttestationConfig, ReceiptAuthorityError, ReceiptCommit,
    ReceiptOperation, ReceiptOperationAuthority, ReceiptOperationPins, ReceiptReadback,
    RestrictedIngressProfileError, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::test_support::database_from_attested_postgres_pool;

use super::super::{proxy_runtime_options, Fixture};
use super::receipt_transaction;

const LOOKUP_CANDIDATE: &str =
    include_str!("../../src/db/migrations/owner/p21_receipt_journal_lookup_candidate.sql");
const INGRESS_API_CANDIDATE: &str =
    include_str!("../../src/db/migrations/owner/ingress_definer_api_candidate.sql");
const RECEIPT_WRITER_CANDIDATE: &str =
    include_str!("../../src/db/migrations/owner/p21_receipt_journal_postgres.sql");

pub(super) async fn exercise(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    epoch_oid: u32,
) {
    let fence_call =
        format!("pg_try_advisory_xact_lock_shared({CATALOG_MUTATION_ADVISORY_LOCK_KEY})");
    assert!(INGRESS_API_CANDIDATE.contains(&fence_call));
    assert!(INGRESS_API_CANDIDATE.contains("waddle_ingress_definer_prepare()"));
    assert!(RECEIPT_WRITER_CANDIDATE.contains("waddle_ingress_definer_prepare()"));
    assert!(LOOKUP_CANDIDATE.contains("waddle_ingress_definer_prepare()"));
    let receipts = receipt_transaction::seed(fixture, 8).await;
    let s = &fixture.target;
    let r = &fixture.runtime;
    let api_name = api.as_str();
    let journal = format!("{s}.p21_receipt_operation_results");
    let function = format!("{s}.waddle_p21_record_effect_receipt(uuid,uuid,integer,bytea)");
    let effect_receipt_function =
        format!("{s}.waddle_ingress_record_effect_receipt(uuid,integer,bytea)");
    let policy_discard_function =
        format!("{s}.waddle_ingress_record_policy_discard(uuid,integer,bytea,text)");
    let lookup_function = format!("{s}.waddle_p21_lookup_receipt_operation(uuid)");
    let lookup_sql = LOOKUP_CANDIDATE
        .replace("__SCHEMA__", &format!("\"{s}\""))
        .replace("__SCHEMA_LITERAL__", &format!("'{s}'"))
        .replace("__RUNTIME_ROLE__", r);
    let lookup_create_or_replace = lookup_sql
        .split_once("\nREVOKE ALL")
        .expect("lookup candidate grants follow function definition")
        .0
        .replacen("CREATE FUNCTION", "CREATE OR REPLACE FUNCTION", 1);

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
            "GRANT USAGE ON SCHEMA {s} TO {}; \
             ALTER DEFAULT PRIVILEGES FOR ROLE {api_name} IN SCHEMA {s} \
                 GRANT EXECUTE ON FUNCTIONS TO {};",
            fixture.outsider, fixture.outsider
        ),
    )
    .await;
    let mut default_probe = fixture
        .admin
        .begin()
        .await
        .expect("begin default ACL probe");
    sqlx::query(&format!("GRANT CREATE ON SCHEMA {s} TO {api_name}"))
        .execute(&mut *default_probe)
        .await
        .expect("grant temporary probe create");
    sqlx::query(&format!("SET LOCAL ROLE {api_name}"))
        .execute(&mut *default_probe)
        .await
        .expect("assume API owner for default ACL probe");
    sqlx::query(&format!(
        "CREATE FUNCTION {s}.p21_default_acl_probe() RETURNS integer \
         LANGUAGE sql AS 'SELECT 7'"
    ))
    .execute(&mut *default_probe)
    .await
    .expect("create function with hostile named default ACL");
    sqlx::query(&format!(
        "REVOKE EXECUTE ON FUNCTION {s}.p21_default_acl_probe() FROM PUBLIC"
    ))
    .execute(&mut *default_probe)
    .await
    .expect("remove PUBLIC execute from named-default positive control");
    sqlx::query("RESET ROLE")
        .execute(&mut *default_probe)
        .await
        .expect("reset default ACL probe role");
    sqlx::query(&format!("REVOKE CREATE ON SCHEMA {s} FROM {api_name}"))
        .execute(&mut *default_probe)
        .await
        .expect("revoke temporary probe create");
    default_probe
        .commit()
        .await
        .expect("commit default ACL positive control");
    let mut outsider_probe =
        PgConnection::connect_with(&fixture.runtime_options.clone().username(&fixture.outsider))
            .await
            .expect("outsider default ACL positive control connection");
    let default_acl_value: i32 = sqlx::query_scalar(&format!("SELECT {s}.p21_default_acl_probe()"))
        .fetch_one(&mut outsider_probe)
        .await
        .expect("named default ACL is effective for outsider");
    assert_eq!(default_acl_value, 7);
    drop(outsider_probe);
    super::execute(
        &fixture.admin,
        &format!("DROP FUNCTION {s}.p21_default_acl_probe()"),
    )
    .await;

    let mut hostile_install = fixture.admin.begin().await.expect("begin hostile install");
    sqlx::query(&format!("GRANT CREATE ON SCHEMA {s} TO {api_name}"))
        .execute(&mut *hostile_install)
        .await
        .expect("grant temporary hostile install create");
    sqlx::query(&format!("SET LOCAL ROLE {api_name}"))
        .execute(&mut *hostile_install)
        .await
        .expect("assume API owner for hostile install");
    let hostile_install_result = sqlx::raw_sql(&lookup_sql)
        .execute(&mut *hostile_install)
        .await;
    assert!(
        hostile_install_result.is_err(),
        "lookup installation must reject named default EXECUTE grants"
    );
    hostile_install
        .rollback()
        .await
        .expect("rollback entire hostile lookup install");
    super::execute(
        &fixture.admin,
        &format!(
            "ALTER DEFAULT PRIVILEGES FOR ROLE {api_name} IN SCHEMA {s} \
             REVOKE EXECUTE ON FUNCTIONS FROM {};",
            fixture.outsider
        ),
    )
    .await;
    let lookup_exists: bool =
        sqlx::query_scalar("SELECT pg_catalog.to_regprocedure($1::text) IS NOT NULL")
            .bind(&lookup_function)
            .fetch_one(&fixture.admin)
            .await
            .expect("lookup rollback observation");
    assert!(
        !lookup_exists,
        "hostile install transaction must roll back the function"
    );

    let mut provision = fixture.admin.begin().await.expect("begin dark promotion");
    sqlx::query(&format!("GRANT CREATE ON SCHEMA {s} TO {api_name}"))
        .execute(&mut *provision)
        .await
        .expect("grant temporary API create");
    sqlx::query(&format!("ALTER FUNCTION {function} OWNER TO {api_name}"))
        .execute(&mut *provision)
        .await
        .expect("transfer record function");
    sqlx::query(&format!("ALTER FUNCTION {function} SECURITY DEFINER"))
        .execute(&mut *provision)
        .await
        .expect("harden record function");
    sqlx::query(&format!("GRANT SELECT,INSERT ON {journal} TO {api_name}"))
        .execute(&mut *provision)
        .await
        .expect("grant API journal access");
    sqlx::query(&format!(
        "REVOKE ALL ON FUNCTION {effect_receipt_function} FROM PUBLIC, {r}"
    ))
    .execute(&mut *provision)
    .await
    .expect("route low-level receipt only through fixed journal API");
    sqlx::query(&format!(
        "REVOKE ALL ON FUNCTION {policy_discard_function} FROM PUBLIC, {r}"
    ))
    .execute(&mut *provision)
    .await
    .expect("remove unjournaled policy-discard entry from P21 role profile");
    sqlx::query(&format!("SET LOCAL ROLE {api_name}"))
        .execute(&mut *provision)
        .await
        .expect("assume API owner");
    sqlx::query(&format!("GRANT EXECUTE ON FUNCTION {function} TO {r}"))
        .execute(&mut *provision)
        .await
        .expect("grant record API to runtime");
    sqlx::raw_sql(&lookup_sql)
        .execute(&mut *provision)
        .await
        .expect("install exact-key lookup under API owner");
    sqlx::query("RESET ROLE")
        .execute(&mut *provision)
        .await
        .expect("reset API role");
    sqlx::query(&format!("REVOKE CREATE ON SCHEMA {s} FROM {api_name}"))
        .execute(&mut *provision)
        .await
        .expect("revoke temporary API create");
    provision.commit().await.expect("commit dark promotion");
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
    let prepare_function_oid: Oid = sqlx::query_scalar(
        "SELECT oid FROM pg_catalog.pg_proc WHERE oid=$1::pg_catalog.regprocedure",
    )
    .bind(format!("{s}.waddle_ingress_definer_prepare()"))
    .fetch_one(&fixture.admin)
    .await
    .expect("common ingress helper OID");
    let effect_receipt_function_oid: Oid = sqlx::query_scalar(
        "SELECT oid FROM pg_catalog.pg_proc WHERE oid=$1::pg_catalog.regprocedure",
    )
    .bind(&effect_receipt_function)
    .fetch_one(&fixture.admin)
    .await
    .expect("low-level receipt function OID");
    let policy_discard_function_oid: Oid = sqlx::query_scalar(
        "SELECT oid FROM pg_catalog.pg_proc WHERE oid=$1::pg_catalog.regprocedure",
    )
    .bind(&policy_discard_function)
    .fetch_one(&fixture.admin)
    .await
    .expect("policy-discard function OID");
    let lookup_function_oid: Oid = sqlx::query_scalar(
        "SELECT oid FROM pg_catalog.pg_proc WHERE oid=$1::pg_catalog.regprocedure",
    )
    .bind(&lookup_function)
    .fetch_one(&fixture.admin)
    .await
    .expect("lookup function OID");
    let pins = ReceiptOperationPins {
        schema_oid,
        schema_owner_role_oid: schema_owner.0,
        runtime_role_oid: role_oid(r.to_owned()).await,
        api_owner_role_oid: role_oid(api_name.to_owned()).await,
        prepare_function_oid: prepare_function_oid.0,
        epoch_relation_oid: epoch_oid,
        journal_relation_oid: journal_oid.0,
        effect_receipt_function_oid: effect_receipt_function_oid.0,
        policy_discard_function_oid: policy_discard_function_oid.0,
        journal_function_oid: function_oid.0,
        journal_lookup_function_oid: lookup_function_oid.0,
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

    let unfenced_runtime_functions: Vec<String> = sqlx::query_scalar(
        "SELECT p.proname || '(' || pg_catalog.pg_get_function_identity_arguments(p.oid) || ')' \
         FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace \
         WHERE n.nspname=$1 AND p.prorettype <> 'pg_catalog.trigger'::pg_catalog.regtype \
           AND p.proname <> 'pubsub_runtime_attach_lock' \
           AND pg_catalog.has_function_privilege($2,p.oid,'EXECUTE') \
           AND pg_catalog.strpos(p.prosrc,'waddle_ingress_definer_prepare()')=0",
    )
    .bind(s)
    .bind(r)
    .fetch_all(&fixture.admin)
    .await
    .expect("runtime ingress API fence profile");
    assert!(
        unfenced_runtime_functions.is_empty(),
        "R-callable ingress functions bypass the common fence: {unfenced_runtime_functions:?}"
    );

    let fenced_operation = ReceiptOperation {
        operation_id: Uuid::new_v4(),
        message: receipts[6].message,
        kind: receipts[6].kind,
        semantic_identity_hash: receipts[6].semantic_identity_hash,
    };
    let mut exclusive = fixture.admin.begin().await.expect("catalog mutator tx");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *exclusive)
        .await
        .expect("mutator owns exclusive catalog fence");
    let mut blocked_writer = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime blocked-writer connection");
    sqlx::query("BEGIN ISOLATION LEVEL READ COMMITTED READ WRITE")
        .execute(&mut blocked_writer)
        .await
        .expect("begin blocked receipt transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}.ingress_protocol_epoch IN ACCESS SHARE MODE"
    ))
    .execute(&mut blocked_writer)
    .await
    .expect("epoch prelock before fixed API entry");
    let blocked_write = sqlx::query_scalar::<_, bool>(&format!(
        "SELECT {s}.waddle_p21_record_effect_receipt($1,$2,$3,$4)"
    ))
    .bind(fenced_operation.operation_id)
    .bind(fenced_operation.message.to_storage())
    .bind(fenced_operation.kind.to_storage())
    .bind(fenced_operation.semantic_identity_hash.as_slice())
    .fetch_one(&mut blocked_writer)
    .await;
    assert!(
        matches!(&blocked_write, Err(error) if error
            .as_database_error()
            .and_then(|database| database.code())
            .as_deref() == Some("55P03")),
        "direct writer call must fail closed under the exclusive catalog fence: {blocked_write:?}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut blocked_writer)
        .await
        .expect("rollback denied writer");
    drop(blocked_writer);

    let mut blocked_lookup = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime blocked-lookup connection");
    let lookup_under_fence = sqlx::query(&format!(
        "SELECT * FROM {s}.waddle_p21_lookup_receipt_operation($1)"
    ))
    .bind(operation.operation_id)
    .fetch_optional(&mut blocked_lookup)
    .await;
    assert!(
        matches!(&lookup_under_fence, Err(error) if error
            .as_database_error()
            .and_then(|database| database.code())
            .as_deref() == Some("55P03")),
        "direct lookup call must fail closed under the exclusive catalog fence: {lookup_under_fence:?}"
    );
    drop(blocked_lookup);
    exclusive.rollback().await.expect("release exclusive fence");

    // Reverse order: the direct R-callable writer gets the shared xact fence;
    // a mutator cannot acquire exclusive until the receipt transaction ends.
    let mut direct_writer = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime direct-writer connection");
    sqlx::query("BEGIN ISOLATION LEVEL READ COMMITTED READ WRITE")
        .execute(&mut direct_writer)
        .await
        .expect("begin direct receipt transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}.ingress_protocol_epoch IN ACCESS SHARE MODE"
    ))
    .execute(&mut direct_writer)
    .await
    .expect("direct writer epoch prelock");
    let direct_inserted: bool = sqlx::query_scalar(&format!(
        "SELECT {s}.waddle_p21_record_effect_receipt($1,$2,$3,$4)"
    ))
    .bind(fenced_operation.operation_id)
    .bind(fenced_operation.message.to_storage())
    .bind(fenced_operation.kind.to_storage())
    .bind(fenced_operation.semantic_identity_hash.as_slice())
    .fetch_one(&mut direct_writer)
    .await
    .expect("fixed API receipt enters shared fence");
    assert!(direct_inserted);
    let mut exclusive_probe = fixture.admin.begin().await.expect("mutator probe tx");
    let exclusive_acquired: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&mut *exclusive_probe)
            .await
            .expect("nonblocking exclusive probe");
    assert!(
        !exclusive_acquired,
        "mutator must wait for writer completion"
    );
    exclusive_probe
        .rollback()
        .await
        .expect("release mutator probe");
    sqlx::query("COMMIT")
        .execute(&mut direct_writer)
        .await
        .expect("commit fixed API receipt");
    drop(direct_writer);
    assert_eq!(
        authority
            .readback(&fenced_operation)
            .await
            .expect("same operation exact readback"),
        ReceiptReadback::Committed { inserted: true }
    );

    let runtime_has_journal_select: bool =
        sqlx::query_scalar("SELECT pg_catalog.has_table_privilege($1,$2,'SELECT')")
            .bind(r)
            .bind(&journal)
            .fetch_one(&fixture.admin)
            .await
            .expect("runtime journal privilege");
    assert!(
        !runtime_has_journal_select,
        "runtime must not read the journal directly"
    );
    let runtime_has_journal_column_select: bool =
        sqlx::query_scalar("SELECT pg_catalog.has_any_column_privilege($1,$2,'SELECT')")
            .bind(r)
            .bind(&journal)
            .fetch_one(&fixture.admin)
            .await
            .expect("runtime journal column privilege");
    assert!(
        !runtime_has_journal_column_select,
        "runtime must not read journal columns directly"
    );
    let mut direct_runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime low-level receipt connection");
    let direct_low_level_call = sqlx::query_scalar::<_, bool>(&format!(
        "SELECT {s}.waddle_ingress_record_effect_receipt($1,$2,$3)"
    ))
    .bind(operation.message.to_storage())
    .bind(operation.kind.to_storage())
    .bind(operation.semantic_identity_hash.as_slice())
    .fetch_one(&mut direct_runtime)
    .await;
    assert!(
        matches!(&direct_low_level_call, Err(error) if error
            .as_database_error()
            .and_then(|database| database.code())
            .as_deref() == Some("42501")),
        "R must not bypass the journal through the underlying receipt writer: {direct_low_level_call:?}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut direct_runtime)
        .await
        .expect("clear denied-call transaction");
    drop(direct_runtime);
    let mut direct_policy_discard = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime policy-discard connection");
    let direct_policy_discard_result = sqlx::query_scalar::<_, bool>(&format!(
        "SELECT {s}.waddle_ingress_record_policy_discard($1,$2,$3,NULL::text)"
    ))
    .bind(receipts[7].message.to_storage())
    .bind(receipts[7].kind.to_storage())
    .bind(receipts[7].semantic_identity_hash.as_slice())
    .fetch_one(&mut direct_policy_discard)
    .await;
    assert!(
        matches!(&direct_policy_discard_result, Err(error) if error
            .as_database_error()
            .and_then(|database| database.code())
            .as_deref() == Some("42501")),
        "R must not create an unjournaled plain receipt through NULL policy discard: {direct_policy_discard_result:?}"
    );
    drop(direct_policy_discard);
    let unjournaled_discard_count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {s}.ingress_effect_receipts WHERE message_key=$1 AND kind=$2 AND semantic_identity_hash=$3"
    ))
    .bind(receipts[7].message.to_storage())
    .bind(receipts[7].kind.to_storage())
    .bind(receipts[7].semantic_identity_hash.as_slice())
    .fetch_one(&fixture.admin)
    .await
    .expect("check no unjournaled policy discard");
    assert_eq!(unjournaled_discard_count, 0);
    let mut outsider =
        PgConnection::connect_with(&fixture.runtime_options.clone().username(&fixture.outsider))
            .await
            .expect("outsider connection");
    let outsider_lookup = sqlx::query(&format!(
        "SELECT * FROM {s}.waddle_p21_lookup_receipt_operation($1)"
    ))
    .bind(operation.operation_id)
    .fetch_one(&mut outsider)
    .await;
    assert!(
        matches!(&outsider_lookup, Err(error) if error
            .as_database_error()
            .and_then(|database| database.code())
            .as_deref() == Some("42501")),
        "non-runtime role must be denied by function EXECUTE ACL: {outsider_lookup:?}"
    );
    drop(outsider);
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

    super::execute(
        &fixture.admin,
        &format!(
            "CREATE SEQUENCE {s}.p21_lookup_probe_seq; \
             GRANT USAGE,SELECT ON SEQUENCE {s}.p21_lookup_probe_seq TO {api_name};"
        ),
    )
    .await;

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
            "lookup owner",
            format!("ALTER FUNCTION {lookup_function} OWNER TO {r}"),
            format!("ALTER FUNCTION {lookup_function} OWNER TO {api_name}"),
        ),
        (
            "lookup search path",
            format!("ALTER FUNCTION {lookup_function} SET search_path=public"),
            format!("ALTER FUNCTION {lookup_function} SET search_path=pg_catalog,pg_temp"),
        ),
        (
            "lookup same-OID body replacement",
            format!(
                "CREATE OR REPLACE FUNCTION {s}.waddle_p21_lookup_receipt_operation(p_operation_id uuid) \
                 RETURNS TABLE (message_key uuid, receipt_kind integer, semantic_identity_hash bytea, inserted boolean) \
                 LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp ROWS 1 \
                 AS $bad_lookup$ \
                   SELECT journal.message_key,journal.receipt_kind, \
                       journal.semantic_identity_hash,journal.inserted \
                     FROM {journal} AS journal \
                    WHERE journal.operation_id=p_operation_id \
                      AND pg_catalog.nextval('{s}.p21_lookup_probe_seq'::pg_catalog.regclass)>0 \
                 $bad_lookup$"
            ),
            lookup_create_or_replace.clone(),
        ),
        (
            "lookup PUBLIC execute",
            format!("GRANT EXECUTE ON FUNCTION {lookup_function} TO PUBLIC"),
            format!("REVOKE EXECUTE ON FUNCTION {lookup_function} FROM PUBLIC"),
        ),
        (
            "lookup runtime grant option",
            format!("GRANT EXECUTE ON FUNCTION {lookup_function} TO {r} WITH GRANT OPTION"),
            format!("REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION {lookup_function} FROM {r}"),
        ),
        (
            "lookup overload",
            format!("CREATE FUNCTION {s}.waddle_p21_lookup_receipt_operation(uuid,boolean) RETURNS boolean LANGUAGE sql AS 'SELECT false'"),
            format!("DROP FUNCTION {s}.waddle_p21_lookup_receipt_operation(uuid,boolean)"),
        ),
        (
            "extra index",
            format!("CREATE INDEX p21_journal_hostile ON {journal}(receipt_kind)"),
            format!("DROP INDEX {s}.p21_journal_hostile"),
        ),
    ] {
        super::execute(&fixture.admin, &change).await;
        let hostile_lookup = label == "lookup same-OID body replacement";
        if hostile_lookup {
            let mut direct_runtime = PgConnection::connect_with(&fixture.runtime_options)
                .await
                .expect("runtime for direct hostile lookup positive control");
            sqlx::query(&format!(
                "SELECT * FROM {s}.waddle_p21_lookup_receipt_operation($1)"
            ))
            .bind(operation.operation_id)
            .fetch_one(&mut direct_runtime)
            .await
            .expect("hostile function positive control invokes its sequence");
            drop(direct_runtime);
        }
        let rejection = if hostile_lookup {
            let result = authority.readback(&operation).await;
            assert!(
                matches!(
                    &result,
                    Err(ReceiptAuthorityError::Profile(
                        RestrictedIngressProfileError::Mismatch("journal lookup function")
                    ))
                ),
                "same-OID hostile lookup must fail the lookup profile check: {result:?}"
            );
            result.map(|_| ()).map_err(|error| format!("{error:?}"))
        } else {
            authority
                .record(&ReceiptOperation {
                    operation_id: Uuid::new_v4(),
                    ..operation
                })
                .await
                .map(|_| ())
                .map_err(|error| format!("{error:?}"))
        };
        if label == "lookup same-OID body replacement" {
            let sentinel: (i64, bool) = sqlx::query_as(&format!(
                "SELECT last_value,is_called FROM {s}.p21_lookup_probe_seq"
            ))
            .fetch_one(&fixture.admin)
            .await
            .expect("lookup sequence sentinel");
            assert_eq!(
                sentinel,
                (1, true),
                "profile rejection must precede hostile lookup invocation"
            );
        }
        super::execute(&fixture.admin, &restore).await;
        let mut restore_acl = fixture
            .admin
            .begin()
            .await
            .expect("begin lookup ACL restoration");
        sqlx::query(&format!("GRANT CREATE ON SCHEMA {s} TO {api_name}"))
            .execute(&mut *restore_acl)
            .await
            .expect("temporary API create for owner restoration");
        sqlx::query(&format!("ALTER FUNCTION {lookup_function} OWNER TO {api_name}"))
            .execute(&mut *restore_acl)
            .await
            .expect("restore lookup owner");
        sqlx::query(&format!("REVOKE CREATE ON SCHEMA {s} FROM {api_name}"))
            .execute(&mut *restore_acl)
            .await
            .expect("revoke temporary API create");
        sqlx::query(&format!(
            "REVOKE ALL ON FUNCTION {lookup_function} FROM PUBLIC, {r}"
        ))
        .execute(&mut *restore_acl)
        .await
        .expect("clear lookup runtime ACL");
        sqlx::query(&format!("SET LOCAL ROLE {api_name}"))
            .execute(&mut *restore_acl)
            .await
            .expect("assume API owner for lookup ACL restore");
        sqlx::query(&format!("GRANT EXECUTE ON FUNCTION {lookup_function} TO {r}"))
            .execute(&mut *restore_acl)
            .await
            .expect("restore owner-granted lookup execute");
        sqlx::query("RESET ROLE")
            .execute(&mut *restore_acl)
            .await
            .expect("reset restored API role");
        restore_acl
            .commit()
            .await
            .expect("commit lookup ACL restoration");
        assert!(rejection.is_err(), "accepted hostile {label}: {rejection:?}");
        assert_eq!(
            authority
                .readback(&operation)
                .await
                .expect("restored profile"),
            ReceiptReadback::Committed { inserted: true }
        );
    }
    super::execute(
        &fixture.admin,
        &format!(
            "CREATE ACCESS METHOD p21_probe_heap TYPE TABLE HANDLER pg_catalog.heap_tableam_handler; \
             ALTER TABLE {journal} SET ACCESS METHOD p21_probe_heap"
        ),
    )
    .await;
    let changed_am: String = sqlx::query_scalar(
        "SELECT am.amname FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_am am ON am.oid=c.relam WHERE c.oid=$1",
    )
    .bind(Oid(pins.journal_relation_oid))
    .fetch_one(&fixture.admin)
    .await
    .expect("journal access method after mutation");
    assert_eq!(changed_am, "p21_probe_heap");
    let rejected = authority.readback(&operation).await;
    assert!(
        matches!(
            &rejected,
            Err(ReceiptAuthorityError::Profile(
                RestrictedIngressProfileError::Mismatch("journal table/ACL")
            ))
        ),
        "non-heap journal must fail before lookup: {rejected:?}"
    );
    super::execute(
        &fixture.admin,
        &format!(
            "ALTER TABLE {journal} SET ACCESS METHOD heap; \
             DROP ACCESS METHOD p21_probe_heap"
        ),
    )
    .await;
    assert_eq!(
        authority
            .readback(&operation)
            .await
            .expect("profile after heap restore"),
        ReceiptReadback::Committed { inserted: true }
    );
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
            prepare_function_oid: u32::MAX,
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
            effect_receipt_function_oid: u32::MAX,
            ..pins
        },
        ReceiptOperationPins {
            policy_discard_function_oid: u32::MAX,
            ..pins
        },
        ReceiptOperationPins {
            journal_function_oid: pins.journal_function_oid + 1,
            ..pins
        },
        ReceiptOperationPins {
            journal_lookup_function_oid: pins.journal_lookup_function_oid + 1,
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

    let pending = ReceiptOperation {
        operation_id: Uuid::new_v4(),
        ..operation
    };
    let mut pending_transaction = fixture
        .admin
        .begin()
        .await
        .expect("begin pending journal row");
    sqlx::query(&format!(
        "INSERT INTO {journal} (operation_id,message_key,receipt_kind,semantic_identity_hash,inserted) \
         VALUES ($1,$2,$3,$4,false)"
    ))
    .bind(pending.operation_id)
    .bind(pending.message.to_storage())
    .bind(pending.kind.to_storage())
    .bind(pending.semantic_identity_hash.as_slice())
    .execute(&mut *pending_transaction)
    .await
    .expect("uncommitted journal result");
    assert_eq!(
        authority
            .readback(&pending)
            .await
            .expect("uncommitted absence"),
        ReceiptReadback::Indeterminate,
        "an uncommitted journal row is not visible as rollback proof"
    );
    pending_transaction
        .rollback()
        .await
        .expect("rollback pending journal row");
    assert_eq!(
        authority
            .readback(&pending)
            .await
            .expect("rolled-back absence"),
        ReceiptReadback::Indeterminate
    );

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
