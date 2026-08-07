//! ADR-0017 Phase 3 Slice 6: dedicated XEP-0198 cross-node resume test
//! suite (XEP custom test-suite hard rule).
//!
//! Two-registry simulation: two `InMemorySmSessionRegistry` instances (one
//! per "node"), each with its own `NodeIdentity`, sharing ONE real
//! Postgres-backed `PostgresClaimStore` + `PostgresFencedSmPersistence`
//! (constructed once per node with that node's own identity) — standing in
//! for two nodes sharing one Postgres control plane. The cross-node relay
//! handshake itself (real network, real `RelayActor`) is deliberately out
//! of scope here — a `FakeAsker` stands in for it, performing exactly what
//! a real remote node's `ResumeStealBridge` would (force-detach the "live"
//! session by persisting it), so this suite proves
//! `attempt_cross_node_resume`'s own dispatch/CAS/hydrate logic without a
//! second OS process. The real cross-node live-steal handshake end-to-end
//! (two real processes, real swarm) is `clustering_cluster_e2e.rs`'s own
//! scenario.
//!
//! Postgres-gated on `WADDLE_TEST_POSTGRES_URL` (skips cleanly otherwise).

#![cfg(feature = "clustering")]

use std::sync::Arc;
use std::time::Duration;

use jid::{BareJid, FullJid};
use tokio::sync::Mutex as AsyncMutex;
use waddle_server::clustering::claims::{NodeLeaseStore, PostgresClaimStore};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::pending_delivery::DatabasePendingDeliveryStorage;
use waddle_server::sm_persistence_fenced::PostgresFencedSmPersistence;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, NodeIdentity,
    ResumeIdentityProof, SharedNodeIdentity, StalePredicate,
};
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId, QuotaPolicy};
use waddle_xmpp::stream_management::{
    CrossNodeResumeOutcome, DetachedSession, DetachedUnackedStanza, InMemorySmSessionRegistry,
    RemoteResumeAskOutcome, RemoteResumeAsker, SmClaimCompletion, SmSessionRegistry,
};

/// Serializes every test in this file: they share the mutable control-plane
/// tables (`clustering_claims`/`clustering_nodes`) plus `sm_sessions`/
/// `sm_unacked` in one Postgres and each starts by DELETE-resetting them.
fn serial_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn clean_db() -> Option<Database> {
    let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
    let db = Database::from_config(
        "xep0198-cross-node-resume-test",
        &DatabaseConfig::new(DatabaseDriver::Postgres, url)
            .with_control_plane_pool(waddle_server::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
    )
    .await
    .expect("open test postgres");
    let claims = PostgresClaimStore::new(db.clone());
    claims.ensure_schema().await.expect("ensure claims schema");
    // `sm_sessions`/`sm_unacked` schema lives behind `PostgresFencedSmPersistence`'s
    // own `open` — ensure it once here before the DELETEs below, using a
    // throwaway identity (this instance is discarded immediately after).
    let _ = PostgresFencedSmPersistence::open(
        db.clone(),
        Arc::new(PostgresClaimStore::new(db.clone())),
        SharedNodeIdentity::new(node_identity()),
    )
    .await
    .expect("ensure sm persistence schema");
    let conn = db.guard().await.expect("guard");
    conn.execute("DELETE FROM clustering_claims", ())
        .await
        .expect("clean claims");
    conn.execute("DELETE FROM clustering_nodes", ())
        .await
        .expect("clean nodes");
    conn.execute("DELETE FROM sm_unacked", ())
        .await
        .expect("clean sm_unacked");
    conn.execute("DELETE FROM sm_sessions", ())
        .await
        .expect("clean sm_sessions");
    Some(db)
}

fn node_identity() -> NodeIdentity {
    NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    )
}

fn alice_jid() -> (BareJid, FullJid) {
    let full: FullJid = "alice@example.com/phone".parse().expect("valid full jid");
    (full.to_bare(), full)
}

fn alice_principal(bare: &BareJid) -> AuthenticatedPrincipalRef {
    AuthenticatedPrincipalRef::new(
        bare.clone(),
        AuthContextId::new(uuid::Uuid::new_v4()),
        AuthContextVersion::INITIAL,
        PrincipalAuthEpoch::INITIAL,
    )
}

async fn persisted_principal_columns(
    db: &Database,
    stream_id: &str,
) -> (Option<String>, Option<String>, Option<i64>, Option<i64>) {
    let conn = db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT bare_jid, auth_context_id, auth_context_version, principal_auth_epoch \
             FROM sm_sessions WHERE stream_id = ?",
            waddle_server::db_params![stream_id],
        )
        .await
        .expect("select persisted principal columns");
    let row = rows
        .next()
        .await
        .expect("read persisted principal row")
        .expect("persisted session row");
    (
        row.get(0).expect("bare_jid"),
        row.get(1).expect("auth_context_id"),
        row.get(2).expect("auth_context_version"),
        row.get(3).expect("principal_auth_epoch"),
    )
}

async fn assert_claim_retains_recovery_snapshot(
    registry: &InMemorySmSessionRegistry,
    stream_id: &str,
    expected_jid: &FullJid,
) {
    let reclaimed = registry
        .claim_session(stream_id)
        .await
        .expect("claim_session after rejected resume");
    let Some(reclaimed) = reclaimed else {
        panic!("rejected resume must retain its detached recovery snapshot");
    };
    assert_eq!(reclaimed.jid, *expected_jid);
    registry
        .release_claim(stream_id)
        .await
        .expect("return recovery snapshot to detached state");
}

/// Build a registry for one simulated "node": its own `NodeIdentity`, its
/// own `PostgresFencedSmPersistence` (bound to that identity), sharing the
/// same underlying `PostgresClaimStore`/Postgres database as every other
/// node in the test.
async fn node_registry(
    db: &Database,
    identity: NodeIdentity,
    asker: Option<Arc<dyn RemoteResumeAsker>>,
) -> (Arc<InMemorySmSessionRegistry>, SharedNodeIdentity) {
    let claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    node_registry_with_claim_store(db, identity, claim_store, asker).await
}

