//! Live relay wire metadata must survive the actual receiver and detach drain.
use super::*;
use crate::clustering::route_bridge::tests::{
    origin_identity, receiver_identity, services_with_claims,
    transient_timestamp::{
        assert_native_replay_time, detach_target, received_at, register_live_target,
        remote_owner_fixture, transient_message,
    },
};
use kameo::actor::Spawn;
use waddle_xmpp::registry::DeliveryKind;
use waddle_xmpp::Stanza;

#[tokio::test]
async fn xep0198_live_route_wire_preserves_original_timestamp_when_target_detached() {
    let fixture = remote_owner_fixture().await;
    let live = LiveRoute::from_current(&fixture.message).expect("timestamp-only live route");
    let decoded: LiveRoute = rmp_serde::from_slice(&encode(&live)).expect("live route wire codec");
    let actor = RelayActor::spawn(RelayActor::new(
        SharedNodeIdentity::new(receiver_identity()),
        false,
        ResumeStealBridge::new(),
        RoomLocalClaims::new(),
        fixture.bridge.clone(),
    ));
    let reply: RelayRouteRemoteResourceStanzaReply = actor
        .ask(decoded)
        .await
        .expect("live route receiver")
        .into();
    assert_eq!(reply.outcome, RemoteResourceRouteOutcome::QueuedDetached);
    let RemoteResourceRouteTarget::FullJid { target, .. } = &fixture.message.target else {
        panic!("full target")
    };
    assert_native_replay_time(&fixture.services, target, received_at()).await;
    actor.stop_gracefully().await.expect("stop relay actor");
}

#[tokio::test]
async fn xep0198_live_frame_wire_and_actual_detach_drain_preserve_original_timestamp() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            libp2p::PeerId::random().to_string(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    bridge.wire(services.clone());
    let Stanza::Message(message) = transient_message() else {
        panic!("message")
    };
    let target = message
        .to
        .as_ref()
        .expect("recipient")
        .try_as_full()
        .expect("full recipient")
        .clone();
    let (owner, mut receiver) = register_live_target(&services, &target).await;
    let registration_id = bridge
        .test_insert_remote_socket_registration(
            target.clone(),
            owner,
            NodeId::new("remote-user-owner".into()),
        )
        .await;
    let frame = RelayDeliverRemoteResourceFrame {
        frame: RemoteResourceOutboundFrame {
            jid: target.clone(),
            registration_id,
            stanza: RemoteStanza(Stanza::Message(message)),
            kind: DeliveryKind::DirectFrame,
            ingress_append: None,
            received_at: Some(received_at()),
        },
        trace: RelayTraceContext::default(),
    };
    let live = LiveFrame::from_current(&frame).expect("timestamp-only live frame");
    let decoded: LiveFrame = rmp_serde::from_slice(&encode(&live)).expect("live frame wire codec");
    let actor = RelayActor::spawn(RelayActor::new(
        SharedNodeIdentity::new(receiver_identity()),
        false,
        ResumeStealBridge::new(),
        RoomLocalClaims::new(),
        bridge,
    ));
    let reply: RelayRemoteResourceFrameReply = actor
        .ask(decoded)
        .await
        .expect("live frame receiver")
        .into();
    assert_eq!(reply.status, RelayRemoteResourceFrameStatus::Delivered);
    // The relay enqueue succeeded; the socket now detaches before writing it.
    detach_target(&services, &target).await;
    crate::server::routes::websocket::tests::relay_timestamp_drain::drain_registered_remote_frame(
        services.sm_session_registry.clone(),
        &target,
        &mut receiver,
    )
    .await;
    assert_native_replay_time(&services, &target, received_at()).await;
    actor.stop_gracefully().await.expect("stop relay actor");
}

#[test]
fn untimestamped_route_and_frame_keep_frozen_baseline_bytes() {
    for message in routes() {
        let baseline = RouteV8::from(message.clone());
        assert_eq!(encode(&baseline), encode(&message));
        let restored = RelayRouteRemoteResourceStanza::from(baseline);
        if let RemoteResourceRouteTarget::FullJid { received_at, .. } = restored.target {
            assert!(received_at.is_none());
        }
    }
    for message in frames() {
        let baseline = FrameV3::from(message.clone());
        assert_eq!(encode(&baseline), encode(&message));
        assert!(RelayDeliverRemoteResourceFrame::from(baseline)
            .frame
            .received_at
            .is_none());
    }
}

#[tokio::test]
async fn timestamped_live_requests_never_downgrade_to_legacy_payloads() {
    let baseline_effects = AtomicUsize::new(0);
    let result = live_or_baseline(false, async { Err::<(), _>(unknown()) }, async {
        baseline_effects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await;
    assert!(matches!(
        result,
        Err(RemoteSendError::UnknownMessage { .. })
    ));
    assert_eq!(
        baseline_effects.load(Ordering::SeqCst),
        0,
        "a legacy route/frame cannot preserve the required receipt timestamp"
    );
}

#[test]
fn owner_forwarder_preserves_keyed_baseline_and_unkeyed_live_timestamp_selection() {
    use crate::clustering::route_bridge::tests::transient_timestamp::owner_forwarded_frame;
    use waddle_xmpp::registry::OutboundStanza;
    let template = frames().remove(0);
    for key in [None, Some(obligation())] {
        let mut outbound = OutboundStanza::new(template.frame.stanza.0.clone());
        outbound.kind = template.frame.kind;
        // A queued keyed frame also has in-process timestamp metadata. This
        // deliberately disagrees with the key: only its authoritative time may
        // cross the baseline DTO, never an extra timestamp-only wire field.
        outbound.original_receipt_at = Some(received_at());
        outbound.ingress_append = key
            .clone()
            .map(|key| key.into_relayed_for(template.frame.jid.clone()));
        let message = RelayDeliverRemoteResourceFrame {
            frame: owner_forwarded_frame(
                &template.frame.jid,
                template.frame.registration_id,
                outbound,
            ),
            trace: RelayTraceContext::default(),
        };
        assert_eq!(message.frame.ingress_append, key);
        if let Some(key) = key {
            assert!(message.frame.received_at.is_none());
            assert!(LiveFrame::from_current(&message).is_none());
            let encoded = encode(&FrameV3::from(message));
            let decoded: FrameV3 = rmp_serde::from_slice(&encoded).expect("keyed baseline codec");
            let current = RelayDeliverRemoteResourceFrame::from(decoded);
            assert_eq!(current.frame.ingress_append, Some(key));
            assert!(current.frame.received_at.is_none());
        } else {
            let live =
                LiveFrame::from_current(&message).expect("unkeyed timestamp uses live endpoint");
            let decoded: LiveFrame =
                rmp_serde::from_slice(&encode(&live)).expect("unkeyed live codec");
            let current = RelayDeliverRemoteResourceFrame::from(decoded);
            assert_eq!(current.frame.received_at, Some(received_at()));
            assert!(current.frame.ingress_append.is_none());
        }
    }
}
