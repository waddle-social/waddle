use super::*;

use crate::muc::admin::AdminItem;
use crate::xep::xep0421::OccupantIdSecret;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

fn test_secret() -> OccupantIdSecret {
    OccupantIdSecret::for_testing(b"test-secret".to_vec())
}

fn test_room() -> MucRoom {
    let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
    MucRoom::new(
        room_jid,
        "waddle-1".to_string(),
        "channel-1".to_string(),
        RoomConfig::default(),
    )
}

fn test_full_jid(user: &str) -> FullJid {
    format!("{}@example.com/res", user)
        .parse()
        .expect("valid jid")
}

fn test_full_jid_resource(user: &str, resource: &str) -> FullJid {
    format!("{}@example.com/{}", user, resource)
        .parse()
        .expect("valid jid")
}

async fn spawn_room_actor() -> ActorRef<RoomActor> {
    RoomActor::spawn(RoomActor::new(test_room(), test_secret()))
}

async fn spawn_room_actor_with_config(mut config: RoomConfig) -> ActorRef<RoomActor> {
    let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
    config.name = "Test Room".to_string();
    RoomActor::spawn(RoomActor::new(
        MucRoom::new(
            room_jid,
            "waddle-1".to_string(),
            "channel-1".to_string(),
            config,
        ),
        test_secret(),
    ))
}

fn presence_has_status(presence: &Presence, code: &str) -> bool {
    presence.payloads.iter().any(|payload| {
        payload.name() == "x"
            && payload.ns() == "http://jabber.org/protocol/muc#user"
            && payload.children().any(|child| {
                child.name() == "status"
                    && child.ns() == "http://jabber.org/protocol/muc#user"
                    && child.attr("code") == Some(code)
            })
    })
}

#[tokio::test]
async fn test_join_and_occupant_count() {
    let actor = spawn_room_actor().await;

    let count = actor.ask(OccupantCount).await.expect("ask");
    assert_eq!(count, 0);

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join should succeed");

    let count = actor.ask(OccupantCount).await.expect("ask");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_join_duplicate_nick_rejected() {
    let actor = spawn_room_actor().await;

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("first join");

    let result = actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("bob"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(RoomActorError::NickAlreadyInUse(nick)))
            if nick == "alice"
    ));
}

#[tokio::test]
async fn test_join_rejected_when_room_full() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        max_occupants: 1,
        ..RoomConfig::default()
    })
    .await;

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("first join");

    let result = actor
        .ask(Join {
            nick: "bob".to_string(),
            real_jid: test_full_jid("bob"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(RoomActorError::RoomFull))
    ));
}

#[tokio::test]
async fn test_join_existing_session_allowed_when_room_full() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        max_occupants: 1,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("first join should succeed");

    let outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: alice,
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("existing session rejoin should bypass full-room rejection");

    assert_eq!(outcome.occupant_count, 1);
}

#[tokio::test]
async fn test_leave() {
    let actor = spawn_room_actor().await;

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");

    actor
        .ask(Leave {
            nick: "alice".to_string(),
        })
        .await
        .expect("leave should succeed");

    let count = actor.ask(OccupantCount).await.expect("ask");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_leave_unknown_nick() {
    let actor = spawn_room_actor().await;

    let result = actor
        .ask(Leave {
            nick: "ghost".to_string(),
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(RoomActorError::OccupantNotFound(nick)))
            if nick == "ghost"
    ));
}

#[tokio::test]
async fn test_get_occupant_by_nick() {
    let actor = spawn_room_actor().await;

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");

    let info = actor
        .ask(GetOccupantByNick {
            nick: "alice".to_string(),
        })
        .await
        .expect("ask");
    assert!(info.is_some());
    let info = info.expect("occupant present");
    assert_eq!(info.nick, "alice");
    assert_eq!(info.role, Role::Participant);
}

#[tokio::test]
async fn test_get_occupant_by_jid() {
    let actor = spawn_room_actor().await;
    let jid = test_full_jid("alice");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: jid.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");

    let info = actor.ask(GetOccupantByJid { jid }).await.expect("ask");
    assert!(info.is_some());
}

#[tokio::test]
async fn test_get_info() {
    let actor = spawn_room_actor().await;

    let info = actor.ask(GetInfo).await.expect("ask");
    assert_eq!(info.occupant_count, 0);
    assert_eq!(
        info.room_jid,
        "testroom@muc.example.com".parse::<BareJid>().expect("jid")
    );
}

#[tokio::test]
async fn test_get_and_update_config() {
    let actor = spawn_room_actor().await;

    let config = actor.ask(GetConfig).await.expect("ask");
    assert!(config.members_only);

    let mut new_config = config;
    new_config.members_only = false;
    actor
        .ask(UpdateConfig { config: new_config })
        .await
        .expect("ask");

    let config = actor.ask(GetConfig).await.expect("ask");
    assert!(!config.members_only);
}

#[tokio::test]
async fn members_only_enforcement_ejects_current_non_members_with_status_322() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::None,
        })
        .await
        .expect("join");

    let mut config = actor.ask(GetConfig).await.expect("config");
    config.members_only = true;
    actor
        .ask(UpdateConfig { config })
        .await
        .expect("config update");

    let updates = actor.ask(EnforceMembersOnly).await.expect("enforce");
    assert_eq!(updates.len(), 1, "only Alice should receive her removal");
    assert_eq!(updates[0].0, alice);
    assert_eq!(updates[0].1.type_, PresenceType::Unavailable);
    assert!(presence_has_status(&updates[0].1, "322"));
    assert!(presence_has_status(&updates[0].1, "110"));

    let count = actor.ask(OccupantCount).await.expect("count");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_change_and_get_affiliation() {
    let actor = spawn_room_actor().await;
    let jid: BareJid = "alice@example.com".parse().expect("jid");

    let aff = actor
        .ask(GetAffiliation { jid: jid.clone() })
        .await
        .expect("ask");
    assert_eq!(aff, Affiliation::None);

    actor
        .ask(ChangeAffiliation {
            jid: jid.clone(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("ask");

    let aff = actor.ask(GetAffiliation { jid }).await.expect("ask");
    assert_eq!(aff, Affiliation::Admin);
}

#[tokio::test]
async fn membership_revocation_in_members_only_room_ejects_occupant_with_status_321() {
    let actor = spawn_room_actor().await;
    let alice_bare: BareJid = "alice@example.com".parse().expect("jid");
    let alice = test_full_jid("alice");

    actor
        .ask(ChangeAffiliation {
            jid: alice_bare.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("member grant");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: snapshot.admission_revision,
        })
        .await
        .expect("join");

    let updates = actor
        .ask(ApplyAffiliationChange {
            actor: Some("admin@example.com".parse().expect("admin")),
            jid: alice_bare,
            affiliation: Affiliation::None,
        })
        .await
        .expect("apply");
    let updates = updates.presence_updates;
    assert_eq!(updates.len(), 1, "only Alice should receive her removal");
    assert_eq!(updates[0].0, alice);
    assert_eq!(updates[0].1.type_, PresenceType::Unavailable);
    assert!(presence_has_status(&updates[0].1, "321"));
    assert!(presence_has_status(&updates[0].1, "110"));

    let count = actor.ask(OccupantCount).await.expect("count");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_list_occupants() {
    let actor = spawn_room_actor().await;

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join alice");

    actor
        .ask(Join {
            nick: "bob".to_string(),
            real_jid: test_full_jid("bob"),
            role: Role::Moderator,
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("join bob");

    let list = actor.ask(ListOccupants).await.expect("ask");
    assert_eq!(list.len(), 2);
}

#[tokio::test]
async fn test_destroy() {
    let actor = spawn_room_actor().await;

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");

    actor.ask(Destroy).await.expect("ask");

    let count = actor.ask(OccupantCount).await.expect("ask");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_apply_admin_items_rejects_moderator_role_change_on_admin() {
    let actor = spawn_room_actor().await;

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Moderator,
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("join");

    let sender_jid = test_full_jid("mod");
    let result = actor
        .ask(ApplyAdminItems {
            sender_jid,
            sender_affiliation: Affiliation::None,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: None,
                nick: Some("alice".to_string()),
                affiliation: None,
                role: Some(Role::Visitor),
                reason: None,
            }],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            AdminApplyError::CannotModifyPrivilegedRole
        ))
    ));

    let occupant = actor
        .ask(GetOccupantByNick {
            nick: "alice".to_string(),
        })
        .await
        .expect("occupant")
        .expect("occupant exists");
    assert_eq!(occupant.role, Role::Moderator);

    let count = actor.ask(OccupantCount).await.expect("count");
    assert_eq!(
        count, 1,
        "actor should stay healthy after permission denial"
    );
}

#[tokio::test]
async fn test_apply_admin_items_rejects_admin_role_change_on_admin() {
    let actor = spawn_room_actor().await;

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Moderator,
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("join alice");

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("bob"),
            sender_affiliation: Affiliation::Admin,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: None,
                nick: Some("alice".to_string()),
                affiliation: None,
                role: Some(Role::None),
                reason: None,
            }],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            AdminApplyError::CannotModifyPrivilegedRole
        ))
    ));

    let occupant = actor
        .ask(GetOccupantByNick {
            nick: "alice".to_string(),
        })
        .await
        .expect("occupant")
        .expect("occupant exists");
    assert_eq!(occupant.role, Role::Moderator);
}