/// Same as [`node_registry`], but for callers (the FIX B/C repair suite)
/// that need to observe/intercept this node's own `ClaimStore` calls —
/// e.g. wrapping `ensure_claimed` to inject an ordinary post-CAS-win
/// hydrate failure. `PostgresFencedSmPersistence` is still opened against
/// the SAME `claim_store` handle a real deployment would wire it with
/// (`sm_persistence_fenced.rs`'s own fencing calls through it too), so a
/// wrapper only needs to override the specific method(s) a test cares
/// about.
async fn node_registry_with_claim_store(
    db: &Database,
    identity: NodeIdentity,
    claim_store: Arc<dyn ClaimStore>,
    asker: Option<Arc<dyn RemoteResumeAsker>>,
) -> (Arc<InMemorySmSessionRegistry>, SharedNodeIdentity) {
    let shared_identity = SharedNodeIdentity::new(identity);
    let persistence = PostgresFencedSmPersistence::open(
        db.clone(),
        Arc::clone(&claim_store),
        shared_identity.clone(),
    )
    .await
    .expect("open fenced persistence");
    let mut registry = InMemorySmSessionRegistry::new()
        .with_persistence(Arc::new(persistence))
        .with_claim_store(claim_store, shared_identity.clone());
    if let Some(asker) = asker {
        registry = registry.with_remote_resume_asker(asker);
    }
    (Arc::new(registry), shared_identity)
}

