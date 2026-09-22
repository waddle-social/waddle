use super::*;
use crate::clustering::claims::{NodeLeaseStore, OrphanedSmSessionClaim};
use crate::clustering::ordered_relay::{
    OrderedRelayChannel, OrderedRelayClaim, OrderedRelayOrigin, OrderedRelayPayload,
    OrderedRelayRecipient, OrderedRelaySequence, OriginInboundSequence,
};
use async_trait::async_trait;
use libp2p::PeerId;
use std::collections::HashSet;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, NodeIdentity,
    ResumeIdentityProof, StalePredicate,
};

#[tokio::test]
async fn relay_replies_follow_repeated_identity_rotations_without_respawn() {
    let identity = SharedNodeIdentity::new(NodeIdentity::new("initial", "first"));
    let mut rotations = identity.subscribe_rotations();
    let actor = RelayActor::spawn(RelayActor::new(
        identity.clone(),
        false,
        ResumeStealBridge::new(),
        RoomLocalClaims::new(),
        OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &crate::config::ClusteringMessagingConfig::default(),
        ),
    ));
    assert_eq!(
        actor
            .ask(RelayPing)
            .await
            .expect("initial pong")
            .node_id
            .as_str(),
        "initial"
    );
    for node in ["recovered-once", "recovered-twice"] {
        identity.rotate(NodeIdentity::new(node, node)).await;
        tokio::time::timeout(Duration::from_secs(1), rotations.changed())
            .await
            .expect("rotation notification")
            .expect("source remains live");
        assert_eq!(
            actor
                .ask(RelayPing)
                .await
                .expect("rotated pong")
                .node_id
                .as_str(),
            node
        );
        let stanza = RemoteStanza(waddle_xmpp::Stanza::Message(
            xmpp_parsers::message::Message::new(None::<jid::Jid>),
        ));
        assert_eq!(
            actor
                .ask(RelayEchoStanza { stanza })
                .await
                .expect("rotated echo")
                .node_id
                .as_str(),
            node
        );
    }
    actor.stop_gracefully().await.expect("stop actor");
}

#[test]
fn changed_muc_proxy_wire_shapes_have_new_remote_message_ids() {
    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayDeliverOrdered>>::REMOTE_ID,
        "waddle.clustering.relay.deliver_ordered.v11"
    );
    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayRouteRemoteResourceStanza>>::REMOTE_ID,
        "waddle.clustering.relay.remote_resource_route.v7"
    );
}

/// Issue #1789: the owner-to-socket frame carries the origin's ingress obligation so
/// the socket node's detach drain can key its append. The shape changed, so the id
/// must (#1597): an old peer answers `UnknownMessage`, which proves no handler ran
/// and must not be read as a stale registration — the mirror has to survive a
/// rolling update so the retry can land once the peer is upgraded.
#[test]
fn remote_resource_frame_v2_carries_the_ingress_obligation() {
    use crate::ingress::identity::IngressAppendObligationRef;
    use crate::ingress::EffectReceiptKey;
    use crate::ingress_substrate::EffectReceiptKind;
    use waddle_xmpp::ingress::{IngressEffectKind, MessageKey};

    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayDeliverRemoteResourceFrame>>::REMOTE_ID,
        "waddle.clustering.relay.remote_resource_frame.v2"
    );

    let obligation = IngressAppendObligationRef {
        message_key: MessageKey::from_storage(uuid::Uuid::from_u128(1789)),
        sender_bare: "romeo@example.test".parse().expect("sender"),
        receipt: EffectReceiptKey {
            kind: EffectReceiptKind::from_storage(IngressEffectKind::RouteDirect.storage_tag()),
            semantic_identity_hash: [89; 32],
        },
        received_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
    };
    let frame = RemoteResourceOutboundFrame {
        jid: "juliet@example.test/phone".parse().expect("full jid"),
        registration_id: serde_json::from_str("\"00000000-0000-0000-0000-000000001789\"")
            .expect("registration id"),
        stanza: crate::clustering::codec::RemoteStanza(waddle_xmpp::Stanza::Message(
            xmpp_parsers::message::Message::new(None::<jid::Jid>),
        )),
        kind: waddle_xmpp::registry::DeliveryKind::PeerStanza,
        ingress_append: Some(obligation.clone()),
    };
    let encoded = serde_json::to_vec(&frame).expect("encode frame");
    let decoded: RemoteResourceOutboundFrame =
        serde_json::from_slice(&encoded).expect("decode frame");
    assert_eq!(decoded.ingress_append, Some(obligation));

    let old_peer = send_error::<std::convert::Infallible>(RemoteSendError::UnknownMessage {
        actor_remote_id: "actor".into(),
        message_remote_id: "waddle.clustering.relay.remote_resource_frame.v2".into(),
    });
    assert!(
        matches!(
            old_peer,
            RelayAskError::Send {
                failure: RelaySendFailure::Codec,
                effect: RelaySendEffect::NoEffect,
                ..
            }
        ),
        "an old peer is a no-effect codec failure, never a stale registration: {old_peer:?}"
    );
}