#[tokio::test]
async fn test_apply_admin_items_rejects_moderator_grant_from_role_only_moderator() {
    let actor = spawn_room_actor().await;

    actor
        .ask(Join {
            nick: "bob".to_string(),
            real_jid: test_full_jid("bob"),
            role: Role::Moderator,
            affiliation: Affiliation::None,
        })
        .await
        .expect("join bob");
    actor
        .ask(Join {
            nick: "carol".to_string(),
            real_jid: test_full_jid("carol"),
            role: Role::Participant,
            affiliation: Affiliation::None,
        })
        .await
        .expect("join carol");

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("bob"),
            sender_affiliation: Affiliation::None,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: None,
                nick: Some("carol".to_string()),
                affiliation: None,
                role: Some(Role::Moderator),
                reason: None,
            }],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(AdminApplyError::PermissionDenied(
            _
        )))
    ));

    let occupant = actor
        .ask(GetOccupantByNick {
            nick: "carol".to_string(),
        })
        .await
        .expect("occupant")
        .expect("occupant exists");
    assert_eq!(occupant.role, Role::Participant);

    let count = actor.ask(OccupantCount).await.expect("count");
    assert_eq!(
        count, 2,
        "actor should stay healthy after permission denial"
    );
}

#[tokio::test]
async fn test_apply_admin_items_cannot_remove_last_owner() {
    let actor = spawn_room_actor().await;
    let owner_jid: BareJid = "owner@example.com".parse().expect("valid bare jid");

    actor
        .ask(ChangeAffiliation {
            jid: owner_jid.clone(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("set owner");

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: Some(owner_jid.clone()),
                nick: None,
                affiliation: Some(Affiliation::Member),
                role: None,
                reason: None,
            }],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            AdminApplyError::CannotRemoveLastOwner
        ))
    ));

    let still_owner = actor
        .ask(IsOwner { jid: owner_jid })
        .await
        .expect("owner check");
    assert!(still_owner, "last owner must be preserved");
}

#[tokio::test]
async fn admin_cannot_ban_or_deaffiliate_owner() {
    let actor = spawn_room_actor().await;
    let owner_jid: BareJid = "owner@example.com".parse().expect("valid bare jid");
    let other_owner_jid: BareJid = "other-owner@example.com".parse().expect("valid bare jid");

    actor
        .ask(ChangeAffiliation {
            jid: owner_jid.clone(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("set owner");
    actor
        .ask(ChangeAffiliation {
            jid: other_owner_jid,
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("set other owner");

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("admin"),
            sender_affiliation: Affiliation::Admin,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: Some(owner_jid.clone()),
                nick: None,
                affiliation: Some(Affiliation::Outcast),
                role: None,
                reason: None,
            }],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            AdminApplyError::CannotAdminModifyOwner
        ))
    ));

    let still_owner = actor
        .ask(IsOwner { jid: owner_jid })
        .await
        .expect("owner check");
    assert!(still_owner, "admin must not be able to ban an owner");
}

#[tokio::test]
async fn admin_cannot_modify_admin_affiliations() {
    let actor = spawn_room_actor().await;
    let target_admin: BareJid = "target-admin@example.com".parse().expect("target admin");
    let target_member: BareJid = "target-member@example.com".parse().expect("target member");

    actor
        .ask(ChangeAffiliation {
            jid: target_admin.clone(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("target admin affiliation");
    actor
        .ask(ChangeAffiliation {
            jid: target_member.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("target member affiliation");

    let demote_result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("admin"),
            sender_affiliation: Affiliation::Admin,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: Some(target_admin.clone()),
                nick: None,
                affiliation: Some(Affiliation::Member),
                role: None,
                reason: None,
            }],
        })
        .await;
    assert!(matches!(
        demote_result,
        Err(SendError::HandlerError(AdminApplyError::PermissionDenied(
            _
        )))
    ));

    let promote_result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("admin"),
            sender_affiliation: Affiliation::Admin,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: Some(target_member.clone()),
                nick: None,
                affiliation: Some(Affiliation::Admin),
                role: None,
                reason: None,
            }],
        })
        .await;
    assert!(matches!(
        promote_result,
        Err(SendError::HandlerError(AdminApplyError::PermissionDenied(
            _
        )))
    ));

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(snapshot.get_affiliation(&target_admin), Affiliation::Admin);
    assert_eq!(
        snapshot.get_affiliation(&target_member),
        Affiliation::Member
    );
}

#[tokio::test]
async fn affiliation_batch_validation_happens_before_members_only_ejection() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    })
    .await;
    let alice_bare: BareJid = "alice@example.com".parse().expect("alice bare");
    let owner_bare: BareJid = "owner@example.com".parse().expect("owner bare");
    let alice = test_full_jid("alice");

    actor
        .ask(ChangeAffiliation {
            jid: alice_bare.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("alice member");
    actor
        .ask(ChangeAffiliation {
            jid: owner_bare.clone(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("owner");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: snapshot.admission_revision,
        })
        .await
        .expect("alice join");

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("admin"),
            sender_affiliation: Affiliation::Admin,
            sender_role: Role::Moderator,
            items: vec![
                AdminItem {
                    jid: Some(alice_bare.clone()),
                    nick: None,
                    affiliation: Some(Affiliation::None),
                    role: None,
                    reason: None,
                },
                AdminItem {
                    jid: Some(owner_bare.clone()),
                    nick: None,
                    affiliation: Some(Affiliation::Outcast),
                    role: None,
                    reason: None,
                },
            ],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            AdminApplyError::CannotAdminModifyOwner
        ))
    ));
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(snapshot.get_affiliation(&alice_bare), Affiliation::Member);
    assert_eq!(snapshot.find_nick_by_real_jid(&alice), Some("alice"));
    assert_eq!(snapshot.get_affiliation(&owner_bare), Affiliation::Owner);
}