fn detached_session(stream_id: &str, jid: &FullJid) -> DetachedSession {
    DetachedSession {
        stream_id: stream_id.to_string(),
        user_id: jid.to_bare().to_string(),
        jid: jid.clone(),
        inbound_count: 3,
        outbound_count: 7,
        last_acked: 5,
        replay_gap_through: None,
        unacked_stanzas: vec![
            DetachedUnackedStanza {
                sequence: 6,
                stanza_xml: "<message xmlns='jabber:client'><body>six</body></message>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            DetachedUnackedStanza {
                sequence: 7,
                stanza_xml: "<message xmlns='jabber:client'><body>seven</body></message>"
                    .to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
        ],
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: true,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

/// Stands in for the relay + `ResumeStealBridge` layer: simulates node A's
/// answer to a live-handshake ask by persisting `session` into
/// `owner_registry` (exactly what a real force-detach would do) the first
/// time a matching-identity ask arrives, then reporting `Detached`.
struct FakeAsker {
    owner_registry: Arc<InMemorySmSessionRegistry>,
    owner_bare_jid: BareJid,
    live_session: AsyncMutex<Option<DetachedSession>>,
    asks_seen: std::sync::atomic::AtomicUsize,
}

impl FakeAsker {
    fn new(
        owner_registry: Arc<InMemorySmSessionRegistry>,
        owner_bare_jid: BareJid,
        session: DetachedSession,
    ) -> Self {
        Self {
            owner_registry,
            owner_bare_jid,
            live_session: AsyncMutex::new(Some(session)),
            asks_seen: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl RemoteResumeAsker for FakeAsker {
    async fn ask_remote_detach(
        &self,
        _node_id: &str,
        _stream_id: &str,
        requester_bare_jid: &BareJid,
    ) -> RemoteResumeAskOutcome {
        self.asks_seen
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if *requester_bare_jid != self.owner_bare_jid {
            return RemoteResumeAskOutcome::IdentityMismatch;
        }
        let mut guard = self.live_session.lock().await;
        match guard.take() {
            Some(session) => {
                self.owner_registry
                    .store_session(session)
                    .await
                    .expect("owner registry stores the force-detached session");
                RemoteResumeAskOutcome::Detached
            }
            None => RemoteResumeAskOutcome::NotLiveRemotely,
        }
    }
}

/// An asker that always reports the owner unreachable (branch 3).
struct UnreachableAsker;

#[async_trait::async_trait]
impl RemoteResumeAsker for UnreachableAsker {
    async fn ask_remote_detach(
        &self,
        _node_id: &str,
        _stream_id: &str,
        _requester_bare_jid: &BareJid,
    ) -> RemoteResumeAskOutcome {
        RemoteResumeAskOutcome::Unreachable
    }
}

const HANDSHAKE_BUDGET: Duration = Duration::from_secs(2);

/// The durable principal reference is written atomically with the detached
/// snapshot on node A, survives hydration on node B, and remains available
/// while node B owns the claimed hand-off. The unacked queue is intentionally
/// non-empty so this also proves that the same successful resume still carries
/// XEP-0198 replay state rather than only authorization metadata.
#[tokio::test]
async fn detached_principal_hydrates_cross_node_and_preserves_replay() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let principal = alice_principal(&bare);
    let (registry_a, _identity_a) = node_registry(&db, node_identity(), None).await;
    let (registry_b, _identity_b) = node_registry(&db, node_identity(), None).await;
    let stream_id = "stream-principal-cross-node";
    let session = detached_session(stream_id, &full);
    let expected_replay = session.unacked_stanzas.clone();

    registry_a
        .store_session_with_principal(session, principal.clone())
        .await
        .expect("node A atomically stores detached snapshot and principal");

    let (stored_bare, stored_context, stored_version, stored_epoch) =
        persisted_principal_columns(&db, stream_id).await;
    assert_eq!(stored_bare.as_deref(), Some(bare.as_str()));
    assert_eq!(
        stored_context.as_deref(),
        Some(principal.auth_context_id().as_uuid().to_string().as_str())
    );
    assert_eq!(
        stored_version,
        Some(i64::try_from(principal.auth_context_version().get()).expect("version fits i64"))
    );
    assert_eq!(
        stored_epoch,
        Some(i64::try_from(principal.auth_epoch().get()).expect("epoch fits i64"))
    );

    let outcome = registry_b
        .attempt_cross_node_resume(stream_id, &bare, HANDSHAKE_BUDGET)
        .await
        .expect("cross-node claim/hydrate succeeds");
    let CrossNodeResumeOutcome::Claimed(resumed) = outcome else {
        panic!("expected cross-node claim with durable principal");
    };
    assert_eq!(resumed.jid, full);
    // Postgres stores receipt timestamps at millisecond precision; compare
    // at that precision rather than chrono's nanoseconds.
    let replay_key = |stanzas: &[waddle_xmpp::stream_management::DetachedUnackedStanza]| {
        stanzas
            .iter()
            .map(|s| {
                (
                    s.sequence,
                    s.stanza_xml.clone(),
                    s.original_receipt_at.timestamp_millis(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        replay_key(&resumed.unacked_stanzas),
        replay_key(&expected_replay),
        "replay survives hydration"
    );
    assert_eq!(
        registry_b
            .session_principal(stream_id)
            .await
            .expect("read principal after cross-node hydration"),
        Some(principal),
        "node B must read the exact durable principal, never a credential"
    );
}

/// A legacy snapshot has all principal columns NULL. It is not resumable;
/// releasing the tentative claim must return the snapshot to recovery state
/// rather than dropping it or leaving it frozen in `claimed_sessions`.
#[tokio::test]
async fn legacy_null_principal_rejects_without_losing_cross_node_recovery_snapshot() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let (registry_a, _identity_a) = node_registry(&db, node_identity(), None).await;
    let (registry_b, _identity_b) = node_registry(&db, node_identity(), None).await;
    let stream_id = "stream-principal-legacy";
    registry_a
        .store_session(detached_session(stream_id, &full))
        .await
        .expect("node A stores a legacy detached snapshot");

    let outcome = registry_b
        .attempt_cross_node_resume(stream_id, &bare, HANDSHAKE_BUDGET)
        .await
        .expect("cross-node hydration itself succeeds");
    assert!(matches!(outcome, CrossNodeResumeOutcome::Claimed(_)));
    assert_eq!(
        registry_b
            .session_principal(stream_id)
            .await
            .expect("read legacy durable principal"),
        None,
        "all-NULL legacy principal columns must force the route-level not-authorized rejection"
    );
    // The Claimed outcome froze the snapshot in `claimed_sessions`. Perform
    // the release the route's not-authorized rejection performs (the claim
    // guard's unwind) before asserting the snapshot is back in recovery
    // state and reclaimable.
    registry_b
        .release_claim(stream_id)
        .await
        .expect("route-level rejection releases the tentative claim");
    assert_claim_retains_recovery_snapshot(&registry_b, stream_id, &full).await;
}

/// SM persistence carries exactly the non-secret durable principal tuple. This
/// guards against accidentally extending the cross-node recovery row with a
/// bearer credential or password-derived material.
#[tokio::test]
async fn persisted_cross_node_principal_contains_only_identity_reference_columns() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let principal = alice_principal(&bare);
    let (registry, _identity) = node_registry(&db, node_identity(), None).await;
    registry
        .store_session_with_principal(
            detached_session("stream-principal-no-credential", &full),
            principal.clone(),
        )
        .await
        .expect("store principal-bearing snapshot");

    let (stored_bare, stored_context, stored_version, stored_epoch) =
        persisted_principal_columns(&db, "stream-principal-no-credential").await;
    assert_eq!(stored_bare.as_deref(), Some(bare.as_str()));
    assert_eq!(
        stored_context.as_deref(),
        Some(principal.auth_context_id().as_uuid().to_string().as_str())
    );
    assert_eq!(stored_version, Some(1));
    assert_eq!(stored_epoch, Some(1));

    let conn = db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_name = 'sm_sessions' \
                AND (column_name ILIKE '%token%' \
                  OR column_name ILIKE '%bearer%' \
                  OR column_name ILIKE '%password%')",
            (),
        )
        .await
        .expect("inspect sm_sessions credential-shaped columns");
    assert!(
        rows.next()
            .await
            .expect("read credential-shaped column")
            .is_none(),
        "sm_sessions must not contain token, bearer, or password columns"
    );
}

#[tokio::test]
async fn detached_owned_elsewhere_steals_and_hydrates_with_h_counter_integrity() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let (registry_a, _identity_a) = node_registry(&db, node_identity(), None).await;
    let (registry_b, _identity_b) = node_registry(&db, node_identity(), None).await;

    let session = detached_session("stream-detached", &full);
    let original_outbound = session.outbound_count;
    let original_inbound = session.inbound_count;
    let original_unacked = session.unacked_stanzas.len();
    registry_a
        .store_session(session)
        .await
        .expect("node A stores the detached session");

    // Node B has no local record at all: claim_session must return None.
    assert!(registry_b
        .claim_session("stream-detached")
        .await
        .expect("claim_session call succeeds")
        .is_none());

    let outcome = registry_b
        .attempt_cross_node_resume("stream-detached", &bare, HANDSHAKE_BUDGET)
        .await
        .expect("attempt_cross_node_resume succeeds");
    let CrossNodeResumeOutcome::Claimed(resumed) = outcome else {
        panic!("expected Claimed");
    };
    assert_eq!(
        resumed.outbound_count, original_outbound,
        "h-counter integrity: outbound_count survives the steal"
    );
    assert_eq!(
        resumed.inbound_count, original_inbound,
        "h-counter integrity: inbound_count survives the steal"
    );
    assert_eq!(
        resumed.unacked_stanzas.len(),
        original_unacked,
        "unacked queue survives the steal intact"
    );
    assert_eq!(resumed.jid, full);
}

#[tokio::test]
async fn live_handshake_via_asker_falls_through_to_detached_path() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let (registry_a, _identity_a) = node_registry(&db, node_identity(), None).await;

    // Node A "enables" (claims) but never detaches — simulating a still-live
    // session: a claim row exists, but nothing is persisted yet.
    let entity = Entity::new(EntityType::SmSession, "stream-live".to_string());
    let claim_store_probe: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    let identity_a = _identity_a.current();
    claim_store_probe
        .ensure_claimed(&entity, &identity_a)
        .await
        .expect("node A claims the live session");

    let session = detached_session("stream-live", &full);
    let asker = Arc::new(FakeAsker::new(
        Arc::clone(&registry_a),
        bare.clone(),
        session.clone(),
    ));
    let (registry_b, _identity_b) = node_registry(&db, node_identity(), Some(asker.clone())).await;

    // No persisted row exists yet — branch 2 (live) must fire.
    let outcome = registry_b
        .attempt_cross_node_resume("stream-live", &bare, HANDSHAKE_BUDGET)
        .await
        .expect("attempt_cross_node_resume succeeds");
    let CrossNodeResumeOutcome::Claimed(resumed) = outcome else {
        panic!("expected Claimed after the live handshake falls through to the detached path");
    };
    assert_eq!(resumed.jid, full);
    assert_eq!(
        asker.asks_seen.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "exactly one ask was needed"
    );
}

#[tokio::test]
async fn forged_previd_wrong_identity_returns_not_authorized_without_stealing() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (_owner_bare, owner_full) = alice_jid();
    let attacker_bare: BareJid = "mallory@example.com".parse().expect("valid jid");
    let (registry_a, identity_a) = node_registry(&db, node_identity(), None).await;
    let (registry_b, _identity_b) = node_registry(&db, node_identity(), None).await;

    let session = detached_session("stream-forged", &owner_full);
    registry_a
        .store_session(session)
        .await
        .expect("node A stores the detached session");

    let entity = Entity::new(EntityType::SmSession, "stream-forged".to_string());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    let before = claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .expect("claim exists");

    let outcome = registry_b
        .attempt_cross_node_resume("stream-forged", &attacker_bare, HANDSHAKE_BUDGET)
        .await
        .expect("attempt_cross_node_resume succeeds");
    assert!(matches!(outcome, CrossNodeResumeOutcome::NotAuthorized));

    // Claim untouched: same owner, same epoch.
    let after = claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .expect("claim still exists");
    assert_eq!(before.owner, after.owner);
    assert_eq!(before.claim_epoch, after.claim_epoch);
    assert_eq!(after.owner, identity_a.current());
}

#[tokio::test]
async fn pure_two_node_detached_steal_race_exactly_one_winner() {
    // Deliberately drives `ClaimStore::steal_for_resume` directly (rather
    // than through two concurrent `attempt_cross_node_resume` calls): the
    // full orchestration has several sequential `.await` points before its
    // own CAS (the `current_claim` read, the persistence read), so two
    // independently-scheduled top-level calls are not guaranteed to
    // observe the SAME pre-race epoch — `tokio::join!` alone cannot force
    // that overlap deterministically. Capturing `observed_epoch` once and
    // racing the bare CAS with it is what actually proves "both nodes race
    // `steal_for_resume` against the same detached claim" (major fix 11)
    // without depending on scheduling luck. The full hydrate/claim_session
    // pipeline is separately verified for the winner below, using the SAME
    // public API `attempt_cross_node_resume` itself calls.
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let (registry_a, _identity_a) = node_registry(&db, node_identity(), None).await;
    let (registry_b, identity_b) = node_registry(&db, node_identity(), None).await;
    let (registry_c, identity_c) = node_registry(&db, node_identity(), None).await;

    registry_a
        .store_session(detached_session("stream-race", &full))
        .await
        .expect("node A stores the detached session");

    let entity = Entity::new(EntityType::SmSession, "stream-race".to_string());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    let observed = claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .expect("claim exists")
        .claim_epoch;
    let proof_b = waddle_xmpp::ownership::verify_resume_identity(&bare, &full.to_bare())
        .expect("identity match for B");
    let proof_c = waddle_xmpp::ownership::verify_resume_identity(&bare, &full.to_bare())
        .expect("identity match for C");
    let identity_b_now = identity_b.current();
    let identity_c_now = identity_c.current();

    let (result_b, result_c) = tokio::join!(
        claim_store.steal_for_resume(&entity, observed, proof_b, &identity_b_now),
        claim_store.steal_for_resume(&entity, observed, proof_c, &identity_c_now),
    );
    let ok_count = [&result_b, &result_c].iter().filter(|r| r.is_ok()).count();
    let conflict_count = [&result_b, &result_c]
        .iter()
        .filter(|r| matches!(r, Err(waddle_xmpp::ownership::ClaimError::Conflict)))
        .count();
    assert_eq!(ok_count, 1, "exactly one racer wins the CAS");
    assert_eq!(
        conflict_count, 1,
        "the loser observes 0 rows (Conflict) — never a second, later win against the same \
         stale observed epoch"
    );

    // The winner's full pipeline (hydrate + claim_session) still resumes
    // correctly, exactly like `attempt_cross_node_resume`'s own branch 1.
    let (winning_registry, winning_identity, winning_epoch) = match (result_b, result_c) {
        (Ok(epoch), Err(_)) => (&registry_b, identity_b_now, epoch),
        (Err(_), Ok(epoch)) => (&registry_c, identity_c_now, epoch),
        other => panic!("expected exactly one winner and one Conflict, got {other:?}"),
    };
    let winning_fence = waddle_xmpp::stream_management::persistence::SmClaimFence::new(
        winning_identity,
        winning_epoch,
    );
    let reservation = winning_registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("winner reserves reclaimed ownership capacity");
    winning_registry
        .hydrate_reclaimed(&[(entity, winning_fence, reservation)])
        .await
        .expect("hydrate_reclaimed succeeds for the winner");
    let resumed = winning_registry
        .claim_session("stream-race")
        .await
        .expect("claim_session succeeds")
        .expect("winner's registry resumes the session");
    assert_eq!(resumed.jid, full);
}

#[tokio::test]
async fn two_simultaneous_live_resume_race_loser_falls_back_to_detached_path_and_fails_cleanly() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let (registry_a, identity_a) = node_registry(&db, node_identity(), None).await;

    let entity = Entity::new(EntityType::SmSession, "stream-live-race".to_string());
    let claim_store_probe: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    claim_store_probe
        .ensure_claimed(&entity, &identity_a.current())
        .await
        .expect("node A claims the live session");

    let session = detached_session("stream-live-race", &full);
    // ONE shared asker/session pool standing in for node A being asked
    // concurrently by two different resuming nodes — only the first ask to
    // arrive actually takes the live session and detaches it; the second
    // observes `NotLiveRemotely` (mirrors the real force-detach channel:
    // only one connection ever exists to force-detach).
    let asker = Arc::new(FakeAsker::new(
        Arc::clone(&registry_a),
        bare.clone(),
        session,
    ));
    let (registry_b, _identity_b) = node_registry(&db, node_identity(), Some(asker.clone())).await;
    let (registry_c, _identity_c) = node_registry(&db, node_identity(), Some(asker.clone())).await;

    let (result_b, result_c) = tokio::join!(
        registry_b.attempt_cross_node_resume("stream-live-race", &bare, HANDSHAKE_BUDGET),
        registry_c.attempt_cross_node_resume("stream-live-race", &bare, HANDSHAKE_BUDGET),
    );
    let outcomes = [
        result_b.expect("attempt_cross_node_resume succeeds"),
        result_c.expect("attempt_cross_node_resume succeeds"),
    ];
    let claimed_count = outcomes
        .iter()
        .filter(|o| matches!(o, CrossNodeResumeOutcome::Claimed(_)))
        .count();
    let not_found_count = outcomes
        .iter()
        .filter(|o| matches!(o, CrossNodeResumeOutcome::NotFound))
        .count();
    assert_eq!(
        claimed_count, 1,
        "exactly one of the two live-resume attempts wins"
    );
    assert_eq!(
        not_found_count, 1,
        "the second requester loses the consent epoch CAS and fails cleanly — it never re-steals \
         a second time even though it falls back to (re-)checking the now-detached path"
    );
}

#[tokio::test]
async fn reaper_wins_mid_resume_interleaving_resume_fails_cleanly() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let (registry_a, _identity_a) = node_registry(&db, node_identity(), None).await;
    let (registry_b, registry_b_identity) = node_registry(&db, node_identity(), None).await;

    registry_a
        .store_session(detached_session("stream-reaped", &full))
        .await
        .expect("node A stores the detached session");

    // `handle_sm_resume`/`attempt_cross_node_resume` captures its observed
    // epoch ONCE, up front (the module's one-shot-CAS discipline — see
    // `cross_node_resume.rs`'s doc comment) — model that here directly:
    // capture the epoch a resume attempt would have observed, THEN let the
    // orphan reaper win `steal_stale(OwnerStale)` for the same entity
    // (simulating the reaper committing mid-resume, after the resume's own
    // read but before its own CAS), THEN attempt `steal_for_resume` with
    // that now-stale observed epoch directly — proving the exact SQL-level
    // behavior the ordering invariant requires: the CAS observes the
    // reaper's bumped epoch and loses (0 rows), regardless of how much
    // later the resuming node's own CAS actually executes. (Driving this
    // through two concurrent `attempt_cross_node_resume` calls instead
    // would not deterministically reproduce this interleaving — the full
    // orchestration's own `current_claim` read would simply observe
    // whichever state already exists when it happens to run.)
    let entity = Entity::new(EntityType::SmSession, "stream-reaped".to_string());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    let node_lease: Arc<dyn NodeLeaseStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    let snapshot = claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .expect("claim exists");
    let observed_epoch = snapshot.claim_epoch;
    let reaper_identity = node_identity();
    node_lease
        .register(&reaper_identity, None)
        .await
        .expect("register the reaper's fresh liveness row");
    claim_store
        .steal_stale(
            &entity,
            observed_epoch,
            StalePredicate::OwnerStale,
            &reaper_identity,
        )
        .await
        .expect("reaper wins steal_stale");

    let identity_b = registry_b_identity.current();
    let proof = waddle_xmpp::ownership::verify_resume_identity(&bare, &full.to_bare())
        .expect("identity match");
    let err = claim_store
        .steal_for_resume(&entity, observed_epoch, proof, &identity_b)
        .await
        .expect_err("resume's steal_for_resume observes the reaper's bumped epoch and loses");
    assert!(
        matches!(err, waddle_xmpp::ownership::ClaimError::Conflict),
        "the resuming node's CAS fails cleanly (0 rows) rather than resuming a snapshot the \
         reaper is concurrently touching"
    );
    // `attempt_cross_node_resume`, called fresh right now, maps this same
    // situation (foreign, non-self claim) through its own top-level read —
    // demonstrated separately by `pure_two_node_detached_steal_race_exactly_one_winner`
    // and `forged_previd_wrong_identity_returns_not_authorized_without_stealing`
    // above; this test's own point is the CAS-level interleaving itself.
    let _ = registry_b;
}

#[tokio::test]
async fn owner_unreachable_past_the_handshake_window_fails_with_owner_unreachable() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, _full) = alice_jid();
    let (_registry_a, identity_a) = node_registry(&db, node_identity(), None).await;

    let entity = Entity::new(EntityType::SmSession, "stream-unreachable".to_string());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    claim_store
        .ensure_claimed(&entity, &identity_a.current())
        .await
        .expect("node A claims the live session");
    // FIX 6 (council-adjudicated): the window-expiry re-check now
    // distinguishes "owner still fresh" from "owner lease expired" via
    // `ClaimSnapshot::owner_lease_fresh`, which reads node A's own
    // `clustering_nodes` liveness row — register (and thereby commit
    // fresh/non-expired) that row so this test still exercises the
    // "genuinely unreachable, not gone" case it names, rather than
    // accidentally falling into the (separately tested, below)
    // lease-expired case just because no liveness row exists at all.
    let node_lease: Arc<dyn NodeLeaseStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    node_lease
        .register(&identity_a.current(), None)
        .await
        .expect("register node A's liveness row as fresh");

    // No persisted row (never detached), and the asker always reports
    // Unreachable — branch 3's held-response window must expire cleanly.
    let asker: Arc<dyn RemoteResumeAsker> = Arc::new(UnreachableAsker);
    let (registry_b, _identity_b) = node_registry(&db, node_identity(), Some(asker)).await;

    let started = std::time::Instant::now();
    let outcome = registry_b
        .attempt_cross_node_resume("stream-unreachable", &bare, Duration::from_millis(500))
        .await
        .expect("attempt_cross_node_resume succeeds");
    assert!(matches!(outcome, CrossNodeResumeOutcome::OwnerUnreachable));
    assert!(
        started.elapsed() >= Duration::from_millis(450),
        "the held-response window is actually honored, not short-circuited"
    );
}

