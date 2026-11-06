//! Isolated PG17 proof for the unregistered, partial v1 role transition.

use super::Fixture;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::ingress_substrate::EPOCH_GUARDED_TABLES;

const ROLE_GRAPH: &str =
    include_str!("../../src/db/migrations/owner/serving_role_graph_v1_candidate.sql");
const FIXED_API: &str =
    include_str!("../../src/db/migrations/owner/ingress_definer_api_candidate.sql");
const LOOKUP: &str =
    include_str!("../../src/db/migrations/owner/p21_receipt_journal_lookup_candidate.sql");
const HELPER_TABLES: &[&str] = &[
    "dnd_projection",
    "dnd_projection_source_version",
    "notification_settings_projection",
    "notification_settings_projection_source_version",
    "pubsub_affiliations",
    "pubsub_items",
    "pubsub_nodes",
    "pubsub_owner_configured",
    "pubsub_push_publications",
    "pubsub_schema_marker",
    "pubsub_schema_version",
    "pubsub_subscriptions",
];

struct Lab {
    fixture: Fixture,
    api: String,
    helper: String,
    attestor: String,
    installer: String,
    trusted_schema: String,
}

impl Lab {
    async fn new(url: &str) -> Self {
        let fixture = Fixture::new(url, true).await;
        let suffix = Uuid::new_v4().simple().to_string();
        let api = format!("v1_a_{suffix}");
        let helper = format!("v1_h_{suffix}");
        let attestor = format!("v1_t_{suffix}");
        let installer = format!("v1_i_{suffix}");
        let trusted_schema = format!("v1_trusted_{suffix}");
        let lab = Self {
            fixture,
            api,
            helper,
            attestor,
            installer,
            trusted_schema,
        };
        let f = &lab.fixture;
        for role in [&lab.api, &lab.helper, &lab.attestor] {
            lab.admin(&format!("CREATE ROLE {role} NOLOGIN NOINHERIT"))
                .await;
        }
        for sql in [
            format!("CREATE ROLE {} LOGIN NOINHERIT", lab.installer),
            format!("ALTER ROLE {} NOLOGIN NOINHERIT", f.owner),
            format!("ALTER ROLE {} NOINHERIT", f.runtime),
            format!("ALTER ROLE {} NOINHERIT", f.migrator),
            format!("GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE", f.owner, f.migrator),
            format!("GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE", lab.attestor, lab.installer),
            format!("REVOKE TEMPORARY ON DATABASE {} FROM PUBLIC", f.database),
            format!("CREATE SCHEMA {} AUTHORIZATION {}", lab.trusted_schema, lab.attestor),
            format!("GRANT USAGE ON SCHEMA {} TO {}", lab.trusted_schema, f.runtime),
            format!("CREATE TABLE {}.pending_delivery (id UUID)", f.schema),
            format!("ALTER TABLE {}.pending_delivery OWNER TO {}", f.schema, f.owner),
            format!("GRANT SELECT, INSERT, UPDATE, DELETE ON {}.ingress_messages TO {}", f.schema, f.runtime),
            format!("GRANT SELECT, UPDATE (id) ON {}.ingress_protocol_epoch TO {}", f.schema, f.runtime),
            format!("CREATE FUNCTION {}.pubsub_runtime_attach_lock() RETURNS VOID LANGUAGE sql SECURITY DEFINER AS 'SELECT NULL::void'", f.schema),
            format!("ALTER FUNCTION {}.pubsub_runtime_attach_lock() OWNER TO {}", f.schema, f.owner),
            format!("REVOKE ALL ON FUNCTION {}.pubsub_runtime_attach_lock() FROM PUBLIC", f.schema),
            format!("GRANT EXECUTE ON FUNCTION {}.pubsub_runtime_attach_lock() TO {}", f.schema, f.runtime),
        ] {
            lab.admin(&sql).await;
        }
        for table in HELPER_TABLES {
            lab.admin(&format!("CREATE TABLE {}.{table} (id INTEGER)", f.schema))
                .await;
            lab.admin(&format!(
                "ALTER TABLE {}.{table} OWNER TO {}",
                f.schema, f.owner
            ))
            .await;
        }
        lab
    }

