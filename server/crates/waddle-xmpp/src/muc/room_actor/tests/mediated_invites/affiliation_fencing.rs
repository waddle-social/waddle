use super::*;

#[tokio::test]
async fn rolled_back_unacknowledged_invite_fences_regrant_join_and_mutation_until_ack() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let invitee_session = test_full_jid("invitee");
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
        .ask(Join {
            nick: "invitee".to_string(),
            real_jid: invitee_session.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("invitee joins before compensation");
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
    assert!(matches!(
        &first,
        MediatedInviteRollbackCommit::Applied { updates, .. }
            if !updates.presence_updates.is_empty()
    ));

    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter: inviter.clone(),
                invitee: invitee.clone(),
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::GrantPending
        ))
    ));
    assert!(matches!(
        actor
            .ask(Join {
                nick: "invitee".to_string(),
                real_jid: invitee_session.clone(),
                role: Role::Participant,
                affiliation: Affiliation::None,
            })
            .await,
        Err(SendError::HandlerError(
            RoomActorError::InviteRollbackPending
        ))
    ));
    assert!(matches!(
        actor
            .ask(ChangeAffiliation {
                jid: invitee.clone(),
                affiliation: Affiliation::Member,
            })
            .await,
        Err(SendError::HandlerError(
            AffiliationMutationError::InviteRollbackPending
        ))
    ));
    assert!(matches!(
        actor
            .ask(ApplyAffiliationChange {
                actor: Some(inviter.to_bare()),
                jid: invitee.clone(),
                affiliation: Affiliation::Admin,
            })
            .await,
        Err(SendError::HandlerError(
            AdminApplyError::InviteRollbackPending
        ))
    ));
    assert_eq!(
        actor
            .ask(SyncResolverAffiliation {
                jid: invitee.clone(),
                affiliation: Affiliation::Member,
                expected_admission_revision: current_admission_revision(&actor).await,
            })
            .await
            .expect("resolver fence outcome"),
        ResolverAffiliationSyncOutcome::InviteRollbackPending,
    );
    assert_eq!(
        actor
            .ask(CommitMediatedInviteGrantRollback { grant })
            .await
            .expect("duplicate commit"),
        first,
    );

    assert_eq!(
        actor
            .ask(AcknowledgeMediatedInviteOperation { operation_id })
            .await
            .expect("acknowledge"),
        MediatedInviteOperationAcknowledgement::Acknowledged,
    );
    actor
        .ask(ChangeAffiliation {
            jid: invitee.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("mutation proceeds after acknowledgement");
    actor
        .ask(Join {
            nick: "invitee".to_string(),
            real_jid: invitee_session,
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join proceeds after acknowledgement");
}

#[tokio::test]
async fn prepared_invite_rollback_reserves_the_invitee_from_join_and_affiliation_writes() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let grant = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter,
            invitee: invitee.clone(),
        })
        .await
        .expect("authorize invite")
        .grant
        .expect("temporary membership grant");
    actor
        .ask(PrepareMediatedInviteGrantRollback { grant })
        .await
        .expect("prepare rollback");
    let revision = current_admission_revision(&actor).await;

    let join = actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid("invitee"),
            nick: "invitee".to_string(),
            affiliation_grant: JoinAffiliationGrant::Unaffiliated,
            local_domain: "example.com".to_string(),
            admission_revision: revision,
        })
        .await;
    assert!(matches!(
        join,
        Err(SendError::HandlerError(
            RoomActorError::InviteRollbackPending
        ))
    ));
    assert_eq!(
        actor
            .ask(SyncResolverAffiliation {
                jid: invitee.clone(),
                affiliation: Affiliation::Member,
                expected_admission_revision: revision,
            })
            .await
            .expect("resolver sync reply"),
        ResolverAffiliationSyncOutcome::InviteRollbackPending,
    );
    assert!(matches!(
        actor
            .ask(ChangeAffiliation {
                jid: invitee,
                affiliation: Affiliation::Admin,
            })
            .await,
        Err(SendError::HandlerError(
            AffiliationMutationError::InviteRollbackPending
        ))
    ));
}

#[tokio::test]
async fn accepted_same_value_member_reaffirmation_supersedes_invite_rollback_authority() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let grant = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter: inviter.clone(),
            invitee: invitee.clone(),
        })
        .await
        .expect("authorize invite")
        .grant
        .expect("temporary membership grant");

    actor
        .ask(ChangeAffiliation {
            jid: invitee.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("explicitly reaffirm membership");

    assert_eq!(
        actor
            .ask(PrepareMediatedInviteGrantRollback { grant })
            .await
            .expect("prepare stale grant"),
        MediatedInviteRollbackPreparation::Superseded,
    );
    actor
        .ask(ChangeAffiliation {
            jid: invitee.clone(),
            affiliation: Affiliation::None,
        })
        .await
        .expect("remove superseding membership");
    assert!(actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter,
            invitee,
        })
        .await
        .expect("superseded operation released the invitee index")
        .grant
        .is_some());
}

