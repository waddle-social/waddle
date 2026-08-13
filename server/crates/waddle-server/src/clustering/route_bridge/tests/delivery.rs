use super::*;
use waddle_xmpp::ownership::ClaimEpoch;
use xmpp_parsers::message::Message;

#[test]
fn full_jid_bridge_rejects_groupchat_payloads() {
    let target = target_full();
    let mut message = Message::new(Some(jid::Jid::from(target.clone())));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;

    assert!(payload_for_recipient(jid::Jid::from(target), &Stanza::Message(message)).is_none());
}
#[tokio::test]
async fn unwired_bridge_reports_unreachable_without_advancing_effects() {
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );

    let err = bridge
        .deliver_reserved(&envelope())
        .await
        .expect_err("unwired bridge cannot deliver");

    assert_eq!(err, OrderedRelayNackReason::Unreachable);
}
#[tokio::test]
async fn receiver_rejects_target_claim_not_owned_by_this_node() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        other_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;

    let envelope = signed_envelope_for_services(&services, &keypair).await;
    let err = validate_claims(&services, &envelope)
        .await
        .expect_err("foreign target owner is not this relay");

    assert_eq!(
        err,
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target
        }
    );
}
#[tokio::test]
async fn receiver_accepts_fresh_origin_and_local_target_claims() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;

    let envelope = signed_envelope_for_services(&services, &keypair).await;
    validate_claims(&services, &envelope)
        .await
        .expect("claims match origin and receiver");
}
#[tokio::test]
async fn receiver_rejects_stale_sender_claim_before_delivery_effects() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let mut envelope = envelope_for_services(&services).await;
    envelope.sender_claim.epoch = ClaimEpoch(99);
    let envelope = sign_envelope(envelope, &keypair);

    let err = validate_claims(&services, &envelope)
        .await
        .expect_err("stale sender claim must be rejected");

    assert_eq!(
        err,
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Sender
        }
    );
}
#[tokio::test]
async fn bare_presence_direct_drops_blocked_sender_before_detached_replay() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

    let blocking = Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new());
    blocking.set_blocklist(target_bare(), vec![sender_full().to_bare()]);
    let services = services_with_claims_and_blocking(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
        blocking,
    )
    .await;

    let detached_stream = "stream-detached-presence";
    services
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: detached_stream.to_string(),
            user_id: target_bare().to_string(),
            jid: target_full(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached target resource");

    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.from = Some(jid::Jid::from(sender_full()));
    presence.to = Some(jid::Jid::from(target_bare()));

    deliver_reserved_bare_presence_direct(&services, &target_bare(), &Stanza::Presence(presence))
        .await
        .expect("blocked presence is silently dropped");

    let detached = services
        .sm_session_registry
        .peek_session(detached_stream)
        .await
        .expect("peek detached target resource")
        .expect("detached target resource remains");
    assert!(
        detached.unacked_stanzas.is_empty(),
        "blocked remote presence must not be queued for SM replay"
    );
}
#[tokio::test]
async fn receiver_nacks_iq_without_live_resource_instead_of_detached_queue() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let mut envelope = envelope_for_services(&services).await;
    envelope.payload = iq_payload();
    let envelope = sign_envelope(envelope, &keypair);
    bridge.wire(Arc::new(services));

    let err = bridge
        .deliver_reserved(&envelope)
        .await
        .expect_err("remote full-JID IQ requires a live local resource");

    assert_eq!(err, OrderedRelayNackReason::TargetUnavailable);
}
#[tokio::test]
async fn receiver_delivers_full_jid_iq_as_peer_stanza() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target_full(),
            entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
        })
        .await
        .expect("register resource");
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let mut envelope = envelope_for_services(&services).await;
    envelope.payload = iq_payload();
    let envelope = sign_envelope(envelope, &keypair);
    bridge.wire(Arc::new(services));

    bridge
        .deliver_reserved(&envelope)
        .await
        .expect("remote full-JID IQ delivers to live resource");

    let outbound = rx.try_recv().expect("queued outbound stanza");
    assert_eq!(
        outbound.kind,
        waddle_xmpp::registry::DeliveryKind::PeerStanza
    );
}
#[tokio::test]
async fn stale_registered_remote_resource_cleans_mirror_and_allows_local_fallback() {
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
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
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
        .expect("register remote mirror in user actor");

    let registration_id = RemoteResourceRegistrationId::fresh();
    bridge.remote_owner_resources.lock().await.insert(
        target.clone(),
        RemoteOwnerRegistration {
            registration_id,
            socket_node: NodeId::new("missing-socket-node".to_string()),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            owner: Arc::clone(&owner),
        },
    );

    let outcome = bridge
        .try_deliver_registered_remote_resource(
            &target,
            &Stanza::Message(Message::new(Some(jid::Jid::from(target.clone())))),
            DeliveryKind::PeerStanza,
        )
        .await;

    assert_eq!(outcome, None);
    assert!(
        !bridge
            .remote_owner_resources
            .lock()
            .await
            .contains_key(&target),
        "stale remote owner map entry must be removed"
    );
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &owner)
            .is_none(),
        "stale remote connection mirror must be removed so local fallback can run"
    );
}
#[tokio::test]
async fn stale_force_detach_error_cleans_old_socket_mirror() {
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
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), old_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old remote mirror in user actor");

    let old_generation = RemoteResourceSocketGeneration::next(None);
    let registration = RemoteOwnerRegistration {
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_node: NodeId::new("missing-old-socket-node".to_string()),
        socket_generation: old_generation,
        owner: Arc::clone(&old_owner),
    };

    assert!(
        bridge
            .finish_remote_owner_registration_retire(
                &services,
                &target,
                &registration,
                Err(RelayAskError::NotFound {
                    node_id: registration.socket_node.clone(),
                }),
            )
            .await,
        "a missing old socket node proves no detach was committed and should permit cleanup"
    );
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &old_owner)
            .is_none(),
        "stale old owner mirror must be removed before replacement"
    );
    if let Some(actor) = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .await
        .expect("get user actor")
    {
        assert!(
            actor
                .ask(waddle_xmpp::registry::GetConnectionEntry {
                    jid: target.clone(),
                })
                .await
                .expect("get old actor mirror")
                .is_none(),
            "stale old user-actor mirror must be removed before replacement"
        );
    }
}