    async fn admin(&self, sql: &str) {
        sqlx::raw_sql(sql)
            .execute(&self.fixture.admin)
            .await
            .unwrap_or_else(|error| panic!("fixture SQL failed: {sql}: {error}"));
    }

    fn render(&self) -> String {
        let f = &self.fixture;
        let api = FIXED_API
            .replace("__SCHEMA_PATH__", &f.schema)
            .replace("__SCHEMA__", &format!("\"{}\"", f.schema))
            .replace("__API_OWNER_LITERAL__", &self.api)
            .replace("__API_OWNER_IDENT__", &self.api)
            .replace("__RUNTIME_ROLE_LITERAL__", &f.runtime)
            .replace("__RUNTIME_ROLE_IDENT__", &f.runtime);
        let lookup = LOOKUP
            .replace("__SCHEMA_LITERAL__", &format!("'{}'", f.schema))
            .replace("__SCHEMA__", &format!("\"{}\"", f.schema))
            .replace("__RUNTIME_ROLE__", &f.runtime);
        let mut sql = ROLE_GRAPH
            .replace("__FIXED_INGRESS_API__", &api)
            .replace("__FIXED_RECEIPT_LOOKUP__", &lookup)
            .replace("__SCHEMA_PATH__", &f.schema)
            .replace("__SCHEMA_LITERAL__", &f.schema)
            .replace("__SCHEMA__", &format!("\"{}\"", f.schema))
            .replace("__TRUSTED_SCHEMA_LITERAL__", &self.trusted_schema);
        for (placeholder, role) in [
            ("S", &f.owner),
            ("R", &f.runtime),
            ("A", &self.api),
            ("H", &self.helper),
            ("T", &self.attestor),
            ("I", &self.installer),
            ("X", &f.migrator),
        ] {
            sql = sql
                .replace(&format!("__{placeholder}_LITERAL__"), role)
                .replace(&format!("__{placeholder}_IDENT__"), role);
        }
        assert!(!sql.contains("__SCHEMA"));
        assert!(!sql.contains("__FIXED_"));
        sql
    }

    async fn attempt_transition(&self, barrier: bool) -> Result<(), sqlx::Error> {
        let mut tx = self.fixture.admin.begin().await.expect("begin transition");
        if barrier {
            sqlx::query(&format!(
                "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
                self.fixture.schema
            ))
            .execute(&mut *tx)
            .await
            .expect("lock epoch barrier");
        }
        let result = sqlx::raw_sql(&self.render()).execute(&mut *tx).await;
        tx.rollback().await.expect("rollback transition probe");
        result.map(|_| ())
    }

    async fn install(&self) {
        let mut tx = self.fixture.admin.begin().await.expect("begin transition");
        sqlx::query(&format!(
            "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
            self.fixture.schema
        ))
        .execute(&mut *tx)
        .await
        .expect("lock epoch barrier");
        sqlx::raw_sql(&self.render())
            .execute(&mut *tx)
            .await
            .expect("install composed candidate");
        tx.commit().await.expect("commit laboratory transition");
    }

    async fn cleanup(self) {
        let Self {
            fixture: f,
            api,
            helper,
            attestor,
            installer,
            ..
        } = self;
        f.admin.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", f.database))
            .execute(&f.cluster)
            .await
            .expect("drop isolated database");
        for role in [
            &installer,
            &attestor,
            &helper,
            &api,
            &f.runtime,
            &f.migrator,
            &f.owner,
        ] {
            sqlx::query(&format!("DROP ROLE {role}"))
                .execute(&f.cluster)
                .await
                .expect("drop laboratory role");
        }
        f.cluster.close().await;
    }
}

fn assert_rejected(result: Result<(), sqlx::Error>, reason: &str) {
    let error = result.expect_err("transition should reject the hostile input");
    let sqlx::Error::Database(error) = error else {
        panic!("expected a PostgreSQL rejection");
    };
    assert!(
        error.message().contains(reason),
        "unexpected rejection: {error}"
    );
}

