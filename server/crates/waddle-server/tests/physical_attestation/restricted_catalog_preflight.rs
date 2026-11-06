//! Fail closed before untrusted manifest reads or catalog expression deparsing.

use std::time::Duration;

use waddle_server::db::physical_attestation::{
    AttestedPgPool, PgRoleName, RestrictedIngressProfileError,
};

use super::{execute, Fixture};

pub(super) async fn exercise(
    fixture: &Fixture,
    pool: &AttestedPgPool,
    api: &PgRoleName,
    epoch_oid: u32,
) {
    let s = &fixture.target;
    let r = &fixture.runtime;
    let backend_pid = {
        let mut lease = pool.acquire_for_test().await.expect("initial pool lease");
        sqlx::query_scalar::<_, i32>("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&mut *lease)
            .await
            .expect("initial backend pid")
    };
    execute(
        &fixture.admin,
        &format!(
            "CREATE SEQUENCE {s}.preflight_sentinel; GRANT USAGE ON SEQUENCE {s}.preflight_sentinel TO {r}; \
             CREATE TYPE {s}.preflight_state AS ENUM ('hostile'); \
             CREATE FUNCTION {s}.preflight_text({s}.preflight_state) RETURNS text \
               LANGUAGE plpgsql VOLATILE AS $$ BEGIN PERFORM pg_catalog.nextval('{s}.preflight_sentinel'); RETURN 'hostile'; END $$; \
             CREATE CAST ({s}.preflight_state AS text) WITH FUNCTION {s}.preflight_text({s}.preflight_state); \
             CREATE FUNCTION {s}.preflight_integer({s}.preflight_state) RETURNS integer \
               LANGUAGE plpgsql VOLATILE AS $$ BEGIN PERFORM pg_catalog.nextval('{s}.preflight_sentinel'); RETURN 0; END $$; \
             CREATE CAST ({s}.preflight_state AS integer) WITH FUNCTION {s}.preflight_integer({s}.preflight_state); \
             CREATE FUNCTION {s}.preflight_operator(integer,integer) RETURNS boolean \
               LANGUAGE plpgsql VOLATILE AS $$ BEGIN PERFORM pg_catalog.nextval('{s}.preflight_sentinel'); RETURN true; END $$; \
             CREATE OPERATOR {s}.=== (LEFTARG=integer, RIGHTARG=integer, FUNCTION={s}.preflight_operator); \
             CREATE FUNCTION {s}.preflight_immutable(integer,integer) RETURNS boolean \
               LANGUAGE sql IMMUTABLE AS 'SELECT $1 = $2'; \
             CREATE OPERATOR {s}.#=# (LEFTARG=integer, RIGHTARG=integer, FUNCTION={s}.preflight_immutable); \
             CREATE OPERATOR CLASS {s}.preflight_ops FOR TYPE integer USING btree AS \
               OPERATOR 1 pg_catalog.< (integer,integer), OPERATOR 2 pg_catalog.<= (integer,integer), \
               OPERATOR 3 pg_catalog.= (integer,integer), OPERATOR 4 pg_catalog.>= (integer,integer), \
               OPERATOR 5 pg_catalog.> (integer,integer), FUNCTION 1 pg_catalog.btint4cmp(integer,integer); \
             CREATE FUNCTION {s}.preflight_view() RETURNS text \
               LANGUAGE plpgsql VOLATILE AS $$ BEGIN PERFORM pg_catalog.nextval('{s}.preflight_sentinel'); RETURN 'hostile'; END $$"
        ),
    )
    .await;

    // Prove the nontransactional sentinel detects each hostile callable, then
    // reset it before testing the verifier.
    for expression in [
        format!("('hostile'::{s}.preflight_state)::text"),
        format!("('hostile'::{s}.preflight_state)::integer"),
        format!("{s}.preflight_operator(0,0)"),
        format!("{s}.preflight_view()"),
    ] {
        sqlx::query(&format!("SELECT {expression}"))
            .execute(&fixture.admin)
            .await
            .expect("sentinel positive control");
        let called: bool =
            sqlx::query_scalar(&format!("SELECT is_called FROM {s}.preflight_sentinel"))
                .fetch_one(&fixture.admin)
                .await
                .expect("sentinel positive-control state");
        assert!(called, "sentinel did not detect {expression}");
        sqlx::query(&format!(
            "SELECT pg_catalog.setval('{s}.preflight_sentinel',1,false)"
        ))
        .execute(&fixture.admin)
        .await
        .expect("reset sentinel");
    }

    // The table shape is valid, but a malformed owner can still put too many
    // rows or a long key in the manifest. The verifier must bound the read.
    for extra_rows in [false, true] {
        if extra_rows {
            sqlx::query(&format!(
                "INSERT INTO {s}.ingress_epoch_guard_manifest (table_name) \
                 SELECT 'preflight_extra_' || value::text \
                 FROM pg_catalog.generate_series(1, 20) AS series(value)"
            ))
            .execute(&fixture.admin)
            .await
            .expect("insert excess manifest rows");
        } else {
            execute(
                &fixture.admin,
                &format!(
                    "ALTER TABLE {s}.ingress_epoch_guard_manifest DISABLE TRIGGER ingress_epoch_guard_manifest_append_only_row"
                ),
            )
            .await;
            sqlx::query(&format!(
                "UPDATE {s}.ingress_epoch_guard_manifest SET table_name = $1 WHERE table_name = 'ingress_messages'"
            ))
            .bind("x".repeat(2048))
            .execute(&fixture.admin)
            .await
            .expect("replace one manifest key with a long value");
            execute(
                &fixture.admin,
                &format!(
                    "ALTER TABLE {s}.ingress_epoch_guard_manifest ENABLE ALWAYS TRIGGER ingress_epoch_guard_manifest_append_only_row"
                ),
            )
            .await;
        }
        let rejected = pool
            .inspect_restricted_ingress_profile(api, epoch_oid)
            .await;
        assert!(
            matches!(
                rejected,
                Err(RestrictedIngressProfileError::Mismatch(
                    "guard manifest rows"
                ))
            ),
            "malformed manifest contents were not refused: {rejected:?}"
        );
        assert_sentinel_not_called(fixture).await;
        let restore = if extra_rows {
            format!(
                "DELETE FROM {s}.ingress_epoch_guard_manifest WHERE table_name LIKE 'preflight_extra_%'"
            )
        } else {
            format!(
                "UPDATE {s}.ingress_epoch_guard_manifest SET table_name = 'ingress_messages' WHERE pg_catalog.length(table_name) > 100"
            )
        };
        execute(&fixture.admin, &format!(
            "ALTER TABLE {s}.ingress_epoch_guard_manifest DISABLE TRIGGER ingress_epoch_guard_manifest_append_only_row; \
             {restore}; \
             ALTER TABLE {s}.ingress_epoch_guard_manifest ENABLE ALWAYS TRIGGER ingress_epoch_guard_manifest_append_only_row"
        )).await;
        assert_pool_restored(fixture, pool, api, epoch_oid, backend_pid).await;
    }

    // A replacement heap can have SELECT grants yet invoke a user cast when
    // the old verifier's manifest query coerces its enum column to text.
    execute(
        &fixture.admin,
        &format!(
            "ALTER TABLE {s}.ingress_epoch_guard_manifest RENAME TO preflight_parked_manifest; \
         CREATE TABLE {s}.ingress_epoch_guard_manifest (table_name {s}.preflight_state); \
         INSERT INTO {s}.ingress_epoch_guard_manifest VALUES ('hostile'); \
         GRANT SELECT ON {s}.ingress_epoch_guard_manifest TO {r}"
        ),
    )
    .await;
    let rejected = pool
        .inspect_restricted_ingress_profile(api, epoch_oid)
        .await;
    assert!(
        matches!(
            rejected,
            Err(RestrictedIngressProfileError::Mismatch(
                "column/trigger preflight"
            ))
        ),
        "replacement type was not rejected by the column preflight: {rejected:?}"
    );
    assert_sentinel_not_called(fixture).await;
    execute(&fixture.admin, &format!(
        "DROP TABLE {s}.ingress_epoch_guard_manifest; ALTER TABLE {s}.preflight_parked_manifest RENAME TO ingress_epoch_guard_manifest"
    )).await;
    assert_pool_restored(fixture, pool, api, epoch_oid, backend_pid).await;

    let hostile_expressions = [
        (
            format!("ALTER TABLE {s}.pending_delivery ALTER COLUMN claim_offered SET DEFAULT ('hostile'::{s}.preflight_state)::{s}.preflight_state::integer"),
            format!("ALTER TABLE {s}.pending_delivery ALTER COLUMN claim_offered SET DEFAULT 0"),
        ),
        (
            format!("ALTER TABLE {s}.pending_delivery ADD CONSTRAINT preflight_enum CHECK (('hostile'::{s}.preflight_state) = ('hostile'::{s}.preflight_state)) NOT VALID"),
            format!("ALTER TABLE {s}.pending_delivery DROP CONSTRAINT preflight_enum"),
        ),
        (
            format!("ALTER TABLE {s}.pending_delivery ADD CONSTRAINT preflight_operator CHECK (claim_offered OPERATOR({s}.===) 0) NOT VALID"),
            format!("ALTER TABLE {s}.pending_delivery DROP CONSTRAINT preflight_operator"),
        ),
        (
            format!("CREATE INDEX preflight_operator_index ON {s}.pending_delivery (claim_offered) WHERE claim_offered OPERATOR({s}.#=#) 0"),
            format!("DROP INDEX {s}.preflight_operator_index"),
        ),
        (
            format!("CREATE INDEX preflight_opclass_index ON {s}.pending_delivery (claim_offered {s}.preflight_ops)"),
            format!("DROP INDEX {s}.preflight_opclass_index"),
        ),
        (
            format!("CREATE INDEX preflight_hash_index ON {s}.pending_delivery USING hash (claim_offered)"),
            format!("DROP INDEX {s}.preflight_hash_index"),
        ),
        (
            format!("CREATE INDEX preflight_collation_index ON {s}.pending_delivery (recipient_jid COLLATE pg_catalog.\"C\")"),
            format!("DROP INDEX {s}.preflight_collation_index"),
        ),
    ];
    for (mutation, restore) in hostile_expressions {
        execute(&fixture.admin, &mutation).await;
        assert!(
            matches!(
                pool.inspect_restricted_ingress_profile(api, epoch_oid)
                    .await,
                Err(RestrictedIngressProfileError::Mismatch(
                    "expression dependency preflight"
                ))
            ),
            "unsafe expression reached a later verifier: {mutation}"
        );
        assert_sentinel_not_called(fixture).await;
        execute(&fixture.admin, &restore).await;
        assert_pool_restored(fixture, pool, api, epoch_oid, backend_pid).await;
    }

    execute(
        &fixture.admin,
        &format!(
            "CREATE TRIGGER preflight_when BEFORE UPDATE ON {s}.pending_delivery \
         FOR EACH ROW WHEN ({s}.preflight_operator(OLD.claim_offered,NEW.claim_offered)) \
         EXECUTE FUNCTION {s}.pending_notification_recovery_ordinal_immutable()"
        ),
    )
    .await;
    assert!(matches!(
        pool.inspect_restricted_ingress_profile(api, epoch_oid)
            .await,
        Err(RestrictedIngressProfileError::Mismatch(
            "trigger expression preflight"
        ))
    ));
    assert_sentinel_not_called(fixture).await;
    execute(
        &fixture.admin,
        &format!("DROP TRIGGER preflight_when ON {s}.pending_delivery"),
    )
    .await;
    assert_pool_restored(fixture, pool, api, epoch_oid, backend_pid).await;

    execute(
        &fixture.admin,
        &format!(
        "CREATE TABLE {s}.preflight_manifest_child () INHERITS ({s}.ingress_epoch_guard_manifest)"
    ),
    )
    .await;
    assert!(pool
        .inspect_restricted_ingress_profile(api, epoch_oid)
        .await
        .is_err());
    assert_sentinel_not_called(fixture).await;
    execute(
        &fixture.admin,
        &format!("DROP TABLE {s}.preflight_manifest_child"),
    )
    .await;
    assert_pool_restored(fixture, pool, api, epoch_oid, backend_pid).await;

    // A view must be rejected before PostgreSQL can execute its SELECT list.
    execute(&fixture.admin, &format!(
        "ALTER TABLE {s}.ingress_epoch_guard_manifest RENAME TO preflight_parked_manifest; \
         CREATE VIEW {s}.ingress_epoch_guard_manifest AS SELECT {s}.preflight_view() AS table_name; \
         GRANT SELECT ON {s}.ingress_epoch_guard_manifest TO {r}"
    )).await;
    assert!(pool
        .inspect_restricted_ingress_profile(api, epoch_oid)
        .await
        .is_err());
    assert_sentinel_not_called(fixture).await;
    execute(&fixture.admin, &format!(
        "DROP VIEW {s}.ingress_epoch_guard_manifest; ALTER TABLE {s}.preflight_parked_manifest RENAME TO ingress_epoch_guard_manifest"
    )).await;
    assert_pool_restored(fixture, pool, api, epoch_oid, backend_pid).await;

    replacement_after_lock_wait(fixture, pool, api, epoch_oid, false).await;
    assert_sentinel_not_called(fixture).await;
    assert_pool_restored(fixture, pool, api, epoch_oid, backend_pid).await;
    replacement_after_lock_wait(fixture, pool, api, epoch_oid, true).await;
    assert_sentinel_not_called(fixture).await;
    assert_pool_restored(fixture, pool, api, epoch_oid, backend_pid).await;
    execute(&fixture.admin, &format!(
        "DROP OPERATOR CLASS {s}.preflight_ops USING btree; \
         DROP OPERATOR {s}.=== (integer,integer); DROP OPERATOR {s}.#=# (integer,integer); \
         DROP CAST ({s}.preflight_state AS text); DROP CAST ({s}.preflight_state AS integer); \
         DROP FUNCTION {s}.preflight_text({s}.preflight_state), {s}.preflight_integer({s}.preflight_state), \
           {s}.preflight_operator(integer,integer), {s}.preflight_immutable(integer,integer), {s}.preflight_view(); \
         DROP TYPE {s}.preflight_state; DROP SEQUENCE {s}.preflight_sentinel"
    )).await;
}

async fn replacement_after_lock_wait(
    fixture: &Fixture,
    pool: &AttestedPgPool,
    api: &PgRoleName,
    epoch_oid: u32,
    replace_with_view: bool,
) {
    let s = &fixture.target;
    let r = &fixture.runtime;
    let manifest_oid: sqlx::postgres::types::Oid = sqlx::query_scalar(
        "SELECT c.oid FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
         WHERE n.nspname=$1 AND c.relname='ingress_epoch_guard_manifest'",
    )
    .bind(s)
    .fetch_one(&fixture.admin)
    .await
    .expect("manifest identity");
    let mut blocker = fixture.admin.begin().await.expect("blocker transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}.ingress_epoch_guard_manifest IN ACCESS EXCLUSIVE MODE"
    ))
    .execute(&mut *blocker)
    .await
    .expect("hold manifest lock");

    let inspect = pool.inspect_restricted_ingress_profile(api, epoch_oid);
    let replacement = async {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
                     JOIN pg_catalog.pg_stat_activity a ON a.pid=l.pid \
                     WHERE a.usename=$1 AND l.relation=$2 AND l.mode='AccessShareLock' AND NOT l.granted)",
                )
                .bind(r)
                .bind(manifest_oid)
                .fetch_one(&fixture.admin)
                .await
                .expect("find waiting manifest lock");
                if waiting { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("observer reached manifest lock after catalog preflight");
        let replacement = if replace_with_view {
            format!("CREATE VIEW {s}.ingress_epoch_guard_manifest AS SELECT {s}.preflight_view() AS table_name")
        } else {
            format!("CREATE TABLE {s}.ingress_epoch_guard_manifest (LIKE {s}.preflight_parked_manifest INCLUDING ALL)")
        };
        sqlx::raw_sql(&format!(
            "ALTER TABLE {s}.ingress_epoch_guard_manifest RENAME TO preflight_parked_manifest; \
             {replacement}; GRANT SELECT ON {s}.ingress_epoch_guard_manifest TO {r}"
        ))
        .execute(&mut *blocker)
        .await
        .expect("replace after observer starts waiting");
        blocker.commit().await.expect("publish replacement");
    };
    let (result, ()) = tokio::join!(inspect, replacement);
    if replace_with_view {
        assert!(result.is_err(), "accepted view after lock wait: {result:?}");
    } else {
        assert!(
            matches!(
                result,
                Err(RestrictedIngressProfileError::Mismatch(
                    "post-lock catalog identities"
                ))
            ),
            "replacement heap was not rejected by the post-wait OID check: {result:?}"
        );
    }
    let kind = if replace_with_view { "VIEW" } else { "TABLE" };
    execute(&fixture.admin, &format!(
        "DROP {kind} {s}.ingress_epoch_guard_manifest; ALTER TABLE {s}.preflight_parked_manifest RENAME TO ingress_epoch_guard_manifest"
    )).await;
}

