use super::*;

#[tokio::test]
async fn mediated_invite_operation_replays_exact_authorization_without_a_second_save() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let store = FakeDurableStore::owned();
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
            claim_fence: test_claim_fence(&test_room().room_jid),
        })
        .await
        .expect("attach durable store");
    let operation_id = invite_operation_id();

    let first = actor
        .ask(AuthorizeMediatedInvite {
            operation_id,
            inviter: inviter.clone(),
            invitee: invitee.clone(),
        })
        .await
        .expect("first authorization");
    assert_eq!(store.save_call_count(), 1);
    let replay = actor
        .ask(AuthorizeMediatedInvite {
            operation_id,
            inviter: inviter.clone(),
            invitee: invitee.clone(),
        })
        .await
        .expect("replayed authorization");

    assert_eq!(replay, first);
    assert_eq!(store.save_call_count(), 1, "replay must not persist twice");
    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id,
                inviter: inviter.clone(),
                invitee: "different@example.com".parse().expect("bare jid"),
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::OperationMismatch
        ))
    ));
    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter,
                invitee,
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::GrantPending
        ))
    ));
}

#[tokio::test]
async fn mediated_invite_prepare_abort_finalize_and_ack_are_idempotent() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let operation_id = invite_operation_id();
    let grant = actor
        .ask(AuthorizeMediatedInvite {
            operation_id,
            inviter,
            invitee: invitee.clone(),
        })
        .await
        .expect("authorize")
        .grant
        .expect("temporary grant");

    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("active acknowledgement attempt"),
        MediatedInviteOperationAcknowledgement::Pending,
    );

    for _ in 0..2 {
        assert_eq!(
            actor
                .ask(PrepareMediatedInviteGrantRollback {
                    grant: grant.clone(),
                })
                .await
                .expect("prepare"),
            MediatedInviteRollbackPreparation::Prepared,
        );
    }
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("prepared acknowledgement attempt"),
        MediatedInviteOperationAcknowledgement::Pending,
    );
    assert_eq!(
        actor
            .ask(FinalizeMediatedInviteGrant { operation_id })
            .await
            .expect("finalize prepared operation"),
        MediatedInviteGrantFinalization::RollbackPending,
    );
    assert_eq!(
        actor
            .ask(AbortMediatedInviteGrantRollback {
                grant: grant.clone(),
            })
            .await
            .expect("abort"),
        MediatedInviteRollbackAbort::Aborted,
    );
    assert_eq!(
        actor
            .ask(AbortMediatedInviteGrantRollback {
                grant: grant.clone(),
            })
            .await
            .expect("replay abort"),
        MediatedInviteRollbackAbort::NotPrepared,
    );
    assert_eq!(
        actor
            .ask(PrepareMediatedInviteGrantRollback {
                grant: grant.clone(),
            })
            .await
            .expect("prepare after abort"),
        MediatedInviteRollbackPreparation::Prepared,
    );
    assert_eq!(
        actor
            .ask(AbortMediatedInviteGrantRollback { grant })
            .await
            .expect("return to active"),
        MediatedInviteRollbackAbort::Aborted,
    );
    assert_eq!(
        actor
            .ask(FinalizeMediatedInviteGrant { operation_id })
            .await
            .expect("finalize"),
        MediatedInviteGrantFinalization::Finalized,
    );
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: invitee })
            .await
            .expect("membership after finalization"),
        Affiliation::Member,
    );
    assert_eq!(
        actor
            .ask(FinalizeMediatedInviteGrant { operation_id })
            .await
            .expect("replay finalize"),
        MediatedInviteGrantFinalization::Finalized,
    );
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("acknowledge"),
        MediatedInviteOperationAcknowledgement::Acknowledged,
    );
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("replay acknowledgement"),
        MediatedInviteOperationAcknowledgement::Unknown,
    );
}

