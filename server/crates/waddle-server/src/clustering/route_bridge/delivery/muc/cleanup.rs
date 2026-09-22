use super::*;
use crate::clustering::relay::{RelayMucCleanup, RelayMucCleanupOutcome};

impl OrderedRelayDeliveryBridge {
    /// A detached/failed connection has no registered remote resource to route
    /// through. Its exact membership generation is sufficient for a leave;
    /// this narrow request cannot join, update presence, or select `Any`.
    pub(crate) async fn cleanup_muc_via_user_owner(
        self: &Arc<Self>,
        sender: &jid::FullJid,
        occupant: &jid::FullJid,
        generation: waddle_xmpp_core::OccupancySessionGeneration,
    ) -> MucProxyRouteDecision {
        let Some(services) = self.services.get() else {
            return MucProxyRouteDecision::OriginUnavailable;
        };
        let Some(claim) = tokio::time::timeout(
            ORDERED_DELIVERY_MAILBOX_TIMEOUT,
            current_claim(services, &user_entity(&sender.to_bare())),
        )
        .await
        .ok()
        .flatten() else {
            return MucProxyRouteDecision::OriginUnavailable;
        };
        if !claim.owner_lease_fresh {
            return MucProxyRouteDecision::OriginUnavailable;
        }
        let request = RelayMucCleanup {
            sender: sender.clone(),
            occupant: occupant.clone(),
            generation,
            user_claim_epoch: claim.claim_epoch,
            trace: RelayTraceContext::default(),
        };
        let outcome = if claim.owner == services.node_identity.current() {
            self.cleanup_muc_on_user_owner(request).await
        } else {
            RelayHandle::new(NodeId::new(claim.owner.node_id), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout)
                .cleanup_muc(request)
                .await
                .unwrap_or(RelayMucCleanupOutcome::Retry)
        };
        match outcome {
            RelayMucCleanupOutcome::Converged => {
                MucProxyRouteDecision::Attempted(MucProxyRouteAttempt {
                    relay_target: None,
                    room_fence: None,
                    outcome: OrderedRelayMucProxyOutcome::Delivered(Vec::new()),
                })
            }
            RelayMucCleanupOutcome::Retry => MucProxyRouteDecision::OriginUnavailable,
        }
    }

    pub(crate) async fn cleanup_muc_on_user_owner(
        self: &Arc<Self>,
        request: RelayMucCleanup,
    ) -> RelayMucCleanupOutcome {
        let Some(services) = self.services.get() else {
            return RelayMucCleanupOutcome::Retry;
        };
        let entity = user_entity(&request.sender.to_bare());
        // Bound observation before reserving an ordered sequence. The ordered
        // send itself must finish its settlement even if the RPC caller times
        // out; cancelling it can strand a reserved sequence and every retry.
        let Some(claim) = tokio::time::timeout(
            ORDERED_DELIVERY_MAILBOX_TIMEOUT,
            current_claim(services, &entity),
        )
        .await
        .ok()
        .flatten() else {
            return RelayMucCleanupOutcome::Retry;
        };
        if !claim.owner_lease_fresh
            || claim.owner != services.node_identity.current()
            || claim.claim_epoch != request.user_claim_epoch
        {
            return RelayMucCleanupOutcome::Retry;
        }
        let origin = OrderedRelayRouteOrigin {
            kind: OrderedRelayRouteOriginKind::Entity(entity.clone()),
            sender_entity: entity,
            inbound_sequence: 0,
            handoff: None,
        };
        let room = request.occupant.to_bare();
        let mut presence =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
        presence.from = Some(request.sender.into());
        presence.to = Some(request.occupant.into());
        let stanza = Stanza::Presence(presence);
        let kind = OrderedRelayMucProxyKind::OccupantPresence;
        let muc_origin = MucProxyOrigin::Connection(request.generation);
        let outcome = match self
            .try_proxy_muc_remote_from_local_origin_decision(
                &room, &stanza, kind, muc_origin, &origin, None,
            )
            .await
        {
            MucProxyRouteDecision::Attempted(attempt) => attempt.outcome,
            MucProxyRouteDecision::RoomUnclaimed => return RelayMucCleanupOutcome::Converged,
            MucProxyRouteDecision::LocalRoom => {
                // The same generation fence applies on the receiving room;
                // no current socket registration is required or fabricated.
                muc_proxy_result_to_ordered_outcome(
                    kind,
                    deliver_reserved_muc_proxy(
                        services, &room, kind, muc_origin, &stanza, None, &mut None,
                    )
                    .await,
                )
            }
            MucProxyRouteDecision::RoomClaimUnavailable
            | MucProxyRouteDecision::OriginUnavailable => return RelayMucCleanupOutcome::Retry,
        };
        match outcome {
            OrderedRelayMucProxyOutcome::Delivered(frames)
                if frames.iter().all(|frame| !matches!(frame, Stanza::Presence(presence) if presence.type_ == xmpp_parsers::presence::Type::Error)) =>
            {
                RelayMucCleanupOutcome::Converged
            }
            _ => RelayMucCleanupOutcome::Retry,
        }
    }
}

