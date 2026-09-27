use super::*;
use crate::clustering::{
    relay::RelayRemoteResourceRegistrationStatus,
    route_bridge::{
        remote_registration_request, remote_registration_request_from_entry, wire_for_test,
        OrderedRelayDeliveryBridge,
    },
    NodeId,
};
use tokio::time::{timeout, Duration};
use waddle_xmpp::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::registry::{ForceDetachOrigin, SocketCleanupState};

async fn owner_bridge(
    state: &Arc<super::super::super::WebSocketState>,
    jid: &FullJid,
) -> Arc<OrderedRelayDeliveryBridge> {
    let identity = NodeIdentity::new("owner", "owner-epoch");
    let claims = Arc::new(InProcessClaimStore::new());
    claims
        .acquire(
            &Entity::new(EntityType::UserActor, jid.to_bare().to_string()),
            &identity,
        )
        .await
        .expect("owner claim");
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    wire_for_test(&bridge, state, claims, SharedNodeIdentity::new(identity)).await;
    bridge
}

async fn finish_requested_cleanup(
    state: &super::super::super::WebSocketState,
    conn: &mut WsConnState,
    rx: &mut mpsc::Receiver<OutboundStanza>,
    origin: ForceDetachOrigin,
) {
    let outcome = super::super::super::cleanup::cleanup_force_detach_connection_shutdown(
        state, rx, conn, false, origin,
    )
    .await;
    super::super::super::cleanup::finish_socket_cleanup(state, conn, outcome).await;
}

async fn assert_remote_replaces_owner_local(remove_route: bool, timeout_first: bool) {
    use waddle_xmpp::muc::room_actor::{GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation};
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;

    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/owner-local".parse().expect("JID");
    let (_, mut incumbent, mut rx, _) =
        bind_fresh_test_connection(state.clone(), jid.clone()).await;
    let lifecycle = incumbent
        .socket_lifecycle
        .as_ref()
        .expect("lifecycle")
        .clone();
    let owner = incumbent.registry_owner.clone().expect("owner");
    let mut control = lifecycle.entry.take_force_detach_rx().expect("control");
    let room = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: "owner-local@muc.example.com".parse().expect("room JID"),
            waddle_id: "owner-local".to_owned(),
            channel_id: "room".to_owned(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("room");
    let sibling: FullJid = "alice@example.com/sibling".parse().expect("sibling JID");
    let sibling_generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
    for (sender, generation) in [
        (&jid, incumbent.occupancy_session),
        (&sibling, sibling_generation),
    ] {
        room.ask(JoinWithAffiliation {
            sender_jid: sender.clone(),
            nick: "alice".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(
                waddle_xmpp_core::Affiliation::Member,
            ),
            local_domain: "example.com".to_owned(),
            admission_revision: room
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .admission_revision,
            session: generation,
        })
        .await
        .expect("join");
    }
    let bridge = owner_bridge(&state, &jid).await;
    let replacement = remote_registration_request(
        &bridge,
        jid.clone(),
        NodeId::new("remote-socket".to_owned()),
    )
    .await;
    if remove_route {
        state
            .deps
            .protocol
            .connection_registry
            .unregister_if_owner(&jid, &owner);
    }
    assert_eq!(
        timeout(
            Duration::from_secs(1),
            bridge.register_remote_user_resource_on_owner(replacement.clone())
        )
        .await
        .expect("registration must not wait for cleanup")
        .status,
        RelayRemoteResourceRegistrationStatus::Busy,
    );
    let request = timeout(Duration::from_secs(1), control.recv())
        .await
        .expect("detach requested")
        .expect("request");
    assert_eq!(request.origin, ForceDetachOrigin::FreshBindReplacement);
    assert_eq!(lifecycle.state(), SocketCleanupState::Running);
    assert_eq!(
        bridge
            .register_remote_user_resource_on_owner(replacement.clone())
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::Busy
    );

    if timeout_first {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(11)).await;
        let guard = state
            .deps
            .protocol
            .connection_registry
            .lock_bind(&jid)
            .await;
        assert!(
            guard
                .pending_retirements()
                .contains(&incumbent.occupancy_session),
            "timeout retains the exact cleanup obligation after route removal"
        );
        drop(guard);
        tokio::time::resume();
        assert_eq!(
            bridge
                .register_remote_user_resource_on_owner(replacement.clone())
                .await
                .status,
            RelayRemoteResourceRegistrationStatus::Busy
        );
        timeout(Duration::from_secs(1), control.recv())
            .await
            .expect("retry detach")
            .expect("request");
    }

    // The originating relay request has already returned. Its independently
    // owned retirement must still complete before the mirror can publish.
    finish_requested_cleanup(&state, &mut incumbent, &mut rx, request.origin).await;
    let guard = timeout(
        Duration::from_secs(5),
        state.deps.protocol.connection_registry.lock_bind(&jid),
    )
    .await
    .expect("background retirement finishes");
    assert!(guard.pending_retirements().is_empty());
    drop(guard);
    assert_eq!(
        bridge
            .register_remote_user_resource_on_owner(replacement.clone())
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::Registered
    );
    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("remote mirror");
    assert!(!entry.is_locally_hosted());
    assert_eq!(
        entry.occupancy_session(),
        replacement.state.occupancy_session
    );
    assert!(!state
        .deps
        .protocol
        .connection_registry
        .is_owned_by(&jid, &owner));
    assert_eq!(lifecycle.state(), SocketCleanupState::Retired);
    let snapshot = room.ask(GetSnapshot).await.expect("retired occupancy");
    assert_eq!(snapshot.room.session_generation(&jid), None);
    assert_eq!(
        snapshot.room.session_generation(&sibling),
        Some(sibling_generation)
    );
}

