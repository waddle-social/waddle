//! Timestamp-only relay metadata preserves native SM custody without ingress authority.
use super::*;
use crate::clustering::ordered_relay::{
    OrderedRelayAck, OrderedRelayReceiverState, OrderedRelayReservation,
};
use chrono::{DateTime, Utc};
use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

pub(crate) fn received_at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 123_456_000).expect("original receipt time")
}

pub(crate) fn transient_message() -> Stanza {
    let mut message = Message::new(Some(target_full().into()));
    message.from = Some(sender_full().into());
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Lang::new(), "transient remote copy".into());
    waddle_xmpp::xep::xep0334::add_hint(&mut message, waddle_xmpp::xep::xep0334::Hint::NoStore);
    Stanza::Message(message)
}

pub(crate) async fn detach_target(services: &OrderedRelayDeliveryServices, target: &jid::FullJid) {
    services.connection_registry.unregister(target);
    services
        .user_registry
        .ask(waddle_xmpp::registry::UnregisterUserResource {
            jid: target.clone(),
            owner: None,
        })
        .await
        .expect("unregister departing target");
    services
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: target.to_string(),
            user_id: target.to_bare().to_string(),
            jid: target.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("retain detached target's accepted SM session");
}

pub(crate) async fn assert_native_replay_time(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    timestamp: DateTime<Utc>,
) {
    let session = services
        .sm_session_registry
        .peek_session(&target.to_string())
        .await
        .expect("read native SM buffer")
        .expect("detached SM session");
    assert_eq!(session.outbound_count, 1);
    assert_eq!(session.unacked_stanzas.len(), 1);
    let queued = &session.unacked_stanzas[0];
    assert_eq!(queued.original_receipt_at, timestamp);
    assert!(
        queued.ingress_receipts.is_empty(),
        "timestamp metadata must not manufacture ingress custody"
    );
    let replay = waddle_xmpp::stream_management::stamp_replay_delay(
        &queued.stanza_xml,
        "example.com",
        queued.original_receipt_at,
    );
    let replay = waddle_xmpp::parser::message_from_string(&replay).expect("native replay message");
    // The existing XEP-0203 builder emits seconds; native custody above keeps
    // the complete timestamp. Neither boundary may substitute relay time.
    let delay =
        waddle_xmpp::xep::xep0203::extract_delay_stamp(&replay).expect("XEP-0203 replay delay");
    assert_eq!(delay.timestamp(), timestamp.timestamp());
    assert_eq!(
        replay.bodies.get(&Lang::new()).map(String::as_str),
        Some("transient remote copy")
    );
}

pub(crate) struct RemoteOwnerFixture {
    pub bridge: Arc<OrderedRelayDeliveryBridge>,
    pub services: Arc<OrderedRelayDeliveryServices>,
    pub message: RelayRouteRemoteResourceStanza,
    _source_receiver: mpsc::Receiver<OutboundStanza>,
}

pub(crate) async fn register_live_target(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
) -> (Arc<AtomicBool>, mpsc::Receiver<OutboundStanza>) {
    let (sender, receiver) = mpsc::channel(4);
    let entry = ConnectionEntry::new(sender);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry,
        })
        .await
        .expect("publish live target before relay capture");
    (owner, receiver)
}