fn assert_permission_denied(error: sqlx::Error, relation_or_reason: &str) {
    let sqlx::Error::Database(error) = error else {
        panic!("expected PostgreSQL permission error");
    };
    assert_eq!(error.code().as_deref(), Some("42501"));
    assert!(
        error.message().contains(relation_or_reason),
        "unexpected permission error: {error}"
    );
}

async fn begin_epoch(runtime: &mut PgConnection, schema: &str) {
    sqlx::query("BEGIN ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *runtime)
        .await
        .expect("begin runtime transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {schema}.ingress_protocol_epoch IN ACCESS SHARE MODE"
    ))
    .execute(&mut *runtime)
    .await
    .expect("runtime epoch prelock");
}

async fn insert_origin(
    runtime: &mut PgConnection,
    schema: &str,
    key: Uuid,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT outcome FROM {schema}.waddle_ingress_resolve_origin(\
         $1::pg_catalog.uuid, 1::pg_catalog.int4, $2::pg_catalog.bytea, $3::pg_catalog.bytea, \
         'alice@example.test'::pg_catalog.text, 1::pg_catalog.int4, \
         'bob@example.test'::pg_catalog.text, $4::pg_catalog.text, \
         NULL::pg_catalog.text, NULL::pg_catalog.uuid, NULL::pg_catalog.numeric, \
         NULL::pg_catalog.numeric, NULL::pg_catalog.uuid, NULL::pg_catalog.numeric)"
    ))
    .bind(key)
    .bind(vec![1_u8; 32])
    .bind([key.as_bytes().as_slice(), key.as_bytes().as_slice()].concat())
    .bind(key.to_string())
    .fetch_one(runtime)
    .await
}

#[tokio::test]
async fn v1_rejects_wrong_barrier_roles_temp_and_acl() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let lab = Lab::new(&url).await;
    let f = &lab.fixture;
    assert_rejected(
        lab.attempt_transition(false).await,
        "requires PG17, distinct principals and the epoch barrier",
    );

    lab.admin(&format!(
        "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
        f.owner, f.runtime
    ))
    .await;
    assert_rejected(
        lab.attempt_transition(true).await,
        "v1 role membership mismatch",
    );
    lab.admin(&format!("REVOKE {} FROM {}", f.owner, f.runtime))
        .await;

    lab.admin(&format!(
        "GRANT TEMPORARY ON DATABASE {} TO {}",
        f.database, f.runtime
    ))
    .await;
    assert_rejected(lab.attempt_transition(true).await, "v1 runtime has");
    lab.admin(&format!(
        "REVOKE TEMPORARY ON DATABASE {} FROM {}",
        f.database, f.runtime
    ))
    .await;

    lab.admin(&format!(
        "GRANT SET ON PARAMETER session_replication_role TO {}",
        f.runtime
    ))
    .await;
    assert_rejected(lab.attempt_transition(true).await, "v1 runtime has");
    lab.admin(&format!(
        "REVOKE SET ON PARAMETER session_replication_role FROM {}",
        f.runtime
    ))
    .await;

    lab.admin(&format!(
        "GRANT EXECUTE ON FUNCTION pg_catalog.pg_ls_tmpdir() TO {}",
        f.runtime
    ))
    .await;
    assert_rejected(lab.attempt_transition(true).await, "v1 runtime has");
    lab.admin(&format!(
        "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_ls_tmpdir() FROM {}",
        f.runtime
    ))
    .await;

    lab.admin(&format!(
        "GRANT SELECT ON {}.ingress_protocol_epoch TO {}",
        f.schema, lab.installer
    ))
    .await;
    assert_rejected(
        lab.attempt_transition(true).await,
        "v1 unexpected guarded relation ACL",
    );
    lab.admin(&format!(
        "REVOKE SELECT ON {}.ingress_protocol_epoch FROM {}",
        f.schema, lab.installer
    ))
    .await;

    // PUBLIC can confer effective privileges to A without a direct A ACL.
    lab.admin(&format!("GRANT UPDATE ON {}.sessions TO PUBLIC", f.schema))
        .await;
    assert_rejected(
        lab.attempt_transition(true).await,
        "v1 role graph postflight failed",
    );
    lab.admin(&format!(
        "REVOKE UPDATE ON {}.sessions FROM PUBLIC",
        f.schema
    ))
    .await;

    // PUBLIC can confer effective privileges to H without a direct H ACL.
    lab.admin(&format!(
        "GRANT SELECT ON {}.pubsub_nodes TO PUBLIC",
        f.schema
    ))
    .await;
    assert_rejected(
        lab.attempt_transition(true).await,
        "v1 role graph postflight failed",
    );
    lab.admin(&format!(
        "REVOKE SELECT ON {}.pubsub_nodes FROM PUBLIC",
        f.schema
    ))
    .await;

    lab.admin(&format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {} IN SCHEMA {} GRANT EXECUTE ON FUNCTIONS TO {}",
        lab.api, f.schema, lab.installer
    ))
    .await;
    assert_rejected(
        lab.attempt_transition(true).await,
        "receipt lookup has unexpected default ACL grantees",
    );
    lab.admin(&format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {} IN SCHEMA {} REVOKE EXECUTE ON FUNCTIONS FROM {}",
        lab.api, f.schema, lab.installer
    ))
    .await;

    // Function signatures in the pinned assets must resolve built-in types
    // even if the trusted installer's connection retained a temporary domain.
    let mut installer = PgConnection::connect_with(&f.admin_options)
        .await
        .expect("admin connection");
    let mut tx = installer
        .begin()
        .await
        .expect("begin hostile installer type probe");
    sqlx::query("CREATE DOMAIN pg_temp.text AS integer")
        .execute(&mut *tx)
        .await
        .expect("create installer temp domain");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        f.schema
    ))
    .execute(&mut *tx)
    .await
    .expect("lock epoch barrier");
    sqlx::raw_sql(&lab.render())
        .execute(&mut *tx)
        .await
        .expect("install with hostile installer temp type");
    tx.rollback().await.expect("rollback installer type probe");
    drop(installer);
    lab.cleanup().await;
}