/// FIX 6 (council-adjudicated): the two terminal conditions on held
/// -response window expiry are actually distinguished, not collapsed. This
/// is the "owner lease expired" half — the claim row still names node A as
/// owner, but node A's own `clustering_nodes` liveness row is
/// committed-`expired` (a dead owner whose claim GC/reaper hasn't caught up
/// yet). The window must expire with `NotFound` (`<failed/>`
/// `item-not-found` — "the session is known gone"), never
/// `OwnerUnreachable` (`resource-constraint`). The companion "owner still
/// fresh" case is `owner_unreachable_past_the_handshake_window_fails_with_owner_unreachable`
/// above.
#[tokio::test]
async fn owner_lease_expired_past_the_handshake_window_fails_with_not_found() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, _full) = alice_jid();
    let (_registry_a, identity_a) = node_registry(&db, node_identity(), None).await;

    let entity = Entity::new(EntityType::SmSession, "stream-lease-expired".to_string());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    claim_store
        .ensure_claimed(&entity, &identity_a.current())
        .await
        .expect("node A claims the live session");

    let node_lease: Arc<dyn NodeLeaseStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    node_lease
        .register(&identity_a.current(), None)
        .await
        .expect("register node A's liveness row");
    // Commit node A's own liveness row expired directly — the owner-stale
    // predicate's own "committed `expired` flag, never a raw heartbeat
    // comparison" rule (see `steal_stale`'s doc comment), realized here the
    // same way `claims.rs`'s own `steal_stale_ignores_raw_heartbeat_only_committed_expired_flag_matters`
    // test does.
    {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_nodes SET expired = true WHERE node_id = ?",
            waddle_server::db_params![identity_a.current().node_id.clone()],
        )
        .await
        .expect("commit node A's liveness row expired");
    }

    // No persisted row (never detached), and the asker always reports
    // Unreachable — the window must still expire, but now onto the
    // different terminal condition.
    let asker: Arc<dyn RemoteResumeAsker> = Arc::new(UnreachableAsker);
    let (registry_b, _identity_b) = node_registry(&db, node_identity(), Some(asker)).await;

    let outcome = registry_b
        .attempt_cross_node_resume("stream-lease-expired", &bare, Duration::from_millis(300))
        .await
        .expect("attempt_cross_node_resume succeeds");
    assert!(
        matches!(outcome, CrossNodeResumeOutcome::NotFound),
        "owner's lease has expired: must report NotFound (item-not-found), not \
         OwnerUnreachable (resource-constraint)"
    );
}

