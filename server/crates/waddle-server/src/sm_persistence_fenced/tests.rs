//! Postgres-gated tests for [`PostgresFencedSmPersistence`] (ADR-0017 Phase
//! 3 Slice 4). Skipped (not failed) when `WADDLE_TEST_POSTGRES_URL` is
//! unset, mirroring `clustering::claims`'s own test style exactly.

use super::*;

#[test]
fn stale_sm_cache_failure_does_not_evict_new_generation() {
    let stream_id = SmSessionId::new("old-a-new-b".to_string());
    let owner = NodeIdentity::new("node-a", "incarnation-a");
    let fence_a = SmClaimFence::new(owner.clone(), ClaimEpoch(1));
    let fence_b = SmClaimFence::new(owner, ClaimEpoch(2));
    let cell_a = Arc::new(OnceCell::new());
    cell_a.set(fence_a.clone()).expect("initialize A");
    let cell_b = Arc::new(OnceCell::new());
    cell_b.set(fence_b.clone()).expect("initialize B");
    let cache = DashMap::new();
    cache.insert(stream_id.clone(), cell_b.clone());

    remove_sm_claim_cell_if(&cache, &stream_id, &cell_a);
    remove_sm_claim_fence_if(&cache, &stream_id, &fence_a);

    assert!(Arc::ptr_eq(cache.get(&stream_id).unwrap().value(), &cell_b));
    assert_eq!(cache.get(&stream_id).unwrap().get(), Some(&fence_b));
}
use crate::clustering::claims::{NodeLeaseStore as _, PostgresClaimStore};
use crate::db::DatabaseConfig;
use chrono::TimeZone;
use std::time::Duration as StdDuration;
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity, StalePredicate};
use waddle_xmpp::stream_management::SmSessionRegistry as _;
use xmpp_parsers::presence::Show;

fn full(s: &str) -> jid::FullJid {
    s.parse().expect("valid full JID fixture")
}

/// Deliberately in the past relative to any real test run — divergence
/// (a)'s whole point is that this value is ignored and ends up
/// overwritten with the *actual* current Postgres time instead.
fn stale_caller_supplied_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap()
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
        detached_at: stale_caller_supplied_time(),
        max_resume_duration: StdDuration::from_secs(60),
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
        stanza: Box::new(waddle_xmpp::Stanza::Message(message)),
        original_receipt_at: stale_caller_supplied_time(),
        purpose: SmUnackedStanzaPurpose::Application,
    }
}

fn node_identity() -> NodeIdentity {
    NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    )
}

/// Mirrors `clustering::claims::tests::seed_node` exactly (duplicated
/// rather than exposed cross-module for a test-only helper — see that
/// function's own doc comment for why `expired` is spliced as a literal).
async fn seed_node(db: &Database, identity: &NodeIdentity, expired: bool) {
    let conn = db.guard().await.expect("guard");
    let expired_literal = if expired { "true" } else { "false" };
    conn.execute(
        &format!(
            "INSERT INTO clustering_nodes (node_id, node_epoch, expired) VALUES (?, ?, {expired_literal})"
        ),
        crate::db_params![identity.node_id.clone(), identity.node_epoch.clone()],
    )
    .await
    .expect("seed node");
}

async fn live_stealer(db: &Database) -> NodeIdentity {
    let stealer = node_identity();
    seed_node(db, &stealer, false).await;
    stealer
}

async fn current_claim_epoch(fixture: &Fixture, entity: &Entity) -> ClaimEpoch {
    fixture
        .claims
        .current_claim(entity)
        .await
        .expect("claim lookup")
        .expect("fenced write established a claim")
        .claim_epoch
}

struct Fixture {
    fenced: PostgresFencedSmPersistence,
    /// A second `ClaimStore` handle onto the same underlying Postgres
    /// database as `fenced`'s own — used to simulate a second node
    /// stealing an entity out from under `fenced`.
    claims: PostgresClaimStore,
    claims_db: Database,
    identity: NodeIdentity,
    shared_identity: SharedNodeIdentity,
    /// Holds `clustering::claims::clustering_control_plane_table_lock()` for
    /// this fixture's — and thus the whole test's — lifetime. Every test in
    /// this module truncates the shared `sm_sessions`/`sm_unacked`/
    /// `clustering_*` tables at setup and then reads/writes them for its
    /// full body, and `cargo test` runs tests within one binary
    /// concurrently by default: without holding this lock across the whole
    /// test (not just the setup wipe), one test's truncate could destroy
    /// another, still-in-flight test's rows. Mirrors `clustering::claims`'s
    /// and `self_fence`'s own Postgres-gated tests, which serialize on this
    /// exact lock for the same reason — this module shares those two
    /// `clustering_*` tables too.
    _table_lock: tokio::sync::MutexGuard<'static, ()>,
}

