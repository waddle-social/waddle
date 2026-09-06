//! Typed results shared by assumed planning and immediate execution.
use waddle_xmpp::{
    inbox::{storage::InboxStorageError, InboxEntry},
    mam::{storage::MamStorageError, StoreOutcome},
    Stanza,
};

#[derive(Debug)]
pub enum EffectOutcome {
    Membership(super::MembershipOutcome),
    MucUserDelivery(
        Result<
            super::invite::MucUserDeliveryProof,
            crate::server::routes::websocket::handlers::message::muc_invite::MucUserDeliveryError,
        >,
    ),
    InviteLedger(
        Result<
            crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerOutcome,
            crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerError,
        >,
    ),
    Completed,
    Frames(Vec<Stanza>),
    #[cfg(feature = "clustering")]
    RelayFrames {
        frames: Vec<Stanza>,
        completion: crate::ingress::execute::RelayFrameReceiptCompletion,
    },
    Archive(Result<StoreOutcome, MamStorageError>),
    Inbox(Result<InboxEntry, InboxStorageError>),
    PlannedInbox(ProjectionRef),
    Delivery(super::super::routing::FullJidDeliveryOutcome),
    Unavailable,
}

impl super::PlannedEffect {
    pub(super) fn assumed_outcome(&self, projection: ProjectionRef) -> EffectOutcome {
        use super::{direct::DurableDirectEffect, room::DurableRoomEffect, DurableEffect, Effect};
        match &self.effect {
            Effect::External(super::ExternalEffect::RoomMembershipMutation(mutation)) => {
                let previous_affiliation = match mutation {
                    super::early::RoomMembershipMutation::Muc(grant) => grant.previous_affiliation,
                    super::early::RoomMembershipMutation::GroupDm(_) => {
                        waddle_xmpp::Affiliation::None
                    }
                };
                EffectOutcome::Membership(super::MembershipOutcome::Granted {
                    previous_affiliation,
                })
            }
            Effect::External(
                super::ExternalEffect::RouteToPeer(route)
                | super::ExternalEffect::QueueOfflineDelivery(route),
            ) => EffectOutcome::MucUserDelivery(Ok(if route.resources.is_empty() {
                super::invite::MucUserDeliveryProof::Queued {
                    row_id: route.fallback.id.clone(),
                }
            } else {
                super::invite::MucUserDeliveryProof::Delivered {
                    resources: route.resources.clone(),
                }
            })),
            Effect::External(super::ExternalEffect::InviteLedger(mutation)) => {
                use crate::server::routes::websocket::{
                    handlers::message::muc_invite::{InviteLedgerMutation, InviteLedgerOutcome},
                    muc_invites::RecordOutcome,
                };
                EffectOutcome::InviteLedger(Ok(match mutation {
                    InviteLedgerMutation::Record { recorded_at, .. } => {
                        InviteLedgerOutcome::Recorded(RecordOutcome::New {
                            created_at: *recorded_at,
                        })
                    }
                    InviteLedgerMutation::Claim { .. } => InviteLedgerOutcome::Claimed(true),
                }))
            }
            Effect::External(super::ExternalEffect::Room(
                super::room::ExternalRoomEffect::ArchiveAfterPin { message, .. },
            ))
            | Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                message,
                ..
            }))
            | Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
                message,
                ..
            })) => EffectOutcome::Archive(Ok(StoreOutcome::Stored(message.id.clone()))),
            Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ProjectInbox {
                ..
            }))
            | Effect::Durable(DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                ..
            }))
            | Effect::Durable(DurableEffect::Direct(DurableDirectEffect::MarkInboxRead {
                ..
            })) => EffectOutcome::PlannedInbox(projection),
            Effect::External(super::ExternalEffect::Delivery(_)) => {
                EffectOutcome::Delivery(super::super::routing::FullJidDeliveryOutcome::Delivered)
            }
            _ => EffectOutcome::Completed,
        }
    }
}

/// Index of an inbox-producing durable operation in the unchanged ingress plan.
/// It remains stable when archive identities are reconciled before commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProjectionRef(pub usize);

/// Successful values returned by Phase B, published only after commit.
#[derive(Clone, Debug)]
pub enum DurableOutcome {
    Inbox(InboxEntry),
}

/// Outcomes belong to one plan and one committed transaction attempt.
#[derive(Debug, Default)]
pub struct AppliedDurableEffects {
    outcomes: std::collections::HashMap<ProjectionRef, DurableOutcome>,
}

impl AppliedDurableEffects {
    pub fn insert(&mut self, projection: ProjectionRef, outcome: DurableOutcome) {
        self.outcomes.insert(projection, outcome);
    }

    pub fn inbox(&self, projection: ProjectionRef) -> Option<&InboxEntry> {
        match self.outcomes.get(&projection)? {
            DurableOutcome::Inbox(entry) => Some(entry),
        }
    }
}
