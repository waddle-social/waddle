use super::*;

pub struct FinalizeMediatedInviteGrant {
    pub operation_id: MediatedInviteOperationId,
}

impl kameo::message::Message<FinalizeMediatedInviteGrant> for RoomActor {
    type Reply = MediatedInviteGrantFinalization;

    async fn handle(
        &mut self,
        msg: FinalizeMediatedInviteGrant,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(record) = self.invite_operations.get_mut(&msg.operation_id) else {
            return MediatedInviteGrantFinalization::Superseded;
        };
        let mut release_invitee = None;
        let outcome = match record.state {
            MediatedInviteOperationState::Active => {
                let had_grant = record.authorization.grant.is_some();
                record.state = MediatedInviteOperationState::Finalized(if had_grant {
                    MediatedInviteOperationCompletion::Completed
                } else {
                    MediatedInviteOperationCompletion::NoGrantRequired
                });
                if had_grant {
                    release_invitee = Some(record.invitee.clone());
                }
                MediatedInviteGrantFinalization::Finalized
            }
            MediatedInviteOperationState::Prepared
            | MediatedInviteOperationState::RolledBackUnacknowledged(_) => {
                MediatedInviteGrantFinalization::RollbackPending
            }
            MediatedInviteOperationState::Finalized(
                MediatedInviteOperationCompletion::Completed
                | MediatedInviteOperationCompletion::NoGrantRequired,
            ) => MediatedInviteGrantFinalization::Finalized,
            MediatedInviteOperationState::Finalized(
                MediatedInviteOperationCompletion::Superseded,
            ) => MediatedInviteGrantFinalization::Superseded,
        };
        if let Some(invitee) = release_invitee {
            self.release_invitee_operation(&invitee, msg.operation_id);
        }
        outcome
    }
}

pub struct AcknowledgeMediatedInviteOperation {
    pub operation_id: MediatedInviteOperationId,
}

impl kameo::message::Message<AcknowledgeMediatedInviteOperation> for RoomActor {
    type Reply = MediatedInviteOperationAcknowledgement;

    async fn handle(
        &mut self,
        msg: AcknowledgeMediatedInviteOperation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(record) = self.invite_operations.get(&msg.operation_id) else {
            return MediatedInviteOperationAcknowledgement::Unknown;
        };
        if matches!(
            record.state,
            MediatedInviteOperationState::Active | MediatedInviteOperationState::Prepared
        ) {
            return MediatedInviteOperationAcknowledgement::Pending;
        }
        let record = self
            .invite_operations
            .remove(&msg.operation_id)
            .expect("record was just observed");
        self.release_invitee_operation(&record.invitee, msg.operation_id);
        MediatedInviteOperationAcknowledgement::Acknowledged
    }
}
