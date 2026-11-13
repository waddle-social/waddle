//! Subscription roster pushes reach a resource another node hosts through a
//! bound request for the coordinator's binding. They bypass the full owner
//! mirror and never append to a stale local stream.

use super::{
    connection_entry_for_current_node, origin_identity, receiver_identity, services_with_claims,
    test_peer_id, Arc, CancellationToken, ClusteringMessagingConfig, ConnectionEntry, DeliveryKind,
    OrderedRelayDeliveryBridge, OutboundStanza, Stanza, WebSocketState,
};
use crate::clustering::route_bridge::{TestBoundOffer, TestBoundTransport, TEST_BOUND_TRANSPORT};
use crate::clustering::ClusteringHandles;
use crate::db::roster::DatabaseRosterStorage;
use crate::server::routes::interpret::{
    FullJidDeliveryOutcome, OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind,
};
use crate::server::routes::websocket::handlers::account_reads::roster_interested;
use crate::server::routes::websocket::handlers::presence::handle_ordered_relay_presence_request;
use crate::server::routes::websocket::handlers::roster_fanout::send_roster_push_to_resources;
use crate::server::routes::websocket::test_bindings::{
    bind_remote_test_resource, sync_test_binding,
};
use crate::server::routes::websocket::test_state::{
    create_test_websocket_state_with_extension_manager, empty_extension_manager, TestStateOverrides,
};
use jid::FullJid;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};
use waddle_xmpp::ownership::account_resource_transition::AccountResourceSnapshot;
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, Entity, EntityType, NodeLeaseIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::registry::{BroadcastOutcome, ConnectionRegistry};
use waddle_xmpp::stream_management::persistence::{InMemorySmPersistence, SmPersistenceStorage};
use waddle_xmpp::stream_management::{
    DetachedSession, InMemorySmSessionRegistry, PromotionCaptureCustody, SmSessionRegistry,
};
use waddle_xmpp_core::roster::{RosterItem, RosterVersion, ROSTER_NS};
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::presence::{Presence, Type};

/// The owner node (`receiver-node`) of an account whose resource
/// `alice@example.com/phone` has its socket on `origin-node`. The account
/// coordinator names the socket's binding, so the owner reaches it by a
/// bound request; the test stands in for committing that request and for
/// the hosting node's decision ([`TEST_BOUND_TRANSPORT`]).
struct RemoteRosterFixture {
    state: Arc<WebSocketState>,
    /// The connection registry of the node that hosts the socket.
    socket_registry: Arc<ConnectionRegistry>,
    /// The binding the account coordinator names for `target`.
    binding: BindingRef,
    socket_receiver: mpsc::Receiver<OutboundStanza>,
    mirror_receiver: mpsc::Receiver<OutboundStanza>,
    persistence: Arc<InMemorySmPersistence>,
    registry: Arc<InMemorySmSessionRegistry>,
    target: FullJid,
    stream: SmSessionId,
}

impl RemoteRosterFixture {
    async fn new(roster_interested: bool) -> Self {
        Self::with_interest(roster_interested, roster_interested).await
    }

