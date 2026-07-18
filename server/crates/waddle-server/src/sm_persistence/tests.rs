use super::*;
use chrono::TimeZone;
use std::time::Duration;

fn full(s: &str) -> FullJid {
    s.parse().unwrap()
}

fn fixed_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, 12, 30, 0).unwrap()
}

fn fixture_session(stream_id: &str) -> PersistedSession {
    PersistedSession {
        stream_id: SmSessionId::new(stream_id),
        user_id: "alice".to_string(),
        jid: full("alice@example.com/web"),
        inbound_count: 7,
        outbound_count: 12,
        last_acked: 10,
        replay_gap_through: Some(9),
        max_resume_time: Some(60),
        detached_at: fixed_time(),
        max_resume_duration: Duration::from_secs(60),
        carbons_enabled: true,
        roster_interested: true,
        blocklist_interested: true,
        presence_available: true,
        presence_show: Some(Show::Chat),
        presence_status: Some("at the keyboard".to_string()),
        presence_priority: 5,
        presence_payloads: Vec::new(),
    }
}

fn fixture_unacked(stream_id: &str, sequence: u32) -> PersistedUnackedStanza {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), format!("m{sequence}"));
    PersistedUnackedStanza {
        stream_id: SmSessionId::new(stream_id),
        sequence,
        stanza: Box::new(Stanza::Message(message)),
        original_receipt_at: fixed_time(),
        purpose: SmUnackedStanzaPurpose::Application,
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn clustering_refuses_portable_sm_persistence() {
    let global_db = crate::db::Database::in_memory("cluster-sm-portable-rejected")
        .await
        .expect("in-memory global database");
    let error = match open_for_cluster_mode(None, true, None, &global_db).await {
        Ok(_) => panic!("cluster mode must never fall back to unfenced portable persistence"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        SmPersistenceError::ClusterRequiresPostgres { .. }
    ));
}

#[tokio::test]
async fn round_trip_session_preserves_every_field() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    let s = fixture_session("stream-1");
    storage.upsert_session(s.clone()).await.unwrap();
    let loaded = storage
        .get_session(&SmSessionId::new("stream-1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.stream_id, s.stream_id);
    assert_eq!(loaded.user_id, s.user_id);
    assert_eq!(loaded.jid, s.jid);
    assert_eq!(loaded.inbound_count, s.inbound_count);
    assert_eq!(loaded.outbound_count, s.outbound_count);
    assert_eq!(loaded.last_acked, s.last_acked);
    assert_eq!(loaded.replay_gap_through, s.replay_gap_through);
    assert_eq!(loaded.max_resume_time, s.max_resume_time);
    assert_eq!(loaded.detached_at, s.detached_at);
    assert_eq!(loaded.max_resume_duration, s.max_resume_duration);
    assert_eq!(loaded.carbons_enabled, s.carbons_enabled);
    assert_eq!(loaded.roster_interested, s.roster_interested);
    assert_eq!(loaded.blocklist_interested, s.blocklist_interested);
    assert_eq!(loaded.presence_available, s.presence_available);
    assert_eq!(loaded.presence_show, s.presence_show);
    assert_eq!(loaded.presence_status, s.presence_status);
    assert_eq!(loaded.presence_priority, s.presence_priority);
    assert_eq!(loaded.presence_payloads, s.presence_payloads);
}

/// #1206: the durable shape must carry the resource's own presence
/// extension payloads (XEP-0115 `<c/>` caps, XEP-0319 `<idle/>`) so a
/// session rehydrated from storage relays them verbatim on probe instead
/// of coming back caps-less. The payloads survive a serialize→TEXT→parse
/// round-trip, verbatim and in order.
#[tokio::test]
async fn round_trip_session_preserves_presence_payloads() {
    use xmpp_parsers::minidom::Element;

    let caps: Element = r#"<c xmlns='http://jabber.org/protocol/caps' hash='sha-1' node='https://example.com/client' ver='zHyEOgxTrkpSdGcQKH8EFPLsriY='/>"#
        .parse()
        .expect("valid XEP-0115 caps element");
    let idle: Element = r#"<idle xmlns='urn:xmpp:idle:1' since='2026-07-08T10:00:00+00:00'/>"#
        .parse()
        .expect("valid XEP-0319 idle element");

    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    let mut s = fixture_session("stream-payloads");
    s.presence_payloads = vec![caps.clone(), idle.clone()];
    storage.upsert_session(s.clone()).await.unwrap();

    let loaded = storage
        .get_session(&SmSessionId::new("stream-payloads"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.presence_payloads,
        vec![caps, idle],
        "durable get_session must return the stored presence payloads verbatim and in order"
    );
}

#[tokio::test]
async fn upsert_session_persists_with_all_nullable_bigint_fields_unset() {
    // Regression for the SM-detach failure mode where every detach
    // attempt failed on Postgres because `replay_gap_through` /
    // `max_resume_time` came in as `None` and the bind path folded
    // every `None` onto an untyped null that the Postgres binder
    // typed as TEXT — rejected by the `bigint` column. SQLite is
    // type-loose enough not to trip on the bug directly, but
    // exercising the all-None path here pins the API surface: writes
    // succeed and reads round-trip `None` to `None`.
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    let mut s = fixture_session("stream-null-fields");
    s.replay_gap_through = None;
    s.max_resume_time = None;
    s.presence_show = None;
    s.presence_status = None;
    storage.upsert_session(s.clone()).await.unwrap();
    let loaded = storage
        .get_session(&SmSessionId::new("stream-null-fields"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.replay_gap_through, None);
    assert_eq!(loaded.max_resume_time, None);
    assert_eq!(loaded.presence_show, None);
    assert_eq!(loaded.presence_status, None);
}

#[tokio::test]
async fn upsert_replaces_existing_session() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    let mut s = fixture_session("stream-1");
    storage.upsert_session(s.clone()).await.unwrap();
    s.inbound_count = 99;
    s.presence_priority = -1;
    storage.upsert_session(s.clone()).await.unwrap();
    let loaded = storage
        .get_session(&SmSessionId::new("stream-1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.inbound_count, 99);
    assert_eq!(loaded.presence_priority, -1);
}

#[tokio::test]
async fn list_unacked_orders_ascending_by_sequence() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    for seq in [3u32, 1, 4, 2] {
        storage
            .append_unacked(fixture_unacked("stream-1", seq))
            .await
            .unwrap();
    }
    let rows = storage
        .list_unacked(&SmSessionId::new("stream-1"))
        .await
        .unwrap();
    let seqs: Vec<u32> = rows.iter().map(|r| r.sequence).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn unacked_purpose_round_trips_and_legacy_insert_defaults_to_application() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    let mut barrier = fixture_unacked("purpose-stream", 1);
    barrier.purpose = SmUnackedStanzaPurpose::ResumeBarrier;
    storage.append_unacked(barrier).await.unwrap();

    let legacy = fixture_unacked("purpose-stream", 2);
    let legacy_xml = serialize_stanza(&legacy.stanza).unwrap();
    storage
        .execute(
            "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms) \
             VALUES (?, ?, ?, ?)",
            crate::db_params![
                "purpose-stream".to_string(),
                2i64,
                legacy_xml,
                legacy.original_receipt_at.timestamp_millis(),
            ],
        )
        .await
        .unwrap();

    let rows = storage
        .list_unacked(&SmSessionId::new("purpose-stream"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].purpose, SmUnackedStanzaPurpose::ResumeBarrier);
    assert_eq!(rows[1].purpose, SmUnackedStanzaPurpose::Application);
}

#[tokio::test]
async fn legacy_sm_unacked_schema_is_backfilled_with_application_purpose() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("legacy-sm-purpose.sqlite");
    let url = format!("sqlite://{}", path.to_str().expect("UTF-8 path"));
    let legacy = fixture_unacked("legacy-purpose-stream", 1);
    let legacy_xml = serialize_stanza(&legacy.stanza).unwrap();

    {
        let db = crate::db::Database::from_config(
            "legacy_sm_purpose",
            &crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Sqlite, url.clone()),
        )
        .await
        .unwrap();
        let conn = db.guard().await.unwrap();
        conn.execute(
            "CREATE TABLE sm_unacked (
                stream_id TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                stanza_xml TEXT NOT NULL,
                original_receipt_at_ms INTEGER NOT NULL,
                PRIMARY KEY (stream_id, sequence)
            )",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO sm_unacked \
             (stream_id, sequence, stanza_xml, original_receipt_at_ms) \
             VALUES (?, ?, ?, ?)",
            crate::db_params![
                legacy.stream_id.as_str().to_string(),
                i64::from(legacy.sequence),
                legacy_xml,
                legacy.original_receipt_at.timestamp_millis(),
            ],
        )
        .await
        .unwrap();
    }

    let storage = DatabaseSmPersistence::open(Some(&url)).await.unwrap();
    let rows = storage
        .list_unacked(&SmSessionId::new("legacy-purpose-stream"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].purpose, SmUnackedStanzaPurpose::Application);
}

#[tokio::test]
async fn ack_through_drops_only_acked_sequences() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    for seq in 1..=4 {
        storage
            .append_unacked(fixture_unacked("stream-1", seq))
            .await
            .unwrap();
    }
    let dropped = storage
        .ack_through(&SmSessionId::new("stream-1"), 2)
        .await
        .unwrap();
    assert_eq!(dropped, 2);
    let remaining = storage
        .list_unacked(&SmSessionId::new("stream-1"))
        .await
        .unwrap();
    assert_eq!(
        remaining.iter().map(|r| r.sequence).collect::<Vec<_>>(),
        vec![3, 4]
    );
}