/// `None` (skip, not fail) when `WADDLE_TEST_POSTGRES_URL` is unset,
/// matching `clustering::claims`'s own Postgres-gated test convention.
async fn fixture() -> Option<Fixture> {
    let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
    // Acquire before touching any shared table — see `_table_lock`'s own
    // doc comment on `Fixture`.
    let table_lock = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    let claims_db = Database::from_config(
        "sm-persistence-fenced-test-claims",
        &DatabaseConfig::new(DatabaseDriver::Postgres, url.clone())
            .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
    )
    .await
    .expect("open claims-side test database");
    let claims = PostgresClaimStore::new(claims_db.clone());
    claims.ensure_schema().await.expect("ensure claims schema");

    let identity = node_identity();
    let shared_identity = SharedNodeIdentity::new(identity.clone());
    let claim_store_for_fenced: Arc<dyn ClaimStore> =
        Arc::new(PostgresClaimStore::new(claims_db.clone()));
    // FIX 4: `open` now takes the already-opened `Database` handle
    // directly (co-located with the claims tables), never an independently
    // resolved URL — `claims_db.clone()` is the same handle `claims`
    // itself uses.
    let fenced = PostgresFencedSmPersistence::open(
        claims_db.clone(),
        claim_store_for_fenced,
        shared_identity.clone(),
    )
    .await
    .expect("open fenced SM persistence");

    // Clean every table this test module touches. Runs after `open` so
    // `sm_sessions`/`sm_unacked` are guaranteed to already exist.
    let conn = claims_db.guard().await.expect("guard");
    for stmt in [
        "DELETE FROM clustering_claims",
        "DELETE FROM clustering_nodes",
        "DELETE FROM clustering_steal_intents",
        "DELETE FROM sm_unacked",
        "DELETE FROM sm_sessions",
    ] {
        conn.execute(stmt, ()).await.expect("clean table");
    }

    Some(Fixture {
        fenced,
        claims,
        claims_db,
        identity,
        shared_identity,
        _table_lock: table_lock,
    })
}

#[tokio::test]
async fn upsert_and_get_session_round_trip_except_divergent_detached_at() {
    let Some(f) = fixture().await else { return };
    // Pre-existing test-timing note: `detached_at_ms` is stored as a
    // millisecond-truncated `bigint` (`(EXTRACT(EPOCH FROM now()) * 1000)::bigint`),
    // while `before` is captured with `Utc::now()`'s full sub-millisecond
    // precision. Postgres's real `now()` is always causally after `before`,
    // but truncation can floor it below `before` if both land in the same
    // millisecond — a small backward tolerance absorbs that without
    // weakening the actual assertion (that `detached_at` is a
    // freshly-stamped value, not the caller's stale fixture timestamp).
    let before = Utc::now() - chrono::Duration::milliseconds(5);
    let session = fixture_session("stream-1");
    f.fenced
        .upsert_session(session.clone())
        .await
        .expect("upsert_session");

    let loaded = f
        .fenced
        .get_session(&SmSessionId::new("stream-1"))
        .await
        .expect("get_session")
        .expect("session present");

    assert_eq!(loaded.stream_id, session.stream_id);
    assert_eq!(loaded.user_id, session.user_id);
    assert_eq!(loaded.jid, session.jid);
    assert_eq!(loaded.inbound_count, session.inbound_count);
    assert_eq!(loaded.max_resume_duration, session.max_resume_duration);

    // Divergence (a): `detached_at` as read back is the Postgres-computed
    // value, not the caller's stale 2020 fixture timestamp.
    assert_ne!(
        loaded.detached_at, session.detached_at,
        "detached_at must NOT be the caller-supplied value"
    );
    assert!(
        loaded.detached_at >= before,
        "detached_at must be stamped from Postgres now() at write time"
    );
}

/// #1206: the fenced backend must persist the resource's presence extension
/// payloads (XEP-0115 caps, XEP-0319 idle) so a cross-node resume rehydrates
/// them verbatim instead of coming back caps-less. Mirrors the portable
/// backend's `round_trip_session_preserves_presence_payloads`.
#[tokio::test]
async fn upsert_and_get_session_preserves_presence_payloads() {
    let Some(f) = fixture().await else { return };
    use xmpp_parsers::minidom::Element;

    let caps: Element = r#"<c xmlns='http://jabber.org/protocol/caps' hash='sha-1' node='https://example.com/client' ver='zHyEOgxTrkpSdGcQKH8EFPLsriY='/>"#
        .parse()
        .expect("valid XEP-0115 caps element");
    let idle: Element = r#"<idle xmlns='urn:xmpp:idle:1' since='2026-07-08T10:00:00+00:00'/>"#
        .parse()
        .expect("valid XEP-0319 idle element");

    let mut session = fixture_session("stream-payloads");
    session.presence_payloads = vec![caps.clone(), idle.clone()];
    f.fenced
        .upsert_session(session)
        .await
        .expect("upsert_session with payloads");

    let loaded = f
        .fenced
        .get_session(&SmSessionId::new("stream-payloads"))
        .await
        .expect("get_session")
        .expect("session present");

    assert_eq!(
        loaded.presence_payloads,
        vec![caps, idle],
        "fenced get_session must return the stored presence payloads verbatim and in order"
    );
}

