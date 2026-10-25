use super::{
    Duration, FullJid, InMemorySmPersistence, IngressCustodyDisposition, PersistedIngressAppend,
    PersistedSession, PersistedUnackedStanza, SmAckWindow, SmClaimFence, SmPersistenceStorage,
    SmSessionId, Stanza,
};
use chrono::Utc;

use crate::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};

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
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
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
    // Build the typed Message via the project's XML hard-rule
    // builders — Element::builder + Body::new — instead of
    // format!-ing an XML string. The fixture stays portable across
    // any future xmpp-parsers minidom upgrades that change the
    // string-form XML shape (whitespace, attribute order, etc.).
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), format!("m{sequence}"));
    PersistedUnackedStanza {
        ingress_receipts: Vec::new(),
        stream_id: sid(stream_id),
        sequence,
        stanza: Box::new(Stanza::Message(message)),
        original_receipt_at: Utc::now(),
    }
}

fn fixture_principal() -> AuthenticatedPrincipalRef {
    AuthenticatedPrincipalRef::new(
        "alice@example.com".parse().expect("valid bare JID"),
        AuthContextId::new(uuid::Uuid::new_v4()),
        AuthContextVersion::INITIAL,
        PrincipalAuthEpoch::INITIAL,
    )
}

#[tokio::test]
async fn atomic_snapshot_with_principal_round_trips_complete_envelope() {
    let store = InMemorySmPersistence::new();
    let principal = fixture_principal();
    let session = fixture_session("principal-envelope");
    let unacked = vec![fixture_unacked("principal-envelope", 1)];

    store
        .store_session_atomic_with_principal(&principal, session, unacked)
        .await
        .expect("atomically persist snapshot envelope");

    let stream_id = sid("principal-envelope");
    assert!(store
        .get_session(&stream_id)
        .await
        .expect("load snapshot")
        .is_some());
    assert_eq!(
        store
            .list_unacked(&stream_id)
            .await
            .expect("load queue")
            .len(),
        1
    );
    assert_eq!(
        store
            .get_session_principal(&stream_id)
            .await
            .expect("load principal"),
        Some(principal)
    );
}