#[tokio::test]
async fn stale_admission_revision_returns_retryable_error_without_joining() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    let config = RoomConfig {
        name: "Renamed".to_string(),
        ..RoomConfig::default()
    };
    actor
        .ask(UpdateConfig { config })
        .await
        .expect("config update");

    let result = actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::None,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            RoomActorError::StaleAdmissionRevision
        ))
    ));
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert!(snapshot.find_nick_by_real_jid(&alice).is_none());
}

#[tokio::test]
async fn role_none_kick_notifies_same_nick_sibling_sessions() {
    let actor = spawn_room_actor().await;
    let alice_laptop = test_full_jid_resource("alice", "laptop");
    let alice_phone = test_full_jid_resource("alice", "phone");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_laptop.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("first session join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_phone.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("same nick sibling session join");

    let updates = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: None,
                nick: Some("alice".to_string()),
                affiliation: None,
                role: Some(Role::None),
                reason: None,
            }],
        })
        .await
        .expect("kick succeeds");

    assert!(updates
        .presence_updates
        .iter()
        .any(|(recipient, presence)| {
            recipient == &alice_laptop && presence_has_status(presence, "307")
        }));
    assert!(updates
        .presence_updates
        .iter()
        .any(|(recipient, presence)| {
            recipient == &alice_phone && presence_has_status(presence, "307")
        }));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
}

#[tokio::test]
async fn members_only_revocation_removes_every_nick_for_bare_jid() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    })
    .await;
    let alice_laptop = test_full_jid_resource("alice", "laptop");
    let alice_phone = test_full_jid_resource("alice", "phone");
    let alice_bare = alice_laptop.to_bare();

    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_laptop.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("first nick join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_phone.clone(),
            nick: "alice-phone".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("second nick join");

    let updates = actor
        .ask(ApplyAffiliationChange {
            actor: Some("owner@example.com".parse().expect("owner")),
            jid: alice_bare,
            affiliation: Affiliation::None,
        })
        .await
        .expect("revocation succeeds");

    assert!(updates
        .presence_updates
        .iter()
        .any(|(recipient, presence)| {
            recipient == &alice_laptop && presence_has_status(presence, "321")
        }));
    assert!(updates
        .presence_updates
        .iter()
        .any(|(recipient, presence)| {
            recipient == &alice_phone && presence_has_status(presence, "321")
        }));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
}

#[tokio::test]
async fn managed_members_only_enforcement_uses_explicit_affiliation_snapshot() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("open-room inherited member join");

    let config = RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    };
    actor
        .ask(UpdateConfig { config })
        .await
        .expect("members-only config");

    let updates = actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: vec![(alice.to_bare(), Affiliation::None)],
        })
        .await
        .expect("managed enforcement succeeds");

    assert!(updates.iter().any(|(recipient, presence)| {
        recipient == &alice && presence_has_status(presence, "322")
    }));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
}

#[tokio::test]
async fn managed_members_only_enforcement_treats_missing_snapshot_entry_as_none() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("stale-open join");

    let config = RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    };
    actor
        .ask(UpdateConfig { config })
        .await
        .expect("members-only config");

    let updates = actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: Vec::new(),
        })
        .await
        .expect("managed enforcement succeeds");

    assert!(updates.iter().any(|(recipient, presence)| {
        recipient == &alice && presence_has_status(presence, "322")
    }));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
}

#[tokio::test]
async fn test_get_room_jid() {
    let actor = spawn_room_actor().await;

    let jid = actor.ask(GetRoomJid).await.expect("ask");
    assert_eq!(
        jid,
        "testroom@muc.example.com".parse::<BareJid>().expect("jid")
    );
}

#[tokio::test]
async fn apply_pin_then_get_pin_list_returns_entry() {
    use crate::muc::pin::{PinPreview, PinStateChange, PinnedEntry};
    use chrono::Utc;
    use waddle_xmpp_core::xep0359::StanzaId;

    let actor = spawn_room_actor().await;
    let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
    let target = StanzaId::new("stanza-1".to_string(), jid::Jid::from(room_jid.clone()));
    let entry = PinnedEntry {
        target_stanza_id: target.clone(),
        pinner_jid: "admin@example.com".parse().expect("valid jid"),
        pinned_at: Utc::now(),
        preview: PinPreview::new(
            "alice@example.com".parse().expect("valid jid"),
            Some("alice".into()),
            "important",
            Utc::now(),
        ),
    };
    actor
        .ask(ApplyPin {
            change: PinStateChange::Pin(entry.clone()),
        })
        .await
        .expect("apply pin");
    let entries = actor.ask(GetPinList).await.expect("get pin list");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].target_stanza_id, target);
    assert_eq!(entries[0].pinner_jid, entry.pinner_jid);
}

#[tokio::test]
async fn apply_unpin_removes_entry() {
    use crate::muc::pin::{PinPreview, PinStateChange, PinnedEntry};
    use chrono::Utc;
    use waddle_xmpp_core::xep0359::StanzaId;

    let actor = spawn_room_actor().await;
    let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
    let target = StanzaId::new("stanza-1".to_string(), jid::Jid::from(room_jid));
    let entry = PinnedEntry {
        target_stanza_id: target.clone(),
        pinner_jid: "admin@example.com".parse().expect("valid jid"),
        pinned_at: Utc::now(),
        preview: PinPreview::new(
            "alice@example.com".parse().expect("valid jid"),
            None,
            "hi",
            Utc::now(),
        ),
    };
    actor
        .ask(ApplyPin {
            change: PinStateChange::Pin(entry),
        })
        .await
        .expect("apply pin");
    actor
        .ask(ApplyPin {
            change: PinStateChange::Unpin {
                target_stanza_id: target.clone(),
            },
        })
        .await
        .expect("apply unpin");
    let entries = actor.ask(GetPinList).await.expect("get pin list");
    assert!(entries.is_empty());
}

#[tokio::test]
async fn apply_unpin_for_unknown_target_is_idempotent() {
    use crate::muc::pin::PinStateChange;
    use waddle_xmpp_core::xep0359::StanzaId;

    let actor = spawn_room_actor().await;
    let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
    let target = StanzaId::new("never-pinned".to_string(), jid::Jid::from(room_jid));
    actor
        .ask(ApplyPin {
            change: PinStateChange::Unpin {
                target_stanza_id: target,
            },
        })
        .await
        .expect("apply unpin no-op");
    let entries = actor.ask(GetPinList).await.expect("get pin list");
    assert!(entries.is_empty());
}

#[tokio::test]
async fn leave_by_real_jid_surfaces_is_persistent_true_for_default_rooms() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");

    let outcome = actor
        .ask(crate::muc::room_actor::LeaveByRealJid { sender_jid: alice })
        .await
        .expect("leave")
        .expect("outcome");
    assert!(
        outcome.is_persistent,
        "default RoomConfig is persistent (Waddle channel shape) — \
         must report is_persistent=true so callers do NOT evict"
    );
    assert_eq!(outcome.occupant_count, 0);
    assert!(outcome.removed_last_session);
}

#[tokio::test]
async fn leave_by_real_jid_surfaces_is_persistent_false_for_instant_rooms() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        persistent: false,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");

    let outcome = actor
        .ask(crate::muc::room_actor::LeaveByRealJid { sender_jid: alice })
        .await
        .expect("leave")
        .expect("outcome");
    assert!(
        !outcome.is_persistent,
        "instant rooms (XEP-0045 §10.1.3) report is_persistent=false \
         so the leave caller knows to evict the empty room from the registry"
    );
    assert_eq!(outcome.occupant_count, 0);
    assert!(outcome.removed_last_session);
}