#[tokio::test]
async fn delete_session_clears_unacked_too() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    storage
        .upsert_session(fixture_session("stream-1"))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("stream-1", 1))
        .await
        .unwrap();
    storage
        .delete_session(&SmSessionId::new("stream-1"))
        .await
        .unwrap();
    assert!(storage
        .get_session(&SmSessionId::new("stream-1"))
        .await
        .unwrap()
        .is_none());
    assert!(storage
        .list_unacked(&SmSessionId::new("stream-1"))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn delete_session_releases_stream_lock_entry() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    storage
        .upsert_session(fixture_session("stream-leaky"))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("stream-leaky", 1))
        .await
        .unwrap();
    assert!(
        storage
            .stream_locks
            .contains_key(&SmSessionId::new("stream-leaky")),
        "append_unacked must populate the stream_locks entry"
    );

    storage
        .delete_session(&SmSessionId::new("stream-leaky"))
        .await
        .unwrap();
    assert!(
        !storage
            .stream_locks
            .contains_key(&SmSessionId::new("stream-leaky")),
        "delete_session must drop its local Arc clone so drop_stream_lock can GC the entry"
    );
}

#[tokio::test]
async fn list_expired_filters_by_detached_plus_duration() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    let now = Utc::now();
    let mut past = fixture_session("expired");
    past.detached_at = now - chrono::Duration::seconds(120);
    past.max_resume_duration = Duration::from_secs(60);
    let mut active = fixture_session("active");
    active.detached_at = now;
    active.max_resume_duration = Duration::from_secs(600);
    storage.upsert_session(past).await.unwrap();
    storage.upsert_session(active).await.unwrap();

    let expired = storage.list_expired_sessions(now).await.unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].stream_id, SmSessionId::new("expired"));
}

