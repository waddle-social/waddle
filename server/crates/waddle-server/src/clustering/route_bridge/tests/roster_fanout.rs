//! Subscription roster pushes must bypass a full owner mirror through the
//! registered remote-resource route without appending to a stale local stream.

use super::{
    connection_entry_for_current_node, origin_identity, receiver_identity, services_with_claims,
    test_peer_id, Arc, CancellationToken, ClusteringMessagingConfig, ConnectionEntry, DeliveryKind,
    NodeId, OrderedRelayDeliveryBridge, OutboundStanza, RelayDeliverRemoteResourceFrame,
    RelayRemoteResourceFrameStatus, RelayTraceContext, RemoteOwnerRegistration,
    RemoteResourceOutboundFrame, RemoteResourceSocketGeneration, Stanza, WebSocketState,
};
use crate::clustering::route_bridge::{
    TestRemoteFrameTransport, TEST_CANCELLED_REMOTE_FRAMES, TEST_REMOTE_FRAME_TRANSPORT,
};
use crate::clustering::ClusteringHandles;
use crate::db::roster::DatabaseRosterStorage;
use crate::server::routes::interpret::{OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind};
use crate::server::routes::websocket::handlers::presence::handle_ordered_relay_presence_request;
use crate::server::routes::websocket::handlers::roster_fanout::send_roster_push_to_resources;
use crate::server::routes::websocket::test_state::{
    create_test_websocket_state_with_extension_manager, empty_extension_manager, TestStateOverrides,
};
use jid::FullJid;
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};
use waddle_xmpp::ownership::{Entity, EntityType, NodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{InMemorySmPersistence, SmPersistenceStorage};
use waddle_xmpp::stream_management::{
    DetachedSession, InMemorySmSessionRegistry, PromotionCaptureCustody, SmSessionRegistry,
};
use waddle_xmpp_core::roster::{RosterItem, RosterVersion, ROSTER_NS};
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::presence::{Presence, Type};

struct RemoteRosterFixture {
    state: Arc<WebSocketState>,
    socket_bridge: Arc<OrderedRelayDeliveryBridge>,
    socket_receiver: mpsc::Receiver<OutboundStanza>,
    mirror_receiver: mpsc::Receiver<OutboundStanza>,
    persistence: Arc<InMemorySmPersistence>,
    registry: Arc<InMemorySmSessionRegistry>,
    target: FullJid,
    stream: SmSessionId,
}

impl RemoteRosterFixture {
    async fn new(roster_interested: bool) -> Self {
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
        let (mirror_sender, mirror_receiver) = mpsc::channel(1);
        mirror_sender
            .try_send(OutboundStanza::new(Stanza::Presence(Presence::new(
                Type::None,
            ))))
            .expect("fill owner mirror before fanout");
        let mirror_entry = ConnectionEntry::remote_hosted(mirror_sender);
        let mirror_owner = mirror_entry.binding_id();
        state
            .deps
            .protocol
            .connection_registry
            .register_entry(target.clone(), mirror_entry);
        if roster_interested {
            state
                .deps
                .protocol
                .connection_registry
                .mark_roster_interested(&target);
        }

        let socket_services = Arc::new(
            services_with_claims(
                receiver_identity(),
                origin_identity(),
                origin_identity(),
                test_peer_id(),
            )
            .await,
        );
        let socket_bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        socket_bridge.wire(socket_services.clone());
        let (socket_sender, socket_receiver) = mpsc::channel(4);
        let socket_entry = connection_entry_for_current_node(socket_sender, &socket_services);
        let socket_owner = socket_entry.binding_id();
        socket_services
            .connection_registry
            .register_entry(target.clone(), socket_entry);
        let registration_id = socket_bridge
            .test_insert_remote_socket_registration(
                target.clone(),
                socket_owner,
                NodeId::new(receiver_identity().node_id),
            )
            .await;
        owner_bridge.remote_owner_resources.lock().await.insert(
            target.clone(),
            RemoteOwnerRegistration {
                occupancy_session: OccupancySessionGeneration::mint(),
                socket_identity: origin_identity(),
                connection_ref: waddle_xmpp::ownership::ConnectionRef::fresh(
                    waddle_xmpp::ownership::NodeLeaseIdentity::from_node_identity(
                        &NodeIdentity::new(origin_identity().node_id, "fixture-epoch"),
                    ),
                ),
                unregister_pending: false,
                registration_id,
                socket_generation: RemoteResourceSocketGeneration::next(None),
                socket_node: NodeId::new(origin_identity().node_id),
                owner: mirror_owner,
            },
        );

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
                roster_interested,
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
            socket_bridge,
            socket_receiver,
            mirror_receiver,
            persistence,
            registry,
            target,
            stream,
        }
    }

    async fn subscription_push(&self) -> Vec<RemoteResourceOutboundFrame> {
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
        TEST_CANCELLED_REMOTE_FRAMES
            .scope(RefCell::new(Vec::new()), async {
                handle_ordered_relay_presence_request(&self.state, &contact, presence, origin)
                    .await
                    .expect("subscription mutation");
                TEST_CANCELLED_REMOTE_FRAMES.with(|frames| frames.take())
            })
            .await
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
    let offered = fixture.subscription_push().await;
    // The stopped real ask returns MaybeCommitted: a second offer would be
    // unsafe. Capture at the actual DTO boundary rather than returning success.
    let [frame] = <[_; 1]>::try_from(offered).expect("exactly one remote roster frame offered");
    assert_eq!(frame.jid, fixture.target);
    assert_eq!(frame.kind, DeliveryKind::DirectFrame);
    assert!(frame.ingress_append.is_none());
    let reply = fixture
        .socket_bridge
        .deliver_remote_resource_frame_on_socket(RelayDeliverRemoteResourceFrame {
            frame,
            trace: RelayTraceContext::default(),
        })
        .await;
    assert_eq!(reply.status, RelayRemoteResourceFrameStatus::Delivered);
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
    assert!(
        fixture.subscription_push().await.is_empty(),
        "registration alone cannot opt a resource into roster pushes"
    );
    assert!(fixture.socket_receiver.try_recv().is_err());
    fixture.assert_source_untouched().await;
}

#[tokio::test]
async fn subscription_roster_remote_backpressure_does_not_append_or_retry() {
    let mut fixture = RemoteRosterFixture::new(true).await;
    let socket_registry = &fixture
        .socket_bridge
        .services
        .get()
        .expect("wired socket services")
        .connection_registry;
    let socket_entry = socket_registry
        .entry(&fixture.target)
        .expect("registered socket entry");
    for _ in 0..4 {
        assert!(socket_registry
            .try_send_outbound_if_owner(
                &fixture.target,
                &socket_entry.binding_id(),
                OutboundStanza::new(Stanza::Presence(Presence::new(Type::None))),
            )
            .is_delivered());
    }
    let offers = Arc::new(AtomicUsize::new(0));
    let offer_count = offers.clone();
    let socket_bridge = fixture.socket_bridge.clone();
    let transport: TestRemoteFrameTransport = Arc::new(move |message| {
        let offers = offers.clone();
        let socket_bridge = socket_bridge.clone();
        Box::pin(async move {
            offers.fetch_add(1, Ordering::SeqCst);
            let reply = socket_bridge
                .deliver_remote_resource_frame_on_socket(message)
                .await;
            assert_eq!(reply.status, RelayRemoteResourceFrameStatus::Backpressure);
            reply
        })
    });
    assert!(TEST_REMOTE_FRAME_TRANSPORT
        .scope(transport, fixture.subscription_push())
        .await
        .is_empty());
    assert_eq!(
        offer_count.load(Ordering::SeqCst),
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
    // The live mirror requested a roster; the independent stale detached stream
    // did not. After replacement neither current resource is interested.
    let fixture = RemoteRosterFixture::new(false).await;
    fixture
        .state
        .deps
        .protocol
        .connection_registry
        .mark_roster_interested(&fixture.target);
    fixture
        .socket_bridge
        .remote_socket_resources
        .lock()
        .await
        .remove(&fixture.target);
    let (replacement_sender, mut replacement_receiver) = mpsc::channel(4);
    let registry = fixture.state.deps.protocol.connection_registry.clone();
    let target = fixture.target.clone();
    let socket_bridge = fixture.socket_bridge.clone();
    let offers = Arc::new(AtomicUsize::new(0));
    let offer_count = offers.clone();
    let transport: TestRemoteFrameTransport = Arc::new(move |message| {
        let registry = registry.clone();
        let target = target.clone();
        let socket_bridge = socket_bridge.clone();
        let replacement_sender = replacement_sender.clone();
        let offers = offers.clone();
        Box::pin(async move {
            offers.fetch_add(1, Ordering::SeqCst);
            let reply = socket_bridge
                .deliver_remote_resource_frame_on_socket(message)
                .await;
            assert_eq!(reply.status, RelayRemoteResourceFrameStatus::Unavailable);
            // Bind a fresh stream before stale-registration cleanup returns.
            // Its default roster interest is false; the old mirror's interest
            // must not authorize a local enqueue to this replacement.
            registry.register_entry(target, ConnectionEntry::new(replacement_sender));
            reply
        })
    });
    assert!(TEST_REMOTE_FRAME_TRANSPORT
        .scope(transport, fixture.subscription_push())
        .await
        .is_empty());
    assert_eq!(offer_count.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .state
            .deps
            .protocol
            .connection_registry
            .entry(&fixture.target)
            .is_some(),
        "cleanup must retain the newer registry owner"
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

#[tokio::test]
async fn ready_local_roster_resource_is_queued_before_an_earlier_remote_reply_stalls() {
    let mut fixture = RemoteRosterFixture::new(false).await;
    let registry = &fixture.state.deps.protocol.connection_registry;
    let other: FullJid = "alice@example.com/other".parse().expect("second live JID");
    let (placeholder_sender, _placeholder_receiver) = mpsc::channel(2);
    registry.register_entry(other.clone(), ConnectionEntry::new(placeholder_sender));
    registry.mark_roster_interested(&fixture.target);
    registry.mark_roster_interested(&other);
    let user = fixture.target.to_bare();
    let order = registry.roster_interested_resources_for_user(&user);
    let [remote, local] = <[_; 2]>::try_from(order.clone()).expect("two live resources");
    let (mirror_sender, mut mirror_receiver) = mpsc::channel(2);
    let mirror_entry = ConnectionEntry::remote_hosted(mirror_sender);
    let mirror_owner = mirror_entry.binding_id();
    registry.register_entry(remote.clone(), mirror_entry);
    let (local_sender, mut local_receiver) = mpsc::channel(2);
    registry.register_entry(local.clone(), ConnectionEntry::new(local_sender));
    registry.mark_roster_interested(&remote);
    registry.mark_roster_interested(&local);
    // Occupied-entry replacement preserves hash keys and their iteration order;
    // the blocked remote must precede the ready local in the real live loop.
    assert_eq!(registry.roster_interested_resources_for_user(&user), order);

    let socket_registry = &fixture
        .socket_bridge
        .services
        .get()
        .expect("wired socket services")
        .connection_registry;
    let socket_entry = socket_registry
        .entry(&fixture.target)
        .expect("actual B socket");
    let socket_owner = socket_entry.binding_id();
    socket_registry.register_entry(remote.clone(), socket_entry);
    let registration_id = fixture
        .socket_bridge
        .test_insert_remote_socket_registration(
            remote.clone(),
            socket_owner,
            NodeId::new(receiver_identity().node_id),
        )
        .await;
    let owner_bridge = fixture
        .state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
        .expect("owner bridge");
    {
        let mut registrations = owner_bridge.remote_owner_resources.lock().await;
        assert!(registrations.remove(&fixture.target).is_some());
        registrations.insert(
            remote.clone(),
            RemoteOwnerRegistration {
                occupancy_session: OccupancySessionGeneration::mint(),
                socket_identity: origin_identity(),
                connection_ref: waddle_xmpp::ownership::ConnectionRef::fresh(
                    waddle_xmpp::ownership::NodeLeaseIdentity::from_node_identity(
                        &NodeIdentity::new(origin_identity().node_id, "fixture-epoch"),
                    ),
                ),
                unregister_pending: false,
                registration_id,
                socket_generation: RemoteResourceSocketGeneration::next(None),
                socket_node: NodeId::new(origin_identity().node_id),
                owner: mirror_owner,
            },
        );
    }
    let reply_stalled = Arc::new(Notify::new());
    let reply_stalled_by_transport = reply_stalled.clone();
    let socket_bridge = fixture.socket_bridge.clone();
    let transport: TestRemoteFrameTransport = Arc::new(move |message| {
        let socket_bridge = socket_bridge.clone();
        let reply_stalled = reply_stalled_by_transport.clone();
        Box::pin(async move {
            let reply = socket_bridge
                .deliver_remote_resource_frame_on_socket(message)
                .await;
            assert_eq!(reply.status, RelayRemoteResourceFrameStatus::Delivered);
            // Model a peer whose actual socket accepted the IQ but whose reply
            // never returns. The source must not wait before queuing locals.
            reply_stalled.notify_one();
            std::future::pending().await
        })
    });
    let item = RosterItem::new("bob@example.com".parse().expect("roster contact"));
    let version: RosterVersion = "version-before-remote-timeout".parse().expect("version");
    let queued = TEST_REMOTE_FRAME_TRANSPORT
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
    assert!(
        mirror_receiver.try_recv().is_err(),
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
    assert_eq!(remote_push.to(), Some(&remote.into()));
    assert!(
        fixture.socket_receiver.try_recv().is_err(),
        "one actual remote IQ"
    );
}

#[tokio::test]
async fn ready_detached_roster_resource_is_persisted_while_a_remote_reply_stalls() {
    let mut fixture = RemoteRosterFixture::new(false).await;
    fixture
        .state
        .deps
        .protocol
        .connection_registry
        .mark_roster_interested(&fixture.target);
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
    let reply_stalled_by_transport = reply_stalled.clone();
    let socket_bridge = fixture.socket_bridge.clone();
    let transport: TestRemoteFrameTransport = Arc::new(move |message| {
        let socket_bridge = socket_bridge.clone();
        let reply_stalled = reply_stalled_by_transport.clone();
        Box::pin(async move {
            let reply = socket_bridge
                .deliver_remote_resource_frame_on_socket(message)
                .await;
            assert_eq!(reply.status, RelayRemoteResourceFrameStatus::Delivered);
            reply_stalled.notify_one();
            std::future::pending().await
        })
    });
    let entry = TEST_REMOTE_FRAME_TRANSPORT
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
                    _ = &mut fanout => panic!("remote reply must remain pending"),
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
    let Stanza::Iq(push) = *entry.stanza else {
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