#[tokio::test]
async fn stale_force_detach_busy_actor_retries_and_cleans_without_janitor_work() {
    use tokio::sync::{oneshot, Notify};

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
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), old_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old remote mirror in user actor");

    let actor = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .await
        .expect("get user actor")
        .expect("user actor exists");
    let entered = Arc::new(Notify::new());
    let (release_tx, release_rx) = oneshot::channel();
    actor
        .tell(
            waddle_xmpp::registry::user_actor::test_support::GateMailbox {
                entered: Arc::clone(&entered),
                release_rx,
            },
        )
        .await
        .expect("queue mailbox gate");
    entered.notified().await;

    let mut saw_mailbox_full = false;
    for _ in 0..128 {
        let sent = actor
            .tell(waddle_xmpp::registry::user_actor::test_support::MailboxNoop)
            .try_send();
        if matches!(sent, Err(kameo::error::SendError::MailboxFull(_))) {
            saw_mailbox_full = true;
            break;
        }
        sent.expect("mailbox filler should enqueue or report full");
    }
    assert!(
        saw_mailbox_full,
        "child mailbox must be busy before cleanup"
    );

    let old_generation = RemoteResourceSocketGeneration::next(None);
    let registration = RemoteOwnerRegistration {
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_node: NodeId::new("busy-old-socket-node".to_string()),
        socket_generation: old_generation,
        owner: Arc::clone(&old_owner),
    };

    let retire = tokio::spawn({
        let services = Arc::clone(&services);
        let bridge = Arc::clone(&bridge);
        let target = target.clone();
        let registration = registration.clone();
        async move {
            bridge
                .finish_remote_owner_registration_retire(
                    &services,
                    &target,
                    &registration,
                    Err(RelayAskError::NotFound {
                        node_id: registration.socket_node.clone(),
                    }),
                )
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    release_tx.send(()).expect("release mailbox gate");

    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), retire)
            .await
            .expect("bounded retry must not hang")
            .expect("retire task joins"),
        "a brief child-actor busy window should converge synchronously"
    );
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &old_owner)
            .is_none(),
        "stale old owner mirror must be removed before replacement"
    );
    assert_eq!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::user_registry::test_support::PendingUnregisterCount)
            .await
            .expect("pending unregister count"),
        0,
        "successful bounded retry must not leave janitor work behind"
    );
    assert!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: target.to_bare(),
            })
            .await
            .expect("get user actor after cleanup")
            .is_none(),
        "successful bounded retry prunes the now-empty user actor"
    );
}

