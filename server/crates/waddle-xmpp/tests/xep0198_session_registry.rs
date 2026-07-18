//! XEP-0198: Stream Management detached session registry suite.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jid::{BareJid, FullJid, Jid};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    InMemorySmPersistence, PersistedSession, PersistedUnackedStanza, SmPersistenceError,
    SmPersistenceStorage, SmUnackedStanzaPurpose,
};
use waddle_xmpp::stream_management::{
    DetachedSession, DetachedSessionSnapshot, InMemorySmSessionRegistry, SmClaimCompletion,
    SmSessionRegistry, StreamManagementState,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

#[test]
fn live_recording_preserves_explicit_resume_barrier_purpose() {
    let mut state = StreamManagementState::new();
    state.enable("typed-purpose".to_string(), true, Some(300));
    let barrier = xmpp_parsers::iq::Iq::Get {
        from: None,
        to: None,
        id: "resume-barrier".to_string(),
        payload: minidom::Element::builder("ping", waddle_xmpp::xep::xep0199::NS_PING).build(),
    };
    let barrier_xml =
        waddle_xmpp::parser::stanza_to_string(barrier).expect("serialize typed resume-barrier IQ");

    let _ = state.record_outbound(barrier_xml, SmUnackedStanzaPurpose::ResumeBarrier);

    let detached = state
        .to_detached_session(DetachedSessionSnapshot {
            user_id: "alice".to_string(),
            jid: "alice@example.com/phone".parse().expect("full jid"),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .expect("enabled resumable state detaches");
    assert_eq!(detached.unacked_stanzas.len(), 1);
    assert_eq!(
        detached.unacked_stanzas[0].purpose,
        SmUnackedStanzaPurpose::ResumeBarrier
    );
}

fn detached_session(stream_id: &str, jid: &str) -> DetachedSession {
    DetachedSession {
        stream_id: stream_id.to_string(),
        user_id: jid.to_string(),
        jid: jid.parse().expect("valid full jid"),
        inbound_count: 7,
        outbound_count: 11,
        last_acked: 10,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: Instant::now(),
        carbons_enabled: true,
        roster_interested: true,
        blocklist_interested: false,
        presence_available: true,
        presence_show: None,
        presence_status: Some("available".to_string()),
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

fn expiring_detached_session(stream_id: &str, jid: &str) -> DetachedSession {
    let mut session = detached_session(stream_id, jid);
    session.max_resume_time = Some(1);
    session
}

fn chat_stanza(to: &FullJid, body: &str) -> Stanza {
    let mut message = Message::new(Some(Jid::from(to.clone())));
    message.id = Some(xmpp_parsers::message::Id(format!("msg-{}", body.len())));
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    Stanza::Message(message)
}

struct BlockingFirstAtomicStore {
    inner: InMemorySmPersistence,
    first_store_seen: AtomicBool,
    first_store_started: tokio::sync::Notify,
    allow_first_store: tokio::sync::Notify,
}

impl BlockingFirstAtomicStore {
    fn new() -> Self {
        Self {
            inner: InMemorySmPersistence::new(),
            first_store_seen: AtomicBool::new(false),
            first_store_started: tokio::sync::Notify::new(),
            allow_first_store: tokio::sync::Notify::new(),
        }
    }

    async fn wait_for_first_store(&self) {
        self.first_store_started.notified().await;
    }

    fn release_first_store(&self) {
        self.allow_first_store.notify_one();
    }
}

#[async_trait]
impl SmPersistenceStorage for BlockingFirstAtomicStore {
    async fn upsert_session(&self, session: PersistedSession) -> Result<(), SmPersistenceError> {
        self.inner.upsert_session(session).await
    }

    async fn get_session(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<PersistedSession>, SmPersistenceError> {
        self.inner.get_session(stream_id).await
    }

    async fn delete_session(&self, stream_id: &SmSessionId) -> Result<(), SmPersistenceError> {
        self.inner.delete_session(stream_id).await
    }

    async fn append_unacked(
        &self,
        stanza: PersistedUnackedStanza,
    ) -> Result<(), SmPersistenceError> {
        self.inner.append_unacked(stanza).await
    }

    async fn ack_through(
        &self,
        stream_id: &SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, SmPersistenceError> {
        self.inner.ack_through(stream_id, up_to_sequence).await
    }

    async fn delete_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, SmPersistenceError> {
        self.inner.delete_unacked(stream_id, sequences).await
    }

    async fn list_unacked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError> {
        self.inner.list_unacked(stream_id).await
    }

    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        self.inner.list_expired_sessions(now).await
    }

    async fn list_all_sessions(&self) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        self.inner.list_all_sessions().await
    }

    async fn store_session_atomic(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        if !self.first_store_seen.swap(true, Ordering::SeqCst) {
            self.first_store_started.notify_waiters();
            self.allow_first_store.notified().await;
        }

        self.inner.delete_session(&session.stream_id).await?;
        self.inner.upsert_session(session).await?;
        for entry in unacked {
            self.inner.append_unacked(entry).await?;
        }
        Ok(())
    }
}

/// Storage double that mislabels rows: its
/// `list_all_sessions_with_unacked` attaches a foreign-stream
/// unacked stanza to every session's queue. Models the class of
/// grouping bug fixed in #1157 arising again in any backend, so the
/// registry-side defense can be exercised through the public
/// restore path.
struct MislabelingStore {
    inner: InMemorySmPersistence,
}

#[async_trait]
impl SmPersistenceStorage for MislabelingStore {
    async fn upsert_session(&self, session: PersistedSession) -> Result<(), SmPersistenceError> {
        self.inner.upsert_session(session).await
    }

    async fn get_session(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<PersistedSession>, SmPersistenceError> {
        self.inner.get_session(stream_id).await
    }

    async fn delete_session(&self, stream_id: &SmSessionId) -> Result<(), SmPersistenceError> {
        self.inner.delete_session(stream_id).await
    }

    async fn append_unacked(
        &self,
        stanza: PersistedUnackedStanza,
    ) -> Result<(), SmPersistenceError> {
        self.inner.append_unacked(stanza).await
    }

    async fn ack_through(
        &self,
        stream_id: &SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, SmPersistenceError> {
        self.inner.ack_through(stream_id, up_to_sequence).await
    }

    async fn delete_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, SmPersistenceError> {
        self.inner.delete_unacked(stream_id, sequences).await
    }

    async fn list_unacked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError> {
        self.inner.list_unacked(stream_id).await
    }

    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        self.inner.list_expired_sessions(now).await
    }

    async fn list_all_sessions(&self) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        self.inner.list_all_sessions().await
    }

    async fn list_all_sessions_with_unacked(
        &self,
    ) -> Result<Vec<(PersistedSession, Vec<PersistedUnackedStanza>)>, SmPersistenceError> {
        let mut groups = self.inner.list_all_sessions_with_unacked().await?;
        for (_, unacked) in &mut groups {
            let to: FullJid = "mallory@example.test/phone".parse().expect("valid jid");
            unacked.push(PersistedUnackedStanza {
                stream_id: SmSessionId::new("attacker-stream"),
                sequence: 99,
                stanza: Box::new(chat_stanza(&to, "leaked secret")),
                original_receipt_at: chrono::Utc::now(),
                purpose: SmUnackedStanzaPurpose::Application,
            });
        }
        Ok(groups)
    }
}

/// Issue #1157 defense in depth: hydration MUST verify each unacked
/// row's own `stream_id` against the session being restored and drop
/// mismatched rows, so a grouping bug anywhere in a storage backend
/// can never replay one user's stanzas on another user's
/// `<resumed/>` (XEP-0198 §5 retransmission is per-stream).
#[tokio::test]
async fn xep0198_restore_drops_unacked_rows_labeled_with_foreign_stream_id() {
    let persistence: Arc<dyn SmPersistenceStorage> = Arc::new(MislabelingStore {
        inner: InMemorySmPersistence::new(),
    });
    let registry = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    let mut session = detached_session("victim-stream", "alice@example.test/laptop");
    session
        .unacked_stanzas
        .push(waddle_xmpp::stream_management::DetachedUnackedStanza {
            sequence: 12,
            stanza_xml:
                "<message xmlns='jabber:client' id='m12'><body>alice's own</body></message>"
                    .to_string(),
            original_receipt_at: chrono::Utc::now(),
            purpose: SmUnackedStanzaPurpose::Application,
        });
    registry.store_session(session).await.expect("store");

    // Simulate restart: restore through the mislabeling storage.
    drop(registry);
    let restored = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    restored.restore_from_persistence().await.expect("restore");

    let claimed = restored
        .claim_session("victim-stream")
        .await
        .expect("claim restored")
        .expect("present");
    assert_eq!(
        claimed.unacked_stanzas.len(),
        1,
        "the foreign-stream row must be dropped, not queued for replay"
    );
    assert_eq!(claimed.unacked_stanzas[0].sequence, 12);
    assert!(
        claimed.unacked_stanzas[0]
            .stanza_xml
            .contains("alice's own"),
        "only the session's own stanza survives hydration"
    );
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
async fn xep0198_initial_store_does_not_overwrite_concurrent_detached_append() {
    let persistence = Arc::new(BlockingFirstAtomicStore::new());
    let storage: Arc<dyn SmPersistenceStorage> = persistence.clone();
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage));
    let jid: FullJid = "alice@example.test/race".parse().expect("valid jid");
    let mut session = detached_session("stream-race-store", jid.as_str());
    session.outbound_count = 0;
    session.last_acked = 0;

    let store_registry = Arc::clone(&registry);
    let store_task = tokio::spawn(async move { store_registry.store_session(session).await });

    tokio::time::timeout(Duration::from_secs(2), persistence.wait_for_first_store())
        .await
        .expect("first durable store started");

    let append_registry = Arc::clone(&registry);
    let append_jid = jid.clone();
    let append_task = tokio::spawn(async move {
        append_registry
            .record_stanza_for_detached_bound_resource(
                &append_jid,
                &chat_stanza(&append_jid, "during-store"),
                chrono::Utc::now(),
            )
            .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    persistence.release_first_store();

    tokio::time::timeout(Duration::from_secs(2), store_task)
        .await
        .expect("store task completed")
        .expect("store task joined")
        .expect("store session");
    let recorded = tokio::time::timeout(Duration::from_secs(2), append_task)
        .await
        .expect("append task completed")
        .expect("append task joined")
        .expect("append result");
    assert!(recorded, "append should find the newly detached session");

    let persisted = persistence
        .list_unacked(&SmSessionId::new("stream-race-store"))
        .await
        .expect("list unacked");
    assert_eq!(
        persisted.len(),
        1,
        "initial store must not erase a detached append persisted during detach handoff"
    );
    assert!(matches!(&*persisted[0].stanza, Stanza::Message(_)));
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
        .detached_carbon_resources_for_user(&bare, std::slice::from_ref(&jid))
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
        .detached_carbon_resources_for_user(&alice, std::slice::from_ref(&phone))
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

/// #1206 (follow-up to #1101/#1103): the SM durable shape must carry a
/// resource's own presence extension payloads (XEP-0115 `<c/>`, XEP-0319
/// `<idle/>`, arbitrary extensions) across a restart / cross-node
/// rehydration. Without this, a session rebuilt from durable storage comes
/// back caps-less, and — because XEP-0198 resume means the client does NOT
/// resend presence — every subsequent probe response relays the resource as
/// available with no `<c/>` for the rest of the session, breaking feature
/// detection toward its subscribers (RFC 6121 §4.3.2 requires the probe
/// response to reproduce the complete presence stanza).
#[tokio::test]
async fn xep0198_persistence_round_trip_preserves_presence_payloads() {
    use std::sync::Arc;
    use waddle_xmpp::stream_management::persistence::InMemorySmPersistence;
    use xmpp_parsers::minidom::Element;

    let caps: Element = r#"<c xmlns='http://jabber.org/protocol/caps' hash='sha-1' node='https://example.com/client' ver='zHyEOgxTrkpSdGcQKH8EFPLsriY='/>"#
        .parse()
        .expect("valid XEP-0115 caps element");
    let idle: Element = r#"<idle xmlns='urn:xmpp:idle:1' since='2026-07-08T10:00:00+00:00'/>"#
        .parse()
        .expect("valid XEP-0319 idle element");

    let persistence: Arc<dyn SmPersistenceStorage> = Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));

    let mut session = detached_session("stream-payloads", "alice@example.com/laptop");
    session.presence_payloads = vec![caps.clone(), idle.clone()];
    registry.store_session(session).await.expect("store");

    // Simulate restart: drop the registry, build a fresh one over the same
    // persistence handle, restore from durable storage.
    drop(registry);
    let restored = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    let count = restored
        .restore_from_persistence()
        .await
        .expect("restore from persistence");
    assert_eq!(count, 1, "one session restored");

    // AC2: a probe of the still-detached available resource carries its
    // stored payloads. `detached_presence_state` is the exact source the
    // probe / subscription-delivery paths read from.
    let jid: FullJid = "alice@example.com/laptop".parse().unwrap();
    let state = restored
        .detached_presence_state(&jid)
        .await
        .expect("query detached presence state")
        .expect("detached available resource present after restore");
    assert_eq!(
        state.payloads,
        vec![caps, idle],
        "durable rehydration must preserve the resource's own presence payloads \
         verbatim and in order"
    );
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
        purpose: SmUnackedStanzaPurpose::Application,
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

/// Build a typed `<message/>` stanza and return its on-the-wire XML.
///
/// AGENTS.md PR Compliance ID 14: tests must construct XMPP XML via
/// structured builders (`xmpp_parsers` / `minidom`) rather than raw
/// string literals — string XML is brittle to escaping bugs and
/// drifts from the production serialization path. `message_to_string`
/// is the same helper the production write loop uses, so this test
/// exercises the same wire shape.
fn late_drain_message_xml(id: &str, body: &str) -> String {
    let mut message = Message::new(None);
    message.id = Some(xmpp_parsers::message::Id(id.to_string()));
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    waddle_xmpp::parser::message_to_string(&message).expect("serialize <message/>")
}

#[test]
fn xep0198_same_sequence_identity_includes_payload_time_and_purpose() {
    let mut session = detached_session("stream-exact-replay", "alice@example.com/laptop");
    let receipt = chrono::Utc::now();
    let xml = late_drain_message_xml("same", "same");

    session
        .record_detached_outbound_at(
            12,
            xml.clone(),
            receipt,
            SmUnackedStanzaPurpose::Application,
        )
        .expect("first insert");
    session
        .record_detached_outbound_at(
            12,
            xml.clone(),
            receipt,
            SmUnackedStanzaPurpose::Application,
        )
        .expect("exact retry is idempotent");
    assert_eq!(session.unacked_stanzas.len(), 1);

    assert!(session
        .record_detached_outbound_at(
            12,
            late_drain_message_xml("different", "different"),
            receipt,
            SmUnackedStanzaPurpose::Application,
        )
        .is_err());
    assert!(session
        .record_detached_outbound_at(
            12,
            xml.clone(),
            receipt + chrono::Duration::seconds(1),
            SmUnackedStanzaPurpose::Application,
        )
        .is_err());

    session.unacked_stanzas[0].purpose = SmUnackedStanzaPurpose::ResumeBarrier;
    assert!(session
        .record_detached_outbound_at(12, xml, receipt, SmUnackedStanzaPurpose::Application)
        .is_err());
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
            late_drain_message_xml("late", "late drain"),
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

/// Presence replay for available detached resources is also part of the
/// XEP-0198 unacked outbound queue. It must be mirrored durably at append
/// time, not only in the in-memory detached snapshot.
#[tokio::test]
async fn xep0198_record_available_resource_presence_persists_durably() {
    use std::sync::Arc;
    use waddle_xmpp::stream_management::persistence::InMemorySmPersistence;

    let persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    let jid: FullJid = "alice@example.com/phone".parse().expect("valid jid");
    registry
        .store_session(detached_session("stream-available", jid.as_str()))
        .await
        .expect("store");

    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.statuses.insert(
        xmpp_parsers::message::Lang(String::new()),
        "still-online".to_string(),
    );
    let t1 = chrono::Utc::now();

    let recorded = registry
        .record_stanza_for_detached_available_resource(&jid, &Stanza::Presence(presence), t1)
        .await
        .expect("record available-resource presence");
    assert!(recorded, "available detached resource was found");

    let unacked = persistence
        .list_unacked(&waddle_xmpp::pending_delivery::SmSessionId::new(
            "stream-available",
        ))
        .await
        .expect("list_unacked");
    assert_eq!(
        unacked.len(),
        1,
        "available-resource presence replay must be persisted at append time"
    );
    assert_eq!(unacked[0].sequence, 12);
    assert_eq!(unacked[0].original_receipt_at, t1);
    assert!(matches!(&*unacked[0].stanza, Stanza::Presence(_)));
}

/// Qodo finding on PR #409: `record_outbound_for_detached_stream_at`
/// MUST NOT durably persist an unacked row when the named session is
/// unknown or expired. The earlier "persist first" ordering left
/// orphan rows in the `sm_unacked` table that no `delete_session`
/// reaper would ever clean up (the in-memory persistence backend
/// happily accepts appends for any stream_id).
#[tokio::test]
async fn xep0198_record_outbound_for_unknown_stream_does_not_persist_orphan_row() {
    use std::sync::Arc;
    use waddle_xmpp::stream_management::persistence::InMemorySmPersistence;

    let persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));

    let recorded = registry
        .record_outbound_for_detached_stream_at(
            "stream-does-not-exist",
            7,
            late_drain_message_xml("orphan", "orphan"),
            chrono::Utc::now(),
        )
        .await
        .expect("record_outbound_for_detached_stream_at");
    assert!(!recorded, "missing session must short-circuit to Ok(false)");

    let unacked = persistence
        .list_unacked(&waddle_xmpp::pending_delivery::SmSessionId::new(
            "stream-does-not-exist",
        ))
        .await
        .expect("list_unacked");
    assert!(
        unacked.is_empty(),
        "no orphan row may be persisted for an unknown stream_id"
    );
}
