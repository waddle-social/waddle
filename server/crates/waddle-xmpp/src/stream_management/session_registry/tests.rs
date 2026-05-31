use super::*;
use std::time::{Duration, Instant};

use chrono::Utc;
use jid::FullJid;
use xmpp_parsers::presence::Show;

use crate::Stanza;

fn make_test_jid() -> FullJid {
    "user@example.com/resource".parse().unwrap()
}

fn message_stanza_xml_with_id(id: String) -> String {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.id = Some(xmpp_parsers::message::Id(id));
    let element = Stanza::Message(message).to_element();
    let mut buffer = Vec::new();
    element.write_to(&mut buffer).expect("serialize message");
    String::from_utf8(buffer).expect("message stanza xml is utf-8")
}

fn make_test_session(stream_id: &str) -> DetachedSession {
    make_test_session_for_jid(stream_id, make_test_jid())
}

fn make_test_session_for_jid(stream_id: &str, jid: FullJid) -> DetachedSession {
    DetachedSession {
        stream_id: stream_id.to_string(),
        user_id: "user@example.com".to_string(),
        jid,
        inbound_count: 10,
        outbound_count: 15,
        last_acked: 12,
        replay_gap_through: None,
        unacked_stanzas: vec![
            DetachedUnackedStanza {
                sequence: 13,
                stanza_xml: "<msg1/>".to_string(),
                original_receipt_at: Utc::now(),
            },
            DetachedUnackedStanza {
                sequence: 14,
                stanza_xml: "<msg2/>".to_string(),
                original_receipt_at: Utc::now(),
            },
            DetachedUnackedStanza {
                sequence: 15,
                stanza_xml: "<msg3/>".to_string(),
                original_receipt_at: Utc::now(),
            },
        ],
        max_resume_time: Some(300),
        detached_at: Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
    }
}

fn make_test_session_with_unacked(stream_id: &str, unacked: Vec<(u32, String)>) -> DetachedSession {
    let now = Utc::now();
    let mut s = make_test_session(stream_id);
    s.unacked_stanzas = unacked
        .into_iter()
        .map(|(sequence, stanza_xml)| DetachedUnackedStanza {
            sequence,
            stanza_xml,
            original_receipt_at: now,
        })
        .collect();
    s
}

#[test]
fn stream_locks_are_fixed_shards_not_per_stream_entries() {
    let registry = InMemorySmSessionRegistry::new();
    let shard_count = registry.stream_locks.len();

    assert!(
        shard_count > 0,
        "registry must have at least one lock shard"
    );

    for index in 0..(shard_count * 4) {
        let _lock = registry
            .stream_lock(&format!("historical-stream-{index}"))
            .expect("stream lock");
    }

    assert_eq!(
        registry.stream_locks.len(),
        shard_count,
        "unique SM stream ids must not grow an unbounded lock map"
    );
}

#[test]
fn detached_session_overflow_blocks_resume_for_older_client_h() {
    let mut session = make_test_session_with_unacked("stream-overflow", Vec::new());
    session.outbound_count = 0;
    session.last_acked = 0;

    for sequence in 1..=(crate::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE as u32 + 1) {
        session.record_detached_outbound_at(
            sequence,
            message_stanza_xml_with_id(format!("m{sequence}")),
            Utc::now(),
        );
    }

    assert_eq!(session.replay_gap_through, Some(1));
    assert!(
        !session.can_resume_from(0),
        "resume must fail when the client still needs an evicted detached stanza"
    );
    assert!(
        session.can_resume_from(1),
        "resume can proceed once the client's h covers the evicted sequence"
    );
}

