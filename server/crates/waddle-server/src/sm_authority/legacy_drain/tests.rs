use jid::{BareJid, Jid};
use waddle_xmpp::pending_delivery::PendingPayload;
use waddle_xmpp::protocol::session_state::Blocklist;

use super::{drain_legacy_sm_sessions, frame_disposition, FrameDisposition, LegacyDrainReport};
use crate::db::Database;

const ALICE: &str = "alice@example.test";
const LEGACY: &str = "legacy-stream";
const DURABLE: &str = "00000000-0000-7000-8000-000000000001";

fn chat(id: &str, from: &str) -> String {
    format!(
        "<message xmlns='jabber:client' type='chat' id='{id}' from='{from}' \
         to='{ALICE}/phone'><body>{id}</body></message>"
    )
}

fn iq() -> String {
    format!(
        "<iq xmlns='jabber:client' type='get' id='ping' from='example.test' \
         to='{ALICE}/phone'><ping xmlns='urn:xmpp:ping'/></iq>"
    )
}

fn alice() -> BareJid {
    ALICE.parse().expect("bare jid")
}

#[test]
fn a_chat_message_is_stored_for_its_bare_recipient() {
    let open = Blocklist::new(Vec::<Jid>::new());
    match frame_disposition(&chat("m1", "bob@example.test/desk"), |_| Some(&open)) {
        FrameDisposition::Store { recipient, payload } => {
            assert_eq!(recipient, alice());
            assert!(matches!(payload, PendingPayload::Transient(_)));
        }
        FrameDisposition::Drop => panic!("a chat message is stored offline"),
    }
}

#[test]
fn iq_presence_and_unparseable_frames_are_dropped() {
    let open = Blocklist::new(Vec::<Jid>::new());
    let presence = format!("<presence xmlns='jabber:client' to='{ALICE}/phone'/>");
    for wire in [iq(), presence, "<not-xml".to_owned()] {
        assert!(matches!(
            frame_disposition(&wire, |_| Some(&open)),
            FrameDisposition::Drop
        ));
    }
}

#[test]
fn a_message_from_a_blocked_sender_is_dropped() {
    let blocked = Blocklist::new(vec!["bob@example.test"
        .parse::<Jid>()
        .expect("blocked jid")]);
    assert!(matches!(
        frame_disposition(&chat("m1", "bob@example.test/desk"), |_| Some(&blocked)),
        FrameDisposition::Drop
    ));
}

async fn database() -> Database {
    let db = Database::in_memory("sm-legacy-drain")
        .await
        .expect("in-memory database");
    crate::db::MigrationRunner::global()
        .run(&db)
        .await
        .expect("global migrations");
    let bounds =
        waddle_xmpp::stream_management::durable::ExposureBounds::new(8, 8192).expect("test bounds");
    crate::sm_authority::DurableSmStore::open(db.clone(), bounds, false)
        .await
        .expect("durable SM relations");
    crate::pending_delivery::bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("pending delivery relations");
    db
}

async fn execute(db: &Database, sql: &str, params: impl crate::db::IntoParams) {
    let connection = db.guard().await.expect("connection");
    connection.execute(sql, params).await.expect(sql);
}

async fn count(db: &Database, sql: &str, params: impl crate::db::IntoParams) -> i64 {
    let connection = db.guard().await.expect("connection");
    let mut rows = connection.query(sql, params).await.expect(sql);
    rows.next()
        .await
        .expect("row")
        .expect("count row")
        .get::<i64>(0)
        .expect("count")
}

async fn seed_session(db: &Database, stream: &str) {
    execute(
        db,
        "INSERT INTO sm_sessions (stream_id, user_id, full_jid, inbound_count, \
             outbound_count, last_acked, detached_at_ms, max_resume_duration_ms, \
             carbons_enabled, roster_interested, presence_available, presence_priority) \
         VALUES (?, ?, ?, 0, 5, 1, 0, 300000, 0, 0, 1, 0)",
        crate::db_params![stream, ALICE, format!("{ALICE}/phone")],
    )
    .await;
}

async fn seed_frame(db: &Database, stream: &str, sequence: i64, wire: &str) {
    execute(
        db,
        "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms) \
         VALUES (?, ?, ?, ?)",
        crate::db_params![stream, sequence, wire, 1_700_000_000_000_i64 + sequence],
    )
    .await;
}

/// An offline row the session claimed and sent at `sequence`.
async fn seed_claimed_row(db: &Database, row: &str, stream: &str, sequence: i64, wire: &str) {
    execute(
        db,
        "INSERT INTO pending_delivery (row_id, recipient_jid, original_receipt_at, \
             payload_kind, transient_xml, flushed_in_session, outbound_sequence) \
         VALUES (?, ?, 1700000000000, 'transient', ?, ?, ?)",
        crate::db_params![row, ALICE, wire, stream, sequence],
    )
    .await;
}