#[tokio::test]
async fn is_dormant_true_for_fresh_empty_room() {
    let actor = spawn_room_actor().await;
    assert!(actor.ask(crate::muc::room_actor::IsDormant).await.unwrap());
}

#[tokio::test]
async fn is_dormant_false_while_occupants_present() {
    let actor = spawn_room_actor().await;
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");
    assert!(!actor.ask(crate::muc::room_actor::IsDormant).await.unwrap());
}

#[tokio::test]
async fn is_dormant_false_when_affiliation_is_set() {
    let actor = spawn_room_actor().await;
    let jid: BareJid = "alice@example.com".parse().expect("bare jid");
    actor
        .ask(ChangeAffiliation {
            jid,
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("change affiliation");
    assert!(
        !actor.ask(crate::muc::room_actor::IsDormant).await.unwrap(),
        "in-memory affiliation grants must keep the room non-dormant \
         so eviction does not drop them"
    );
}

#[tokio::test]
async fn is_dormant_true_after_last_occupant_leaves_with_no_stored_state() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");
    actor
        .ask(crate::muc::room_actor::LeaveByRealJid { sender_jid: alice })
        .await
        .expect("leave")
        .expect("outcome");
    assert!(
        actor.ask(crate::muc::room_actor::IsDormant).await.unwrap(),
        "an empty room with no subject/pins/affiliations is dormant \
         and safe for the room dormancy janitor to reap"
    );
}

// ---------------------------------------------------------------------------
// ListAffiliations
// ---------------------------------------------------------------------------

fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
}