async fn assert_sentinel_not_called(fixture: &Fixture) {
    let called: bool = sqlx::query_scalar(&format!(
        "SELECT is_called FROM {}.preflight_sentinel",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("sentinel state");
    assert!(!called, "hostile sentinel executed");
}

async fn assert_pool_restored(
    fixture: &Fixture,
    pool: &AttestedPgPool,
    api: &PgRoleName,
    epoch_oid: u32,
    expected_pid: i32,
) {
    pool.inspect_restricted_ingress_profile(api, epoch_oid)
        .await
        .expect("restored profile and reusable pool connection");
    let mut connection = pool
        .acquire_for_test()
        .await
        .expect("pool lease after rejection");
    let (path, session, current, pid, read_only): (String, String, String, i32, String) = sqlx::query_as(
        "SELECT pg_catalog.current_setting('search_path'), session_user::text, current_user::text, \
         pg_catalog.pg_backend_pid(), pg_catalog.current_setting('transaction_read_only')",
    )
    .fetch_one(&mut *connection)
    .await
    .expect("clean pooled session");
    assert_eq!(path, format!("\"{}\", pg_temp", fixture.target));
    assert_eq!(session, fixture.runtime);
    assert_eq!(current, fixture.runtime);
    assert_eq!(
        pid, expected_pid,
        "reused the same physical pooled connection"
    );
    assert_eq!(
        read_only, "off",
        "failed observer transaction was rolled back"
    );
}