#[tokio::test]
async fn round_trip_unacked_preserves_typed_stanza() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    storage
        .append_unacked(fixture_unacked("stream-1", 1))
        .await
        .unwrap();
    let rows = storage
        .list_unacked(&SmSessionId::new("stream-1"))
        .await
        .unwrap();
    let body = match &*rows[0].stanza {
        Stanza::Message(m) => m.bodies.values().next().cloned(),
        _ => panic!("expected Message"),
    };
    assert_eq!(body, Some("m1".to_string()));
}

/// Issue #209 PR #405: the libSQL backend overrides
/// `list_all_sessions_with_unacked` with a single LEFT JOIN
/// query (vs the trait default's N+1). Verify the SQL grouping
/// produces correct (PersistedSession, Vec<PersistedUnackedStanza>)
/// tuples for sessions with 0, 1, and N unacked rows.
#[tokio::test]
async fn list_all_sessions_with_unacked_uses_single_join_query() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    // Insert mixed-cardinality fixture: alpha=0, beta=2, gamma=1.
    storage
        .upsert_session(fixture_session("alpha"))
        .await
        .unwrap();
    storage
        .upsert_session(fixture_session("beta"))
        .await
        .unwrap();
    storage
        .upsert_session(fixture_session("gamma"))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("beta", 1))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("beta", 2))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("gamma", 1))
        .await
        .unwrap();

    let grouped = storage.list_all_sessions_with_unacked().await.unwrap();
    // The libSQL backend ORDERs BY stream_id ASC, so the
    // assertions can rely on alphabetical order without an
    // explicit sort.
    assert_eq!(grouped.len(), 3);
    assert_eq!(grouped[0].0.stream_id.as_str(), "alpha");
    assert!(grouped[0].1.is_empty(), "session with no unacked");
    assert_eq!(grouped[1].0.stream_id.as_str(), "beta");
    assert_eq!(grouped[1].1.len(), 2);
    assert_eq!(grouped[1].1[0].sequence, 1);
    assert_eq!(grouped[1].1[1].sequence, 2);
    assert_eq!(grouped[2].0.stream_id.as_str(), "gamma");
    assert_eq!(grouped[2].1.len(), 1);

    // Sanity: the round-tripped unacked stanzas decode back to
    // typed Message values (same shape `list_unacked` returns).
    let body = match &*grouped[1].1[0].stanza {
        Stanza::Message(m) => m.bodies.values().next().cloned(),
        _ => panic!("expected Message"),
    };
    assert_eq!(body, Some("m1".to_string()));

    // The JOIN result MUST equal the N+1 trait default applied
    // to the same data — pin the JOIN's correctness by spot-
    // checking the stream-id ordering directly.
    let mut sessions = grouped
        .iter()
        .map(|(s, _)| s.stream_id.as_str().to_string())
        .collect::<Vec<_>>();
    sessions.sort();
    assert_eq!(sessions, vec!["alpha", "beta", "gamma"]);
}