#[tokio::test]
async fn store_session_atomic_also_applies_divergence_a() {
    let Some(f) = fixture().await else { return };
    // See `upsert_and_get_session_round_trip_except_divergent_detached_at`'s
    // comment: `detached_at_ms`'s millisecond truncation needs a small
    // backward tolerance against `Utc::now()`'s sub-millisecond precision.
    let before = Utc::now() - chrono::Duration::milliseconds(5);
    let session = fixture_session("stream-atomic");
    let mut unacked = vec![
        fixture_unacked("stream-atomic", 1),
        fixture_unacked("stream-atomic", 2),
    ];
    unacked[1].purpose = SmUnackedStanzaPurpose::ResumeBarrier;
    f.fenced
        .store_session_atomic(session.clone(), unacked)
        .await
        .expect("store_session_atomic");

    let loaded = f
        .fenced
        .get_session(&SmSessionId::new("stream-atomic"))
        .await
        .expect("get_session")
        .expect("session present");
    assert_ne!(loaded.detached_at, session.detached_at);
    assert!(loaded.detached_at >= before);

    let queue = f
        .fenced
        .list_unacked(&SmSessionId::new("stream-atomic"))
        .await
        .expect("list_unacked");
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0].sequence, 1);
    assert_eq!(queue[1].sequence, 2);
    assert_eq!(queue[0].purpose, SmUnackedStanzaPurpose::Application);
    assert_eq!(queue[1].purpose, SmUnackedStanzaPurpose::ResumeBarrier);
}

#[tokio::test]
async fn list_expired_sessions_ignores_caller_now_and_uses_postgres_now() {
    let Some(f) = fixture().await else { return };
    // detached_at is always Postgres now() (divergence a); a zero-length
    // resume window means the session is expired the instant it's
    // written, regardless of what `now` this call passes in.
    let mut session = fixture_session("stream-expired");
    session.max_resume_duration = StdDuration::from_millis(0);
    f.fenced
        .upsert_session(session)
        .await
        .expect("upsert_session");

    // Divergence (b): pass a `now` far in the past. If it were honored
    // literally, `detached_at (real now) + 0 <= (a year ago)` would be
    // false and the session would NOT show up as expired.
    let stale_now = Utc::now() - chrono::Duration::days(365);
    let expired = f
        .fenced
        .list_expired_sessions(stale_now)
        .await
        .expect("list_expired_sessions");
    assert!(
        expired
            .iter()
            .any(|s| s.stream_id.as_str() == "stream-expired"),
        "list_expired_sessions must evaluate the window against Postgres now(), \
         not the (stale) caller-supplied `now` parameter"
    );
}

#[tokio::test]
async fn delete_session_removes_session_and_unacked_queue() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-delete");
    f.fenced
        .upsert_session(fixture_session("stream-delete"))
        .await
        .expect("upsert_session");
    f.fenced
        .append_unacked(fixture_unacked("stream-delete", 1))
        .await
        .expect("append_unacked");

    f.fenced
        .delete_session(&stream_id)
        .await
        .expect("delete_session");

    assert!(f
        .fenced
        .get_session(&stream_id)
        .await
        .expect("get_session")
        .is_none());
    assert!(f
        .fenced
        .list_unacked(&stream_id)
        .await
        .expect("list_unacked")
        .is_empty());
}

#[tokio::test]
async fn authoritative_delete_reuses_the_guard_while_rotation_is_queued() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-authoritative-delete");
    f.fenced
        .upsert_session(fixture_session(stream_id.as_str()))
        .await
        .expect("upsert_session");
    let authority = f
        .shared_identity
        .guard_if_current(&f.identity)
        .await
        .expect("current authority");
    let rotating_identity = f.shared_identity.clone();
    let rotation = tokio::spawn(async move {
        rotating_identity
            .rotate(NodeIdentity::new("node-a", "next-incarnation"))
            .await;
    });
    tokio::task::yield_now().await;

    tokio::time::timeout(
        StdDuration::from_secs(1),
        f.fenced
            .delete_session_with_authority(&stream_id, &authority),
    )
    .await
    .expect("authoritative delete must not reacquire the queued identity gate")
    .expect("delete_session_with_authority");
    drop(authority);
    tokio::time::timeout(StdDuration::from_secs(1), rotation)
        .await
        .expect("rotation proceeds after delete")
        .expect("rotation task");
}

#[tokio::test]
async fn authoritative_delete_rejects_a_foreign_equal_identity_guard() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-foreign-authority");
    f.fenced
        .upsert_session(fixture_session(stream_id.as_str()))
        .await
        .expect("upsert_session");
    let foreign_identity = SharedNodeIdentity::new(f.identity.clone());
    let foreign_authority = foreign_identity
        .guard_if_current(&f.identity)
        .await
        .expect("equal-valued foreign authority");

    assert!(matches!(
        f.fenced
            .delete_session_with_authority(&stream_id, &foreign_authority)
            .await,
        Err(SmPersistenceError::NotOwner { .. })
    ));
    assert!(f
        .fenced
        .get_session(&stream_id)
        .await
        .expect("get_session")
        .is_some());
    let correct_authority = f
        .shared_identity
        .guard_if_current(&f.identity)
        .await
        .expect("correct authority");
    f.fenced
        .delete_session_with_authority(&stream_id, &correct_authority)
        .await
        .expect("foreign rejection must retain the legitimate fence cache");
}