#[tokio::test]
async fn v1_fixed_api_runs_v1009_with_select_only_runtime_and_exact_lookup() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let lab = Lab::new(&url).await;
    let f = &lab.fixture;
    let old_helper: i64 = sqlx::query_scalar(&format!(
        "SELECT '{}.pubsub_runtime_attach_lock()'::regprocedure::oid::bigint",
        f.schema
    ))
    .fetch_one(&f.admin)
    .await
    .expect("old helper OID");

    // A temp object can survive TEMP revocation in an existing session. The
    // selected functions and retained V1009 trigger must resolve real objects.
    lab.admin(&format!(
        "GRANT TEMPORARY ON DATABASE {} TO {}",
        f.database, f.runtime
    ))
    .await;
    let mut runtime = PgConnection::connect_with(&f.runtime_options)
        .await
        .expect("runtime login");
    sqlx::raw_sql("CREATE TEMP TABLE ingress_messages (witness INTEGER); CREATE DOMAIN pg_temp.text AS integer")
        .execute(&mut runtime).await.expect("create retained hostile temp objects");
    lab.admin(&format!(
        "REVOKE TEMPORARY ON DATABASE {} FROM {}",
        f.database, f.runtime
    ))
    .await;
    lab.install().await;

    let new_helper: (i64, String) = sqlx::query_as(&format!(
        "SELECT p.oid::bigint, pg_catalog.pg_get_userbyid(p.proowner)::text FROM pg_catalog.pg_proc p WHERE p.oid='{}.pubsub_runtime_attach_lock()'::regprocedure", f.schema
    )).fetch_one(&f.admin).await.expect("new helper identity");
    assert_ne!(new_helper.0, old_helper);
    assert_eq!(new_helper.1, lab.helper);

    for table in EPOCH_GUARDED_TABLES {
        let relation = format!("{}.{table}", f.schema);
        let (runtime_denied, api_exact): (bool, bool) = sqlx::query_as(
            "SELECT NOT pg_catalog.has_table_privilege($1, $3, 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN') \
             AND NOT pg_catalog.has_any_column_privilege($1, $3, 'SELECT,INSERT,UPDATE,REFERENCES'), \
             pg_catalog.has_table_privilege($2, $3, 'SELECT') \
             AND pg_catalog.has_table_privilege($2, $3, 'INSERT') \
             AND pg_catalog.has_table_privilege($2, $3, 'UPDATE') \
             AND pg_catalog.has_table_privilege($2, $3, 'DELETE') \
             AND NOT pg_catalog.has_table_privilege($2, $3, 'TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')"
        )
        .bind(&f.runtime).bind(&lab.api).bind(&relation)
        .fetch_one(&f.admin).await.expect("inspect guarded-table ACL");
        assert!(runtime_denied && api_exact, "unexpected ACL on {relation}");
        let denied = sqlx::query(&format!("DELETE FROM ONLY {relation} WHERE false"))
            .execute(&mut runtime)
            .await
            .expect_err("runtime guarded DML must fail");
        assert_permission_denied(denied, table);
    }
    for table in HELPER_TABLES {
        let relation = format!("{}.{table}", f.schema);
        let exact: bool = sqlx::query_scalar(
            "SELECT pg_catalog.has_table_privilege($1, $2, 'UPDATE') \
             AND NOT pg_catalog.has_table_privilege($1, $2, 'SELECT,INSERT,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')"
        ).bind(&lab.helper).bind(&relation).fetch_one(&f.admin).await.expect("inspect helper ACL");
        assert!(exact, "unexpected helper privilege on {relation}");
    }
    let epoch_acl: (bool, bool, bool) = sqlx::query_as(
        "SELECT pg_catalog.has_table_privilege($1, $3, 'SELECT'), \
         pg_catalog.has_column_privilege($2, $3, 'id', 'UPDATE'), \
         pg_catalog.has_column_privilege($2, $3, 'epoch', 'UPDATE')",
    )
    .bind(&f.runtime)
    .bind(&lab.api)
    .bind(format!("{}.ingress_protocol_epoch", f.schema))
    .fetch_one(&f.admin)
    .await
    .expect("inspect epoch ACL");
    assert_eq!(epoch_acl, (true, true, false));
    sqlx::query(&format!("SELECT {}.pubsub_runtime_attach_lock()", f.schema))
        .execute(&mut runtime)
        .await
        .expect("fixed H helper locks ONLY relations");

    assert!(
        sqlx::query(&format!(
            "UPDATE {}.ingress_protocol_epoch SET id=id WHERE id=1",
            f.schema
        ))
        .execute(&mut runtime)
        .await
        .is_err(),
        "R lacks epoch UPDATE(id)"
    );
    assert!(
        sqlx::query(&format!(
            "INSERT INTO {}.ingress_messages DEFAULT VALUES",
            f.schema
        ))
        .execute(&mut runtime)
        .await
        .is_err(),
        "R lacks direct guarded DML"
    );
    assert!(
        sqlx::query(&format!(
            "SELECT {}.waddle_ingress_definer_prepare()",
            f.schema
        ))
        .execute(&mut runtime)
        .await
        .is_err(),
        "R cannot call internal preparation helper"
    );
    assert!(
        sqlx::query(&format!(
            "SELECT * FROM {}.p21_receipt_operation_results",
            f.schema
        ))
        .execute(&mut runtime)
        .await
        .is_err(),
        "R cannot enumerate journal"
    );
    begin_epoch(&mut runtime, &f.schema).await;
    assert!(
        sqlx::query(&format!(
            "SELECT epoch FROM {}.ingress_protocol_epoch WHERE id=1 FOR SHARE",
            f.schema
        ))
        .execute(&mut runtime)
        .await
        .is_err(),
        "SELECT alone cannot acquire V1009's row lock"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback rejected row lock");

    let key = Uuid::new_v4();
    let operation = Uuid::new_v4();
    let hash = vec![7_u8; 32];
    begin_epoch(&mut runtime, &f.schema).await;
    assert_eq!(
        insert_origin(&mut runtime, &f.schema, key)
            .await
            .expect("A-owned fixed API executes V1009"),
        "inserted"
    );
    let intent: bool = sqlx::query_scalar(&format!(
        "SELECT {}.waddle_ingress_insert_effect_intent($1::pg_catalog.uuid, 0::pg_catalog.numeric, 1::pg_catalog.int4, $2::pg_catalog.bytea, ''::pg_catalog.bytea)",
        f.schema
    ))
    .bind(key)
    .bind(&hash)
    .fetch_one(&mut runtime)
    .await
    .expect("insert effect intent");
    assert!(intent);
    let recorded: bool = sqlx::query_scalar(&format!(
        "SELECT {}.waddle_p21_record_effect_receipt($1, $2, 1, $3)",
        f.schema
    ))
    .bind(operation)
    .bind(key)
    .bind(&hash)
    .fetch_one(&mut runtime)
    .await
    .expect("record fixed receipt");
    assert!(recorded);
    sqlx::query("COMMIT")
        .execute(&mut runtime)
        .await
        .expect("commit fixed API calls");
    let found: (Uuid, i32, Vec<u8>, bool) = sqlx::query_as(&format!(
        "SELECT * FROM {}.waddle_p21_lookup_receipt_operation($1)",
        f.schema
    ))
    .bind(operation)
    .fetch_one(&mut runtime)
    .await
    .expect("lookup exact operation");
    assert_eq!(found, (key, 1, hash, true));
    let absent: Option<Uuid> = sqlx::query_scalar(&format!(
        "SELECT message_key FROM {}.waddle_p21_lookup_receipt_operation($1)",
        f.schema
    ))
    .bind(Uuid::new_v4())
    .fetch_optional(&mut runtime)
    .await
    .expect("lookup absent operation");
    assert!(absent.is_none());
    // Even a mistaken extra EXECUTE grant cannot make a different login pass
    // the fixed API's session identity check. Remove the test-only grants.
    lab.admin(&format!(
        "GRANT USAGE ON SCHEMA {} TO {}",
        f.schema, lab.installer
    ))
    .await;
    lab.admin(&format!(
        "GRANT SELECT ON {}.ingress_protocol_epoch TO {}",
        f.schema, lab.installer
    ))
    .await;
    lab.admin(&format!(
        "GRANT EXECUTE ON FUNCTION {}.waddle_ingress_lock_message(UUID) TO {}",
        f.schema, lab.installer
    ))
    .await;
    let mut wrong = PgConnection::connect_with(&f.runtime_options.clone().username(&lab.installer))
        .await
        .expect("connect different login");
    begin_epoch(&mut wrong, &f.schema).await;
    let denied = sqlx::query(&format!(
        "SELECT {}.waddle_ingress_lock_message($1)",
        f.schema
    ))
    .bind(key)
    .execute(&mut wrong)
    .await
    .expect_err("wrong session login must fail");
    assert_permission_denied(denied, "restricted to the runtime login");
    sqlx::query("ROLLBACK")
        .execute(&mut wrong)
        .await
        .expect("rollback wrong login");
    drop(wrong);
    lab.admin(&format!(
        "REVOKE EXECUTE ON FUNCTION {}.waddle_ingress_lock_message(UUID) FROM {}",
        f.schema, lab.installer
    ))
    .await;
    lab.admin(&format!(
        "REVOKE SELECT ON {}.ingress_protocol_epoch FROM {}",
        f.schema, lab.installer
    ))
    .await;
    lab.admin(&format!(
        "REVOKE USAGE ON SCHEMA {} FROM {}",
        f.schema, lab.installer
    ))
    .await;

    let shadow_count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_temp.ingress_messages")
        .fetch_one(&mut runtime)
        .await
        .expect("check temp shadow");
    assert_eq!(shadow_count, 0);

    // Removing A's sole row-lock column right breaks the actual retained
    // invoker trigger, proving the positive call used A's permission.
    lab.admin(&format!(
        "REVOKE UPDATE (id) ON {}.ingress_protocol_epoch FROM {}",
        f.schema, lab.api
    ))
    .await;
    begin_epoch(&mut runtime, &f.schema).await;
    let denied = insert_origin(&mut runtime, &f.schema, Uuid::new_v4())
        .await
        .expect_err("V1009 denies A without UPDATE(id)");
    assert_permission_denied(denied, "ingress_protocol_epoch");
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback revoked A privilege");
    lab.admin(&format!(
        "GRANT UPDATE (id) ON {}.ingress_protocol_epoch TO {}",
        f.schema, lab.api
    ))
    .await;
    drop(runtime);
    lab.cleanup().await;
}
