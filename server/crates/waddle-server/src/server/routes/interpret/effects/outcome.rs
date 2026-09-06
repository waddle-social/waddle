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
            (),
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
    Archive(Result<StoreOutcome, MamStorageError>),
    Inbox(Result<InboxEntry, InboxStorageError>),
    Delivery(super::super::routing::FullJidDeliveryOutcome),
    Unavailable,
}

impl super::PlannedEffect {
    pub(super) fn assumed_outcome(&self) -> EffectOutcome {
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
                super::ExternalEffect::RouteToPeer(_)
                | super::ExternalEffect::QueueOfflineDelivery(_),
            ) => EffectOutcome::MucUserDelivery(Ok(())),
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
            Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                message,
                ..
            }))
            | Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
                message,
                ..
            })) => EffectOutcome::Archive(Ok(StoreOutcome::Stored(message.id.clone()))),
            Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ProjectInbox {
                entry,
                ..
            }))
            | Effect::Durable(DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                entry,
                ..
            })) => EffectOutcome::Inbox(Ok(entry.as_ref().clone())),
            Effect::External(super::ExternalEffect::Delivery(_)) => {
                EffectOutcome::Delivery(super::super::routing::FullJidDeliveryOutcome::Delivered)
            }
            _ => EffectOutcome::Completed,
        }
    }
}
