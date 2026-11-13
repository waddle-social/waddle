//! PG17 hostile catalog/ACL mutations against the non-authorizing observer.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::postgres::PgPool;
use sqlx::{Connection, PgConnection};
use waddle_server::db::physical_attestation::{
    attested_pool, AttestedPgPool, PgRoleName, RestrictedIngressProfileError,
    CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::ingress_substrate::INGRESS_GUARDED_TABLES;
use waddle_server::pending_delivery::bootstrap_pending_delivery_owner_schema;

use super::{
    prepare_candidate_application_fixture, test_url, url_with_schema, Fixture, CATALOG_GRANT_LOCK,
};

#[path = "restricted_catalog_preflight.rs"]
mod catalog_preflight;
#[path = "restricted_completion_transaction.rs"]
mod completion_transaction;
#[path = "restricted_receipt_journal.rs"]
mod receipt_journal;
#[path = "restricted_receipt_transaction.rs"]
mod receipt_transaction;
#[path = "restricted_transaction_probe.rs"]
mod transaction_probe;

const CANDIDATE: &str =
    include_str!("../../src/db/migrations/owner/ingress_definer_api_candidate.sql");

async fn execute(admin: &PgPool, sql: &str) {
    sqlx::raw_sql(sql)
        .execute(admin)
        .await
        .expect("fixture mutation/restore");
}

#[tokio::test]
async fn restricted_ingress_profile_refuses_hostile_pg17_catalogs() {
    let _guard = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else {
        return;
    };
    let fixture = Fixture::new_with_c_collation(url).await;
    let config = prepare_candidate_application_fixture(&fixture).await;
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
        .fetch_one(&fixture.admin)
        .await
        .expect("PG version");
    assert!((170_000..180_000).contains(&version), "requires PG17");
    let owner = Database::from_config(
        "pending owner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &fixture.target),
        ),
    )
    .await
    .expect("pending owner database");
    bootstrap_pending_delivery_owner_schema(&owner)
        .await
        .expect("canonical pending owner schema");
    drop(owner);
    let api = format!("api_{}", uuid::Uuid::new_v4().simple());
    let s = &fixture.target;
    let r = &fixture.runtime;
    execute(&fixture.admin, &format!("CREATE ROLE {api} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT; GRANT USAGE, CREATE ON SCHEMA {s} TO {api}; GRANT SELECT ON {s}.ingress_protocol_epoch, {s}.sessions TO {api}; GRANT UPDATE(id) ON {s}.ingress_protocol_epoch, {s}.sessions TO {api}; GRANT SELECT ON {s}.ingress_epoch_guard_manifest TO {r};")).await;
    for table in INGRESS_GUARDED_TABLES {
        execute(&fixture.admin, &format!("REVOKE ALL ON {s}.{table} FROM {r}; GRANT SELECT,INSERT,UPDATE,DELETE ON {s}.{table} TO {api};")).await;
    }
    let candidate = CANDIDATE
        .replace("__SCHEMA_PATH__", &format!("\"{s}\""))
        .replace("__SCHEMA__", &format!("\"{s}\""))
        .replace("__API_OWNER_LITERAL__", &api)
        .replace("__API_OWNER_IDENT__", &api)
        .replace("__RUNTIME_ROLE_LITERAL__", r)
        .replace("__RUNTIME_ROLE_IDENT__", r);
    execute(&fixture.admin, &candidate).await;
    execute(
        &fixture.admin,
        &format!("REVOKE CREATE ON SCHEMA {s} FROM {api}"),
    )
    .await;
    let epoch_oid: Oid = sqlx::query_scalar(
        "SELECT oid FROM pg_class WHERE relnamespace=$1 AND relname='ingress_protocol_epoch'",
    )
    .bind(Oid(fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("PG identity")
        .schema
        .oid))
    .fetch_one(&fixture.admin)
    .await
    .expect("epoch OID");
    let epoch_oid = epoch_oid.0;
    let api_role = PgRoleName::new(&api).expect("API role");
    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        1,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let observed = pool
        .inspect_restricted_ingress_profile(&api_role, epoch_oid)
        .await
        .expect("exact restricted profile");
    assert_eq!(observed.manifest_version, 2);
    assert_eq!(observed.epoch_relation_oid, epoch_oid);
    let mut mutation = fixture
        .admin
        .begin()
        .await
        .expect("catalog mutator transaction");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *mutation)
        .await
        .expect("hold exclusive catalog mutation fence");
    assert!(matches!(
        pool.inspect_restricted_ingress_profile(&api_role, epoch_oid)
            .await,
        Err(RestrictedIngressProfileError::Mismatch(
            "catalog mutation fence"
        ))
    ));
    mutation.rollback().await.expect("release catalog mutator");
    pool.inspect_restricted_ingress_profile(&api_role, epoch_oid)
        .await
        .expect("diagnostic resumes after catalog mutation fence");
    transaction_probe::exercise(&fixture, &config, &api_role, epoch_oid).await;
    catalog_preflight::exercise(&fixture, &pool, &api_role, epoch_oid).await;
    assert!(
        pool.inspect_restricted_ingress_profile(&api_role, epoch_oid + 1)
            .await
            .is_err(),
        "wrong epoch identity"
    );
    assert!(
        pool.inspect_restricted_ingress_profile(
            &PgRoleName::new(r).expect("runtime role"),
            epoch_oid
        )
        .await
        .is_err(),
        "runtime cannot be API owner"
    );
    let mut wrong_lineage = fixture.lineage.clone();
    wrong_lineage
        .postgres_identity
        .as_mut()
        .expect("PG identity")
        .schema
        .oid += 1;
    let wrong_pool = attested_pool(
        fixture.runtime_options.clone(),
        fixture.with_lineage(wrong_lineage),
        1,
        Duration::from_secs(1),
    )
    .expect("wrong physical pool");
    assert!(
        wrong_pool
            .inspect_restricted_ingress_profile(&api_role, epoch_oid)
            .await
            .is_err(),
        "wrong physical schema identity"
    );
    wrong_pool.close().await;
    let function = format!("{s}.waddle_ingress_message_is_terminal(uuid)");
    let definition: String = sqlx::query_scalar("SELECT pg_get_functiondef($1::regprocedure)")
        .bind(&function)
        .fetch_one(&fixture.admin)
        .await
        .expect("original function");
    let recreate = format!("{definition}; ALTER FUNCTION {function} OWNER TO {api}; REVOKE ALL ON FUNCTION {function} FROM PUBLIC; GRANT EXECUTE ON FUNCTION {function} TO {r};");
    let schema_owner: String =
        sqlx::query_scalar("SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname=$1")
            .bind(s)
            .fetch_one(&fixture.admin)
            .await
            .expect("schema owner");
    let t = format!("{s}.ingress_messages");
    let trigger = "ingress_messages_definer_api_guard_dml";
    let trigger_def: String = sqlx::query_scalar(
        "SELECT pg_get_triggerdef(oid) FROM pg_trigger WHERE tgrelid=$1::regclass AND tgname=$2",
    )
    .bind(&t)
    .bind(trigger)
    .fetch_one(&fixture.admin)
    .await
    .expect("trigger definition");
    let cases = vec![
        ("missing function", format!("DROP FUNCTION {function}"), recreate.clone()),
        ("extra overload", format!("CREATE FUNCTION {s}.waddle_ingress_message_is_terminal(text) RETURNS boolean LANGUAGE sql AS 'SELECT true'"),format!("DROP FUNCTION {s}.waddle_ingress_message_is_terminal(text)")),
        ("different named definer",format!("CREATE FUNCTION {s}.alternate_authority() RETURNS boolean LANGUAGE sql SECURITY DEFINER AS 'SELECT true'; ALTER FUNCTION {s}.alternate_authority() OWNER TO {api}"),format!("DROP FUNCTION {s}.alternate_authority()")),
        ("cross schema definer",format!("CREATE SCHEMA alternate AUTHORIZATION {api}; GRANT USAGE ON SCHEMA alternate TO {r}; CREATE FUNCTION alternate.authority() RETURNS boolean LANGUAGE sql SECURITY DEFINER AS 'SELECT true'; ALTER FUNCTION alternate.authority() OWNER TO {api}"),"DROP SCHEMA alternate CASCADE".to_owned()),
        ("different owner definer",format!("CREATE FUNCTION {s}.alternate_owner() RETURNS boolean LANGUAGE sql SECURITY DEFINER AS 'SELECT true'"),format!("DROP FUNCTION {s}.alternate_owner()")),
        ("noncallable alternate", "CREATE SCHEMA hidden; CREATE FUNCTION hidden.authority() RETURNS boolean LANGUAGE sql SECURITY DEFINER AS 'SELECT true'; REVOKE EXECUTE ON FUNCTION hidden.authority() FROM PUBLIC".to_owned(), "DROP SCHEMA hidden CASCADE".to_owned()),
        ("extra API",format!("CREATE FUNCTION {s}.waddle_ingress_hostile() RETURNS boolean LANGUAGE sql AS 'SELECT true'"),format!("DROP FUNCTION {s}.waddle_ingress_hostile()")),
        ("body", format!("CREATE OR REPLACE FUNCTION {s}.waddle_ingress_message_is_terminal(p_message_key uuid) RETURNS boolean LANGUAGE plpgsql AS 'BEGIN RETURN true; END'"),recreate.clone()),
        ("language",format!("CREATE OR REPLACE FUNCTION {s}.waddle_ingress_message_is_terminal(p_message_key uuid) RETURNS boolean LANGUAGE sql AS 'SELECT true'"),recreate.clone()),
        ("search path",format!("ALTER FUNCTION {function} SET search_path=public,pg_catalog"),recreate.clone()),
        ("extra config",format!("ALTER FUNCTION {function} SET row_security=off"),format!("ALTER FUNCTION {function} RESET row_security")),
        ("security invoker",format!("ALTER FUNCTION {function} SECURITY INVOKER"),recreate.clone()),
        ("volatility",format!("ALTER FUNCTION {function} IMMUTABLE"),recreate.clone()),
        ("strictness",format!("ALTER FUNCTION {function} STRICT"),recreate.clone()),
        ("owner",format!("ALTER FUNCTION {function} OWNER TO {schema_owner}"),format!("ALTER FUNCTION {function} OWNER TO {api}")),
        ("missing execute",format!("REVOKE EXECUTE ON FUNCTION {function} FROM {r}"),format!("GRANT EXECUTE ON FUNCTION {function} TO {r}")),
        ("public execute",format!("GRANT EXECUTE ON FUNCTION {function} TO PUBLIC"),format!("REVOKE EXECUTE ON FUNCTION {function} FROM PUBLIC")),
        ("outsider execute",format!("GRANT EXECUTE ON FUNCTION {function} TO {}",fixture.outsider),format!("REVOKE EXECUTE ON FUNCTION {function} FROM {}",fixture.outsider)),
        ("grant option",format!("GRANT EXECUTE ON FUNCTION {function} TO {r} WITH GRANT OPTION"),format!("REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION {function} FROM {r}")),
        ("API login",format!("ALTER ROLE {api} LOGIN"),format!("ALTER ROLE {api} NOLOGIN")),
        ("API bypass",format!("ALTER ROLE {api} BYPASSRLS"),format!("ALTER ROLE {api} NOBYPASSRLS")),
        ("membership",format!("GRANT {api} TO {r}"),format!("REVOKE {api} FROM {r}")),
        ("API membership",format!("GRANT {} TO {api}",fixture.outsider),format!("REVOKE {} FROM {api}",fixture.outsider)),
        ("runtime TEMP",format!("GRANT TEMP ON DATABASE {} TO {r}",fixture.database_name),format!("REVOKE TEMP ON DATABASE {} FROM {r}",fixture.database_name)),
        ("API CREATE",format!("GRANT CREATE ON SCHEMA {s} TO {api}"),format!("REVOKE CREATE ON SCHEMA {s} FROM {api}")),
        ("parameter SET",format!("GRANT SET ON PARAMETER session_replication_role TO {r}"),format!("REVOKE SET ON PARAMETER session_replication_role FROM {r}")),
        ("runtime SELECT",format!("GRANT SELECT ON {t} TO {r}"),format!("REVOKE SELECT ON {t} FROM {r}")),
        ("runtime column UPDATE",format!("GRANT UPDATE(message_key) ON {t} TO {r}"),format!("REVOKE UPDATE(message_key) ON {t} FROM {r}")),
        ("API TRUNCATE",format!("GRANT TRUNCATE ON {t} TO {api}"),format!("REVOKE TRUNCATE ON {t} FROM {api}")),
        ("API missing DELETE",format!("REVOKE DELETE ON {t} FROM {api}"),format!("GRANT DELETE ON {t} TO {api}")),
        ("disabled trigger",format!("ALTER TABLE {t} DISABLE TRIGGER {trigger}"),format!("ALTER TABLE {t} ENABLE ALWAYS TRIGGER {trigger}")),
        ("origin trigger",format!("ALTER TABLE {t} ENABLE TRIGGER {trigger}"),format!("ALTER TABLE {t} ENABLE ALWAYS TRIGGER {trigger}")),
        ("missing trigger",format!("DROP TRIGGER {trigger} ON {t}"),format!("{trigger_def}; ALTER TABLE {t} ENABLE ALWAYS TRIGGER {trigger}")),
        ("extra trigger",format!("CREATE TRIGGER hostile BEFORE DELETE ON {t} FOR EACH STATEMENT EXECUTE FUNCTION {s}.waddle_ingress_definer_epoch_guard()"),format!("DROP TRIGGER hostile ON {t}")),
        ("default",format!("ALTER TABLE {t} ALTER COLUMN digest_version SET DEFAULT 1"),format!("ALTER TABLE {t} ALTER COLUMN digest_version DROP DEFAULT")),
        ("constraint",format!("ALTER TABLE {t} ADD CONSTRAINT hostile_check CHECK (digest_version > 0)"),format!("ALTER TABLE {t} DROP CONSTRAINT hostile_check")),
        ("extra index",format!("CREATE INDEX hostile_index ON {t}(digest_version)"),format!("DROP INDEX {s}.hostile_index")),
        ("pending default",format!("ALTER TABLE {s}.pending_delivery ALTER COLUMN claim_offered SET DEFAULT 9"),format!("ALTER TABLE {s}.pending_delivery ALTER COLUMN claim_offered SET DEFAULT 0")),
        ("internal trigger",format!("ALTER TABLE {s}.ingress_origin_aliases DISABLE TRIGGER ALL"),format!("ALTER TABLE {s}.ingress_origin_aliases ENABLE TRIGGER ALL; ALTER TABLE {s}.ingress_origin_aliases ENABLE ALWAYS TRIGGER ingress_origin_aliases_epoch_guard_dml; ALTER TABLE {s}.ingress_origin_aliases ENABLE ALWAYS TRIGGER ingress_origin_aliases_epoch_guard_truncate; ALTER TABLE {s}.ingress_origin_aliases ENABLE ALWAYS TRIGGER ingress_origin_aliases_definer_api_guard_dml")),
        ("column type",format!("ALTER TABLE {s}.pending_delivery ALTER COLUMN claim_offered TYPE bigint"),format!("ALTER TABLE {s}.pending_delivery ALTER COLUMN claim_offered TYPE integer")),
        ("sessions inheritance child",format!("CREATE TABLE {s}.session_child () INHERITS ({s}.sessions)"),format!("DROP TABLE {s}.session_child")),
        ("pending inheritance child",format!("CREATE TABLE {s}.pending_child () INHERITS ({s}.pending_delivery)"),format!("DROP TABLE {s}.pending_child")),
        ("API dependency UPDATE",format!("GRANT UPDATE ON {s}.sessions TO {api}"),format!("REVOKE UPDATE ON {s}.sessions FROM {api}; GRANT UPDATE(id) ON {s}.sessions TO {api}")),
        ("row security",format!("ALTER TABLE {t} ENABLE ROW LEVEL SECURITY"),format!("ALTER TABLE {t} DISABLE ROW LEVEL SECURITY")),
        ("dormant policy",format!("CREATE POLICY hostile ON {t} USING (true)"),format!("DROP POLICY hostile ON {t}")),
        ("guard manifest row",format!("INSERT INTO {s}.ingress_epoch_guard_manifest(table_name) VALUES ('hostile')"),format!("ALTER TABLE {s}.ingress_epoch_guard_manifest DISABLE TRIGGER ingress_epoch_guard_manifest_append_only_row; DELETE FROM {s}.ingress_epoch_guard_manifest WHERE table_name='hostile'; ALTER TABLE {s}.ingress_epoch_guard_manifest ENABLE ALWAYS TRIGGER ingress_epoch_guard_manifest_append_only_row")),
        ("ungranted hidden view",format!("CREATE SCHEMA private_view; CREATE VIEW private_view.hidden AS SELECT message_key FROM {t}"),"DROP SCHEMA private_view CASCADE".to_owned()),
        ("column-granted view",format!("CREATE SCHEMA column_view; CREATE VIEW column_view.exposed AS SELECT message_key FROM {t}; GRANT USAGE ON SCHEMA column_view TO {r}; GRANT SELECT(message_key), UPDATE(message_key) ON column_view.exposed TO {r}"),"DROP SCHEMA column_view CASCADE".to_owned()),
        ("foreign table",format!("CREATE FOREIGN DATA WRAPPER hostile_fdw; CREATE SERVER hostile_server FOREIGN DATA WRAPPER hostile_fdw; CREATE SCHEMA foreign_probe; CREATE FOREIGN TABLE foreign_probe.exposed(id integer) SERVER hostile_server; GRANT USAGE ON SCHEMA foreign_probe TO {r}; GRANT SELECT ON foreign_probe.exposed TO {r}"),"DROP SCHEMA foreign_probe CASCADE; DROP SERVER hostile_server; DROP FOREIGN DATA WRAPPER hostile_fdw".to_owned()),
        ("ordinary rewrite rule",format!("CREATE SCHEMA rewrite_probe; CREATE TABLE rewrite_probe.input(id integer); CREATE RULE discard AS ON INSERT TO rewrite_probe.input DO INSTEAD NOTHING; GRANT USAGE ON SCHEMA rewrite_probe TO {r}; GRANT INSERT ON rewrite_probe.input TO {r}"),"DROP SCHEMA rewrite_probe CASCADE".to_owned()),
        ("extra table",format!("CREATE TABLE {s}.ingress_hostile(id integer)"),format!("DROP TABLE {s}.ingress_hostile")),
        ("replacement epoch",format!("ALTER TABLE {s}.ingress_protocol_epoch RENAME TO old_epoch; CREATE TABLE {s}.ingress_protocol_epoch(LIKE {s}.old_epoch INCLUDING ALL); GRANT SELECT ON {s}.ingress_protocol_epoch TO {r}"),format!("DROP TABLE {s}.ingress_protocol_epoch; ALTER TABLE {s}.old_epoch RENAME TO ingress_protocol_epoch")),
    ];
    for (label, mutate, restore) in cases {
        execute(&fixture.admin, &mutate).await;
        let rejected = pool
            .inspect_restricted_ingress_profile(&api_role, epoch_oid)
            .await;
        execute(&fixture.admin, &restore).await;
        assert!(rejected.is_err(), "accepted hostile {label}: {rejected:?}");
        pool.inspect_restricted_ingress_profile(&api_role, epoch_oid)
            .await
            .unwrap_or_else(|error| panic!("restore after {label}: {error:?}"));
    }
    prove_view_lock_bypasses(&fixture, &pool, &api_role, epoch_oid).await;
    let mut lease = pool.acquire_for_test().await.expect("retained session");
    sqlx::query("SET search_path=public,pg_catalog")
        .execute(&mut *lease)
        .await
        .expect("hostile retained path");
    drop(lease);
    pool.inspect_restricted_ingress_profile(&api_role, epoch_oid)
        .await
        .expect("checkout normalized");
    pool.close().await;
    let reconnected = attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        1,
        Duration::from_secs(2),
    )
    .expect("fresh pool");
    reconnected
        .inspect_restricted_ingress_profile(&api_role, epoch_oid)
        .await
        .expect("fresh physical identity/profile");
    reconnected.close().await;
    let mut raw = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    for sql in [
        format!("SELECT * FROM {t} FOR UPDATE"),
        format!("DELETE FROM {t}"),
        format!("SELECT {s}.waddle_ingress_definer_prepare()"),
    ] {
        assert!(
            sqlx::query(&sql).execute(&mut raw).await.is_err(),
            "runtime bypass: {sql}"
        );
    }
    drop(raw);
    receipt_transaction::exercise(&fixture, &config, &api_role, epoch_oid).await;
    completion_transaction::exercise(&fixture, &config, &api_role, &observed).await;
    receipt_journal::exercise(&fixture, &config, &api_role, epoch_oid).await;
    execute(
        &fixture.admin,
        &format!("DROP OWNED BY {api}; DROP ROLE {api}"),
    )
    .await;
    fixture.cleanup().await;
}