#[tokio::test]
async fn append_ack_through_and_delete_unacked_lifecycle() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-unacked");
    f.fenced
        .upsert_session(fixture_session("stream-unacked"))
        .await
        .expect("upsert_session");
    for seq in 1..=3u32 {
        f.fenced
            .append_unacked(fixture_unacked("stream-unacked", seq))
            .await
            .expect("append_unacked");
    }

    let acked = f
        .fenced
        .ack_through(&stream_id, 2)
        .await
        .expect("ack_through");
    assert_eq!(acked, 2);
    let remaining = f
        .fenced
        .list_unacked(&stream_id)
        .await
        .expect("list_unacked");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].sequence, 3);

    let deleted = f
        .fenced
        .delete_unacked(&stream_id, &[3])
        .await
        .expect("delete_unacked");
    assert_eq!(deleted, 1);
    assert!(f
        .fenced
        .list_unacked(&stream_id)
        .await
        .expect("list_unacked")
        .is_empty());
}

#[tokio::test]
async fn record_promotion_failure_increments_and_returns_count() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-promo");
    f.fenced
        .upsert_session(fixture_session("stream-promo"))
        .await
        .expect("upsert_session");

    let first = f
        .fenced
        .record_promotion_failure(&stream_id)
        .await
        .expect("record_promotion_failure");
    let second = f
        .fenced
        .record_promotion_failure(&stream_id)
        .await
        .expect("record_promotion_failure");
    assert_eq!(first, 1);
    assert_eq!(second, 2);
}

#[tokio::test]
async fn record_promotion_failure_on_missing_stream_returns_zero() {
    let Some(f) = fixture().await else { return };
    let count = f
        .fenced
        .record_promotion_failure(&SmSessionId::new("does-not-exist"))
        .await
        .expect("record_promotion_failure");
    assert_eq!(count, 0);
}

/// The steal-committed-mid-transaction case, named explicitly in the
/// Slice 4 plan's Tests list: a claim stolen out from under this node
/// BEFORE a fenced write starts must make that write observe zero rows
/// from its `FOR SHARE` SELECT and abort — before touching
/// `sm_sessions`/`sm_unacked` at all.
#[tokio::test]
async fn delete_session_aborts_before_any_write_once_the_claim_is_stolen() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-stolen");
    f.fenced
        .upsert_session(fixture_session("stream-stolen"))
        .await
        .expect("upsert_session establishes the claim");

    // Make the current owner's node row stale, then steal the entity to a
    // different node — the fenced impl's cached generation is now invalid.
    seed_node(&f.claims_db, &f.identity, true).await;
    let entity = Entity::new(EntityType::SmSession, "stream-stolen".to_string());
    let owner_epoch = current_claim_epoch(&f, &entity).await;
    let stealer = live_stealer(&f.claims_db).await;
    f.claims
        .steal_stale(&entity, owner_epoch, StalePredicate::OwnerStale, &stealer)
        .await
        .expect("steal succeeds against a stale owner");

    let result = f.fenced.delete_session(&stream_id).await;
    assert!(
        matches!(result, Err(SmPersistenceError::NotOwner { .. })),
        "expected NotOwner, got {result:?}"
    );

    // The write never happened: the session row is untouched.
    assert!(
        f.fenced
            .get_session(&stream_id)
            .await
            .expect("get_session")
            .is_some(),
        "delete_session must roll back before deleting anything once fencing fails"
    );
}

/// Recreating a claim under the same stable node id but a new process
/// incarnation must not let an old exact fence mutate the replacement's
/// durable state. Every fenced persistence transaction binds both the
/// monotonic claim generation and `node_epoch`.
#[tokio::test]
async fn quarantine_rejects_same_node_id_new_incarnation_after_claim_recreation() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-incarnation-aba");
    let entity = Entity::new(EntityType::SmSession, stream_id.as_str().to_string());
    f.fenced
        .upsert_session(fixture_session(stream_id.as_str()))
        .await
        .expect("old incarnation establishes a claim and durable row");

    let old_epoch = f
        .claims
        .current_claim(&entity)
        .await
        .expect("read old claim")
        .expect("old claim exists")
        .claim_epoch;
    assert_eq!(
        f.claims
            .release_exact(&entity, &f.identity, old_epoch)
            .await
            .expect("release old claim"),
        waddle_xmpp::ownership::ExactReleaseOutcome::Released
    );

    let replacement =
        NodeIdentity::new(f.identity.node_id.clone(), uuid::Uuid::new_v4().to_string());
    f.claims
        .register(&replacement, None)
        .await
        .expect("same node id re-registers under a new incarnation");
    let replacement_epoch = f
        .claims
        .acquire(&entity, &replacement)
        .await
        .expect("replacement recreates claim with a fresh generation");
    assert!(
        replacement_epoch > old_epoch,
        "claim recreation must allocate a monotonic generation"
    );

    let old_fence = SmClaimFence::new(f.identity.clone(), old_epoch);
    f.shared_identity.rotate(replacement.clone()).await;
    let cached_write_error = f
        .fenced
        .upsert_session(fixture_session(stream_id.as_str()))
        .await
        .expect_err("cached old-incarnation fence must not be rebound to the replacement");
    assert!(matches!(
        cached_write_error,
        SmPersistenceError::NotOwner { .. }
    ));
    let error = f
        .fenced
        .quarantine_session(&stream_id, &old_fence)
        .await
        .expect_err("old-incarnation quarantine must fail exact fencing");
    assert!(matches!(error, SmPersistenceError::NotOwner { .. }));
    assert!(
        f.fenced
            .get_session(&stream_id)
            .await
            .expect("read durable row")
            .is_some(),
        "failed old-incarnation quarantine must preserve the new owner's durable row"
    );
}