    /// `live_interest` is the remote resource's roster interest, as its
    /// owner mirror and the coordinator's snapshot record it;
    /// `detached_interest` is that of the stale local detached stream.
    async fn with_interest(live_interest: bool, detached_interest: bool) -> Self {
        let stopped = CancellationToken::new();
        stopped.cancel();
        let owner_bridge =
            OrderedRelayDeliveryBridge::new(stopped, &ClusteringMessagingConfig::default());
        let persistence = Arc::new(InMemorySmPersistence::new());
        let registry =
            Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
        let state = create_test_websocket_state_with_extension_manager(
            empty_extension_manager().await,
            TestStateOverrides {
                sm_session_registry: Some(registry.clone()),
                clustering: Some(ClusteringHandles {
                    ordered_relay_delivery_bridge: Some(owner_bridge.clone()),
                    ..ClusteringHandles::default()
                }),
                ..TestStateOverrides::default()
            },
        )
        .await;
        let mut owner_services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await;
        owner_services.connection_registry = state.deps.protocol.connection_registry.clone();
        owner_services.user_registry = state.deps.protocol.user_registry.clone();
        owner_services.sm_session_registry = registry.clone();
        owner_services.web_socket_state = Arc::downgrade(&state);
        owner_bridge.wire(Arc::new(owner_services));

        let target: FullJid = "alice@example.com/phone".parse().expect("remote target");
        // The owner's remote-hosted entry for the resource, with a full queue:
        // nothing may be handed to it.
        let (mirror_sender, mirror_receiver) = mpsc::channel(1);
        mirror_sender
            .try_send(OutboundStanza::new(Stanza::Presence(Presence::new(
                Type::None,
            ))))
            .expect("fill owner mirror before fanout");
        state.deps.protocol.connection_registry.register_entry(
            target.clone(),
            ConnectionEntry::remote_hosted(mirror_sender),
        );
        if live_interest {
            state
                .deps
                .protocol
                .connection_registry
                .mark_roster_interested(&target);
        }

        let socket_services = services_with_claims(
            receiver_identity(),
            origin_identity(),
            origin_identity(),
            test_peer_id(),
        )
        .await;
        let (socket_sender, socket_receiver) = mpsc::channel(4);
        let socket_entry = connection_entry_for_current_node(socket_sender, &socket_services);
        let binding = socket_entry
            .binding_ref()
            .cloned()
            .expect("the socket's entry carries its binding");
        let socket_registry = socket_services.connection_registry.clone();
        socket_registry.register_entry(target.clone(), socket_entry);
        bind_remote_test_resource(
            &state,
            &target,
            &binding,
            AccountResourceSnapshot {
                roster_interested: live_interest,
                ..AccountResourceSnapshot::default()
            },
        )
        .await;

        let stream = SmSessionId::new("stale-owner-roster-stream");
        registry
            .store_session(DetachedSession {
                promotion_custody: PromotionCaptureCustody::never_claimed(),
                stream_id: stream.as_str().into(),
                user_id: target.to_bare().to_string(),
                jid: target.clone(),
                occupancy_session: OccupancySessionGeneration::mint(),
                inbound_count: 3,
                outbound_count: 7,
                last_acked: 7,
                replay_gap_through: None,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: Instant::now(),
                carbons_enabled: false,
                roster_interested: detached_interest,
                blocklist_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            })
            .await
            .expect("retain local detached target");
        // The original owner still discovers a local target after its durable
        // source disappeared. A detached append must reject, never recreate it.
        persistence
            .delete_session(&stream)
            .await
            .expect("remove canonical detached source");
        Self {
            state,
            socket_registry,
            binding,
            socket_receiver,
            mirror_receiver,
            persistence,
            registry,
            target,
            stream,
        }
    }

    async fn subscription_push(&self) {
        let from = self.target.to_bare();
        let contact = "bob@example.com".parse::<jid::BareJid>().expect("contact");
        let mut presence = Presence::new(Type::Subscribed);
        presence.from = Some(from.clone().into());
        presence.to = Some(contact.clone().into());
        let entity = Entity::new(EntityType::UserActor, from.to_string());
        let origin = OrderedRelayRouteOrigin {
            kind: OrderedRelayRouteOriginKind::Entity(entity.clone()),
            sender_entity: entity,
            inbound_sequence: 0,
            handoff: None,
        };
        handle_ordered_relay_presence_request(&self.state, &contact, presence, origin)
            .await
            .expect("subscription mutation");
    }

    /// The hosting node's half of a bound request: the stanza goes only to
    /// the socket connection that carries the named binding.
    fn hand_to_binding(registry: &ConnectionRegistry, offer: &TestBoundOffer) -> BroadcastOutcome {
        registry
            .try_send_outbound_to_binding(
                &offer.target,
                &offer.binding,
                OutboundStanza::new(offer.stanza.clone()),
            )
            .expect("the socket node hosts the named binding")
    }

