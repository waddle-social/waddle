use super::*;

mod authorization;
mod completion;
mod rollback;

pub use authorization::AuthorizeMediatedInvite;
pub use completion::{AcknowledgeMediatedInviteOperation, FinalizeMediatedInviteGrant};
pub use rollback::{
    AbortMediatedInviteGrantRollback, CommitMediatedInviteGrantRollback,
    PrepareMediatedInviteGrantRollback,
};

/// Maximum actor-local idempotency and rollback records retained for one room.
///
/// Records are never evicted while the actor remains alive: forgetting a known
/// id would let a delayed retry be re-authorized under changed room policy.
/// Once the bound is full, known ids still replay and new ids fail closed until
/// a caller acknowledges one or an otherwise safe room eviction drops the
/// actor-local replay cache.
pub(super) const MAX_RETAINED_MEDIATED_INVITE_OPERATIONS: usize = 256;

/// Caller-chosen idempotency key for one mediated-invite operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MediatedInviteOperationId(uuid::Uuid);

impl MediatedInviteOperationId {
    pub const fn new(id: uuid::Uuid) -> Self {
        Self(id)
    }

    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    pub const fn as_uuid(self) -> uuid::Uuid {
        self.0
    }
}

/// Opaque authority to compensate the exact temporary membership grant
/// created by one mediated-invite authorization turn.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InviteMembershipGrant {
    operation_id: MediatedInviteOperationId,
    invitee: BareJid,
    previous_affiliation: Affiliation,
}

impl InviteMembershipGrant {
    pub fn operation_id(&self) -> MediatedInviteOperationId {
        self.operation_id
    }

    pub fn invitee(&self) -> &BareJid {
        &self.invitee
    }

    pub fn previous_affiliation(&self) -> Affiliation {
        self.previous_affiliation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediatedInviteAuthorized {
    pub grant: Option<InviteMembershipGrant>,
    pub invitee_affiliation: Affiliation,
    pub members_only: bool,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum MediatedInviteGrantError {
    #[error("the mediated-invite operation id was already used with different inputs")]
    OperationMismatch,
    #[error("the room actor is sealed pending destruction")]
    RoomSealed,
    #[error("the inviter is not an occupant of this room")]
    NotOccupant,
    #[error("the inviter may not invite users under this room's policy")]
    Forbidden,
    #[error("the invitee is banned from this room")]
    InviteeBanned,
    #[error("the invitee is already a member of this group DM")]
    InviteeAlreadyMember,
    #[error("another mediated invite owns a temporary membership grant for this invitee")]
    GrantPending,
    #[error("durable room-state restore has not yet completed; retry")]
    RestorePending,
    #[error("the temporary membership grant could not be persisted before commit: {0}")]
    GrantPersistFailed(#[source] DurablePersistError),
    #[error("the room has too many unresolved mediated-invite operations")]
    OperationCapacityReached,
    #[error(transparent)]
    Mutation(#[from] RoomMutationError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediatedInviteRollbackPreparation {
    Prepared,
    /// The rollback committed, but its exact output is still cached and unacknowledged.
    ///
    /// Callers must replay [`CommitMediatedInviteGrantRollback`], deliver the
    /// returned effects, and only then send [`AcknowledgeMediatedInviteOperation`].
    AlreadyRolledBack,
    Superseded,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediatedInviteRollbackCommit {
    Applied {
        previous_affiliation: Affiliation,
        updates: AdminItemsApplied,
    },
    Superseded,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum MediatedInviteRollbackError {
    #[error("this room is no longer serviceable by this actor")]
    NotOwner,
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
    #[error("the invite rollback could not be persisted before it was applied: {0}")]
    PersistFailedBeforeApply(#[source] DurablePersistError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum MediatedInviteRollbackAbort {
    Aborted,
    NotPrepared,
    /// The room is sealed for eviction or destruction; retry through the
    /// lifecycle owner rather than altering rollback authority in place.
    RoomSealed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum MediatedInviteGrantFinalization {
    Finalized,
    RollbackPending,
    Superseded,
    /// The room is sealed for eviction or destruction; its idempotency record
    /// remains intact for recovery.
    RoomSealed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum MediatedInviteOperationAcknowledgement {
    Acknowledged,
    Pending,
    Unknown,
    /// The room is sealed for eviction or destruction; the operation record
    /// remains retained rather than being acknowledged away.
    RoomSealed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediatedInviteOperationCompletion {
    Completed,
    NoGrantRequired,
    Superseded,
}

#[derive(Debug, Clone, PartialEq)]
enum MediatedInviteOperationState {
    Active,
    Prepared,
    RolledBackUnacknowledged(MediatedInviteRollbackCommit),
    Finalized(MediatedInviteOperationCompletion),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct MediatedInviteOperationRecord {
    inviter: FullJid,
    invitee: BareJid,
    authorization: MediatedInviteAuthorized,
    state: MediatedInviteOperationState,
}

impl RoomActor {
    pub(super) fn has_lifecycle_fenced_invite_operation(&self) -> bool {
        self.invite_operations.values().any(|record| {
            matches!(
                &record.state,
                MediatedInviteOperationState::Prepared
                    | MediatedInviteOperationState::Active
                    | MediatedInviteOperationState::RolledBackUnacknowledged(_)
            ) && record.authorization.grant.is_some()
        })
    }

    fn has_invite_operation_capacity(&self) -> bool {
        self.invite_operations.len() < MAX_RETAINED_MEDIATED_INVITE_OPERATIONS
    }

    fn invite_grant_is_active(&self, grant: &InviteMembershipGrant) -> bool {
        self.invite_operations
            .get(&grant.operation_id)
            .is_some_and(|record| {
                record.authorization.grant.as_ref() == Some(grant)
                    && record.state == MediatedInviteOperationState::Active
            })
    }

    fn invite_rollback_is_prepared(&self, grant: &InviteMembershipGrant) -> bool {
        self.invite_operations
            .get(&grant.operation_id)
            .is_some_and(|record| {
                record.authorization.grant.as_ref() == Some(grant)
                    && record.state == MediatedInviteOperationState::Prepared
            })
    }

    pub(super) fn invite_rollback_pending(&self, jid: &BareJid) -> bool {
        self.invite_operation_by_invitee
            .get(jid)
            .and_then(|operation_id| self.invite_operations.get(operation_id))
            .is_some_and(|record| {
                matches!(
                    record.state,
                    MediatedInviteOperationState::Prepared
                        | MediatedInviteOperationState::RolledBackUnacknowledged(_)
                )
            })
    }

    fn release_invitee_operation(
        &mut self,
        invitee: &BareJid,
        operation_id: MediatedInviteOperationId,
    ) {
        if self.invite_operation_by_invitee.get(invitee) == Some(&operation_id) {
            self.invite_operation_by_invitee.remove(invitee);
        }
    }

    pub(super) fn invalidate_invite_grant(&mut self, jid: &BareJid) {
        let Some(operation_id) = self.invite_operation_by_invitee.get(jid).copied() else {
            return;
        };
        let Some(record) = self.invite_operations.get(&operation_id) else {
            self.invite_operation_by_invitee.remove(jid);
            return;
        };
        if record.state != MediatedInviteOperationState::Active {
            return;
        }
        self.invite_operations
            .get_mut(&operation_id)
            .expect("record was just observed")
            .state =
            MediatedInviteOperationState::Finalized(MediatedInviteOperationCompletion::Superseded);
        self.release_invitee_operation(jid, operation_id);
    }
}