#[tokio::test]
async fn identity_rotation_exactly_releases_the_cached_old_incarnation_before_retry() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-identity-rotation-cleanup");
    let entity = Entity::new(EntityType::SmSession, stream_id.as_str().to_string());
    f.fenced
        .upsert_session(fixture_session(stream_id.as_str()))
        .await
        .expect("old incarnation establishes the cached claim fence");

    let replacement =
        NodeIdentity::new(f.identity.node_id.clone(), uuid::Uuid::new_v4().to_string());
    f.shared_identity.rotate(replacement.clone()).await;

    let rotation_error = f
        .fenced
        .upsert_session(fixture_session(stream_id.as_str()))
        .await
        .expect_err("the first post-rotation write performs cleanup and retries explicitly");
    assert!(matches!(
        rotation_error,
        SmPersistenceError::NotOwner { .. }
    ));
    assert!(
        f.claims
            .current_claim(&entity)
            .await
            .expect("claim lookup after exact cleanup")
            .is_none(),
        "the stale incarnation must not block the replacement identity"
    );

    f.fenced
        .upsert_session(fixture_session(stream_id.as_str()))
        .await
        .expect("the replacement identity acquires on the explicit retry");
    let claim = f
        .claims
        .current_claim(&entity)
        .await
        .expect("replacement claim lookup")
        .expect("replacement claim exists");
    assert_eq!(claim.owner, replacement);
}

#[tokio::test]
async fn record_promotion_failure_aborts_before_any_write_once_the_claim_is_stolen() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-promo-stolen");
    f.fenced
        .upsert_session(fixture_session("stream-promo-stolen"))
        .await
        .expect("upsert_session establishes the claim");

    seed_node(&f.claims_db, &f.identity, true).await;
    let entity = Entity::new(EntityType::SmSession, "stream-promo-stolen".to_string());
    let owner_epoch = current_claim_epoch(&f, &entity).await;
    let stealer = live_stealer(&f.claims_db).await;
    f.claims
        .steal_stale(&entity, owner_epoch, StalePredicate::OwnerStale, &stealer)
        .await
        .expect("steal succeeds against a stale owner");

    let result = f.fenced.record_promotion_failure(&stream_id).await;
    assert!(
        matches!(result, Err(SmPersistenceError::NotOwner { .. })),
        "expected NotOwner, got {result:?}"
    );

    // The counter was never touched: a fresh (non-fenced) read shows 0.
    let conn = f.claims_db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT promotion_attempts FROM sm_sessions WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row present");
    let attempts: i64 = row.get(0).expect("promotion_attempts column");
    assert_eq!(attempts, 0, "UPDATE must not have run once fencing failed");
}

#[tokio::test]
async fn upsert_session_fails_closed_when_entity_already_claimed_by_another_node() {
    let Some(f) = fixture().await else { return };
    // A different node claims the entity first (simulating this SM-ID
    // already being owned elsewhere before this node ever saw it).
    let entity = Entity::new(EntityType::SmSession, "stream-preclaimed".to_string());
    let other = node_identity();
    f.claims.acquire(&entity, &other).await.expect("acquire");

    let result = f
        .fenced
        .upsert_session(fixture_session("stream-preclaimed"))
        .await;
    assert!(
        matches!(result, Err(SmPersistenceError::NotOwner { .. })),
        "expected NotOwner, got {result:?}"
    );
    assert!(
        f.fenced
            .get_session(&SmSessionId::new("stream-preclaimed"))
            .await
            .expect("get_session")
            .is_none(),
        "a failed claim acquire must never write the session row"
    );
}

#[tokio::test]
async fn fenced_transaction_guard_blocks_identity_rotation_until_commit_finishes() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-identity-commit-boundary");
    f.fenced
        .upsert_session(fixture_session(stream_id.as_str()))
        .await
        .expect("seed session and claim");
    let fence = f
        .fenced
        .claim_fence_for(&stream_id)
        .await
        .expect("claim fence");
    let mut tx = f.fenced.db.begin().await.expect("transaction");
    let identity_guard = f
        .fenced
        .assert_fenced(&mut tx, &stream_id, &fence)
        .await
        .expect("fenced transaction authority");

    let rotating_identity = f.shared_identity.clone();
    let replacement =
        NodeIdentity::new(f.identity.node_id.clone(), uuid::Uuid::new_v4().to_string());
    let rotation = tokio::spawn(async move {
        rotating_identity.rotate(replacement).await;
    });
    tokio::task::yield_now().await;
    assert!(
        !rotation.is_finished(),
        "identity rotation must wait while a fenced transaction is authorized"
    );

    tx.execute(
        "UPDATE sm_sessions SET inbound_count = inbound_count + 1 WHERE stream_id = ?",
        crate::db_params![stream_id.as_str().to_string()],
    )
    .await
    .expect("guarded mutation");
    tx.commit().await.expect("guarded commit");
    assert!(
        !rotation.is_finished(),
        "commit completion alone must not release identity authority early"
    );
    drop(identity_guard);
    rotation.await.expect("rotation task");
}

