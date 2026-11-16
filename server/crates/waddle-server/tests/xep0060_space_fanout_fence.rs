//! XEP-0060 fan-out from a Space node no host-grant scope enrolls (ADR-0020
//! D5, gate G16): a subscriber list captured before a ban or an
//! unsubscription delivers nothing to the subscriber that change removed.
//!
//! Fan-out lists a node's deliverable subscribers, builds the event and
//! enumerates each subscriber's resources before it hands anything over.
//! A ban or an unsubscription that commits in between used to be ignored:
//! the event reached the queue of a subscriber the node no longer delivered
//! to. Fan-out from a Space node now reads each captured subscription again
//! immediately before the first hand-over to its subscriber, so the schedule
//! that showed the unsafe delivery, captured subscribers, then an
//! acknowledged ban, then the hand-over, delivers nothing to the banned
//! subscriber. The other subscribers of the capture still receive the event,
//! a subscriber added after the capture is not added to it, and another
//! node's fan-out proceeds while this one is held.
//!
//! What this is and is not: the fence of a PubSub affiliation or
//! subscription change on a Space no scope enrolls is its storage commit,
//! decided against at the hand-over. It carries no replica acknowledgement,
//! so a hand-over whose read preceded the commit can follow it. A Space
//! node an enrolled scope owns delivers nothing at all
//! (`xep0045_host_grant_fence`).
//!
//! The cases run the production fan-out against PubSub storage on a
//! PostgreSQL database of their own, behind a wrapper that delays the return
//! of one completed subscriber listing. A lane that sets
//! `WADDLE_TEST_REQUIRE_POSTGRES=1` fails them without PostgreSQL.
#![cfg(feature = "test-support")]

// The shared ingress fixture serves several suites; this one uses its
// isolated database only.
#[allow(dead_code)]
mod ingress_support;
#[path = "xep0060_space_fanout_fence/storage.rs"]
mod storage;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use ingress_support::IsolatedPostgresDatabase;
use jid::{BareJid, FullJid, Jid};
use storage::{CaptureBarrier, CapturingStorage};
use tokio::sync::mpsc;
use waddle_server::db::DatabaseDriver;
use waddle_server::pubsub::DatabasePubSubStorage;
use waddle_server::test_support::{
    fan_out_published_item, register_test_connection, websocket_state, WebSocketState,
};
use waddle_xmpp::pubsub::{parse_pubsub_event, Affiliation, NodeConfig, PubSubItem, PubSubStorage};
use waddle_xmpp::registry::{DeliveryKind, OutboundStanza};
use waddle_xmpp::xep::xep0402::{build_bookmark_item, Bookmark};
use waddle_xmpp::Stanza;

const BUDGET: Duration = Duration::from_secs(5);
/// The test state's Spaces service.
const SPACES: &str = "spaces.example.com";

/// PubSub storage on a PostgreSQL database of the case's own, an
/// independent connection to it, and a state whose fan-out reads through
/// the capturing wrapper.
struct Fixture {
    database: IsolatedPostgresDatabase,
    observer: DatabasePubSubStorage,
    storage: Arc<DatabasePubSubStorage>,
    barrier: Arc<CaptureBarrier>,
    state: Arc<WebSocketState>,
    owner: BareJid,
    node: String,
}

impl Fixture {
    async fn start(label: &str) -> Option<Self> {
        let database = IsolatedPostgresDatabase::create(label).await?;
        let storage = Arc::new(
            DatabasePubSubStorage::open(Some(database.url()))
                .await
                .expect("PostgreSQL PubSub storage"),
        );
        assert_eq!(storage.database().driver(), DatabaseDriver::Postgres);
        let observer = DatabasePubSubStorage::open(Some(database.url()))
            .await
            .expect("independent PostgreSQL connection");
        let owner: BareJid = SPACES.parse().expect("service");
        let node = format!("capture-{}", uuid::Uuid::new_v4());
        let barrier = Arc::new(CaptureBarrier::new());
        let mut state = websocket_state().await;
        Arc::get_mut(&mut state)
            .expect("unshared fixture state")
            .deps
            .protocol
            .pubsub_storage = Arc::new(CapturingStorage {
            inner: Arc::clone(&storage),
            barrier: Arc::clone(&barrier),
            owner: owner.clone(),
            node: node.clone(),
        });
        Some(Self {
            database,
            observer,
            storage,
            barrier,
            state,
            owner,
            node,
        })
    }

