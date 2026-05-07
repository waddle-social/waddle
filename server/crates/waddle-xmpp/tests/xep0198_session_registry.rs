//! XEP-0198: Stream Management detached session registry suite.

use std::time::{Duration, Instant};

use jid::{BareJid, FullJid, Jid};
use waddle_xmpp::stream_management::{
    DetachedSession, InMemorySmSessionRegistry, SmClaimCompletion, SmSessionRegistry,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Body, Message};

fn detached_session(stream_id: &str, jid: &str) -> DetachedSession {
    DetachedSession {
        stream_id: stream_id.to_string(),
        user_id: jid.to_string(),
        jid: jid.parse().expect("valid full jid"),
        inbound_count: 7,
        outbound_count: 11,
        last_acked: 10,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: Instant::now(),
        carbons_enabled: true,
        roster_interested: true,
        presence_available: true,
        presence_show: None,
        presence_status: Some("available".to_string()),
        presence_priority: 0,
    }
}

fn expiring_detached_session(stream_id: &str, jid: &str) -> DetachedSession {
    let mut session = detached_session(stream_id, jid);
    session.max_resume_time = Some(1);
    session
}

fn chat_stanza(to: &FullJid, body: &str) -> Stanza {
    let mut message = Message::new(Some(Jid::from(to.clone())));
    message.id = Some(format!("msg-{}", body.len()));
    message.bodies.insert(String::new(), Body(body.to_string()));
    Stanza::Message(message)
}

#[tokio::test]
async fn xep0198_claimed_session_remains_writable_until_completed() {
    let registry = InMemorySmSessionRegistry::new();
    let jid: FullJid = "alice@example.test/phone".parse().expect("valid jid");
    registry
        .store_session(detached_session("stream-1", jid.as_str()))
        .await
        .expect("store detached session");

    let claimed = registry
        .claim_session("stream-1")
        .await
        .expect("claim session")
        .expect("session exists");
    assert_eq!(claimed.unacked_stanzas.len(), 0);

    assert!(registry
        .record_stanza_for_detached_bound_resource(
            &jid,
            &chat_stanza(&jid, "handoff"),
            chrono::Utc::now(),
        )
        .await
        .expect("record during claim"));

    let completed = registry
        .complete_claim("stream-1")
        .await
        .expect("complete claim")
        .expect("claimed session returned");
    let SmClaimCompletion::Resumed(completed) = completed else {
        panic!("claim should still be resumable");
    };
    assert_eq!(completed.unacked_stanzas.len(), 1);
    assert!(completed.unacked_stanzas[0].stanza_xml.contains("handoff"));
    assert_eq!(registry.session_count().await, 0);
}

#[tokio::test]
async fn xep0198_releasing_claim_restores_session_with_handoff_records() {
    let registry = InMemorySmSessionRegistry::new();
    let jid: FullJid = "alice@example.test/tablet".parse().expect("valid jid");
    registry
        .store_session(detached_session("stream-2", jid.as_str()))
        .await
        .expect("store detached session");
    registry
        .claim_session("stream-2")
        .await
        .expect("claim session")
        .expect("session exists");

    assert!(registry
        .record_stanza_for_detached_bound_resource(
            &jid,
            &chat_stanza(&jid, "retry"),
            chrono::Utc::now(),
        )
        .await
        .expect("record during failed resume"));
    registry
        .release_claim("stream-2")
        .await
        .expect("release claim");

    let restored = registry
        .take_session("stream-2")
        .await
        .expect("take restored session")
        .expect("session restored");
    assert_eq!(restored.unacked_stanzas.len(), 1);
    assert!(restored.unacked_stanzas[0].stanza_xml.contains("retry"));
}

