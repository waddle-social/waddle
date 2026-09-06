//! Typed dependency matching and execution-time completion proofs.
use crate::server::routes::{
    interpret::effects::{
        early::RoomMembershipMutation, EffectOutcome, ExternalEffect, PlanEffectDependency,
    },
    websocket::{
        handlers::message::muc_invite::{InviteLedgerMutation, InviteLedgerOutcome},
        muc_invites::RecordOutcome,
    },
};

pub(super) fn produces(effect: &ExternalEffect, dependency: &PlanEffectDependency) -> bool {
    match (effect, dependency) {
        (
            ExternalEffect::RoomMembershipMutation(mutation),
            PlanEffectDependency::AfterRoomMembership { room, member },
        ) => {
            let (actual_room, actual_member) = membership_identity(mutation);
            room == actual_room && member == actual_member
        }
        (
            ExternalEffect::InviteLedger(mutation),
            PlanEffectDependency::AfterInviteLedger { invite },
        ) => {
            let (InviteLedgerMutation::Record { invite: actual, .. }
            | InviteLedgerMutation::Claim { invite: actual }) = mutation;
            actual == invite
        }
        (
            ExternalEffect::DmPinMutation(mutation),
            PlanEffectDependency::AfterDmPinMutation { pair, target },
        ) => &mutation.pair == pair && &mutation.target_stanza_id == target,
        _ => false,
    }
}

pub(super) fn membership_identity(
    mutation: &RoomMembershipMutation,
) -> (&jid::BareJid, &jid::BareJid) {
    match mutation {
        RoomMembershipMutation::Muc(grant) => (&grant.room, &grant.invitee),
        RoomMembershipMutation::GroupDm(mutation) => {
            (&mutation.grant.room, &mutation.grant.invitee)
        }
    }
}

pub(super) fn permits_dependents(effect: &ExternalEffect, outcome: &EffectOutcome) -> bool {
    matches!(
        (effect, outcome),
        (
            ExternalEffect::RoomMembershipMutation(_),
            EffectOutcome::Membership(_)
        ) | (ExternalEffect::DmPinMutation(_), EffectOutcome::Completed)
            | (
                ExternalEffect::InviteLedger(_),
                EffectOutcome::InviteLedger(Ok(InviteLedgerOutcome::Recorded(
                    RecordOutcome::New { .. }
                ) | InviteLedgerOutcome::Claimed(true)))
            )
    )
}

/// None means a predecessor is still pending. Missing predecessors fail closed.
pub(super) fn ready(
    dependencies: &[PlanEffectDependency],
    effects: &[ExternalEffect],
    completed: &[Option<bool>],
) -> Option<bool> {
    let mut ready = true;
    for dependency in dependencies {
        if matches!(dependency, PlanEffectDependency::AfterArchive { .. }) {
            continue; // Phase B has already committed all durable dependencies.
        }
        let predecessors = effects
            .iter()
            .enumerate()
            .filter(|(_, effect)| produces(effect, dependency))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if predecessors.is_empty() {
            ready = false;
        }
        for index in predecessors {
            ready &= completed[index]?;
        }
    }
    Some(ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ledger_noop_and_missing_predecessor_do_not_release_delivery() {
        let invite = crate::server::routes::websocket::muc_invites::OutstandingInvite {
            room: "room@muc.example.com".parse().expect("room"),
            invitee: "invitee@example.com".parse().expect("invitee"),
            inviter: "inviter@example.com".parse().expect("inviter"),
        };
        let effect = ExternalEffect::InviteLedger(InviteLedgerMutation::Claim {
            invite: invite.clone(),
        });
        let dependency = PlanEffectDependency::AfterInviteLedger { invite };
        assert_eq!(
            ready(
                std::slice::from_ref(&dependency),
                std::slice::from_ref(&effect),
                &[None]
            ),
            None
        );
        assert_eq!(
            ready(
                std::slice::from_ref(&dependency),
                std::slice::from_ref(&effect),
                &[Some(true)]
            ),
            Some(true)
        );
        assert_eq!(
            ready(
                std::slice::from_ref(&dependency),
                std::slice::from_ref(&effect),
                &[Some(false)]
            ),
            Some(false)
        );
        assert_eq!(ready(&[dependency], &[], &[]), Some(false));
        for outcome in [
            InviteLedgerOutcome::Claimed(false),
            InviteLedgerOutcome::Recorded(RecordOutcome::AlreadyOutstanding),
        ] {
            assert!(!permits_dependents(
                &effect,
                &EffectOutcome::InviteLedger(Ok(outcome))
            ));
        }
    }
}