/// FIX 8(a) (council-adjudicated): the cross-node resume-RETRANSMIT race —
/// a client retransmits `<resume/>` (or a second connection attempts resume
/// for the same `previd`) against the SAME new node while the first
/// handshake is still in flight. Distinct from
/// `two_simultaneous_live_resume_race_loser_falls_back_to_detached_path_and_fails_cleanly`
/// above (which races two DIFFERENT resuming nodes/registries): this drives
/// two concurrent `attempt_cross_node_resume` calls on the exact SAME
/// registry, proving the in-process dedup holds locally too — exactly one
/// wins, the other fails cleanly, and no double-hydration leaves a stray
/// claimable copy behind.
#[tokio::test]
async fn resume_retransmit_race_against_the_same_new_node_dedups_without_double_hydration() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let (registry_a, _identity_a) = node_registry(&db, node_identity(), None).await;
    let (registry_b, identity_b) = node_registry(&db, node_identity(), None).await;

    registry_a
        .store_session(detached_session("stream-retransmit", &full))
        .await
        .expect("node A stores the detached session");

    let entity = Entity::new(EntityType::SmSession, "stream-retransmit".to_string());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    let before = claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim before resume race")
        .expect("detached session has an ownership claim");

    // Two concurrent attempts on the SAME registry_b — modeling a client
    // that retransmits `<resume/>` (or opens a second connection) against
    // the new node before the first attempt's response ever reaches it.
    let (result_1, result_2) = tokio::join!(
        registry_b.attempt_cross_node_resume("stream-retransmit", &bare, HANDSHAKE_BUDGET),
        registry_b.attempt_cross_node_resume("stream-retransmit", &bare, HANDSHAKE_BUDGET),
    );
    let outcomes = [
        result_1.expect("attempt_cross_node_resume succeeds"),
        result_2.expect("attempt_cross_node_resume succeeds"),
    ];
    let claimed_count = outcomes
        .iter()
        .filter(|o| matches!(o, CrossNodeResumeOutcome::Claimed(_)))
        .count();
    let not_found_count = outcomes
        .iter()
        .filter(|o| matches!(o, CrossNodeResumeOutcome::NotFound))
        .count();
    assert_eq!(
        claimed_count, 1,
        "exactly one of the two retransmitted resume attempts wins"
    );
    assert_eq!(
        not_found_count, 1,
        "the other retransmit fails cleanly (stale-epoch CAS loss), never a second steal"
    );

    // No double-hydration: the winner's `claim_session` already consumed
    // the hydrated copy inside `attempt_cross_node_resume` itself, so a
    // fresh `claim_session` call on this same registry must find nothing
    // left over to claim a second time.
    assert!(
        registry_b
            .claim_session("stream-retransmit")
            .await
            .expect("claim_session call succeeds")
            .is_none(),
        "no stray claimable copy should survive the dedup'd retransmit race"
    );

    // The single winner replaced node A's claim with a fresh monotonic
    // generation owned by node B. Generations are globally allocated, so
    // neither their initial value nor adjacency is deterministic.
    let after = claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .expect("claim still exists (held by the winner via claimed_sessions)");
    assert!(
        after.claim_epoch > before.claim_epoch,
        "the winning steal_for_resume must allocate a newer claim generation"
    );
    assert_eq!(
        after.owner,
        identity_b.current(),
        "the sole successful resume CAS must leave node B as the exact owner"
    );
}