#[tokio::test]
async fn xep0198_release_claim_drops_expired_claimed_session() {
    let registry = InMemorySmSessionRegistry::new();
    let jid: FullJid = "alice@example.test/watch".parse().expect("valid jid");
    registry
        .store_session(expiring_detached_session(
            "stream-expiring-claim",
            jid.as_str(),
        ))
        .await
        .expect("store expiring detached session");
    registry
        .claim_session("stream-expiring-claim")
        .await
        .expect("claim session")
        .expect("session exists");
    tokio::time::sleep(Duration::from_secs(2)).await;

    registry
        .release_claim("stream-expiring-claim")
        .await
        .expect("release expired claim");

    assert!(registry
        .take_session("stream-expiring-claim")
        .await
        .expect("take expired released claim")
        .is_none());
    assert!(!registry
        .record_stanza_for_detached_bound_resource(
            &jid,
            &chat_stanza(&jid, "expired"),
            chrono::Utc::now(),
        )
        .await
        .expect("record against expired released claim"));
}

#[tokio::test]
async fn xep0198_complete_claim_drops_expired_claimed_session() {
    let registry = InMemorySmSessionRegistry::new();
    let jid: FullJid = "alice@example.test/radio".parse().expect("valid jid");
    registry
        .store_session(expiring_detached_session(
            "stream-complete-expired-claim",
            jid.as_str(),
        ))
        .await
        .expect("store expiring detached session");
    registry
        .claim_session("stream-complete-expired-claim")
        .await
        .expect("claim session")
        .expect("session exists before expiry");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let completed = registry
        .complete_claim("stream-complete-expired-claim")
        .await
        .expect("complete expired claim")
        .expect("expired claim returns cleanup session");
    assert!(
        matches!(completed, SmClaimCompletion::Expired(session) if session.stream_id == "stream-complete-expired-claim")
    );
    assert!(registry
        .take_session("stream-complete-expired-claim")
        .await
        .expect("take expired completed claim")
        .is_none());
}

#[tokio::test]
async fn xep0198_expired_claimed_sessions_are_hidden_from_detached_fanout() {
    let registry = InMemorySmSessionRegistry::new();
    let jid: FullJid = "alice@example.test/tv".parse().expect("valid jid");
    let bare: BareJid = "alice@example.test".parse().expect("valid bare jid");
    registry
        .store_session(expiring_detached_session(
            "stream-expired-claim",
            jid.as_str(),
        ))
        .await
        .expect("store expiring detached session");
    registry
        .claim_session("stream-expired-claim")
        .await
        .expect("claim expiring session")
        .expect("session exists before expiry");
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(registry
        .interested_detached_resources_for_user(&bare)
        .await
        .expect("interested resources")
        .is_empty());
    assert!(registry
        .detached_carbon_resources_for_user(&bare, &jid)
        .await
        .expect("carbon resources")
        .is_empty());
    assert!(!registry
        .record_stanza_for_detached_resource(
            &jid,
            &chat_stanza(&jid, "expired"),
            chrono::Utc::now(),
        )
        .await
        .expect("record expired claimed interested"));
}

#[tokio::test]
async fn xep0198_fresh_bind_invalidation_removes_claimed_session() {
    let registry = InMemorySmSessionRegistry::new();
    let jid: FullJid = "alice@example.test/phone".parse().expect("valid jid");
    registry
        .store_session(detached_session("stream-claim", jid.as_str()))
        .await
        .expect("store detached session");
    registry
        .claim_session("stream-claim")
        .await
        .expect("claim session")
        .expect("session exists");

    let invalidated = registry
        .invalidate_sessions_for_jid(&jid)
        .await
        .expect("invalidate claimed session");
    assert_eq!(invalidated.len(), 1);
    assert_eq!(invalidated[0].stream_id, "stream-claim");
    assert!(registry
        .complete_claim("stream-claim")
        .await
        .expect("complete invalidated claim")
        .is_none());
    registry
        .release_claim("stream-claim")
        .await
        .expect("release invalidated claim");
    assert!(registry
        .take_session("stream-claim")
        .await
        .expect("take invalidated session")
        .is_none());
}

