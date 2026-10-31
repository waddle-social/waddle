//! Negative control: current Space fanout can enqueue after a later Outcast commit.
//! This characterizes queue acceptance, not authorized admission, durable SM custody,
//! socket delivery, remote federation or the policy a future fence should enforce.
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use tokio::sync::mpsc;
use waddle_xmpp::pubsub::{
    build_pubsub_event, parse_pubsub_event, Affiliation, NodeConfig, PubSubEvent, PubSubItem,
    PubSubStorage,
};
use waddle_xmpp::registry::{DeliveryKind, OutboundStanza};
use waddle_xmpp::xep::xep0402::{build_bookmark_item, Bookmark};
use waddle_xmpp::Stanza;

use super::{create_test_websocket_state, register_test_connection};
use crate::db::DatabaseDriver;
use crate::pubsub::DatabasePubSubStorage;
use crate::server::routes::websocket::handlers::pubsub_fanout::{fan_out_publish, FanOutRequest};
use crate::server::routes::websocket::WebSocketState;

mod storage;
use storage::{CaptureBarrier, CapturingStorage};

const BUDGET: Duration = Duration::from_secs(5);

async fn publish(state: &WebSocketState, owner: &BareJid, node: &str, item: &PubSubItem) {
    tokio::time::timeout(
        BUDGET,
        fan_out_publish(
            state,
            FanOutRequest {
                owner,
                node,
                published_item: item,
                item_id: item.id.as_deref().expect("fixed item ID"),
                publisher: None,
                publisher_full: None,
                is_pep: false,
            },
        ),
    )
    .await
    .expect("fanout must complete within the test budget");
}

async fn receive(
    rx: &mut mpsc::Receiver<OutboundStanza>,
    owner: &BareJid,
    node: &str,
    target: &FullJid,
    item: &PubSubItem,
) {
    let outbound = tokio::time::timeout(BUDGET, rx.recv())
        .await
        .expect("queued notification deadline")
        .expect("open resource queue");
    assert_eq!(outbound.kind, DeliveryKind::DirectFrame);
    assert!(outbound.pending_row_id.is_none());
    assert!(outbound.ingress_append.is_none());
    let Stanza::Message(actual) = outbound.stanza else {
        panic!("expected typed event message")
    };
    // Check semantic fields independently of the builder used by production.
    let parsed = parse_pubsub_event(&actual).expect("typed queued PubSub event");
    assert_eq!(parsed.node, node);
    assert_eq!(parsed.items.len(), 1);
    assert_eq!(parsed.items[0].id, item.id);
    assert_eq!(parsed.items[0].payload, item.payload);
    assert!(parsed.items[0].publisher.is_none());
    let expected = build_pubsub_event(
        &Jid::from(owner.clone()),
        &Jid::from(target.clone()),
        &PubSubEvent::new(node, vec![item.clone()]),
    );
    assert_eq!(
        Element::from(actual),
        Element::from(expected),
        "exact frozen event, payload, item ID and resource target"
    );
    assert!(
        rx.try_recv().is_err(),
        "one queue acceptance for this invocation"
    );
}

async fn seed(
    storage: &DatabasePubSubStorage,
    owner: &BareJid,
    node: &str,
    subscriber: &FullJid,
    item: &PubSubItem,
) {
    assert!(
        storage
            .get_or_create_node(owner, node)
            .await
            .expect("create real node")
            .1
    );
    storage
        .update_node_config(owner, node, &NodeConfig::spaces_public())
        .await
        .expect("Space config");
    storage
        .set_affiliation(owner, node, &subscriber.to_bare(), Affiliation::Member)
        .await
        .expect("member commit");
    storage
        .subscribe(owner, node, &Jid::from(subscriber.to_bare()))
        .await
        .expect("subscription commit");
    storage
        .publish_item(owner, node, item, None, false)
        .await
        .expect("item commit");
}

