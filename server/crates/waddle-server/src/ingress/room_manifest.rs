//! The sealed audience and child identities of a canonical room submission.
pub(crate) mod replay;
use std::collections::HashSet;

use jid::{BareJid, FullJid};
use waddle_xmpp::{
    ingress::{DeliveryKey, EffectAuthorityKey, IngressEffectIntent, MessageKey},
    muc::{
        durable::{
            AdminMutationId, RoomClaimFenceContext, RoomCommittedCoordinates, RoomLifecycleId,
            RoomRevision,
        },
        room_actor::OccupancyWatermark,
    },
    ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity},
};
use waddle_xmpp_core::xep0359::StanzaId;

use super::decision::EffectReceiptKey;
use crate::{ingress::substrate::IngressSubstrateError, ingress::uow::IngressUowError};

/// One actor snapshot, captured before the handled frontier advances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomManifestSnapshot {
    pub coordinates: Option<RoomCommittedCoordinates>,
    pub occupancy_incarnation: AdminMutationId,
    pub occupants: Vec<(FullJid, OccupancyWatermark)>,
    /// Trusted original room HostOwnedCopy effects, classified before acceptance.
    pub host_no_transport: Vec<FullJid>,
}

impl From<&waddle_xmpp::muc::room_actor::RoomChainSnapshot> for RoomManifestSnapshot {
    fn from(snapshot: &waddle_xmpp::muc::room_actor::RoomChainSnapshot) -> Self {
        Self {
            coordinates: snapshot.durable_coordinates,
            occupancy_incarnation: snapshot.occupancy_incarnation,
            host_no_transport: Vec::new(),
            occupants: snapshot
                .occupants
                .iter()
                .map(|occupant| (occupant.full_jid.clone(), occupant.watermark))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomManifestChildDisposition {
    Delivery,
    /// Extension room copies observe through hooks and require no transport I/O.
    HostRoomCopyRequiresNoTransport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomManifestChild {
    pub receipt: EffectReceiptKey,
    pub delivery_key: DeliveryKey,
    pub target: Option<FullJid>,
    pub occupancy: Option<OccupancyWatermark>,
    pub disposition: RoomManifestChildDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenRoomManifest {
    pub room: BareJid,
    pub stanza_id: StanzaId,
    pub coordinates: Option<RoomCommittedCoordinates>,
    pub claim_fence: Option<RoomClaimFenceContext>,
    pub occupancy_incarnation: AdminMutationId,
    pub children: Vec<RoomManifestChild>,
    message_key: MessageKey,
    intents: Vec<IngressEffectIntent>,
}

impl FrozenRoomManifest {
    pub fn build(
        message_key: MessageKey,
        room: BareJid,
        stanza_id: StanzaId,
        snapshot: &RoomManifestSnapshot,
        claim_fence: Option<RoomClaimFenceContext>,
        intents: &[IngressEffectIntent],
    ) -> Result<Self, IngressUowError> {
        // Use the same canonical audience ordering as the recorded intent
        // codec, while retaining the original effect ordinal order.
        let intents = intents
            .iter()
            .map(|intent| intent.with_encoded_v1(IngressEffectIntent::decode_v1)?)
            .collect::<Result<Vec<_>, waddle_xmpp::ingress::EffectIntentCodecError>>()?;
        if stanza_id.by != room {
            return Err(IngressUowError::MissingRoomStanzaId);
        }
        if claim_fence.as_ref().is_some_and(|fence| {
            fence.entity.entity_type != EntityType::RoomActor
                || fence.entity.id != room.to_string()
                || fence.epoch.0 <= 0
        }) {
            return Err(IngressUowError::RoomGenerationStale);
        }
        let mut children = Vec::new();
        let mut seen = HashSet::new();
        let direct_targets: HashSet<_> = intents
            .iter()
            .flat_map(|intent| match intent {
                IngressEffectIntent::RouteDirect { fanout, .. } => fanout.clone(),
                _ => Vec::new(),
            })
            .collect();
        let room_targets: HashSet<_> = intents
            .iter()
            .flat_map(|intent| match intent {
                IngressEffectIntent::RouteMucGroupchat {
                    occupants,
                    reflection,
                    ..
                } => {
                    let mut targets = occupants.clone();
                    targets.push(reflection.clone());
                    targets
                }
                IngressEffectIntent::RouteMucSystemBroadcast { occupants, .. } => occupants.clone(),
                _ => Vec::new(),
            })
            .collect();
        if snapshot.host_no_transport.iter().any(|target| {
            !room_targets.contains(target)
                || !snapshot
                    .occupants
                    .iter()
                    .any(|(occupant, _)| occupant == target)
        }) {
            return Err(IngressUowError::RoomGenerationStale);
        }
        for intent in &intents {
            let receipt = super::receipt_key(intent)?;
            let effect = intent.semantic_key();
            let delivery_key = match intent.authority_key() {
                EffectAuthorityKey::Inbox {
                    owner,
                    partner,
                    thread,
                } => {
                    DeliveryKey::inbox_projection(message_key, &owner, (&partner, thread.as_ref()))
                }
                _ => DeliveryKey::effect(message_key, &effect),
            };
            if seen.insert((
                receipt.kind.to_storage(),
                receipt.semantic_identity_hash,
                None,
            )) {
                children.push(RoomManifestChild {
                    receipt: receipt.clone(),
                    delivery_key,
                    target: None,
                    occupancy: None,
                    disposition: RoomManifestChildDisposition::Delivery,
                });
            }
            let targets = match intent {
                IngressEffectIntent::RouteDirect { fanout, .. } => fanout.clone(),
                IngressEffectIntent::RouteMucGroupchat {
                    occupants,
                    reflection,
                    ..
                } => {
                    let mut targets = occupants.clone();
                    if !direct_targets.contains(reflection) {
                        targets.push(reflection.clone());
                    }
                    targets.retain(|target| {
                        target != reflection || !direct_targets.contains(reflection)
                    });
                    targets
                }
                IngressEffectIntent::RouteMucSystemBroadcast { occupants, .. } => occupants.clone(),
                IngressEffectIntent::Carbons {
                    carbon_recipients, ..
                } => carbon_recipients.clone(),
                IngressEffectIntent::RouteOccupantPm { recipient, .. }
                | IngressEffectIntent::CallSignal { recipient, .. }
                | IngressEffectIntent::ErrorReply { recipient, .. } => vec![recipient.clone()],
                _ => Vec::new(),
            };
            for target in targets {
                let requires_no_transport = snapshot.host_no_transport.contains(&target)
                    && match intent {
                        IngressEffectIntent::RouteMucGroupchat { reflection, .. } => {
                            &target != reflection
                        }
                        IngressEffectIntent::RouteMucSystemBroadcast { .. } => true,
                        _ => false,
                    };
                let occupancy = snapshot
                    .occupants
                    .iter()
                    .find(|(jid, _)| jid == &target)
                    .map(|(_, generation)| *generation);
                if room_targets.contains(&target) && occupancy.is_none() {
                    return Err(IngressUowError::RoomGenerationStale);
                }
                if seen.insert((
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash,
                    Some(target.clone()),
                )) {
                    children.push(RoomManifestChild {
                        receipt: receipt.clone(),
                        delivery_key: DeliveryKey::resource(message_key, &effect, &target),
                        occupancy,
                        disposition: if requires_no_transport {
                            RoomManifestChildDisposition::HostRoomCopyRequiresNoTransport
                        } else {
                            RoomManifestChildDisposition::Delivery
                        },
                        target: Some(target),
                    });
                }
            }
        }
        Ok(Self {
            room,
            stanza_id,
            coordinates: snapshot.coordinates,
            claim_fence,
            occupancy_incarnation: snapshot.occupancy_incarnation,
            children,
            message_key,
            intents,
        })
    }

    pub fn message_key(&self) -> MessageKey {
        self.message_key
    }

    pub fn recorded_intents(&self) -> &[IngressEffectIntent] {
        &self.intents
    }

    pub fn child(
        &self,
        receipt: &EffectReceiptKey,
        target: Option<&FullJid>,
    ) -> Option<&RoomManifestChild> {
        self.children
            .iter()
            .find(|child| &child.receipt == receipt && child.target.as_ref() == target)
    }

    pub fn contains(&self, receipt: &EffectReceiptKey, target: Option<&FullJid>) -> bool {
        self.child(receipt, target).is_some()
    }
}

/// Envelope metadata is bounded independently of the XML frame and each intent.
/// Oversized manifests fail their transaction rather than sealing a partial audience.
const MAX_ROOM_MANIFEST_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredRoomManifest {
    message_key: MessageKey,
    room: BareJid,
    stanza_id: StanzaId,
    coordinates: Option<StoredCoordinates>,
    claim_fence: Option<StoredFence>,
    occupancy_incarnation: uuid::Uuid,
    children: Vec<StoredChild>,
    intents: Vec<StoredIntent>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCoordinates {
    lifecycle: uuid::Uuid,
    revision: i64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFence {
    entity: Entity,
    epoch: ClaimEpoch,
    node_id: String,
    node_epoch: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredChild {
    receipt: EffectReceiptKey,
    delivery_key: DeliveryKey,
    target: Option<FullJid>,
    occupancy: Option<u64>,
    disposition: RoomManifestChildDisposition,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIntent {
    kind: i32,
    payload: serde_json::Value,
}

impl StoredRoomManifest {
    pub(crate) fn from_domain(
        manifest: &FrozenRoomManifest,
    ) -> Result<Self, IngressSubstrateError> {
        let intents = manifest
            .intents
            .iter()
            .map(|intent| {
                intent
                    .with_encoded_v1(|kind, payload| {
                        serde_json::from_slice(payload)
                            .map(|payload| StoredIntent { kind, payload })
                    })
                    .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)?
                    .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)
            })
            .collect::<Result<_, _>>()?;
        let stored = Self {
            message_key: manifest.message_key,
            room: manifest.room.clone(),
            stanza_id: manifest.stanza_id.clone(),
            coordinates: manifest.coordinates.map(|coordinates| StoredCoordinates {
                lifecycle: coordinates.lifecycle.as_uuid(),
                revision: coordinates.revision.as_i64(),
            }),
            claim_fence: manifest.claim_fence.as_ref().map(|fence| StoredFence {
                entity: fence.entity.clone(),
                epoch: fence.epoch,
                node_id: fence.owner.node_id.clone(),
                node_epoch: fence.owner.node_epoch.clone(),
            }),
            occupancy_incarnation: manifest.occupancy_incarnation.as_uuid(),
            children: manifest
                .children
                .iter()
                .map(|child| StoredChild {
                    receipt: child.receipt.clone(),
                    delivery_key: child.delivery_key,
                    target: child.target.clone(),
                    occupancy: child.occupancy.map(OccupancyWatermark::revision),
                    disposition: child.disposition,
                })
                .collect(),
            intents,
        };
        stored.check_size()?;
        if stored.clone().into_domain()? != *manifest {
            return Err(IngressSubstrateError::InvalidStoredEnvelope);
        }
        Ok(stored)
    }

    fn check_size(&self) -> Result<(), IngressSubstrateError> {
        let bytes =
            serde_json::to_vec(self).map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)?;
        if bytes.len() > MAX_ROOM_MANIFEST_BYTES {
            return Err(IngressSubstrateError::InvalidStoredEnvelope);
        }
        Ok(())
    }

    pub(crate) fn into_domain(self) -> Result<FrozenRoomManifest, IngressSubstrateError> {
        self.check_size()?;
        let invalid = || IngressSubstrateError::InvalidStoredEnvelope;
        let coordinates = self
            .coordinates
            .map(|coordinates| {
                Ok(RoomCommittedCoordinates {
                    lifecycle: RoomLifecycleId::from_uuid(coordinates.lifecycle),
                    revision: RoomRevision::from_stored(coordinates.revision)
                        .ok_or_else(invalid)?,
                })
            })
            .transpose()?;
        let claim_fence = self.claim_fence.map(|fence| {
            RoomClaimFenceContext::new(
                fence.entity,
                NodeIdentity::new(fence.node_id, fence.node_epoch),
                fence.epoch,
            )
        });
        let intents = self
            .intents
            .into_iter()
            .map(|intent| {
                let payload = serde_json::to_vec(&intent.payload).map_err(|_| invalid())?;
                IngressEffectIntent::decode_v1(intent.kind, &payload).map_err(|_| invalid())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let children: Vec<_> = self
            .children
            .into_iter()
            .map(|child| RoomManifestChild {
                receipt: child.receipt,
                delivery_key: child.delivery_key,
                target: child.target,
                occupancy: child.occupancy.map(OccupancyWatermark::from_revision),
                disposition: child.disposition,
            })
            .collect();
        let mut keys = HashSet::new();
        let mut receipt_targets = HashSet::new();
        let mut occupants = Vec::new();
        let mut host_no_transport = Vec::new();
        for child in &children {
            if !keys.insert(child.delivery_key)
                || !receipt_targets.insert((
                    child.receipt.kind.to_storage(),
                    child.receipt.semantic_identity_hash,
                    child.target.clone(),
                ))
                || child.target.is_none() && child.occupancy.is_some()
            {
                return Err(invalid());
            }
            if let (Some(target), Some(occupancy)) = (&child.target, child.occupancy) {
                if occupants
                    .iter()
                    .any(|(jid, generation)| jid == target && *generation != occupancy)
                {
                    return Err(invalid());
                }
                occupants.push((target.clone(), occupancy));
                if child.disposition
                    == RoomManifestChildDisposition::HostRoomCopyRequiresNoTransport
                    && !host_no_transport.contains(target)
                {
                    host_no_transport.push(target.clone());
                }
            } else if child.disposition != RoomManifestChildDisposition::Delivery {
                return Err(invalid());
            }
        }
        let snapshot = RoomManifestSnapshot {
            coordinates,
            occupancy_incarnation: AdminMutationId::from_uuid(self.occupancy_incarnation),
            occupants,
            host_no_transport,
        };
        let rebuilt = FrozenRoomManifest::build(
            self.message_key,
            self.room,
            self.stanza_id,
            &snapshot,
            claim_fence,
            &intents,
        )
        .map_err(|_| invalid())?;
        if rebuilt.children != children {
            return Err(invalid());
        }
        Ok(rebuilt)
    }
}
