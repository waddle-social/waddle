//! Receiver authority is optional: a rejected key must still deliver the stanza.
use super::*;
use crate::ingress::{
    commit::commit_submission, identity::IngressAppendObligationRef, test_support::IngressFixture,
};
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};
use waddle_xmpp::stream_management::{
    DetachedSession, InMemorySmSessionRegistry, SmSessionRegistry,
};

#[derive(Clone, Copy, Debug)]
enum AuthorityCase {
    Authorized,
    LiveRecipient,
    CanonicalAbsent,
    CanonicalSenderMismatch,
    ClaimMismatch,
    StanzaMismatch,
    ArchivePositionMismatch,
}

async fn ingress_append_authority(
    fixture: IngressFixture,
    second_hop: bool,
    muc: bool,
    live_recipient: bool,
) {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared database");
    let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool),
        Arc::new(fixture.authority().await),
    )
    .await;
    let keypair = Keypair::generate_ed25519();
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    services.web_socket_state = Arc::downgrade(&state);
    services.sm_session_registry = sm.clone();
    let source: jid::FullJid = if muc {
        "room@muc.example.com/romeo"
    } else {
        "romeo@example.com/phone"
    }
    .parse()
    .expect("source");
    let source_entity = if muc {
        Entity::new(EntityType::RoomActor, source.to_bare().to_string())
    } else {
        user_entity(&source.to_bare())
    };
    let source_epoch = services
        .claim_store
        .acquire(&source_entity, &origin_identity())
        .await
        .expect("sender claim");
    let services = Arc::new(services);
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(services.clone());
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
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
        .expect("source registration");
    let registration_id = RemoteResourceRegistrationId::fresh();
    let socket_generation = RemoteResourceSocketGeneration::next(None);
    bridge.remote_owner_resources.lock().await.insert(
        source.clone(),
        RemoteOwnerRegistration {
            socket_identity: NodeIdentity::new("fixture-socket", "fixture-epoch"),
            unregister_pending: false,
            registration_id,
            socket_generation,
            socket_node: NodeId::new("source-socket-node".to_owned()),
            owner,
        },
    );

    let cases: &[AuthorityCase] = if live_recipient {
        &[AuthorityCase::Authorized, AuthorityCase::LiveRecipient]
    } else {
        &[
            AuthorityCase::Authorized,
            AuthorityCase::CanonicalAbsent,
            AuthorityCase::CanonicalSenderMismatch,
            AuthorityCase::ClaimMismatch,
            AuthorityCase::StanzaMismatch,
            AuthorityCase::ArchivePositionMismatch,
        ]
    };
    for (index, case) in cases.iter().copied().enumerate() {
        if muc && index != 0 {
            continue;
        }
        let recipient = target_bare()
            .with_resource_str(&format!("authority-{index}"))
            .expect("recipient");
        let mut live_rx = if matches!(case, AuthorityCase::LiveRecipient) {
            let (tx, rx) = mpsc::channel(1);
            let entry = ConnectionEntry::new(tx);
            services
                .connection_registry
                .register_entry(recipient.clone(), entry.clone());
            services
                .user_registry
                .ask(waddle_xmpp::registry::RegisterUserResource {
                    jid: recipient.clone(),
                    entry,
                })
                .await
                .expect("live recipient registration");
            assert!(sm
                .peek_session(&recipient.to_string())
                .await
                .expect("session read")
                .is_none());
            assert_eq!(fixture.count("sm_ingress_appends").await, 1);
            Some(rx)
        } else {
            sm.store_session(DetachedSession {
                stream_id: recipient.to_string(),
                user_id: recipient.to_bare().to_string(),
                jid: recipient.clone(),
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
            .expect("detached recipient");
            None
        };
        let mut submission = fixture.submission(None, "receiver authority");
        let room_stamp = waddle_xmpp_core::xep0359::StanzaId::new(
            format!("room-authority-{index}"),
            source.to_bare().into(),
        );
        let intent = if muc {
            IngressEffectIntent::RouteMucGroupchat {
                room: source.to_bare(),
                occupants: vec![recipient.clone()],
                reflection: submission.sender.clone(),
                room_generation: waddle_xmpp::ingress::EntityGeneration::INITIAL,
                route_identity: EffectMessageIdentity::stanza(room_stamp.clone()),
            }
        } else {
            IngressEffectIntent::RouteDirect {
                recipient: recipient.to_bare(),
                fanout: vec![recipient.clone()],
                route_identity: EffectMessageIdentity::capture_ordinal(index as u64),
            }
        };
        if muc {
            let mut occupant = submission.plan.sanitized_message.clone();
            occupant.from = Some(source.clone().into());
            occupant.to = Some(recipient.clone().into());
            occupant.type_ = xmpp_parsers::message::MessageType::Groupchat;
            waddle_xmpp_core::xep0359::add_stanza_id(&mut occupant, &room_stamp);
            waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
                &mut occupant,
                &waddle_xmpp::xep::xep0421::OccupantId("sender-occupant".into()),
            );
            crate::ingress::test_support::capture_room_message(&mut submission.plan, &occupant);
        }
        let receipt = crate::ingress::receipt_key(&intent).expect("receipt");
        submission.plan.intents = vec![intent];
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("canonical row");
        let mut obligation = IngressAppendObligationRef {
            archive_positions: Vec::new(),
            dispatch_stream: None,
            message_key: decision.message_key.expect("canonical key"),
            sender_bare: source.to_bare(),
            receipt: receipt.clone(),
            received_at: None,
        };
        let mut message = submission
            .plan
            .room_canonical_message
            .as_deref()
            .unwrap_or(&submission.plan.sanitized_message)
            .clone();
        message.to = Some(recipient.clone().into());
        match case {
            AuthorityCase::Authorized | AuthorityCase::LiveRecipient => {}
            AuthorityCase::CanonicalAbsent => {
                obligation.message_key = waddle_xmpp::ingress::MessageKey::new()
            }
            // The row still names Romeo; authenticate and send as the other user.
            AuthorityCase::CanonicalSenderMismatch => {
                obligation.sender_bare = sender_full().to_bare();
                message.from = Some(sender_full().into());
            }
            AuthorityCase::ClaimMismatch => obligation.sender_bare = sender_full().to_bare(),
            AuthorityCase::StanzaMismatch => message.from = Some(sender_full().into()),
            AuthorityCase::ArchivePositionMismatch => obligation.archive_positions.push(
                waddle_xmpp::stream_management::ArchiveDispatchPosition {
                    archive: recipient.to_bare(),
                    ordinal: waddle_xmpp::mam::ArchiveOrdinal::FIRST,
                },
            ),
        }
        let alternate_sender = matches!(case, AuthorityCase::CanonicalSenderMismatch);
        let ledger_key = waddle_xmpp::stream_management::SmIngressAppendKey {
            message_key: obligation.message_key,
            kind: waddle_xmpp::stream_management::SmIngressReceiptKind::from_storage(
                receipt.kind.to_storage(),
            ),
            semantic_identity_hash: receipt.semantic_identity_hash,
            resource: recipient.clone(),
        };
        if second_hop {
            // The registration authenticates the source independently of stanza.from.
            let source_jid = if alternate_sender {
                let alternate = sender_full();
                let (tx, _alternate_rx) = mpsc::channel(1);
                let entry = ConnectionEntry::new(tx);
                let owner = entry.carbons_handle();
                services
                    .connection_registry
                    .register_entry(alternate.clone(), entry.clone());
                services
                    .user_registry
                    .ask(waddle_xmpp::registry::RegisterUserResource {
                        jid: alternate.clone(),
                        entry,
                    })
                    .await
                    .expect("alternate registration");
                bridge.remote_owner_resources.lock().await.insert(
                    alternate.clone(),
                    RemoteOwnerRegistration {
                        socket_identity: NodeIdentity::new("fixture-socket", "fixture-epoch"),
                        unregister_pending: false,
                        registration_id,
                        socket_generation,
                        socket_node: NodeId::new("source-socket-node".to_owned()),
                        owner,
                    },
                );
                alternate
            } else {
                source.clone()
            };
            let reply = bridge
                .route_remote_resource_stanza_on_owner(
                    RelayRouteRemoteResourceStanza {
                        source_jid,
                        registration_id,
                        socket_generation,
                        target: RemoteResourceRouteTarget::FullJid {
                            target: recipient.clone(),
                            stanza: RemoteStanza(Stanza::Message(message.clone())),
                            ingress_append: Some(obligation),
                        },
                        trace: RelayTraceContext::default(),
                    },
                    &mut None,
                )
                .await;
            assert_eq!(
                reply.outcome,
                if matches!(case, AuthorityCase::ArchivePositionMismatch) {
                    RemoteResourceRouteOutcome::Unavailable
                } else {
                    RemoteResourceRouteOutcome::QueuedDetached
                },
                "{case:?}"
            );
        } else {
            let mut envelope = envelope_for_services(&services).await;
            if !alternate_sender {
                envelope.sender_claim = OrderedRelayClaim {
                    entity: source_entity.clone(),
                    epoch: source_epoch,
                };
            }
            if muc {
                envelope.channel.origin =
                    crate::clustering::ordered_relay::OrderedRelayOrigin::Entity(
                        source_entity.clone(),
                    );
                envelope.origin_claim = envelope.sender_claim.clone();
            }
            envelope.channel.recipient =
                crate::clustering::ordered_relay::OrderedRelayRecipient::FullJid(recipient.clone());
            envelope.payload = OrderedRelayPayload::Message {
                recipient: recipient.clone().into(),
                stanza: RemoteStanza(Stanza::Message(message.clone())),
                ingress_append: Some(obligation),
            };
            // Exercise the authenticated delivery seam directly, including its
            // stanza-sender defense even if the ordered parser also rejects it.
            let result = bridge
                .deliver_reserved(&sign_envelope(envelope, &keypair), &mut None)
                .await;
            if matches!(case, AuthorityCase::ArchivePositionMismatch) {
                assert!(matches!(
                    result,
                    Err(OrderedRelayNackReason::TargetUnavailable)
                ));
            } else {
                result.expect("unsequenced delivery can proceed without an optional key");
            }
        }
        if let Some(rx) = live_rx.as_mut() {
            let outbound = rx
                .try_recv()
                .expect("live recipient receives relayed stanza");
            let Stanza::Message(delivered) = outbound.stanza else {
                panic!("live recipient receives a message");
            };
            assert_eq!(delivered, message);
            assert!(sm
                .peek_session(&recipient.to_string())
                .await
                .expect("session read")
                .is_none());
            assert!(
                crate::sm_persistence::ingress_append::get(&fixture.db, &ledger_key)
                    .await
                    .expect("live recipient ledger read")
                    .is_none()
            );
            assert_eq!(fixture.count("sm_ingress_appends").await, 1);
            continue;
        }
        let queued = sm
            .peek_session(&recipient.to_string())
            .await
            .expect("queue read")
            .expect("session");
        if matches!(case, AuthorityCase::ArchivePositionMismatch) {
            assert!(
                queued.unacked_stanzas.is_empty(),
                "ordered copies cannot degrade to unkeyed appends"
            );
            assert_eq!(queued.outbound_count, 0);
            continue;
        }
        assert_eq!(queued.unacked_stanzas.len(), 1, "{case:?}");
        assert_eq!(queued.outbound_count, 1, "{case:?}");
        let queued_message = Message::try_from(
            queued.unacked_stanzas[0]
                .stanza_xml
                .parse::<minidom::Element>()
                .expect("queued XML"),
        )
        .expect("queued message");
        assert_eq!(queued_message.from, message.from);
        assert_eq!(queued_message.to, message.to);
        assert_eq!(queued_message.type_, message.type_);
        assert_eq!(queued_message.bodies, message.bodies);
        assert_eq!(
            crate::sm_persistence::ingress_append::get(&fixture.db, &ledger_key)
                .await
                .expect("ledger read")
                .is_some(),
            matches!(case, AuthorityCase::Authorized),
            "{case:?}"
        );
    }
    assert_eq!(fixture.count("sm_ingress_appends").await, 1);
    fixture.close().await;
}