#[tokio::test]
async fn xep_0198_scrub_for_tombstone_removes_matching_1on1_message() {
    // XEP-0424 §"prevent further distribution" + XEP-0198 resume
    // safety: when a tombstone is applied, the original
    // `<message id='target'>` must not replay on a recipient's
    // resume. Locks the matcher against false negatives (matching
    // messages must be removed) and false positives (non-matching
    // messages and non-message frames must be preserved). Scoped
    // by the recipient's bare JID so the matcher cannot reach
    // outside the conversation.
    let registry = InMemorySmSessionRegistry::new();
    let session = make_test_session_with_unacked(
            "stream-tomb",
            vec![
                (
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='target' type='chat'><body>secret</body><thread parent='root'>child</thread></message>"
                        .to_string(),
                ),
                (
                    2,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='other' type='chat'><body>safe</body></message>"
                        .to_string(),
                ),
                (3, "<presence/>".to_string()),
                (4, "<iq type='result' id='not-a-message'/>".to_string()),
            ],
        );
    registry.store_session(session).await.unwrap();

    let removed = registry
        .scrub_unacked_for_tombstone("target", "user@example.com")
        .await
        .unwrap();
    assert_eq!(removed, 1, "exactly one matching message should be removed");

    let again = registry
        .peek_session("stream-tomb")
        .await
        .unwrap()
        .expect("session still present");
    assert_eq!(again.unacked_stanzas.len(), 3);
    assert!(
        !again
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("id='target'")),
        "scrubbed message must not appear in queue"
    );
    assert!(
        again
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("id='other'")),
        "non-matching message must remain"
    );
    assert!(
        again
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("<presence")),
        "presence frame must remain (not a message)"
    );
    assert!(
        again
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("<iq")),
        "iq frame must remain (not a message)"
    );
}

#[tokio::test]
async fn xep_0198_detached_replay_preserves_xep_0201_thread_metadata() {
    use xmpp_parsers::message::{Message, MessageType, Thread};

    let registry = InMemorySmSessionRegistry::new();
    let jid = make_test_jid();
    let session = make_test_session_for_jid("stream-threaded-replay", jid.clone());
    registry.store_session(session).await.unwrap();

    let mut msg = Message::new(Some(jid::Jid::from(jid.clone())));
    msg.from = Some(jid::Jid::from(
        "sender@example.com/web".parse::<FullJid>().expect("jid"),
    ));
    msg.id = Some(xmpp_parsers::message::Id(
        "detached-threaded-message".to_string(),
    ));
    msg.type_ = MessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "threaded".to_string());
    msg.thread = Some(Thread {
        id: "conversation-thread".to_string(),
        parent: None,
    });
    msg.payloads.push(
        minidom::Element::builder("thread", "urn:example:other:0")
            .attr(
                <minidom::rxml::NcName as std::convert::TryFrom<&str>>::try_from("kind")
                    .expect("validated NcName"),
                "extension",
            )
            .append("not-xep-0201")
            .build(),
    );

    assert!(registry
        .record_stanza_for_detached_bound_resource(&jid, &Stanza::Message(msg), Utc::now())
        .await
        .unwrap());
    let stored = registry
        .peek_session("stream-threaded-replay")
        .await
        .unwrap()
        .expect("detached session remains");
    let replay = stored
        .unacked_stanzas
        .last()
        .map(|entry| &entry.stanza_xml)
        .expect("recorded replay stanza");
    let element = replay
        .parse::<minidom::Element>()
        .expect("valid stanza xml");

    assert!(element.children().any(|child| {
        child.name() == "thread"
            && child.ns() == "jabber:client"
            && child.text() == "conversation-thread"
    }));
    assert!(element.children().any(|child| {
        child.name() == "thread"
            && child.ns() == "urn:example:other:0"
            && child.text() == "not-xep-0201"
    }));
}

#[tokio::test]
async fn xep_0198_scrub_for_tombstone_matches_groupchat_stanza_id() {
    // Groupchat retractions key off the room's XEP-0359 stanza-id
    // per the "archive id == wire stanza-id" invariant
    // (`archive_groupchat_message`). The cached reflection
    // preserves the sender's original `message.id` AND carries
    // `<stanza-id by='room' id='canonical'/>`; the retraction
    // request targets `canonical`, not the sender's id. The
    // matcher must therefore check stanza-id children too —
    // surfaced by Copilot review on PR #305.
    let registry = InMemorySmSessionRegistry::new();
    let session = make_test_session_with_unacked(
            "stream-muc",
            vec![(
                1,
                "<message xmlns='jabber:client' from='room@conf.example.com/alice' to='user@example.com/resource' id='sender-wire-id' type='groupchat'><body>moderated</body><stanza-id xmlns='urn:xmpp:sid:0' by='room@conf.example.com' id='canonical-archive-id'/></message>"
                    .to_string(),
            )],
        );
    registry.store_session(session).await.unwrap();

    let removed = registry
        .scrub_unacked_for_tombstone("canonical-archive-id", "room@conf.example.com")
        .await
        .unwrap();
    assert_eq!(
        removed, 1,
        "groupchat tombstone keyed by stanza-id must scrub the reflection"
    );
}

