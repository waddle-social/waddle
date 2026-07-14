use super::*;

#[tokio::test]
async fn unacknowledged_rollback_blocks_dormancy_and_both_seal_guards() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        persistent: false,
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
        .expect("persist inviter affiliation");
    let operation_id = invite_operation_id();
    let grant = actor
        .ask(AuthorizeMediatedInvite {
            operation_id,
            inviter: inviter.clone(),
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
        .expect("prepare");
    actor
        .ask(CommitMediatedInviteGrantRollback { grant })
        .await
        .expect("commit");
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::None,
        })
        .await
        .expect("remove inviter affiliation");
    actor
        .ask(Leave {
            nick: "inviter".to_string(),
        })
        .await
        .expect("leave room");

    let probe = actor.ask(IsDormant).await.expect("dormancy probe");
    assert!(
        !probe.dormant,
        "unacknowledged rollback output must block dormancy"
    );
    for guard in [SealGuard::Dormant, SealGuard::EmptyNonPersistent] {
        assert_eq!(
            actor
                .ask(SealIfInactive {
                    expected_occupancy_revision: probe.occupancy_revision,
                    guard,
                })
                .await
                .expect("seal verdict"),
            SealIfInactiveOutcome::Refused,
            "unacknowledged rollback output must block {guard:?} sealing",
        );
    }
    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("acknowledge"),
        MediatedInviteOperationAcknowledgement::Acknowledged,
    );
    assert!(
        actor
            .ask(IsDormant)
            .await
            .expect("post-ack dormancy probe")
            .dormant,
        "acknowledgement releases the actor lifecycle fence",
    );
}

#[tokio::test]
async fn mediated_invite_authorization_fails_closed_while_durable_restore_is_pending() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let store = FlakyThenRecoveringStore::new(usize::MAX, invitee.clone());
    actor
        .ask(RestoreDurableRoomState { store })
        .await
        .expect("failed restore still replies");

    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter,
                invitee: invitee.clone(),
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::RestorePending
        ))
    ));
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: invitee })
            .await
            .expect("no grant from default state"),
        Affiliation::None,
    );
}

#[tokio::test]
async fn invite_grant_persist_failure_leaves_memory_unchanged_and_creates_no_token() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let store = FailNthAffiliationSaveStore::new(1);
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
        })
        .await
        .expect("attach durable store");

    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter: inviter.clone(),
                invitee: invitee.clone(),
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::GrantPersistFailed(_)
        ))
    ));
    assert_eq!(
        actor
            .ask(GetAffiliation {
                jid: invitee.clone(),
            })
            .await
            .expect("unchanged affiliation"),
        Affiliation::None,
    );
    assert_eq!(store.save_call_count(), 1);

    let retry = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter,
            invitee,
        })
        .await
        .expect("retry after the one failed persistence call");
    assert!(
        retry.grant.is_some(),
        "failed persistence created no ghost token"
    );
    assert_eq!(store.save_call_count(), 2);
}

#[tokio::test]
async fn banned_invitee_rejection_performs_no_durable_member_write() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let store = FakeDurableStore::owned();
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
        })
        .await
        .expect("attach recording durable store");
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("persist inviter affiliation after restore");
    actor
        .ask(ChangeAffiliation {
            jid: invitee.clone(),
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("persist invitee ban");
    let writes_before_invite = store.saved_affiliations();

    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter,
                invitee: invitee.clone(),
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::InviteeBanned
        ))
    ));
    assert_eq!(store.saved_affiliations(), writes_before_invite);
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: invitee })
            .await
            .expect("ban remains authoritative"),
        Affiliation::Outcast,
    );
}

#[tokio::test]
async fn invite_grant_and_rollback_persist_the_exact_room_invitee_and_affiliations() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let store = FakeDurableStore::owned();
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
        })
        .await
        .expect("attach recording durable store");

    let grant = authorize_invite_grant(&actor, inviter, invitee.clone()).await;
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

    let room_jid: BareJid = "testroom@muc.example.com".parse().expect("room jid");
    assert_eq!(
        store.saved_affiliations(),
        vec![
            (room_jid.clone(), invitee.clone(), Affiliation::Member),
            (room_jid, invitee, Affiliation::None),
        ],
    );
}

