//! The expiry of a detached XEP-0198 session that holds a message from a
//! remote sender (ADR-0020 D7; gate G18; XEP-0198 §5): the message is
//! treated as one sent to an unavailable resource, as for any sender, but
//! nothing ever answers its sender. With room in the account's offline
//! store it is stored there, from a stranger as from a contact; beyond the
//! store's quota it is dropped, never bounced and never retained for a
//! bounce. A local sender's message beyond the quota is not dropped, which
//! shows the distinction is the sender's. Through the promotion chain every
//! expiry path runs (`sm_promotion::promote_session_unacked`), with an
//! in-memory offline store and no connected resource.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use jid::{BareJid, FullJid};
use kameo::actor::{ActorRef, Spawn};
use minidom::Element;
use waddle_server::sm_promotion::promote_session_unacked;
use waddle_xmpp::pending_delivery::storage::{
    InMemoryPendingDeliveryStorage, PendingDeliveryStorage,
};
use waddle_xmpp::pending_delivery::{PendingPayload, QuotaPolicy};
use waddle_xmpp::protocol::Blocklist;
use waddle_xmpp::registry::{ConnectionRegistry, UserRegistryActor};
use waddle_xmpp::stream_management::{
    DetachedSession, DetachedUnackedStanza, PromotionCaptureCustody,
};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

const DOMAIN: &str = "example.com";
const ALICE_PHONE: &str = "alice@example.com/phone";
const MALLORY: &str = "mallory@home-b.test/desk";

fn chat(from: &str, id: &str) -> String {
    let mut message =
        Message::new_with_type(MessageType::Chat, Some(ALICE_PHONE.parse().expect("a JID")));
    message.from = Some(from.parse().expect("a JID"));
    message.id = Some(Id(id.to_owned()));
    message.bodies.insert(Lang::new(), "hello".to_owned());
    String::from(&Element::from(message))
}

/// Alice's phone, detached with `frames` unacknowledged, its resume window
/// over.
fn expired(frames: Vec<String>) -> DetachedSession {
    let phone: FullJid = ALICE_PHONE.parse().expect("a JID");
    DetachedSession {
        promotion_custody: PromotionCaptureCustody::never_claimed(),
        stream_id: "alice-phone-stream".to_owned(),
        user_id: phone.to_bare().to_string(),
        jid: phone,
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: u32::try_from(frames.len()).expect("a few frames"),
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: frames
            .into_iter()
            .zip(1u32..)
            .map(|(stanza_xml, sequence)| DetachedUnackedStanza {
                provenance: Default::default(),
                ingress_receipts: Vec::new(),
                sequence,
                stanza_xml,
                original_receipt_at: Utc::now(),
            })
            .collect(),
        max_resume_time: Some(0),
        detached_at: Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: true,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

fn registry() -> (ConnectionRegistry, ActorRef<UserRegistryActor>) {
    (
        ConnectionRegistry::new(),
        UserRegistryActor::spawn(UserRegistryActor::new()),
    )
}

fn alice() -> BareJid {
    "alice@example.com".parse().expect("a JID")
}

#[tokio::test]
async fn a_strangers_message_held_by_an_expired_session_is_stored_offline_unanswered() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let (connections, users) = registry();

    let summary = promote_session_unacked(
        &expired(vec![chat(MALLORY, "held")]),
        &connections,
        &users,
        &storage,
        &Blocklist::empty(),
        DOMAIN,
        &[],
    )
    .await;

    assert_eq!(summary.queued, 1, "{summary:?}");
    assert_eq!(summary.bounced, 0, "{summary:?}");
    assert_eq!(summary.storage_failed, 0, "{summary:?}");
    let stored = storage.list(&alice()).await.expect("the store is readable");
    assert_eq!(stored.len(), 1);
    let PendingPayload::Transient(message) = &stored[0].payload else {
        panic!("a message stored inline: {:?}", stored[0].payload);
    };
    assert_eq!(message.id.as_ref().map(|id| id.0.as_str()), Some("held"));
    // Nothing was stored for anybody else: no bounce went anywhere.
    let mallory: BareJid = "mallory@home-b.test".parse().expect("a JID");
    assert_eq!(storage.count(&mallory).await.expect("readable"), 0);
}

#[tokio::test]
async fn beyond_the_offline_quota_a_strangers_message_is_dropped_never_bounced_or_retained() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::new(QuotaPolicy::CountCap {
            max_rows: 0,
        }));
    let (connections, users) = registry();

    let summary = promote_session_unacked(
        &expired(vec![chat(MALLORY, "over"), chat(MALLORY, "over-too")]),
        &connections,
        &users,
        &storage,
        &Blocklist::empty(),
        DOMAIN,
        &[],
    )
    .await;

    // Both are handled for good, so the session's custody is released and
    // nothing waits for a bounce that would tell the sender anything.
    assert_eq!(summary.dropped, 2, "{summary:?}");
    assert_eq!(summary.bounced, 0, "{summary:?}");
    assert_eq!(summary.storage_failed, 0, "{summary:?}");
    assert!(!summary.has_storage_failure());
    assert_eq!(storage.count(&alice()).await.expect("readable"), 0);
}

#[tokio::test]
async fn beyond_the_offline_quota_a_local_senders_message_is_not_dropped() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::new(QuotaPolicy::CountCap {
            max_rows: 0,
        }));
    let (connections, users) = registry();

    let summary = promote_session_unacked(
        &expired(vec![chat("bob@example.com/laptop", "local")]),
        &connections,
        &users,
        &storage,
        &Blocklist::empty(),
        DOMAIN,
        &[],
    )
    .await;

    // A local sender is answered or its message retained for the next
    // attempt, as before.
    assert_eq!(summary.dropped, 0, "{summary:?}");
    assert!(
        summary.bounced == 1 || summary.storage_failed == 1,
        "{summary:?}"
    );
}