#[tokio::test]
async fn mediated_invite_rollback_restores_an_outcast_ban() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    actor
        .ask(ChangeAffiliation {
            jid: invitee.clone(),
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("ban invitee");
    let grant = authorize_invite_grant(&actor, inviter, invitee.clone()).await;
    assert_eq!(grant.previous_affiliation(), Affiliation::Outcast);
    actor
        .ask(PrepareMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("prepare rollback");
    assert!(matches!(
        actor
            .ask(CommitMediatedInviteGrantRollback { grant })
            .await
            .expect("commit rollback"),
        MediatedInviteRollbackCommit::Applied {
            previous_affiliation: Affiliation::Outcast,
            ..
        }
    ));
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: invitee })
            .await
            .expect("restored ban"),
        Affiliation::Outcast,
    );
}

#[tokio::test]
async fn later_admin_and_owner_promotions_cannot_be_clobbered_by_invite_rollback() {
    for promotion in [Affiliation::Admin, Affiliation::Owner] {
        let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
        let grant = authorize_invite_grant(&actor, inviter, invitee.clone()).await;
        actor
            .ask(ChangeAffiliation {
                jid: invitee.clone(),
                affiliation: promotion,
            })
            .await
            .expect("promote invitee");
        assert_eq!(
            actor
                .ask(PrepareMediatedInviteGrantRollback { grant })
                .await
                .expect("prepare superseded rollback"),
            MediatedInviteRollbackPreparation::Superseded,
        );
        assert_eq!(
            actor
                .ask(GetAffiliation { jid: invitee })
                .await
                .expect("promotion survives"),
            promotion,
        );
    }
}

#[tokio::test]
async fn successor_owner_promotion_survives_original_owner_demotion_and_stale_rollback() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("make inviter the original owner");
    let grant = authorize_invite_grant(&actor, inviter.clone(), invitee.clone()).await;

    actor
        .ask(ApplyAdminItems {
            sender_jid: inviter.clone(),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![
                AdminItem {
                    jid: Some(invitee.clone()),
                    affiliation: Some(Affiliation::Owner),
                    role: None,
                    nick: None,
                    reason: None,
                },
                AdminItem {
                    jid: Some(inviter.to_bare()),
                    affiliation: Some(Affiliation::Member),
                    role: None,
                    nick: None,
                    reason: None,
                },
            ],
        })
        .await
        .expect("promote successor before demoting original owner");

    assert_eq!(
        actor
            .ask(PrepareMediatedInviteGrantRollback { grant })
            .await
            .expect("stale rollback"),
        MediatedInviteRollbackPreparation::Superseded,
    );
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: invitee })
            .await
            .expect("successor affiliation"),
        Affiliation::Owner,
    );
    assert_eq!(
        actor
            .ask(GetAffiliation {
                jid: inviter.to_bare(),
            })
            .await
            .expect("original owner affiliation"),
        Affiliation::Member,
    );
}