    /// A Space node with `item` published and `subscribers` subscribed as
    /// members.
    async fn seed(&self, node: &str, subscribers: &[&FullJid], item: &PubSubItem) {
        assert!(
            self.storage
                .get_or_create_node(&self.owner, node)
                .await
                .expect("create node")
                .1
        );
        self.storage
            .update_node_config(&self.owner, node, &NodeConfig::spaces_public())
            .await
            .expect("Space config");
        for subscriber in subscribers {
            self.storage
                .set_affiliation(
                    &self.owner,
                    node,
                    &subscriber.to_bare(),
                    Affiliation::Member,
                )
                .await
                .expect("member");
            self.storage
                .subscribe(&self.owner, node, &Jid::from(subscriber.to_bare()))
                .await
                .expect("subscription");
        }
        self.storage
            .publish_item(&self.owner, node, item, None, false)
            .await
            .expect("item");
    }

    async fn connect(&self, account: &FullJid) -> mpsc::Receiver<OutboundStanza> {
        let (queue, received) = mpsc::channel(8);
        register_test_connection(&self.state, account, queue).await;
        received
    }

    /// Fan out the publication of `item` on `node`, as a publish request
    /// does once the item is stored.
    async fn publish(&self, node: &str, item: &PubSubItem) {
        tokio::time::timeout(
            Duration::from_secs(30),
            fan_out_published_item(
                &self.state,
                &self.owner,
                node,
                item.id.as_deref().expect("item ID"),
                item.payload.clone().expect("item payload"),
                false,
            ),
        )
        .await
        .expect("fan-out completes");
    }

    async fn close(self) {
        drop(self.state);
        drop(self.storage);
        drop(self.observer);
        self.database.drop_database().await;
    }
}

/// The one event `queue` holds for `node`, carrying `item`.
async fn received(queue: &mut mpsc::Receiver<OutboundStanza>, node: &str, item: &PubSubItem) {
    let outbound = tokio::time::timeout(BUDGET, queue.recv())
        .await
        .expect("queued notification")
        .expect("open queue");
    assert_eq!(outbound.kind, DeliveryKind::DirectFrame);
    let Stanza::Message(message) = outbound.stanza else {
        panic!("expected an event message")
    };
    let event = parse_pubsub_event(&message).expect("PubSub event");
    assert_eq!(event.node, node);
    assert_eq!(event.items.len(), 1);
    assert_eq!(event.items[0].id, item.id);
    assert_eq!(event.items[0].payload, item.payload);
    assert!(queue.try_recv().is_err(), "one event for one fan-out");
}

fn bookmark(name: &str) -> PubSubItem {
    build_bookmark_item(
        &Bookmark::new("room@muc.example.com".parse().expect("room")).with_name(name),
    )
}

fn account(jid: &str) -> FullJid {
    jid.parse().expect("full JID")
}

