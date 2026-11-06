//! Unregistered GC experiment. Custody writers are quiescent for this lab.
use super::*;

const FK_TRANSITION: &str =
    include_str!("../../src/db/migrations/owner/ingress_gc_fk_transition_v1_candidate.sql");
const GC_API: &str = include_str!("../../src/db/migrations/owner/ingress_gc_api_v1_candidate.sql");
const SOURCE: &str = "00000000-0000-0000-0000-000000000001";
const CHILD: &str = "00000000-0000-0000-0000-000000000002";
const STANDALONE: &str = "00000000-0000-0000-0000-000000000031";
const NULL_TAIL: &str = "00000000-0000-0000-0000-000000000011";
const MISSING_RECEIPT: &str = "00000000-0000-0000-0000-000000000012";
const UNSETTLED: &str = "00000000-0000-0000-0000-000000000013";
const PENDING_SM: &str = "00000000-0000-0000-0000-000000000014";
const PENDING_DELIVERY: &str = "00000000-0000-0000-0000-000000000015";
const SM_REF: &str = "00000000-0000-0000-0000-000000000016";
const OWNER_PENDING: &str = "00000000-0000-0000-0000-000000000017";

fn render(sql: &str, api_owner: &str, runtime: &str) -> String {
    sql.replace("__SCHEMA_PATH__", "\"guarded\"")
        .replace("__SCHEMA__", "\"guarded\"")
        .replace("__API_OWNER_LITERAL__", &api_owner.replace('\'', "''"))
        .replace("__RUNTIME_ROLE_LITERAL__", &runtime.replace('\'', "''"))
        .replace("__API_OWNER_IDENT__", &quote_identifier(api_owner))
        .replace("__RUNTIME_ROLE_IDENT__", &quote_identifier(runtime))
}

async fn sql(connection: &mut PgConnection, statement: &str) {
    sqlx::raw_sql(statement)
        .execute(connection)
        .await
        .unwrap_or_else(|error| panic!("SQL failed: {error}\n{statement}"));
}

async fn gc(connection: &mut PgConnection, key: &str) -> Result<String, sqlx::Error> {
    sqlx::query_scalar("SELECT guarded.waddle_ingress_gc_candidate_v1($1::uuid)")
        .bind(key)
        .fetch_one(connection)
        .await
}

async fn gc_tx(connection: &mut PgConnection, key: &str) -> String {
    begin(connection).await;
    writer_lock(connection).await;
    let result = gc(connection, key).await.expect("candidate result");
    commit(connection).await;
    result
}