#[tokio::test]
async fn xep0198_replacing_same_jid_does_not_evict_unrelated_capacity_entry() {
    let registry = InMemorySmSessionRegistry::with_capacity(2);
    let old_jid: FullJid = "alice@example.test/phone".parse().expect("valid jid");
    let other_jid: FullJid = "bob@example.test/phone".parse().expect("valid jid");
    let mut old = detached_session("old-alice", old_jid.as_str());
    old.detached_at = Instant::now() - Duration::from_secs(20);
    let mut other = detached_session("bob", other_jid.as_str());
    other.detached_at = Instant::now() - Duration::from_secs(10);

    registry.store_session(old).await.expect("store old alice");
    registry.store_session(other).await.expect("store bob");
    registry
        .store_session(detached_session("new-alice", old_jid.as_str()))
        .await
        .expect("store replacement alice");

    assert!(registry
        .take_session("bob")
        .await
        .expect("take bob")
        .is_some());
    assert!(registry
        .take_session("old-alice")
        .await
        .expect("take old alice")
        .is_none());
    assert!(registry
        .take_session("new-alice")
        .await
        .expect("take new alice")
        .is_some());
}

#[tokio::test]
async fn xep0198_detached_resource_lists_preserve_stream_flags() {
    let registry = InMemorySmSessionRegistry::new();
    let alice: BareJid = "alice@example.test".parse().expect("valid bare jid");
    let phone: FullJid = "alice@example.test/phone".parse().expect("valid jid");
    let tablet: FullJid = "alice@example.test/tablet".parse().expect("valid jid");
    let laptop: FullJid = "alice@example.test/laptop".parse().expect("valid jid");

    let mut tablet_session = detached_session("stream-tablet", tablet.as_str());
    tablet_session.carbons_enabled = false;
    let mut laptop_session = detached_session("stream-laptop", laptop.as_str());
    laptop_session.roster_interested = false;
    laptop_session.presence_available = false;

    registry
        .store_session(detached_session("stream-phone", phone.as_str()))
        .await
        .expect("store phone");
    registry
        .store_session(tablet_session)
        .await
        .expect("store tablet");
    registry
        .store_session(laptop_session)
        .await
        .expect("store laptop");

    let interested = registry
        .interested_detached_resources_for_user(&alice)
        .await
        .expect("list roster interested");
    assert!(interested.contains(&phone));
    assert!(interested.contains(&tablet));
    assert!(!interested.contains(&laptop));

    let carbon = registry
        .detached_carbon_resources_for_user(&alice, &phone)
        .await
        .expect("list carbon resources");
    assert!(!carbon.contains(&phone));
    assert!(!carbon.contains(&tablet));
    assert!(carbon.contains(&laptop));

    let available = registry
        .available_detached_resources_for_user(&alice)
        .await
        .expect("list available resources");
    assert!(available.contains(&phone));
    assert!(available.contains(&tablet));
    assert!(!available.contains(&laptop));
}

// -----------------------------------------------------------------------------
// Slice (d) PRs #346, #344, #361 — extended XEP-0198 contract coverage.
// -----------------------------------------------------------------------------

/// Locked Q8 = B (issue #209 PR #344): SM session round-trips
/// through `SmPersistenceStorage` survive a process restart. Detach
/// → write → restore → resume.
#[tokio::test]
async fn xep0198_session_round_trips_through_persistence() {
    use std::sync::Arc;
    use waddle_xmpp::stream_management::persistence::InMemorySmPersistence;

    let persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    registry
        .store_session(detached_session(
            "stream-restart",
            "alice@example.com/laptop",
        ))
        .await
        .expect("store");

    // Simulate restart: drop the registry, build a fresh one over the
    // same persistence handle, restore.
    drop(registry);
    let restored = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    let count = restored
        .restore_from_persistence()
        .await
        .expect("restore from persistence");
    assert_eq!(count, 1, "one session restored");

    // The restored session is resumable.
    let resumed = restored
        .claim_session("stream-restart")
        .await
        .expect("claim restored session")
        .expect("session present after restore");
    assert_eq!(resumed.jid.to_string(), "alice@example.com/laptop");
    assert!(resumed.carbons_enabled);
}

