//! Dark claims and positive observations; no live delivery or release.

use std::sync::Arc;
use std::time::Duration;

use waddle_server::db::physical_attestation::attested_pool;
use waddle_server::db::{DatabaseConfig, DatabaseDriver, DatabasePool, PoolConfig};
use waddle_server::pending_delivery::bootstrap_pending_delivery_owner_schema;
use waddle_server::test_support::{OrderedClaimError, OrderedClaimLab, OrderedClaimPhase};
use waddle_xmpp::pending_delivery::{PendingRowId, SmSessionId};

use super::Fixture;

fn session() -> SmSessionId {
    SmSessionId::new("ordered-lab-session")
}

fn recipient(name: &str) -> jid::BareJid {
    format!("{name}@example.test")
        .parse()
        .expect("fixture recipient")
}

fn ids(rows: &[waddle_server::test_support::OrderedClaimObservationLab]) -> Vec<&str> {
    rows.iter().map(|row| row.row_id().as_str()).collect()
}

#[tokio::test]
async fn sqlite_claim_and_observation_are_main_qualified_and_single_connection() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
    config.pool_size = 1;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("SQLite pool");
    waddle_server::test_support::initialize_mam_on_sqlite_test_pool(&pool)
        .await
        .expect("owner MAM");
    bootstrap_pending_delivery_owner_schema(pool.global())
        .await
        .expect("owner pending schema");
    let db = pool.global().guard().await.expect("owner connection");
    for (id, stanza, seq) in [
        ("main-first", "stanza-first", 1_i64),
        ("main-second", "stanza-second", 2_i64),
    ] {
        db.execute("INSERT INTO main.mam_messages (id, room_jid, timestamp, from_jid, to_jid, stanza_id, archive_seq) VALUES (?, ?, ?, ?, ?, ?, ?)",
            waddle_server::db_params![id, "alice@example.test", "2020-01-01T00:00:00Z", "sender@example.test", "alice@example.test", stanza, seq]).await.expect("archive row");
    }
    for (id, stanza) in [("z-row", "stanza-first"), ("a-row", "stanza-second")] {
        db.execute("INSERT INTO main.pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id) VALUES (?, ?, ?, ?, ?, ?)",
            waddle_server::db_params![id, "alice@example.test", 1_i64, "archived", "alice@example.test", stanza]).await.expect("pending row");
    }
    db.execute(
        "CREATE TEMP TABLE pending_delivery AS SELECT * FROM main.pending_delivery WHERE 0",
        (),
    )
    .await
    .expect("shadow pending");
    db.execute(
        "INSERT INTO temp.pending_delivery SELECT * FROM main.pending_delivery",
        (),
    )
    .await
    .expect("shape-compatible pending decoys");
    db.execute(
        "CREATE TEMP TABLE mam_messages AS SELECT * FROM main.mam_messages WHERE 0",
        (),
    )
    .await
    .expect("shadow MAM");
    db.execute(
        "INSERT INTO temp.mam_messages SELECT * FROM main.mam_messages",
        (),
    )
    .await
    .expect("shape-compatible archive decoys");
    db.execute("UPDATE temp.mam_messages SET archive_seq = CASE WHEN id = 'main-first' THEN 20 ELSE 10 END", ()).await.expect("opposite archive order");
    drop(db);

    let lab = Arc::new(OrderedClaimLab::from_pool(&pool).await.expect("no-DDL lab"));
    let attempt = lab
        .attempt(recipient("alice"), session(), 2)
        .expect("attempt");
    let duplicate = attempt.clone();
    let (a, b) = tokio::join!(lab.claim(&attempt), lab.claim(&duplicate));
    let (claimed, refused) = match (a, b) {
        (Ok(rows), Err(error)) | (Err(error), Ok(rows)) => (rows, error),
        _ => panic!("exactly one claim must start"),
    };
    assert!(matches!(refused, OrderedClaimError::AttemptAlreadyStarted));
    assert_eq!(
        ids(&claimed),
        ["z-row", "a-row"],
        "main archive order wins over TEMP decoy"
    );
    assert!(claimed
        .iter()
        .all(|row| row.phase() == OrderedClaimPhase::Unoffered));
    let observed = lab.observe(&attempt).await.expect("observe");
    assert_eq!(
        ids(&observed),
        ["a-row", "z-row"],
        "observation is by row id, not original order"
    );
    let db = pool.global().guard().await.expect("inspect decoys");
    let mut shadow = db
        .query(
            "SELECT COUNT(*) FROM temp.pending_delivery WHERE flushed_in_session IS NOT NULL",
            (),
        )
        .await
        .expect("shadow count");
    assert_eq!(
        shadow
            .next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("integer"),
        0
    );
    drop(shadow);
    db.execute(
        "UPDATE main.pending_delivery SET claim_offered = 1 WHERE row_id = 'z-row'",
        (),
    )
    .await
    .expect("offer fixture");
    drop(db);
    let z = PendingRowId::new("z-row");
    assert_eq!(
        lab.observe_row(&attempt, &z)
            .await
            .expect("status")
            .expect("visible")
            .phase(),
        OrderedClaimPhase::Offered
    );
    let db = pool.global().guard().await.expect("mark sequenced");
    db.execute(
        "UPDATE main.pending_delivery SET outbound_sequence = 3 WHERE row_id = 'z-row'",
        (),
    )
    .await
    .expect("sequence fixture");
    drop(db);
    assert_eq!(
        lab.observe_row(&attempt, &z)
            .await
            .expect("status")
            .expect("visible")
            .phase(),
        OrderedClaimPhase::Sequenced { offered: true }
    );
    let db = pool.global().guard().await.expect("corrupt phase");
    db.execute(
        "UPDATE main.pending_delivery SET claim_offered = 2 WHERE row_id = 'z-row'",
        (),
    )
    .await
    .expect("invalid phase");
    drop(db);
    assert!(matches!(
        lab.observe_row(&attempt, &z).await,
        Err(OrderedClaimError::CorruptRow)
    ));
    let db = pool.global().guard().await.expect("restore phase");
    db.execute(
        "UPDATE main.pending_delivery SET claim_offered = ? WHERE row_id = 'z-row'",
        waddle_server::db_params!["x".repeat(100_000)],
    )
    .await
    .expect("oversized corrupt scalar");
    drop(db);
    assert!(matches!(
        lab.observe_row(&attempt, &z).await,
        Err(OrderedClaimError::CorruptRow)
    ));
    let db = pool.global().guard().await.expect("restore phase");
    db.execute(
        "UPDATE main.pending_delivery SET claim_offered = 1 WHERE row_id = 'z-row'",
        (),
    )
    .await
    .expect("restore");
    db.execute("UPDATE main.pending_delivery SET recipient_jid = 'other@example.test' WHERE row_id = 'z-row'", ()).await.expect("change recipient");
    drop(db);
    assert!(lab
        .observe_row(&attempt, &z)
        .await
        .expect("mismatch")
        .is_none());
    let other_lab = OrderedClaimLab::from_pool(&pool)
        .await
        .expect("same pool, distinct authority");
    assert!(matches!(
        other_lab.observe(&attempt).await,
        Err(OrderedClaimError::WrongAuthority)
    ));

    let db = pool.global().guard().await.expect("inconsistent row");
    db.execute("INSERT INTO main.pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, outbound_sequence, claim_offered) VALUES ('dirty-row', 'dirty@example.test', 1, 'transient', 17, 2)", ()).await.expect("seed inconsistent markers");
    drop(db);
    let dirty = lab
        .attempt(recipient("dirty"), session(), 1)
        .expect("dirty attempt");
    assert!(matches!(
        lab.claim(&dirty).await,
        Err(OrderedClaimError::CorruptRow)
    ));
    let db = pool
        .global()
        .guard()
        .await
        .expect("inspect inconsistent row");
    let mut dirty_row = db.query("SELECT outbound_sequence, claim_offered FROM main.pending_delivery WHERE row_id='dirty-row'", ()).await.expect("dirty state");
    let dirty_row = dirty_row.next().await.expect("row").expect("present");
    assert_eq!(
        (
            dirty_row.get::<i64>(0).expect("sequence"),
            dirty_row.get::<i64>(1).expect("offer")
        ),
        (17, 2)
    );
    drop(db);

    let db = pool.global().guard().await.expect("overflow fixture");
    db.execute("UPDATE main.pending_delivery SET recipient_jid = 'alice@example.test', claim_offered = 0, outbound_sequence = NULL WHERE row_id = 'z-row'", ()).await.expect("restore identity");
    db.execute(
        "UPDATE main.pending_delivery SET claim_token='malformed' WHERE row_id='z-row'",
        (),
    )
    .await
    .expect("change token");
    drop(db);
    assert!(lab
        .observe_row(&attempt, &z)
        .await
        .expect("changed token stays unknown")
        .is_none());
    let db = pool.global().guard().await.expect("restore token");
    db.execute("UPDATE main.pending_delivery SET claim_token=(SELECT claim_token FROM main.pending_delivery WHERE row_id='a-row') WHERE row_id='z-row'", ()).await.expect("restore exact token");
    db.execute("INSERT INTO main.pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, flushed_in_session, claim_token, claim_offered) SELECT 'extra-row', recipient_jid, 1, 'transient', flushed_in_session, claim_token, 0 FROM main.pending_delivery WHERE row_id = 'z-row'", ()).await.expect("duplicate attempt identity");
    drop(db);
    assert!(matches!(
        lab.observe(&attempt).await,
        Err(OrderedClaimError::ObservationOverflow)
    ));
    let db = pool
        .global()
        .guard()
        .await
        .expect("oversized identity fixture");
    db.execute(
        "DELETE FROM main.pending_delivery WHERE row_id = 'extra-row'",
        (),
    )
    .await
    .expect("remove overflow");
    let oversized = "x".repeat(129);
    db.execute(
        "UPDATE main.pending_delivery SET row_id = ? WHERE row_id = 'z-row'",
        waddle_server::db_params![oversized.clone()],
    )
    .await
    .expect("oversized row id");
    drop(db);
    assert!(matches!(
        lab.observe(&attempt).await,
        Err(OrderedClaimError::CorruptRow)
    ));
    assert!(matches!(
        lab.observe_row(&attempt, &PendingRowId::new(oversized.clone()))
            .await,
        Err(OrderedClaimError::CorruptRow)
    ));
    let db = pool.global().guard().await.expect("remove oversized row");
    db.execute(
        "DELETE FROM main.pending_delivery WHERE row_id = ?",
        waddle_server::db_params![oversized],
    )
    .await
    .expect("remove");
    drop(db);
    assert!(lab
        .observe_row(&attempt, &z)
        .await
        .expect("removed row")
        .is_none());

    let db = pool.global().guard().await.expect("TEMP trigger");
    db.execute("CREATE TEMP TRIGGER reject_pending AFTER UPDATE ON main.pending_delivery BEGIN SELECT RAISE(ABORT, 'should not run'); END", ()).await.expect("trigger");
    drop(db);
    let trigger_attempt = lab
        .attempt(recipient("nobody"), session(), 1)
        .expect("attempt");
    assert!(matches!(
        lab.claim(&trigger_attempt).await,
        Err(OrderedClaimError::TemporaryTrigger)
    ));
    let db = pool.global().guard().await.expect("remove trigger");
    db.execute("DROP TRIGGER temp.reject_pending", ())
        .await
        .expect("drop trigger");
    db.execute(
        "ALTER TABLE main.mam_messages RENAME TO hidden_mam_messages",
        (),
    )
    .await
    .expect("hide main");
    drop(db);
    assert!(
        OrderedClaimLab::from_pool(&pool).await.is_err(),
        "TEMP MAM cannot stand in for missing main"
    );
    let db = pool.global().guard().await.expect("restore MAM");
    db.execute(
        "ALTER TABLE main.hidden_mam_messages RENAME TO mam_messages",
        (),
    )
    .await
    .expect("restore");
    drop(db);

    let blocked_attempt = lab
        .attempt(recipient("blocked"), session(), 1)
        .expect("attempt");
    let held = pool.global().begin().await.expect("occupy only checkout");
    let task = {
        let lab = Arc::clone(&lab);
        let attempt = blocked_attempt.clone();
        tokio::spawn(async move { lab.claim(&attempt).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    task.abort();
    assert!(task.await.expect_err("cancelled task").is_cancelled());
    held.rollback().await.expect("return checkout");
    assert!(matches!(
        lab.claim(&blocked_attempt).await,
        Err(OrderedClaimError::AttemptAlreadyStarted)
    ));
    assert!(lab
        .observe(&blocked_attempt)
        .await
        .expect("no visible claim")
        .is_empty());
    assert!(
        lab.observe(&attempt).await.is_ok(),
        "database and sole permit survive checkout cancellation"
    );
}

#[tokio::test]
async fn sqlite_multi_connection_pool_is_rejected() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
    config.pool_size = 2;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("pool");
    assert!(matches!(
        OrderedClaimLab::from_pool(&pool).await,
        Err(OrderedClaimError::InvalidSqliteCapacity)
    ));
}

pub(super) async fn exercise_postgres(fixture: &Fixture) {
    // Remove broad privileges used by the preceding legacy fixture.
    for sql in [
        format!("REVOKE ALL ON {}.pending_delivery FROM {}", fixture.target, fixture.runtime),
        format!("GRANT SELECT ON {}.pending_delivery TO {}", fixture.target, fixture.runtime),
        format!("GRANT UPDATE (flushed_in_session, outbound_sequence, claimed_at_ms, claim_token, claim_offered) ON {}.pending_delivery TO {}", fixture.target, fixture.runtime),
    ] {
        sqlx::query(&sql).execute(&fixture.admin).await.expect("restricted ordered grant");
    }
    let permissions: (bool, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'INSERT'), pg_catalog.has_table_privilege($1, $2, 'DELETE'), \
         pg_catalog.has_table_privilege($1, $2, 'TRUNCATE'), pg_catalog.has_schema_privilege($1, $3, 'CREATE'), \
         pg_catalog.has_database_privilege($1, pg_catalog.current_database(), 'TEMP')",
    ).bind(&fixture.runtime).bind(format!("{}.pending_delivery", fixture.target)).bind(&fixture.target)
        .fetch_one(&fixture.admin).await.expect("inspect runtime privileges");
    assert_eq!(permissions, (false, false, false, false, false));
    let attested = attested_pool(
        fixture.runtime_options.clone(),
        fixture.config(),
        3,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("ordered claim lab", attested, 2, 1)
        .await
        .expect("main and ingress lanes");
    let lab = Arc::new(
        OrderedClaimLab::from_pool(&pool)
            .await
            .expect("attach without DDL"),
    );
    let shadow = format!("ordered_shadow_{}", uuid::Uuid::new_v4().simple());
    for sql in [
        format!("CREATE SCHEMA {shadow}"),
        format!("CREATE TABLE {shadow}.pending_delivery AS SELECT * FROM {}.pending_delivery WHERE false", fixture.target),
        format!("CREATE TABLE {shadow}.mam_messages AS SELECT * FROM {}.mam_messages WHERE false", fixture.target),
        format!("CREATE FUNCTION {shadow}.false_equal(text,text) RETURNS boolean LANGUAGE SQL IMMUTABLE AS 'SELECT false'"),
        format!("CREATE OPERATOR {shadow}.= (LEFTARG=text, RIGHTARG=text, FUNCTION={shadow}.false_equal)"),
        format!("GRANT USAGE ON SCHEMA {shadow} TO {}", fixture.runtime),
        format!("GRANT SELECT ON {shadow}.pending_delivery, {shadow}.mam_messages TO {}", fixture.runtime),
    ] {
        sqlx::query(&sql).execute(&fixture.admin).await.expect("create shadow");
    }
    let hostile: bool = sqlx::query_scalar(&format!(
        "SELECT 'same'::text OPERATOR({shadow}.=) 'same'::text"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("verify hostile operator");
    assert!(!hostile);
    for (row, who) in [
        ("ordered-a", "alice"),
        ("ordered-b", "bob"),
        ("ordered-lost", "carol"),
        ("ordered-race", "dave"),
        ("ordered-pointer", "frank"),
        ("ordered-marker", "grace"),
        ("ordered-hold", "erin"),
    ] {
        sqlx::query(&format!("INSERT INTO {}.pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, transient_xml) VALUES ($1, $2, 1, 'transient', '<message/>')", fixture.target))
            .bind(row).bind(recipient(who).to_string()).execute(&fixture.admin).await.expect("seed pending row");
    }
    let first = lab
        .attempt(recipient("alice"), session(), 1)
        .expect("first attempt");
    let rival = lab
        .attempt(recipient("alice"), SmSessionId::new("other-session"), 1)
        .expect("rival");
    let other = lab
        .attempt(recipient("bob"), session(), 1)
        .expect("different recipient");
    let (one, two, independent) = tokio::join!(
        lab.claim_with_search_path(&first, shadow.clone()),
        lab.claim(&rival),
        lab.claim(&other),
    );
    let won = match (&one, &two) {
        (Ok(rows), Err(OrderedClaimError::Contended)) if ids(rows) == ["ordered-a"] => &first,
        (Err(OrderedClaimError::Contended), Ok(rows)) if ids(rows) == ["ordered-a"] => &rival,
        _ => panic!("one same-recipient claim must win"),
    };
    assert_eq!(
        ids(&independent.expect("different recipient progresses")),
        ["ordered-b"]
    );
    assert_eq!(
        ids(&lab
            .observe_with_search_path(won, shadow.clone())
            .await
            .expect("shadow-resistant observation")),
        ["ordered-a"]
    );
    let shadow_count: i64 =
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {shadow}.pending_delivery"))
            .fetch_one(&fixture.admin)
            .await
            .expect("shadow count");
    assert_eq!(shadow_count, 0);

    let lost = lab
        .attempt(recipient("carol"), session(), 1)
        .expect("lost attempt");
    assert!(matches!(
        lab.claim_with_lost_commit_ack(&lost).await,
        Err(OrderedClaimError::Indeterminate)
    ));
    assert_eq!(
        ids(&lab
            .observe(&lost)
            .await
            .expect("positive committed observation")),
        ["ordered-lost"]
    );
    assert!(matches!(
        lab.claim(&lost).await,
        Err(OrderedClaimError::AttemptAlreadyStarted)
    ));
    sqlx::query(&format!(
        "DELETE FROM {}.pending_delivery WHERE row_id = 'ordered-lost'",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove observed row");
    assert!(
        lab.observe(&lost)
            .await
            .expect("current observation")
            .is_empty(),
        "absence after deletion is not rollback proof"
    );

    // Move a candidate after the claim's statement snapshot but before its
    // UPDATE acquires the row lock.
    let mut mover = fixture.admin.begin().await.expect("row mover");
    sqlx::query(&format!("UPDATE {}.pending_delivery SET recipient_jid = 'moved@example.test' WHERE row_id = 'ordered-race'", fixture.target)).execute(&mut *mover).await.expect("lock and move row");
    let race = lab
        .attempt(recipient("dave"), session(), 1)
        .expect("race attempt");
    let claim_task = {
        let lab = Arc::clone(&lab);
        let attempt = race.clone();
        tokio::spawn(async move { lab.claim(&attempt).await })
    };
    wait_for_runtime_lock(fixture, "WITH eligible").await;
    mover.commit().await.expect("commit changed recipient");
    assert!(
        claim_task.await.expect("join").expect("claim").is_empty(),
        "moved row must not be claimed"
    );
    let moved: (String, Option<String>) = sqlx::query_as(&format!("SELECT recipient_jid, flushed_in_session FROM {}.pending_delivery WHERE row_id='ordered-race'", fixture.target)).fetch_one(&fixture.admin).await.expect("moved row");
    assert_eq!(moved, ("moved@example.test".to_owned(), None));
    assert!(lab
        .observe(&race)
        .await
        .expect("no current match")
        .is_empty());

    let mut pointer_mover = fixture.admin.begin().await.expect("pointer mover");
    sqlx::query(&format!("UPDATE {}.pending_delivery SET payload_kind='archived', archive_stanza_by='frank@example.test', archive_stanza_id='changed' WHERE row_id='ordered-pointer'", fixture.target))
        .execute(&mut *pointer_mover).await.expect("lock and change ordering evidence");
    let pointer = lab
        .attempt(recipient("frank"), session(), 1)
        .expect("pointer attempt");
    let claim_task = {
        let lab = Arc::clone(&lab);
        let attempt = pointer.clone();
        tokio::spawn(async move { lab.claim(&attempt).await })
    };
    wait_for_runtime_lock(fixture, "WITH eligible").await;
    pointer_mover
        .commit()
        .await
        .expect("commit changed pointer");
    assert!(
        claim_task.await.expect("join").expect("claim").is_empty(),
        "changed ordering evidence must not be claimed"
    );
    let unclaimed: bool = sqlx::query_scalar(&format!(
        "SELECT flushed_in_session IS NULL FROM {}.pending_delivery WHERE row_id='ordered-pointer'",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("pointer row");
    assert!(unclaimed);
    assert!(lab
        .observe(&pointer)
        .await
        .expect("no current match")
        .is_empty());

    let mut marker_mover = fixture.admin.begin().await.expect("marker mover");
    sqlx::query(&format!("UPDATE {}.pending_delivery SET outbound_sequence=17, claim_offered=1 WHERE row_id='ordered-marker'", fixture.target)).execute(&mut *marker_mover).await.expect("lock and change custody markers");
    let marker = lab
        .attempt(recipient("grace"), session(), 1)
        .expect("marker attempt");
    let claim_task = {
        let lab = Arc::clone(&lab);
        let attempt = marker.clone();
        tokio::spawn(async move { lab.claim(&attempt).await })
    };
    wait_for_runtime_lock(fixture, "WITH eligible").await;
    marker_mover.commit().await.expect("commit changed markers");
    assert!(
        claim_task.await.expect("join").expect("claim").is_empty(),
        "changed custody markers must not be cleared"
    );
    let markers: (Option<i32>, i32, Option<String>) = sqlx::query_as(&format!("SELECT outbound_sequence, claim_offered, flushed_in_session FROM {}.pending_delivery WHERE row_id='ordered-marker'", fixture.target)).fetch_one(&fixture.admin).await.expect("marker row");
    assert_eq!(markers, (Some(17), 1, None));

    // Hold a deferred trigger inside COMMIT, cancel the client, and observe
    // the same attempt before and after the original backend can finish.
    const HOLD_KEY: i64 = 7_139_466_181_007_443;
    sqlx::query(&format!("CREATE FUNCTION {}.ordered_hold_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.row_id = 'ordered-hold' THEN PERFORM pg_catalog.pg_advisory_xact_lock({HOLD_KEY}); END IF; RETURN NEW; END $$", fixture.target)).execute(&fixture.admin).await.expect("commit gate function");
    sqlx::query(&format!("CREATE CONSTRAINT TRIGGER ordered_hold_commit AFTER UPDATE ON {}.pending_delivery DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {}.ordered_hold_commit()", fixture.target, fixture.target)).execute(&fixture.admin).await.expect("commit gate trigger");
    let mut blocker = fixture.admin.acquire().await.expect("gate session");
    sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
        .bind(HOLD_KEY)
        .execute(&mut *blocker)
        .await
        .expect("hold gate");
    let held_attempt = lab
        .attempt(recipient("erin"), session(), 1)
        .expect("held attempt");
    let claim_task = {
        let lab = Arc::clone(&lab);
        let attempt = held_attempt.clone();
        tokio::spawn(async move { lab.claim(&attempt).await })
    };
    wait_for_runtime_lock(fixture, "COMMIT").await;
    assert!(lab
        .observe(&held_attempt)
        .await
        .expect("read while COMMIT waits")
        .is_empty());
    claim_task.abort();
    assert!(claim_task.await.expect_err("cancelled").is_cancelled());
    wait_for_runtime_lock(fixture, "COMMIT").await;
    assert!(matches!(
        lab.claim(&held_attempt).await,
        Err(OrderedClaimError::AttemptAlreadyStarted)
    ));
    let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
        .bind(HOLD_KEY)
        .fetch_one(&mut *blocker)
        .await
        .expect("release gate");
    assert!(unlocked);
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if !lab
                .observe(&held_attempt)
                .await
                .expect("attested observation")
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("late original COMMIT becomes visible");
    assert_eq!(
        ids(&lab.observe(&held_attempt).await.expect("committed row")),
        ["ordered-hold"]
    );
    sqlx::query(&format!(
        "DROP TRIGGER ordered_hold_commit ON {}.pending_delivery",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("drop trigger");
    sqlx::query(&format!(
        "DROP FUNCTION {}.ordered_hold_commit()",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("drop function");

    sqlx::query(&format!(
        "CREATE TABLE {}.ordered_inherited () INHERITS ({}.pending_delivery)",
        fixture.target, fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("inherited child");
    assert!(matches!(
        OrderedClaimLab::from_pool(&pool).await,
        Err(OrderedClaimError::UnsafePendingRelation)
    ));
    let inherited = lab
        .attempt(recipient("inherited"), session(), 1)
        .expect("attempt");
    assert!(matches!(
        lab.claim(&inherited).await,
        Err(OrderedClaimError::UnsafePendingRelation)
    ));
    sqlx::query(&format!("DROP TABLE {}.ordered_inherited", fixture.target))
        .execute(&fixture.admin)
        .await
        .expect("drop inherited child");

    sqlx::query(&format!(
        "ALTER TABLE {}.pending_delivery DROP CONSTRAINT pending_delivery_pkey",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove row identity key");
    assert!(matches!(
        OrderedClaimLab::from_pool(&pool).await,
        Err(OrderedClaimError::UnsafePendingRelation)
    ));
    sqlx::query(&format!("CREATE UNIQUE INDEX ordered_varchar_key ON {}.pending_delivery (row_id pg_catalog.varchar_ops)", fixture.target)).execute(&fixture.admin).await.expect("unsupported unique operator class");
    assert!(matches!(
        OrderedClaimLab::from_pool(&pool).await,
        Err(OrderedClaimError::UnsafePendingRelation)
    ));
    sqlx::query(&format!(
        "DROP INDEX {}.ordered_varchar_key",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("drop unsupported index");
    sqlx::query(&format!(
        "ALTER TABLE {}.pending_delivery ADD PRIMARY KEY (row_id)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore row identity key");
    sqlx::query(&format!("ALTER TABLE {}.pending_delivery ALTER COLUMN outbound_sequence TYPE TEXT USING outbound_sequence::TEXT", fixture.target)).execute(&fixture.admin).await.expect("drift numeric type");
    assert!(matches!(
        OrderedClaimLab::from_pool(&pool).await,
        Err(OrderedClaimError::UnsafePendingRelation)
    ));
    sqlx::query(&format!("ALTER TABLE {}.pending_delivery ALTER COLUMN outbound_sequence TYPE INTEGER USING outbound_sequence::INTEGER", fixture.target)).execute(&fixture.admin).await.expect("restore numeric type");

    sqlx::query(&format!(
        "ALTER TABLE {}.mam_messages RENAME TO hidden_ordered_mam",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("hide target MAM");
    assert!(
        OrderedClaimLab::from_pool(&pool).await.is_err(),
        "shadow MAM cannot replace the missing target"
    );
    sqlx::query(&format!(
        "ALTER TABLE {}.hidden_ordered_mam RENAME TO mam_messages",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore target MAM");
    drop(lab);
    drop(pool);
}

async fn wait_for_runtime_lock(fixture: &Fixture, fragment: &str) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=$1 AND usename=$2 AND wait_event_type='Lock' AND position($3 IN query)>0)")
                .bind(&fixture.database_name).bind(&fixture.runtime).bind(fragment)
                .fetch_one(&fixture.admin).await.expect("observe blocked backend");
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("runtime backend reached the expected lock");
}

#[tokio::test]
async fn sqlite_nocase_columns_do_not_weaken_attempt_identity() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
    config.pool_size = 1;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("pool");
    let db = pool.global().guard().await.expect("schema");
    db.execute_batch("CREATE TABLE main.pending_delivery (row_id TEXT COLLATE NOCASE PRIMARY KEY, recipient_jid TEXT COLLATE NOCASE NOT NULL, flushed_in_session TEXT COLLATE NOCASE, outbound_sequence INTEGER, claimed_at_ms INTEGER, claim_token TEXT COLLATE NOCASE, claim_offered INTEGER NOT NULL DEFAULT 0, payload_kind TEXT, archive_stanza_by TEXT COLLATE NOCASE, archive_stanza_id TEXT COLLATE NOCASE); CREATE TABLE main.mam_messages (id TEXT COLLATE NOCASE, stanza_id TEXT COLLATE NOCASE, room_jid TEXT COLLATE NOCASE, archive_seq INTEGER);").await.expect("case-insensitive fixture schema");
    db.execute("INSERT INTO main.mam_messages VALUES ('physical-one', 'MixedCase', 'alice@example.test', 1), ('physical-two', 'mixedcase', 'alice@example.test', 10), ('physical-three', 'another', 'alice@example.test', 5)", ()).await.expect("archives");
    db.execute("INSERT INTO main.pending_delivery (row_id, recipient_jid, payload_kind, archive_stanza_by, archive_stanza_id) VALUES ('zCase', 'alice@example.test', 'archived', 'alice@example.test', 'MixedCase'), ('aRow', 'alice@example.test', 'archived', 'alice@example.test', 'another')", ()).await.expect("pending");
    drop(db);
    let lab = OrderedClaimLab::from_pool(&pool).await.expect("attach");
    assert!(matches!(
        lab.attempt(recipient("alice"), SmSessionId::new(""), 1),
        Err(OrderedClaimError::InvalidIdentity)
    ));
    assert!(matches!(
        lab.attempt(recipient("alice"), SmSessionId::new("s".repeat(129)), 1),
        Err(OrderedClaimError::InvalidIdentity)
    ));
    let attempt = lab
        .attempt(recipient("alice"), session(), 2)
        .expect("attempt");
    assert_eq!(
        ids(&lab.claim(&attempt).await.expect("claim")),
        ["zCase", "aRow"],
        "archive IDs are compared bytewise"
    );
    assert!(lab
        .observe_row(&attempt, &PendingRowId::new("zcase"))
        .await
        .expect("wrong case")
        .is_none());
    let db = pool.global().guard().await.expect("change session case");
    db.execute("UPDATE main.pending_delivery SET flushed_in_session = 'ORDERED-LAB-SESSION' WHERE row_id = 'zCase'", ()).await.expect("change case");
    drop(db);
    assert!(lab
        .observe_row(&attempt, &PendingRowId::new("zCase"))
        .await
        .expect("wrong session case")
        .is_none());
}

#[tokio::test]
async fn sqlite_duplicate_row_ids_are_refused_before_any_claim() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
    config.pool_size = 1;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("pool");
    let db = pool.global().guard().await.expect("fixture");
    db.execute_batch("CREATE TABLE main.pending_delivery (row_id TEXT, recipient_jid TEXT, flushed_in_session TEXT, outbound_sequence INTEGER, claimed_at_ms INTEGER, claim_token TEXT, claim_offered INTEGER, payload_kind TEXT, archive_stanza_by TEXT, archive_stanza_id TEXT); CREATE TABLE main.mam_messages (id TEXT, stanza_id TEXT, room_jid TEXT, archive_seq INTEGER); INSERT INTO main.pending_delivery VALUES ('duplicate', 'alice@example.test', NULL, NULL, NULL, NULL, 0, 'transient', NULL, NULL), ('duplicate', 'alice@example.test', NULL, NULL, NULL, NULL, 0, 'transient', NULL, NULL);").await.expect("malformed fixture");
    drop(db);
    assert!(matches!(
        OrderedClaimLab::from_pool(&pool).await,
        Err(OrderedClaimError::UnsafePendingRelation)
    ));
    let db = pool.global().guard().await.expect("inspect");
    let mut rows = db
        .query(
            "SELECT count(*) FROM main.pending_delivery WHERE flushed_in_session IS NOT NULL",
            (),
        )
        .await
        .expect("unchanged");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("count"),
        0
    );
}

#[tokio::test]
async fn duplicate_stanza_id_after_claim_can_change_later_lookup() {
    use waddle_server::config::{IngressConfig, LineageConfig};
    use waddle_server::test_support::SharedCorrectnessFixture;
    use waddle_xmpp_core::xep0359::StanzaId;

    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
    config.pool_size = 1;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("pool");
    waddle_server::test_support::initialize_mam_on_sqlite_test_pool(&pool)
        .await
        .expect("owner MAM");
    bootstrap_pending_delivery_owner_schema(pool.global())
        .await
        .expect("owner pending");
    let db = pool.global().guard().await.expect("seed");
    db.execute("INSERT INTO main.mam_messages (id, room_jid, timestamp, from_jid, to_jid, stanza_id, archive_seq) VALUES ('first', 'alice@example.test', '2020-01-01T00:00:00Z', 'sender@example.test', 'alice@example.test', 'shared-id', 1)", ()).await.expect("first archive");
    db.execute("INSERT INTO main.pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id) VALUES ('race-row', 'alice@example.test', 1, 'archived', 'alice@example.test', 'shared-id')", ()).await.expect("pending");
    drop(db);
    let lab = OrderedClaimLab::from_pool(&pool).await.expect("lab");
    let attempt = lab
        .attempt(recipient("alice"), session(), 1)
        .expect("attempt");
    assert_eq!(
        ids(&lab.claim(&attempt).await.expect("claim")),
        ["race-row"]
    );
    let db = pool.global().guard().await.expect("new archive");
    db.execute("INSERT INTO main.mam_messages (id, room_jid, timestamp, from_jid, to_jid, stanza_id, archive_seq) VALUES ('later', 'alice@example.test', '2020-01-01T00:00:01Z', 'sender@example.test', 'alice@example.test', 'shared-id', 2)", ()).await.expect("later same stanza id");
    drop(db);
    let resolver = SharedCorrectnessFixture::from_pool(
        &pool,
        &IngressConfig {
            pool_size: 1,
            retry_attempts: 1,
            remote_room_acceptance: Default::default(),
        },
        LineageConfig::default(),
        #[cfg(feature = "clustering")]
        None,
    )
    .expect("same-pool resolver");
    let stamp = StanzaId::new("shared-id", "alice@example.test".parse().expect("archive"));
    let found = resolver
        .lookup_archive_or_stanza_id(&stamp)
        .await
        .expect("lookup")
        .expect("archive");
    assert_eq!(
        found.id, "later",
        "the later lookup is not pinned to the row used for claim ordering"
    );
}

#[tokio::test]
async fn sqlite_numeric_affinity_identity_columns_are_refused() {
    for (row_id_type, session_type) in [("TEXT PRIMARY KEY", "INTEGER"), ("INTEGER UNIQUE", "TEXT")]
    {
        let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
        config.pool_size = 1;
        let pool = DatabasePool::try_new(config, PoolConfig)
            .await
            .expect("pool");
        let db = pool.global().guard().await.expect("schema");
        db.execute_batch(&format!("CREATE TABLE main.pending_delivery (row_id {row_id_type}, recipient_jid TEXT, flushed_in_session {session_type}, outbound_sequence INTEGER, claimed_at_ms INTEGER, claim_token TEXT, claim_offered INTEGER, payload_kind TEXT, archive_stanza_by TEXT, archive_stanza_id TEXT); CREATE TABLE main.mam_messages (id TEXT, stanza_id TEXT, room_jid TEXT, archive_seq INTEGER);"))
            .await.expect("numeric-affinity schema");
        drop(db);
        assert!(matches!(
            OrderedClaimLab::from_pool(&pool).await,
            Err(OrderedClaimError::UnsafePendingRelation)
        ));
    }
}

#[tokio::test]
async fn sqlite_blob_row_ids_are_never_positive_observations() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
    config.pool_size = 1;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("pool");
    waddle_server::test_support::initialize_mam_on_sqlite_test_pool(&pool)
        .await
        .expect("MAM");
    bootstrap_pending_delivery_owner_schema(pool.global())
        .await
        .expect("pending");
    let db = pool.global().guard().await.expect("seed");
    db.execute("INSERT INTO main.pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind) VALUES (CAST('blob-row' AS BLOB), 'blob@example.test', 1, 'transient'), ('text-row', 'text@example.test', 1, 'transient')", ()).await.expect("rows");
    drop(db);
    let lab = OrderedClaimLab::from_pool(&pool).await.expect("lab");
    let blob = lab
        .attempt(recipient("blob"), session(), 1)
        .expect("blob attempt");
    assert!(matches!(
        lab.claim(&blob).await,
        Err(OrderedClaimError::CorruptRow)
    ));
    let db = pool.global().guard().await.expect("verify rollback");
    let mut count = db.query("SELECT count(*) FROM main.pending_delivery WHERE recipient_jid='blob@example.test' AND flushed_in_session IS NOT NULL", ()).await.expect("count");
    assert_eq!(
        count
            .next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("integer"),
        0
    );
    drop(count);
    drop(db);
    let text = lab
        .attempt(recipient("text"), session(), 1)
        .expect("text attempt");
    assert_eq!(ids(&lab.claim(&text).await.expect("claim")), ["text-row"]);
    let db = pool.global().guard().await.expect("mutate row type");
    db.execute(
        "UPDATE main.pending_delivery SET row_id=CAST('text-row' AS BLOB) WHERE row_id='text-row'",
        (),
    )
    .await
    .expect("BLOB identity");
    drop(db);
    assert!(matches!(
        lab.observe(&text).await,
        Err(OrderedClaimError::CorruptRow)
    ));
}
