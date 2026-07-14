use super::*;

pub struct AuthorizeMediatedInvite {
    pub operation_id: MediatedInviteOperationId,
    pub inviter: FullJid,
    pub invitee: BareJid,
}

impl kameo::message::Message<AuthorizeMediatedInvite> for RoomActor {
    type Reply = Result<MediatedInviteAuthorized, MediatedInviteGrantError>;

    async fn handle(
        &mut self,
        msg: AuthorizeMediatedInvite,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(record) = self.invite_operations.get(&msg.operation_id) {
            return if record.inviter == msg.inviter && record.invitee == msg.invitee {
                Ok(record.authorization.clone())
            } else {
                Err(MediatedInviteGrantError::OperationMismatch)
            };
        }
        if self.seal_state.is_sealed() {
            return Err(MediatedInviteGrantError::RoomSealed);
        }
        self.gate_mutation().await?;
        if self.ensure_restored_before_join().await.is_err() {
            return Err(MediatedInviteGrantError::RestorePending);
        }
        if self.room.find_nick_by_real_jid(&msg.inviter).is_none() {
            return Err(MediatedInviteGrantError::NotOccupant);
        }
        let inviter_affiliation = self.room.get_affiliation(&msg.inviter.to_bare());
        let lacks_required_affiliation = if self.room.config.group_dm {
            inviter_affiliation < Affiliation::Member
        } else if self.room.config.members_only {
            inviter_affiliation < Affiliation::Admin
        } else {
            false
        };
        if lacks_required_affiliation {
            return Err(MediatedInviteGrantError::Forbidden);
        }
        let membership_grant_is_relevant = self.room.config.requires_membership();
        if membership_grant_is_relevant
            && self.invite_operation_by_invitee.contains_key(&msg.invitee)
        {
            return Err(MediatedInviteGrantError::GrantPending);
        }
        let previous_affiliation = self.room.get_affiliation(&msg.invitee);
        if previous_affiliation == Affiliation::Outcast {
            return Err(MediatedInviteGrantError::InviteeBanned);
        }
        if self.room.config.group_dm && previous_affiliation >= Affiliation::Member {
            return Err(MediatedInviteGrantError::InviteeAlreadyMember);
        }
        if !self.has_invite_operation_capacity() {
            return Err(MediatedInviteGrantError::OperationCapacityReached);
        }
        let authorization = if !membership_grant_is_relevant {
            MediatedInviteAuthorized {
                grant: None,
                invitee_affiliation: previous_affiliation,
                members_only: false,
            }
        } else if previous_affiliation >= Affiliation::Member {
            MediatedInviteAuthorized {
                grant: None,
                invitee_affiliation: previous_affiliation,
                members_only: true,
            }
        } else {
            // The durable affiliation is authoritative across actor restarts.
            // Persist first; only a successful write permits the infallible
            // in-memory transition and creation of rollback authority.
            self.persist_affiliation(&msg.invitee, Affiliation::Member)
                .await
                .map_err(MediatedInviteGrantError::GrantPersistFailed)?;
            let changed = self
                .room
                .set_affiliation(msg.invitee.clone(), Affiliation::Member);
            debug_assert!(changed.is_some(), "a below-Member affiliation must change");
            self.admission_revision = self.admission_revision.saturating_add(1);
            MediatedInviteAuthorized {
                grant: Some(InviteMembershipGrant {
                    operation_id: msg.operation_id,
                    invitee: msg.invitee.clone(),
                    previous_affiliation,
                }),
                invitee_affiliation: Affiliation::Member,
                members_only: membership_grant_is_relevant,
            }
        };
        if authorization.grant.is_some() {
            self.invite_operation_by_invitee
                .insert(msg.invitee.clone(), msg.operation_id);
        }
        let operation_state = if authorization.grant.is_some() {
            MediatedInviteOperationState::Active
        } else {
            MediatedInviteOperationState::Finalized(
                MediatedInviteOperationCompletion::NoGrantRequired,
            )
        };
        self.invite_operations.insert(
            msg.operation_id,
            MediatedInviteOperationRecord {
                inviter: msg.inviter,
                invitee: msg.invitee,
                authorization: authorization.clone(),
                state: operation_state,
            },
        );
        Ok(authorization)
    }
}
