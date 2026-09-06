use super::delivery::muc::*;
use super::delivery::receiver::*;
use super::delivery::*;
use super::reassert::REASSERT_CLAIM_READ_TIMEOUT;
use super::*;
use crate::clustering::ordered_relay::OrderedRelaySequence;
use kameo::actor::Spawn;
use libp2p::PeerId;
use std::collections::HashSet;
use waddle_xmpp::ownership::{ClaimEpoch, ClaimError, InProcessClaimStore, NodeIdentity};
use xmpp_parsers::message::{Lang, Message};

struct StaticNodeLease {
    origin: NodeIdentity,
    peer_id: String,
}

#[async_trait::async_trait]
impl NodeLeaseStore for StaticNodeLease {
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

    async fn peer_id_for_node(&self, node: &NodeIdentity) -> Result<Option<String>, ClaimError> {
        if node == &self.origin {
            Ok(Some(self.peer_id.clone()))
        } else {
            Ok(None)
        }
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
    ) -> Result<Vec<crate::clustering::claims::OrphanedSmSessionClaim>, ClaimError> {
        Ok(Vec::new())
    }

    async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
        Ok(None)
    }
}

struct StaticAllowlist {
    peer_id: PeerId,
}

#[async_trait::async_trait]
impl AllowlistStore for StaticAllowlist {
    async fn ensure_schema(&self) -> Result<(), super::super::allowlist::AllowlistError> {
        Ok(())
    }

    async fn enrolled_peers(
        &self,
    ) -> Result<HashSet<PeerId>, super::super::allowlist::AllowlistError> {
        Ok(HashSet::from([self.peer_id]))
    }
}

pub(super) fn origin_identity() -> NodeIdentity {
    NodeIdentity::new("origin-node", "origin-epoch")
}

pub(super) fn receiver_identity() -> NodeIdentity {
    NodeIdentity::new("receiver-node", "receiver-epoch")
}

fn other_identity() -> NodeIdentity {
    NodeIdentity::new("other-node", "other-epoch")
}

fn origin_entity() -> Entity {
    Entity::new(EntityType::SmSession, "stream-1")
}

fn sender_full() -> jid::FullJid {
    "romeo@example.test/home".parse().expect("full jid")
}

fn sender_entity() -> Entity {
    user_entity(&sender_full().to_bare())
}

fn target_bare() -> jid::BareJid {
    "juliet@example.test".parse().expect("bare jid")
}

fn target_full() -> jid::FullJid {
    "juliet@example.test/phone".parse().expect("full jid")
}

fn target_entity() -> Entity {
    user_entity(&target_bare())
}

fn envelope_claims(target_epoch: i64) -> OrderedRelayEnvelopeClaims {
    OrderedRelayEnvelopeClaims::new(
        OrderedRelayClaim {
            entity: origin_entity(),
            epoch: ClaimEpoch(0),
        },
        OrderedRelayClaim {
            entity: sender_entity(),
            epoch: ClaimEpoch(0),
        },
        OrderedRelayClaim {
            entity: target_entity(),
            epoch: ClaimEpoch(target_epoch),
        },
    )
}

fn message_payload() -> OrderedRelayPayload {
    let full = target_full();
    let mut message = Message::new(Some(jid::Jid::from(full.clone())));
    message.from = Some(jid::Jid::from(sender_full()));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Lang::new(), "hello from remote".to_string());
    OrderedRelayPayload::Message {
        recipient: jid::Jid::from(full),
        stanza: RemoteStanza(Stanza::Message(message)),
    }
}

fn iq_payload() -> OrderedRelayPayload {
    let full = target_full();
    let mut iq = xmpp_parsers::iq::Iq::from_get("iq-1", xmpp_parsers::ping::Ping);
    *iq.from_mut() = Some(jid::Jid::from(sender_full()));
    *iq.to_mut() = Some(jid::Jid::from(full.clone()));
    OrderedRelayPayload::Iq {
        recipient: jid::Jid::from(full),
        stanza: RemoteStanza(Stanza::Iq(Box::new(iq))),
    }
}