#[tokio::test]
async fn xep_0198_scrub_for_tombstone_does_not_cross_conversations() {
    // Two clients independently use `id='msg-1'` in different
    // conversations. Retracting in conversation A must not delete
    // the queued message in conversation B that happens to share
    // the same wire id. Codex P1 review on PR #305.
    let registry = InMemorySmSessionRegistry::new();
    let session = make_test_session_with_unacked(
            "stream-cross",
            vec![
                (
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='msg-1' type='chat'><body>conv-A</body></message>"
                        .to_string(),
                ),
                (
                    2,
                    "<message xmlns='jabber:client' from='carol@elsewhere.com/web' to='user@example.com/resource' id='msg-1' type='chat'><body>conv-B</body></message>"
                        .to_string(),
                ),
            ],
        );
    registry.store_session(session).await.unwrap();

    // Tombstone is scoped to alice@example.com (the sender of
    // conversation A's archive context). The matcher must NOT
    // remove the carol→user message even though it shares the
    // wire id, because alice is neither its `from` nor `to`.
    let removed = registry
        .scrub_unacked_for_tombstone("msg-1", "alice@example.com")
        .await
        .unwrap();
    assert_eq!(
        removed, 1,
        "only the alice-scoped message should be removed"
    );

    let again = registry
        .peek_session("stream-cross")
        .await
        .unwrap()
        .expect("session still present");
    assert!(
        again
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("conv-B")),
        "conversation B's message must survive — different scope"
    );
}

#[tokio::test]
async fn xep_0198_scrub_for_tombstone_ignores_non_xep0359_stanza_id_namespace() {
    // XEP-0359 §3 scopes `<stanza-id/>` to `urn:xmpp:sid:0`. An
    // unrelated extension element that happens to be named
    // "stanza-id" in a different namespace must NOT trigger a
    // tombstone scrub (Copilot review on PR #305).
    let registry = InMemorySmSessionRegistry::new();
    let session = make_test_session_with_unacked(
            "stream-ns",
            vec![(
                1,
                "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='wire-id' type='chat'><body>safe</body><stanza-id xmlns='urn:example:other:0' id='target'/></message>"
                    .to_string(),
            )],
        );
    registry.store_session(session).await.unwrap();

    let removed = registry
        .scrub_unacked_for_tombstone("target", "user@example.com")
        .await
        .unwrap();
    assert_eq!(
        removed, 0,
        "stanza-id in non-XEP-0359 namespace must not be matched"
    );
}

#[tokio::test]
async fn xep_0198_scrub_for_tombstone_handles_no_match() {
    let registry = InMemorySmSessionRegistry::new();
    registry
            .store_session(make_test_session_with_unacked(
                "stream-nomatch",
                vec![(
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com' id='other' type='chat'><body>x</body></message>"
                        .to_string(),
                )],
            ))
            .await
            .unwrap();
    let removed = registry
        .scrub_unacked_for_tombstone("not-here", "user@example.com")
        .await
        .unwrap();
    assert_eq!(removed, 0);
}

#[tokio::test]
async fn test_store_and_take_session() {
    let registry = InMemorySmSessionRegistry::new();

    let session = make_test_session("stream-123");
    registry.store_session(session).await.unwrap();

    assert_eq!(registry.session_count().await, 1);

    // Take the session
    let retrieved = registry.take_session("stream-123").await.unwrap();
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.stream_id, "stream-123");
    assert_eq!(retrieved.outbound_count, 15);

    // Session should be gone now
    assert_eq!(registry.session_count().await, 0);
    let again = registry.take_session("stream-123").await.unwrap();
    assert!(again.is_none());
}

#[tokio::test]
async fn test_store_session_replaces_existing_session_for_same_full_jid() {
    let registry = InMemorySmSessionRegistry::new();
    let mut first = make_test_session("stream-old");
    first.roster_interested = true;
    let mut second = make_test_session("stream-new");
    second.roster_interested = true;

    registry.store_session(first).await.unwrap();
    registry.store_session(second).await.unwrap();

    assert!(registry.take_session("stream-old").await.unwrap().is_none());
    let current = registry
        .take_session("stream-new")
        .await
        .unwrap()
        .expect("newer detached session should remain");
    assert_eq!(current.stream_id, "stream-new");
}