#[tokio::test]
async fn rollback_persist_failure_retains_the_exact_token_and_reservation_for_retry() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let store = FailNthAffiliationSaveStore::new(2);
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
        })
        .await
        .expect("attach durable store");
    let operation_id = invite_operation_id();
    let grant = actor
        .ask(AuthorizeMediatedInvite {
            operation_id,
            inviter: inviter.clone(),
            invitee: invitee.clone(),
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

    assert!(matches!(
        actor
            .ask(CommitMediatedInviteGrantRollback {
                grant: grant.clone(),
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteRollbackError::PersistFailedBeforeApply(_)
        ))
    ));
    assert_eq!(
        actor
            .ask(GetAffiliation {
                jid: invitee.clone(),
            })
            .await
            .expect("membership remains after failed commit"),
        Affiliation::Member,
    );
    assert!(matches!(
        actor
            .ask(ChangeAffiliation {
                jid: invitee.clone(),
                affiliation: Affiliation::Admin,
            })
            .await,
        Err(SendError::HandlerError(
            AffiliationMutationError::InviteRollbackPending
        ))
    ));
    actor
        .ask(Leave {
            nick: "inviter".to_string(),
        })
        .await
        .expect("leave while rollback remains prepared");
    let probe = actor.ask(IsDormant).await.expect("dormancy probe");
    assert_eq!(
        actor
            .ask(SealIfInactive {
                expected_occupancy_revision: probe.occupancy_revision,
                guard: SealGuard::EmptyNonPersistent,
            })
            .await
            .expect("empty-room seal verdict"),
        SealIfInactiveOutcome::Refused,
        "a prepared rollback must survive empty non-persistent room cleanup",
    );
    let expected_grant = grant.clone();
    drop(grant);
    let recovered_grant = actor
        .ask(AuthorizeMediatedInvite {
            operation_id,
            inviter,
            invitee: invitee.clone(),
        })
        .await
        .expect("recover exact authorization after losing the first reply state")
        .grant
        .expect("recover exact rollback authority");
    assert_eq!(recovered_grant, expected_grant);
    assert_eq!(
        actor
            .ask(PrepareMediatedInviteGrantRollback {
                grant: recovered_grant.clone(),
            })
            .await
            .expect("same token is still prepared"),
        MediatedInviteRollbackPreparation::Prepared,
    );
    assert!(matches!(
        actor
            .ask(CommitMediatedInviteGrantRollback {
                grant: recovered_grant,
            })
            .await
            .expect("retry rollback after transient failure"),
        MediatedInviteRollbackCommit::Applied { .. }
    ));
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: invitee })
            .await
            .expect("rollback eventually converges"),
        Affiliation::None,
    );
    assert_eq!(store.save_call_count(), 3);
}

#[tokio::test]
async fn rollback_ownership_loss_is_typed_and_keeps_the_operation_prepared() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let store = FakeDurableStore::owned();
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
        })
        .await
        .expect("attach owned durable store");
    let grant = authorize_invite_grant(&actor, inviter, invitee.clone()).await;
    actor
        .ask(PrepareMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("prepare while the actor owns the room");
    *store.fenced.lock().expect("fence lock") = Some(false);

    assert!(matches!(
        actor
            .ask(CommitMediatedInviteGrantRollback {
                grant: grant.clone(),
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteRollbackError::NotOwner
        ))
    ));
    assert_eq!(
        store.save_call_count(),
        1,
        "ownership rejection must happen before rollback persistence",
    );
    assert_eq!(
        actor
            .ask(GetAffiliation {
                jid: invitee.clone(),
            })
            .await
            .expect("unchanged temporary membership"),
        Affiliation::Member,
    );
    assert_eq!(
        actor
            .ask(PrepareMediatedInviteGrantRollback { grant })
            .await
            .expect("prepared state remains replayable"),
        MediatedInviteRollbackPreparation::Prepared,
    );
}
