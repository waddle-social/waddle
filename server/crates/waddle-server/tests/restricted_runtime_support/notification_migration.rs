//! Baseline-only notification rows used by migration and restricted-runtime witnesses.

use sha2::{Digest, Sha256};
use waddle_server::db::{Database, DatabaseDriver};

const SQLITE_BASELINE: &str = include_str!("../../sql/baseline/sqlite.sql");
const POSTGRES_BASELINE: &str = include_str!("../../sql/baseline/postgres.sql");

pub async fn install_baseline_only(db: &Database, schema: &str) {
    let template = match db.driver() {
        DatabaseDriver::Sqlite => SQLITE_BASELINE,
        DatabaseDriver::Postgres => POSTGRES_BASELINE,
    };
    let bootstrap = match db.driver() {
        DatabaseDriver::Sqlite => include_str!("../../sql/baseline/sqlite.bootstrap.sql"),
        DatabaseDriver::Postgres => include_str!("../../sql/baseline/postgres.bootstrap.sql"),
    };
    let sql = template
        .replace("__SCHEMA_QUOTED__", &format!("\"{schema}\""))
        .replace("__SCHEMA_LITERAL__", &format!("'{schema}'"))
        .replace("__SCHEMA__", schema);
    let mut tx = db.begin_immediate().await.expect("baseline transaction");
    tx.execute_batch(bootstrap)
        .await
        .expect("baseline bootstrap");
    tx.execute_batch(&sql).await.expect("baseline schema");
    tx.execute(
        "INSERT INTO _migrations (version, description, checksum) VALUES (?, ?, ?)",
        waddle_server::db_params![
            2000_i64,
            "schema baseline",
            hex::encode(Sha256::digest(template.as_bytes()))
        ],
    )
    .await
    .expect("baseline ledger row");
    tx.commit().await.expect("commit baseline");
}

pub async fn seed_notification_rows(db: &Database) {
    let conn = db.guard().await.expect("notification seed connection");
    conn.execute(
        "INSERT INTO notification_candidates (recipient_bare_jid, conversation_jid, sender_jid, thread_id, stanza_id_by, stanza_id, class, reason, created_at_ms, policy_error_count, next_attempt_at_ms, outboxed_at_ms, suppressed_reason, noping, no_store, no_permanent_store, last_message_body, reaction, delivery_id, quarantined_at_ms, quarantined_suppressed_reason) VALUES ('bob@example.com', 'alice@example.com', 'alice@example.com/phone', 'thread', 'bob@example.com', 'original', 'dm', 'offline_dm', 1000, 2, 2000, 3000, 'waddle_dnd', 1, 1, 1, 'preserved body', 1, 'delivery', 4000, 'original suppression')",
        (),
    )
    .await
    .expect("seed baseline candidate");
    for (job_id, status) in [("queued-job", "queued"), ("in-progress-job", "in-progress")] {
        conn.execute(
            "INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, thread_id, class, message_count, context_xml, summary_sender_jid, summary_body, approved_payload_xml, approved_publish_options_xml, queue_acceptance_may_exist, status, attempt_count, policy_error_count, last_error, next_attempt_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms, published_at_ms) VALUES (?, 'bob@example.com', 'push.example.com', 'node', 'alice@example.com', 'alice@example.com/phone', '[\"alice@example.com/phone\"]', 'thread', 'dm', 3, 'original-context', 'alice@example.com/phone', 'summary body', 'frozen payload', 'frozen options', 1, ?, 4, 5, 'previous error', 2000, 3000, 'claim', 1000, 4000, 5000)",
            waddle_server::db_params![job_id, status],
        )
        .await
        .expect("seed baseline outbox job");
    }
}

pub async fn assert_rows_preserved_and_insert_call(db: &Database) {
    let conn = db
        .guard()
        .await
        .expect("notification verification connection");
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM notification_candidates WHERE recipient_bare_jid = 'bob@example.com' AND conversation_jid = 'alice@example.com' AND sender_jid = 'alice@example.com/phone' AND thread_id = 'thread' AND stanza_id_by = 'bob@example.com' AND stanza_id = 'original' AND class = 'dm' AND reason = 'offline_dm' AND created_at_ms = 1000 AND policy_error_count = 2 AND next_attempt_at_ms = 2000 AND outboxed_at_ms = 3000 AND suppressed_reason = 'waddle_dnd' AND noping = 1 AND no_store = 1 AND no_permanent_store = 1 AND last_message_body = 'preserved body' AND reaction = 1 AND delivery_id = 'delivery' AND quarantined_at_ms = 4000 AND quarantined_suppressed_reason = 'original suppression' AND call_sid IS NULL AND call_video IS NULL", 1_i64),
        ("SELECT COUNT(*) FROM notification_outbox WHERE recipient_bare_jid = 'bob@example.com' AND push_service_jid = 'push.example.com' AND node = 'node' AND conversation_jid = 'alice@example.com' AND sender_jid = 'alice@example.com/phone' AND sender_jids = '[\"alice@example.com/phone\"]' AND thread_id = 'thread' AND class = 'dm' AND message_count = 3 AND context_xml = 'original-context' AND summary_sender_jid = 'alice@example.com/phone' AND summary_body = 'summary body' AND approved_payload_xml = 'frozen payload' AND approved_publish_options_xml = 'frozen options' AND queue_acceptance_may_exist = 1 AND ((job_id = 'queued-job' AND status = 'queued') OR (job_id = 'in-progress-job' AND status = 'in-progress')) AND attempt_count = 4 AND policy_error_count = 5 AND last_error = 'previous error' AND next_attempt_at_ms = 2000 AND claimed_at_ms = 3000 AND claim_token = 'claim' AND created_at_ms = 1000 AND updated_at_ms = 4000 AND published_at_ms = 5000", 2_i64),
    ] {
        let mut rows = conn.query(sql, ()).await.expect("read preserved rows");
        let row = rows.next().await.expect("count row").expect("count exists");
        assert_eq!(row.get::<i64>(0).expect("count"), expected);
    }
    conn.execute(
        "INSERT INTO notification_candidates (recipient_bare_jid, conversation_jid, sender_jid, stanza_id_by, stanza_id, class, reason, created_at_ms, call_sid, call_video) VALUES ('bob@example.com', 'alice@example.com', 'alice@example.com/phone', 'bob@example.com', 'call-stanza', 'call', 'offline_call', 6000, 'call-session', 1)",
        (),
    )
    .await
    .expect("call candidate accepted after migration");
    conn.execute(
        "INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, status, created_at_ms, updated_at_ms) VALUES ('call-job', 'bob@example.com', 'push.example.com', 'node', 'alice@example.com', 'alice@example.com/phone', '[\"alice@example.com/phone\"]', 'call', 1, 'call-context', 'queued', 6000, 6000)",
        (),
    )
    .await
    .expect("call job accepted after migration");
}