/// Issue #209 PR #405: `store_session_atomic` wraps the upsert
/// + N appends in a `Database::begin` transaction. Verify the
/// success path produces the same observable state as the
/// non-atomic upsert + appends, and that `get_session` /
/// `list_unacked` see the rows after commit.
#[tokio::test]
async fn store_session_atomic_round_trips_via_transaction() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    let session = fixture_session("atomic-stream");
    let mut unacked = vec![
        fixture_unacked("atomic-stream", 1),
        fixture_unacked("atomic-stream", 2),
        fixture_unacked("atomic-stream", 3),
    ];
    unacked[1].purpose = SmUnackedStanzaPurpose::ResumeBarrier;
    storage
        .store_session_atomic(session, unacked)
        .await
        .unwrap();

    // Session row written.
    let read = storage
        .get_session(&SmSessionId::new("atomic-stream"))
        .await
        .unwrap()
        .expect("session present after atomic write");
    assert_eq!(read.stream_id.as_str(), "atomic-stream");

    // All unacked rows written.
    let listed = storage
        .list_unacked(&SmSessionId::new("atomic-stream"))
        .await
        .unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].sequence, 1);
    assert_eq!(listed[2].sequence, 3);
    assert_eq!(listed[0].purpose, SmUnackedStanzaPurpose::Application);
    assert_eq!(listed[1].purpose, SmUnackedStanzaPurpose::ResumeBarrier);

    let grouped = storage.list_all_sessions_with_unacked().await.unwrap();
    let (_, joined_queue) = grouped
        .iter()
        .find(|(session, _)| session.stream_id.as_str() == "atomic-stream")
        .expect("atomic session in joined cold-start read");
    assert_eq!(
        joined_queue[1].purpose,
        SmUnackedStanzaPurpose::ResumeBarrier
    );
}