async fn seed_one_per_tier(actor: &ActorRef<RoomActor>) {
    // Distinct local parts so the JID-ascending sort order is well
    // defined and verifiable in each test: alice < bob < carol < dave.
    actor
        .ask(ChangeAffiliation {
            jid: bare("alice@example.com"),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("seed owner");
    actor
        .ask(ChangeAffiliation {
            jid: bare("bob@example.com"),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("seed admin");
    actor
        .ask(ChangeAffiliation {
            jid: bare("carol@example.com"),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("seed member");
    actor
        .ask(ChangeAffiliation {
            jid: bare("dave@example.com"),
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("seed outcast");
}

#[tokio::test]
async fn list_affiliations_empty_room_returns_empty_vec() {
    let actor = spawn_room_actor().await;

    let entries = actor
        .ask(ListAffiliations { filter: None })
        .await
        .expect("ask");
    assert!(entries.is_empty(), "fresh room has no stored affiliations");
}

#[tokio::test]
async fn list_affiliations_no_filter_returns_all_tiers_sorted_by_jid() {
    let actor = spawn_room_actor().await;
    seed_one_per_tier(&actor).await;

    let entries = actor
        .ask(ListAffiliations { filter: None })
        .await
        .expect("ask");

    let jids: Vec<String> = entries.iter().map(|e| e.jid.to_string()).collect();
    assert_eq!(
        jids,
        vec![
            "alice@example.com".to_string(),
            "bob@example.com".to_string(),
            "carol@example.com".to_string(),
            "dave@example.com".to_string(),
        ],
        "entries must be sorted ascending by JID"
    );

    let tiers: Vec<Affiliation> = entries.iter().map(|e| e.affiliation).collect();
    assert_eq!(
        tiers,
        vec![
            Affiliation::Owner,
            Affiliation::Admin,
            Affiliation::Member,
            Affiliation::Outcast,
        ],
        "each seeded tier must be present"
    );

    // granted_at is intentionally None today — storage gap noted in
    // AffiliationEntry's doc-comment.
    assert!(
        entries.iter().all(|e| e.granted_at.is_none()),
        "granted_at is not yet recorded by the in-memory store"
    );
}

#[tokio::test]
async fn room_snapshot_includes_durable_member_recipients_from_same_actor_read() {
    let actor = spawn_room_actor().await;
    seed_one_per_tier(&actor).await;
    let sender = test_full_jid("sender");
    actor
        .ask(Join {
            nick: "sender".to_string(),
            real_jid: sender.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join sender");

    let snapshot = actor
        .ask(GetRoomSnapshot { sender_jid: sender })
        .await
        .expect("snapshot");

    let recipients = snapshot
        .durable_recipient_bare_jids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        recipients,
        vec![
            "alice@example.com".to_string(),
            "bob@example.com".to_string(),
            "carol@example.com".to_string(),
        ],
        "snapshot recipients must be affiliation-derived and exclude outcasts/non-members"
    );
}

// ---------------------------------------------------------------------------
// F1 — hydrated durable recipients must not survive membership removal.
//
// The spawn-time hydrated mirror (#1135) was only filtered against
// `Affiliation::Outcast`, so every removal path that sets
// `Affiliation::None` (group-DM leave, XEP-0045 admin removal, tuple
// deletion) left the ex-member receiving inbox rows with preview text
// until the actor respawned.
// ---------------------------------------------------------------------------

/// Mutable durable membership source for hydrating a room actor
/// directly in tests (mirrors the registry-level fake in
/// `room_registry_actor/tests.rs`). Its member list can be updated
/// mid-test to model the durable store changing (a GraphQL
/// `persist_channel_affiliation` deleting channel tuples), and it can
/// be armed to fail so the re-hydration failure path is exercisable.
struct FixedMembershipSource {
    members: std::sync::Mutex<Vec<BareJid>>,
    fail: std::sync::atomic::AtomicBool,
}

impl FixedMembershipSource {
    fn new(members: Vec<BareJid>) -> Self {
        Self {
            members: std::sync::Mutex::new(members),
            fail: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Replace what the durable store reports — models tuple deletion
    /// landing before the actor processes the affiliation change.
    fn set_members(&self, members: Vec<BareJid>) {
        *self.members.lock().expect("members lock") = members;
    }

    /// Make every subsequent query fail.
    fn fail_queries(&self) {
        self.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl crate::muc::affiliation::DurableMembershipSource for FixedMembershipSource {
    fn list_durable_member_jids(
        &self,
        _waddle_id: &str,
        _channel_id: &str,
    ) -> crate::muc::affiliation::DurableMembershipFuture<'_> {
        let fail = self.fail.load(std::sync::atomic::Ordering::SeqCst);
        let members = self.members.lock().expect("members lock").clone();
        Box::pin(async move {
            if fail {
                Err(crate::XmppError::internal(
                    "simulated durable membership source failure",
                ))
            } else {
                Ok(members)
            }
        })
    }
}

async fn hydrate_durable_recipients(
    actor: &ActorRef<RoomActor>,
    members: Vec<BareJid>,
) -> std::sync::Arc<FixedMembershipSource> {
    let source = std::sync::Arc::new(FixedMembershipSource::new(members));
    actor
        .ask(HydrateDurableRecipients {
            source: std::sync::Arc::clone(&source) as _,
        })
        .await
        .expect("hydrate durable recipients");
    source
}

async fn snapshot_recipient_strings(actor: &ActorRef<RoomActor>) -> Vec<String> {
    let snapshot = actor
        .ask(GetRoomSnapshot {
            sender_jid: test_full_jid("observer"),
        })
        .await
        .expect("snapshot");
    snapshot
        .durable_recipient_bare_jids
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[tokio::test]
async fn change_affiliation_to_none_prunes_hydrated_durable_recipient() {
    let actor = spawn_room_actor().await;
    let source = hydrate_durable_recipients(&actor, vec![bare("alice@example.com")]).await;
    // Channel-only member: the durable channel tuples were deleted
    // before the affiliation change reaches the actor.
    source.set_members(Vec::new());

    actor
        .ask(ChangeAffiliation {
            jid: bare("alice@example.com"),
            affiliation: Affiliation::None,
        })
        .await
        .expect("remove membership");

    assert!(
        snapshot_recipient_strings(&actor).await.is_empty(),
        "a hydrated durable recipient whose affiliation is set to None \
         must stop receiving inbox fan-out immediately, not at respawn"
    );
}

#[tokio::test]
async fn change_affiliation_to_outcast_prunes_hydrated_durable_recipient() {
    let actor = spawn_room_actor().await;
    // The source deliberately KEEPS alice: a ban is unambiguous, so the
    // direct prune must be final without any re-hydration query (and
    // the snapshot's Outcast filter backs it up).
    let _source = hydrate_durable_recipients(&actor, vec![bare("alice@example.com")]).await;

    actor
        .ask(ChangeAffiliation {
            jid: bare("alice@example.com"),
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("ban member");

    assert!(
        snapshot_recipient_strings(&actor).await.is_empty(),
        "a banned hydrated durable recipient must drop out of fan-out"
    );
}

#[tokio::test]
async fn readded_member_reappears_in_durable_recipients_after_hydrated_prune() {
    let actor = spawn_room_actor().await;
    let source = hydrate_durable_recipients(&actor, vec![bare("alice@example.com")]).await;
    source.set_members(Vec::new());

    actor
        .ask(ChangeAffiliation {
            jid: bare("alice@example.com"),
            affiliation: Affiliation::None,
        })
        .await
        .expect("remove membership");
    actor
        .ask(ChangeAffiliation {
            jid: bare("alice@example.com"),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("re-add membership");

    assert_eq!(
        snapshot_recipient_strings(&actor).await,
        vec!["alice@example.com".to_string()],
        "a re-added member must reappear via the affiliation-list side \
         of the durable-recipient union"
    );
}

#[tokio::test]
async fn apply_affiliation_change_to_none_prunes_hydrated_durable_recipient() {
    let actor = spawn_room_actor().await;
    let source = hydrate_durable_recipients(&actor, vec![bare("alice@example.com")]).await;
    source.set_members(Vec::new());

    actor
        .ask(ApplyAffiliationChange {
            actor: None,
            jid: bare("alice@example.com"),
            affiliation: Affiliation::None,
        })
        .await
        .expect("apply affiliation change");

    assert!(
        snapshot_recipient_strings(&actor).await.is_empty(),
        "ApplyAffiliationChange to None must prune the hydrated mirror"
    );
}

// ---------------------------------------------------------------------------
// R1 (round-2 review) — revoking only the explicit CHANNEL grant must
// not prune a user who remains durably entitled via the SPACE: the
// hydration union covers channel AND space relations, so an
// affiliation change to None re-runs hydration and the mirror
// converges to the durable truth instead of guessing. Only a source
// failure falls back to the prune (privacy beats availability — F1).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn space_entitled_member_survives_channel_grant_revocation() {
    let actor = spawn_room_actor().await;
    // The durable union still reports alice (space-level member) even
    // after her explicit channel grant is deleted.
    let _source = hydrate_durable_recipients(&actor, vec![bare("alice@example.com")]).await;

    actor
        .ask(ChangeAffiliation {
            jid: bare("alice@example.com"),
            affiliation: Affiliation::None,
        })
        .await
        .expect("revoke channel grant");

    assert_eq!(
        snapshot_recipient_strings(&actor).await,
        vec!["alice@example.com".to_string()],
        "a space-entitled member must stay a durable recipient when only \
         the explicit channel grant is revoked"
    );
}

#[tokio::test]
async fn space_entitled_member_survives_admin_item_revocation() {
    let actor = spawn_room_actor().await;
    let _source = hydrate_durable_recipients(&actor, vec![bare("alice@example.com")]).await;

    actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: Some(bare("alice@example.com")),
                nick: None,
                affiliation: Some(Affiliation::None),
                role: None,
                reason: None,
            }],
        })
        .await
        .expect("apply admin items");

    assert_eq!(
        snapshot_recipient_strings(&actor).await,
        vec!["alice@example.com".to_string()],
        "the XEP-0045 admin removal path must also converge to the \
         durable truth for space-entitled members"
    );
}

#[tokio::test]
async fn space_entitled_member_survives_members_only_enforcement() {
    let actor = spawn_room_actor().await;
    let _source = hydrate_durable_recipients(&actor, vec![bare("alice@example.com")]).await;
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::None,
        })
        .await
        .expect("join alice");

    actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: Vec::new(),
        })
        .await
        .expect("enforce members-only");

    assert_eq!(
        snapshot_recipient_strings(&actor).await,
        vec!["alice@example.com".to_string()],
        "members-only enforcement must not drop a member the durable \
         union still reports (space entitlement)"
    );
}

#[tokio::test]
async fn rehydration_failure_keeps_revoked_jid_excluded() {
    let actor = spawn_room_actor().await;
    let source = hydrate_durable_recipients(&actor, vec![bare("alice@example.com")]).await;
    // The re-hydration query fails: fail toward NOT delivering to the
    // removed jid — keep the prune (F1: privacy beats availability).
    source.fail_queries();

    actor
        .ask(ChangeAffiliation {
            jid: bare("alice@example.com"),
            affiliation: Affiliation::None,
        })
        .await
        .expect("revoke membership");

    assert!(
        snapshot_recipient_strings(&actor).await.is_empty(),
        "when the re-hydration query fails, the None'd jid must stay \
         excluded from durable fan-out"
    );
}

#[tokio::test]
async fn apply_admin_items_removal_prunes_hydrated_durable_recipient() {
    let actor = spawn_room_actor().await;
    let source = hydrate_durable_recipients(&actor, vec![bare("alice@example.com")]).await;
    source.set_members(Vec::new());

    actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: Some(bare("alice@example.com")),
                nick: None,
                affiliation: Some(Affiliation::None),
                role: None,
                reason: None,
            }],
        })
        .await
        .expect("apply admin items");

    assert!(
        snapshot_recipient_strings(&actor).await.is_empty(),
        "the XEP-0045 admin batch removal path must prune the hydrated mirror"
    );
}

#[tokio::test]
async fn enforce_members_only_affiliations_prunes_hydrated_durable_recipient() {
    let actor = spawn_room_actor().await;
    let source = hydrate_durable_recipients(&actor, vec![bare("alice@example.com")]).await;
    source.set_members(Vec::new());
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::None,
        })
        .await
        .expect("join alice");

    // No durable affiliation tuples: the enforcement pass resolves
    // every occupant to Affiliation::None.
    actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: Vec::new(),
        })
        .await
        .expect("enforce members-only");

    assert!(
        snapshot_recipient_strings(&actor).await.is_empty(),
        "the members-only enforcement pass must prune hydrated \
         recipients whose durable affiliation resolved to None"
    );
}

#[tokio::test]
async fn list_affiliations_filter_outcast_returns_only_outcasts() {
    let actor = spawn_room_actor().await;
    seed_one_per_tier(&actor).await;

    let entries = actor
        .ask(ListAffiliations {
            filter: Some(Affiliation::Outcast),
        })
        .await
        .expect("ask");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].jid, bare("dave@example.com"));
    assert_eq!(entries[0].affiliation, Affiliation::Outcast);
}