/// Issue #1803: the cross-node resource-presence probe is a NEW message id,
/// not a change to the ordered-relay envelope or its replies, so
/// `deliver_ordered.vN` must NOT move for it (#1597). The id and the typed
/// reply shape are pinned here, and an old peer's `UnknownMessage` is a
/// no-effect codec failure — which the ghost-eviction guard reads as "could
/// not prove absence", making both orders of a rolling update safe.
#[test]
fn resource_presence_is_a_new_message_id_with_a_round_tripping_reply() {
    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayResourcePresence>>::REMOTE_ID,
        "waddle.clustering.relay.resource_presence.v1"
    );
    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayDeliverOrdered>>::REMOTE_ID,
        "waddle.clustering.relay.deliver_ordered.v11",
        "a new relay message must not bump the ordered-relay envelope id"
    );

    let message = RelayResourcePresence {
        target: "juliet@example.test/web-1803".parse().expect("full jid"),
        trace: crate::clustering::trace_context::RelayTraceContext::default(),
    };
    let decoded: RelayResourcePresence =
        serde_json::from_slice(&serde_json::to_vec(&message).expect("encode probe"))
            .expect("decode probe");
    assert_eq!(decoded.target, message.target);

    for reply in [
        RelayResourcePresenceReply::Present,
        RelayResourcePresenceReply::Absent,
    ] {
        assert_eq!(
            serde_json::from_slice::<RelayResourcePresenceReply>(
                &serde_json::to_vec(&reply).expect("encode reply")
            )
            .expect("decode reply"),
            reply
        );
    }

    let old_peer = send_error::<std::convert::Infallible>(RemoteSendError::UnknownMessage {
        actor_remote_id: "actor".into(),
        message_remote_id: "waddle.clustering.relay.resource_presence.v1".into(),
    });
    assert!(
        matches!(
            old_peer,
            RelayAskError::Send {
                failure: RelaySendFailure::Codec,
                effect: RelaySendEffect::NoEffect,
                ..
            }
        ),
        "a peer that predates the probe is a no-effect codec failure: {old_peer:?}"
    );
}

#[test]
fn incomplete_carbons_reply_has_new_remote_message_id() {
    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayRemoteUserSideEffect>>::REMOTE_ID,
        "waddle.clustering.relay.remote_user_side_effect.v3"
    );
    let status = RelayRemoteUserSideEffectStatus::Incomplete {
        reason: crate::server::routes::interpret::carbons::CarbonFanoutFailure::DetachedAppend,
    };
    let encoded = serde_json::to_vec(&status).expect("encode typed fanout failure");
    assert_eq!(
        serde_json::from_slice::<RelayRemoteUserSideEffectStatus>(&encoded)
            .expect("decode typed fanout failure"),
        status
    );
}

struct HangingClaimStore;

