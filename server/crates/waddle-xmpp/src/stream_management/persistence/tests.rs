use super::*;
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
        shadow_ordinal: crate::stream_management::ShadowOrdinal::ZERO,
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
