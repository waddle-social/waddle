use super::super::super::connection::{
    finish_connection, ConnectionShutdownChannels, FrameAuthority,
};
use super::super::super::outbound::{handle_outbound_stanza, OutboundAuthority};
use super::super::super::send::InterruptibleSink;
use super::super::super::timers::TransportTimers;
use super::{
    bind_fresh_test_connection, create_test_session, create_test_websocket_state,
    finish_test_socket_cleanup, mpsc, publish_stream_id_and_presence, Arc, FullJid, OutboundStanza,
    RegistrationAfterFrame, Stanza,
};
use crate::clustering::relay::{
    RelayForceDetachRemoteUserResource, RelayRemoteResourceForceDetachStatus,
};
use crate::clustering::route_bridge::{
    track_remote_socket_for_test, wire_for_test, OrderedRelayDeliveryBridge,
};
use crate::clustering::trace_context::RelayTraceContext;
use std::time::Duration;
use waddle_xmpp::muc::room_actor::{GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation};
use waddle_xmpp::muc::room_registry_actor::CreateRoom;
use waddle_xmpp::ownership::{InProcessClaimStore, NodeIdentity, SharedNodeIdentity};
use waddle_xmpp::registry::{ForceDetachOrigin, ForceDetachOutcome, SocketCleanupState};
use waddle_xmpp::stream_management::SmSessionRegistry;

#[tokio::test]
async fn cross_node_fresh_bind_retires_socket_blocked_in_readiness() {
    assert_remote_retirement(super::super::send::PendingReadySink { sent: Vec::new() }).await;
}

#[tokio::test]
async fn cross_node_fresh_bind_retires_socket_blocked_in_flush() {
    assert_remote_retirement(super::super::send::PendingFlushSink { sent: Vec::new() }).await;
}