/// #1206: the production store path is `store_session` → the OVERRIDDEN
/// `store_session_atomic`, and cold-start restore reads back through the
/// `list_all_sessions_with_unacked` JOIN. Both must carry the resource's
/// presence payloads. This drives the end-to-end write+JOIN-read path and
/// simultaneously guards the JOIN column indices: the unacked stanza must
/// still decode after `presence_payloads` was inserted as a session column.
#[tokio::test]
async fn store_session_atomic_and_join_read_preserve_presence_payloads() {
    use xmpp_parsers::minidom::Element;

    let caps: Element = r#"<c xmlns='http://jabber.org/protocol/caps' hash='sha-1' node='https://example.com/client' ver='zHyEOgxTrkpSdGcQKH8EFPLsriY='/>"#
        .parse()
        .expect("valid XEP-0115 caps element");
    let idle: Element = r#"<idle xmlns='urn:xmpp:idle:1' since='2026-07-08T10:00:00+00:00'/>"#
        .parse()
        .expect("valid XEP-0319 idle element");

    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    let mut session = fixture_session("atomic-payloads");
    session.presence_payloads = vec![caps.clone(), idle.clone()];
    storage
        .store_session_atomic(session, vec![fixture_unacked("atomic-payloads", 1)])
        .await
        .unwrap();

    let grouped = storage.list_all_sessions_with_unacked().await.unwrap();
    let (restored, unacked) = grouped
        .iter()
        .find(|(s, _)| s.stream_id.as_str() == "atomic-payloads")
        .expect("session present in cold-start restore set");

    assert_eq!(
        restored.presence_payloads,
        vec![caps, idle],
        "the atomic store + JOIN restore must preserve the resource's presence payloads"
    );

    // Column-index guard: the unacked stanza still decodes end-to-end after
    // presence_payloads shifted the JOIN's unacked columns by one.
    assert_eq!(unacked.len(), 1, "the unacked stanza must survive the JOIN");
    assert_eq!(unacked[0].sequence, 1);
    let body = match &*unacked[0].stanza {
        Stanza::Message(m) => m.bodies.values().next().cloned(),
        _ => panic!("expected Message"),
    };
    assert_eq!(body, Some("m1".to_string()));
}

/// #1206 hardening (review): a corrupt `presence_payloads` cell must NOT
/// poison the whole session on restore. Presence caps are non-essential and
/// self-heal on the client's next presence broadcast, but the session's
/// XEP-0198 unacked message queue is precious — decode degrades to caps-less
/// and keeps the session (and its queue) instead of dropping it.
#[tokio::test]
async fn corrupt_presence_payloads_cell_degrades_to_caps_less_not_session_drop() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    storage
        .store_session_atomic(
            fixture_session("corrupt-payloads"),
            vec![fixture_unacked("corrupt-payloads", 1)],
        )
        .await
        .unwrap();

    // Simulate storage-layer corruption of ONLY the presence_payloads cell
    // (unclosed element — parses to an error).
    storage
        .execute(
            "UPDATE sm_sessions SET presence_payloads = ? WHERE stream_id = ?",
            crate::db_params![
                "<c xmlns='urn:x'".to_string(),
                "corrupt-payloads".to_string(),
            ],
        )
        .await
        .unwrap();

    // get_session: session survives, caps-less.
    let loaded = storage
        .get_session(&SmSessionId::new("corrupt-payloads"))
        .await
        .unwrap()
        .expect("session must survive a corrupt presence_payloads cell");
    assert!(
        loaded.presence_payloads.is_empty(),
        "a corrupt payloads cell degrades to caps-less"
    );

    // Cold-start JOIN restore: the session AND its unacked queue survive
    // (the corrupt cell must not turn it into a dropped poison pill).
    let grouped = storage.list_all_sessions_with_unacked().await.unwrap();
    let (session, unacked) = grouped
        .iter()
        .find(|(s, _)| s.stream_id.as_str() == "corrupt-payloads")
        .expect("session must not be dropped as a poison pill");
    assert!(session.presence_payloads.is_empty());
    assert_eq!(
        unacked.len(),
        1,
        "the unacked message queue must be preserved despite the corrupt payloads cell"
    );
}