/// Proves `delete_session`'s fencing check and its subsequent DELETEs are
/// genuinely atomic — not merely "usually fine": a concurrent steal can
/// never land in the window between the `FOR SHARE` SELECT succeeding and
/// the DELETEs committing, because both contend for the same
/// `clustering_claims` row. Modeled on
/// `clustering::claims::tests::steal_commit_interleaved_inside_a_fenced_transaction`.
#[tokio::test]
async fn concurrent_steal_vs_delete_session_never_produces_torn_state() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-race");
    f.fenced
        .upsert_session(fixture_session("stream-race"))
        .await
        .expect("upsert_session");
    seed_node(&f.claims_db, &f.identity, true).await;

    let entity = Entity::new(EntityType::SmSession, "stream-race".to_string());
    let owner_epoch = current_claim_epoch(&f, &entity).await;
    let stealer = live_stealer(&f.claims_db).await;

    let delete_result = f.fenced.delete_session(&stream_id);
    let steal_result =
        f.claims
            .steal_stale(&entity, owner_epoch, StalePredicate::OwnerStale, &stealer);
    let (delete_outcome, _steal_outcome) = tokio::join!(delete_result, steal_result);

    // Whichever statement's transaction actually committed first, the
    // session row must be in a *consistent* state: either fully deleted
    // (delete_session won the race before the steal changed the epoch) or
    // fully intact (the steal's epoch bump landed first, so
    // delete_session's FOR SHARE SELECT saw zero rows and rolled back
    // before deleting anything) — never partially deleted.
    let session_present = f
        .fenced
        .get_session(&stream_id)
        .await
        .expect("get_session")
        .is_some();
    let unacked_present = !f
        .fenced
        .list_unacked(&stream_id)
        .await
        .expect("list_unacked")
        .is_empty();

    match delete_outcome {
        Ok(()) => {
            assert!(
                !session_present,
                "delete_session Ok(()) must mean the row is gone"
            );
            assert!(!unacked_present);
        }
        Err(SmPersistenceError::NotOwner { .. }) => {
            assert!(
                session_present,
                "delete_session NotOwner must mean the row was never touched"
            );
        }
        Err(other) => panic!("unexpected delete_session error: {other:?}"),
    }
}

/// FIX 6: `store_session_atomic`'s own fencing check must abort the
/// transaction — before the DELETE-then-INSERT-then-append sequence runs
/// at all — once the claim has been stolen out from under this node.
/// Mirrors `delete_session_aborts_before_any_write_once_the_claim_is_stolen`.
#[tokio::test]
async fn store_session_atomic_aborts_before_any_write_once_the_claim_is_stolen() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-atomic-stolen");
    f.fenced
        .upsert_session(fixture_session("stream-atomic-stolen"))
        .await
        .expect("upsert_session establishes the claim");
    f.fenced
        .append_unacked(fixture_unacked("stream-atomic-stolen", 1))
        .await
        .expect("append_unacked establishes an original unacked row");

    seed_node(&f.claims_db, &f.identity, true).await;
    let entity = Entity::new(EntityType::SmSession, "stream-atomic-stolen".to_string());
    let owner_epoch = current_claim_epoch(&f, &entity).await;
    let stealer = live_stealer(&f.claims_db).await;
    f.claims
        .steal_stale(&entity, owner_epoch, StalePredicate::OwnerStale, &stealer)
        .await
        .expect("steal succeeds against a stale owner");

    let mut new_session = fixture_session("stream-atomic-stolen");
    new_session.inbound_count = 999;
    let new_unacked = vec![fixture_unacked("stream-atomic-stolen", 2)];
    let result = f
        .fenced
        .store_session_atomic(new_session, new_unacked)
        .await;
    assert!(
        matches!(result, Err(SmPersistenceError::NotOwner { .. })),
        "expected NotOwner, got {result:?}"
    );

    // The write never happened: the original session and its original
    // unacked row are both untouched.
    let loaded = f
        .fenced
        .get_session(&stream_id)
        .await
        .expect("get_session")
        .expect("session present");
    assert_eq!(
        loaded.inbound_count, 7,
        "store_session_atomic must roll back before touching sm_sessions once fencing fails"
    );
    let queue = f
        .fenced
        .list_unacked(&stream_id)
        .await
        .expect("list_unacked");
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].sequence, 1);
}

