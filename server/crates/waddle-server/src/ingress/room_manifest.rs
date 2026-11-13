//! The sealed audience and child identities of a canonical room submission.
use std::collections::HashSet;

use jid::{BareJid, FullJid};
use waddle_xmpp::ingress::{DeliveryKey, EffectAuthorityKey, IngressEffectIntent, MessageKey};
use waddle_xmpp::muc::durable::{
    AdminMutationId, RoomClaimFenceContext, RoomCommittedCoordinates, RoomLifecycleId, RoomRevision,
};
use waddle_xmpp::muc::room_actor::OccupancyWatermark;
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};
use waddle_xmpp_core::xep0359::StanzaId;
use waddle_xmpp_core::{OccupancySeatId, OccupancySessionGeneration};

use super::decision::EffectReceiptKey;
use crate::ingress_substrate::IngressSubstrateError;
use crate::ingress_uow::IngressUowError;

/// The seat a room child was sealed for: who held the target's seat in the
/// room when the manifest was sealed.
///
/// A node that cannot ask the sealing actor compares this with the room's
/// durable occupancy. The frozen occupant still holds the seat exactly when
/// the recorded row has the same connection generation and the same
/// occupancy identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealedSeat {
    /// Generation of the connection that held the seat.
    pub generation: OccupancySessionGeneration,
    /// Identity of the occupancy that held the seat. `None` when the sealing
    /// actor knew none: the seat was recorded before seats had identities,
    /// or the manifest was stored before children recorded one. Such a seat
    /// is the frozen occupant's only while the recorded row has no identity
    /// either, because every join admitted since writes one.
    pub identity: Option<OccupancySeatId>,
}