/// Cross the real wire DTO and actor handler after capture, then let the
/// authenticated sender owner find the original target already detached.
pub(crate) async fn remote_owner_fixture() -> RemoteOwnerFixture {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(services.clone());
    let source = sender_full();
    let (sender, source_receiver) = mpsc::channel(1);
    let entry = ConnectionEntry::new(sender);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(source.clone(), entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: source.clone(),
            entry,
        })
        .await
        .expect("register authenticated source mirror");
    let registration_id = RemoteResourceRegistrationId::fresh();
    let socket_generation = RemoteResourceSocketGeneration::next(None);
    bridge.remote_owner_resources.lock().await.insert(
        source.clone(),
        RemoteOwnerRegistration {
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            socket_identity: NodeIdentity::new("fixture-socket", "fixture-epoch"),
            unregister_pending: false,
            registration_id,
            socket_generation,
            socket_node: NodeId::new("source-socket-node".into()),
            owner,
        },
    );
    let (_, _target_receiver) = register_live_target(&services, &target_full()).await;
    let message = RelayRouteRemoteResourceStanza {
        source_jid: source,
        registration_id,
        socket_generation,
        target: RemoteResourceRouteTarget::FullJid {
            target: target_full(),
            stanza: RemoteStanza(transient_message()),
            ingress_append: None,
            received_at: Some(received_at()),
        },
        trace: RelayTraceContext::default(),
    };
    detach_target(&services, &target_full()).await;
    RemoteOwnerFixture {
        bridge,
        services,
        message,
        _source_receiver: source_receiver,
    }
}

#[tokio::test]
async fn xep0198_ordered_timestamp_crosses_wire_into_native_sm_and_xep0203_delay() {
    let keypair = Keypair::generate_ed25519();
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(services.clone());
    let (_, _target_receiver) = register_live_target(&services, &target_full()).await;
    let mut envelope = envelope_for_services(&services).await;
    envelope.payload = OrderedRelayPayload::Message {
        recipient: target_full().into(),
        stanza: RemoteStanza(transient_message()),
        ingress_append: None,
        received_at: Some(received_at()),
    };
    let envelope = sign_envelope(envelope, &keypair);
    let encoded = rmp_serde::to_vec_named(&envelope).expect("encode ordered wire envelope");
    let decoded: RemoteStanzaEnvelope =
        rmp_serde::from_slice(&encoded).expect("decode ordered wire envelope");
    assert_eq!(
        decoded.signing_bytes().unwrap(),
        envelope.signing_bytes().unwrap()
    );
    detach_target(&services, &target_full()).await;

    let mut changed = decoded.clone();
    let OrderedRelayPayload::Message {
        received_at: timestamp,
        ..
    } = &mut changed.payload
    else {
        panic!("message envelope")
    };
    *timestamp = Some(received_at() + chrono::Duration::seconds(1));
    let signature = &decoded
        .origin_proof
        .as_ref()
        .expect("signed envelope")
        .signature;
    assert!(!keypair
        .public()
        .verify(&changed.signing_bytes().unwrap(), signature));
    assert!(
        bridge.deliver_reserved(&changed, &mut None).await.is_err(),
        "tampered timestamp cannot cross origin authentication"
    );

    let mut receiver = OrderedRelayReceiverState::default();
    let OrderedRelayReservation::Reserved(reserved) = receiver.reserve(decoded.clone()) else {
        panic!("reserve original")
    };
    assert_timestamp_conflict(&mut receiver, changed.clone());
    bridge
        .deliver_reserved(&decoded, &mut None)
        .await
        .expect("accepted ordered receiver SM append");
    assert!(matches!(
        receiver.commit_reserved(*reserved),
        OrderedRelayReply::Ack(_)
    ));
    assert_timestamp_conflict(&mut receiver, changed);
    assert!(matches!(
        receiver.reserve(decoded),
        OrderedRelayReservation::Completed(OrderedRelayReply::Ack(OrderedRelayAck {
            duplicate: true,
            ..
        }))
    ));
    assert_native_replay_time(&services, &target_full(), received_at()).await;
}

fn assert_timestamp_conflict(
    receiver: &mut OrderedRelayReceiverState,
    envelope: RemoteStanzaEnvelope,
) {
    assert!(
        matches!(
            receiver.reserve(envelope),
            OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            }))
        ),
        "same sequence with a changed receipt timestamp must not acquire an effect reservation"
    );
}

/// Keep the regression on the exact DTO builder used by the owner forwarder.
pub(crate) fn owner_forwarded_frame(
    target: &jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    outbound: OutboundStanza,
) -> RemoteResourceOutboundFrame {
    super::super::registration::remote_resource_outbound_frame(target, registration_id, &outbound)
}
