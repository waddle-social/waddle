use super::{
    relay_name, Arc, CancellationToken, Duration, Entity, EntityType, NodeId,
    OrderedRelayDeliveryBridge, OrderedRelayReply, OrderedRelaySequence, RelayActor,
    RelayDeliverOrdered,
};
use crate::clustering::ordered_relay::{
    OrderedRelayReceiverState, OrderedRelayReservation, ORDERED_RELAY_DIVERSION_COOLDOWN,
};
use crate::clustering::route_bridge::tests::{
    origin_identity, receiver_identity, services_with_claims,
};
use crate::server::routes::interpret::{
    FullJidDeliveryOutcome, OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind,
};
use futures::StreamExt;
use kameo::remote;
use kameo::remote::messaging::{SwarmRequest, SwarmResponse};
use kameo::remote::registry::ActorRegistration;
use libp2p::swarm::SwarmEvent;
use libp2p::{noise, request_response, tcp, yamux, StreamProtocol, SwarmBuilder};
use std::borrow::Cow;

/// Exercise the public messaging transport and ordered-delivery boundary
/// together: membership eviction is not evidence that a sent request never ran.
#[tokio::test]
async fn evicted_peer_keeps_committed_ordered_send_ambiguous() {
    let _trace = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_test_writer()
            .finish(),
    );
    let mut sender = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("sender transport")
        .with_behaviour(|key| {
            remote::Behaviour::new(
                key.public().to_peer_id(),
                remote::messaging::Config::default(),
            )
        })
        .expect("sender behaviour")
        .build();
    sender.behaviour().init_global();
    let mut owner = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("owner transport")
        .with_behaviour(|_| {
            request_response::cbor::Behaviour::<SwarmRequest, SwarmResponse>::new(
                [(
                    StreamProtocol::new("/kameo/messaging/1.0.0"),
                    request_response::ProtocolSupport::Full,
                )],
                request_response::Config::default(),
            )
        })
        .expect("owner behaviour")
        .build();
    owner
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("listen address"))
        .expect("listen");
    let address = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = owner.select_next_some().await {
                break address;
            }
        }
    })
    .await
    .expect("owner listen address");
    let owner_peer = *owner.local_peer_id();
    sender.add_peer_address(owner_peer, address.clone());
    sender
        .dial(
            libp2p::swarm::dial_opts::DialOpts::peer_id(owner_peer)
                .addresses(vec![address.clone()])
                .build(),
        )
        .expect("dial owner");
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut sender_connected = false;
        let mut owner_connected = false;
        while !sender_connected || !owner_connected {
            tokio::select! {
                event = sender.select_next_some() => {
                    sender_connected |= matches!(event, SwarmEvent::ConnectionEstablished { peer_id, .. } if peer_id == owner_peer);
                },
                event = owner.select_next_some() => {
                    owner_connected |= matches!(event, SwarmEvent::ConnectionEstablished { peer_id, .. } if peer_id == *sender.local_peer_id());
                },
            }
        }
    })
    .await
    .expect("both peers observe the healthy connection before dispatch");
    let owner_identity = receiver_identity();
    let actor_id = kameo::actor::ActorId::new_with_peer_id(1387, owner_peer);
    let relay_name = relay_name(&NodeId::new(owner_identity.node_id.clone()));
    sender
        .behaviour_mut()
        .registry
        .register(
            relay_name.clone(),
            ActorRegistration::new(
                actor_id,
                Cow::Borrowed(<RelayActor as remote::RemoteActor>::REMOTE_ID),
            ),
        )
        .expect("register remote owner fixture");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if sender
                .behaviour_mut()
                .registry
                .lookup_local(&relay_name)
                .expect("local registry")
                .is_some()
            {
                break;
            }
            tokio::select! {
                _ = sender.select_next_some() => {},
                _ = owner.select_next_some() => {},
            }
        }
    })
    .await
    .expect("relay registration published");

    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            owner_identity,
            origin_identity(),
            libp2p::identity::Keypair::generate_ed25519()
                .public()
                .to_peer_id()
                .to_string(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    bridge.wire_origin_signer(libp2p::identity::Keypair::generate_ed25519());
    bridge.wire(services);
    let target: jid::BareJid = "juliet@example.test".parse().expect("target");
    let mut message = xmpp_parsers::message::Message::new(Some(target.clone().into()));
    message.from = Some("romeo@example.test/home".parse().expect("sender"));
    message
        .bodies
        .insert(Default::default(), "committed before peer departure".into());
    let stanza = waddle_xmpp::Stanza::Message(message);
    let mut origin = OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::SmSession(
            waddle_xmpp::pending_delivery::SmSessionId::new("stream-1"),
        ),
        sender_entity: Entity::new(EntityType::UserActor, "romeo@example.test"),
        inbound_sequence: 1,
        handoff: None,
    };
    let mut receiver = OrderedRelayReceiverState::default();
    let mut held_reply = None;
    let outcome = {
        let first = bridge.try_deliver_bare_jid_remote(&target, &stanza, &origin);
        tokio::pin!(first);
        tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut first => break result,
                _ = sender.select_next_some() => {},
                event = owner.select_next_some() => {
                    if let SwarmEvent::Behaviour(request_response::Event::Message {
                        message: request_response::Message::Request {request: SwarmRequest::Ask {payload, ..}, channel, ..}, ..
                    }) = event {
                        let request: RelayDeliverOrdered = rmp_serde::from_slice(&payload).expect("typed ordered request");
                        assert_eq!(request.envelope.sequence, OrderedRelaySequence::FIRST);
                        let OrderedRelayReservation::Reserved(reserved) = receiver.reserve(request.envelope) else {
                            panic!("first remote effect must reserve");
                        };
                        assert!(matches!(receiver.commit_reserved(*reserved), OrderedRelayReply::Ack(_)));
                        held_reply = Some(channel);
                        // The handler has committed, while its reply is still held.
                        sender.behaviour_mut().messaging.remove_peer(&owner_peer);
                    }
                },
            }
        }
    }).await.expect("eviction resolves the in-flight ask")
    };
    assert!(
        held_reply.is_some(),
        "remote handler committed before eviction; early outcome: {outcome:?}"
    );
    assert_eq!(
        outcome,
        Some(FullJidDeliveryOutcome::MaybeCommitted),
        "an executed request must forbid local or carbon fallback after eviction"
    );

    // A fresh membership admission can resume the channel after its ambiguity
    // cooldown. The committed first sequence must never become available again.
    tokio::time::pause();
    tokio::time::advance(ORDERED_RELAY_DIVERSION_COOLDOWN + Duration::from_millis(10)).await;
    tokio::time::resume();
    sender.behaviour_mut().messaging.allow_peer(&owner_peer);
    sender.add_peer_address(owner_peer, address);
    origin.inbound_sequence = 2;
    let second = bridge.try_deliver_bare_jid_remote(&target, &stanza, &origin);
    tokio::pin!(second);
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut second => break result,
                _ = sender.select_next_some() => {},
                event = owner.select_next_some() => {
                    if let SwarmEvent::Behaviour(request_response::Event::Message {
                        message: request_response::Message::Request {request: SwarmRequest::Ask {payload, ..}, channel, ..}, ..
                    }) = event {
                        let request: RelayDeliverOrdered = rmp_serde::from_slice(&payload).expect("typed ordered request");
                        assert_eq!(request.envelope.sequence, OrderedRelaySequence(2), "eviction cannot roll back a remotely committed sequence");
                        let OrderedRelayReservation::Reserved(reserved) = receiver.reserve(request.envelope) else {
                            panic!("second remote effect must reserve the successor sequence");
                        };
                        let reply = receiver.commit_reserved(*reserved);
                        owner.behaviour_mut().send_response(channel, SwarmResponse::Ask(Ok(rmp_serde::to_vec_named(&reply).expect("typed ordered reply")))).expect("reply channel open");
                    }
                },
            }
        }
    }).await.expect("readmitted peer accepts the successor envelope");
    assert_eq!(outcome, Some(FullJidDeliveryOutcome::Delivered));
    drop(held_reply);
}