#[tokio::test]
async fn destroy_seal_preserves_mediated_invite_lifecycle_bookkeeping() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let operation_id = invite_operation_id();
    let grant = actor
        .ask(AuthorizeMediatedInvite {
            operation_id,
            inviter,
            invitee,
        })
        .await
        .expect("authorize")
        .grant
        .expect("temporary grant");
    actor
        .ask(PrepareMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("prepare rollback");
    let attempt = crate::muc::DestroyAttemptId::generate();
    actor
        .ask(SealForDestroy { attempt })
        .await
        .expect("seal room");

    assert_eq!(
        actor
            .ask(FinalizeMediatedInviteGrant { operation_id })
            .await
            .expect("sealed finalization"),
        MediatedInviteGrantFinalization::RoomSealed,
    );
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("sealed acknowledgement"),
        MediatedInviteOperationAcknowledgement::RoomSealed,
    );
    assert_eq!(
        actor
            .ask(AbortMediatedInviteGrantRollback {
                grant: grant.clone(),
            })
            .await
            .expect("sealed abort"),
        MediatedInviteRollbackAbort::RoomSealed,
    );
    assert!(actor
        .ask(UnsealDestroy { attempt })
        .await
        .expect("unseal matching attempt"),);
    assert_eq!(
        actor
            .ask(FinalizeMediatedInviteGrant { operation_id })
            .await
            .expect("finalization after unseal"),
        MediatedInviteGrantFinalization::RollbackPending,
        "the sealed calls must not advance or discard the prepared operation",
    );
    assert_eq!(
        actor
            .ask(AbortMediatedInviteGrantRollback { grant })
            .await
            .expect("abort after unseal"),
        MediatedInviteRollbackAbort::Aborted,
    );
}

#[tokio::test]
async fn mediated_invite_rollback_commit_replays_exact_outcome_until_acknowledged() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let store = FakeDurableStore::owned();
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
            claim_fence: test_claim_fence(&test_room().room_jid),
        })
        .await
        .expect("attach recording durable store");
    let operation_id = invite_operation_id();
    let grant = actor
        .ask(AuthorizeMediatedInvite {
            operation_id,
            inviter,
            invitee: invitee.clone(),
        })
        .await
        .expect("authorize")
        .grant
        .expect("temporary grant");
    assert_eq!(store.save_call_count(), 1, "grant persists exactly once");
    actor
        .ask(PrepareMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("prepare");

    let first = actor
        .ask(CommitMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("commit");
    assert_eq!(store.save_call_count(), 2, "rollback persists exactly once");
    let recovery_prepare = actor
        .ask(PrepareMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("replay prepare after a lost commit reply");
    assert_eq!(
        recovery_prepare,
        MediatedInviteRollbackPreparation::AlreadyRolledBack,
        "the cached rollback outcome must remain explicit in a strict prepare-then-commit recovery loop",
    );
    let replay = actor
        .ask(CommitMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("replay commit");
    assert_eq!(replay, first);
    assert_eq!(
        store.save_call_count(),
        2,
        "lost-reply recovery must replay the cached outcome without persisting again",
    );
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: invitee })
            .await
            .expect("rolled-back affiliation"),
        Affiliation::None,
    );
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("acknowledge"),
        MediatedInviteOperationAcknowledgement::Acknowledged,
    );
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("replay acknowledgement"),
        MediatedInviteOperationAcknowledgement::Unknown,
    );
    assert_eq!(
        actor
            .ask(CommitMediatedInviteGrantRollback { grant })
            .await
            .expect("commit after acknowledgement"),
        MediatedInviteRollbackCommit::Superseded,
    );
}

#[tokio::test]
async fn delayed_ack_of_finalized_operation_does_not_release_a_newer_grant() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let old_operation_id = invite_operation_id();
    actor
        .ask(AuthorizeMediatedInvite {
            operation_id: old_operation_id,
            inviter: inviter.clone(),
            invitee: invitee.clone(),
        })
        .await
        .expect("authorize old operation");
    assert_eq!(
        actor
            .ask(FinalizeMediatedInviteGrant {
                operation_id: old_operation_id,
            })
            .await
            .expect("finalize old operation"),
        MediatedInviteGrantFinalization::Finalized,
    );
    actor
        .ask(ChangeAffiliation {
            jid: invitee.clone(),
            affiliation: Affiliation::None,
        })
        .await
        .expect("remove finalized membership");

    let newer = authorize_invite_grant(&actor, inviter.clone(), invitee.clone()).await;
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation {
                operation_id: old_operation_id,
            })
            .await
            .expect("delayed old acknowledgement"),
        MediatedInviteOperationAcknowledgement::Acknowledged,
    );
    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter,
                invitee,
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::GrantPending
        ))
    ));
    assert_eq!(
        actor
            .ask(PrepareMediatedInviteGrantRollback { grant: newer })
            .await
            .expect("newer grant still owns the invitee fence"),
        MediatedInviteRollbackPreparation::Prepared,
    );
}