/// Issue #209 PR #405 (Greptile/Copilot P2 review):
/// `store_session_atomic` MUST roll back the entire transaction
/// on any mid-batch failure — including the session-row update.
/// Force a fault by passing two unacked stanzas with the same
/// sequence; the second INSERT trips the `(stream_id, sequence)`
/// PRIMARY KEY constraint inside the transaction.
///
/// After the failed call, both `get_session` and `list_unacked`
/// MUST observe the prior state (here: nothing), proving the
/// session row update did NOT commit.
#[tokio::test]
async fn store_session_atomic_rolls_back_on_unacked_constraint_violation() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    let session = fixture_session("rollback-stream");
    // Two stanzas with the SAME sequence — the second INSERT
    // hits the (stream_id, sequence) PRIMARY KEY constraint.
    // Note that the new DELETE-before-INSERT (Greptile P1 fix)
    // clears any pre-existing rows, so the conflict is between
    // the two stanzas in THIS batch, not against pre-existing
    // state.
    let unacked = vec![
        fixture_unacked("rollback-stream", 1),
        fixture_unacked("rollback-stream", 1), // duplicate sequence
    ];
    let result = storage.store_session_atomic(session, unacked).await;
    assert!(
        result.is_err(),
        "duplicate (stream_id, sequence) MUST fail the transaction"
    );

    // Critical assertion: the session row MUST NOT be present.
    // Without `tx.commit()`, dropping the Transaction rolls back
    // every statement including the session upsert.
    let session_after = storage
        .get_session(&SmSessionId::new("rollback-stream"))
        .await
        .unwrap();
    assert!(
        session_after.is_none(),
        "transaction rollback MUST hide the session row update"
    );

    // No unacked rows leaked either.
    let unacked_after = storage
        .list_unacked(&SmSessionId::new("rollback-stream"))
        .await
        .unwrap();
    assert!(
        unacked_after.is_empty(),
        "transaction rollback MUST hide partial unacked inserts"
    );
}

/// Issue #209 PR #405 (Greptile P1 review): `store_session_atomic`
/// must be idempotent against pre-existing unacked rows for the
/// same stream_id (e.g., a previous `persist_delete_session`
/// failure left rows behind). The atomic store DELETEs existing
/// rows inside the transaction before the INSERT loop, so the
/// (stream_id, sequence) PRIMARY KEY constraint can't roll back
/// the session row update.
#[tokio::test]
async fn store_session_atomic_clears_stale_unacked_before_inserting() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    // Pre-seed an unacked row at (stream_id, sequence=1) — this
    // simulates a previous persist_delete_session that failed in
    // the eviction path.
    storage
        .append_unacked(fixture_unacked("retry-stream", 1))
        .await
        .unwrap();
    // Now atomic-store with NEW unacked rows that include the
    // same sequence (1). Without the DELETE-before-INSERT, the
    // INSERT for sequence=1 would fail and roll back the session.
    let session = fixture_session("retry-stream");
    let unacked = vec![
        fixture_unacked("retry-stream", 1),
        fixture_unacked("retry-stream", 2),
    ];
    storage
        .store_session_atomic(session, unacked)
        .await
        .expect("atomic store survives pre-existing unacked rows");

    // Session row written.
    assert!(storage
        .get_session(&SmSessionId::new("retry-stream"))
        .await
        .unwrap()
        .is_some());
    // Exactly the new unacked rows are present (the stale row
    // was DELETEd inside the transaction).
    let listed = storage
        .list_unacked(&SmSessionId::new("retry-stream"))
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].sequence, 1);
    assert_eq!(listed[1].sequence, 2);
}