#[tokio::test]
async fn stale_force_detach_persistently_busy_actor_records_janitor_retry() {
    use tokio::sync::{oneshot, Notify};

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
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), old_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old remote mirror in user actor");

    let actor = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .await
        .expect("get user actor")
        .expect("user actor exists");
    let entered = Arc::new(Notify::new());
    let (release_tx, release_rx) = oneshot::channel();
    actor
        .tell(
            waddle_xmpp::registry::user_actor::test_support::GateMailbox {
                entered: Arc::clone(&entered),
                release_rx,
            },
        )
        .await
        .expect("queue mailbox gate");
    entered.notified().await;

    let mut saw_mailbox_full = false;
    for _ in 0..128 {
        let sent = actor
            .tell(waddle_xmpp::registry::user_actor::test_support::MailboxNoop)
            .try_send();
        if matches!(sent, Err(kameo::error::SendError::MailboxFull(_))) {
            saw_mailbox_full = true;
            break;
        }
        sent.expect("mailbox filler should enqueue or report full");
    }
    assert!(
        saw_mailbox_full,
        "child mailbox must be busy before cleanup"
    );

    let old_generation = RemoteResourceSocketGeneration::next(None);
    let registration = RemoteOwnerRegistration {
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_node: NodeId::new("stuck-old-socket-node".to_string()),
        socket_generation: old_generation,
        owner: Arc::clone(&old_owner),
    };
    bridge
        .remote_owner_resources
        .lock()
        .await
        .insert(target.clone(), registration.clone());

    bridge
        .cleanup_remote_owner_resource_if_registration(&target, registration.registration_id)
        .await;
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &old_owner)
            .is_none(),
        "stale old owner mirror must be removed immediately after recording retry work"
    );
    assert!(
        !bridge
            .remote_owner_resources
            .lock()
            .await
            .contains_key(&target),
        "cleanup must forget the stale owner-side tracking entry immediately"
    );
    assert_eq!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::user_registry::test_support::PendingUnregisterCount)
            .await
            .expect("pending unregister count"),
        1,
        "persistent busy cleanup must leave one exact janitor retry record"
    );

    release_tx.send(()).expect("release mailbox gate");
    tokio::task::yield_now().await;

    assert_eq!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::RetryUserRegistryConvergence)
            .await
            .expect("retry convergence"),
        (0, 0)
    );

    let (fresh_tx, _fresh_rx) = mpsc::channel(1);
    let fresh_entry = ConnectionEntry::new(fresh_tx);
    let successor = RelayRegisterRemoteUserResource {
        jid: target.clone(),
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_generation: RemoteResourceSocketGeneration::next(None),
        socket_node: NodeId::new("replacement-socket-node".to_string()),
        state: RemoteResourceStateSnapshot::from_entry(
            &fresh_entry,
            services.connection_registry.get_presence_state(&target),
        ),
        trace: RelayTraceContext::default(),
    };
    assert_eq!(
        bridge
            .register_remote_user_resource_on_owner(successor.clone())
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::Registered,
        "a successor same-JID remote resource must register after janitor convergence"
    );
    assert!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: target.to_bare(),
            })
            .await
            .expect("get user actor after successor registration")
            .is_some(),
        "the successor registration should recreate the user actor cleanly"
    );
    let successor_registration = bridge
        .remote_owner_resources
        .lock()
        .await
        .get(&target)
        .cloned()
        .expect("successor owner registration");
    assert_eq!(
        successor_registration.registration_id, successor.registration_id,
        "the stale tracking entry must be replaced by the successor registration"
    );
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &successor_registration.owner)
            .is_some(),
        "the successor registration must publish a fresh routing mirror"
    );
}

#[tokio::test]
async fn remote_owner_unregister_reply_reports_recorded_retry() {
    use tokio::sync::{oneshot, Notify};

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
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), old_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old remote mirror in user actor");

    let actor = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .await
        .expect("get user actor")
        .expect("user actor exists");
    let entered = Arc::new(Notify::new());
    let (release_tx, release_rx) = oneshot::channel();
    actor
        .tell(
            waddle_xmpp::registry::user_actor::test_support::GateMailbox {
                entered: Arc::clone(&entered),
                release_rx,
            },
        )
        .await
        .expect("queue mailbox gate");
    entered.notified().await;

    let mut saw_mailbox_full = false;
    for _ in 0..128 {
        let sent = actor
            .tell(waddle_xmpp::registry::user_actor::test_support::MailboxNoop)
            .try_send();
        if matches!(sent, Err(kameo::error::SendError::MailboxFull(_))) {
            saw_mailbox_full = true;
            break;
        }
        sent.expect("mailbox filler should enqueue or report full");
    }
    assert!(
        saw_mailbox_full,
        "child mailbox must be busy before cleanup"
    );

    let registration = RemoteOwnerRegistration {
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_node: NodeId::new("busy-socket-node".to_string()),
        socket_generation: RemoteResourceSocketGeneration::next(None),
        owner: Arc::clone(&old_owner),
    };
    bridge
        .remote_owner_resources
        .lock()
        .await
        .insert(target.clone(), registration.clone());

    let reply = bridge
        .unregister_remote_user_resource_on_owner(RelayUnregisterRemoteUserResource {
            jid: target.clone(),
            registration_id: registration.registration_id,
            socket_generation: registration.socket_generation,
            trace: RelayTraceContext::default(),
        })
        .await;

    assert_eq!(
        reply.status,
        RelayRemoteResourceUnregisterStatus::RecordedRetry
    );
    assert_eq!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::user_registry::test_support::PendingUnregisterCount)
            .await
            .expect("pending unregister count"),
        1,
        "owner reply must only report RecordedRetry once the janitor obligation exists"
    );

    release_tx.send(()).expect("release mailbox gate");
}

#[tokio::test]
async fn receiver_nacks_dropped_peer_delivery_instead_of_acknowledging_loss() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    tx.try_send(waddle_xmpp::registry::OutboundStanza::peer_stanza(
        Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
    ))
    .expect("seed outbound channel to capacity");
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target_full(),
            entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
        })
        .await
        .expect("register resource");
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let envelope = sign_envelope(envelope_for_services(&services).await, &keypair);
    bridge.wire(Arc::new(services));

    let err = bridge
        .deliver_reserved(&envelope)
        .await
        .expect_err("full outbound channel must not be ACKed as delivered");

    assert_eq!(err, OrderedRelayNackReason::Backpressure);
}
