//! Turso 0.8.4 WAL-mode baseline compatibility spike for Waddle's
//! `server/crates/waddle-server/sql/baseline/sqlite.sql` schema, 8 triggers,
//! MAM full-text search (`GLOB` vs SQLite `FTS5` vs Turso native `USING fts`),
//! `CAST(unixepoch('subsec') * 1000 AS INTEGER)` monotonicity, and WAL
//! checkpoint / cacheflush behavior.

use std::time::Duration;

use tempfile::TempDir;
use turso::{Builder, Connection, Database, Value};

const BASELINE_SQL: &str = include_str!("../../waddle-server/sql/baseline/sqlite.sql");

async fn open_wal_db(enable_index_method: bool) -> (TempDir, Database, Connection) {
    let dir = TempDir::new().expect("create tempdir");
    let db_path = dir.path().join("tenant_spike.db");
    let path_str = db_path.to_str().expect("utf-8 tempdir path");

    let mut builder = Builder::new_local(path_str);
    if enable_index_method {
        builder = builder.experimental_index_method(true);
    }
    let db = builder.build().await.expect("open turso local db");
    let conn = db.connect().expect("connect to turso db");

    let mode_rows = conn
        .pragma_update("journal_mode", "WAL")
        .await
        .expect("set PRAGMA journal_mode=WAL");
    let mode_str = mode_rows
        .first()
        .and_then(|row| row.get_value(0).ok())
        .map(|val| match val {
            Value::Text(s) => s.to_ascii_lowercase(),
            other => format!("{other:?}"),
        })
        .unwrap_or_default();
    assert_eq!(mode_str, "wal", "Turso must enter standard WAL mode");

    conn.pragma_update("foreign_keys", "ON")
        .await
        .expect("enable foreign_keys");

    (dir, db, conn)
}

async fn apply_baseline_schema(conn: &Connection) {
    conn.execute_batch(BASELINE_SQL)
        .await
        .expect("apply server/crates/waddle-server/sql/baseline/sqlite.sql in Turso WAL mode");
}

async fn query_i64(conn: &Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.expect("run query");
    let row = rows
        .next()
        .await
        .expect("fetch row")
        .expect("expected at least one row");
    match row.get_value(0).expect("get column 0") {
        Value::Integer(n) => n,
        other => panic!("expected integer value, got {other:?}"),
    }
}

#[tokio::test]
async fn test_wal_mode_baseline_schema_and_checkpoint_probe() {
    let (_dir, _db, conn) = open_wal_db(false).await;
    apply_baseline_schema(&conn).await;

    let trigger_count = query_i64(
        &conn,
        "SELECT count(*) FROM sqlite_master WHERE type = 'trigger'",
    )
    .await;
    assert_eq!(
        trigger_count, 8,
        "all 8 baseline triggers must be registered in sqlite_master"
    );

    let epoch = query_i64(
        &conn,
        "SELECT epoch FROM ingress_protocol_epoch WHERE id = 1",
    )
    .await;
    assert_eq!(epoch, 0);

    let last_ordinal = query_i64(
        &conn,
        "SELECT last_ordinal FROM pending_notification_recovery_counter WHERE singleton = 1",
    )
    .await;
    assert_eq!(last_ordinal, 0);

    // Probe WAL checkpoint & cacheflush primitives for M2 LRU FD pool eviction.
    let truncate_res = conn.query("PRAGMA wal_checkpoint(TRUNCATE)", ()).await;
    let passive_res = conn.query("PRAGMA wal_checkpoint(PASSIVE)", ()).await;
    let cacheflush_res = conn.cacheflush();

    eprintln!(
        "[turso-spike] wal_checkpoint(TRUNCATE)={:?}, wal_checkpoint(PASSIVE)={:?}, cacheflush={:?}",
        truncate_res.as_ref().map(|_| "ok"),
        passive_res.as_ref().map(|_| "ok"),
        cacheflush_res,
    );

    // At least one flush/checkpoint primitive must succeed cleanly.
    assert!(
        truncate_res.is_ok() || passive_res.is_ok() || cacheflush_res.is_ok(),
        "expected at least one WAL flush/checkpoint primitive to succeed"
    );
}