#[tokio::test]
async fn a_subscriber_banned_after_the_capture_receives_nothing() {
    let Some(fixture) = Fixture::start("space_fanout_ban").await else {
        return;
    };
    let node = fixture.node.clone();
    let healthy_node = format!("healthy-{}", uuid::Uuid::new_v4());
    let original = account("original@example.com/web");
    let steady = account("steady@example.com/web");
    let newcomer = account("newcomer@example.com/web");
    let healthy = account("healthy@example.com/web");
    let item = bookmark("captured content");
    fixture.seed(&node, &[&original, &steady], &item).await;
    fixture.seed(&healthy_node, &[&healthy], &item).await;
    let mut original_queue = fixture.connect(&original).await;
    let mut steady_queue = fixture.connect(&steady).await;
    let mut newcomer_queue = fixture.connect(&newcomer).await;
    let mut healthy_queue = fixture.connect(&healthy).await;

    // The fan-out is held when its subscriber listing has completed: the
    // captured list holds both subscribers.
    let paused = fixture.publish(&node, &item);
    let controller = async {
        tokio::time::timeout(BUDGET, fixture.barrier.captured.notified())
            .await
            .expect("subscriber listing captured");
        assert_eq!(fixture.barrier.count.load(Ordering::SeqCst), 2);
        assert!(original_queue.try_recv().is_err());

        // The ban commits and is acknowledged while the capture is held.
        assert_eq!(
            fixture
                .observer
                .set_affiliation(
                    &fixture.owner,
                    &node,
                    &original.to_bare(),
                    Affiliation::Outcast
                )
                .await
                .expect("ban acknowledged"),
            Affiliation::Member
        );
        assert_eq!(
            fixture
                .observer
                .affiliation(&fixture.owner, &node, &original.to_bare())
                .await
                .expect("committed affiliation"),
            Affiliation::Outcast
        );
        // A subscription made after the capture is not part of it.
        fixture
            .observer
            .subscribe(&fixture.owner, &node, &Jid::from(newcomer.to_bare()))
            .await
            .expect("later subscription");

        // Another node's fan-out is not held up by this one.
        fixture.publish(&healthy_node, &item).await;
        received(&mut healthy_queue, &healthy_node, &item).await;
        assert!(original_queue.try_recv().is_err());
        assert!(steady_queue.try_recv().is_err());
        fixture.barrier.released.store(true, Ordering::SeqCst);
        fixture.barrier.release.notify_one();
    };
    tokio::join!(paused, controller);

    // The stale capture does not reach the banned subscriber's queue. The
    // subscriber that is still deliverable receives the event from the same
    // capture, and the later subscriber is not added to it.
    assert!(
        original_queue.try_recv().is_err(),
        "a captured subscriber banned before the hand-over received the event"
    );
    received(&mut steady_queue, &node, &item).await;
    assert!(newcomer_queue.try_recv().is_err());

    // A fan-out that lists its subscribers now reaches the two that remain.
    fixture.publish(&node, &item).await;
    received(&mut steady_queue, &node, &item).await;
    received(&mut newcomer_queue, &node, &item).await;
    assert!(original_queue.try_recv().is_err());
    fixture.close().await;
}

#[tokio::test]
async fn a_subscription_removed_after_the_capture_receives_nothing() {
    let Some(fixture) = Fixture::start("space_fanout_unsub").await else {
        return;
    };
    let node = fixture.node.clone();
    let leaver = account("leaver@example.com/web");
    let steady = account("steady@example.com/web");
    let item = bookmark("captured content");
    fixture.seed(&node, &[&leaver, &steady], &item).await;
    let mut leaver_queue = fixture.connect(&leaver).await;
    let mut steady_queue = fixture.connect(&steady).await;

    let paused = fixture.publish(&node, &item);
    let controller = async {
        tokio::time::timeout(BUDGET, fixture.barrier.captured.notified())
            .await
            .expect("subscriber listing captured");
        assert_eq!(fixture.barrier.count.load(Ordering::SeqCst), 2);
        let removed = fixture
            .observer
            .unsubscribe(&fixture.owner, &node, &Jid::from(leaver.to_bare()), None)
            .await
            .expect("unsubscription acknowledged");
        assert!(removed, "the subscription was removed");
        fixture.barrier.released.store(true, Ordering::SeqCst);
        fixture.barrier.release.notify_one();
    };
    tokio::join!(paused, controller);

    assert!(
        leaver_queue.try_recv().is_err(),
        "a captured subscription removed before the hand-over received the event"
    );
    received(&mut steady_queue, &node, &item).await;
    fixture.close().await;
}