/// Locked Q8 = B persist-after-promotion contract (issue #209
/// PR #346, post-Copilot-review): `drain_expired` and
/// `drain_all_for_shutdown` MUST NOT delete the durable SM row
/// up-front. Only `confirm_drained` performs the durable delete,
/// and the caller invokes it AFTER successful Q6 promotion. This
/// way a partial-promotion failure (panic, storage error) leaves
/// the unacked queue intact for restart-time retry.
#[tokio::test]
async fn xep0198_drain_expired_does_not_delete_durable_row_until_confirmed() {
    use std::sync::Arc;
    use waddle_xmpp::stream_management::persistence::InMemorySmPersistence;

    let persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    registry
        .store_session(expiring_detached_session(
            "stream-drain-no-delete",
            "alice@example.com/laptop",
        ))
        .await
        .expect("store");

    // Wait for expiry then drain.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let drained = registry
        .drain_expired()
        .await
        .expect("drain_expired succeeds");
    assert_eq!(drained.len(), 1, "one expired session drained");

    // Critical: durable row STILL present until confirm_drained.
    let stored = persistence
        .get_session(&waddle_xmpp::pending_delivery::SmSessionId::new(
            "stream-drain-no-delete",
        ))
        .await
        .expect("get durable session");
    assert!(
        stored.is_some(),
        "drain_expired MUST NOT delete durable row up-front (PR #346 Copilot review)"
    );

    // Confirm: now the durable row is deleted.
    registry.confirm_drained("stream-drain-no-delete").await;
    let after = persistence
        .get_session(&waddle_xmpp::pending_delivery::SmSessionId::new(
            "stream-drain-no-delete",
        ))
        .await
        .expect("get durable session post-confirm");
    assert!(
        after.is_none(),
        "confirm_drained deletes the durable row after successful promotion"
    );
}

/// Same persist-after-promotion contract for the graceful-shutdown
/// path: `drain_all_for_shutdown` removes from in-memory but leaves
/// durable rows for restart recovery.
#[tokio::test]
async fn xep0198_drain_all_for_shutdown_does_not_delete_durable_row_until_confirmed() {
    use std::sync::Arc;
    use waddle_xmpp::stream_management::persistence::InMemorySmPersistence;

    let persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    registry
        .store_session(detached_session(
            "stream-shutdown-no-delete",
            "alice@example.com/laptop",
        ))
        .await
        .expect("store");

    // Shutdown drain (NOT expiry-based — pulls everything).
    let drained = registry
        .drain_all_for_shutdown()
        .await
        .expect("drain_all_for_shutdown succeeds");
    assert_eq!(drained.len(), 1, "one live session drained");

    // Durable row preserved — restart-recovery path.
    let stored = persistence
        .get_session(&waddle_xmpp::pending_delivery::SmSessionId::new(
            "stream-shutdown-no-delete",
        ))
        .await
        .expect("get durable session");
    assert!(
        stored.is_some(),
        "drain_all_for_shutdown MUST NOT delete durable row up-front \
         so restart can restore it (PR #346 Copilot review)"
    );

    registry.confirm_drained("stream-shutdown-no-delete").await;
    let after = persistence
        .get_session(&waddle_xmpp::pending_delivery::SmSessionId::new(
            "stream-shutdown-no-delete",
        ))
        .await
        .expect("get durable session post-confirm");
    assert!(after.is_none());
}

/// Issue #209 PR #361: `DetachedSession.unacked_stanzas` carries
/// `original_receipt_at` per stanza. The Q6 SM-expiry promotion
/// path consumes this for the XEP-0203 `<delay/>` stamp on offline
/// replays. Verify the field round-trips through detach + restore.
#[tokio::test]
async fn xep0198_unacked_original_receipt_at_round_trips_through_persistence() {
    use chrono::TimeZone;
    use std::sync::Arc;
    use waddle_xmpp::stream_management::{
        persistence::InMemorySmPersistence, DetachedUnackedStanza,
    };

    let persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));

    let t1 = chrono::Utc
        .with_ymd_and_hms(2026, 5, 1, 12, 0, 0)
        .single()
        .expect("valid time");
    let mut session = detached_session("stream-receipt-rtt", "alice@example.com/laptop");
    // Use jabber:client namespace so the persistence layer's XML
    // round-trip can re-parse the stanza into a typed Stanza.
    session.unacked_stanzas.push(DetachedUnackedStanza {
        sequence: 12,
        stanza_xml: "<message xmlns='jabber:client' id='m12'><body>queued at T1</body></message>"
            .to_string(),
        original_receipt_at: t1,
    });
    registry.store_session(session).await.expect("store");

    // Simulate restart and restore.
    drop(registry);
    let restored = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    restored.restore_from_persistence().await.expect("restore");
    let claimed = restored
        .claim_session("stream-receipt-rtt")
        .await
        .expect("claim restored")
        .expect("present");
    assert_eq!(claimed.unacked_stanzas.len(), 1);
    assert_eq!(
        claimed.unacked_stanzas[0].original_receipt_at, t1,
        "original_receipt_at survives detach + persist + restore + claim"
    );
}