#[async_trait]
impl ClaimStore for HangingClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        Ok(())
    }

    async fn acquire(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn ensure_claimed(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn steal_stale(
        &self,
        _entity: &Entity,
        _observed: ClaimEpoch,
        _staleness: StalePredicate,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn steal_for_resume(
        &self,
        _entity: &Entity,
        _observed: ClaimEpoch,
        _witness: ResumeIdentityProof,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn current_claim(&self, _entity: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        std::future::pending().await
    }

    async fn fence(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
        _mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn release(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
        _mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }

    async fn release_many(
        &self,
        _entities: &[Entity],
        _me: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        unreachable!("ordered relay timeout test only calls current_claim")
    }
}

struct NoopNodeLease;

#[async_trait]
impl NodeLeaseStore for NoopNodeLease {
    async fn list_orphaned_room_actor_claims_page(
        &self,
        _after: Option<crate::clustering::claims::RoomOrphanScanCursor>,
        _limit: usize,
    ) -> Result<crate::clustering::claims::OrphanedRoomActorClaimPage, ClaimError> {
        Ok(crate::clustering::claims::OrphanedRoomActorClaimPage {
            candidates: Vec::new(),
            next_cursor: None,
            has_more: false,
            quarantined: 0,
        })
    }

    async fn register(
        &self,
        _me: &NodeIdentity,
        _pod_template_hash: Option<String>,
    ) -> Result<(), ClaimError> {
        Ok(())
    }

    async fn heartbeat(
        &self,
        _me: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<bool, ClaimError> {
        Ok(true)
    }

    async fn expire(
        &self,
        _owner: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<bool, ClaimError> {
        Ok(true)
    }

    async fn mark_draining(&self, _me: &NodeIdentity) -> Result<(), ClaimError> {
        Ok(())
    }

    async fn count_other_live_nodes(
        &self,
        _me: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<usize, ClaimError> {
        Ok(0)
    }

    async fn list_other_unexpired_nodes(
        &self,
        _me: &NodeIdentity,
        _limit: usize,
    ) -> Result<Vec<NodeIdentity>, ClaimError> {
        Ok(Vec::new())
    }

    async fn reconcile(
        &self,
        _me: &NodeIdentity,
        _locally_owned: &[Entity],
    ) -> Result<Vec<Entity>, ClaimError> {
        Ok(Vec::new())
    }

    async fn report_steal_intent(
        &self,
        _entity: &Entity,
        _reporter: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        Ok(())
    }

    async fn owner_steal_intents(
        &self,
        _me: &NodeIdentity,
    ) -> Result<Vec<(Entity, ClaimEpoch)>, ClaimError> {
        Ok(Vec::new())
    }

    async fn clear_steal_intent(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
        _mine: ClaimEpoch,
    ) -> Result<u64, ClaimError> {
        Ok(0)
    }

    async fn list_orphaned_sm_session_claims(
        &self,
    ) -> Result<Vec<OrphanedSmSessionClaim>, ClaimError> {
        Ok(Vec::new())
    }

    async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
        Ok(None)
    }
}

struct NoopAllowlist;

#[async_trait]
impl crate::clustering::allowlist::AllowlistStore for NoopAllowlist {
    async fn ensure_schema(&self) -> Result<(), crate::clustering::allowlist::AllowlistError> {
        Ok(())
    }

    async fn enrolled_peers(
        &self,
    ) -> Result<HashSet<PeerId>, crate::clustering::allowlist::AllowlistError> {
        Ok(HashSet::new())
    }
}

pub(super) fn timeout_envelope() -> RemoteStanzaEnvelope {
    use waddle_xmpp::pending_delivery::SmSessionId;
    use xmpp_parsers::message::{Lang, Message};

    let target: jid::FullJid = "timeout@example.test/phone"
        .parse()
        .expect("valid full jid");
    let sender: jid::FullJid = "sender@example.test/laptop"
        .parse()
        .expect("valid full jid");
    let origin_stream = SmSessionId::new("stream-timeout");
    let mut message = Message::new(Some(jid::Jid::from(target.clone())));
    message.from = Some(jid::Jid::from(sender.clone()));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Lang::new(), "timeout test".to_string());

    RemoteStanzaEnvelope {
        asserted_origin_node: NodeId::new("origin-node".to_string()),
        channel: OrderedRelayChannel {
            origin: OrderedRelayOrigin::SmSession(origin_stream.clone()),
            recipient: OrderedRelayRecipient::FullJid(target.clone()),
            origin_epoch: ClaimEpoch(0),
            target_epoch: ClaimEpoch(0),
        },
        sequence: OrderedRelaySequence::FIRST,
        origin_inbound_sequence: OriginInboundSequence(1),
        origin_claim: OrderedRelayClaim {
            entity: Entity::new(EntityType::SmSession, origin_stream.to_string()),
            epoch: ClaimEpoch(0),
        },
        sender_claim: OrderedRelayClaim {
            entity: Entity::new(EntityType::UserActor, sender.to_bare().to_string()),
            epoch: ClaimEpoch(0),
        },
        target_claim: OrderedRelayClaim {
            entity: Entity::new(EntityType::UserActor, target.to_bare().to_string()),
            epoch: ClaimEpoch(0),
        },
        payload: OrderedRelayPayload::Message {
            ingress_append: None,
            recipient: jid::Jid::from(target),
            stanza: RemoteStanza(waddle_xmpp::Stanza::Message(message)),
        },
        origin_proof: None,
    }
}

fn spawn_test_relay_actor() -> kameo::actor::ActorRef<RelayActor> {
    use kameo::actor::Spawn;
    let resume_bridge = ResumeStealBridge::new();
    resume_bridge.wire(Arc::new(waddle_xmpp::registry::ConnectionRegistry::new()));
    RelayActor::spawn(RelayActor::new(
        waddle_xmpp::ownership::SharedNodeIdentity::new(waddle_xmpp::ownership::NodeIdentity::new(
            "span-test-node".to_string(),
            "incarnation".to_string(),
        )),
        false,
        resume_bridge,
        RoomLocalClaims::new(),
        OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &crate::config::ClusteringMessagingConfig::default(),
        ),
    ))
}

mod dispatch;
mod dispatch_span;
mod error_classification;