#[tokio::test]
async fn test_peek_session() {
    let registry = InMemorySmSessionRegistry::new();

    let session = make_test_session("stream-456");
    registry.store_session(session).await.unwrap();

    // Peek should not remove
    let peeked = registry.peek_session("stream-456").await.unwrap();
    assert!(peeked.is_some());
    assert_eq!(registry.session_count().await, 1);

    // Peek again
    let peeked2 = registry.peek_session("stream-456").await.unwrap();
    assert!(peeked2.is_some());
}

#[tokio::test]
async fn test_claimed_session_remains_writable_for_handoff_fanout() {
    let registry = InMemorySmSessionRegistry::new();

    let mut session = make_test_session("stream-claimed");
    session.roster_interested = true;
    let jid = session.jid.clone();
    registry.store_session(session).await.unwrap();

    let claimed = registry
        .claim_session("stream-claimed")
        .await
        .unwrap()
        .expect("claim");
    assert_eq!(claimed.stream_id, "stream-claimed");
    assert_eq!(
        registry.session_count().await,
        0,
        "claimed sessions must move out of the normal detached map"
    );

    assert!(
        registry
            .record_stanza_for_detached_resource(
                &jid,
                &{
                    let mut presence =
                        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
                    presence.statuses.insert(
                        xmpp_parsers::message::Lang(String::new()),
                        "during-claim".to_string(),
                    );
                    Stanza::Presence(presence)
                },
                Utc::now(),
            )
            .await
            .unwrap(),
        "fanout during resume handoff must write to the claimed session"
    );

    let completed = registry
        .complete_claim("stream-claimed")
        .await
        .unwrap()
        .expect("completed claim");
    match completed {
        SmClaimCompletion::Resumed(completed) => {
            assert!(
                completed
                    .unacked_stanzas
                    .iter()
                    .any(|entry| entry.stanza_xml.contains("during-claim")),
                "completed claim must include fanout recorded during handoff"
            );
        }
        SmClaimCompletion::Expired(_) => panic!("claim should still be resumable"),
        SmClaimCompletion::ReplayWindowTruncated(_) => {
            panic!("claim should still have a complete replay window")
        }
    }
}

#[tokio::test]
async fn blocklist_interested_detached_resources_include_claimed_sessions_and_record_pushes() {
    let registry = InMemorySmSessionRegistry::new();

    let mut stored = make_test_session_for_jid(
        "stream-blocklist-stored",
        "user@example.com/web".parse().unwrap(),
    );
    stored.blocklist_interested = true;
    let mut claimed = make_test_session_for_jid(
        "stream-blocklist-claimed",
        "user@example.com/phone".parse().unwrap(),
    );
    claimed.blocklist_interested = true;
    let claimed_jid = claimed.jid.clone();

    registry.store_session(stored).await.unwrap();
    registry.store_session(claimed).await.unwrap();
    registry
        .claim_session("stream-blocklist-claimed")
        .await
        .unwrap()
        .expect("claim");

    let bare: jid::BareJid = "user@example.com".parse().unwrap();
    let resources = registry
        .blocklist_interested_detached_resources_for_user(&bare)
        .await
        .unwrap();
    assert_eq!(resources.len(), 2);
    assert!(resources.contains(&"user@example.com/web".parse().unwrap()));
    assert!(resources.contains(&claimed_jid));

    let mut message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(claimed_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id("block-push-test".to_string()));
    assert!(
        registry
            .record_stanza_for_detached_blocklist_resource(
                &claimed_jid,
                &Stanza::Message(message),
                Utc::now(),
            )
            .await
            .unwrap(),
        "blocklist push should record to a claimed blocklist-interested session"
    );

    let completed = registry
        .complete_claim("stream-blocklist-claimed")
        .await
        .unwrap()
        .expect("completed claim");
    match completed {
        SmClaimCompletion::Resumed(completed) => assert!(
            completed
                .unacked_stanzas
                .iter()
                .any(|entry| entry.stanza_xml.contains("block-push-test")),
            "completed claim must include blocklist push recorded during handoff"
        ),
        SmClaimCompletion::Expired(_) => panic!("claim should still be resumable"),
        SmClaimCompletion::ReplayWindowTruncated(_) => {
            panic!("claim should still have a complete replay window")
        }
    }
}