async fn seed(owner: &mut PgConnection) {
    sql(
        owner,
        "CREATE TABLE guarded.pending_delivery (row_id text PRIMARY KEY)",
    )
    .await;
    begin(owner).await;
    writer_lock(owner).await;
    proof(owner, 0).await;
    sql(owner, r#"
      INSERT INTO guarded.ingress_messages
        (message_key, digest_version, digest, source_binding_version, source_bare_jid,
         source_auth_context_id, source_auth_context_version, source_auth_epoch)
      VALUES ('00000000-0000-0000-0000-000000000001', 1, decode(repeat('01',32),'hex'),
              1, 'source@example.test', '00000000-0000-0000-0000-000000000003', 1, 1),
             ('00000000-0000-0000-0000-000000000017', 1, decode(repeat('17',32),'hex'),
              1, 'source@example.test', '00000000-0000-0000-0000-000000000003', 1, 1),
             ('00000000-0000-0000-0000-000000000021', 1, decode(repeat('21',32),'hex'),
              1, 'source@example.test', '00000000-0000-0000-0000-000000000003', 1, 1);
      INSERT INTO guarded.ingress_messages (message_key,digest_version,digest)
      SELECT key::uuid, 1, decode(repeat('02',32),'hex') FROM unnest(ARRAY[
        '00000000-0000-0000-0000-000000000002',
        '00000000-0000-0000-0000-000000000011',
        '00000000-0000-0000-0000-000000000012',
        '00000000-0000-0000-0000-000000000013',
        '00000000-0000-0000-0000-000000000014',
        '00000000-0000-0000-0000-000000000015',
        '00000000-0000-0000-0000-000000000016',
        '00000000-0000-0000-0000-000000000022',
        '00000000-0000-0000-0000-000000000031']) AS key;
      UPDATE guarded.ingress_messages SET owner_decision=0 WHERE message_key IN
        ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000017','00000000-0000-0000-0000-000000000021');
      INSERT INTO guarded.ingress_owner_rejections VALUES
        ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002'),
        ('00000000-0000-0000-0000-000000000021','00000000-0000-0000-0000-000000000022');
      UPDATE guarded.ingress_messages SET owner_decision=2 WHERE message_key IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000021');
      UPDATE guarded.ingress_messages SET terminal_at='2000-01-01',retention_eligible_at='2000-01-01';
      UPDATE guarded.ingress_messages SET retention_eligible_at=NULL WHERE message_key='00000000-0000-0000-0000-000000000011';
      INSERT INTO guarded.ingress_effect_intents (message_key,effect_ordinal,kind,semantic_identity_hash,payload_version,payload)
      SELECT message_key,0,CASE WHEN message_key='00000000-0000-0000-0000-000000000017' THEN 12 ELSE 1 END,
             decode(repeat('04',32),'hex'),1,''::bytea FROM guarded.ingress_messages
      WHERE message_key NOT IN ('00000000-0000-0000-0000-000000000011', '00000000-0000-0000-0000-000000000021', '00000000-0000-0000-0000-000000000022');
      INSERT INTO guarded.ingress_effect_receipts (message_key,kind,semantic_identity_hash)
      SELECT message_key,kind,semantic_identity_hash FROM guarded.ingress_effect_intents
      WHERE message_key <> '00000000-0000-0000-0000-000000000012';
      INSERT INTO guarded.ingress_carbon_receipts SELECT message_key,kind,semantic_identity_hash,'recipient'
      FROM guarded.ingress_effect_intents WHERE message_key IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002');
      INSERT INTO guarded.ingress_delivery_receipts (message_key,kind,semantic_identity_hash,resource)
      SELECT message_key,kind,semantic_identity_hash,'resource' FROM guarded.ingress_effect_intents
      WHERE message_key IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002');
      INSERT INTO guarded.ingress_effect_descendants (message_key,kind,semantic_identity_hash,descendant_key,settled_at)
      SELECT message_key,kind,semantic_identity_hash,'descendant',CASE WHEN message_key='00000000-0000-0000-0000-000000000013' THEN NULL ELSE '2000-01-01'::timestamptz END
      FROM guarded.ingress_effect_intents WHERE message_key IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000013');
      INSERT INTO guarded.ingress_send_attempts (message_key,kind,semantic_identity_hash,recipient,node_id,node_incarnation,lease_token,expires_at_ms,state)
      SELECT message_key,kind,semantic_identity_hash,'recipient','node','inc','token',1,2 FROM guarded.ingress_effect_intents
      WHERE message_key IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002');
      INSERT INTO guarded.ingress_archive_dispatch (archive_jid,archive_seq,message_key,kind,semantic_identity_hash,resource,pending_row_id)
      SELECT 'archive',1,message_key,kind,semantic_identity_hash,
             CASE WHEN message_key='00000000-0000-0000-0000-000000000015' THEN '' ELSE 'resource' END,
             CASE WHEN message_key='00000000-0000-0000-0000-000000000015' THEN 'pending' ELSE '' END
      FROM guarded.ingress_effect_intents WHERE message_key IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000015');
      INSERT INTO guarded.ingress_origin_aliases (alias_key_hash,sender_bare_jid,target_kind,target_jid,origin_id,message_key)
      SELECT decode(repeat(replace(message_key::text,'-',''),2),'hex'),'source@example.test',1,'target@example.test','origin-' || message_key::text,message_key
      FROM guarded.ingress_messages WHERE message_key IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000031');
      INSERT INTO guarded.ingress_deliveries (delivery_key,message_key)
      SELECT message_key,message_key FROM guarded.ingress_messages
      WHERE message_key IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000031');
      INSERT INTO guarded.sm_ingress_appends (message_key,receipt_kind,semantic_identity_hash,resource,accepting_stream_id,sequence,appended_at_ms,custody_payload,original_receipt_at_ms,disposition)
      SELECT message_key::text,1,decode(repeat('04',32),'hex'),'settled','stream',1,1,'payload',1,1
      FROM guarded.ingress_messages WHERE message_key IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002','00000000-0000-0000-0000-000000000031');
      INSERT INTO guarded.pending_delivery VALUES ('pending');
      INSERT INTO guarded.muc_invite_claims SELECT message_key,'room','invitee','inviter',1 FROM guarded.ingress_messages
      WHERE message_key IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002');
      INSERT INTO guarded.sm_ingress_appends (message_key,receipt_kind,semantic_identity_hash,resource,accepting_stream_id,sequence,appended_at_ms,custody_payload,original_receipt_at_ms,disposition)
      VALUES ('00000000-0000-0000-0000-000000000014',1,decode(repeat('04',32),'hex'),'resource','stream',1,1,'payload',1,0);
      INSERT INTO guarded.ingress_sm_refs (sm_ingress_id,ingress_ordinal,message_key,wire_h,wire_generation)
      VALUES ('00000000-0000-0000-0000-000000000099',1,'00000000-0000-0000-0000-000000000016',1,0);
    "#).await;
    commit(owner).await;
}

#[tokio::test]
async fn quiescent_gc_v1_catalog_roles_retention_and_rollback() {
    let Some(mut fixture) = Fixture::new().await else {
        return;
    };
    let mut admin = fixture.observer().await;
    let version: i32 =
        sqlx::query_scalar("SELECT pg_catalog.current_setting('server_version_num')::integer")
            .fetch_one(&mut admin)
            .await
            .expect("version");
    assert!(
        (170_000..180_000).contains(&version),
        "pilot requires PostgreSQL 17"
    );
    let api = format!("guard_gc_api_{}", uuid::Uuid::new_v4().simple());
    fixture.extra_roles.push(api.clone());
    sqlx::query(&format!("CREATE ROLE {api} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT"))
        .execute(&fixture.admin).await.expect("API role");
    let mut owner = fixture.owner().await;
    seed(&mut owner).await;

    // Every populated FK is checked and replaced together with ACL and API.
    begin(&mut admin).await;
    sql(&mut admin, &render(FK_TRANSITION, &api, &fixture.runtime)).await;
    for table in TABLES {
        sql(&mut admin, &format!("REVOKE ALL PRIVILEGES ON guarded.{table} FROM {}; GRANT SELECT, INSERT, UPDATE, DELETE ON guarded.{table} TO {api}", fixture.runtime)).await;
    }
    sql(&mut admin, &format!("GRANT USAGE, CREATE ON SCHEMA guarded TO {api}; GRANT SELECT ON guarded.ingress_protocol_epoch, guarded.sessions, guarded.pending_delivery TO {api}; GRANT UPDATE (id) ON guarded.ingress_protocol_epoch, guarded.sessions TO {api};")).await;
    sql(
        &mut admin,
        &render_definer_api_candidate("guarded", &api, &fixture.runtime),
    )
    .await;
    sql(&mut admin, &render(GC_API, &api, &fixture.runtime)).await;
    sql(
        &mut admin,
        &format!("REVOKE CREATE ON SCHEMA guarded FROM {api}"),
    )
    .await;
    commit(&mut admin).await;
    assert_no_guarded_dml(&mut admin, &fixture.runtime).await;

    let fks: Vec<(String, String, bool, bool, bool)> = sqlx::query_as(
      "SELECT c.conname::text, c.confdeltype::text, c.condeferrable, c.condeferred, c.convalidated FROM pg_catalog.pg_constraint c JOIN pg_catalog.pg_class p ON p.oid=c.confrelid WHERE c.contype='f' AND p.relnamespace='guarded'::regnamespace AND p.relname IN ('ingress_messages','ingress_effect_intents') ORDER BY c.conname")
      .fetch_all(&mut admin).await.expect("transitioned FK catalog");
    assert_eq!(fks.len(), 13);
    for (name, action, deferrable, deferred, validated) in &fks {
        assert!(*validated && !*deferred, "{name}");
        match name.as_str() {
            "waddle_ingress_owner_rejections_child_gc_v1_fkey" => {
                assert_eq!((action.as_str(), *deferrable), ("a", true))
            }
            "ingress_owner_rejections_source_message_key_fkey" => {
                assert_eq!((action.as_str(), *deferrable), ("r", false))
            }
            "muc_invite_claims_message_key_fkey" => {
                assert_eq!((action.as_str(), *deferrable), ("c", false))
            }
            _ => assert_eq!((action.as_str(), *deferrable), ("a", false), "{name}"),
        }
    }
    let mut runtime = fixture.runtime().await;
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    forged_proof(&mut runtime).await;
    assert!(
        sqlx::query("DELETE FROM guarded.ingress_messages WHERE false")
            .execute(&mut runtime)
            .await
            .is_err()
    );
    sql(&mut runtime, "ROLLBACK").await;
    assert!(sqlx::query(&format!("SET ROLE {api}"))
        .execute(&mut runtime)
        .await
        .is_err());
    assert!(sqlx::query("CREATE TEMP TABLE gc_shadow (id int)")
        .execute(&mut runtime)
        .await
        .is_err());

    // A pin can exist without an FK and without any other custody barrier.
    // Installing either experimental schema after transition must still make
    // every invocation fail before touching even an expired source pair.
    for relation in ["ingress_admission_pins", "direct_admission_lab"] {
        sql(&mut admin, &format!("CREATE TABLE guarded.{relation} (message_key uuid NOT NULL); INSERT INTO guarded.{relation} VALUES ('{SOURCE}'::uuid), ('{NULL_TAIL}'::uuid)")).await;
        for key in [SOURCE, NULL_TAIL] {
            begin(&mut runtime).await;
            writer_lock(&mut runtime).await;
            let error = gc(&mut runtime, key)
                .await
                .expect_err("unsupported admission custody must fail closed");
            assert!(
                error
                    .to_string()
                    .contains("unsupported admission custody relation"),
                "{relation}: {error}"
            );
            sql(&mut runtime, "ROLLBACK").await;
        }
        let intact: i64 = sqlx::query_scalar("SELECT count(*) FROM guarded.ingress_messages WHERE message_key IN ($1::uuid, $2::uuid)")
            .bind(SOURCE).bind(CHILD).fetch_one(&mut admin).await.expect("retained pinned pair");
        assert_eq!(intact, 2);
        let still_unset: bool = sqlx::query_scalar("SELECT retention_eligible_at IS NULL FROM guarded.ingress_messages WHERE message_key=$1::uuid")
            .bind(NULL_TAIL).fetch_one(&mut admin).await.expect("pin blocks frontier adoption");
        assert!(still_unset);
        sql(&mut admin, &format!("DROP TABLE guarded.{relation}")).await;
    }

    // A table-owner SECURITY DEFINER trigger cannot impersonate the API owner.
    sql(&mut admin, &format!(r#"
      CREATE TABLE guarded.gc_hostile (id int);
      ALTER TABLE guarded.gc_hostile OWNER TO {owner};
      CREATE FUNCTION guarded.gc_hostile_trigger() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
      SET search_path = pg_catalog, pg_temp AS $f$ BEGIN DELETE FROM guarded.ingress_messages WHERE false; RETURN NEW; END $f$;
      ALTER FUNCTION guarded.gc_hostile_trigger() OWNER TO {owner};
      CREATE TRIGGER hostile BEFORE INSERT ON guarded.gc_hostile FOR EACH ROW EXECUTE FUNCTION guarded.gc_hostile_trigger();
      GRANT INSERT ON guarded.gc_hostile TO {runtime};
    "#, owner=fixture.owner, runtime=fixture.runtime)).await;
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    forged_proof(&mut runtime).await;
    assert!(sqlx::query("INSERT INTO guarded.gc_hostile VALUES (1)")
        .execute(&mut runtime)
        .await
        .is_err());
    sql(&mut runtime, "ROLLBACK").await;

    for key in [
        MISSING_RECEIPT,
        UNSETTLED,
        PENDING_SM,
        PENDING_DELIVERY,
        SM_REF,
        OWNER_PENDING,
        CHILD,
    ] {
        assert_eq!(gc_tx(&mut runtime, key).await, "retained", "{key}");
    }
    assert_eq!(gc_tx(&mut runtime, NULL_TAIL).await, "adopted");
    let adopted: bool = sqlx::query_scalar("SELECT retention_eligible_at BETWEEN clock_timestamp() - interval '1 minute' AND clock_timestamp() FROM guarded.ingress_messages WHERE message_key=$1::uuid")
        .bind(NULL_TAIL).fetch_one(&mut admin).await.expect("database clock");
    assert!(adopted);
    assert_eq!(gc_tx(&mut runtime, NULL_TAIL).await, "skipped");

    assert_eq!(gc_tx(&mut runtime, STANDALONE).await, "deleted_one");
    for table in [
        "ingress_messages",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_origin_aliases",
        "ingress_deliveries",
    ] {
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM guarded.{table} WHERE message_key=$1::uuid"
        ))
        .bind(STANDALONE)
        .fetch_one(&mut admin)
        .await
        .expect("standalone graph removal");
        assert_eq!(count, 0, "{table}");
    }
    let settled: i64 =
        sqlx::query_scalar("SELECT count(*) FROM guarded.sm_ingress_appends WHERE message_key=$1")
            .bind(STANDALONE)
            .fetch_one(&mut admin)
            .await
            .expect("settled standalone append removal");
    assert_eq!(settled, 0);

    // Both canonical locks use SKIP LOCKED; the epoch-exclusive transition
    // waits behind a runtime prelock and is bounded by lock_timeout.
    for key in [SOURCE, CHILD] {
        begin(&mut admin).await;
        sqlx::query("SELECT 1 FROM guarded.ingress_messages WHERE message_key=$1::uuid FOR UPDATE")
            .bind(key)
            .execute(&mut admin)
            .await
            .expect("hold canonical row");
        assert_eq!(gc_tx(&mut runtime, SOURCE).await, "skipped");
        sql(&mut admin, "ROLLBACK").await;
    }
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    begin(&mut admin).await;
    sql(&mut admin, "SET LOCAL lock_timeout='100ms'").await;
    let lock_error =
        sqlx::query("LOCK TABLE ONLY guarded.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE")
            .execute(&mut admin)
            .await
            .expect_err("transition must wait behind epoch prelock");
    assert_eq!(
        lock_error
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("55P03")
    );
    sql(&mut admin, "ROLLBACK").await;
    commit(&mut runtime).await;

    // Test-only definer attempts an invalid child-only delete after GC. Even
    // if the caller deferred the named constraint before entering the API,
    // the API must force it IMMEDIATE before returning.
    sql(
        &mut admin,
        &format!(
            r#"CREATE FUNCTION guarded.gc_test_remove_child(uuid) RETURNS void
      LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
      DELETE FROM guarded.ingress_messages WHERE message_key=$1 $f$;
      ALTER FUNCTION guarded.gc_test_remove_child(uuid) OWNER TO {api};
      REVOKE ALL ON FUNCTION guarded.gc_test_remove_child(uuid) FROM PUBLIC;
      GRANT EXECUTE ON FUNCTION guarded.gc_test_remove_child(uuid) TO {runtime};"#,
            runtime = fixture.runtime
        ),
    )
    .await;
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    sql(
        &mut runtime,
        "SET CONSTRAINTS guarded.waddle_ingress_owner_rejections_child_gc_v1_fkey DEFERRED",
    )
    .await;
    assert_eq!(
        gc(&mut runtime, SOURCE)
            .await
            .expect("collect within rollback probe"),
        "deleted_pair"
    );
    let forced = sqlx::query(
        "SELECT guarded.gc_test_remove_child('00000000-0000-0000-0000-000000000022'::uuid)",
    )
    .execute(&mut runtime)
    .await
    .expect_err("API must restore the child FK to immediate");
    assert_eq!(
        forced.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23503")
    );
    assert_eq!(
        forced.as_database_error().and_then(|e| e.constraint()),
        Some("waddle_ingress_owner_rejections_child_gc_v1_fkey")
    );
    sql(&mut runtime, "ROLLBACK").await;
    sql(
        &mut admin,
        "DROP FUNCTION guarded.gc_test_remove_child(uuid)",
    )
    .await;

    sql(&mut admin, r#"CREATE FUNCTION guarded.gc_fail() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN RAISE EXCEPTION 'injected'; END $f$;
      CREATE TRIGGER gc_fail BEFORE DELETE ON guarded.ingress_effect_receipts FOR EACH STATEMENT EXECUTE FUNCTION guarded.gc_fail();"#).await;
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    assert!(gc(&mut runtime, SOURCE).await.is_err());
    sql(&mut runtime, "ROLLBACK").await;
    let graph_count: i64 = sqlx::query_scalar("SELECT count(*) FROM guarded.ingress_archive_dispatch WHERE message_key IN ($1::uuid,$2::uuid)")
        .bind(SOURCE).bind(CHILD).fetch_one(&mut admin).await.expect("rolled back leaves");
    assert_eq!(graph_count, 2);
    sql(
        &mut admin,
        "DROP TRIGGER gc_fail ON guarded.ingress_effect_receipts; DROP FUNCTION guarded.gc_fail()",
    )
    .await;
    assert_eq!(gc_tx(&mut runtime, SOURCE).await, "deleted_pair");
    assert_eq!(
        gc_tx(&mut runtime, SOURCE).await,
        "skipped",
        "retry after a committed but lost acknowledgement"
    );
    for table in [
        "ingress_messages",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_carbon_receipts",
        "ingress_delivery_receipts",
        "ingress_effect_descendants",
        "ingress_send_attempts",
        "ingress_archive_dispatch",
        "ingress_origin_aliases",
        "ingress_deliveries",
        "muc_invite_claims",
    ] {
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM guarded.{table} WHERE message_key IN ($1::uuid,$2::uuid)"
        ))
        .bind(SOURCE)
        .bind(CHILD)
        .fetch_one(&mut admin)
        .await
        .expect("verify pair removal");
        assert_eq!(count, 0, "{table}");
    }
    let mappings: i64 = sqlx::query_scalar("SELECT count(*) FROM guarded.ingress_owner_rejections WHERE source_message_key=$1::uuid OR child_message_key=$2::uuid")
        .bind(SOURCE).bind(CHILD).fetch_one(&mut admin).await.expect("verify mapping removal");
    assert_eq!(mappings, 0);
    let settled_pair: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM guarded.sm_ingress_appends WHERE message_key IN ($1, $2)",
    )
    .bind(SOURCE)
    .bind(CHILD)
    .fetch_one(&mut admin)
    .await
    .expect("settled pair appends removed");
    assert_eq!(settled_pair, 0);

    // A stopped writer is a laboratory prerequisite, not a database-enforced
    // fence. Model an already-authorized late append after collection and
    // prove that the unguarded, FK-free custody table still accepts it.
    sql(
        &mut admin,
        &format!(
            "GRANT INSERT ON guarded.sm_ingress_appends TO {}",
            fixture.runtime
        ),
    )
    .await;
    sql(&mut runtime, r#"INSERT INTO guarded.sm_ingress_appends
        (message_key,receipt_kind,semantic_identity_hash,resource,accepting_stream_id,sequence,appended_at_ms,custody_payload,original_receipt_at_ms,disposition)
        VALUES ('00000000-0000-0000-0000-000000000001',1,decode(repeat('05',32),'hex'),'late','stream',1,1,'payload',1,0)"#).await;
    let late: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM guarded.sm_ingress_appends WHERE message_key=$1 AND disposition=0)")
        .bind(SOURCE).fetch_one(&mut admin).await.expect("late custody proof");
    assert!(
        late,
        "a pre-delete check alone cannot fence delayed custody"
    );
    drop((runtime, owner, admin));
    fixture.cleanup().await;
}