#[tokio::test]
async fn remote_fresh_bind_retires_owner_local_incumbent() {
    assert_remote_replaces_owner_local(false, false).await;
}

#[tokio::test]
async fn remote_fresh_bind_waits_for_owner_local_cleanup_after_route_removal() {
    assert_remote_replaces_owner_local(true, false).await;
}

#[tokio::test]
async fn remote_fresh_bind_retries_timed_out_owner_local_retirement() {
    assert_remote_replaces_owner_local(true, true).await;
}

#[tokio::test]
async fn remote_registration_never_waits_on_owner_local_bind_gate() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/bind-gate".parse().expect("JID");
    let bridge = owner_bridge(&state, &jid).await;
    let request =
        remote_registration_request(&bridge, jid.clone(), NodeId::new("remote".to_owned())).await;
    let guard = state
        .deps
        .protocol
        .connection_registry
        .lock_bind(&jid)
        .await;
    assert_eq!(
        timeout(
            Duration::from_secs(1),
            bridge.register_remote_user_resource_on_owner(request.clone())
        )
        .await
        .expect("remote registration must never invert the local bind lock order")
        .status,
        RelayRemoteResourceRegistrationStatus::Busy
    );
    drop(guard);
    assert_eq!(
        bridge
            .register_remote_user_resource_on_owner(request)
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::Registered
    );
}

#[tokio::test]
async fn remote_registration_refuses_untracked_owner_local_connection() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/untracked".parse().expect("JID");
    let (tx, _rx) = mpsc::channel(1);
    let owner = super::super::register_test_connection(&state, &jid, tx).await;
    let bridge = owner_bridge(&state, &jid).await;
    let request =
        remote_registration_request(&bridge, jid.clone(), NodeId::new("remote".to_owned())).await;
    assert_eq!(
        bridge
            .register_remote_user_resource_on_owner(request)
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::StaleRegistration
    );
    assert!(
        state
            .deps
            .protocol
            .connection_registry
            .is_owned_by(&jid, &owner),
        "a missing lifecycle cannot authorize overwriting a live local route"
    );
}

#[tokio::test]
async fn remote_same_generation_resume_preserves_owner_local_sm_snapshot() {
    use waddle_xmpp::stream_management::SmSessionRegistry;

    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/owner-resume".parse().expect("JID");
    let (_, mut incumbent, mut rx, _) =
        bind_fresh_test_connection(state.clone(), jid.clone()).await;
    let session = create_test_session(&state, "alice").await;
    incumbent.authenticated_session = Some(session.clone());
    let enabled = handle_xmpp_frame(
        &element_to_xml(
            Element::builder("enable", SM_NS)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .build(),
        ),
        "example.com",
        &state,
        &mut incumbent,
    )
    .await;
    let enabled: Element = enabled[0].parse().expect("enabled frame");
    let stream_id = enabled.attr("id").expect("stream id").to_owned();
    incumbent.publish_pending_sm_enable(&state);
    let lifecycle = incumbent
        .socket_lifecycle
        .as_ref()
        .expect("lifecycle")
        .clone();
    let mut control = lifecycle.entry.take_force_detach_rx().expect("control");
    let bridge = owner_bridge(&state, &jid).await;
    let request = remote_registration_request_from_entry(
        jid.clone(),
        NodeId::new("resume-socket".to_owned()),
        &lifecycle.entry,
    );
    assert_eq!(
        bridge
            .register_remote_user_resource_on_owner(request.clone())
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::Busy
    );
    let detach = timeout(Duration::from_secs(1), control.recv())
        .await
        .expect("detach request")
        .expect("request");
    assert_eq!(detach.origin, ForceDetachOrigin::CrossNodeResume);
    finish_requested_cleanup(&state, &mut incumbent, &mut rx, detach.origin).await;
    let guard = timeout(
        Duration::from_secs(5),
        state.deps.protocol.connection_registry.lock_bind(&jid),
    )
    .await
    .expect("background detach finishes");
    assert!(guard.pending_retirements().is_empty());
    drop(guard);
    let snapshot = state
        .deps
        .protocol
        .sm_session_registry
        .peek_session(&stream_id)
        .await
        .expect("snapshot lookup")
        .expect("resumable snapshot preserved");
    assert_eq!(snapshot.occupancy_session, incumbent.occupancy_session);
    assert_eq!(lifecycle.state(), SocketCleanupState::Detached);

    // Exercise the XEP-0198 resume parser/claim path with the retained snapshot.
    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&jid);
    resumed.authenticated_session = Some(session);
    handle_xmpp_frame(
        &element_to_xml(
            Element::builder("resume", SM_NS)
                .attr(
                    minidom::rxml::xml_ncname!("previd").to_owned(),
                    stream_id.as_str(),
                )
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                .build(),
        ),
        "example.com",
        &state,
        &mut resumed,
    )
    .await;
    assert_eq!(
        resumed.pending_resume_stream_id.as_deref(),
        Some(stream_id.as_str())
    );
    assert_eq!(resumed.occupancy_session, incumbent.occupancy_session);
    assert_eq!(
        bridge
            .register_remote_user_resource_on_owner(request)
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::Registered
    );
}