/// FIX 6: the same steal-vs-write race as
/// `concurrent_steal_vs_delete_session_never_produces_torn_state`, exercised
/// against `store_session_atomic`'s own DELETE-then-INSERT-then-append
/// sequence: whichever side commits first, the durable state must never end
/// up torn between the old and new session/unacked rows.
#[tokio::test]
async fn concurrent_steal_vs_store_session_atomic_never_produces_torn_state() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-atomic-race");
    f.fenced
        .upsert_session(fixture_session("stream-atomic-race"))
        .await
        .expect("upsert_session");
    f.fenced
        .append_unacked(fixture_unacked("stream-atomic-race", 1))
        .await
        .expect("append_unacked establishes an original unacked row");
    seed_node(&f.claims_db, &f.identity, true).await;

    let entity = Entity::new(EntityType::SmSession, "stream-atomic-race".to_string());
    let owner_epoch = current_claim_epoch(&f, &entity).await;
    let stealer = live_stealer(&f.claims_db).await;

    let mut new_session = fixture_session("stream-atomic-race");
    new_session.inbound_count = 999;
    let new_unacked = vec![fixture_unacked("stream-atomic-race", 2)];

    let store_result = f.fenced.store_session_atomic(new_session, new_unacked);
    let steal_result =
        f.claims
            .steal_stale(&entity, owner_epoch, StalePredicate::OwnerStale, &stealer);
    let (store_outcome, _steal_outcome) = tokio::join!(store_result, steal_result);

    let loaded = f
        .fenced
        .get_session(&stream_id)
        .await
        .expect("get_session")
        .expect("session present");
    let queue = f
        .fenced
        .list_unacked(&stream_id)
        .await
        .expect("list_unacked");

    match store_outcome {
        Ok(()) => {
            assert_eq!(
                loaded.inbound_count, 999,
                "store_session_atomic Ok(()) must mean the new session landed"
            );
            assert_eq!(queue.len(), 1);
            assert_eq!(queue[0].sequence, 2);
        }
        Err(SmPersistenceError::NotOwner { .. }) => {
            assert_eq!(
                loaded.inbound_count, 7,
                "store_session_atomic NotOwner must mean the original row survived untouched"
            );
            assert_eq!(queue.len(), 1);
            assert_eq!(
                queue[0].sequence, 1,
                "the original unacked row must survive untouched"
            );
        }
        Err(other) => panic!("unexpected store_session_atomic error: {other:?}"),
    }
}

/// FIX 6: the same steal-vs-write race, exercised against
/// `record_promotion_failure`'s fenced `UPDATE ... RETURNING`.
#[tokio::test]
async fn concurrent_steal_vs_record_promotion_failure_never_produces_torn_state() {
    let Some(f) = fixture().await else { return };
    let stream_id = SmSessionId::new("stream-promo-race");
    f.fenced
        .upsert_session(fixture_session("stream-promo-race"))
        .await
        .expect("upsert_session");
    seed_node(&f.claims_db, &f.identity, true).await;

    let entity = Entity::new(EntityType::SmSession, "stream-promo-race".to_string());
    let owner_epoch = current_claim_epoch(&f, &entity).await;
    let stealer = live_stealer(&f.claims_db).await;

    let promo_result = f.fenced.record_promotion_failure(&stream_id);
    let steal_result =
        f.claims
            .steal_stale(&entity, owner_epoch, StalePredicate::OwnerStale, &stealer);
    let (promo_outcome, _steal_outcome) = tokio::join!(promo_result, steal_result);

    let conn = f.claims_db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT promotion_attempts FROM sm_sessions WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row present");
    let attempts: i64 = row.get(0).expect("promotion_attempts column");

    match promo_outcome {
        Ok(count) => {
            assert_eq!(
                count, 1,
                "record_promotion_failure Ok(1) must mean the UPDATE committed"
            );
            assert_eq!(attempts, 1);
        }
        Err(SmPersistenceError::NotOwner { .. }) => {
            assert_eq!(
                attempts, 0,
                "record_promotion_failure NotOwner must mean the UPDATE never ran"
            );
        }
        Err(other) => panic!("unexpected record_promotion_failure error: {other:?}"),
    }
}

/// FIX 6: `list_all_sessions` round-trips every persisted session under the
/// fenced impl (read-only; not itself claim-scoped — see the trait method's
/// own doc comment — but must still faithfully enumerate every row).
#[tokio::test]
async fn list_all_sessions_round_trips_every_persisted_session() {
    let Some(f) = fixture().await else { return };
    f.fenced
        .upsert_session(fixture_session("stream-list-a"))
        .await
        .expect("upsert_session a");
    f.fenced
        .upsert_session(fixture_session("stream-list-b"))
        .await
        .expect("upsert_session b");

    let sessions = f
        .fenced
        .list_all_sessions()
        .await
        .expect("list_all_sessions");
    let mut stream_ids: Vec<String> = sessions
        .iter()
        .map(|s| s.stream_id.as_str().to_string())
        .collect();
    stream_ids.sort();
    assert_eq!(
        stream_ids,
        vec!["stream-list-a".to_string(), "stream-list-b".to_string()]
    );
}

/// FIX 1: two concurrent first writes for the same brand-new, not-yet-
/// claimed stream_id must both succeed (the `ensure_claimed` self-reacquire
/// path, layered under the per-key `OnceCell` single-flight) and leave
/// exactly one `clustering_claims` row behind for the entity.
#[tokio::test]
async fn concurrent_first_writes_for_a_fresh_stream_id_both_succeed_exactly_one_claim() {
    let Some(f) = fixture().await else { return };
    let claims_db = f.claims_db.clone();
    let fenced = std::sync::Arc::new(f.fenced);
    let stream_id = "stream-concurrent-first-write";

    let fenced_a = fenced.clone();
    let fenced_b = fenced.clone();
    let task_a =
        tokio::spawn(async move { fenced_a.upsert_session(fixture_session(stream_id)).await });
    let task_b =
        tokio::spawn(async move { fenced_b.upsert_session(fixture_session(stream_id)).await });
    let (result_a, result_b) = tokio::join!(task_a, task_b);
    result_a
        .expect("task a join")
        .expect("task a upsert_session must succeed");
    result_b
        .expect("task b join")
        .expect("task b upsert_session must succeed");

    let conn = claims_db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM clustering_claims WHERE entity = ?",
            crate::db_params![format!("sm_session:{stream_id}")],
        )
        .await
        .expect("count query");
    let count: i64 = rows
        .next()
        .await
        .expect("row")
        .expect("row present")
        .get(0)
        .expect("count column");
    assert_eq!(count, 1, "exactly one claims row must exist for the entity");
}

