use super::*;
use chrono::Utc;

fn full(s: &str) -> FullJid {
    s.parse().unwrap()
}

fn sid(s: &str) -> SmSessionId {
    SmSessionId::new(s)
}

fn fixture_session(stream_id: &str) -> PersistedSession {
    PersistedSession {
        stream_id: sid(stream_id),
        user_id: "alice".to_string(),
        jid: full("alice@example.com/web"),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: Some(9),
        max_resume_time: Some(60),
        detached_at: Utc::now(),
        max_resume_duration: Duration::from_secs(60),
        carbons_enabled: true,
        roster_interested: true,
        blocklist_interested: true,
        presence_available: true,
        presence_show: None,
        presence_status: None,
        presence_priority: 1,
        presence_payloads: Vec::new(),
    }
}

fn fixture_unacked(stream_id: &str, sequence: u32) -> PersistedUnackedStanza {
    fixture_unacked_with_body(stream_id, sequence, &format!("m{sequence}"))
}

fn fixture_unacked_with_body(stream_id: &str, sequence: u32, body: &str) -> PersistedUnackedStanza {
    // Build the typed Message via the project's XML hard-rule
    // builders — Element::builder + Body::new — instead of
    // format!-ing an XML string. The fixture stays portable across
    // any future xmpp-parsers minidom upgrades that change the
    // string-form XML shape (whitespace, attribute order, etc.).
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    PersistedUnackedStanza {
        stream_id: sid(stream_id),
        sequence,
        stanza: Box::new(Stanza::Message(message)),
        original_receipt_at: Utc::now(),
        purpose: SmUnackedStanzaPurpose::Application,
    }
}

fn message_body(row: &PersistedUnackedStanza) -> Option<&str> {
    match row.stanza.as_ref() {
        Stanza::Message(message) => message.bodies.values().next().map(String::as_str),
        Stanza::Iq(_) | Stanza::Presence(_) => None,
    }
}

#[tokio::test]
async fn upsert_get_round_trip() {
    let store = InMemorySmPersistence::new();
    let s = fixture_session("stream-1");
    store.upsert_session(s.clone()).await.unwrap();
    let loaded = store.get_session(&sid("stream-1")).await.unwrap().unwrap();
    assert_eq!(loaded.user_id, s.user_id);
    assert!(loaded.carbons_enabled);
    assert!(loaded.blocklist_interested);
    assert_eq!(loaded.replay_gap_through, Some(9));
}

#[tokio::test]
async fn ack_through_drops_only_acked_sequences() {
    let store = InMemorySmPersistence::new();
    for seq in 1..=4 {
        store
            .append_unacked(fixture_unacked("stream-1", seq))
            .await
            .unwrap();
    }
    let dropped = store.ack_through(&sid("stream-1"), 2).await.unwrap();
    assert_eq!(dropped, 2);
    let remaining = store.list_unacked(&sid("stream-1")).await.unwrap();
    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining[0].sequence, 3);
    assert_eq!(remaining[1].sequence, 4);
}

#[tokio::test]
async fn delete_session_clears_unacked_too() {
    let store = InMemorySmPersistence::new();
    store
        .upsert_session(fixture_session("stream-1"))
        .await
        .unwrap();
    store
        .append_unacked(fixture_unacked("stream-1", 1))
        .await
        .unwrap();
    store.delete_session(&sid("stream-1")).await.unwrap();
    assert!(store.get_session(&sid("stream-1")).await.unwrap().is_none());
    assert!(store
        .list_unacked(&sid("stream-1"))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn list_expired_returns_only_past_sessions() {
    let store = InMemorySmPersistence::new();
    let now = Utc::now();
    let mut past = fixture_session("expired");
    past.detached_at = now - chrono::Duration::seconds(120);
    past.max_resume_duration = Duration::from_secs(60);
    let mut future = fixture_session("active");
    future.detached_at = now;
    future.max_resume_duration = Duration::from_secs(600);

    store.upsert_session(past).await.unwrap();
    store.upsert_session(future).await.unwrap();
    let expired = store.list_expired_sessions(now).await.unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].stream_id, sid("expired"));
}

#[tokio::test]
async fn persisted_unacked_round_trips_original_receipt_at() {
    // Issue #209 PR #361: `original_receipt_at` is the
    // server-side receipt time of the original stanza (NOT
    // append/list time). The Q6 SM-expiry promotion path
    // consumes this for the XEP-0203 `<delay/>` stamp on offline
    // replays per XEP-0203 §4.1 + XEP-0198 §5 line 364.
    //
    // Verify the value supplied at append time round-trips
    // verbatim through `list_unacked` — i.e. the storage layer
    // does NOT stamp `Utc::now()` at write or read time.
    let store = InMemorySmPersistence::new();
    let receipt_time =
        chrono::DateTime::<Utc>::from_timestamp_millis(1_700_000_000_000).expect("valid millis");
    let mut entry = fixture_unacked("stream-receipt", 1);
    entry.original_receipt_at = receipt_time;
    entry.purpose = SmUnackedStanzaPurpose::ResumeBarrier;
    store.append_unacked(entry).await.unwrap();
    let listed = store.list_unacked(&sid("stream-receipt")).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].purpose,
        SmUnackedStanzaPurpose::ResumeBarrier,
        "typed replay purpose must survive in-memory persistence"
    );
    assert_eq!(
        listed[0].original_receipt_at, receipt_time,
        "original_receipt_at must round-trip exactly (not be re-stamped \
         at write or read time)"
    );
}