#[tokio::test]
async fn test_all_eight_baseline_triggers_enforced() {
    let (_dir, _db, conn) = open_wal_db(false).await;
    apply_baseline_schema(&conn).await;

    // Trigger 1: ingress_messages_source_binding_insert_guard
    conn.execute(
        "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES ('m-unbound', 1, zeroblob(32))",
        (),
    )
    .await
    .expect("valid unbound ingress_messages insert");

    conn.execute(
        "INSERT INTO ingress_messages (
            message_key, digest_version, digest,
            source_binding_version, source_bare_jid, source_auth_context_id,
            source_auth_context_version, source_auth_epoch, owner_decision
         ) VALUES (
            'm-bound', 1, zeroblob(32),
            1, 'alice@example.com', 'ctx1', '1', '1', NULL
         )",
        (),
    )
    .await
    .expect("valid bound ingress_messages insert");

    conn.execute(
        "INSERT INTO ingress_messages (
            message_key, digest_version, digest,
            source_binding_version, source_bare_jid, source_auth_context_id,
            source_auth_context_version, source_auth_epoch, owner_decision
         ) VALUES (
            'm-bound-accept', 1, zeroblob(32),
            1, 'bob@example.com', 'ctx2', '1', '1', NULL
         )",
        (),
    )
    .await
    .expect("valid second bound ingress_messages insert");

    let t1_err = conn
        .execute(
            "INSERT INTO ingress_messages (
                message_key, digest_version, digest,
                source_binding_version, source_bare_jid, source_auth_context_id,
                source_auth_context_version, source_auth_epoch
             ) VALUES (
                'm-bad', 1, zeroblob(32),
                1, '', 'ctx1', '1', '1'
             )",
            (),
        )
        .await
        .expect_err("Trigger 1 must reject malformed source binding");
    assert!(
        t1_err
            .to_string()
            .contains("waddle: malformed immutable ingress source binding"),
        "unexpected Trigger 1 error: {t1_err}"
    );

    // Trigger 2: ingress_messages_source_binding_update_guard
    let t2_immut_err = conn
        .execute(
            "UPDATE ingress_messages SET source_bare_jid = 'mallory@example.com' WHERE message_key = 'm-bound'",
            (),
        )
        .await
        .expect_err("Trigger 2 must reject mutating source_bare_jid");
    assert!(
        t2_immut_err
            .to_string()
            .contains("waddle: ingress source binding or owner decision is immutable"),
        "unexpected Trigger 2 error: {t2_immut_err}"
    );

    // Valid transition NULL -> 0
    conn.execute(
        "UPDATE ingress_messages SET owner_decision = 0 WHERE message_key = 'm-bound'",
        (),
    )
    .await
    .expect("Trigger 2 allows owner_decision NULL -> 0");

    // Invalid transition 0 -> 2 without rejection row
    let t2_reject_err = conn
        .execute(
            "UPDATE ingress_messages SET owner_decision = 2 WHERE message_key = 'm-bound'",
            (),
        )
        .await
        .expect_err("Trigger 2 must reject owner_decision 0 -> 2 without rejection row");
    assert!(
        t2_reject_err
            .to_string()
            .contains("waddle: ingress source binding or owner decision is immutable"),
        "unexpected Trigger 2 reject error: {t2_reject_err}"
    );

    // Valid transition NULL -> 0 -> 1 on m-bound-accept (no rejection row)
    conn.execute(
        "UPDATE ingress_messages SET owner_decision = 0 WHERE message_key = 'm-bound-accept'",
        (),
    )
    .await
    .expect("Trigger 2 allows owner_decision NULL -> 0 on m-bound-accept");

    conn.execute(
        "UPDATE ingress_messages SET owner_decision = 1 WHERE message_key = 'm-bound-accept'",
        (),
    )
    .await
    .expect("Trigger 2 allows owner_decision 0 -> 1 without rejection row");

    // Trigger 4: ingress_owner_rejections_insert_guard
    let t4_err = conn
        .execute(
            "INSERT INTO ingress_owner_rejections (source_message_key, child_message_key) VALUES ('m-unbound', 'm-bound')",
            (),
        )
        .await
        .expect_err("Trigger 4 must reject unbound source_message_key");
    assert!(
        t4_err
            .to_string()
            .contains("waddle: owner rejection requires an undecided bound source"),
        "unexpected Trigger 4 error: {t4_err}"
    );

    conn.execute(
        "INSERT INTO ingress_owner_rejections (source_message_key, child_message_key) VALUES ('m-bound', 'm-unbound')",
        (),
    )
    .await
    .expect("Trigger 4 allows rejection row for undecided bound source");

    // Now Trigger 2 allows 0 -> 2 because rejection row exists
    conn.execute(
        "UPDATE ingress_messages SET owner_decision = 2 WHERE message_key = 'm-bound'",
        (),
    )
    .await
    .expect("Trigger 2 allows owner_decision 0 -> 2 once rejection row exists");

    // Trigger 5: ingress_owner_rejections_update_guard
    let t5_err = conn
        .execute(
            "UPDATE ingress_owner_rejections SET child_message_key = 'm-bound-accept' WHERE source_message_key = 'm-bound'",
            (),
        )
        .await
        .expect_err("Trigger 5 must reject updates to ingress_owner_rejections");
    assert!(
        t5_err
            .to_string()
            .contains("waddle: owner rejection binding is immutable"),
        "unexpected Trigger 5 error: {t5_err}"
    );

    // Trigger 3: ingress_owner_rejections_delete_guard
    let t3_err = conn
        .execute(
            "DELETE FROM ingress_owner_rejections WHERE source_message_key = 'm-bound'",
            (),
        )
        .await
        .expect_err("Trigger 3 must reject delete while child row still exists");
    assert!(
        t3_err
            .to_string()
            .contains("waddle: owner rejection survives until child retention"),
        "unexpected Trigger 3 error: {t3_err}"
    );

    // Trigger 6: pending_notification_recovery_allocate
    conn.execute(
        "INSERT INTO pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind)
         VALUES ('pd-1', 'alice@example.com', 1700000000, 'archive')",
        (),
    )
    .await
    .expect("insert into pending_delivery");

    let allocated_ord = query_i64(
        &conn,
        "SELECT notification_recovery_ordinal FROM pending_delivery WHERE row_id = 'pd-1'",
    )
    .await;
    assert_eq!(allocated_ord, 1, "Trigger 6 must allocate ordinal 1");

    let counter_ord = query_i64(
        &conn,
        "SELECT last_ordinal FROM pending_notification_recovery_counter WHERE singleton = 1",
    )
    .await;
    assert_eq!(counter_ord, 1, "Trigger 6 must increment counter to 1");

    // Trigger 7: pending_notification_recovery_counter_monotone
    let t7_err = conn
        .execute(
            "UPDATE pending_notification_recovery_counter SET last_ordinal = 0 WHERE singleton = 1",
            (),
        )
        .await
        .expect_err("Trigger 7 must prevent rewinding recovery counter");
    assert!(
        t7_err
            .to_string()
            .contains("pending recovery counter cannot rewind"),
        "unexpected Trigger 7 error: {t7_err}"
    );

    // Trigger 8: pending_notification_recovery_ordinal_immutable
    let t8_err = conn
        .execute(
            "UPDATE pending_delivery SET notification_recovery_ordinal = 99 WHERE row_id = 'pd-1'",
            (),
        )
        .await
        .expect_err("Trigger 8 must prevent mutating allocated ordinal");
    assert!(
        t8_err
            .to_string()
            .contains("pending recovery ordinal is immutable"),
        "unexpected Trigger 8 error: {t8_err}"
    );
}