#[tokio::test]
async fn no_grant_invite_operation_is_replayed_finalized_and_acknowledged() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        ..RoomConfig::default()
    })
    .await;
    let inviter = test_full_jid("inviter");
    let invitee = test_full_jid("invitee").to_bare();
    actor
        .ask(Join {
            nick: "inviter".to_string(),
            real_jid: inviter.clone(),
            role: Role::Participant,
            affiliation: Affiliation::None,
        })
        .await
        .expect("join inviter");
    let operation_id = invite_operation_id();
    let first = actor
        .ask(AuthorizeMediatedInvite {
            operation_id,
            inviter: inviter.clone(),
            invitee: invitee.clone(),
        })
        .await
        .expect("authorize open-room invite");
    assert!(first.grant.is_none());
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("acknowledge already-terminal no-grant operation"),
        MediatedInviteOperationAcknowledgement::Acknowledged,
    );
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("replay no-grant acknowledgement"),
        MediatedInviteOperationAcknowledgement::Unknown,
        "acknowledging a no-grant operation removes its replay record",
    );

    let replay_operation_id = invite_operation_id();
    let replayable = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: replay_operation_id,
            inviter: inviter.clone(),
            invitee: invitee.clone(),
        })
        .await
        .expect("authorize replayable open-room invite");
    assert_eq!(
        actor
            .ask(FinalizeMediatedInviteGrant {
                operation_id: replay_operation_id,
            })
            .await
            .expect("finalize"),
        MediatedInviteGrantFinalization::Finalized,
    );
    assert_eq!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: replay_operation_id,
                inviter,
                invitee,
            })
            .await
            .expect("replay finalized authorization"),
        replayable,
    );
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation {
                operation_id: replay_operation_id,
            })
            .await
            .expect("acknowledge"),
        MediatedInviteOperationAcknowledgement::Acknowledged,
    );
}

#[tokio::test]
async fn completed_no_grant_operations_are_bounded_without_forgetting_known_ids() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        group_dm: false,
        ..RoomConfig::default()
    })
    .await;
    let inviter = test_full_jid("inviter");
    actor
        .ask(Join {
            nick: "inviter".to_string(),
            real_jid: inviter.clone(),
            role: Role::Participant,
            affiliation: Affiliation::None,
        })
        .await
        .expect("join inviter");

    let mut operation_ids = Vec::new();
    let first_invitee: BareJid = "invitee-0@example.com".parse().expect("bare invitee JID");
    let mut first_authorization = None;
    for index in 0..MAX_RETAINED_MEDIATED_INVITE_OPERATIONS {
        let operation_id = invite_operation_id();
        operation_ids.push(operation_id);
        let authorization = actor
            .ask(AuthorizeMediatedInvite {
                operation_id,
                inviter: inviter.clone(),
                invitee: format!("invitee-{index}@example.com")
                    .parse()
                    .expect("bare invitee JID"),
            })
            .await
            .expect("bounded no-grant authorization");
        if index == 0 {
            first_authorization = Some(authorization);
        }
    }

    assert_eq!(
        actor
            .ask(GetMediatedInviteOperationCount)
            .await
            .expect("operation count"),
        MAX_RETAINED_MEDIATED_INVITE_OPERATIONS,
    );
    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter: inviter.clone(),
                invitee: "overflow@example.com".parse().expect("bare invitee JID"),
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::OperationCapacityReached
        ))
    ));
    assert_eq!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: operation_ids[0],
                inviter: inviter.clone(),
                invitee: first_invitee,
            })
            .await
            .expect("known operation replays at capacity"),
        first_authorization.expect("first authorization"),
        "capacity must not forget a known idempotency key",
    );
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation {
                operation_id: operation_ids[0],
            })
            .await
            .expect("acknowledge one completed operation"),
        MediatedInviteOperationAcknowledgement::Acknowledged,
    );
    actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter,
            invitee: "after-ack@example.com".parse().expect("bare invitee JID"),
        })
        .await
        .expect("acknowledgement releases one bounded slot");
    assert_eq!(
        actor
            .ask(GetMediatedInviteOperationCount)
            .await
            .expect("operation count after replacement"),
        MAX_RETAINED_MEDIATED_INVITE_OPERATIONS,
    );
}

#[tokio::test]
async fn no_grant_operation_does_not_pin_an_otherwise_dormant_room() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        persistent: false,
        ..RoomConfig::default()
    })
    .await;
    let inviter = test_full_jid("inviter");
    actor
        .ask(Join {
            nick: "inviter".to_string(),
            real_jid: inviter.clone(),
            role: Role::Participant,
            affiliation: Affiliation::None,
        })
        .await
        .expect("join inviter");
    actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter,
            invitee: "invitee@example.com".parse().expect("bare invitee JID"),
        })
        .await
        .expect("authorize no-grant invite");
    actor
        .ask(Leave {
            nick: "inviter".to_string(),
        })
        .await
        .expect("leave room");

    let probe = actor.ask(IsDormant).await.expect("dormancy probe");
    assert!(probe.dormant, "no-grant replay state must not pin the room");
    assert_eq!(
        actor
            .ask(SealIfInactive {
                expected_occupancy_revision: probe.occupancy_revision,
                guard: SealGuard::Dormant,
            })
            .await
            .expect("seal dormant room"),
        SealIfInactiveOutcome::Inactive,
        "no-grant replay state must not block guarded sealing",
    );
}

