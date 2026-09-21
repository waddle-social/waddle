//! Exact settlement evidence is distinct from the derived delivery audience.
use super::{external_route_identity, external_route_recipient, external_route_targets};
use crate::{
    ingress::decision::EffectReceiptKey,
    ingress_uow::IngressUowError,
    server::routes::interpret::effects::{delivery::ExternalDeliveryEffect, ExternalEffect},
};
use jid::{BareJid, FullJid};
use waddle_xmpp::ingress::{
    EffectMessageIdentity, EntityGeneration, IngressEffectIntent, StoredMessagePayload,
};

#[derive(Clone, Debug)]
pub enum ProgressObligation {
    Direct {
        recipient: BareJid,
    },
    MucGroupchat {
        room: BareJid,
        occupants: Vec<FullJid>,
        reflection: FullJid,
        room_generation: EntityGeneration,
    },
    MucSystemBroadcast {
        room: BareJid,
        occupants: Vec<FullJid>,
        room_generation: EntityGeneration,
        system_message: Option<StoredMessagePayload>,
    },
}

#[derive(Clone, Debug)]
pub struct RouteProgress {
    pub receipt: EffectReceiptKey,
    pub obligation: ProgressObligation,
    pub route_identity: EffectMessageIdentity,
    pub received_at: Option<chrono::DateTime<chrono::Utc>>,
    pub fanout: Vec<FullJid>,
    pub completed: Vec<FullJid>,
    /// This attempt's reflection is fresh work, even inside the frozen fanout.
    pub current_attempt_reflection: Option<FullJid>,
}

impl RouteProgress {
    pub(crate) fn from_intent(
        intent: &IngressEffectIntent,
        received_at: Option<chrono::DateTime<chrono::Utc>>,
        completed: Vec<FullJid>,
    ) -> Result<Option<Self>, IngressUowError> {
        let (obligation, route_identity, fanout) = match intent {
            IngressEffectIntent::RouteDirect {
                recipient,
                fanout,
                route_identity,
            } => (
                ProgressObligation::Direct {
                    recipient: recipient.clone(),
                },
                route_identity,
                fanout.clone(),
            ),
            IngressEffectIntent::RouteMucGroupchat {
                room,
                occupants,
                reflection,
                room_generation,
                route_identity,
            } => (
                ProgressObligation::MucGroupchat {
                    room: room.clone(),
                    occupants: occupants.clone(),
                    reflection: reflection.clone(),
                    room_generation: *room_generation,
                },
                route_identity,
                occupants
                    .iter()
                    .filter(|target| *target != reflection)
                    .cloned()
                    .collect(),
            ),
            IngressEffectIntent::RouteMucSystemBroadcast {
                room,
                occupants,
                room_generation,
                system_message,
                route_identity,
            } => (
                ProgressObligation::MucSystemBroadcast {
                    room: room.clone(),
                    occupants: occupants.clone(),
                    room_generation: *room_generation,
                    system_message: system_message.clone(),
                },
                route_identity,
                occupants.clone(),
            ),
            _ => return Ok(None),
        };
        Ok(Some(Self {
            receipt: crate::ingress::receipt_key(intent)?,
            obligation,
            route_identity: route_identity.clone(),
            received_at,
            fanout,
            completed,
            current_attempt_reflection: None,
        }))
    }

    pub fn settle_evidence(&self) -> IngressEffectIntent {
        match &self.obligation {
            ProgressObligation::Direct { recipient } => IngressEffectIntent::RouteDirect {
                recipient: recipient.clone(),
                fanout: self.fanout.clone(),
                route_identity: self.route_identity.clone(),
            },
            ProgressObligation::MucGroupchat {
                room,
                occupants,
                reflection,
                room_generation,
            } => IngressEffectIntent::RouteMucGroupchat {
                room: room.clone(),
                occupants: occupants.clone(),
                reflection: reflection.clone(),
                room_generation: *room_generation,
                route_identity: self.route_identity.clone(),
            },
            ProgressObligation::MucSystemBroadcast {
                room,
                occupants,
                room_generation,
                system_message,
            } => IngressEffectIntent::RouteMucSystemBroadcast {
                room: room.clone(),
                occupants: occupants.clone(),
                room_generation: *room_generation,
                system_message: system_message.clone(),
                route_identity: self.route_identity.clone(),
            },
        }
    }

    pub(crate) fn is_direct(&self) -> bool {
        matches!(self.obligation, ProgressObligation::Direct { .. })
    }

    /// The room whose occupancy owns this frozen fanout, or `None` for a
    /// direct route, whose audience no room roster can decide.
    pub(crate) fn room(&self) -> Option<&BareJid> {
        match &self.obligation {
            ProgressObligation::Direct { .. } => None,
            ProgressObligation::MucGroupchat { room, .. }
            | ProgressObligation::MucSystemBroadcast { room, .. } => Some(room),
        }
    }

    /// Correlation includes drifted/completed occupants for replay filtering,
    /// but excludes sender reflection, which never carries aggregate evidence.
    pub(crate) fn correlates(&self, effect: &ExternalEffect) -> bool {
        let room = match &self.obligation {
            ProgressObligation::Direct { recipient } => {
                return external_route_recipient(effect).as_ref() == Some(recipient)
                    && external_route_identity(effect) == Some(&self.route_identity)
            }
            ProgressObligation::MucGroupchat {
                room, reflection, ..
            } => {
                if single_target(effect) == Some(reflection)
                    || (single_target(effect) == self.current_attempt_reflection.as_ref()
                        && external_route_identity(effect) != Some(&self.route_identity))
                {
                    return false;
                }
                room
            }
            ProgressObligation::MucSystemBroadcast { room, .. } => room,
        };
        let Some(target) = single_target(effect) else {
            return false;
        };
        matches!(&self.route_identity, EffectMessageIdentity::StanzaId(id) if id.by == *room)
            && crate::ingress::receipts::routing::full_delivery(effect, target).is_some_and(
                |message| {
                    crate::ingress::receipts::routing::message_identity(
                        message,
                        &self.route_identity,
                    )
                },
            )
    }

    pub(crate) fn matches(&self, effect: &ExternalEffect) -> bool {
        self.correlates(effect)
            && (self.is_direct()
                || single_target(effect).is_some_and(|target| self.fanout.contains(target)))
    }

    pub(crate) fn remaining(&self, effect: &ExternalEffect) -> Vec<FullJid> {
        external_route_targets(effect)
            .into_iter()
            .filter(|target| self.fanout.contains(target) && !self.completed.contains(target))
            .collect()
    }
}

pub(crate) fn single_target(effect: &ExternalEffect) -> Option<&FullJid> {
    match effect {
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::HostOwnedCopy { target: jid, .. }
            | ExternalDeliveryEffect::RouteToPeer { jid, .. }
            | ExternalDeliveryEffect::RelayFullJid { target: jid, .. },
        ) => Some(jid),
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. }) => {
            match resources.as_slice() {
                [target] => Some(target),
                _ => None,
            }
        }
        _ => None,
    }
}