    /// A transport that records every bound request and answers `outcome`.
    fn recording(
        outcome: FullJidDeliveryOutcome,
    ) -> (TestBoundTransport, Arc<Mutex<Vec<TestBoundOffer>>>) {
        let offers = Arc::new(Mutex::new(Vec::new()));
        let recorded = offers.clone();
        let transport: TestBoundTransport = Arc::new(move |offer| {
            recorded.lock().expect("bound offers").push(offer);
            Box::pin(async move { outcome })
        });
        (transport, offers)
    }

    fn assert_addressed_to_socket(&self, offer: &TestBoundOffer) {
        assert_eq!(offer.target, self.target);
        assert_eq!(offer.binding, self.binding);
        assert_eq!(
            offer.node,
            origin_identity(),
            "addressed to the hosting node"
        );
        assert_eq!(offer.kind, DeliveryKind::DirectFrame);
        assert!(offer.ingress_append.is_none());
    }

    async fn assert_source_untouched(&self) {
        let source = self
            .registry
            .peek_session(self.stream.as_str())
            .await
            .expect("explicit remote offer must bypass the stale detached append")
            .expect("original source remains local");
        assert_eq!(
            (
                source.inbound_count,
                source.outbound_count,
                source.last_acked
            ),
            (3, 7, 7)
        );
        assert!(source.unacked_stanzas.is_empty());
        assert!(self
            .persistence
            .get_session(&self.stream)
            .await
            .expect("durable source")
            .is_none());
        assert!(self
            .persistence
            .list_unacked(&self.stream)
            .await
            .expect("durable replay")
            .is_empty());
    }
}

#[tokio::test]
async fn subscription_roster_push_bypasses_full_owner_mirror_to_real_remote_socket() {
    let mut fixture = RemoteRosterFixture::new(true).await;
    // The hosting node's decision is not read: the origin reports the
    // request as possibly committed, so a second offer would be unsafe.
    let (transport, offers) =
        RemoteRosterFixture::recording(FullJidDeliveryOutcome::MaybeCommitted);
    TEST_BOUND_TRANSPORT
        .scope(transport, fixture.subscription_push())
        .await;
    let offered = std::mem::take(&mut *offers.lock().expect("bound offers"));
    let [offer] = <[_; 1]>::try_from(offered).expect("exactly one bound roster request");
    fixture.assert_addressed_to_socket(&offer);
    assert_eq!(
        RemoteRosterFixture::hand_to_binding(&fixture.socket_registry, &offer),
        BroadcastOutcome::Delivered
    );
    let delivered = fixture
        .socket_receiver
        .try_recv()
        .expect("real socket queue accepted roster IQ");
    let Stanza::Iq(iq) = delivered.stanza else {
        panic!("roster push must be an IQ")
    };
    let Iq::Set {
        from, to, payload, ..
    } = *iq
    else {
        panic!("roster push must be IQ set")
    };
    assert_eq!(from, Some(fixture.target.to_bare().into()));
    assert_eq!(to, Some(fixture.target.clone().into()));
    assert!(payload.is("query", ROSTER_NS));
    let roster = DatabaseRosterStorage::new(fixture.state.deps.app_state.db_pool.global().clone());
    let version = roster
        .get_roster_version(&fixture.target.to_bare())
        .await
        .expect("stored roster version")
        .expect("mutation version");
    assert_eq!(payload.attr("ver"), Some(version.as_str()));
    let item = payload.get_child("item", ROSTER_NS).expect("roster item");
    assert_eq!(item.attr("jid"), Some("bob@example.com"));
    assert_eq!(item.attr("approved"), Some("true"));
    assert_eq!(item.attr("subscription"), Some("none"));
    assert!(
        fixture.socket_receiver.try_recv().is_err(),
        "socket receives one roster IQ"
    );
    assert!(matches!(
        fixture
            .mirror_receiver
            .try_recv()
            .expect("original full mirror entry")
            .stanza,
        Stanza::Presence(_)
    ));
    assert!(
        fixture.mirror_receiver.try_recv().is_err(),
        "MaybeCommitted cannot also queue a local fallback"
    );
    fixture.assert_source_untouched().await;
}

