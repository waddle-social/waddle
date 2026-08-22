use super::*;

#[tokio::test]
async fn mediated_invite_authorization_rejects_a_demoted_inviter_without_granting_membership() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("demote inviter before invite authorization");

    let result = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter: inviter.clone(),
            invitee: invitee.clone(),
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(MediatedInviteGrantError::Forbidden))
    ));
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: invitee })
            .await
            .expect("invitee affiliation"),
        Affiliation::None,
    );
}

#[tokio::test]
async fn mediated_invite_authorization_grants_member_only_when_the_actor_creates_membership() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;

    let authorized = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter,
            invitee: invitee.clone(),
        })
        .await
        .expect("members-only admin invite");

    let grant = authorized.grant.expect("actor-created membership token");
    assert_eq!(grant.invitee(), &invitee);
    assert_eq!(grant.previous_affiliation(), Affiliation::None);
    assert_eq!(authorized.invitee_affiliation, Affiliation::Member);
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: invitee })
            .await
            .expect("invitee affiliation"),
        Affiliation::Member,
    );
}

#[tokio::test]
async fn mediated_invite_requires_the_exact_full_jid_to_remain_an_occupant() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let _ = departed(
        actor
            .ask(LeaveByRealJid {
                sender_jid: inviter.clone(),
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave reply"),
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
            MediatedInviteGrantError::NotOccupant
        ))
    ));
}

#[tokio::test]
async fn mediated_invite_rejects_a_sibling_resource_that_is_not_the_occupant() {
    let (actor, inviter, invitee) = joined_members_only_invite_actor().await;
    let sibling_resource = test_full_jid_resource("inviter", "phone");
    assert_ne!(sibling_resource, inviter);

    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter: sibling_resource,
                invitee,
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::NotOccupant
        ))
    ));
}

#[tokio::test]
async fn open_room_occupant_invite_never_changes_the_member_list() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        ..RoomConfig::default()
    })
    .await;
    let inviter = test_full_jid("visitor");
    let invitee = test_full_jid("invitee").to_bare();
    actor
        .ask(Join {
            nick: "visitor".to_string(),
            real_jid: inviter.clone(),
            role: Role::Participant,
            // Authorization in an open room is occupant-scoped only. This
            // intentionally unusual fixture proves affiliation is not
            // accidentally consulted by the invite policy.
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("open-room join");

    let authorized = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter,
            invitee: invitee.clone(),
        })
        .await
        .expect("open-room occupant invite");
    assert!(authorized.grant.is_none());
    assert_eq!(authorized.invitee_affiliation, Affiliation::None);
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: invitee })
            .await
            .expect("invitee affiliation"),
        Affiliation::None,
    );
}

#[tokio::test]
async fn open_room_no_grant_operations_do_not_reserve_the_invitee_index() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        group_dm: false,
        ..RoomConfig::default()
    })
    .await;
    let first_inviter = test_full_jid("first");
    let second_inviter = test_full_jid("second");
    let invitee = test_full_jid("invitee").to_bare();
    for (nick, inviter) in [
        ("first", first_inviter.clone()),
        ("second", second_inviter.clone()),
    ] {
        actor
            .ask(Join {
                nick: nick.to_string(),
                real_jid: inviter,
                role: Role::Participant,
                affiliation: Affiliation::None,
            })
            .await
            .expect("join inviter");
    }

    let first = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter: first_inviter,
            invitee: invitee.clone(),
        })
        .await
        .expect("first no-grant authorization");
    let second = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter: second_inviter,
            invitee,
        })
        .await
        .expect("second no-grant authorization");
    assert!(first.grant.is_none());
    assert!(second.grant.is_none());
}

#[tokio::test]
async fn ordinary_members_only_invites_require_admin_and_preserve_existing_membership() {
    let (actor, admin, invitee) = joined_members_only_invite_actor().await;
    let member = test_full_jid("member");
    actor
        .ask(Join {
            nick: "member".to_string(),
            real_jid: member.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join member");
    actor
        .ask(ChangeAffiliation {
            jid: member.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("store member affiliation");
    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter: member,
                invitee: invitee.clone(),
            })
            .await,
        Err(SendError::HandlerError(MediatedInviteGrantError::Forbidden))
    ));

    actor
        .ask(ChangeAffiliation {
            jid: invitee.clone(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("pre-existing admin");
    let authorized = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter: admin,
            invitee,
        })
        .await
        .expect("admin invite of existing affiliate");
    assert!(authorized.grant.is_none());
    assert_eq!(authorized.invitee_affiliation, Affiliation::Admin);
}

#[tokio::test]
async fn group_dm_invites_require_member_and_conflict_with_existing_membership() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        group_dm: true,
        ..RoomConfig::default()
    })
    .await;
    let inviter = test_full_jid("member");
    let invitee = test_full_jid("invitee").to_bare();
    actor
        .ask(Join {
            nick: "member".to_string(),
            real_jid: inviter.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join group-DM member");
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("store inviter membership");
    let grant = authorize_invite_grant(&actor, inviter.clone(), invitee.clone()).await;
    actor
        .ask(FinalizeMediatedInviteGrant {
            operation_id: grant.operation_id(),
        })
        .await
        .expect("finalize first invite");
    actor
        .ask(AcknowledgeMediatedInviteOperation {
            operation_id: grant.operation_id(),
        })
        .await
        .expect("acknowledge first invite");

    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter,
                invitee,
            })
            .await,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::InviteeAlreadyMember
        ))
    ));
}