/// Issue #209 PR #405 (Greptile/Copilot/Qodo P1 review): a
/// single poison-pill `sm_unacked` row MUST NOT brick cold
/// startup. `list_all_sessions_with_unacked` should skip the
/// poisoned session and return the rest.
#[tokio::test]
async fn list_all_sessions_with_unacked_skips_poison_pill_unacked_rows() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    // Two healthy sessions.
    storage
        .upsert_session(fixture_session("alpha"))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("alpha", 1))
        .await
        .unwrap();
    storage
        .upsert_session(fixture_session("gamma"))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("gamma", 1))
        .await
        .unwrap();
    // One poison-pill session: insert a sm_unacked row whose
    // stanza_xml is malformed XML so decode fails. We bypass
    // `append_unacked` (which serializes a typed Stanza) and
    // write the raw poison directly via the underlying db.
    storage
        .upsert_session(fixture_session("beta"))
        .await
        .unwrap();
    storage
        .execute(
            "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms) \
             VALUES (?, ?, ?, ?)",
            crate::db_params![
                "beta".to_string(),
                1i64,
                "not valid xml <<<".to_string(),
                0i64
            ],
        )
        .await
        .expect("insert poison row");

    let grouped = storage.list_all_sessions_with_unacked().await.unwrap();
    // Healthy sessions present; poison-pill session's unacked
    // row was skipped, so beta appears with an empty queue
    // (since the only row failed to decode).
    let stream_ids: Vec<_> = grouped
        .iter()
        .map(|(s, _)| s.stream_id.as_str().to_string())
        .collect();
    assert!(stream_ids.contains(&"alpha".to_string()));
    assert!(stream_ids.contains(&"gamma".to_string()));
    assert!(stream_ids.contains(&"beta".to_string()));
    let beta_unacked = grouped
        .iter()
        .find(|(s, _)| s.stream_id.as_str() == "beta")
        .map(|(_, u)| u);
    assert_eq!(beta_unacked.map(Vec::len), Some(0));
}

#[tokio::test]
async fn corrupt_replay_purpose_quarantines_the_whole_joined_session_group() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    storage
        .upsert_session(fixture_session("alpha"))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("alpha", 1))
        .await
        .unwrap();
    storage
        .upsert_session(fixture_session("beta"))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("beta", 1))
        .await
        .unwrap();

    let poison = fixture_unacked("beta", 2);
    let poison_xml = serialize_stanza(&poison.stanza).unwrap();
    storage
        .execute(
            "INSERT INTO sm_unacked \
             (stream_id, sequence, stanza_xml, original_receipt_at_ms, purpose) \
             VALUES (?, ?, ?, ?, ?)",
            crate::db_params![
                "beta".to_string(),
                2i64,
                poison_xml,
                poison.original_receipt_at.timestamp_millis(),
                "unknown-purpose".to_string(),
            ],
        )
        .await
        .unwrap();

    let grouped = storage.list_all_sessions_with_unacked().await.unwrap();
    assert!(grouped
        .iter()
        .any(|(session, queue)| session.stream_id.as_str() == "alpha" && queue.len() == 1));
    assert!(
        grouped
            .iter()
            .all(|(session, _)| session.stream_id.as_str() != "beta"),
        "an unknown typed purpose must quarantine the whole stream, including earlier rows"
    );
}

/// Regression for #1157: a session row that fails to decode while
/// its unacked rows decode fine MUST have those rows dropped with
/// the group — not appended to the preceding valid session's queue,
/// which would replay one user's stanzas on another user's
/// `<resumed/>` (XEP-0198 §5 retransmission is per-stream).
#[tokio::test]
async fn poison_session_rows_do_not_leak_into_preceding_sessions_queue() {
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    // Valid session "alpha" with one unacked stanza; sorts before
    // "beta" under the query's ORDER BY stream_id ASC.
    storage
        .upsert_session(fixture_session("alpha"))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("alpha", 1))
        .await
        .unwrap();
    // Session "beta" with two healthy unacked stanzas, then corrupt
    // beta's session row so decode_session fails while the unacked
    // rows still decode.
    storage
        .upsert_session(fixture_session("beta"))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("beta", 1))
        .await
        .unwrap();
    storage
        .append_unacked(fixture_unacked("beta", 2))
        .await
        .unwrap();
    storage
        .execute(
            "UPDATE sm_sessions SET presence_show = 'bogus' WHERE stream_id = ?",
            crate::db_params!["beta".to_string()],
        )
        .await
        .expect("corrupt beta session row");

    let grouped = storage.list_all_sessions_with_unacked().await.unwrap();
    // The poison group is fully dropped...
    assert_eq!(grouped.len(), 1);
    let (session, unacked) = &grouped[0];
    assert_eq!(session.stream_id.as_str(), "alpha");
    // ...and alpha's queue holds exactly its own stanza — beta's
    // rows must not have been appended to it.
    assert_eq!(unacked.len(), 1);
    assert_eq!(unacked[0].sequence, 1);
    assert_eq!(unacked[0].stream_id.as_str(), "alpha");
}