/// FIX 8(b) (council-adjudicated): recipient-claim-move retry. A cross-node
/// resume completes while `pending_delivery` rows tagged for the session
/// exist — standing in for "pending_delivery rows move with the claim" is
/// trivial here (both simulated nodes already share one Postgres
/// `pending_delivery` table, exactly like every other cross-node
/// simulation in this suite), so the Phase-3-reachable half this test
/// proves is the part that actually matters post-move: the SM-ack-keyed
/// delete path (`delete_acked_in_window`) is idempotent under retry — a
/// retried ack (e.g. the client's `<a h='N'/>` re-arriving, or the
/// recovering session's own at-least-once redelivery of the same ack)
/// dedups to a clean no-op rather than erroring or double-processing.
/// Actual cross-node stanza ROUTING to reconstruct the flush in the first
/// place is Phase 4 (this plan's own non-goal) — recorded as deviation 44
/// in the phase plan.
#[tokio::test]
async fn recipient_claim_move_retry_dedups_pending_delivery_delete() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let (registry_a, _identity_a) = node_registry(&db, node_identity(), None).await;
    let (registry_b, _identity_b) = node_registry(&db, node_identity(), None).await;

    let stream_id = "stream-claim-move";
    let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.to_string());

    // A pending_delivery row, claimed for this session before the claim
    // moves — standing in for a stanza queued for offline delivery that
    // was flushed to (and pushed by) the pre-resume connection.
    let pending_storage = DatabasePendingDeliveryStorage::open(
        Some(&std::env::var("WADDLE_TEST_POSTGRES_URL").expect("checked by clean_db above")),
        QuotaPolicy::Unlimited,
    )
    .await
    .expect("open pending_delivery storage against the same test database");
    {
        let conn = db.guard().await.expect("guard");
        conn.execute("DELETE FROM pending_delivery", ())
            .await
            .expect("clean pending_delivery");
    }
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(bare.clone())));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    pending_storage
        .insert(PendingRow {
            id: PendingRowId::fresh(),
            recipient: bare.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Transient(Box::new(message)),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert pending_delivery row");
    let claimed_rows = pending_storage
        .claim_for_session(&bare, &session_id)
        .await
        .expect("claim_for_session");
    assert_eq!(
        claimed_rows.len(),
        1,
        "exactly one row claimed for this session"
    );
    pending_storage
        .record_pushed_at(&claimed_rows[0].id, 1)
        .await
        .expect("record_pushed_at");

    // Detach + cross-node steal the SM session itself (branch 1: already
    // persisted).
    let mut session = detached_session(stream_id, &full);
    session.outbound_count = 1;
    registry_a
        .store_session(session)
        .await
        .expect("node A stores the detached session");
    let outcome = registry_b
        .attempt_cross_node_resume(stream_id, &bare, HANDSHAKE_BUDGET)
        .await
        .expect("attempt_cross_node_resume succeeds");
    assert!(
        matches!(outcome, CrossNodeResumeOutcome::Claimed(_)),
        "the SM session claim must have moved to node B"
    );

    // The resumed session's ack (h=1) deletes the row — the retry path
    // dedups: a second, retried delete_acked_in_window for the SAME h is a
    // clean no-op, never an error and never a double-delete side effect.
    let removed_first = pending_storage
        .delete_acked_in_window(&session_id, 0, 1)
        .await
        .expect("delete_acked_in_window");
    assert_eq!(
        removed_first, 1,
        "the claimed, pushed row is deleted on first ack"
    );
    let removed_retry = pending_storage
        .delete_acked_in_window(&session_id, 0, 1)
        .await
        .expect("delete_acked_in_window retry");
    assert_eq!(
        removed_retry, 0,
        "a retried ack for the same h must dedup to zero rows removed, not error or re-delete"
    );
}

/// FIX 8(c) (council-adjudicated): deferred-h/handoff coupling across the
/// steal. The phase plan's Slice 6 tests paragraph names this scenario as
/// "`<r/>` answered immediately with `h` excluding unresolved handoffs" —
/// investigation (this test's own research pass) found this is the SAME
/// single-node `claimed_sessions`-writable-during-handoff machinery
/// `session_registry/tests.rs::test_claimed_session_remains_writable_for_handoff_fanout`
/// already covers (`record_stanza_for_detached_resource` merging fanout
/// into a session's unacked queue while it sits in `claimed_sessions`,
/// between `claim_session` and `complete_claim`), inherited UNCHANGED by
/// the cross-node path: `attempt_cross_node_resume`'s branch 1 calls the
/// exact same `hydrate_reclaimed` → `claim_session` pair the local-only
/// resume path uses, so the resulting `claimed_sessions` entry is
/// indistinguishable from a purely local claim — no new cross-node-specific
/// h-counter or handoff-coupling code exists to exercise. Recorded as
/// deviation 45 in the phase plan rather than silently skipped; this test
/// still empirically proves the existing mechanism carries over unchanged
/// through an actual cross-node steal (not merely asserted by code
/// inspection).
#[tokio::test]
async fn handoff_fanout_survives_a_cross_node_steal_unchanged() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let (registry_a, _identity_a) = node_registry(&db, node_identity(), None).await;
    let (registry_b, _identity_b) = node_registry(&db, node_identity(), None).await;

    let mut session = detached_session("stream-handoff-cross-node", &full);
    session.roster_interested = true;
    registry_a
        .store_session(session)
        .await
        .expect("node A stores the detached session");

    let outcome = registry_b
        .attempt_cross_node_resume("stream-handoff-cross-node", &bare, HANDSHAKE_BUDGET)
        .await
        .expect("attempt_cross_node_resume succeeds");
    assert!(
        matches!(outcome, CrossNodeResumeOutcome::Claimed(_)),
        "the cross-node steal must have landed the session in claimed_sessions on node B"
    );

    // A stanza fanned out to this resource during the handoff window
    // (between the cross-node steal landing and the connection finishing
    // registration) must still be writable — exactly the local-only
    // handoff-fanout contract, now exercised post-steal.
    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.statuses.insert(
        xmpp_parsers::message::Lang(String::new()),
        "during-cross-node-handoff".to_string(),
    );
    let wrote = registry_b
        .record_stanza_for_detached_resource(
            &full,
            &waddle_xmpp::Stanza::Presence(presence),
            chrono::Utc::now(),
        )
        .await
        .expect("record_stanza_for_detached_resource succeeds");
    assert!(
        wrote,
        "fanout during the post-cross-node-steal handoff must write to the claimed session"
    );

    let completed = registry_b
        .complete_claim("stream-handoff-cross-node")
        .await
        .expect("complete_claim succeeds")
        .expect("completed claim");
    match completed {
        SmClaimCompletion::Resumed(completed) => {
            assert!(
                completed
                    .unacked_stanzas
                    .iter()
                    .any(|entry| entry.stanza_xml.contains("during-cross-node-handoff")),
                "completed claim must include fanout recorded during the post-steal handoff"
            );
        }
        other => panic!("expected a resumable completion, got a non-resumable variant: {other:?}"),
    }
}