/// Issue #209 PR #405: the trait default for
/// `list_all_sessions_with_unacked` falls back to N+1; verify
/// it returns sessions paired with their unacked queues. The
/// libSQL backend overrides with a single LEFT JOIN — that
/// override is exercised separately in
/// `server/crates/waddle-server/src/sm_persistence.rs` tests.
#[tokio::test]
async fn list_all_sessions_with_unacked_groups_by_session() {
    let store = InMemorySmPersistence::new();
    // Session A: 0 unacked rows.
    store
        .upsert_session(fixture_session("alpha"))
        .await
        .unwrap();
    // Session B: 2 unacked rows.
    store.upsert_session(fixture_session("beta")).await.unwrap();
    store
        .append_unacked(fixture_unacked("beta", 1))
        .await
        .unwrap();
    store
        .append_unacked(fixture_unacked("beta", 2))
        .await
        .unwrap();
    // Session C: 1 unacked row.
    store
        .upsert_session(fixture_session("gamma"))
        .await
        .unwrap();
    store
        .append_unacked(fixture_unacked("gamma", 1))
        .await
        .unwrap();

    let mut grouped = store.list_all_sessions_with_unacked().await.unwrap();
    // Sort by stream_id for deterministic assertions (the trait
    // doesn't mandate ordering since the in-memory backend uses a
    // HashMap).
    grouped.sort_by(|a, b| a.0.stream_id.as_str().cmp(b.0.stream_id.as_str()));
    assert_eq!(grouped.len(), 3);
    assert_eq!(grouped[0].0.stream_id.as_str(), "alpha");
    assert!(grouped[0].1.is_empty(), "session with no unacked");
    assert_eq!(grouped[1].0.stream_id.as_str(), "beta");
    assert_eq!(grouped[1].1.len(), 2);
    assert_eq!(grouped[2].0.stream_id.as_str(), "gamma");
    assert_eq!(grouped[2].1.len(), 1);
}

#[tokio::test]
async fn list_session_ids_returns_sorted_direct_keys() {
    let store = InMemorySmPersistence::new();
    store
        .upsert_session(fixture_session("stream-b"))
        .await
        .unwrap();
    store
        .upsert_session(fixture_session("stream-a"))
        .await
        .unwrap();

    assert_eq!(
        store.list_session_ids().await.unwrap(),
        vec![sid("stream-a"), sid("stream-b")]
    );
}

/// Issue #209 PR #405: the trait default for
/// `store_session_atomic` falls back to delete + upsert + N appends.
/// Verify the success path produces the expected complete snapshot.
#[tokio::test]
async fn store_session_atomic_writes_session_and_unacked_together() {
    let store = InMemorySmPersistence::new();
    let session = fixture_session("atomic-1");
    let unacked = vec![
        fixture_unacked("atomic-1", 1),
        fixture_unacked("atomic-1", 2),
        fixture_unacked("atomic-1", 3),
    ];
    store.store_session_atomic(session, unacked).await.unwrap();
    assert!(store.get_session(&sid("atomic-1")).await.unwrap().is_some());
    let listed = store.list_unacked(&sid("atomic-1")).await.unwrap();
    assert_eq!(listed.len(), 3);
}

#[tokio::test]
async fn store_session_atomic_replaces_existing_unacked_snapshot() {
    let store = InMemorySmPersistence::new();
    store
        .store_session_atomic(
            fixture_session("atomic-replace"),
            vec![
                fixture_unacked("atomic-replace", 1),
                fixture_unacked("atomic-replace", 2),
            ],
        )
        .await
        .unwrap();

    store
        .store_session_atomic(
            fixture_session("atomic-replace"),
            vec![
                fixture_unacked("atomic-replace", 2),
                fixture_unacked("atomic-replace", 3),
            ],
        )
        .await
        .unwrap();

    let listed = store.list_unacked(&sid("atomic-replace")).await.unwrap();
    let sequences: Vec<u32> = listed.iter().map(|entry| entry.sequence).collect();
    assert_eq!(
        sequences,
        vec![2, 3],
        "full detached snapshots must replace prior unacked rows, not append duplicates"
    );
}

