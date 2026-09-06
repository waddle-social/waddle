//! Bind execution-time grant ownership to dependent compensation.
use super::{
    invite::InviteDeliveryFailure, Effect, ExternalEffect, MembershipOutcome, PlannedEffect,
};
use crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerMutation;
use jid::BareJid;

impl PlannedEffect {
    /// Phase C must bind a membership result into its remaining effects before
    /// executing them. A preserved membership belongs to another operation and
    /// must never be revoked by this invitation's failure handling.
    pub fn resolve_membership_outcome(
        &mut self,
        room: &BareJid,
        member: &BareJid,
        outcome: MembershipOutcome,
    ) {
        if let MembershipOutcome::Granted {
            previous_affiliation,
        } = outcome
        {
            let failure = match &mut self.effect {
                Effect::External(ExternalEffect::InviteLedger(InviteLedgerMutation::Record {
                    failure,
                    ..
                })) => failure.as_deref_mut(),
                Effect::External(
                    ExternalEffect::RouteToPeer(route)
                    | ExternalEffect::QueueOfflineDelivery(route),
                ) => route.failure.as_deref_mut(),
                _ => None,
            };
            if let Some(
                InviteDeliveryFailure::RollbackMucMembership(grant)
                | InviteDeliveryFailure::RollbackMuc { grant, .. },
            ) = failure
            {
                if &grant.room == room && &grant.invitee == member {
                    grant.previous_affiliation = previous_affiliation;
                }
            }
            return;
        }
        match &mut self.effect {
            Effect::External(ExternalEffect::InviteLedger(InviteLedgerMutation::Record {
                failure,
                ..
            })) => {
                if failure
                    .as_deref()
                    .is_some_and(|failure| owns_membership(failure, room, member))
                {
                    *failure = None;
                }
            }
            Effect::External(
                ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route),
            ) => {
                if let Some(failure) = route.failure.as_deref() {
                    if owns_membership(failure, room, member) {
                        let invite = match failure {
                            InviteDeliveryFailure::RollbackMuc { invite, .. }
                            | InviteDeliveryFailure::RollbackGroupDm { invite, .. } => {
                                Some(invite.clone())
                            }
                            _ => None,
                        };
                        route.failure = invite
                            .map(|invite| Box::new(InviteDeliveryFailure::RemoveLedger(invite)));
                    }
                }
            }
            _ => {}
        }
    }
}

fn owns_membership(failure: &InviteDeliveryFailure, room: &BareJid, member: &BareJid) -> bool {
    match failure {
        InviteDeliveryFailure::RollbackMucMembership(grant)
        | InviteDeliveryFailure::RollbackMuc { grant, .. } => {
            &grant.room == room && &grant.invitee == member
        }
        InviteDeliveryFailure::RollbackGroupDm { grant, .. } => {
            &grant.grant.room == room && &grant.grant.invitee == member
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::{
        handlers::message::muc_invite::MucMembershipMutation, muc_invites::OutstandingInvite,
    };
    use kameo::actor::Spawn;
    use waddle_xmpp::{
        muc::{room_actor::RoomActor, MucRoom, RoomConfig},
        pending_delivery::{PendingPayload, PendingRow, PendingRowId},
        xep::xep0421::OccupantIdSecret,
    };

    #[tokio::test]
    async fn preserved_membership_removes_only_its_own_compensation() {
        let invite = OutstandingInvite {
            room: "room@muc.example.com".parse().expect("room"),
            invitee: "bob@example.com".parse().expect("member"),
            inviter: "alice@example.com".parse().expect("inviter"),
        };
        let actor = RoomActor::spawn(RoomActor::new(
            MucRoom::new(
                invite.room.clone(),
                "test".into(),
                "room".into(),
                RoomConfig::default(),
            ),
            OccupantIdSecret::new(vec![b't'; 32]).expect("secret"),
        ));
        let grant = Box::new(MucMembershipMutation {
            room: invite.room.clone(),
            invitee: invite.invitee.clone(),
            actor: actor.clone(),
            previous_affiliation: waddle_xmpp::Affiliation::None,
        });
        let mut ledger = PlannedEffect::new(Effect::External(ExternalEffect::InviteLedger(
            InviteLedgerMutation::Record {
                invite: invite.clone(),
                recorded_at: chrono::Utc::now(),
                failure: Some(Box::new(InviteDeliveryFailure::RollbackMucMembership(
                    grant.clone(),
                ))),
            },
        )));
        let message = Box::new(xmpp_parsers::message::Message::new(Some(
            invite.invitee.clone().into(),
        )));
        let mut route = PlannedEffect::new(Effect::External(ExternalEffect::QueueOfflineDelivery(
            super::super::invite::MucUserRoute {
                recipient: invite.invitee.clone(),
                resources: vec![],
                fallback: PendingRow {
                    id: PendingRowId::fresh(),
                    recipient: invite.invitee.clone(),
                    original_receipt_at: chrono::Utc::now(),
                    payload: PendingPayload::Transient(message.clone()),
                    flushed_in_session: None,
                    outbound_sequence: None,
                },
                message,
                failure: Some(Box::new(InviteDeliveryFailure::RollbackMuc {
                    grant,
                    invite: invite.clone(),
                })),
            },
        )));
        for effect in [&mut ledger, &mut route] {
            effect.resolve_membership_outcome(
                &invite.room,
                &invite.invitee,
                MembershipOutcome::Granted {
                    previous_affiliation: waddle_xmpp::Affiliation::Outcast,
                },
            );
            effect.resolve_membership_outcome(
                &invite.room,
                &invite.inviter,
                MembershipOutcome::Preserved,
            );
        }
        assert!(
            matches!(&ledger.effect, Effect::External(ExternalEffect::InviteLedger(
            InviteLedgerMutation::Record { failure: Some(failure), .. }
        )) if matches!(failure.as_ref(), InviteDeliveryFailure::RollbackMucMembership(grant)
            if grant.previous_affiliation == waddle_xmpp::Affiliation::Outcast))
        );
        assert!(
            matches!(&route.effect, Effect::External(ExternalEffect::QueueOfflineDelivery(route))
            if matches!(route.failure.as_deref(), Some(InviteDeliveryFailure::RollbackMuc { grant, .. })
                if grant.previous_affiliation == waddle_xmpp::Affiliation::Outcast))
        );
        for effect in [&mut ledger, &mut route] {
            effect.resolve_membership_outcome(
                &invite.room,
                &invite.invitee,
                MembershipOutcome::Preserved,
            );
        }
        assert!(matches!(
            &ledger.effect,
            Effect::External(ExternalEffect::InviteLedger(InviteLedgerMutation::Record {
                failure: None,
                ..
            }))
        ));
        assert!(
            matches!(&route.effect, Effect::External(ExternalEffect::QueueOfflineDelivery(route))
            if matches!(route.failure.as_deref(), Some(InviteDeliveryFailure::RemoveLedger(found)) if found == &invite))
        );
        actor.kill();
    }
}