/// `ClaimStore` test double (FIX B/C regression guard): delegates every
/// method to a real `PostgresClaimStore`, except `ensure_claimed`, whose
/// FIRST call fails with an injected `ClaimError::Backend` and every call
/// after that delegates normally. This reproduces an ORDINARY (no
/// cancellation, no timeout) post-CAS-win `hydrate_reclaimed` failure:
/// `hydrate_reclaimed`'s own internal self-reacquire `ensure_claimed` call
/// is the first (and, in this test, only) call this double ever sees.
struct EnsureClaimedFailsOnceClaimStore {
    inner: Arc<dyn ClaimStore>,
    fail_next: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl ClaimStore for EnsureClaimedFailsOnceClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        self.inner.ensure_schema().await
    }

    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        self.inner.acquire(entity, me).await
    }

    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        if self
            .fail_next
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ClaimError::Backend(
                "injected ensure_claimed failure (FIX B/C test double)".to_string(),
            ));
        }
        self.inner.ensure_claimed(entity, me).await
    }

    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.inner
            .steal_stale(entity, observed, staleness, me)
            .await
    }

    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.inner
            .steal_for_resume(entity, observed, witness, me)
            .await
    }

    async fn current_claim(&self, entity: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.inner.current_claim(entity).await
    }

    async fn fence(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        self.inner.fence(entity, me, mine).await
    }

    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        self.inner.release(entity, me, mine).await
    }

    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError> {
        self.inner.release_many(entities, me).await
    }
}