/// Issue #209 finding #3: `drain_all_for_shutdown` must NOT pull
/// sessions out of `claimed_sessions`. A claimed session has an
/// in-flight `<resume previd='…'/>` between `claim_session` and
/// `complete_claim`. Draining it here causes duplicate delivery —
/// the resuming connection gets the SM replay AND the shutdown
/// drain re-promotes the same unacked queue through Q6, generating
/// a fresh `pending_delivery` row that re-flushes on next presence.
#[tokio::test]
async fn xep0198_drain_all_for_shutdown_skips_claimed_sessions() {
    let registry = InMemorySmSessionRegistry::new();
    registry
        .store_session(detached_session(
            "stream-resuming",
            "alice@example.com/laptop",
        ))
        .await
        .expect("store");
    registry
        .store_session(detached_session("stream-detached", "bob@example.com/web"))
        .await
        .expect("store");

    // Mid-flight resume: session-A is now in claimed_sessions.
    let _claimed = registry
        .claim_session("stream-resuming")
        .await
        .expect("claim")
        .expect("session present");

    // Shutdown drain fires now (SIGTERM mid-resume).
    let drained = registry
        .drain_all_for_shutdown()
        .await
        .expect("drain_all_for_shutdown");
    let drained_ids: Vec<_> = drained.iter().map(|s| s.stream_id.as_str()).collect();
    assert!(
        drained_ids.contains(&"stream-detached"),
        "detached session is drained"
    );
    assert!(
        !drained_ids.contains(&"stream-resuming"),
        "claimed (resuming) session must NOT be drained — \
         the in-flight <resume/> path is responsible for its lifecycle"
    );

    // The claimed session is still present in the registry — the
    // resuming connection can proceed with `complete_claim`.
    let still_claimed = registry
        .complete_claim("stream-resuming")
        .await
        .expect("complete claim");
    assert!(
        still_claimed.is_some(),
        "complete_claim succeeds because shutdown drain did not steal the session"
    );
}

/// Issue #209 finding #8: stanzas appended via
/// `record_outbound_for_detached_stream_at` (the second-detach drain
/// path) must mirror to durable persistence so a process crash before
/// resume doesn't lose them. Earlier code only updated the in-memory
/// view, so the durable `sm_unacked` table held the snapshot from
/// `store_session` but missed any frame that arrived in `outbound_rx`
/// between the first drain and the registry unregister.
#[tokio::test]
async fn xep0198_record_outbound_for_detached_stream_at_persists_durably() {
    use std::sync::Arc;
    use waddle_xmpp::stream_management::persistence::InMemorySmPersistence;

    let persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    registry
        .store_session(detached_session(
            "stream-late-drain",
            "alice@example.com/laptop",
        ))
        .await
        .expect("store");

    let t1 = chrono::Utc::now();
    let recorded = registry
        .record_outbound_for_detached_stream_at(
            "stream-late-drain",
            42,
            "<message xmlns='jabber:client' id='late'><body>late drain</body></message>"
                .to_string(),
            t1,
        )
        .await
        .expect("record_outbound_for_detached_stream_at");
    assert!(recorded, "session was found and stanza recorded in-memory");

    // Critical: the durable `sm_unacked` table must contain the row
    // immediately, NOT only after a subsequent `store_session_atomic`.
    let unacked = persistence
        .list_unacked(&waddle_xmpp::pending_delivery::SmSessionId::new(
            "stream-late-drain",
        ))
        .await
        .expect("list_unacked");
    assert_eq!(
        unacked.len(),
        1,
        "late-drain stanza must be persisted at append time, not only at next store_session"
    );
    assert_eq!(unacked[0].sequence, 42);
    assert_eq!(unacked[0].original_receipt_at, t1);
}
