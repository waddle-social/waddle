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

#[test]
fn changed_muc_proxy_wire_shapes_have_new_remote_message_ids() {
    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayDeliverOrdered>>::REMOTE_ID,
        "waddle.clustering.relay.deliver_ordered.v6"
    );
    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayRouteRemoteResourceStanza>>::REMOTE_ID,
        "waddle.clustering.relay.remote_resource_route.v4"
    );
}

#[test]
fn incomplete_carbons_reply_has_new_remote_message_id() {
    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayRemoteUserSideEffect>>::REMOTE_ID,
        "waddle.clustering.relay.remote_user_side_effect.v2"
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

fn timeout_envelope() -> RemoteStanzaEnvelope {
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
        NodeId::new("span-test-node".to_string()),
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