#[cfg(test)]
tokio::task_local! {
    pub(crate) static TEST_CLEANUP_RELAY: Arc<DelayedCleanupRelay>;
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct DelayedCleanupRelay {
    receiver: tokio::sync::Mutex<crate::clustering::ordered_relay::OrderedRelayReceiverState>,
    sequences: tokio::sync::Mutex<Vec<u64>>,
}

#[cfg(test)]
impl DelayedCleanupRelay {
    pub(crate) async fn deliver(&self, envelope: RemoteStanzaEnvelope) -> OrderedRelayReply {
        if self.sequences.lock().await.is_empty() {
            tokio::time::sleep(ORDERED_RECEIVER_DELIVERY_TIMEOUT + Duration::from_secs(1)).await;
        }
        self.sequences.lock().await.push(envelope.sequence.0);
        let mut receiver = self.receiver.lock().await;
        match receiver.reserve(envelope) {
            crate::clustering::ordered_relay::OrderedRelayReservation::Reserved(reserved) => {
                receiver.commit_reserved(*reserved)
            }
            crate::clustering::ordered_relay::OrderedRelayReservation::Completed(reply) => reply,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::route_bridge::tests::{
        receiver_identity, services_with_claims, test_peer_id,
    };
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use waddle_xmpp::muc::room_actor::{GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation};
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp_core::{Affiliation, OccupancySessionGeneration};

    #[tokio::test(start_paused = true)]
    async fn slow_cleanup_settles_ordered_sequence_before_the_next_departure() {
        let services = services_with_claims(
            receiver_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await;
        let sender: jid::FullJid = "alice@example.com/departed"
            .parse()
            .expect("valid departed sender JID");
        let room: jid::BareJid = "cleanup@muc.example.com"
            .parse()
            .expect("valid cleanup room JID");
        let epoch = services
            .claim_store
            .acquire(&user_entity(&sender.to_bare()), &receiver_identity())
            .await
            .expect("acquire the sender user claim");
        services
            .claim_store
            .acquire(
                &room_entity(&room),
                &crate::clustering::route_bridge::tests::origin_identity(),
            )
            .await
            .expect("acquire the remote room claim");
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire_origin_signer(libp2p::identity::Keypair::generate_ed25519());
        bridge.wire(Arc::new(services));
        let request = RelayMucCleanup {
            sender,
            occupant: room
                .with_resource_str("alice")
                .expect("valid cleanup occupant JID"),
            generation: OccupancySessionGeneration::mint(),
            user_claim_epoch: epoch,
            trace: RelayTraceContext::default(),
        };
        let receiver = Arc::new(DelayedCleanupRelay::default());
        TEST_CLEANUP_RELAY
            .scope(receiver.clone(), async {
                assert_eq!(
                    bridge.cleanup_muc_on_user_owner(request.clone()).await,
                    RelayMucCleanupOutcome::Converged
                );
                assert_eq!(
                    bridge.cleanup_muc_on_user_owner(request).await,
                    RelayMucCleanupOutcome::Converged
                );
            })
            .await;
        assert_eq!(*receiver.sequences.lock().await, vec![1, 2]);
    }

    #[tokio::test]
    async fn cleanup_without_registration_is_claim_and_generation_fenced() {
        let state = create_test_websocket_state().await;
        let mut services = services_with_claims(
            receiver_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await;
        services.web_socket_state = Arc::downgrade(&state);
        let sender: jid::FullJid = "alice@example.com/departed"
            .parse()
            .expect("valid departed sender JID");
        let sibling: jid::FullJid = "alice@example.com/live"
            .parse()
            .expect("valid live sibling JID");
        let room: jid::BareJid = "cleanup@muc.example.com"
            .parse()
            .expect("valid cleanup room JID");
        let epoch = services
            .claim_store
            .acquire(&user_entity(&sender.to_bare()), &receiver_identity())
            .await
            .expect("acquire the sender user claim");
        services
            .claim_store
            .acquire(&room_entity(&room), &receiver_identity())
            .await
            .expect("acquire the local room claim");
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "w".to_owned(),
                channel_id: "c".to_owned(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create the cleanup test room");
        let generation = OccupancySessionGeneration::mint();
        let sibling_generation = OccupancySessionGeneration::mint();
        for (jid, generation) in [(&sender, generation), (&sibling, sibling_generation)] {
            actor
                .ask(JoinWithAffiliation {
                    sender_jid: jid.clone(),
                    nick: "alice".to_owned(),
                    affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                    local_domain: "example.com".to_owned(),
                    admission_revision: actor
                        .ask(GetSnapshot)
                        .await
                        .expect("read the room admission revision")
                        .admission_revision,
                    session: generation,
                })
                .await
                .expect("join the test occupant with its generation");
        }
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::new(services));
        let request = RelayMucCleanup {
            sender: sender.clone(),
            occupant: room
                .with_resource_str("alice")
                .expect("valid cleanup occupant JID"),
            generation,
            user_claim_epoch: epoch,
            trace: RelayTraceContext::default(),
        };
        let mut stale_claim = request.clone();
        stale_claim.user_claim_epoch = waddle_xmpp::ownership::ClaimEpoch(epoch.0 + 1);
        assert_eq!(
            bridge.cleanup_muc_on_user_owner(stale_claim).await,
            RelayMucCleanupOutcome::Retry
        );
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("read occupancy after rejecting the stale claim")
                .room
                .session_generation(&sender),
            Some(generation)
        );

        let mut wrong_generation = request.clone();
        wrong_generation.generation = OccupancySessionGeneration::mint();
        assert_eq!(
            bridge.cleanup_muc_on_user_owner(wrong_generation).await,
            RelayMucCleanupOutcome::Converged
        );
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("read occupancy after ignoring the wrong generation")
                .room
                .session_generation(&sender),
            Some(generation)
        );

        assert_eq!(
            bridge.cleanup_muc_on_user_owner(request.clone()).await,
            RelayMucCleanupOutcome::Converged
        );
        let snapshot = actor
            .ask(GetSnapshot)
            .await
            .expect("read occupancy after generation-scoped cleanup");
        assert_eq!(snapshot.room.session_generation(&sender), None);
        assert_eq!(
            snapshot.room.session_generation(&sibling),
            Some(sibling_generation)
        );
        // The same retained generation is harmless after an acknowledgement is lost.
        assert_eq!(
            bridge.cleanup_muc_on_user_owner(request).await,
            RelayMucCleanupOutcome::Converged
        );
    }
}
