use super::{
    current_admission_revision, departed, presence_has_status, test_claim_fence,
    test_full_jid_resource, test_room, test_secret, AbortMediatedInviteGrantRollback,
    AckDepartureOutcome, AckDepartureReceipt, AcknowledgeMediatedInviteOperation, AdminApplyError,
    AdminItem, AffiliationMutationError, ApplyAdminItems, ApplyAffiliationChange,
    CommitMediatedInviteGrantRollback, ConfigEffectPlan, DestroyAttemptId, DurablePersistError,
    EnforceMembersOnlyAffiliations, FailNthAffiliationSaveStore, FakeDurableStore,
    FinalizeMediatedInviteGrant, FlakyThenRecoveringStore, GetAffiliation, GetConfig,
    GetOccupantByJid, GetSnapshot, IsDormant, JoinAffiliationGrant, JoinDenialReason,
    JoinWithAffiliation, LeaveAttemptId, LeaveByRealJid, LeaveOrigin, LeaveSessionSelector,
    MediatedInviteGrantError, MediatedInviteGrantFinalization,
    MediatedInviteOperationAcknowledgement, MediatedInviteRollbackAbort,
    MediatedInviteRollbackCommit, MediatedInviteRollbackError, MediatedInviteRollbackPreparation,
    MucRoom, PrepareMediatedInviteGrantRollback, ResolverAffiliationSyncOutcome,
    RestoreDurableRoomState, RoomActorError, SealForDestroy, SealGuard, SealIfInactive,
    SealIfInactiveOutcome, SendError, SyncResolverAffiliation, UnsealDestroy, UpdateConfig,
    UpdateGroupDmConfigByMember,
};
use super::{
    spawn_room_actor_with_config, test_full_jid, ActorRef, Affiliation, AuthorizeMediatedInvite,
    BareJid, ChangeAffiliation, Context, FullJid, InviteMembershipGrant, Join,
    MediatedInviteOperationId, OccupancySessionGeneration, Role, RoomActor, RoomConfig,
};
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
            session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
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