/// Regression for #456: existing Postgres SM tables created before
/// the driver-aware BIGINT selector must be widened online. The test
/// exercises both session timestamp fields and unacked receipt
/// timestamps with values that do not fit in Postgres int4.
#[tokio::test]
async fn postgres_handles_i32_overflow_session_and_unacked_timestamps_ms() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (postgres-backed regression for sm persistence BIGINT)"
        );
        return;
    };

    let storage = DatabaseSmPersistence::open(Some(&database_url))
        .await
        .expect("open postgres sm persistence");
    let stream_id = format!("postgres-bigint-{}", uuid::Uuid::new_v4());
    let receipt_ms: i64 = 1_778_000_000_000;
    assert!(
        receipt_ms > i64::from(i32::MAX),
        "test value must exceed Postgres int4 range"
    );
    let receipt_at =
        DateTime::<Utc>::from_timestamp_millis(receipt_ms).expect("valid receipt timestamp");

    let mut session = fixture_session(&stream_id);
    session.detached_at = receipt_at;
    storage
        .upsert_session(session.clone())
        .await
        .expect("BIGINT sm_sessions timestamps accept values past i32::MAX");
    let loaded = storage
        .get_session(&session.stream_id)
        .await
        .expect("load postgres sm session")
        .expect("postgres sm session row");
    assert_eq!(loaded.detached_at, receipt_at);

    let mut unacked = fixture_unacked(&stream_id, 1);
    unacked.original_receipt_at = receipt_at;
    storage
        .append_unacked(unacked)
        .await
        .expect("BIGINT sm_unacked original_receipt_at_ms accepts values past i32::MAX");

    let listed = storage
        .list_unacked(&SmSessionId::new(&stream_id))
        .await
        .expect("list postgres unacked stanzas");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].original_receipt_at, receipt_at);

    storage
        .delete_session(&SmSessionId::new(&stream_id))
        .await
        .expect("cleanup postgres sm rows");
}

#[tokio::test]
async fn delete_unacked_removes_only_named_sequences_for_stream() {
    // Issue #1145: the tombstone scrub needs a targeted durable
    // delete keyed on the (stream_id, sequence) PK so retracted
    // stanzas cannot be rehydrated after a restart. Rows for other
    // sequences and other streams must be untouched.
    let storage = DatabaseSmPersistence::open(None).await.unwrap();
    storage
        .upsert_session(fixture_session("stream-a"))
        .await
        .unwrap();
    storage
        .upsert_session(fixture_session("stream-b"))
        .await
        .unwrap();
    for seq in [1u32, 2, 3] {
        storage
            .append_unacked(fixture_unacked("stream-a", seq))
            .await
            .unwrap();
    }
    storage
        .append_unacked(fixture_unacked("stream-b", 2))
        .await
        .unwrap();

    let removed = storage
        .delete_unacked(&SmSessionId::new("stream-a"), &[1, 3])
        .await
        .unwrap();
    assert_eq!(removed, 2);

    let remaining_a = storage
        .list_unacked(&SmSessionId::new("stream-a"))
        .await
        .unwrap();
    let seqs: Vec<u32> = remaining_a.iter().map(|r| r.sequence).collect();
    assert_eq!(seqs, vec![2]);
    let remaining_b = storage
        .list_unacked(&SmSessionId::new("stream-b"))
        .await
        .unwrap();
    assert_eq!(remaining_b.len(), 1, "other streams' rows are untouched");

    // Deleting sequences that no longer exist is a no-op, not an error.
    let removed = storage
        .delete_unacked(&SmSessionId::new("stream-a"), &[1, 3])
        .await
        .unwrap();
    assert_eq!(removed, 0);
}