#[tokio::test]
async fn principal_is_absent_for_a_session_never_stored_with_one() {
    let store = InMemorySmPersistence::new();
    let stream_id = sid("principal-absent");

    store
        .store_session_atomic(fixture_session("principal-absent"), Vec::new())
        .await
        .expect("persist ordinary snapshot");

    assert_eq!(
        store
            .get_session_principal(&stream_id)
            .await
            .expect("load absent principal"),
        None
    );
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
    store.append_unacked(entry).await.unwrap();
    let listed = store.list_unacked(&sid("stream-receipt")).await.unwrap();
    assert_eq!(listed.len(), 1);
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

fn expiry_ingress_append(entry: &PersistedUnackedStanza) -> PersistedIngressAppend {
    PersistedIngressAppend {
        key: crate::stream_management::SmIngressAppendKey {
            message_key: crate::ingress::MessageKey::new(),
            kind: crate::stream_management::SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [7; 32],
            resource: full("alice@example.com/web"),
        },
        accepting_stream: entry.stream_id.clone(),
        sequence: entry.sequence,
        appended_at: entry.original_receipt_at,
        payload: *entry.stanza.clone(),
        original_receipt_at: entry.original_receipt_at,
        disposition: IngressCustodyDisposition::Pending,
    }
}

#[tokio::test]
async fn nonresumable_transition_atomically_trims_valid_wrapping_acknowledgements() {
    let storage = InMemorySmPersistence::new();
    let fence = crate::stream_management::persistence::SmClaimFence::new(
        crate::ownership::NodeIdentity::new("test-node", "test-incarnation"),
        crate::ownership::ClaimEpoch(1),
    );
    for (name, floor, outbound, h, retained) in [
        ("ordinary-expiry", 10_u32, 12_u32, 11_u32, vec![12]),
        ("wrapping-expiry", u32::MAX - 1, 1, 0, vec![1]),
        ("unchanged-ack", 10, 12, 10, vec![11, 12]),
        ("fully-acknowledged", 10, 12, 12, vec![]),
    ] {
        let mut session = fixture_session(name);
        session.last_acked = floor;
        session.outbound_count = outbound;
        let stream = session.stream_id.clone();
        let queue = (1..=outbound.wrapping_sub(floor))
            .map(|distance| fixture_unacked(name, floor.wrapping_add(distance)))
            .collect::<Vec<_>>();
        let appends = queue.iter().map(expiry_ingress_append).collect::<Vec<_>>();
        storage
            .store_session_atomic_with_principal_and_ingress_appends(
                &fixture_principal(),
                session,
                queue,
                appends.clone(),
            )
            .await
            .expect("seed source and ingress custody");
        for _ in 0..2 {
            storage
                .mark_session_nonresumable(&stream, h, &fence, None)
                .await
                .expect("retire existing source idempotently");
            let source = storage
                .get_session(&stream)
                .await
                .expect("read source")
                .expect("retained source");
            assert_eq!(source.last_acked, h);
            assert_eq!(source.outbound_count, outbound);
            assert_eq!(source.max_resume_time, Some(0));
            assert_eq!(source.max_resume_duration, Duration::ZERO);
            for append in &appends {
                let allocation = storage
                    .get_ingress_append(&append.key)
                    .await
                    .expect("read ingress allocation")
                    .expect("retained allocation proof");
                let expected = if retained.contains(&append.sequence) {
                    IngressCustodyDisposition::Pending
                } else {
                    IngressCustodyDisposition::Acknowledged
                };
                assert_eq!(allocation.disposition, expected);
            }
            assert_eq!(
                storage
                    .list_unacked(&stream)
                    .await
                    .expect("retained suffix")
                    .iter()
                    .map(|entry| entry.sequence)
                    .collect::<Vec<_>>(),
                retained
            );
        }
    }
}

#[tokio::test]
async fn nonresumable_transition_rejects_invalid_ack_and_missing_source_without_writes() {
    let storage = InMemorySmPersistence::new();
    let fence = crate::stream_management::persistence::SmClaimFence::new(
        crate::ownership::NodeIdentity::new("test-node", "test-incarnation"),
        crate::ownership::ClaimEpoch(1),
    );
    for (name, floor, outbound, invalid) in [
        ("ack-ahead", 10_u32, 12_u32, 13_u32),
        ("ack-behind", 10, 12, 9),
        ("wrapping-ahead", u32::MAX - 1, 1, 2),
        ("wrapping-behind", u32::MAX - 1, 1, u32::MAX - 2),
    ] {
        let mut session = fixture_session(name);
        session.last_acked = floor;
        session.outbound_count = outbound;
        let stream = session.stream_id.clone();
        let resume_time = session.max_resume_time;
        let resume_duration = session.max_resume_duration;
        let sequences = (1..=outbound.wrapping_sub(floor))
            .map(|distance| floor.wrapping_add(distance))
            .collect::<Vec<_>>();
        let queue = sequences
            .iter()
            .map(|sequence| fixture_unacked(name, *sequence))
            .collect::<Vec<_>>();
        let appends = queue.iter().map(expiry_ingress_append).collect::<Vec<_>>();
        storage
            .store_session_atomic_with_principal_and_ingress_appends(
                &fixture_principal(),
                session,
                queue,
                appends.clone(),
            )
            .await
            .expect("seed source and ingress custody");
        assert!(storage
            .mark_session_nonresumable(&stream, invalid, &fence, None)
            .await
            .is_err());
        let source = storage
            .get_session(&stream)
            .await
            .expect("read source")
            .expect("unchanged source");
        assert_eq!(source.last_acked, floor);
        assert_eq!(source.outbound_count, outbound);
        assert_eq!(source.max_resume_time, resume_time);
        assert_eq!(source.max_resume_duration, resume_duration);
        for append in &appends {
            assert_eq!(
                storage
                    .get_ingress_append(&append.key)
                    .await
                    .expect("read untouched custody")
                    .expect("allocation remains")
                    .disposition,
                IngressCustodyDisposition::Pending
            );
        }
        assert_eq!(
            storage
                .list_unacked(&stream)
                .await
                .expect("unchanged queue")
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            sequences
        );
        storage
            .delete_session(&stream)
            .await
            .expect("delete source");
        assert!(storage
            .mark_session_nonresumable(&stream, floor, &fence, None)
            .await
            .is_err());
        assert!(storage
            .get_session(&stream)
            .await
            .expect("missing source")
            .is_none());
        assert!(storage
            .list_unacked(&stream)
            .await
            .expect("missing queue")
            .is_empty());
    }
}

#[tokio::test]
async fn acknowledged_windows_retain_source_until_exact_settlement() {
    let storage = InMemorySmPersistence::new();
    let fence = SmClaimFence::new(
        crate::ownership::NodeIdentity::new("node", "incarnation"),
        crate::ownership::ClaimEpoch(1),
    );
    let stream = sid("ack-journal");
    let mut source = fixture_session(stream.as_str());
    source.last_acked = u32::MAX;
    source.outbound_count = 1;
    storage
        .store_session_atomic(
            source,
            vec![
                fixture_unacked(stream.as_str(), 0),
                fixture_unacked(stream.as_str(), 1),
            ],
        )
        .await
        .unwrap();
    assert!(storage
        .pending_claim_protected_streams()
        .await
        .unwrap()
        .contains(&stream));
    storage
        .acknowledge_claimed_session(&stream, 0, &fence, None)
        .await
        .unwrap();
    let window = SmAckWindow {
        from_exclusive: u32::MAX,
        to_inclusive: 0,
    };
    assert_eq!(
        storage.pending_acknowledgements(&stream).await.unwrap(),
        vec![window]
    );
    assert_eq!(
        storage.pending_claim_protected_streams().await.unwrap(),
        vec![stream.clone()]
    );
    storage
        .acknowledge_claimed_session(&stream, 0, &fence, None)
        .await
        .unwrap();
    assert_eq!(
        storage.pending_acknowledgements(&stream).await.unwrap(),
        vec![window]
    );
    let source = storage.get_session(&stream).await.unwrap().unwrap();
    assert_eq!(source.last_acked, 0);
    assert_eq!(source.max_resume_time, Some(60));
    assert!(storage.delete_session(&stream).await.is_err());
    assert_eq!(storage.list_unacked(&stream).await.unwrap()[0].sequence, 1);
    storage
        .confirm_acknowledgement(
            &stream,
            SmAckWindow {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            &fence,
            None,
        )
        .await
        .unwrap();
    assert!(storage.delete_session(&stream).await.is_err());
    for _ in 0..2 {
        storage
            .confirm_acknowledgement(&stream, window, &fence, None)
            .await
            .unwrap();
    }
    storage.delete_session(&stream).await.unwrap();
    assert!(storage
        .pending_claim_protected_streams()
        .await
        .unwrap()
        .is_empty());
}