#[tokio::test]
#[ignore = "requires native PostgreSQL; dedicated space_fanout_postgres lane"]
async fn postgres_space_fanout_outcast_after_capture_negative_control() {
    let url = waddle_ws_test_support::require_test_postgres_url()
        .expect("native PostgreSQL prerequisite is mandatory");
    assert!(
        url.starts_with("postgres://") || url.starts_with("postgresql://"),
        "PostgreSQL URL required"
    );
    let database = Arc::new(
        DatabasePubSubStorage::open(Some(&url))
            .await
            .expect("real PostgreSQL PubSub storage"),
    );
    assert_eq!(database.database().driver(), DatabaseDriver::Postgres);
    let observer = DatabasePubSubStorage::open(Some(&url))
        .await
        .expect("independent PostgreSQL connection");
    let owner: BareJid = "spaces.example.com".parse().expect("service");
    // Fresh node identities avoid inherited rows if this native fixture is reused.
    let node = format!("capture-{}", uuid::Uuid::new_v4());
    let healthy_node = format!("healthy-{}", uuid::Uuid::new_v4());
    let original: FullJid = "original@example.com/web".parse().expect("original");
    let newcomer: FullJid = "newcomer@example.com/web".parse().expect("newcomer");
    let healthy: FullJid = "healthy@example.com/web".parse().expect("healthy");
    let item = build_bookmark_item(
        &Bookmark::new("room@muc.example.com".parse().expect("room")).with_name("captured content"),
    );
    seed(&database, &owner, &node, &original, &item).await;
    seed(&database, &owner, &healthy_node, &healthy, &item).await;
    let persisted = observer
        .get_items(&owner, &node, None, &[])
        .await
        .expect("independent committed payload read");
    assert_eq!(persisted.len(), 1);
    let frozen = persisted[0].to_pubsub_item();
    assert_eq!(frozen.id, item.id);
    assert_eq!(frozen.payload, item.payload);

    let barrier = Arc::new(CaptureBarrier::new());
    let mut state = create_test_websocket_state().await;
    Arc::get_mut(&mut state)
        .expect("unshared fixture state")
        .deps
        .protocol
        .pubsub_storage = Arc::new(CapturingStorage {
        inner: database.clone(),
        barrier: barrier.clone(),
        owner: owner.clone(),
        node: node.clone(),
    });
    let (tx, mut original_rx) = mpsc::channel(8);
    register_test_connection(&state, &original, tx).await;
    let (tx, mut newcomer_rx) = mpsc::channel(8);
    register_test_connection(&state, &newcomer, tx).await;
    let (tx, mut healthy_rx) = mpsc::channel(8);
    register_test_connection(&state, &healthy, tx).await;

    // join! keeps ownership of both futures: no detached fanout task survives a
    // failed controller assertion. This first invocation needs the longer
    // budget because its intentionally paused interval includes the controller.
    let paused = fan_out_publish(
        &state,
        FanOutRequest {
            owner: &owner,
            node: &node,
            published_item: &frozen,
            item_id: frozen.id.as_deref().expect("frozen ID"),
            publisher: None,
            publisher_full: None,
            is_pep: false,
        },
    );
    let controller = async {
        tokio::time::timeout(BUDGET, barrier.captured.notified())
            .await
            .expect("real subscriber query captured");
        assert_eq!(barrier.count.load(Ordering::SeqCst), 1);
        assert!(
            original_rx.try_recv().is_err(),
            "no send before query returns"
        );
        assert_eq!(
            observer
                .set_affiliation(&owner, &node, &original.to_bare(), Affiliation::Outcast)
                .await
                .expect("Outcast write acknowledged"),
            Affiliation::Member
        );
        assert_eq!(
            observer
                .affiliation(&owner, &node, &original.to_bare())
                .await
                .expect("committed affiliation"),
            Affiliation::Outcast
        );
        assert!(observer
            .list_deliverable_subscribers(&owner, &node)
            .await
            .expect("fresh query excludes outcast")
            .is_empty());
        observer
            .subscribe(&owner, &node, &Jid::from(newcomer.to_bare()))
            .await
            .expect("new subscription committed after capture");
        let current = observer
            .list_deliverable_subscribers(&owner, &node)
            .await
            .expect("current subscribers");
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].subscriber, Jid::from(newcomer.to_bare()));

        // Actual other-scope mutation and queue acceptance while the first
        // query is held. Healthy storage is assumed; this is not outage proof.
        observer
            .set_affiliation(
                &owner,
                &healthy_node,
                &healthy.to_bare(),
                Affiliation::Publisher,
            )
            .await
            .expect("independent scope mutation");
        publish(&state, &owner, &healthy_node, &frozen).await;
        receive(&mut healthy_rx, &owner, &healthy_node, &healthy, &frozen).await;
        assert!(!barrier.released.load(Ordering::SeqCst));
        assert!(original_rx.try_recv().is_err());
        barrier.released.store(true, Ordering::SeqCst);
        barrier.release.notify_one();
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(paused, controller);
    })
    .await
    .expect("bounded deterministic schedule");

    // Expected unsafe negative control: the captured recipient receives the
    // exact notification despite the later committed Outcast. No new policy
    // is inferred from capture, and a late subscriber is not added to it.
    receive(&mut original_rx, &owner, &node, &original, &frozen).await;
    assert!(
        newcomer_rx.try_recv().is_err(),
        "captured audience cannot expand"
    );
    publish(&state, &owner, &node, &frozen).await;
    receive(&mut newcomer_rx, &owner, &node, &newcomer, &frozen).await;
    assert!(
        original_rx.try_recv().is_err(),
        "fresh completed fanout excludes committed Outcast"
    );
}