#[tokio::test]
async fn list_affiliations_filter_member_returns_only_members() {
    let actor = spawn_room_actor().await;
    seed_one_per_tier(&actor).await;
    // Add a second member to verify filter returns *all* matches, not
    // just the first.
    actor
        .ask(ChangeAffiliation {
            jid: bare("erin@example.com"),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("seed second member");

    let entries = actor
        .ask(ListAffiliations {
            filter: Some(Affiliation::Member),
        })
        .await
        .expect("ask");

    let jids: Vec<String> = entries.iter().map(|e| e.jid.to_string()).collect();
    assert_eq!(
        jids,
        vec![
            "carol@example.com".to_string(),
            "erin@example.com".to_string(),
        ],
        "member filter returns all members in JID-ascending order"
    );
    assert!(entries.iter().all(|e| e.affiliation == Affiliation::Member));
}

#[tokio::test]
async fn list_affiliations_filter_none_tier_returns_empty() {
    // Affiliation::None is the implicit default and is never stored in
    // the affiliation list (see `AffiliationList::set` — assigning
    // Affiliation::None removes the entry). A filter request for it
    // must therefore always come back empty even when other tiers are
    // populated.
    let actor = spawn_room_actor().await;
    seed_one_per_tier(&actor).await;

    let entries = actor
        .ask(ListAffiliations {
            filter: Some(Affiliation::None),
        })
        .await
        .expect("ask");

    assert!(
        entries.is_empty(),
        "Affiliation::None is not stored; filter must return empty"
    );
}

// ---------------------------------------------------------------------------
// XEP-0272 `<muji xmlns='urn:xmpp:jingle:muji:0'/>` presence-update tests.
// XEP-0045 §5.1.3 + XEP-0272 §Joining / §Leaving.
// ---------------------------------------------------------------------------

fn audio_muji() -> crate::xep::xep0272::Muji {
    use crate::xep::xep0167::MediaKind;
    use crate::xep::xep0272::{Creator, Muji, MujiContent};
    Muji::with_contents(vec![MujiContent::new(
        "audio",
        Creator::Initiator,
        MediaKind::Audio,
    )])
}

fn video_muji() -> crate::xep::xep0272::Muji {
    use crate::xep::xep0167::MediaKind;
    use crate::xep::xep0272::{Creator, Muji, MujiContent};
    Muji::with_contents(vec![MujiContent::new(
        "video",
        Creator::Initiator,
        MediaKind::Video,
    )])
}

fn empty_muji() -> crate::xep::xep0272::Muji {
    // No `<preparing/>`, no `<content/>` → XEP-0272 §Leaving "absence
    // marker": the participant has exited the call.
    crate::xep::xep0272::Muji::default()
}

fn preparing_muji() -> crate::xep::xep0272::Muji {
    crate::xep::xep0272::Muji::preparing()
}

#[tokio::test]
async fn upsert_muji_presence_returns_none_for_non_occupant() {
    // A WebSocket session that isn't yet in the room MUST NOT be
    // able to push a Muji presence advertisement — the server falls
    // back to `handle_muc_join` for that case.
    let actor = spawn_room_actor().await;
    let stranger = test_full_jid("stranger");

    let outcome = actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: stranger,
            muji: audio_muji(),
        })
        .await
        .expect("ask");
    assert!(
        outcome.is_none(),
        "non-occupant must not be allowed to advertise a Muji presence"
    );
}

#[tokio::test]
async fn upsert_muji_presence_active_stores_and_returns_muji() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");

    let outcome = actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("ask")
        .expect("alice is an occupant");

    assert_eq!(outcome.update.sender_nick, "alice");
    let active = outcome
        .active_muji
        .as_ref()
        .expect("active Muji advertisement is stored");
    assert!(active.is_active());
    assert_eq!(active.contents.len(), 1);
    // Recipient list always includes the sender so the WebSocket
    // session that emitted the presence sees the reflection back
    // (XEP-0045 §5.1.3 "presence MUST be reflected to all
    // occupants, including the sender").
    assert!(
        outcome.update.recipients.iter().any(|jid| jid == &alice),
        "sender must be in the recipient set"
    );
}

#[tokio::test]
async fn upsert_muji_presence_recipients_include_every_sibling_session_of_sender() {
    // Multi-resource regression — pins the contract that the
    // `presence/muc_update.rs` WebSocket handler relies on. When
    // alice has TWO sessions in the room under the same nick
    // (e.g. desktop + mobile, XEP-0045 §7.2 same-bare multi-session)
    // and one session advertises a Muji presence, the recipients list
    // MUST contain BOTH session full JIDs. The router uses this list
    // to dispatch the reflection per session — without the sibling
    // full JID, the second client never receives the live indicator.
    let actor = spawn_room_actor().await;
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: desktop.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("mobile join recognized as same-bare multi-session");

    let outcome = actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: desktop.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("ask")
        .expect("alice is an occupant");

    assert!(
        outcome.update.recipients.iter().any(|jid| jid == &desktop),
        "sender's own session must be in the recipient set: {:?}",
        outcome.update.recipients
    );
    assert!(
        outcome.update.recipients.iter().any(|jid| jid == &mobile),
        "sibling session of the same bare JID must also be in the recipient set: {:?}",
        outcome.update.recipients
    );
}

#[tokio::test]
async fn same_nick_sibling_preparing_does_not_clobber_active_muji_snapshot() {
    let actor = spawn_room_actor().await;
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: desktop.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("mobile join");

    actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: desktop.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("active ask")
        .expect("desktop is an occupant");

    let preparing = actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: mobile.clone(),
            muji: preparing_muji(),
        })
        .await
        .expect("preparing ask")
        .expect("mobile is an occupant");

    assert!(
        preparing
            .sender_muji
            .as_ref()
            .is_some_and(|muji| muji.preparing && !muji.is_active()),
        "mobile still receives its preparing reflection"
    );
    assert!(
        preparing
            .active_muji
            .as_ref()
            .is_some_and(|muji| muji.is_active() && muji.preparing),
        "other occupants see both desktop's active advertisement and mobile's preparing state"
    );

    let replay = actor
        .ask(JoinWithAffiliation {
            sender_jid: bob,
            nick: "bob".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("bob join");

    let desktop_replay = replay
        .existing_occupants
        .iter()
        .find(|occupant| occupant.nick == "alice" && occupant.jid == desktop)
        .expect("bob sees alice desktop");
    let mobile_replay = replay
        .existing_occupants
        .iter()
        .find(|occupant| occupant.nick == "alice" && occupant.jid == mobile)
        .expect("bob sees alice mobile");
    assert!(
        desktop_replay
            .muji
            .as_ref()
            .is_some_and(|muji| muji.is_active() && !muji.preparing),
        "late join replay keeps desktop's active advertisement on the desktop JID"
    );
    assert!(
        mobile_replay
            .muji
            .as_ref()
            .is_some_and(|muji| muji.preparing && !muji.is_active()),
        "late join replay keeps mobile's preparing advertisement on the mobile JID"
    );
}

#[tokio::test]
async fn late_join_replay_includes_preparing_only_same_nick_muji_with_exact_owner() {
    let actor = spawn_room_actor().await;
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: desktop.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("mobile join");

    actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: mobile.clone(),
            muji: preparing_muji(),
        })
        .await
        .expect("preparing ask")
        .expect("mobile is an occupant");

    let replay = actor
        .ask(JoinWithAffiliation {
            sender_jid: bob,
            nick: "bob".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("bob join");

    let desktop_replay = replay
        .existing_occupants
        .iter()
        .find(|occupant| occupant.nick == "alice" && occupant.jid == desktop)
        .expect("bob sees alice desktop");
    let mobile_replay = replay
        .existing_occupants
        .iter()
        .find(|occupant| occupant.nick == "alice" && occupant.jid == mobile)
        .expect("bob sees alice mobile");
    assert!(
        desktop_replay.muji.is_none(),
        "desktop has no Muji state to replay"
    );
    assert!(
        mobile_replay
            .muji
            .as_ref()
            .is_some_and(|muji| muji.preparing && !muji.is_active()),
        "late join replay preserves preparing-only state on its exact full JID"
    );
}

#[test]
fn same_nick_active_resources_are_aggregated_for_other_occupants() {
    let mut room = test_room();
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile");

    room.upsert_muji_presence("alice", desktop, audio_muji());
    let video = room.upsert_muji_presence("alice", mobile, video_muji());

    let aggregate = video
        .room_muji
        .as_ref()
        .expect("other occupants see same-nick aggregate Muji");
    assert!(aggregate.is_active(), "aggregate has content");
    assert_eq!(
        aggregate.contents.len(),
        2,
        "aggregate carries all same-nick active resources instead of choosing one"
    );
}

#[test]
fn active_call_started_is_room_wide_not_per_nick() {
    let mut room = test_room();
    let alice: FullJid = "alice@example.com/desktop".parse().expect("alice");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob");

    let alice_start = room.upsert_muji_presence("alice", alice, audio_muji());
    let bob_join = room.upsert_muji_presence("bob", bob, video_muji());

    assert!(
        alice_start.active_call_started,
        "first active Muji in the room starts the call session"
    );
    assert!(
        !bob_join.active_call_started,
        "another occupant joining the existing active call must not emit a second anchor"
    );
}

#[tokio::test]
async fn upsert_muji_presence_empty_clears_state_and_returns_none() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");
    // First, advertise active.
    actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("ask")
        .expect("alice is an occupant");

    // Transition to empty (XEP-0272 §Leaving "absence" marker) — the
    // actor should clear stored state AND signal no active Muji.
    // The presence broadcaster uses `None` here to strip the
    // `<muji/>` payload from the reflected stanza, telling other
    // occupants the call ended.
    let outcome = actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: empty_muji(),
        })
        .await
        .expect("ask")
        .expect("alice is an occupant");
    assert!(
        outcome.active_muji.is_none(),
        "empty Muji clears the stored extension"
    );
}

