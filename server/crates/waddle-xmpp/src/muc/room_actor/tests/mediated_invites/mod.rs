use super::*;
use crate::muc::room_actor::mediated_invites::MAX_RETAINED_MEDIATED_INVITE_OPERATIONS;

fn invite_operation_id() -> MediatedInviteOperationId {
    MediatedInviteOperationId::generate()
}

struct GetMediatedInviteOperationCount;

impl kameo::message::Message<GetMediatedInviteOperationCount> for RoomActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: GetMediatedInviteOperationCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.invite_operations.len()
    }
}

async fn joined_members_only_invite_actor() -> (ActorRef<RoomActor>, FullJid, BareJid) {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    })
    .await;
    let inviter = test_full_jid("inviter");
    let invitee = test_full_jid("invitee").to_bare();
    actor
        .ask(Join {
            nick: "inviter".to_string(),
            real_jid: inviter.clone(),
            role: Role::Moderator,
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("join inviter");
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("persist inviter admin affiliation");
    (actor, inviter, invitee)
}

async fn authorize_invite_grant(
    actor: &ActorRef<RoomActor>,
    inviter: FullJid,
    invitee: BareJid,
) -> InviteMembershipGrant {
    actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter,
            invitee,
        })
        .await
        .expect("authorize mediated invite")
        .grant
        .expect("actor-created temporary membership")
}

mod affiliation_fencing;
mod authorization;
mod durability;
mod lifecycle;