#[tokio::test]
async fn capacity_never_evicts_live_grants_or_persists_an_overflow_grant() {
    let (actor, inviter, _) = joined_members_only_invite_actor().await;
    let store = FakeDurableStore::owned();
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
            claim_fence: test_claim_fence(&test_room().room_jid),
        })
        .await
        .expect("attach durable store");
    let mut first_operation = None;

    for index in 0..MAX_RETAINED_MEDIATED_INVITE_OPERATIONS {
        let operation_id = invite_operation_id();
        let invitee: BareJid = format!("pending-{index}@example.com")
            .parse()
            .expect("bare invitee JID");
        let authorization = actor
            .ask(AuthorizeMediatedInvite {
                operation_id,
                inviter: inviter.clone(),
                invitee: invitee.clone(),
            })
            .await
            .expect("authorize live grant within capacity");
        if index == 0 {
            first_operation = Some((operation_id, invitee, authorization));
        }
    }
    assert_eq!(
        store.save_call_count(),
        MAX_RETAINED_MEDIATED_INVITE_OPERATIONS
    );

    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter: inviter.clone(),
                invitee: "overflow@example.com".parse().expect("bare invitee JID"),
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::OperationCapacityReached
        ))
    ));
    assert_eq!(
        store.save_call_count(),
        MAX_RETAINED_MEDIATED_INVITE_OPERATIONS,
        "capacity rejection must happen before durable persistence",
    );
    assert_eq!(
        actor
            .ask(GetMediatedInviteOperationCount)
            .await
            .expect("operation count after overflow rejection"),
        MAX_RETAINED_MEDIATED_INVITE_OPERATIONS,
        "capacity rejection must not evict a live operation",
    );

    let (first_operation_id, first_invitee, first_authorization) =
        first_operation.expect("first operation");
    assert_eq!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: first_operation_id,
                inviter: inviter.clone(),
                invitee: first_invitee,
            })
            .await
            .expect("replay first live operation"),
        first_authorization,
        "the first live record must survive capacity rejection",
    );

    assert_eq!(
        actor
            .ask(FinalizeMediatedInviteGrant {
                operation_id: first_operation_id,
            })
            .await
            .expect("finalize one live grant"),
        MediatedInviteGrantFinalization::Finalized,
        "the live record remains finalizable after capacity rejection",
    );
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation {
                operation_id: first_operation_id,
            })
            .await
            .expect("acknowledge finalized operation"),
        MediatedInviteOperationAcknowledgement::Acknowledged,
    );
    actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter,
            invitee: "after-finalize@example.com"
                .parse()
                .expect("bare invitee JID"),
        })
        .await
        .expect("completed record makes room for the next grant");
    assert_eq!(
        actor
            .ask(GetMediatedInviteOperationCount)
            .await
            .expect("operation count"),
        MAX_RETAINED_MEDIATED_INVITE_OPERATIONS,
    );
}

#[tokio::test]
async fn invite_grant_and_rollback_advance_the_admission_fence() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let before_grant = current_admission_revision(&actor).await;
    let grant = authorize_invite_grant(&actor, inviter, invitee.clone()).await;
    let operation_id = grant.operation_id();
    assert_eq!(
        current_admission_revision(&actor).await,
        before_grant + 1,
        "the temporary membership changes admission state",
    );
    assert!(matches!(
        actor
            .ask(SyncResolverAffiliation {
                jid: "unrelated@example.com".parse().expect("bare JID"),
                affiliation: Affiliation::None,
                expected_admission_revision: before_grant,
            })
            .await
            .expect("unrelated resolver repair"),
        ResolverAffiliationSyncOutcome::Applied { .. }
    ));

    actor
        .ask(PrepareMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("prepare rollback");
    actor
        .ask(CommitMediatedInviteGrantRollback { grant })
        .await
        .expect("commit rollback");
    actor
        .ask(AcknowledgeMediatedInviteOperation { operation_id })
        .await
        .expect("acknowledge rollback");
    assert_eq!(
        current_admission_revision(&actor).await,
        before_grant + 2,
        "restoring the prior affiliation changes admission state again",
    );
    assert_eq!(
        actor
            .ask(SyncResolverAffiliation {
                jid: invitee,
                affiliation: Affiliation::None,
                expected_admission_revision: before_grant,
            })
            .await
            .expect("stale invitee repair"),
        ResolverAffiliationSyncOutcome::StaleAdmissionRevision,
        "grant and rollback must invalidate pre-grant work for the invitee",
    );
}