#[tokio::test]
async fn complete_claim_releases_when_handoff_creates_replay_gap() {
    let registry = InMemorySmSessionRegistry::new();
    let mut session = make_test_session_with_unacked("stream-handoff-gap", Vec::new());
    session.outbound_count = 0;
    session.last_acked = 0;

    registry
        .store_session(session)
        .await
        .expect("store session");
    registry
        .claim_session("stream-handoff-gap")
        .await
        .expect("claim")
        .expect("session exists");

    for sequence in 1..=(crate::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE as u32 + 1) {
        registry
            .record_outbound_for_detached_stream_at(
                "stream-handoff-gap",
                sequence,
                message_stanza_xml_with_id(format!("m{sequence}")),
                Utc::now(),
            )
            .await
            .expect("record detached outbound");
    }

    let completed = registry
        .complete_claim_if_resumable("stream-handoff-gap", 0)
        .await
        .expect("complete checked claim")
        .expect("claim still exists");
    let SmClaimCompletion::ReplayWindowTruncated(truncated) = completed else {
        panic!("late replay gap must fail resume completion")
    };
    assert_eq!(truncated.replay_gap_through, Some(1));

    let restored = registry
        .peek_session("stream-handoff-gap")
        .await
        .expect("peek restored session")
        .expect("truncated claim is restored to detached pool");
    assert_eq!(restored.replay_gap_through, Some(1));
    assert!(
        !restored.can_resume_from(0),
        "restored session must continue rejecting the stale h value"
    );
}