/// FIX B/C, Postgres-gated: an ORDINARY `hydrate_reclaimed` failure right
/// after a real `steal_for_resume` CAS win — no cancellation, no timeout,
/// just an injected backend error on the post-win self-reacquire — must
/// repair (release the just-won claim in real Postgres) rather than strand
/// it, and the client's very next resume attempt against the SAME stream
/// id must actually recover the session via FIX C's unclaimed-but
/// -persisted branch. This is the "sibling" hazard the ADR-0017 Phase 3
/// convergence check traced alongside the FIX 3 shutdown-race defect
/// (deviation 55): the claim would otherwise be held by a live node under
/// a fresh lease that the orphan reaper's `OwnerStale` predicate can never
/// fire against — a permanent wedge, not merely a slow one.
#[tokio::test]
async fn post_win_hydrate_failure_repairs_and_client_retry_recovers_the_session() {
    let _guard = serial_lock().lock().await;
    let Some(db) = clean_db().await else {
        return;
    };
    let (bare, full) = alice_jid();
    let (registry_a, _identity_a) = node_registry(&db, node_identity(), None).await;

    let real_claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
    let wrapped_claim_store: Arc<dyn ClaimStore> = Arc::new(EnsureClaimedFailsOnceClaimStore {
        inner: Arc::clone(&real_claim_store),
        fail_next: std::sync::atomic::AtomicBool::new(true),
    });
    let (registry_b, _identity_b) = node_registry_with_claim_store(
        &db,
        node_identity(),
        Arc::clone(&wrapped_claim_store),
        None,
    )
    .await;

    let session = detached_session("stream-hydrate-fail-pg", &full);
    registry_a
        .store_session(session)
        .await
        .expect("node A stores the detached session");

    // First attempt: node B's `steal_for_resume` CAS genuinely wins in
    // Postgres (untouched by the wrapper), but the very next step —
    // `hydrate_reclaimed`'s own internal self-reacquire `ensure_claimed`
    // — hits the injected one-shot failure. FIX B must repair rather than
    // surface this as a bare error.
    let outcome = registry_b
        .attempt_cross_node_resume("stream-hydrate-fail-pg", &bare, HANDSHAKE_BUDGET)
        .await
        .expect("attempt_cross_node_resume must not error: FIX B repairs, it does not fail");
    // The injected ensure_claimed failure is a storage-class (Error-source)
    // repair: the durable row still exists, so it must surface as retryable
    // StorageUnavailable, never as item-not-found (adversarial round 1,
    // HIGH: storage loss must not masquerade as absence).
    assert!(
        matches!(outcome, CrossNodeResumeOutcome::StorageUnavailable),
        "post-win hydrate failure must repair to retryable StorageUnavailable; got {outcome:?}"
    );

    // Prove the repair actually released the claim in real Postgres (not
    // just in this process's in-memory bookkeeping).
    let entity = Entity::new(EntityType::SmSession, "stream-hydrate-fail-pg".to_string());
    assert!(
        real_claim_store
            .current_claim(&entity)
            .await
            .expect("current_claim must not error")
            .is_none(),
        "FIX B's repair must release the just-won claim in Postgres"
    );

    // FIX C: the client's retry (the injected failure was one-shot, so
    // every `ClaimStore` call from here on behaves exactly like the real
    // store) must actually recover the persisted session — not just avoid
    // an error.
    let retry_outcome = registry_b
        .attempt_cross_node_resume("stream-hydrate-fail-pg", &bare, HANDSHAKE_BUDGET)
        .await
        .expect("the FIX C retry must not error");
    let CrossNodeResumeOutcome::Claimed(resumed) = retry_outcome else {
        panic!("expected Claimed on the FIX C retry, got {retry_outcome:?}");
    };
    assert_eq!(
        resumed.jid, full,
        "the recovered session must be the same one node A originally detached"
    );
}