#[tokio::test]
async fn clear_muji_presence_clears_existing_state_without_muji_payload() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    let bob = test_full_jid("bob");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("alice join");
    actor
        .ask(Join {
            nick: "bob".to_string(),
            real_jid: bob.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("bob join");
    actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("ask")
        .expect("alice is an occupant");

    let outcome = actor
        .ask(crate::muc::room_actor::ClearMujiPresence {
            sender_jid: alice.clone(),
        })
        .await
        .expect("ask")
        .expect("alice had Muji state to clear");

    assert_eq!(outcome.update.sender_nick, "alice");
    assert!(
        outcome.active_muji.is_none(),
        "available presence without <muji/> clears the stored Muji advertisement"
    );
    assert!(
        outcome.update.recipients.iter().any(|jid| jid == &bob),
        "remaining occupants must be notified of the clear"
    );

    let carol = test_full_jid("carol");
    let join_outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: carol,
            nick: "carol".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("carol join");
    let alice_replay = join_outcome
        .existing_occupants
        .iter()
        .find(|o| o.nick == "alice")
        .expect("alice still present");
    assert!(
        alice_replay.muji.is_none(),
        "late join replay must not include stale Muji after a no-payload clear"
    );
}

#[tokio::test]
async fn clear_muji_presence_reflects_plain_presence_when_no_state_exists() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("alice join");

    let outcome = actor
        .ask(crate::muc::room_actor::ClearMujiPresence { sender_jid: alice })
        .await
        .expect("ask");

    let outcome = outcome.expect("existing occupant presence update is reflected");
    assert_eq!(outcome.update.sender_nick, "alice");
    assert!(outcome.sender_muji.is_none());
    assert!(outcome.active_muji.is_none());
}

#[tokio::test]
async fn join_replay_includes_active_muji_from_existing_occupant() {
    // Late joiners see the chip light up immediately via the join
    // replay, not just after the next presence update from the call
    // participant.
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    let bob = test_full_jid("bob");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("alice join");
    actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("ask")
        .expect("alice is an occupant");

    let join_outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: bob,
            nick: "bob".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("bob join");

    let alice_replay = join_outcome
        .existing_occupants
        .iter()
        .find(|o| o.nick == "alice")
        .expect("alice in existing occupants");
    let muji = alice_replay
        .muji
        .as_ref()
        .expect("alice's Muji advertisement is replayed to bob");
    assert!(muji.is_active());
}

#[tokio::test]
async fn leaving_occupant_clears_muji_state() {
    // A tab close mid-call MUST NOT leave the chip lit forever for
    // remaining occupants.
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    let carol = test_full_jid("carol");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("alice join");
    actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("ask")
        .expect("alice is an occupant");

    // Alice leaves; carol joins after.
    actor
        .ask(crate::muc::room_actor::LeaveByRealJid { sender_jid: alice })
        .await
        .expect("leave");
    let join_outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: carol,
            nick: "carol".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("carol join");

    assert!(
        !join_outcome
            .existing_occupants
            .iter()
            .any(|o| o.nick == "alice"),
        "alice is gone, so no replay entry for her"
    );
}

#[tokio::test]
async fn leaving_originator_session_clears_muji_state_even_with_peer_sessions_remaining() {
    // Multi-resource ghost regression. alice has two sessions on the
    // same nick — desktop on the call, mobile in the room but not on
    // the call. When desktop disconnects, the Muji advertisement
    // bound to that specific session must clear.
    let actor = spawn_room_actor().await;
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: desktop.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("mobile join recognized as same-bare multi-session");

    // Desktop is the session that advertised the active Muji.
    let active_outcome = actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: desktop.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("ask")
        .expect("alice is an occupant");
    assert!(
        active_outcome.active_muji.is_some(),
        "desktop's active advertisement is stored",
    );

    // Desktop disconnects uncleanly; mobile is still in the room.
    let leave_outcome = actor
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: desktop.clone(),
        })
        .await
        .expect("desktop leave");
    let leave_outcome = leave_outcome.expect("alice present");
    assert!(
        !leave_outcome.removed_last_session,
        "mobile is still in the room, so alice's nick is not vacated"
    );
    assert!(
        leave_outcome.cleared_muji_state,
        "leaving originator resource must report that Muji state was cleared"
    );
    assert_eq!(
        leave_outcome.remaining_nick_real_jid.as_ref(),
        Some(&mobile),
        "remaining same-nick resource is the canonical identity for the clear broadcast"
    );

    // A late joiner (carol) must NOT see alice's stale Muji ad.
    let carol = test_full_jid("carol");
    let join_outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: carol,
            nick: "carol".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("carol join");
    let alice_replay = join_outcome
        .existing_occupants
        .iter()
        .find(|o| o.nick == "alice")
        .expect("alice (via mobile) still in occupant list");
    assert!(
        alice_replay.muji.is_none(),
        "Muji cleared when originating session left, even with peer sessions remaining",
    );
}

#[tokio::test]
async fn leaving_non_originator_session_preserves_muji_state() {
    // The mirror of the multi-session ghost test: if alice has two
    // sessions and only the NON-call session leaves, the Muji
    // advertisement bound to the still-present originator must
    // survive. This pins that the per-session keying isn't
    // over-eager.
    let actor = spawn_room_actor().await;
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: desktop.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("mobile join");

    // Desktop owns the Muji advertisement.
    actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: desktop.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("ask")
        .expect("alice is an occupant");

    // Mobile (NOT the originator) disconnects.
    actor
        .ask(crate::muc::room_actor::LeaveByRealJid { sender_jid: mobile })
        .await
        .expect("mobile leave");

    // A late joiner sees alice still on the call — desktop is the
    // originator and is still in the room.
    let carol = test_full_jid("carol");
    let join_outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: carol,
            nick: "carol".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("carol join");
    let alice_replay = join_outcome
        .existing_occupants
        .iter()
        .find(|o| o.nick == "alice")
        .expect("alice still in room via desktop");
    let muji = alice_replay
        .muji
        .as_ref()
        .expect("Muji preserved when non-originator session leaves");
    assert!(muji.is_active());
}