#[tokio::test]
async fn test_mam_fulltext_glob_and_fts5_vs_turso_fts_probe() {
    let (_dir, _db, conn) = open_wal_db(true).await;
    apply_baseline_schema(&conn).await;

    conn.execute(
        "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, archive_seq)
         VALUES ('m1', 'room@muc.example', '2026-11-25T00:00:00Z', 'alice@example', 'room@muc.example', 'Release notes for v2', 1)",
        (),
    )
    .await
    .expect("insert mam m1");
    conn.execute(
        "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, archive_seq)
         VALUES ('m2', 'room@muc.example', '2026-11-25T00:00:01Z', 'bob@example', 'room@muc.example', 'Überraschung with *star* and [bracket]', 2)",
        (),
    )
    .await
    .expect("insert mam m2");
    conn.execute(
        "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, archive_seq)
         VALUES ('m3', 'room@muc.example', '2026-11-25T00:00:02Z', 'carol@example', 'room@muc.example', 'Unrelated message', 3)",
        (),
    )
    .await
    .expect("insert mam m3");

    // 1. Waddle's exact runtime MAM full-text GLOB filter pattern
    let mut rows = conn
        .query(
            "SELECT id FROM mam_messages WHERE room_jid = ?1 AND body GLOB ?2 AND body GLOB ?3",
            [
                "room@muc.example",
                "*[rR][eE][lL][eE][aA][sS][eE]*",
                "*[nN][oO][tT][eE][sS]*",
            ],
        )
        .await
        .expect("run MAM GLOB query");
    let mut matched = Vec::new();
    while let Some(row) = rows.next().await.expect("next row") {
        if let Value::Text(id) = row.get_value(0).expect("id col") {
            matched.push(id);
        }
    }
    assert_eq!(matched, vec!["m1".to_owned()]);

    // Unicode case-fold + escaped literal metacharacter
    let mut rows2 = conn
        .query(
            "SELECT id FROM mam_messages WHERE room_jid = ?1 AND body GLOB ?2 AND body GLOB ?3",
            [
                "room@muc.example",
                "*[üÜ][bB][eE][rR]*",
                "*[*][sS][tT][aA][rR][*]*",
            ],
        )
        .await
        .expect("run Unicode/escaped MAM GLOB query");
    let mut matched2 = Vec::new();
    while let Some(row) = rows2.next().await.expect("next row") {
        if let Value::Text(id) = row.get_value(0).expect("id col") {
            matched2.push(id);
        }
    }
    assert_eq!(matched2, vec!["m2".to_owned()]);

    // 2. Honest SQLite FTS5 probe: Turso 0.8.4 does NOT support SQLite FTS5 virtual tables.
    let fts5_res = conn
        .execute("CREATE VIRTUAL TABLE mam_fts_probe USING fts5(body)", ())
        .await;
    eprintln!("[turso-spike] SQLite FTS5 probe result: {fts5_res:?}");
    assert!(
        fts5_res.is_err(),
        "Turso 0.8.4 is expected to reject SQLite FTS5 virtual tables"
    );

    // 3. Turso native Tantivy FTS (`USING fts`) probe
    let native_fts_create = conn
        .execute(
            "CREATE INDEX idx_mam_fts ON mam_messages USING fts (body)",
            (),
        )
        .await;
    eprintln!("[turso-spike] Turso native USING fts index result: {native_fts_create:?}");
    if native_fts_create.is_ok() {
        let mut fts_rows = conn
            .query(
                "SELECT id FROM mam_messages WHERE fts_match(body, 'release')",
                (),
            )
            .await
            .expect("Turso native fts_match query");
        let mut fts_matched = Vec::new();
        while let Some(row) = fts_rows.next().await.expect("next fts row") {
            if let Value::Text(id) = row.get_value(0).expect("fts id col") {
                fts_matched.push(id);
            }
        }
        eprintln!("[turso-spike] Turso native fts_match('release') rows: {fts_matched:?}");
        assert_eq!(fts_matched, vec!["m1".to_owned()]);
    }
}