async fn seed_durable_session(db: &Database, session: &str) {
    execute(
        db,
        "INSERT INTO sm_durable_sessions (session_id, resume_token_sha256, full_jid, state, \
             generation, binding, bare_jid, auth_context_id, auth_context_version, \
             principal_auth_epoch, inbound_handled, outbound_acked, outbound_exposure_ceiling, \
             outbound_appended, outbound_unacked_bytes, max_resume_secs, enable_operation, \
             last_operation, updated_at_ms) \
         VALUES (?, 'token', ?, 2, 1, 'binding', ?, 'context', 1, 1, 0, 0, 8, 1, 10, 300, \
             'enable', 'enable', 0)",
        crate::db_params![session, format!("{ALICE}/tablet"), ALICE],
    )
    .await;
    seed_session(db, session).await;
    seed_frame(db, session, 1, &chat("durable", "carol@example.test/desk")).await;
}

/// A legacy session's retained message reaches offline storage once, a frame
/// that is a replay copy of a claimed offline row is not stored again, an
/// acknowledged claimed row is deleted, the remaining claim is released and
/// a durable session's rows are left alone. A rerun changes nothing.
#[tokio::test]
async fn the_drain_moves_each_retained_message_offline_once() {
    let db = database().await;
    seed_session(&db, LEGACY).await;
    // Sequence 1 was acknowledged: its claimed row goes, its frame is gone.
    seed_claimed_row(
        &db,
        "acked-row",
        LEGACY,
        1,
        &chat("acked", "dan@example.test"),
    )
    .await;
    execute(
        &db,
        "INSERT INTO sm_acknowledged_windows (stream_id, from_exclusive, to_inclusive) \
         VALUES (?, 0, 1)",
        crate::db_params![LEGACY],
    )
    .await;
    // Sequence 2 replays a claimed offline row.
    seed_claimed_row(
        &db,
        "replayed-row",
        LEGACY,
        2,
        &chat("offline", "dan@example.test"),
    )
    .await;
    seed_frame(&db, LEGACY, 2, &chat("offline", "dan@example.test")).await;
    seed_frame(&db, LEGACY, 3, &chat("retained", "bob@example.test/desk")).await;
    seed_frame(&db, LEGACY, 4, &iq()).await;
    // A frame of a session whose row was never written is drained too.
    seed_frame(
        &db,
        "orphan-stream",
        1,
        &chat("orphan", "erin@example.test"),
    )
    .await;
    seed_durable_session(&db, DURABLE).await;

    let report = drain_legacy_sm_sessions(&db).await.expect("drain");
    assert_eq!(
        report,
        LegacyDrainReport {
            sessions: 2,
            stored: 2,
            released: 1,
            acknowledged: 1,
            dropped: 1,
        }
    );

    let offline = |id: &'static str| {
        let db = db.clone();
        async move {
            count(
                &db,
                "SELECT COUNT(*) FROM pending_delivery WHERE transient_xml LIKE ?",
                crate::db_params![format!("%>{id}</body>%")],
            )
            .await
        }
    };
    assert_eq!(offline("retained").await, 1);
    assert_eq!(offline("orphan").await, 1);
    assert_eq!(offline("offline").await, 1, "the replay copy is not stored");
    assert_eq!(offline("acked").await, 0, "the acknowledged row is deleted");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM pending_delivery WHERE flushed_in_session IS NOT NULL",
            (),
        )
        .await,
        0,
        "no offline row stays claimed by a drained session"
    );
    for (table, column) in [
        ("sm_sessions", "stream_id"),
        ("sm_unacked", "stream_id"),
        ("sm_acknowledged_windows", "stream_id"),
    ] {
        assert_eq!(
            count(
                &db,
                &format!("SELECT COUNT(*) FROM {table} WHERE {column} <> ?"),
                crate::db_params![DURABLE],
            )
            .await,
            0,
            "{table} keeps no legacy row"
        );
    }
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM sm_unacked WHERE stream_id = ?",
            crate::db_params![DURABLE],
        )
        .await,
        1,
        "the durable session keeps its frame"
    );

    let before = count(&db, "SELECT COUNT(*) FROM pending_delivery", ()).await;
    assert_eq!(
        drain_legacy_sm_sessions(&db).await.expect("rerun"),
        LegacyDrainReport::default()
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM pending_delivery", ()).await,
        before
    );
}

/// A frame an earlier promotion already transferred, recorded by its
/// promotion receipt, is not stored a second time.
#[tokio::test]
async fn a_frame_with_a_promotion_receipt_is_not_stored_again() {
    let db = database().await;
    seed_session(&db, LEGACY).await;
    seed_frame(&db, LEGACY, 2, &chat("promoted", "bob@example.test/desk")).await;
    execute(
        &db,
        "INSERT INTO pending_delivery_sm_promotions (origin_stream_id, sequence, created_at_ms) \
         VALUES (?, 2, 0)",
        crate::db_params![LEGACY],
    )
    .await;

    let report = drain_legacy_sm_sessions(&db).await.expect("drain");
    assert_eq!(report.sessions, 1);
    assert_eq!(report.stored, 0);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM pending_delivery_sm_promotions",
            ()
        )
        .await,
        0
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM sm_unacked", ()).await, 0);
}