#[tokio::test]
async fn subscription_roster_push_does_not_offer_uninterested_registered_remote_resource() {
    let mut fixture = RemoteRosterFixture::new(false).await;
    let (transport, offers) = RemoteRosterFixture::recording(FullJidDeliveryOutcome::Delivered);
    TEST_BOUND_TRANSPORT
        .scope(transport, fixture.subscription_push())
        .await;
    assert!(
        offers.lock().expect("bound offers").is_empty(),
        "a binding alone cannot opt a resource into roster pushes"
    );
    assert!(fixture.socket_receiver.try_recv().is_err());
    fixture.assert_source_untouched().await;
}

#[tokio::test]
async fn subscription_roster_remote_backpressure_does_not_append_or_retry() {
    let mut fixture = RemoteRosterFixture::new(true).await;
    let socket_entry = fixture
        .socket_registry
        .entry(&fixture.target)
        .expect("registered socket entry");
    for _ in 0..4 {
        assert!(fixture
            .socket_registry
            .try_send_outbound_if_owner(
                &fixture.target,
                &socket_entry.binding_id(),
                OutboundStanza::new(Stanza::Presence(Presence::new(Type::None))),
            )
            .is_delivered());
    }
    // The lane is at its pending cap: the request is refused before it is
    // committed, a definite no-effect outcome (`Dropped`).
    let (transport, offers) = RemoteRosterFixture::recording(FullJidDeliveryOutcome::Dropped);
    TEST_BOUND_TRANSPORT
        .scope(transport, fixture.subscription_push())
        .await;
    assert_eq!(
        offers.lock().expect("bound offers").len(),
        1,
        "Some(Dropped) concludes this attempt without another offer"
    );
    for _ in 0..4 {
        assert!(matches!(
            fixture
                .socket_receiver
                .try_recv()
                .expect("original full socket entry")
                .stanza,
            Stanza::Presence(_)
        ));
    }
    assert!(fixture.socket_receiver.try_recv().is_err());
    assert!(matches!(
        fixture
            .mirror_receiver
            .try_recv()
            .expect("original full mirror entry")
            .stanza,
        Stanza::Presence(_)
    ));
    assert!(fixture.mirror_receiver.try_recv().is_err());
    fixture.assert_source_untouched().await;
}

#[tokio::test]
async fn subscription_roster_unavailable_remote_does_not_enqueue_to_uninterested_replacement() {
    // The live resource requested a roster; the independent stale detached
    // stream did not. After replacement neither current resource is interested.
    let fixture = RemoteRosterFixture::with_interest(true, false).await;
    let (replacement_sender, mut replacement_receiver) = mpsc::channel(4);
    let registry = fixture.state.deps.protocol.connection_registry.clone();
    let target = fixture.target.clone();
    let offers = Arc::new(AtomicUsize::new(0));
    let offer_count = offers.clone();
    let transport: TestBoundTransport = Arc::new(move |_offer| {
        let registry = registry.clone();
        let target = target.clone();
        let replacement_sender = replacement_sender.clone();
        let offers = offers.clone();
        Box::pin(async move {
            offers.fetch_add(1, Ordering::SeqCst);
            // Bind a fresh stream before the refusal returns. Its default
            // roster interest is false; the old resource's interest must not
            // authorize a local enqueue to this replacement.
            registry.register_entry(target, ConnectionEntry::new(replacement_sender));
            // The hosting node refused the request: the binding no longer
            // holds the resource there.
            FullJidDeliveryOutcome::Unavailable
        })
    });
    TEST_BOUND_TRANSPORT
        .scope(transport, fixture.subscription_push())
        .await;
    assert_eq!(offer_count.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .state
            .deps
            .protocol
            .connection_registry
            .entry(&fixture.target)
            .is_some(),
        "the newer registry owner is retained"
    );
    assert!(!fixture
        .state
        .deps
        .protocol
        .connection_registry
        .is_roster_interested(&fixture.target));
    assert!(
        replacement_receiver.try_recv().is_err(),
        "lost roster interest forbids fallback to the new stream"
    );
    fixture.assert_source_untouched().await;
}