/// One actor snapshot, captured before the handled frontier advances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomManifestSnapshot {
    pub coordinates: Option<RoomCommittedCoordinates>,
    pub occupancy_incarnation: AdminMutationId,
    pub occupants: Vec<(FullJid, OccupancyWatermark)>,
    /// The seat of each occupant whose connection generation the room knows.
    pub seats: Vec<(FullJid, SealedSeat)>,
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
            seats: snapshot
                .occupants
                .iter()
                .filter_map(|occupant| {
                    Some((
                        occupant.full_jid.clone(),
                        SealedSeat {
                            generation: occupant.session?,
                            identity: occupant.seat,
                        },
                    ))
                })
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
    /// The seat the target held when the manifest was sealed. It identifies
    /// the frozen occupant to a node that cannot ask the sealing actor about
    /// `occupancy`. `None` for a child without a room occupancy, for a seat
    /// whose generation the sealing actor did not know, and for every child
    /// of a manifest stored before seats were recorded.
    pub seat: Option<SealedSeat>,
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
                    seat: None,
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
                    // The seat belongs to the occupancy: a child without one
                    // records none.
                    let seat = occupancy.and_then(|_| {
                        snapshot
                            .seats
                            .iter()
                            .find(|(jid, _)| jid == &target)
                            .map(|(_, seat)| *seat)
                    });
                    children.push(RoomManifestChild {
                        receipt: receipt.clone(),
                        delivery_key: DeliveryKey::resource(message_key, &effect, &target),
                        occupancy,
                        seat,
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
    /// Version of the child records. A manifest stored without this field is
    /// version 1. Every manifest is written at the latest version; a binary
    /// that does not know a version refuses the record.
    #[serde(default = "StoredRoomManifest::children_without_seats")]
    children_version: u8,
    children: Vec<StoredChild>,
    intents: Vec<StoredIntent>,
}

/// Child records without `seat`: every manifest stored before seats were
/// recorded.
const CHILDREN_WITHOUT_SEATS: u8 = 1;
/// Child records whose `seat` is the generation of the connection that held
/// the target's seat at sealing, or null when the sealing actor knew none.
/// No child has a `seat_identity`: a version 2 child reads as a version 3
/// child whose seat has no identity.
const CHILDREN_WITH_SEATS: u8 = 2;
/// Child records that also carry `seat_identity`, the identity of the
/// occupancy that held the seat, or null when the sealing actor knew none.
/// A `seat_identity` without a `seat` is refused.
const CHILDREN_WITH_SEAT_IDENTITIES: u8 = 3;

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
    /// Present from child version 2 on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seat: Option<OccupancySessionGeneration>,
    /// Present from child version 3 on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seat_identity: Option<OccupancySeatId>,
    disposition: RoomManifestChildDisposition,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIntent {
    kind: i32,
    payload: serde_json::Value,
}

impl StoredRoomManifest {
    fn children_without_seats() -> u8 {
        CHILDREN_WITHOUT_SEATS
    }

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
            children_version: CHILDREN_WITH_SEAT_IDENTITIES,
            children: manifest
                .children
                .iter()
                .map(|child| StoredChild {
                    receipt: child.receipt.clone(),
                    delivery_key: child.delivery_key,
                    target: child.target.clone(),
                    occupancy: child.occupancy.map(OccupancyWatermark::revision),
                    seat: child.seat.map(|seat| seat.generation),
                    seat_identity: child.seat.and_then(|seat| seat.identity),
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
        // Each version admits only the fields it defines, and an identity
        // belongs to a seat.
        let fields_match_version = match self.children_version {
            CHILDREN_WITHOUT_SEATS => self
                .children
                .iter()
                .all(|child| child.seat.is_none() && child.seat_identity.is_none()),
            CHILDREN_WITH_SEATS => self
                .children
                .iter()
                .all(|child| child.seat_identity.is_none()),
            CHILDREN_WITH_SEAT_IDENTITIES => self
                .children
                .iter()
                .all(|child| child.seat.is_some() || child.seat_identity.is_none()),
            _ => false,
        };
        if !fields_match_version {
            return Err(invalid());
        }
        let children: Vec<_> = self
            .children
            .into_iter()
            .map(|child| RoomManifestChild {
                receipt: child.receipt,
                delivery_key: child.delivery_key,
                target: child.target,
                occupancy: child.occupancy.map(OccupancyWatermark::from_revision),
                seat: child.seat.map(|generation| SealedSeat {
                    generation,
                    identity: child.seat_identity,
                }),
                disposition: child.disposition,
            })
            .collect();
        let mut keys = HashSet::new();
        let mut receipt_targets = HashSet::new();
        let mut occupants = Vec::new();
        let mut seats: Vec<(FullJid, SealedSeat)> = Vec::new();
        let mut host_no_transport = Vec::new();
        for child in &children {
            if child.seat.is_some() && child.occupancy.is_none() {
                return Err(invalid());
            }
            if let (Some(target), Some(seat)) = (&child.target, child.seat) {
                if seats
                    .iter()
                    .any(|(jid, recorded)| jid == target && *recorded != seat)
                {
                    return Err(invalid());
                }
                seats.push((target.clone(), seat));
            }
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
            seats,
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

#[cfg(test)]
mod stored_child_version_tests {
    use super::*;
    use waddle_xmpp::ingress::{EffectMessageIdentity, EntityGeneration};

    fn manifest_with_a_seat() -> (FrozenRoomManifest, FullJid, SealedSeat) {
        let room: BareJid = "room@muc.example.test".parse().expect("room");
        let sender: FullJid = "sender@example.test/web".parse().expect("sender");
        let peer: FullJid = "peer@example.test/phone".parse().expect("peer");
        let stamp = StanzaId::new("sealed", room.clone().into());
        let route = IngressEffectIntent::RouteMucGroupchat {
            room: room.clone(),
            occupants: vec![sender.clone(), peer.clone()],
            reflection: sender.clone(),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::stanza(stamp.clone()),
        };
        let seat = SealedSeat {
            generation: OccupancySessionGeneration::mint(),
            identity: Some(OccupancySeatId::mint()),
        };
        let manifest = FrozenRoomManifest::build(
            MessageKey::new(),
            room,
            stamp,
            &RoomManifestSnapshot {
                coordinates: None,
                occupancy_incarnation: AdminMutationId::generate(),
                occupants: vec![
                    (sender, OccupancyWatermark::from_revision(1)),
                    (peer.clone(), OccupancyWatermark::from_revision(2)),
                ],
                seats: vec![(peer.clone(), seat)],
                host_no_transport: Vec::new(),
            },
            None,
            &[route],
        )
        .expect("manifest");
        (manifest, peer, seat)
    }

    fn seats(manifest: &FrozenRoomManifest) -> Vec<(FullJid, SealedSeat)> {
        manifest
            .children
            .iter()
            .filter_map(|child| Some((child.target.clone()?, child.seat?)))
            .collect()
    }

    #[test]
    fn a_stored_manifest_keeps_the_seat_each_child_was_sealed_with() {
        let (manifest, peer, seat) = manifest_with_a_seat();
        assert_eq!(seats(&manifest), vec![(peer, seat)]);
        let stored =
            serde_json::to_value(StoredRoomManifest::from_domain(&manifest).expect("store"))
                .expect("encode");
        assert_eq!(stored["children_version"], 3);
        let read: StoredRoomManifest = serde_json::from_value(stored).expect("decode");
        assert_eq!(read.into_domain().expect("domain"), manifest);
    }

    /// A version 2 manifest has a `seat` on its children and no
    /// `seat_identity`. It is read as it was written: each seat keeps its
    /// generation and has no identity.
    #[test]
    fn a_version_two_manifest_is_read_with_seats_that_have_no_identity() {
        let (manifest, peer, seat) = manifest_with_a_seat();
        let mut stored =
            serde_json::to_value(StoredRoomManifest::from_domain(&manifest).expect("store"))
                .expect("encode");
        stored["children_version"] = serde_json::json!(2);
        let mut removed = 0;
        for child in stored["children"].as_array_mut().expect("children") {
            removed += usize::from(
                child
                    .as_object_mut()
                    .expect("child")
                    .remove("seat_identity")
                    .is_some(),
            );
        }
        assert_eq!(removed, 1, "only the seated child carried an identity");
        let read: StoredRoomManifest = serde_json::from_value(stored).expect("decode version 2");
        let read = read.into_domain().expect("domain");
        let without_identity = SealedSeat {
            generation: seat.generation,
            identity: None,
        };
        assert_eq!(seats(&read), vec![(peer, without_identity)]);
        let mut expected = manifest;
        for child in &mut expected.children {
            if let Some(seat) = &mut child.seat {
                seat.identity = None;
            }
        }
        assert_eq!(read, expected, "nothing but the identities differs");
    }

    #[test]
    fn a_seat_identity_is_refused_in_a_version_two_manifest_and_without_a_seat() {
        let (manifest, _, _) = manifest_with_a_seat();
        let stored =
            serde_json::to_value(StoredRoomManifest::from_domain(&manifest).expect("store"))
                .expect("encode");
        let mut version_two = stored.clone();
        version_two["children_version"] = serde_json::json!(2);
        let read: StoredRoomManifest = serde_json::from_value(version_two).expect("decode");
        assert!(read.into_domain().is_err());

        let mut seatless = stored;
        let mut removed = 0;
        for child in seatless["children"].as_array_mut().expect("children") {
            let child = child.as_object_mut().expect("child");
            if child.contains_key("seat_identity") {
                removed += usize::from(child.remove("seat").is_some());
            }
        }
        assert_eq!(removed, 1);
        let read: StoredRoomManifest = serde_json::from_value(seatless).expect("decode");
        assert!(read.into_domain().is_err());
    }

    /// A manifest stored before seats were recorded has neither the version
    /// field nor a `seat` on any child. It is read as it was written, and
    /// its children name no seat.
    #[test]
    fn a_manifest_stored_without_seats_is_read_with_no_seat_on_any_child() {
        let (manifest, _, _) = manifest_with_a_seat();
        let mut stored =
            serde_json::to_value(StoredRoomManifest::from_domain(&manifest).expect("store"))
                .expect("encode");
        let object = stored.as_object_mut().expect("manifest object");
        object.remove("children_version").expect("version field");
        for child in object["children"].as_array_mut().expect("children") {
            let child = child.as_object_mut().expect("child");
            child.remove("seat");
            child.remove("seat_identity");
        }
        let read: StoredRoomManifest = serde_json::from_value(stored).expect("decode version 1");
        let read = read.into_domain().expect("domain");
        assert!(seats(&read).is_empty());
        let mut expected = manifest;
        for child in &mut expected.children {
            child.seat = None;
        }
        assert_eq!(read, expected, "nothing but the seats differs");
    }

    #[test]
    fn a_seat_in_a_version_one_manifest_and_an_unknown_version_are_refused() {
        let (manifest, _, _) = manifest_with_a_seat();
        let stored =
            serde_json::to_value(StoredRoomManifest::from_domain(&manifest).expect("store"))
                .expect("encode");
        for version in [serde_json::json!(1), serde_json::json!(4)] {
            let mut changed = stored.clone();
            changed["children_version"] = version;
            let read: StoredRoomManifest = serde_json::from_value(changed).expect("decode");
            assert!(read.into_domain().is_err());
        }
    }
}