#[tokio::test]
async fn leaving_one_active_same_nick_session_preserves_sibling_active_muji_state() {
    let actor = spawn_room_actor().await;
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: desktop.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("mobile join");

    actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: desktop.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("desktop active")
        .expect("desktop is an occupant");
    actor
        .ask(crate::muc::room_actor::UpsertMujiPresence {
            sender_jid: mobile.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("mobile active")
        .expect("mobile is an occupant");

    let leave_outcome = actor
        .ask(crate::muc::room_actor::LeaveByRealJid { sender_jid: mobile })
        .await
        .expect("mobile leave")
        .expect("alice present");

    assert!(leave_outcome.cleared_muji_state);
    assert!(
        leave_outcome
            .remaining_muji
            .as_ref()
            .is_some_and(|muji| muji.is_active()),
        "desktop's active Muji must remain after mobile hangs up"
    );

    let carol = test_full_jid("carol");
    let join_outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: carol,
            nick: "carol".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("carol join");
    let alice_replay = join_outcome
        .existing_occupants
        .iter()
        .find(|o| o.nick == "alice")
        .expect("alice still in room via desktop");
    assert!(
        alice_replay
            .muji
            .as_ref()
            .is_some_and(|muji| muji.is_active()),
        "late join replay preserves the remaining active sibling"
    );
}

#[tokio::test]
async fn kick_reports_every_removed_session_for_sfu_eviction() {
    let actor = spawn_room_actor().await;

    // Alice is in the room from two devices under one nick; both
    // sessions must be surfaced when she is kicked (issue #935).
    actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid_resource("alice", "desktop"),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("join alice desktop");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid_resource("alice", "mobile"),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("join alice mobile");

    let applied = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("mod"),
            sender_affiliation: Affiliation::Admin,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: None,
                nick: Some("alice".to_string()),
                affiliation: None,
                role: Some(Role::None),
                reason: None,
            }],
        })
        .await
        .expect("kick alice");

    let mut removed = applied.removed_by_moderation.clone();
    removed.sort_by_key(std::string::ToString::to_string);
    assert_eq!(
        removed,
        vec![
            test_full_jid_resource("alice", "desktop"),
            test_full_jid_resource("alice", "mobile"),
        ],
        "kick (XEP-0045 status 307) must surface all removed sessions"
    );
    assert!(
        !applied.presence_updates.is_empty(),
        "kick still fans out unavailable presence"
    );
}

#[tokio::test]
async fn ban_reports_every_removed_session_for_sfu_eviction() {
    let actor = spawn_room_actor().await;

    actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid_resource("alice", "desktop"),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("join alice desktop");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid_resource("alice", "mobile"),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("join alice mobile");

    let applied = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("mod"),
            sender_affiliation: Affiliation::Admin,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: Some("alice@example.com".parse().expect("bare jid")),
                nick: None,
                affiliation: Some(Affiliation::Outcast),
                role: None,
                reason: None,
            }],
        })
        .await
        .expect("ban alice");

    let mut removed = applied.removed_by_moderation.clone();
    removed.sort_by_key(std::string::ToString::to_string);
    assert_eq!(
        removed,
        vec![
            test_full_jid_resource("alice", "desktop"),
            test_full_jid_resource("alice", "mobile"),
        ],
        "ban (XEP-0045 status 301) must surface all removed sessions"
    );
}

#[tokio::test]
async fn non_removing_admin_changes_report_no_moderation_removals() {
    let actor = spawn_room_actor().await;

    actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid("alice"),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("join alice");

    // A plain role demotion (still an occupant afterwards) must NOT
    // mark the session for SFU eviction.
    let applied = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("mod"),
            sender_affiliation: Affiliation::Admin,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: None,
                nick: Some("alice".to_string()),
                affiliation: None,
                role: Some(Role::Visitor),
                reason: None,
            }],
        })
        .await
        .expect("demote alice");

    assert!(
        applied.removed_by_moderation.is_empty(),
        "a role change that keeps the occupant must not evict their call session"
    );
}

#[tokio::test]
async fn admin_set_error_after_ban_item_must_not_partially_apply() {
    // #935 review finding: [ban X, demote sole owner A, promote C]
    // passed the batch pre-validation (final state has owner C) but
    // the mutation loop's per-step last-owner check erred AFTER the
    // ban of X already mutated the room — X's removal and 301
    // presences (and the SFU eviction contract built on them) were
    // silently dropped. The set must be atomic: rejected before any
    // mutation.
    let actor = spawn_room_actor().await;
    let owner_bare: BareJid = "owner@example.com".parse().expect("owner jid");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid("victim"),
            nick: "victim".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("victim joins");
    actor
        .ask(ChangeAffiliation {
            jid: owner_bare.clone(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("set owner");

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![
                AdminItem {
                    jid: Some("victim@example.com".parse().expect("bare")),
                    nick: None,
                    affiliation: Some(Affiliation::Outcast),
                    role: None,
                    reason: None,
                },
                AdminItem {
                    jid: Some(owner_bare.clone()),
                    nick: None,
                    affiliation: Some(Affiliation::Member),
                    role: None,
                    reason: None,
                },
                AdminItem {
                    jid: Some("successor@example.com".parse().expect("bare")),
                    nick: None,
                    affiliation: Some(Affiliation::Owner),
                    role: None,
                    reason: None,
                },
            ],
        })
        .await;

    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(
                AdminApplyError::CannotRemoveLastOwner
            ))
        ),
        "order-sensitive batch is rejected up front"
    );
    let occupant = actor
        .ask(GetOccupantByNick {
            nick: "victim".to_string(),
        })
        .await
        .expect("ask")
        .expect("victim must still be an occupant — no partial application");
    assert_eq!(occupant.affiliation, Affiliation::Member);
}

#[tokio::test]
async fn admin_set_with_owner_grant_before_demotion_applies_fully() {
    // Order-valid counterpart of the atomicity test: promoting the
    // successor BEFORE demoting the sole owner is accepted, and a ban
    // in the same set still surfaces its removed sessions.
    let actor = spawn_room_actor().await;
    let owner_bare: BareJid = "owner@example.com".parse().expect("owner jid");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid("victim"),
            nick: "victim".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("victim joins");
    actor
        .ask(ChangeAffiliation {
            jid: owner_bare.clone(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("set owner");

    let applied = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![
                AdminItem {
                    jid: Some("successor@example.com".parse().expect("bare")),
                    nick: None,
                    affiliation: Some(Affiliation::Owner),
                    role: None,
                    reason: None,
                },
                AdminItem {
                    jid: Some(owner_bare.clone()),
                    nick: None,
                    affiliation: Some(Affiliation::Member),
                    role: None,
                    reason: None,
                },
                AdminItem {
                    jid: Some("victim@example.com".parse().expect("bare")),
                    nick: None,
                    affiliation: Some(Affiliation::Outcast),
                    role: None,
                    reason: None,
                },
            ],
        })
        .await
        .expect("order-valid batch applies");

    assert_eq!(
        applied.removed_by_moderation,
        vec![test_full_jid("victim")],
        "the ban in the batch surfaces its removed session"
    );
}