/// A transport whose hosting node accepts the request on the real socket and
/// whose decision never returns.
fn stalled_after_delivery(
    fixture: &RemoteRosterFixture,
    stalled: Arc<Notify>,
) -> TestBoundTransport {
    let socket_registry = fixture.socket_registry.clone();
    Arc::new(move |offer| {
        let socket_registry = socket_registry.clone();
        let stalled = stalled.clone();
        Box::pin(async move {
            assert_eq!(
                RemoteRosterFixture::hand_to_binding(&socket_registry, &offer),
                BroadcastOutcome::Delivered
            );
            // The source must not wait for this decision before queuing
            // other resources.
            stalled.notify_one();
            std::future::pending().await
        })
    })
}

#[tokio::test]
async fn ready_local_roster_resource_is_queued_before_an_earlier_remote_reply_stalls() {
    let mut fixture = RemoteRosterFixture::new(true).await;
    let registry = &fixture.state.deps.protocol.connection_registry;
    let local: FullJid = "alice@example.com/web".parse().expect("second live JID");
    let (local_sender, mut local_receiver) = mpsc::channel(2);
    registry.register_entry(
        local.clone(),
        ConnectionEntry::new_for_connection(
            local_sender,
            ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(&receiver_identity())),
        ),
    );
    registry.mark_roster_interested(&local);
    sync_test_binding(&fixture.state, &local).await;
    let user = fixture.target.to_bare();
    // The blocked remote precedes the ready local in the coordinator's
    // order, which is the order of the live loop.
    assert_eq!(
        roster_interested(&fixture.state, &user).await,
        vec![fixture.target.clone(), local.clone()]
    );

    let reply_stalled = Arc::new(Notify::new());
    let transport = stalled_after_delivery(&fixture, reply_stalled.clone());
    let item = RosterItem::new("bob@example.com".parse().expect("roster contact"));
    let version: RosterVersion = "version-before-remote-timeout".parse().expect("version");
    let queued = TEST_BOUND_TRANSPORT
        .scope(transport, async {
            let mut fanout = Box::pin(send_roster_push_to_resources(
                &fixture.state,
                &user,
                &item,
                &version,
                None,
            ));
            let ready_while_remote_pending = async {
                let queued = local_receiver.recv().await.expect("ready local connection");
                reply_stalled.notified().await;
                queued
            };
            let queued = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    queued = ready_while_remote_pending => queued,
                    () = &mut fanout => panic!("remote reply must remain pending"),
                }
            })
            .await
            .expect(
                "ready local resource must be queued while the earlier remote reply is pending",
            );
            assert!(futures::poll!(fanout.as_mut()).is_pending());
            drop(fanout);
            queued
        })
        .await;
    let Stanza::Iq(push) = queued.stanza else {
        panic!("local roster IQ")
    };
    let Iq::Set { to, payload, .. } = *push else {
        panic!("local roster IQ set")
    };
    assert_eq!(to, Some(local.into()));
    assert!(payload.is("query", ROSTER_NS));
    assert_eq!(payload.attr("ver"), Some("version-before-remote-timeout"));
    assert!(local_receiver.try_recv().is_err(), "one ready-local IQ");
    assert!(matches!(
        fixture
            .mirror_receiver
            .try_recv()
            .expect("original full mirror entry")
            .stanza,
        Stanza::Presence(_)
    ));
    assert!(
        fixture.mirror_receiver.try_recv().is_err(),
        "local-only pass must skip remote-hosted mirrors"
    );
    let Stanza::Iq(remote_push) = fixture
        .socket_receiver
        .try_recv()
        .expect("actual B socket accepted the remote IQ")
        .stanza
    else {
        panic!("remote roster IQ")
    };
    assert_eq!(remote_push.to(), Some(&fixture.target.clone().into()));
    assert!(
        fixture.socket_receiver.try_recv().is_err(),
        "one actual remote IQ"
    );
}

