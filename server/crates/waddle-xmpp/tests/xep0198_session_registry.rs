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
        .record_stanza_for_detached_bound_resource(&jid, &chat_stanza(&jid, "handoff"))
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
    assert!(completed.unacked_stanzas[0].1.contains("handoff"));
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
        .record_stanza_for_detached_bound_resource(&jid, &chat_stanza(&jid, "retry"))
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
    assert!(restored.unacked_stanzas[0].1.contains("retry"));
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
        .record_stanza_for_detached_bound_resource(&jid, &chat_stanza(&jid, "expired"))
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
        .record_stanza_for_detached_resource(&jid, &chat_stanza(&jid, "expired"))
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