#[tokio::test]
async fn same_id_replacement_preserves_predecessor_as_exact_terminal_generation() {
    let store = InMemorySmPersistence::new();
    let stream_id = sid("same-id");
    store
        .store_session_atomic(
            fixture_session(stream_id.as_str()),
            vec![fixture_unacked_with_body(
                stream_id.as_str(),
                1,
                "predecessor",
            )],
        )
        .await
        .unwrap();
    assert_eq!(store.record_promotion_failure(&stream_id).await.unwrap(), 1);
    assert_eq!(store.record_promotion_failure(&stream_id).await.unwrap(), 2);

    let generation_id = SmSessionGenerationId::new();
    let key = SmTerminalGenerationKey::new(stream_id.clone(), generation_id);
    let predecessor = PersistedTerminalGeneration::new(
        key.clone(),
        PersistedSmSnapshot::new(
            fixture_session(stream_id.as_str()),
            vec![fixture_unacked_with_body(
                stream_id.as_str(),
                1,
                "predecessor",
            )],
        )
        .unwrap(),
    )
    .unwrap();
    let successor = PersistedSmSnapshot::new(
        fixture_session(stream_id.as_str()),
        vec![fixture_unacked_with_body(
            stream_id.as_str(),
            1,
            "successor",
        )],
    )
    .unwrap();

    store
        .replace_resumable_session_atomic(successor, Some(predecessor))
        .await
        .unwrap();

    let current = store.list_unacked(&stream_id).await.unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(message_body(&current[0]), Some("successor"));
    let terminal = store
        .get_terminal_generation(&key)
        .await
        .unwrap()
        .expect("predecessor terminal generation");
    assert_eq!(terminal.promotion_attempts(), 2);
    assert_eq!(terminal.snapshot().unacked().len(), 1);
    assert_eq!(
        message_body(&terminal.snapshot().unacked()[0]),
        Some("predecessor")
    );
    let scan = store.list_terminal_generations().await.unwrap();
    assert!(matches!(
        scan.as_slice(),
        [TerminalGenerationScanEntry::Persisted(terminal)] if terminal.key() == &key
    ));
    let targeted = store
        .list_terminal_generations_for_stream(&stream_id)
        .await
        .unwrap();
    assert!(matches!(
        targeted.as_slice(),
        [TerminalGenerationScanEntry::Persisted(terminal)] if terminal.key() == &key
    ));
    assert_eq!(
        generation_id.to_string().parse::<SmSessionGenerationId>(),
        Ok(generation_id),
        "the exact generation identity must survive durable text encoding"
    );

    // The new logical generation starts with independent retry history.
    assert_eq!(store.record_promotion_failure(&stream_id).await.unwrap(), 1);
    assert_eq!(
        store
            .get_terminal_generation(&key)
            .await
            .unwrap()
            .unwrap()
            .promotion_attempts(),
        2
    );
}

#[tokio::test]
async fn terminal_prune_and_delete_are_generation_exact() {
    let store = InMemorySmPersistence::new();
    let stream_id = sid("generation-exact");
    let key = SmTerminalGenerationKey::new(stream_id.clone(), SmSessionGenerationId::new());
    let predecessor = PersistedTerminalGeneration::new(
        key.clone(),
        PersistedSmSnapshot::new(
            fixture_session(stream_id.as_str()),
            vec![
                fixture_unacked_with_body(stream_id.as_str(), 1, "predecessor-one"),
                fixture_unacked_with_body(stream_id.as_str(), 2, "predecessor-two"),
            ],
        )
        .unwrap(),
    )
    .unwrap();
    let successor = PersistedSmSnapshot::new(
        fixture_session(stream_id.as_str()),
        vec![fixture_unacked_with_body(
            stream_id.as_str(),
            1,
            "successor-one",
        )],
    )
    .unwrap();
    store
        .replace_resumable_session_atomic(successor, Some(predecessor))
        .await
        .unwrap();

    assert_eq!(store.delete_terminal_unacked(&key, &[1]).await.unwrap(), 1);
    let terminal = store.get_terminal_generation(&key).await.unwrap().unwrap();
    assert_eq!(
        terminal
            .snapshot()
            .unacked()
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        vec![2]
    );
    let current = store.list_unacked(&stream_id).await.unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(message_body(&current[0]), Some("successor-one"));

    store.delete_terminal_generation(&key).await.unwrap();
    assert!(store.get_terminal_generation(&key).await.unwrap().is_none());
    assert!(store.has_durable_work(&stream_id).await.unwrap());
    assert!(store.get_session(&stream_id).await.unwrap().is_some());

    store.delete_session(&stream_id).await.unwrap();
    assert!(!store.has_durable_work(&stream_id).await.unwrap());
}

#[tokio::test]
async fn snapshot_constructors_reject_cross_stream_rows_before_storage() {
    let store = InMemorySmPersistence::new();
    let error = PersistedSmSnapshot::new(
        fixture_session("session-a"),
        vec![fixture_unacked("session-b", 1)],
    )
    .expect_err("cross-stream queue must be rejected");
    assert!(matches!(
        error,
        PersistedSmSnapshotError::UnackedStreamMismatch { .. }
    ));
    assert!(!store.has_durable_work(&sid("session-a")).await.unwrap());

    let snapshot = PersistedSmSnapshot::new(fixture_session("session-a"), Vec::new()).unwrap();
    let error = PersistedTerminalGeneration::new(
        SmTerminalGenerationKey::new(sid("session-b"), SmSessionGenerationId::new()),
        snapshot,
    )
    .expect_err("terminal key must match its snapshot");
    assert!(matches!(
        error,
        PersistedSmSnapshotError::TerminalStreamMismatch { .. }
    ));
}