#[tokio::test]
async fn ready_detached_roster_resource_is_persisted_while_a_remote_reply_stalls() {
    let mut fixture = RemoteRosterFixture::with_interest(true, false).await;
    let detached_jid: FullJid = "alice@example.com/detached"
        .parse()
        .expect("independent detached JID");
    let detached_stream = SmSessionId::new("independent-roster-stream");
    let original = fixture
        .registry
        .peek_session(fixture.stream.as_str())
        .await
        .expect("original local ghost lookup")
        .expect("original local ghost");
    fixture
        .registry
        .store_session(DetachedSession {
            promotion_custody: PromotionCaptureCustody::never_claimed(),
            stream_id: detached_stream.as_str().into(),
            jid: detached_jid.clone(),
            occupancy_session: OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            detached_at: Instant::now(),
            roster_interested: true,
            ..original
        })
        .await
        .expect("store independent detached resource");
    let reply_stalled = Arc::new(Notify::new());
    let transport = stalled_after_delivery(&fixture, reply_stalled.clone());
    let entry = TEST_BOUND_TRANSPORT
        .scope(transport, async {
            let mut fanout = Box::pin(fixture.subscription_push());
            let persisted_while_remote_pending = async {
                let entry = loop {
                    let queue = fixture
                        .persistence
                        .list_unacked(&detached_stream)
                        .await
                        .expect("observe independent durable replay");
                    if !queue.is_empty() {
                        let [entry] =
                            <[_; 1]>::try_from(queue).expect("one independent replay allocation");
                        break entry;
                    }
                    tokio::task::yield_now().await;
                };
                reply_stalled.notified().await;
                entry
            };
            let entry = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    entry = persisted_while_remote_pending => entry,
                    () = &mut fanout => panic!("remote reply must remain pending"),
                }
            })
            .await
            .expect("independent detached append must finish while remote reply is pending");
            assert!(futures::poll!(fanout.as_mut()).is_pending());
            drop(fanout);
            entry
        })
        .await;
    assert_eq!(entry.stream_id, detached_stream);
    assert_eq!(entry.sequence, 1);
    let Stanza::Iq(push) = entry.stanza.into_stanza() else {
        panic!("durable roster IQ")
    };
    let Iq::Set {
        to, id, payload, ..
    } = *push
    else {
        panic!("durable roster IQ set")
    };
    assert_eq!(to, Some(detached_jid.into()));
    assert!(payload.is("query", ROSTER_NS));
    let roster = DatabaseRosterStorage::new(fixture.state.deps.app_state.db_pool.global().clone());
    let version = roster
        .get_roster_version(&fixture.target.to_bare())
        .await
        .expect("mutation version lookup")
        .expect("mutation version");
    assert_eq!(payload.attr("ver"), Some(version.as_str()));
    let persisted = fixture
        .persistence
        .get_session(&detached_stream)
        .await
        .expect("independent source lookup")
        .expect("independent source retained");
    assert_eq!(
        (
            persisted.inbound_count,
            persisted.outbound_count,
            persisted.last_acked
        ),
        (0, 1, 0)
    );
    let resumed = fixture
        .registry
        .peek_session(detached_stream.as_str())
        .await
        .expect("reconcile any cancelled append publication")
        .expect("resumable independent resource");
    assert_eq!(resumed.outbound_count, 1);
    assert_eq!(resumed.unacked_stanzas.len(), 1);
    assert_eq!(resumed.unacked_stanzas[0].sequence, 1);
    let queue = fixture
        .persistence
        .list_unacked(&detached_stream)
        .await
        .expect("independent replay after cancellation");
    assert_eq!(queue.len(), 1);
    let Stanza::Iq(replayed) = queue[0].stanza.as_ref() else {
        panic!("retained replay IQ")
    };
    assert_eq!(replayed.id(), id);
    assert!(matches!(
        fixture
            .mirror_receiver
            .try_recv()
            .expect("original full mirror entry")
            .stanza,
        Stanza::Presence(_)
    ));
    assert!(fixture.mirror_receiver.try_recv().is_err());
    assert!(matches!(
        fixture
            .socket_receiver
            .try_recv()
            .expect("actual B accepted the remote IQ")
            .stanza,
        Stanza::Iq(_)
    ));
    assert!(fixture.socket_receiver.try_recv().is_err());
    fixture.assert_source_untouched().await;
}