#[tokio::test]
async fn prepared_rollback_blocks_guarded_and_batched_affiliation_paths() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let grant = authorize_invite_grant(&actor, inviter.clone(), invitee.clone()).await;
    let revision = current_admission_revision(&actor).await;
    let invitee_full = test_full_jid("invitee");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: invitee_full.clone(),
            nick: "invitee".to_string(),
            affiliation_grant: JoinAffiliationGrant::Unaffiliated,
            local_domain: "example.com".to_string(),
            admission_revision: revision,
        })
        .await
        .expect("invitee joins before compensation");
    actor
        .ask(PrepareMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("prepare rollback");

    assert!(matches!(
        actor
            .ask(ApplyAffiliationChange {
                actor: Some(inviter.to_bare()),
                jid: invitee.clone(),
                affiliation: Affiliation::Admin,
            })
            .await,
        Err(SendError::HandlerError(
            AdminApplyError::InviteRollbackPending
        ))
    ));
    assert!(matches!(
        actor
            .ask(ApplyAdminItems {
                sender_jid: inviter,
                sender_affiliation: Affiliation::Admin,
                sender_role: Role::Moderator,
                items: vec![AdminItem {
                    jid: Some(invitee.clone()),
                    affiliation: Some(Affiliation::Member),
                    role: None,
                    nick: None,
                    reason: None,
                }],
            })
            .await,
        Err(SendError::HandlerError(
            AdminApplyError::InviteRollbackPending
        ))
    ));
    assert!(matches!(
        actor
            .ask(EnforceMembersOnlyAffiliations {
                affiliations: vec![(invitee.clone(), Affiliation::Member)],
            })
            .await,
        Err(SendError::HandlerError(
            AffiliationMutationError::InviteRollbackPending
        ))
    ));
    assert!(matches!(
        actor
            .ask(Join {
                nick: "invitee-phone".to_string(),
                real_jid: test_full_jid_resource("invitee", "phone"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await,
        Err(SendError::HandlerError(
            RoomActorError::InviteRollbackPending
        ))
    ));

    assert_eq!(
        actor
            .ask(AbortMediatedInviteGrantRollback { grant })
            .await
            .expect("abort rollback"),
        MediatedInviteRollbackAbort::Aborted,
    );
    assert!(actor
        .ask(GetOccupantByJid { jid: invitee_full })
        .await
        .expect("occupant lookup")
        .is_some());
}

#[tokio::test]
async fn a_batch_targeting_a_reserved_invitee_is_rejected_before_any_item_applies() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let unaffected: BareJid = "unaffected@example.com".parse().expect("bare jid");
    let grant = authorize_invite_grant(&actor, inviter.clone(), invitee.clone()).await;
    actor
        .ask(PrepareMediatedInviteGrantRollback { grant })
        .await
        .expect("prepare rollback");

    assert!(matches!(
        actor
            .ask(ApplyAdminItems {
                sender_jid: inviter,
                sender_affiliation: Affiliation::Admin,
                sender_role: Role::Moderator,
                items: vec![
                    AdminItem {
                        jid: Some(unaffected.clone()),
                        affiliation: Some(Affiliation::Member),
                        role: None,
                        nick: None,
                        reason: None,
                    },
                    AdminItem {
                        jid: Some(invitee),
                        affiliation: Some(Affiliation::Member),
                        role: None,
                        nick: None,
                        reason: None,
                    },
                ],
            })
            .await,
        Err(SendError::HandlerError(
            AdminApplyError::InviteRollbackPending
        ))
    ));
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: unaffected })
            .await
            .expect("unaffected affiliation"),
        Affiliation::None,
        "the item before the reserved invitee was not partially applied",
    );
}

#[tokio::test]
async fn committed_rollback_ejects_a_joined_invitee_with_status_321() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let grant = authorize_invite_grant(&actor, inviter, invitee.clone()).await;
    let invitee_full = test_full_jid("invitee");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: invitee_full.clone(),
            nick: "invitee".to_string(),
            affiliation_grant: JoinAffiliationGrant::Unaffiliated,
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("invitee joins");
    actor
        .ask(PrepareMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("prepare rollback");
    let MediatedInviteRollbackCommit::Applied { updates, .. } = actor
        .ask(CommitMediatedInviteGrantRollback { grant })
        .await
        .expect("commit rollback")
    else {
        panic!("exact prepared grant must apply");
    };
    assert!(updates
        .presence_updates
        .iter()
        .any(|(recipient, presence)| recipient == &invitee_full
            && presence_has_status(presence, "321")));
    assert!(actor
        .ask(GetOccupantByJid { jid: invitee_full })
        .await
        .expect("occupant lookup")
        .is_none());
}

#[tokio::test]
async fn refused_resolver_affiliation_writes_preserve_invite_rollback_authority() {
    let (join_actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let join_grant = authorize_invite_grant(&join_actor, inviter, invitee.clone()).await;
    join_actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid("invitee"),
            nick: "invitee".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&join_actor).await,
        })
        .await
        .expect("resolver-derived join is admitted by explicit invite membership");
    assert_eq!(
        join_actor
            .ask(PrepareMediatedInviteGrantRollback { grant: join_grant })
            .await
            .expect("prepare after refused join resolver write"),
        MediatedInviteRollbackPreparation::Prepared,
    );

    let (sync_actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let sync_grant = authorize_invite_grant(&sync_actor, inviter, invitee.clone()).await;
    assert_eq!(
        sync_actor
            .ask(SyncResolverAffiliation {
                jid: invitee,
                affiliation: Affiliation::None,
                expected_admission_revision: current_admission_revision(&sync_actor).await,
            })
            .await
            .expect("resolver sync"),
        ResolverAffiliationSyncOutcome::Applied,
    );
    assert_eq!(
        sync_actor
            .ask(PrepareMediatedInviteGrantRollback { grant: sync_grant })
            .await
            .expect("prepare after refused resolver sync"),
        MediatedInviteRollbackPreparation::Prepared,
    );
}

#[tokio::test]
async fn rejected_creator_owner_grant_preserves_invite_rollback_authority() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("establish an existing owner");
    let grant = authorize_invite_grant(&actor, inviter, invitee.clone()).await;

    actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid("invitee"),
            nick: "invitee".to_string(),
            affiliation_grant: JoinAffiliationGrant::CreatorOwner,
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("creator claim is ignored because an owner exists");

    assert_eq!(
        actor
            .ask(PrepareMediatedInviteGrantRollback { grant })
            .await
            .expect("prepare after ignored creator claim"),
        MediatedInviteRollbackPreparation::Prepared,
    );
}