#[tokio::test]
async fn test_session_not_found() {
    let registry = InMemorySmSessionRegistry::new();

    let result = registry.take_session("nonexistent").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_session_expired() {
    let registry = InMemorySmSessionRegistry::new();

    // Create an already-expired session
    let mut session = make_test_session("stream-expired");
    session.max_resume_time = Some(0); // 0 seconds means expired immediately

    registry.store_session(session).await.unwrap();

    // Wait a tiny bit to ensure expiration
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Should return None because expired
    let result = registry.take_session("stream-expired").await.unwrap();
    assert!(result.is_none());
    assert_eq!(registry.session_count().await, 0);
}

#[tokio::test]
async fn test_cleanup_expired() {
    let registry = InMemorySmSessionRegistry::new();

    // Store some sessions
    let mut expired = make_test_session("stream-exp1");
    expired.max_resume_time = Some(0);
    registry.store_session(expired).await.unwrap();

    let valid =
        make_test_session_for_jid("stream-valid", "user@example.com/valid".parse().unwrap());
    registry.store_session(valid).await.unwrap();

    // Wait for expiration
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Cleanup
    let removed = registry.cleanup_expired().await.unwrap();
    assert_eq!(removed, 1);
    assert_eq!(registry.session_count().await, 1);

    // Valid session should still be there
    let result = registry.take_session("stream-valid").await.unwrap();
    assert!(result.is_some());
}

#[tokio::test]
async fn test_capacity_limit() {
    let registry = InMemorySmSessionRegistry::with_capacity(3);

    // Store 3 sessions
    for i in 0..3 {
        let session = make_test_session_for_jid(
            &format!("stream-{}", i),
            format!("user@example.com/resource-{i}").parse().unwrap(),
        );
        registry.store_session(session).await.unwrap();
    }

    assert_eq!(registry.session_count().await, 3);

    // Store a 4th - should evict oldest
    let session = make_test_session_for_jid(
        "stream-new",
        "user@example.com/resource-new".parse().unwrap(),
    );
    registry.store_session(session).await.unwrap();

    assert_eq!(registry.session_count().await, 3);

    // stream-0 should be gone (oldest)
    let result = registry.take_session("stream-0").await.unwrap();
    assert!(result.is_none());

    // stream-new should be there
    let result = registry.take_session("stream-new").await.unwrap();
    assert!(result.is_some());
}

#[test]
fn test_stanzas_to_resend_count() {
    let session = make_test_session("test");

    // Client says h=12, we have 13, 14, 15 - all 3 need resending
    assert_eq!(session.stanzas_to_resend_count(12), 3);

    // Client says h=14, we have 13, 14, 15 - only 15 needs resending
    assert_eq!(session.stanzas_to_resend_count(14), 1);

    // Client says h=15, we have 13, 14, 15 - none need resending
    assert_eq!(session.stanzas_to_resend_count(15), 0);
}

#[test]
fn test_remaining_time() {
    let session = make_test_session("test");

    let remaining = session.remaining_time();
    assert!(remaining.as_secs() <= 300);
    assert!(remaining.as_secs() >= 299); // Should be close to 300
}

// --- SmPersistenceStorage integration (slice (d) phase 3) -------

use super::super::persistence::SmPersistenceStorage as _;

fn realistic_message_stanza(body: &str) -> String {
    // Build a valid XMPP message via the typed builder so the
    // persistence path can parse it back to a typed Stanza on
    // store_session. The fmt-pinned indentation is what the
    // serializer emits when rebuilt via Element::from(message).
    let mut m = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    let element: xmpp_parsers::minidom::Element = m.into();
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize message");
    String::from_utf8(buf).expect("utf8")
}

fn realistic_test_session(stream_id: &str) -> DetachedSession {
    realistic_test_session_for_jid(stream_id, make_test_jid())
}

fn realistic_test_session_for_jid(stream_id: &str, jid: FullJid) -> DetachedSession {
    DetachedSession {
        stream_id: stream_id.to_string(),
        user_id: "user@example.com".to_string(),
        jid,
        inbound_count: 4,
        outbound_count: 7,
        last_acked: 5,
        replay_gap_through: None,
        unacked_stanzas: vec![
            DetachedUnackedStanza {
                sequence: 6,
                stanza_xml: realistic_message_stanza("first"),
                original_receipt_at: Utc::now(),
            },
            DetachedUnackedStanza {
                sequence: 7,
                stanza_xml: realistic_message_stanza("second"),
                original_receipt_at: Utc::now(),
            },
        ],
        max_resume_time: Some(120),
        detached_at: Instant::now(),
        carbons_enabled: true,
        roster_interested: true,
        blocklist_interested: false,
        presence_available: true,
        presence_show: Some(Show::Chat),
        presence_status: Some("online".to_string()),
        presence_priority: 3,
    }
}

#[tokio::test]
async fn store_session_mirrors_to_persistence_when_attached() {
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let session = realistic_test_session("stream-1");
    registry.store_session(session.clone()).await.unwrap();

    let stream_id = crate::pending_delivery::SmSessionId::new("stream-1");
    let persisted = storage.get_session(&stream_id).await.unwrap().unwrap();
    assert_eq!(persisted.user_id, session.user_id);
    assert_eq!(persisted.jid, session.jid);
    assert_eq!(persisted.inbound_count, session.inbound_count);
    assert_eq!(persisted.outbound_count, session.outbound_count);
    assert_eq!(persisted.last_acked, session.last_acked);
    assert_eq!(persisted.carbons_enabled, session.carbons_enabled);
    let unacked = storage.list_unacked(&stream_id).await.unwrap();
    assert_eq!(unacked.len(), 2);
    let seqs: Vec<u32> = unacked.iter().map(|u| u.sequence).collect();
    assert_eq!(seqs, vec![6, 7]);
}

#[tokio::test]
async fn take_session_deletes_from_persistence() {
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    registry
        .store_session(realistic_test_session("stream-1"))
        .await
        .unwrap();
    // Resume — should drain durable storage.
    let _ = registry.take_session("stream-1").await.unwrap();
    let stream_id = crate::pending_delivery::SmSessionId::new("stream-1");
    assert!(storage.get_session(&stream_id).await.unwrap().is_none());
    assert!(storage.list_unacked(&stream_id).await.unwrap().is_empty());
}

#[tokio::test]
async fn restore_from_persistence_rebuilds_in_memory_view() {
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    // Pre-populate storage as if a previous server lifecycle had
    // detached two sessions for distinct users. Using distinct
    // JIDs is important: store_session evicts any prior detached
    // session with the same JID (RFC-aligned: a fresh bind for
    // a JID supersedes any older detached stream for that JID),
    // and the durable mirror also deletes the evicted row, so
    // two sessions with the same JID would resolve to one.
    {
        let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
        registry
            .store_session(realistic_test_session_for_jid(
                "stream-1",
                "alice@example.com/web".parse().unwrap(),
            ))
            .await
            .unwrap();
        registry
            .store_session(realistic_test_session_for_jid(
                "stream-2",
                "bob@example.com/laptop".parse().unwrap(),
            ))
            .await
            .unwrap();
    }
    // Simulate restart: brand-new registry, only persistence
    // attached. The in-memory view starts empty.
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    assert_eq!(registry.session_count().await, 0);

    let hydrated = registry.restore_from_persistence().await.unwrap();
    assert_eq!(hydrated, 2);
    assert_eq!(registry.session_count().await, 2);

    // Both sessions resumable post-restart.
    let resumed = registry.take_session("stream-1").await.unwrap();
    assert!(resumed.is_some());
    let resumed = resumed.unwrap();
    assert_eq!(resumed.unacked_stanzas.len(), 2);
    assert!(resumed.carbons_enabled);
    assert_eq!(resumed.presence_priority, 3);
}

#[tokio::test]
async fn restore_is_noop_when_no_persistence_attached() {
    let registry = InMemorySmSessionRegistry::new();
    assert_eq!(registry.restore_from_persistence().await.unwrap(), 0);
}

#[tokio::test]
async fn complete_claim_deletes_durable_session_on_resume() {
    // The real resume path is claim_session -> complete_claim,
    // not take_session. Without durable cleanup at the
    // complete_claim commitment point, a successful resume
    // would leave rows in storage that restart_from_persistence
    // would resurrect. (Codex P1 + Copilot review on PR #344.)
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    registry
        .store_session(realistic_test_session("stream-1"))
        .await
        .unwrap();
    let stream_id = crate::pending_delivery::SmSessionId::new("stream-1");
    assert!(storage.get_session(&stream_id).await.unwrap().is_some());

    let _claimed = registry.claim_session("stream-1").await.unwrap();
    let outcome = registry.complete_claim("stream-1").await.unwrap();
    assert!(matches!(outcome, Some(SmClaimCompletion::Resumed(_))));

    assert!(storage.get_session(&stream_id).await.unwrap().is_none());
    assert!(storage.list_unacked(&stream_id).await.unwrap().is_empty());
}

#[tokio::test]
async fn store_session_evicts_jid_collision_durably() {
    // Two store_session calls for the same JID with different
    // stream_ids: the second supersedes the first per RFC
    // resume semantics. The first's durable rows must be
    // deleted too — otherwise restart_from_persistence
    // resurrects the obsolete stream and exposes a stale
    // <resume previd='…'/> path. (Copilot review on PR #344.)
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    registry
        .store_session(realistic_test_session_for_jid(
            "stream-old",
            "alice@example.com/web".parse().unwrap(),
        ))
        .await
        .unwrap();
    registry
        .store_session(realistic_test_session_for_jid(
            "stream-new",
            "alice@example.com/web".parse().unwrap(),
        ))
        .await
        .unwrap();
    let old_id = crate::pending_delivery::SmSessionId::new("stream-old");
    let new_id = crate::pending_delivery::SmSessionId::new("stream-new");
    assert!(
        storage.get_session(&old_id).await.unwrap().is_none(),
        "evicted stream-old should be removed from durable storage"
    );
    assert!(
        storage.get_session(&new_id).await.unwrap().is_some(),
        "stream-new should remain"
    );
}

#[tokio::test]
async fn restore_skips_and_deletes_expired_sessions() {
    // Sessions whose resume window already closed during the
    // server's downtime must not be rehydrated, AND their
    // durable rows must be deleted so restart doesn't re-load
    // them next boot. (Copilot review on PR #344.)
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());

    // Manually insert an already-expired session by writing
    // directly to storage with a detached_at + duration in the
    // past.
    let now = chrono::Utc::now();
    let expired = super::super::persistence::PersistedSession {
        stream_id: crate::pending_delivery::SmSessionId::new("stream-expired"),
        user_id: "alice".to_string(),
        jid: "alice@example.com/web".parse().unwrap(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        max_resume_time: Some(60),
        detached_at: now - chrono::Duration::seconds(120),
        max_resume_duration: Duration::from_secs(60),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
    };
    storage.upsert_session(expired).await.unwrap();

    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let hydrated = registry.restore_from_persistence().await.unwrap();
    assert_eq!(hydrated, 0);
    // Durable cleanup of expired rows.
    assert!(storage
        .get_session(&crate::pending_delivery::SmSessionId::new("stream-expired"))
        .await
        .unwrap()
        .is_none());
}