// An intermediate sender owner has no local detached recipient. It must still
// authorize the key before forwarding, so recovery cannot append it twice.
async fn forwarded_obligation_survives_intermediate_hop(fixture: IngressFixture) {
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared database");
    let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool),
        Arc::new(fixture.authority().await),
    )
    .await;
    let keypair = Keypair::generate_ed25519();
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        origin_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    services.web_socket_state = Arc::downgrade(&state);
    let recipient = target_full();
    let mut submission = fixture.submission(None, "forwarded obligation");
    let source = submission.sender.clone();
    services
        .claim_store
        .acquire(&user_entity(&source.to_bare()), &origin_identity())
        .await
        .expect("local sender claim");
    assert!(services
        .sm_session_registry
        .detached_resources_for_user(&recipient.to_bare())
        .await
        .expect("local detached resources")
        .is_empty());
    let intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.to_bare(),
        fanout: vec![recipient.clone()],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    };
    let receipt = crate::ingress::receipt_key(&intent).expect("receipt");
    submission.plan.intents = vec![intent];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("canonical row naming the sender");
    let obligation = IngressAppendObligationRef {
        archive_positions: Vec::new(),
        dispatch_stream: None,
        message_key: decision.message_key.expect("canonical key"),
        sender_bare: source.to_bare(),
        receipt,
        received_at: Some(chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("timestamp")),
    };
    let mut message = submission.plan.sanitized_message.clone();
    message.to = Some(recipient.clone().into());
    let stanza = Stanza::Message(message.clone());
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
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
        .expect("source registration");
    let stopped = CancellationToken::new();
    stopped.cancel();
    let bridge = OrderedRelayDeliveryBridge::new(stopped, &ClusteringMessagingConfig::default());
    bridge.wire_origin_signer(keypair);
    bridge.wire(Arc::new(services));
    let registration_id = RemoteResourceRegistrationId::fresh();
    let socket_generation = RemoteResourceSocketGeneration::next(None);
    bridge.remote_owner_resources.lock().await.insert(
        source.clone(),
        RemoteOwnerRegistration {
            socket_identity: NodeIdentity::new("fixture-socket", "fixture-epoch"),
            unregister_pending: false,
            registration_id,
            socket_generation,
            socket_node: NodeId::new("source-socket-node".to_owned()),
            owner,
        },
    );
    // Capture at the real ordered ask boundary; cancellation needs no remote
    // actor or elapsed-time synchronization and leaves the offered payload intact.
    let captured = TEST_CANCELLED_ENVELOPES
        .scope(std::cell::RefCell::new(Vec::new()), async {
            bridge
                .route_remote_resource_stanza_on_owner(
                    RelayRouteRemoteResourceStanza {
                        source_jid: source,
                        registration_id,
                        socket_generation,
                        target: RemoteResourceRouteTarget::FullJid {
                            target: recipient.clone(),
                            stanza: RemoteStanza(stanza.clone()),
                            ingress_append: Some(obligation.clone()),
                        },
                        trace: RelayTraceContext::default(),
                    },
                    &mut None,
                )
                .await;
            TEST_CANCELLED_ENVELOPES.with(|envelopes| envelopes.take())
        })
        .await;
    assert_eq!(
        captured.len(),
        1,
        "must forward exactly one ordered envelope"
    );
    let OrderedRelayPayload::Message {
        recipient: forwarded_recipient,
        stanza: forwarded_stanza,
        ingress_append,
    } = &captured[0].payload
    else {
        panic!("onward message envelope");
    };
    assert_eq!(forwarded_recipient, &jid::Jid::from(recipient));
    let Stanza::Message(forwarded_message) = &forwarded_stanza.0 else {
        panic!("forwarded message stanza");
    };
    assert_eq!(forwarded_message, &message);
    assert_eq!(
        ingress_append.as_ref(),
        Some(&obligation),
        "intermediate hop must preserve the append obligation"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_forwarded_obligation_survives_intermediate_hop() {
    forwarded_obligation_survives_intermediate_hop(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_forwarded_obligation_survives_intermediate_hop() {
    if let Some(fixture) = IngressFixture::postgres("forwarded_obligation").await {
        forwarded_obligation_survives_intermediate_hop(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_live_recipient_delivery_writes_no_append_ledger_row() {
    ingress_append_authority(IngressFixture::sqlite().await, false, false, true).await;
}
#[tokio::test]
async fn postgres_live_recipient_delivery_writes_no_append_ledger_row() {
    if let Some(fixture) = IngressFixture::postgres("live_recipient_append_auth").await {
        ingress_append_authority(fixture, false, false, true).await;
    }
}

#[tokio::test]
async fn sqlite_ordered_ingress_append_authorization() {
    ingress_append_authority(IngressFixture::sqlite().await, false, false, false).await;
}
#[tokio::test]
async fn postgres_ordered_ingress_append_authorization() {
    if let Some(fixture) = IngressFixture::postgres("ordered_append_auth").await {
        ingress_append_authority(fixture, false, false, false).await;
    }
}
#[tokio::test]
async fn sqlite_second_hop_ingress_append_authorization() {
    ingress_append_authority(IngressFixture::sqlite().await, true, false, false).await;
}
#[tokio::test]
async fn postgres_second_hop_ingress_append_authorization() {
    if let Some(fixture) = IngressFixture::postgres("second_hop_append_auth").await {
        ingress_append_authority(fixture, true, false, false).await;
    }
}

#[tokio::test]
async fn sqlite_muc_occupant_ingress_append_is_keyed() {
    ingress_append_authority(IngressFixture::sqlite().await, false, true, false).await;
}
#[tokio::test]
async fn postgres_muc_occupant_ingress_append_is_keyed() {
    if let Some(fixture) = IngressFixture::postgres("muc_occupant_append").await {
        ingress_append_authority(fixture, false, true, false).await;
    }
}

#[tokio::test]
async fn origin_preparation_signs_direct_and_muc_ingress_append_obligations() {
    let keypair = Keypair::generate_ed25519();
    let public_key = keypair.public();
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            origin_identity(),
            public_key.to_peer_id().to_string(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire_origin_signer(keypair);
    for muc in [false, true] {
        let mut envelope = envelope_for_services(&services).await;
        let mut message = Message::new(Some(target_full().into()));
        let identity = EffectMessageIdentity::capture_ordinal(1);
        let intent = if muc {
            let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
            message.from = Some(room.with_resource_str("romeo").expect("occupant").into());
            message.type_ = xmpp_parsers::message::MessageType::Groupchat;
            let entity = Entity::new(EntityType::RoomActor, room.to_string());
            envelope.channel.origin =
                crate::clustering::ordered_relay::OrderedRelayOrigin::Entity(entity.clone());
            envelope.origin_claim.entity = entity.clone();
            envelope.sender_claim = envelope.origin_claim.clone();
            IngressEffectIntent::RouteMucGroupchat {
                room,
                occupants: vec![target_full()],
                reflection: sender_full(),
                room_generation: waddle_xmpp::ingress::EntityGeneration::INITIAL,
                route_identity: identity,
            }
        } else {
            message.from = Some(sender_full().into());
            message.type_ = xmpp_parsers::message::MessageType::Chat;
            IngressEffectIntent::RouteDirect {
                recipient: target_bare(),
                fanout: vec![target_full()],
                route_identity: identity,
            }
        };
        let context = crate::server::routes::interpret::SmIngressAppendContext {
            archive_positions: Vec::new(),
            dispatch_stream: None,
            message_key: waddle_xmpp::ingress::MessageKey::new(),
            receipt: crate::ingress::receipt_key(&intent).expect("receipt"),
            received_at: Some(
                chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("timestamp"),
            ),
        };
        let expected = IngressAppendObligationRef::from_context(
            &context,
            message.from.as_ref().expect("sender").to_bare(),
        );
        let stanza = Stanza::Message(message);
        let prepared = bridge
            .prepare_remote_delivery(RemoteDeliverySeed {
                ingress_append_context: Some(context),
                services: services.clone(),
                target_entity: target_entity(),
                previous_owner: receiver_identity(),
                channel: envelope.channel,
                asserted_origin_node: envelope.asserted_origin_node,
                origin_inbound_sequence: envelope.origin_inbound_sequence,
                origin_claim: envelope.origin_claim,
                sender_claim: envelope.sender_claim,
                target_claim: envelope.target_claim,
                payload: payload_for_recipient(target_full().into(), &stanza)
                    .expect("message payload"),
                target: target_full().into(),
                stanza,
                is_iq: false,
            })
            .await
            .unwrap_or_else(|_| panic!("origin preparation must succeed"));
        let OrderedRelayPayload::Message { ingress_append, .. } = &prepared.envelope.payload else {
            panic!("message payload")
        };
        assert_eq!(ingress_append.as_ref(), Some(&expected));
        let proof = prepared
            .envelope
            .origin_proof
            .as_ref()
            .expect("origin signature");
        assert!(public_key.verify(
            &prepared.envelope.signing_bytes().expect("signing view"),
            &proof.signature
        ));
        let mut altered = prepared.envelope.clone();
        let OrderedRelayPayload::Message {
            ingress_append: Some(obligation),
            ..
        } = &mut altered.payload
        else {
            panic!("obligation")
        };
        obligation.message_key = waddle_xmpp::ingress::MessageKey::new();
        assert!(!public_key.verify(
            &altered.signing_bytes().expect("altered signing view"),
            &proof.signature
        ));
    }
}