#[tokio::test]
async fn test_unixepoch_subsec_millis_monotonicity() {
    let (_dir, _db, conn) = open_wal_db(false).await;

    let ts1 = query_i64(&conn, "SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)").await;
    tokio::time::sleep(Duration::from_millis(25)).await;
    let ts2 = query_i64(&conn, "SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)").await;

    assert!(
        ts1 > 1_700_000_000_000,
        "expected realistic unixepoch ms timestamp, got {ts1}"
    );
    assert!(
        ts2 > ts1,
        "expected subsecond millisecond monotonicity: ts1={ts1}, ts2={ts2}"
    );
    assert!(
        ts2 - ts1 >= 15,
        "expected >=15ms delta after 25ms sleep, got {}",
        ts2 - ts1
    );

    // Verify usage inside a CAS lease table matching Plan Step 3's PlacementLeaseStore pattern.
    conn.execute_batch(
        "CREATE TABLE placement_leases (
            tenant_id TEXT PRIMARY KEY,
            holder_id TEXT NOT NULL,
            generation INTEGER NOT NULL,
            expires_at_ms INTEGER NOT NULL
        );",
    )
    .await
    .expect("create lease table");

    conn.execute(
        "INSERT INTO placement_leases (tenant_id, holder_id, generation, expires_at_ms)
         VALUES ('acme.example', 'k1', 1, CAST(unixepoch('subsec') * 1000 AS INTEGER) + 5000)",
        (),
    )
    .await
    .expect("insert lease with unixepoch('subsec') * 1000");

    let expires_at = query_i64(
        &conn,
        "SELECT expires_at_ms FROM placement_leases WHERE tenant_id = 'acme.example'",
    )
    .await;
    assert!(expires_at >= ts2 + 4_900);
}