// ---------------------------------------------------------------------
// ADR-0017 Phase 3 Slice 5: `InMemorySmSessionRegistry::restore_from_persistence`
// acquire-then-hydrate, exercised against two simulated nodes sharing one
// Postgres database (mirrors this module's own `fixture()` pattern, one
// level up: two `PostgresFencedSmPersistence`/`InMemorySmSessionRegistry`
// pairs instead of one).
// ---------------------------------------------------------------------

/// A second simulated node, sharing `f`'s underlying Postgres database
/// (`claims_db`) but under its own fresh [`NodeIdentity`]/[`SharedNodeIdentity`]
/// and its own [`PostgresFencedSmPersistence`]/[`InMemorySmSessionRegistry`]
/// pair — exactly what a genuinely different process in the same cluster
/// would look like.
struct SecondNode {
    registry: waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    identity: NodeIdentity,
}

async fn second_node(f: &Fixture) -> SecondNode {
    let identity = node_identity();
    let shared_identity = SharedNodeIdentity::new(identity.clone());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(f.claims_db.clone()));
    let fenced = PostgresFencedSmPersistence::open(
        f.claims_db.clone(),
        claim_store,
        shared_identity.clone(),
    )
    .await
    .expect("open second node's fenced SM persistence");
    let registry_claim_store: Arc<dyn ClaimStore> =
        Arc::new(PostgresClaimStore::new(f.claims_db.clone()));
    let registry = waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()
        .with_persistence(std::sync::Arc::new(fenced))
        .with_claim_store(registry_claim_store, shared_identity);
    SecondNode { registry, identity }
}

#[tokio::test]
async fn restore_from_persistence_hydrates_only_unclaimed_or_self_claimed_rows() {
    let Some(f) = fixture().await else { return };
    // `f.fenced` (bound to `f.identity`, "node A") claims+persists
    // "stream-a" via its own normal write path — exactly how a real
    // detach on node A would.
    f.fenced
        .upsert_session(fixture_session("stream-a"))
        .await
        .expect("node A claims and persists stream-a");

    let node_b = second_node(&f).await;
    // `node_b`'s own persistence claims+persists "stream-b" under node B's
    // identity — self-claimed from B's perspective.
    let persisted_b = node_b
        .registry
        .restore_from_persistence()
        .await
        .expect("first restore on node B (nothing self-claimed yet)");
    assert_eq!(
        persisted_b, 0,
        "node B must not hydrate node A's still-live claim on stream-a"
    );
    assert_eq!(
        node_b.registry.session_count().await,
        0,
        "stream-a must not appear in node B's in-memory view"
    );

    // Now genuinely self-claim a row under node B (mirrors a live detach
    // happening on node B) and restore again.
    let claim_store_b: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(f.claims_db.clone()));
    let fenced_b_direct = PostgresFencedSmPersistence::open(
        f.claims_db.clone(),
        claim_store_b,
        SharedNodeIdentity::new(node_b.identity.clone()),
    )
    .await
    .expect("open a direct fenced handle sharing node B's identity");
    fenced_b_direct
        .upsert_session(fixture_session("stream-b"))
        .await
        .expect("node B claims and persists stream-b");

    let persisted_b_second_pass = node_b
        .registry
        .restore_from_persistence()
        .await
        .expect("second restore on node B");
    assert_eq!(
        persisted_b_second_pass, 1,
        "node B must self-reacquire and hydrate its own stream-b, still skipping stream-a"
    );
    assert!(
        node_b
            .registry
            .peek_session("stream-b")
            .await
            .expect("peek stream-b")
            .is_some(),
        "stream-b (self-claimed) must be hydrated"
    );
    assert!(
        node_b
            .registry
            .peek_session("stream-a")
            .await
            .expect("peek stream-a")
            .is_none(),
        "stream-a (claimed by node A) must never be hydrated by node B"
    );

    // A fresh registry instance under the SAME identity (simulating node
    // B restarting) must self-reacquire stream-b again via
    // `ensure_claimed`'s self-match — never spuriously conflict with its
    // own pre-restart claim.
    let claim_store_b2: Arc<dyn ClaimStore> =
        Arc::new(PostgresClaimStore::new(f.claims_db.clone()));
    let fenced_b2: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        Arc::new(
            PostgresFencedSmPersistence::open(
                f.claims_db.clone(),
                claim_store_b2.clone(),
                SharedNodeIdentity::new(node_b.identity.clone()),
            )
            .await
            .expect("open node B's post-restart fenced handle"),
        );
    let registry_b2 = waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()
        .with_persistence(fenced_b2)
        .with_claim_store(
            claim_store_b2,
            SharedNodeIdentity::new(node_b.identity.clone()),
        );
    let persisted_after_restart = registry_b2
        .restore_from_persistence()
        .await
        .expect("restore after simulated restart");
    assert_eq!(
        persisted_after_restart, 1,
        "node B's own restart must self-reacquire its own pre-restart claim on stream-b"
    );
}