async fn assert_remote_retirement<S>(inner: S)
where
    S: futures::Sink<axum::extract::ws::Message, Error = &'static str> + Unpin,
{
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/cross-node-stall".parse().unwrap();
    let (outcome, mut incumbent, mut outbound_rx, _) =
        bind_fresh_test_connection(state.clone(), jid.clone()).await;
    assert!(matches!(outcome, RegistrationAfterFrame::Registered(_)));
    incumbent.authenticated_session = Some(create_test_session(&state, "alice").await);
    let lifecycle = incumbent.socket_lifecycle.clone().unwrap();
    let old_owner = incumbent.registry_owner.clone().unwrap();
    let mut force_detach_rx = Some(lifecycle.entry.take_force_detach_rx().unwrap());
    let stream_id = "cross-node-stalled-stream";
    incumbent
        .sm_state
        .enable(stream_id.to_owned(), true, Some(300));
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim(stream_id)
            .await
            .unwrap(),
    );
    publish_stream_id_and_presence(&state, &jid, &old_owner, &incumbent);

    let room = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: "cross-node-stall@muc.example.com".parse().unwrap(),
            waddle_id: "w".to_owned(),
            channel_id: "c".to_owned(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .unwrap();
    room.ask(JoinWithAffiliation {
        sender_jid: jid.clone(),
        nick: "alice".to_owned(),
        affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp_core::Affiliation::Member),
        local_domain: "example.com".to_owned(),
        admission_revision: room.ask(GetSnapshot).await.unwrap().admission_revision,
        session: incumbent.occupancy_session,
    })
    .await
    .unwrap();

    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    wire_for_test(
        &bridge,
        &state,
        Arc::new(InProcessClaimStore::new()),
        SharedNodeIdentity::new(NodeIdentity::new("socket-node", "socket-epoch")),
    )
    .await;
    let registration_id = track_remote_socket_for_test(&bridge, &jid, old_owner.clone()).await;
    let request = RelayForceDetachRemoteUserResource {
        jid: jid.clone(),
        registration_id,
        occupancy_session: Some(incumbent.occupancy_session),
        origin: ForceDetachOrigin::FreshBindReplacement,
        requester_bare_jid: jid.to_bare(),
        trace: RelayTraceContext::default(),
    };
    let permit = state.deps.app_state.node_lifecycle.admit().unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut sink = InterruptibleSink::new(inner, incumbent.write_interruption.clone());
    let mut reader = futures::stream::pending::<Result<axum::extract::ws::Message, &'static str>>();
    let mut timers = TransportTimers::new();
    let stanza = xmpp_parsers::iq::Iq::from_get("blocked-outbound", xmpp_parsers::ping::Ping);
    let retirement = bridge.force_detach_remote_user_resource_on_socket(request.clone());
    tokio::pin!(retirement);
    let started = tokio::time::Instant::now();
    {
        let outbound = handle_outbound_stanza(
            &mut sink,
            &mut reader,
            &state,
            &mut incumbent,
            &mut timers,
            OutboundStanza::new(Stanza::Iq(Box::new(stanza))),
            OutboundAuthority {
                permit: &permit,
                shutdown: &shutdown,
            },
        );
        tokio::pin!(outbound);
        assert!(futures::poll!(outbound.as_mut()).is_pending());
        assert!(futures::poll!(retirement.as_mut()).is_pending());
        assert!(!tokio::time::timeout(Duration::from_secs(1), outbound)
            .await
            .expect("remote replacement interrupts the ordinary 60-second write budget"));
    }
    // Keep the relay future alive while the owning task crosses its full
    // shutdown path below; enqueue/interruption alone must not release it.
    assert!(futures::poll!(retirement.as_mut()).is_pending());

    // The full shutdown tail processes this parked request as a fresh
    // bind, rather than dropping it as an unhandled resume suffix.
    let enable_carbons =
        xmpp_parsers::iq::Iq::from_set("parked-enable", xmpp_parsers::carbons::Enable);
    assert!(!lifecycle.entry.is_carbons_enabled());
    incumbent.deferred_inbound.push_back(
        waddle_xmpp::parser::stanza_to_string(enable_carbons)
            .unwrap()
            .into(),
    );
    let (_handoff_tx, mut handoff_rx) = mpsc::unbounded_channel();
    let mut pending = Vec::new();
    let (_, reply) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(
            finish_connection(
                &state,
                &mut incumbent,
                sink.uninterrupted(),
                FrameAuthority {
                    permit: &permit,
                    shutdown: &shutdown,
                },
                false,
                ConnectionShutdownChannels {
                    outbound_rx: &mut outbound_rx,
                    handoff_rx: &mut handoff_rx,
                    force_detach_rx: &mut force_detach_rx,
                    pending_force_detach: &mut pending,
                },
            ),
            async {
                let reply = retirement.await;
                assert_eq!(
                    lifecycle.state(),
                    SocketCleanupState::Retired,
                    "relay acknowledgement must follow the socket cleanup boundary"
                );
                reply
            }
        )
    })
    .await
    .expect("owning shutdown finishes within the relay/bind budgets");
    assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Detached);
    assert_eq!(reply.outcome, ForceDetachOutcome::NotPersisted);
    assert!(started.elapsed() < crate::config::ORDERED_RELAY_REPLY_TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(lifecycle.state(), SocketCleanupState::Retired);
    assert!(incumbent.deferred_inbound.is_empty());
    assert!(
        lifecycle.entry.is_carbons_enabled(),
        "fresh-bind shutdown processes parked input before terminal cleanup"
    );
    assert!(state
        .deps
        .protocol
        .sm_session_registry
        .peek_session(stream_id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        room.ask(GetSnapshot)
            .await
            .unwrap()
            .room
            .session_generation(&jid),
        None
    );

    let (outcome, mut successor, mut successor_rx, _) =
        bind_fresh_test_connection(state.clone(), jid.clone()).await;
    assert!(matches!(outcome, RegistrationAfterFrame::Registered(_)));
    assert_ne!(successor.occupancy_session, incumbent.occupancy_session);
    assert!(
        !successor.sm_state.enabled,
        "fresh binding must not resume the old stream"
    );
    room.ask(JoinWithAffiliation {
        sender_jid: jid.clone(),
        nick: "alice".to_owned(),
        affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp_core::Affiliation::Member),
        local_domain: "example.com".to_owned(),
        admission_revision: room.ask(GetSnapshot).await.unwrap().admission_revision,
        session: successor.occupancy_session,
    })
    .await
    .unwrap();
    finish_test_socket_cleanup(&state, &mut incumbent, &mut outbound_rx).await;
    assert_eq!(
        bridge
            .force_detach_remote_user_resource_on_socket(request)
            .await
            .status,
        RelayRemoteResourceForceDetachStatus::Detached
    );
    assert!(state
        .deps
        .protocol
        .connection_registry
        .is_owned_by(&jid, successor.registry_owner.as_ref().unwrap()));
    assert!(!successor.write_interruption.is_cancelled());
    assert_eq!(
        room.ask(GetSnapshot)
            .await
            .unwrap()
            .room
            .session_generation(&jid),
        Some(successor.occupancy_session)
    );
    finish_test_socket_cleanup(&state, &mut successor, &mut successor_rx).await;
}