fn envelope() -> RemoteStanzaEnvelope {
    RemoteStanzaEnvelope {
        asserted_origin_node: NodeId::new(origin_identity().node_id),
        channel: OrderedRelayChannel {
            origin: OrderedRelayOrigin::SmSession(waddle_xmpp::pending_delivery::SmSessionId::new(
                "stream-1",
            )),
            recipient: OrderedRelayRecipient::FullJid(target_full()),
            target_epoch: waddle_xmpp::ownership::ClaimEpoch(0),
        },
        sequence: OrderedRelaySequence::FIRST,
        origin_inbound_sequence: OriginInboundSequence(1),
        origin_claim: OrderedRelayClaim {
            entity: origin_entity(),
            epoch: waddle_xmpp::ownership::ClaimEpoch(0),
        },
        sender_claim: OrderedRelayClaim {
            entity: sender_entity(),
            epoch: waddle_xmpp::ownership::ClaimEpoch(0),
        },
        target_claim: OrderedRelayClaim {
            entity: target_entity(),
            epoch: waddle_xmpp::ownership::ClaimEpoch(0),
        },
        payload: message_payload(),
        origin_proof: None,
    }
}

fn sign_envelope(mut envelope: RemoteStanzaEnvelope, keypair: &Keypair) -> RemoteStanzaEnvelope {
    let signing_bytes = envelope.signing_bytes().expect("signing bytes");
    envelope.origin_proof = Some(OrderedRelayOriginProof {
        public_key: keypair.public().encode_protobuf(),
        signature: keypair.sign(&signing_bytes).expect("sign envelope"),
    });
    envelope
}

async fn envelope_for_services(services: &OrderedRelayDeliveryServices) -> RemoteStanzaEnvelope {
    async fn epoch_for(services: &OrderedRelayDeliveryServices, entity: &Entity) -> ClaimEpoch {
        services
            .claim_store
            .current_claim(entity)
            .await
            .expect("claim lookup")
            .expect("seeded claim")
            .claim_epoch
    }

    let mut envelope = envelope();
    envelope.origin_claim.epoch = epoch_for(services, &origin_entity()).await;
    envelope.sender_claim.epoch = epoch_for(services, &sender_entity()).await;
    let target_epoch = epoch_for(services, &target_entity()).await;
    envelope.target_claim.epoch = target_epoch;
    envelope.channel.target_epoch = target_epoch;
    envelope
}

async fn signed_envelope_for_services(
    services: &OrderedRelayDeliveryServices,
    keypair: &Keypair,
) -> RemoteStanzaEnvelope {
    sign_envelope(envelope_for_services(services).await, keypair)
}

pub(super) fn test_peer_id() -> String {
    Keypair::generate_ed25519()
        .public()
        .to_peer_id()
        .to_string()
}

pub(crate) async fn services_with_claims(
    origin_owner: NodeIdentity,
    target_owner: NodeIdentity,
    receiver: NodeIdentity,
    origin_peer_id: String,
) -> OrderedRelayDeliveryServices {
    services_with_claims_and_blocking(
        origin_owner,
        target_owner,
        receiver,
        origin_peer_id,
        Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
    )
    .await
}

async fn services_with_claims_and_blocking(
    origin_owner: NodeIdentity,
    target_owner: NodeIdentity,
    receiver: NodeIdentity,
    origin_peer_id: String,
    blocking_storage: Arc<dyn BlockingStorage>,
) -> OrderedRelayDeliveryServices {
    let store = Arc::new(InProcessClaimStore::new());
    store
        .acquire(&origin_entity(), &origin_owner)
        .await
        .expect("origin claim");
    store
        .acquire(&sender_entity(), &origin_owner)
        .await
        .expect("sender claim");
    store
        .acquire(&target_entity(), &target_owner)
        .await
        .expect("target claim");
    OrderedRelayDeliveryServices {
        claim_store: store,
        allowlist_store: Arc::new(StaticAllowlist {
            peer_id: origin_peer_id.parse().expect("valid test peer id"),
        }),
        node_lease: Arc::new(StaticNodeLease {
            origin: origin_owner,
            peer_id: origin_peer_id,
        }),
        node_identity: SharedNodeIdentity::new(receiver),
        connection_registry: Arc::new(ConnectionRegistry::new()),
        user_registry: UserRegistryActor::spawn(UserRegistryActor::new()),
        sm_session_registry: Arc::new(InMemorySmSessionRegistry::new()),
        blocking_storage,
        web_socket_state: Weak::new(),
    }
}

mod delivery;
mod nack_channels;
mod reassert;
