use super::*;

/// Enter the fail-closed compensation phase for one temporary grant.
///
/// Before sending this message, callers must durably retain the operation id,
/// inviter, and invitee. A prepared operation is intentionally never expired
/// or auto-aborted: an external managed-membership write may have committed
/// even when its reply was lost. Recovery replays [`AuthorizeMediatedInvite`]
/// with the same operation id and exact inputs to recover the grant, then
/// either retries [`CommitMediatedInviteGrantRollback`] or restores the
/// external Member relation before [`AbortMediatedInviteGrantRollback`].
/// An [`MediatedInviteRollbackPreparation::AlreadyRolledBack`] reply means
/// the exact rollback output remains cached after a lost reply. The caller
/// must replay [`CommitMediatedInviteGrantRollback`], deliver that output,
/// and acknowledge the operation only after delivery.
pub struct PrepareMediatedInviteGrantRollback {
    pub grant: InviteMembershipGrant,
}

impl kameo::message::Message<PrepareMediatedInviteGrantRollback> for RoomActor {
    type Reply = Result<MediatedInviteRollbackPreparation, RoomMutationError>;

    async fn handle(
        &mut self,
        msg: PrepareMediatedInviteGrantRollback,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self
            .invite_operations
            .get(&msg.grant.operation_id)
            .is_some_and(|record| {
                record.authorization.grant.as_ref() == Some(&msg.grant)
                    && matches!(
                        record.state,
                        MediatedInviteOperationState::RolledBackUnacknowledged(_)
                    )
            })
        {
            return Ok(MediatedInviteRollbackPreparation::AlreadyRolledBack);
        }
        if self.invite_rollback_is_prepared(&msg.grant) {
            return Ok(MediatedInviteRollbackPreparation::Prepared);
        }
        if !self.invite_grant_is_active(&msg.grant)
            || self.room.get_affiliation(&msg.grant.invitee) != Affiliation::Member
        {
            return Ok(MediatedInviteRollbackPreparation::Superseded);
        }
        self.gate_mutation().await?;
        self.invite_operations
            .get_mut(&msg.grant.operation_id)
            .expect("active grant has an operation record")
            .state = MediatedInviteOperationState::Prepared;
        Ok(MediatedInviteRollbackPreparation::Prepared)
    }
}

pub struct CommitMediatedInviteGrantRollback {
    pub grant: InviteMembershipGrant,
}

impl kameo::message::Message<CommitMediatedInviteGrantRollback> for RoomActor {
    type Reply = Result<MediatedInviteRollbackCommit, MediatedInviteRollbackError>;

    async fn handle(
        &mut self,
        msg: CommitMediatedInviteGrantRollback,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(record) = self.invite_operations.get(&msg.grant.operation_id) else {
            return Ok(MediatedInviteRollbackCommit::Superseded);
        };
        if record.authorization.grant.as_ref() != Some(&msg.grant) {
            return Ok(MediatedInviteRollbackCommit::Superseded);
        }
        match &record.state {
            MediatedInviteOperationState::RolledBackUnacknowledged(outcome) => {
                return Ok(outcome.clone());
            }
            MediatedInviteOperationState::Prepared => {}
            MediatedInviteOperationState::Active => {
                return Ok(MediatedInviteRollbackCommit::Superseded);
            }
            MediatedInviteOperationState::Finalized(_) => {
                return Ok(MediatedInviteRollbackCommit::Superseded);
            }
        }
        if self.room.get_affiliation(&msg.grant.invitee) != Affiliation::Member {
            return Ok(MediatedInviteRollbackCommit::Superseded);
        }
        match self.gate_mutation().await {
            Ok(()) => {}
            Err(RoomMutationError::NotOwner) => {
                return Err(MediatedInviteRollbackError::NotOwner);
            }
            Err(RoomMutationError::OwnershipUnavailable) => {
                return Err(MediatedInviteRollbackError::OwnershipUnavailable);
            }
            Err(RoomMutationError::PersistFailed) => {
                return Err(MediatedInviteRollbackError::OwnershipUnavailable);
            }
        }
        self.commit_durable(
            crate::muc::durable::RoomDurableMutation::MediatedInviteRollback(
                crate::muc::durable::AffiliationEntry::new(
                    msg.grant.invitee.clone(),
                    (msg.grant.previous_affiliation != Affiliation::None)
                        .then_some(msg.grant.previous_affiliation),
                ),
            ),
        )
        .await
        .map_err(MediatedInviteRollbackError::PersistFailedBeforeApply)?;

        // Token construction proves `previous_affiliation < Member`, so
        // the existing last-owner guard cannot reject this transition.
        // Persistence has already succeeded; the remaining memory update
        // is therefore infallible and emits the canonical status-321/ban
        // presence through the same guarded path as admin changes.
        let updates = super::super::admin_handlers::apply_affiliation_change(
            &mut self.room,
            &self.occupant_id_secret,
            msg.grant.invitee.clone(),
            msg.grant.previous_affiliation,
            None,
            None,
        )
        .expect("invite rollback restores only a below-Member affiliation");
        let needs_rehydration = self
            .prune_durable_recipient_if_removed(&msg.grant.invitee, msg.grant.previous_affiliation);
        self.advance_member_admission_revision(&msg.grant.invitee);
        if needs_rehydration {
            self.refresh_durable_recipients_from_source().await;
        }
        let outcome = MediatedInviteRollbackCommit::Applied {
            previous_affiliation: msg.grant.previous_affiliation,
            updates,
        };
        self.invite_operations
            .get_mut(&msg.grant.operation_id)
            .expect("prepared grant has an operation record")
            .state = MediatedInviteOperationState::RolledBackUnacknowledged(outcome.clone());
        Ok(outcome)
    }
}

pub struct AbortMediatedInviteGrantRollback {
    pub grant: InviteMembershipGrant,
}

impl kameo::message::Message<AbortMediatedInviteGrantRollback> for RoomActor {
    type Reply = MediatedInviteRollbackAbort;

    async fn handle(
        &mut self,
        msg: AbortMediatedInviteGrantRollback,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.seal_state.is_sealed() {
            return MediatedInviteRollbackAbort::RoomSealed;
        }
        if self.invite_rollback_is_prepared(&msg.grant) {
            self.invite_operations
                .get_mut(&msg.grant.operation_id)
                .expect("prepared grant has an operation record")
                .state = MediatedInviteOperationState::Active;
            MediatedInviteRollbackAbort::Aborted
        } else {
            MediatedInviteRollbackAbort::NotPrepared
        }
    }
}