async fn prove_view_lock_bypasses(
    fixture: &Fixture,
    pool: &AttestedPgPool,
    api: &PgRoleName,
    epoch_oid: u32,
) {
    let s = &fixture.target;
    let r = &fixture.runtime;
    // Seed through the fixed API with its required epoch prelock. Every attack
    // below uses a separate fresh transaction with no epoch lock or proof.
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime proof connection");
    let mut seed = runtime.begin().await.expect("seed fixed API transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}.ingress_protocol_epoch IN ACCESS SHARE MODE"
    ))
    .execute(&mut *seed)
    .await
    .expect("seed epoch prelock");
    let inserted: bool = sqlx::query_scalar(&format!(
        "SELECT {s}.waddle_ingress_insert_message(pg_catalog.gen_random_uuid(),1, \
         pg_catalog.decode(pg_catalog.repeat('5a',32),'hex'),NULL::smallint,NULL::bytea, \
         NULL::text,NULL::uuid,NULL::numeric,NULL::numeric,NULL::uuid,NULL::numeric)"
    ))
    .fetch_one(&mut *seed)
    .await
    .expect("seed through exact definer API");
    assert!(inserted);
    seed.commit().await.expect("commit fixture canonical row");
    let direct: (bool, bool) = sqlx::query_as(
        "SELECT pg_catalog.has_table_privilege($1,$2,'SELECT,UPDATE'), \
         pg_catalog.has_any_column_privilege($1,$2,'SELECT,UPDATE')",
    )
    .bind(r)
    .bind(format!("{s}.ingress_messages"))
    .fetch_one(&fixture.admin)
    .await
    .expect("direct runtime privilege denial");
    assert_eq!(direct, (false, false));
    for nested in [false, true] {
        let setup = if nested {
            format!(
                "CREATE SCHEMA hidden_view AUTHORIZATION {api}; \
                 CREATE SCHEMA exposed_view AUTHORIZATION {outsider}; \
                 CREATE VIEW hidden_view.inner_messages AS SELECT message_key FROM {s}.ingress_messages; \
                 ALTER VIEW hidden_view.inner_messages OWNER TO {api}; \
                 GRANT USAGE ON SCHEMA hidden_view TO {outsider}; \
                 GRANT SELECT,UPDATE ON hidden_view.inner_messages TO {outsider}; \
                 CREATE VIEW exposed_view.messages AS SELECT message_key FROM hidden_view.inner_messages; \
                 ALTER VIEW exposed_view.messages OWNER TO {outsider}; \
                 GRANT USAGE ON SCHEMA exposed_view TO {r}; \
                 GRANT SELECT,UPDATE ON exposed_view.messages TO {r}",
                api=api.as_str(),outsider=fixture.outsider,
            )
        } else {
            format!(
                "CREATE SCHEMA exposed_view AUTHORIZATION {api}; \
                 CREATE VIEW exposed_view.messages AS SELECT message_key FROM {s}.ingress_messages; \
                 ALTER VIEW exposed_view.messages OWNER TO {api}; \
                 GRANT USAGE ON SCHEMA exposed_view TO {r}; \
                 GRANT SELECT,UPDATE ON exposed_view.messages TO {r}",api=api.as_str(),
            )
        };
        execute(&fixture.admin, &setup).await;
        if nested {
            let can_use_hidden: bool = sqlx::query_scalar(
                "SELECT pg_catalog.has_schema_privilege($1,'hidden_view','USAGE')",
            )
            .bind(r)
            .fetch_one(&fixture.admin)
            .await
            .expect("hidden namespace privilege");
            assert!(
                !can_use_hidden,
                "nested view hides its inner owner-rights authority"
            );
        }
        let mut attack = runtime.begin().await.expect("rollback-only view attack");
        let rows: Vec<uuid::Uuid> =
            sqlx::query_scalar("SELECT message_key FROM exposed_view.messages FOR UPDATE")
                .fetch_all(&mut *attack)
                .await
                .expect("view can lock guarded canonical row");
        assert_eq!(rows.len(), 1, "direct={}", !nested);
        let locks: (bool,bool) = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid=pg_catalog.pg_backend_pid() \
               AND relation=$1::regclass AND mode='RowShareLock' AND granted), \
             EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid=pg_catalog.pg_backend_pid() \
               AND relation=$2::regclass AND granted)",
        ).bind(format!("{s}.ingress_messages")).bind(format!("{s}.ingress_protocol_epoch"))
            .fetch_one(&mut *attack).await.expect("observe actual row-lock bypass");
        assert_eq!(
            locks,
            (true, false),
            "guarded RowShareLock without epoch barrier"
        );
        attack.rollback().await.expect("release attack locks");
        assert!(matches!(
            pool.inspect_restricted_ingress_profile(api, epoch_oid)
                .await,
            Err(RestrictedIngressProfileError::Mismatch(
                "unexpected non-system view/rule/foreign relation"
            ))
        ));
        execute(&fixture.admin, "DROP SCHEMA exposed_view CASCADE").await;
        if nested {
            execute(&fixture.admin, "DROP SCHEMA hidden_view CASCADE").await;
        }
        pool.inspect_restricted_ingress_profile(api, epoch_oid)
            .await
            .expect("restored view-free profile");
    }
}