#[tokio::test]
async fn group_dm_invites_reject_an_occupant_below_member() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        group_dm: true,
        ..RoomConfig::default()
    })
    .await;
    let inviter = test_full_jid("visitor");
    let invitee = test_full_jid("invitee").to_bare();
    actor
        .ask(Join {
            nick: "visitor".to_string(),
            real_jid: inviter.clone(),
            role: Role::Participant,
            affiliation: Affiliation::None,
        })
        .await
        .expect("join below-Member occupant");

    assert!(matches!(
        actor
            .ask(AuthorizeMediatedInvite {
                operation_id: invite_operation_id(),
                inviter,
                invitee,
            })
            .await,
        Err(SendError::HandlerError(MediatedInviteGrantError::Forbidden))
    ));
}

#[test]
fn group_dm_admission_is_members_only_even_if_config_state_is_inconsistent() {
    let mut room = MucRoom::new(
        "group@muc.example.com".parse().expect("room jid"),
        "waddle".to_string(),
        "channel".to_string(),
        RoomConfig::default(),
    );
    room.config.group_dm = true;
    room.config.members_only = false;

    assert!(
        !room.can_user_join(&test_full_jid("stranger").to_bare()),
        "group-DM privacy must not depend on a redundant config flag remaining normalized",
    );
}

#[tokio::test]
async fn group_dm_construction_normalizes_members_only_before_admission() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        group_dm: true,
        ..RoomConfig::default()
    })
    .await;
    let snapshot = actor.ask(GetSnapshot).await.expect("group-DM snapshot");
    assert!(
        snapshot.room.config.members_only,
        "a group DM must expose a normalized members-only config",
    );

    let outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid("stranger"),
            nick: "stranger".to_string(),
            affiliation_grant: JoinAffiliationGrant::Unaffiliated,
            local_domain: "example.com".to_string(),
            admission_revision: snapshot.admission_revision,
        })
        .await;
    assert!(matches!(
        outcome,
        Err(SendError::HandlerError(RoomActorError::JoinForbidden {
            reason: JoinDenialReason::MembersOnly,
        }))
    ));
}

#[tokio::test]
async fn group_dm_member_config_update_cannot_disable_members_only() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        group_dm: true,
        ..RoomConfig::default()
    })
    .await;
    let member = test_full_jid("member");
    actor
        .ask(Join {
            nick: "member".to_string(),
            real_jid: member.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join group-DM member");
    actor
        .ask(ChangeAffiliation {
            jid: member.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("store member affiliation");
    let mut config = actor.ask(GetConfig).await.expect("group-DM config");
    config.members_only = false;
    config.group_dm = false;

    let snapshot = actor
        .ask(UpdateGroupDmConfigByMember {
            config,
            sender_jid: member,
        })
        .await
        .expect("member config update");
    assert!(
        snapshot.room.config.members_only,
        "group-DM config updates must preserve members-only admission",
    );
    assert!(
        snapshot.room.config.group_dm,
        "a group-DM member update cannot reclassify the room",
    );
}

#[tokio::test]
async fn room_actor_boundary_normalizes_group_dm_authorization_policy() {
    let mut room = test_room();
    room.config.group_dm = true;
    room.config.members_only = false;
    let actor = RoomActor::spawn(RoomActor::new(room, test_secret()));
    let inviter = test_full_jid("member");
    let invitee = test_full_jid("invitee").to_bare();
    actor
        .ask(Join {
            nick: "member".to_string(),
            real_jid: inviter.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join group-DM member");
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("store inviter membership");

    let snapshot = actor.ask(GetSnapshot).await.expect("normalized snapshot");
    assert!(snapshot.room.config.members_only);
    let authorized = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: invite_operation_id(),
            inviter,
            invitee,
        })
        .await
        .expect("group-DM mediated invitation");

    assert!(authorized.grant.is_some());
    assert_eq!(authorized.invitee_affiliation, Affiliation::Member);
    assert!(authorized.members_only);
}

#[tokio::test]
async fn generic_config_update_normalizes_group_dm_members_only() {
    let actor = spawn_room_actor_with_config(RoomConfig::default()).await;
    let config = RoomConfig {
        members_only: false,
        group_dm: true,
        ..RoomConfig::default()
    };

    actor
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("generic config update");

    assert!(
        actor
            .ask(GetConfig)
            .await
            .expect("normalized config")
            .members_only,
    );
}
