use super::*;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};

fn unknown() -> RemoteSendError<Infallible> {
    RemoteSendError::UnknownMessage {
        actor_remote_id: "waddle.clustering.relay".into(),
        message_remote_id: "unsupported".into(),
    }
}

#[tokio::test]
async fn unknown_live_endpoint_attempts_baseline_once() {
    let effects = AtomicUsize::new(0);
    let result = live_or_baseline(async { Err::<(), _>(unknown()) }, async {
        effects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await;
    assert!(result.is_ok());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn supported_live_endpoint_never_dispatches_baseline() {
    let effects = AtomicUsize::new(0);
    live_or_baseline(
        async {
            effects.fetch_add(1, Ordering::SeqCst);
            Ok::<_, RemoteSendError<Infallible>>(())
        },
        async {
            effects.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    )
    .await
    .expect("live result");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ambiguous_reply_or_transport_failures_do_not_repeat_effects() {
    for error in [
        RemoteSendError::DeserializeMessage("reply decoding failed".into()),
        RemoteSendError::SerializeReply("reply encoding failed".into()),
        RemoteSendError::ReplyTimeout,
        RemoteSendError::ActorStopped,
        RemoteSendError::ConnectionClosed,
        RemoteSendError::NetworkTimeout,
    ] {
        let effects = AtomicUsize::new(0);
        let result = live_or_baseline(
            async {
                effects.fetch_add(1, Ordering::SeqCst);
                Err::<(), RemoteSendError<Infallible>>(error)
            },
            async {
                effects.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(effects.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn unsupported_baseline_is_bounded_and_remains_no_effect() {
    let attempts = AtomicUsize::new(0);
    let result = live_or_baseline(
        async {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(unknown())
        },
        async {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err(unknown())
        },
    )
    .await
    .expect_err("both versions unsupported");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(classify_effect(&result), RelaySendEffect::NoEffect);
}

#[tokio::test]
async fn cancellation_drops_pending_ask_without_a_late_fallback() {
    let effects = AtomicUsize::new(0);
    let stop = CancellationToken::new();
    let attempt = live_or_baseline(
        async {
            stop.cancel();
            std::future::pending::<Result<(), RemoteSendError<Infallible>>>().await
        },
        async {
            effects.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    );
    tokio::select! {
        biased;
        _ = stop.cancelled() => {},
        _ = attempt => panic!("pending ask completed"),
    }
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

fn stanzas() -> Vec<RemoteStanza> {
    // Inputs at the XML parsing boundary cover the non-obligation traffic which
    // used to be lost on a wire bump: IQ, presence, roster/blocklist pushes and
    // MUC destruction. The relay itself always receives typed stanzas.
    [
        "<iq xmlns='jabber:client' type='get' id='query' from='a@example.test/phone' to='b@example.test/tablet'><query xmlns='jabber:iq:version'/></iq>",
        "<presence xmlns='jabber:client' from='a@example.test/phone' to='room@example.test/nick'/>",
        "<iq xmlns='jabber:client' type='set' id='roster'><query xmlns='jabber:iq:roster' ver='3'><item jid='b@example.test' subscription='both'/></query></iq>",
        "<iq xmlns='jabber:client' type='set' id='block'><block xmlns='urn:xmpp:blocking'><item jid='b@example.test'/></block></iq>",
        "<presence xmlns='jabber:client' type='unavailable' from='room@example.test/nick'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='none' role='none'/><destroy><reason>Closed</reason></destroy></x></presence>",
        "<message xmlns='jabber:client' type='chat' id='message' from='a@example.test/phone' to='b@example.test/tablet'><body>Hello</body></message>",
    ].into_iter().map(|xml| RemoteStanza(crate::clustering::codec::decode_stanza(xml).expect("fixture stanza"))).collect()
}

fn obligation() -> IngressAppendObligationRef {
    use waddle_xmpp::ingress::{IngressEffectKind, MessageKey};
    IngressAppendObligationRef {
        message_key: MessageKey::from_storage(uuid::Uuid::from_u128(1804)),
        sender_bare: "a@example.test".parse().expect("sender"),
        receipt: crate::ingress::EffectReceiptKey {
            kind: crate::ingress_substrate::EffectReceiptKind::from_storage(
                IngressEffectKind::RouteDirect.storage_tag(),
            ),
            semantic_identity_hash: [18; 32],
        },
        received_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
        archive_positions: vec![waddle_xmpp::stream_management::ArchiveDispatchPosition {
            archive: "a@example.test".parse().expect("archive"),
            ordinal: waddle_xmpp::mam::ArchiveOrdinal::FIRST,
        }],
        dispatch_stream: Some(waddle_xmpp::pending_delivery::SmSessionId::new("stream")),
    }
}

fn route(target: RemoteResourceRouteTarget) -> RelayRouteRemoteResourceStanza {
    RelayRouteRemoteResourceStanza {
        source_jid: "a@example.test/phone".parse().expect("source"),
        registration_id: serde_json::from_str("\"00000000-0000-0000-0000-000000001804\"")
            .expect("registration"),
        socket_generation: serde_json::from_str("3").expect("generation"),
        target,
        trace: RelayTraceContext::default(),
    }
}

fn routes() -> Vec<RelayRouteRemoteResourceStanza> {
    use waddle_xmpp::auth::{
        AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
    };
    let mut routes = Vec::new();
    for stanza in stanzas() {
        routes.push(route(RemoteResourceRouteTarget::FullJid {
            target: "b@example.test/tablet".parse().expect("target"),
            stanza: stanza.clone(),
            ingress_append: None,
        }));
        routes.push(route(RemoteResourceRouteTarget::BareJid {
            target: "b@example.test".parse().expect("target"),
            stanza,
        }));
    }
    for ingress_append in [None, Some(obligation())] {
        routes.push(route(RemoteResourceRouteTarget::ProcessedDirectMessage {
            target: "b@example.test/tablet".parse().expect("target"),
            stanza: stanzas().pop().expect("message"),
            ingress_append: ingress_append.clone(),
        }));
        routes.push(route(RemoteResourceRouteTarget::FullJid {
            target: "b@example.test/tablet".parse().expect("target"),
            stanza: stanzas().pop().expect("message"),
            ingress_append,
        }));
    }
    for kind in [
        OrderedRelayMucProxyKind::JoinPresence,
        OrderedRelayMucProxyKind::OccupantPresence,
        OrderedRelayMucProxyKind::GroupchatMessage,
        OrderedRelayMucProxyKind::PrivateMessage,
        OrderedRelayMucProxyKind::BareRoomIq,
        OrderedRelayMucProxyKind::OccupantIq,
        OrderedRelayMucProxyKind::FanoutChunk,
        OrderedRelayMucProxyKind::MujiJingleIq,
    ] {
        for admitted in [false, true] {
            routes.push(route(RemoteResourceRouteTarget::MucProxy {
                canonical: admitted.then(|| crate::ingress::IngressCanonicalRef {
                    message_key: obligation().message_key,
                    sender_bare: "a@example.test".parse().expect("sender"),
                    origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("origin")),
                }),
                principal: admitted.then(|| {
                    AuthenticatedPrincipalRef::new(
                        "a@example.test".parse().expect("principal"),
                        AuthContextId::new(uuid::Uuid::from_u128(1804)),
                        AuthContextVersion::INITIAL,
                        PrincipalAuthEpoch::INITIAL,
                    )
                }),
                stanza_lang: admitted.then(|| xmpp_parsers::message::Lang("en".into())),
                room_jid: "room@example.test".parse().expect("room"),
                kind,
                origin: if admitted {
                    MucProxyOrigin::Connection(
                        serde_json::from_str("\"00000000-0000-0000-0000-000000001804\"")
                            .expect("occupancy"),
                    )
                } else {
                    MucProxyOrigin::Server
                },
                stanza: match kind {
                    OrderedRelayMucProxyKind::JoinPresence
                    | OrderedRelayMucProxyKind::OccupantPresence => stanzas()[1].clone(),
                    OrderedRelayMucProxyKind::GroupchatMessage
                    | OrderedRelayMucProxyKind::PrivateMessage
                    | OrderedRelayMucProxyKind::FanoutChunk => stanzas()[5].clone(),
                    _ => stanzas()[0].clone(),
                },
            }));
        }
    }
    routes
}

fn frames() -> Vec<RelayDeliverRemoteResourceFrame> {
    let source = route(RemoteResourceRouteTarget::BareJid {
        target: "b@example.test".parse().expect("target"),
        stanza: stanzas()[0].clone(),
    });
    let mut frames = Vec::new();
    for stanza in stanzas() {
        for kind in [DeliveryKind::PeerStanza, DeliveryKind::DirectFrame] {
            for ingress_append in [None, Some(obligation())] {
                frames.push(RelayDeliverRemoteResourceFrame {
                    frame: RemoteResourceOutboundFrame {
                        jid: "b@example.test/tablet".parse().expect("target"),
                        registration_id: source.registration_id,
                        stanza: stanza.clone(),
                        kind,
                        ingress_append,
                    },
                    trace: RelayTraceContext::default(),
                });
            }
        }
    }
    frames
}

fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    rmp_serde::to_vec_named(value).expect("MessagePack encoding")
}

#[test]
fn frozen_baseline_accepts_pre_upgrade_requests_and_replies_in_both_directions() {
    for current in routes() {
        // The domain encoding here is the existing binary's v8 encoding. The
        // new receiver is a separate DTO, and both directions must agree.
        let received: RouteV8 =
            rmp_serde::from_slice(&encode(&current)).expect("old sender -> new receiver");
        assert_eq!(encode(&received), encode(&current));
        let new_sender = RouteV8::from(current.clone());
        let old_receiver: RelayRouteRemoteResourceStanza =
            rmp_serde::from_slice(&encode(&new_sender)).expect("new sender -> old receiver");
        assert_eq!(encode(&old_receiver), encode(&current));
        let domain: RelayRouteRemoteResourceStanza = received.into();
        assert_eq!(
            encode(&domain),
            encode(&current),
            "authority and obligation survive adapter"
        );
    }
    for current in frames() {
        let received: FrameV3 =
            rmp_serde::from_slice(&encode(&current)).expect("old frame -> new receiver");
        assert_eq!(encode(&received), encode(&current));
        let old_receiver: RelayDeliverRemoteResourceFrame =
            rmp_serde::from_slice(&encode(&FrameV3::from(current.clone())))
                .expect("new frame -> old receiver");
        assert_eq!(encode(&old_receiver), encode(&current));
        let domain: RelayDeliverRemoteResourceFrame = received.into();
        assert_eq!(encode(&domain), encode(&current));
    }
    for outcome in [
        RemoteResourceRouteOutcome::Delivered,
        RemoteResourceRouteOutcome::QueuedDetached,
        RemoteResourceRouteOutcome::Unavailable,
        RemoteResourceRouteOutcome::Dropped,
        RemoteResourceRouteOutcome::StaleRegistration,
        RemoteResourceRouteOutcome::MaybeCommitted,
        RemoteResourceRouteOutcome::JoinMaybeCommitted,
    ] {
        let reply = route_reply(outcome);
        let frozen: RouteReply = rmp_serde::from_slice(&encode(&reply)).expect("old reply decoded");
        assert_eq!(encode(&frozen), encode(&reply));
        let old: RelayRouteRemoteResourceStanzaReply =
            rmp_serde::from_slice(&encode(&RouteReply::from(reply.clone())))
                .expect("new reply decoded");
        assert_eq!(encode(&old), encode(&reply));
        assert_eq!(
            encode(&RelayRouteRemoteResourceStanzaReply::from(frozen)),
            encode(&reply)
        );
    }
}

fn route_reply(outcome: RemoteResourceRouteOutcome) -> RelayRouteRemoteResourceStanzaReply {
    RelayRouteRemoteResourceStanzaReply {
        reply_receipt: Some(
            serde_json::from_str("\"00000000-0000-0000-0000-000000001804\"")
                .expect("receipt token"),
        ),
        owner_receipts: vec![waddle_xmpp::stream_management::SmIngressFrameReceipt {
            message_key: obligation().message_key,
            kind: waddle_xmpp::stream_management::SmIngressReceiptKind::from_storage(1),
            semantic_identity_hash: [19; 32],
        }],
        outcome,
        replies: stanzas(),
    }
}

#[test]
fn live_adapters_preserve_authority_stanzas_and_reject_append_obligations() {
    for current in routes() {
        let eligible = match &current.target {
            RemoteResourceRouteTarget::ProcessedDirectMessage { .. } => false,
            RemoteResourceRouteTarget::FullJid { ingress_append, .. } => ingress_append.is_none(),
            _ => true,
        };
        let live = LiveRoute::from_current(&current);
        assert_eq!(live.is_some(), eligible);
        if let Some(live) = live {
            let decoded: LiveRoute =
                rmp_serde::from_slice(&encode(&live)).expect("stable live route");
            assert_eq!(
                encode(&RelayRouteRemoteResourceStanza::from(decoded)),
                encode(&current)
            );
        }
    }
    for current in frames() {
        let live = LiveFrame::from_current(&current);
        assert_eq!(live.is_some(), current.frame.ingress_append.is_none());
        if let Some(live) = live {
            let decoded: LiveFrame =
                rmp_serde::from_slice(&encode(&live)).expect("stable live frame");
            assert_eq!(
                encode(&RelayDeliverRemoteResourceFrame::from(decoded)),
                encode(&current)
            );
        }
    }
}

#[test]
fn complete_frozen_wire_corpus_has_not_changed() {
    use sha2::{Digest, Sha256};
    let mut corpus = Vec::new();
    for current in routes() {
        corpus.push(encode(&RouteV8::from(current.clone())));
        if let Some(live) = LiveRoute::from_current(&current) {
            corpus.push(encode(&live));
        }
    }
    for current in frames() {
        corpus.push(encode(&FrameV3::from(current.clone())));
        if let Some(live) = LiveFrame::from_current(&current) {
            corpus.push(encode(&live));
        }
    }
    for outcome in [
        RemoteResourceRouteOutcome::Delivered,
        RemoteResourceRouteOutcome::QueuedDetached,
        RemoteResourceRouteOutcome::Unavailable,
        RemoteResourceRouteOutcome::Dropped,
        RemoteResourceRouteOutcome::StaleRegistration,
        RemoteResourceRouteOutcome::MaybeCommitted,
        RemoteResourceRouteOutcome::JoinMaybeCommitted,
    ] {
        corpus.push(encode(&RouteReply::from(route_reply(outcome))));
    }
    for status in [
        RelayRemoteResourceFrameStatus::Delivered,
        RelayRemoteResourceFrameStatus::Backpressure,
        RelayRemoteResourceFrameStatus::Unavailable,
    ] {
        let reply = RelayRemoteResourceFrameReply { status };
        let frozen = FrameReply::from(reply.clone());
        assert_eq!(encode(&frozen), encode(&reply));
        assert_eq!(
            RelayRemoteResourceFrameReply::from(frozen.clone()).status,
            status
        );
        corpus.push(encode(&frozen));
    }
    // Pin shared scalar/authority leaves as well as outer contracts. Never update
    // this hash for a production schema change: retain these DTOs and add a new
    // contract. Change fixture coverage only with a reviewed fixture update.
    assert_eq!(
        hex::encode(Sha256::digest(encode(&corpus))),
        "9ed8834110e03e569e9512e3697f4e1805aaeb1d8c88b340ef787446bd501b8b"
    );
}

#[tokio::test]
async fn old_and_new_frame_senders_reach_socket_once_through_retained_actor_handlers() {
    use crate::clustering::route_bridge::tests::{
        origin_identity, receiver_identity, services_with_claims,
    };
    use kameo::remote::RemoteMessage;
    use waddle_xmpp::registry::ConnectionEntry;
    assert_eq!(
        <RelayActor as RemoteMessage<RouteV8>>::REMOTE_ID,
        "waddle.clustering.relay.remote_resource_route.v8"
    );
    assert_eq!(
        <RelayActor as RemoteMessage<FrameV3>>::REMOTE_ID,
        "waddle.clustering.relay.remote_resource_frame.v3"
    );
    assert_eq!(
        <RelayActor as RemoteMessage<LiveRoute>>::REMOTE_ID,
        "waddle.clustering.relay.live_resource_route.v1"
    );
    assert_eq!(
        <RelayActor as RemoteMessage<LiveFrame>>::REMOTE_ID,
        "waddle.clustering.relay.live_resource_frame.v1"
    );

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
    bridge.wire(Arc::clone(&services));
    let target: jid::FullJid = "b@example.test/tablet".parse().expect("target");
    let (tx, mut rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), entry);
    let registration_id = bridge
        .test_insert_remote_socket_registration(
            target.clone(),
            owner,
            NodeId::new("remote-owner".to_owned()),
        )
        .await;
    let actor = RelayActor::spawn(RelayActor::new(
        SharedNodeIdentity::new(receiver_identity()),
        false,
        ResumeStealBridge::new(),
        RoomLocalClaims::new(),
        bridge,
    ));
    for mut message in frames()
        .into_iter()
        .filter(|message| message.frame.ingress_append.is_none())
    {
        message.frame.registration_id = registration_id;
        // Old sender -> new receiver: decode the real pre-upgrade wire bytes
        // and dispatch the retained message handler, not a synthetic mock ACK.
        let baseline: FrameV3 =
            rmp_serde::from_slice(&encode(&message)).expect("old frame decoded");
        let reply = actor.ask(baseline).await.expect("retained receiver");
        assert_eq!(
            RelayRemoteResourceFrameReply::from(reply).status,
            RelayRemoteResourceFrameStatus::Delivered
        );
        let outbound = rx.recv().await.expect("old sender frame delivered");
        assert_eq!(RemoteStanza(outbound.stanza), message.frame.stanza);
        assert_eq!(outbound.kind, message.frame.kind);
        assert!(rx.try_recv().is_err(), "one effect from old sender");

        // New sender -> old receiver: the peer rejects the live id before
        // dispatch, then receives the baseline frame through its real handler.
        let reply = live_or_baseline(
            async { Err::<RelayRemoteResourceFrameReply, _>(unknown()) },
            async {
                let old: RelayDeliverRemoteResourceFrame =
                    rmp_serde::from_slice(&encode(&FrameV3::from(message.clone())))
                        .expect("old receiver codec");
                Ok(actor
                    .ask(FrameV3::from(old))
                    .await
                    .expect("baseline receiver")
                    .into())
            },
        )
        .await
        .expect("bounded fallback delivery");
        assert_eq!(reply.status, RelayRemoteResourceFrameStatus::Delivered);
        let outbound = rx.recv().await.expect("fallback frame delivered");
        assert_eq!(RemoteStanza(outbound.stanza), message.frame.stanza);
        assert_eq!(outbound.kind, message.frame.kind);
        assert!(rx.try_recv().is_err(), "fallback must not duplicate");

        // New sender -> new receiver uses the independent live handler. The
        // fallback is observable if accidentally polled even after success.
        let reply = live_or_baseline(
            async {
                Ok::<RelayRemoteResourceFrameReply, RemoteSendError<Infallible>>(
                    actor
                        .ask(LiveFrame::from_current(&message).expect("live frame"))
                        .await
                        .expect("live receiver")
                        .into(),
                )
            },
            async { panic!("supported live endpoint must not fall back") },
        )
        .await
        .expect("live delivery");
        assert_eq!(reply.status, RelayRemoteResourceFrameStatus::Delivered);
        let outbound = rx.recv().await.expect("live frame delivered");
        assert_eq!(RemoteStanza(outbound.stanza), message.frame.stanza);
        assert_eq!(outbound.kind, message.frame.kind);
        assert!(rx.try_recv().is_err(), "one effect from new sender");
    }
    actor.stop_gracefully().await.expect("stop actor");
}