#[tokio::test]
async fn gc_v1_transition_rejects_unknown_graph_and_breaks_old_cascade_writer() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut owner = fixture.owner().await;
    let mut runtime = fixture.runtime().await;
    let transition = render(FK_TRANSITION, "unused_api", &fixture.runtime);
    for relation in ["ingress_admission_pins", "direct_admission_lab"] {
        sql(
            &mut owner,
            &format!("CREATE TABLE guarded.{relation} (message_key uuid NOT NULL)"),
        )
        .await;
        begin(&mut owner).await;
        let error = sqlx::raw_sql(&transition)
            .execute(&mut owner)
            .await
            .expect_err("admission schema must fail transition preflight");
        assert!(
            error
                .to_string()
                .contains("unsupported admission custody relation"),
            "{relation}: {error}"
        );
        sql(&mut owner, "ROLLBACK").await;
        sql(&mut owner, &format!("DROP TABLE guarded.{relation}")).await;
    }
    sql(&mut owner, "CREATE TABLE guarded.gc_unexpected (id uuid REFERENCES guarded.ingress_messages(message_key) ON DELETE CASCADE)").await;
    begin(&mut owner).await;
    assert!(
        sqlx::raw_sql(&transition)
            .execute(&mut owner)
            .await
            .is_err(),
        "unknown dependent relation must fail preflight"
    );
    sql(&mut owner, "ROLLBACK").await;
    let original: String = sqlx::query_scalar("SELECT confdeltype::text FROM pg_catalog.pg_constraint WHERE conrelid='guarded.ingress_effect_intents'::regclass AND conname='ingress_effect_intents_message_key_fkey'")
        .fetch_one(&mut owner).await.expect("original FK remains");
    assert_eq!(
        original, "c",
        "failed transition must not partially replace FKs"
    );
    sql(&mut owner, "DROP TABLE guarded.gc_unexpected").await;
    begin(&mut owner).await;
    sql(&mut owner, &transition).await;
    commit(&mut owner).await;
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    sql(&mut runtime, r#"
      INSERT INTO guarded.ingress_messages (message_key,digest_version,digest)
      VALUES ('00000000-0000-0000-0000-000000000030',1,decode(repeat('01',32),'hex'));
      INSERT INTO guarded.ingress_effect_intents (message_key,effect_ordinal,kind,semantic_identity_hash,payload_version,payload)
      VALUES ('00000000-0000-0000-0000-000000000030',0,1,decode(repeat('02',32),'hex'),1,''::bytea);
    "#).await;
    let old_delete = sqlx::query("DELETE FROM guarded.ingress_messages WHERE message_key='00000000-0000-0000-0000-000000000030'")
        .execute(&mut runtime).await.expect_err("old cascade-dependent GC must fail after transition");
    assert_eq!(
        old_delete
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("23503")
    );
    sql(&mut runtime, "ROLLBACK").await;
    drop((owner, runtime));
    fixture.cleanup().await;
}
