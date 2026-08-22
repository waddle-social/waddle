use super::*;

use crate::muc::admin::AdminItem;
use crate::xep::xep0421::OccupantIdSecret;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

mod mediated_invites;

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

fn test_claim_fence(room_jid: &BareJid) -> crate::muc::RoomClaimFenceContext {
    crate::muc::RoomClaimFenceContext::new(
        crate::ownership::Entity::new(
            crate::ownership::EntityType::RoomActor,
            room_jid.to_string(),
        ),
        crate::ownership::NodeIdentity::new("test-node", "test-node-epoch"),
        crate::ownership::ClaimEpoch(1),
    )
}

fn validate_test_claim_fence(
    room_jid: &BareJid,
    fence: &crate::muc::RoomClaimFenceContext,
) -> Result<(), crate::XmppError> {
    if fence == &test_claim_fence(room_jid) {
        Ok(())
    } else {
        Err(crate::XmppError::OwnershipLost {
            entity: crate::ownership::Entity::new(
                crate::ownership::EntityType::RoomActor,
                room_jid.to_string(),
            ),
        })
    }
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

#[tokio::test]
async fn restoring_live_roster_rederives_occupant_authorization() {
    let mut stale_room = test_room();
    stale_room.add_occupant(crate::muc::Occupant {
        real_jid: test_full_jid("visitor"),
        nick: "visitor".to_string(),
        role: Role::Participant,
        affiliation: crate::Affiliation::None,
        is_remote: false,
        home_server: None,
    });
    let mut authoritative_room = test_room();
    authoritative_room.config.moderated = true;
    let actor = RoomActor::spawn(RoomActor::new(authoritative_room, test_secret()));

    actor
        .ask(RestoreLiveRoster {
            room: stale_room,
            occupancy_revision: 0,
            departures: Default::default(),
        })
        .await
        .expect("restore live roster");

    let snapshot = actor.ask(GetSnapshot).await.expect("room snapshot");
    let visitor = snapshot
        .room
        .occupants
        .get("visitor")
        .expect("restored visitor");
    assert_eq!(visitor.affiliation, crate::Affiliation::None);
    assert_eq!(visitor.role, Role::Visitor);
}

async fn current_admission_revision(actor: &ActorRef<RoomActor>) -> u64 {
    actor
        .ask(GetSnapshot)
        .await
        .expect("room snapshot")
        .admission_revision
}

fn departed(disposition: LeaveDisposition) -> LeaveOutcome {
    match disposition {
        LeaveDisposition::Left(outcome) => *outcome,
        other => panic!("expected an applied departure, got {other:?}"),
    }
}

#[test]
fn durable_restore_only_advances_for_admission_policy_changes() {
    let mut actor = RoomActor::new(test_room(), test_secret());
    let cosmetic_config = RoomConfig {
        name: "Restored room name".to_string(),
        description: Some("Restored description".to_string()),
        enable_logging: false,
        ..RoomConfig::default()
    };
    actor.install_durable_room_state(crate::muc::durable::DurableRoomState {
        coordinates: None,
        config_coordinates: None,
        waddle_id: "waddle-1".to_string(),
        channel_id: "channel-1".to_string(),
        config: cosmetic_config,
        subject: None,
        affiliations: Vec::new(),
    });
    assert_eq!(
        actor.admission_revision, 0,
        "cosmetic restore fields must not invalidate admission work"
    );

    let mut admission_config = actor.room.config.clone();
    admission_config.members_only = false;
    actor.install_durable_room_state(crate::muc::durable::DurableRoomState {
        coordinates: None,
        config_coordinates: None,
        waddle_id: "waddle-1".to_string(),
        channel_id: "channel-1".to_string(),
        config: admission_config,
        subject: None,
        affiliations: Vec::new(),
    });
    assert_eq!(
        actor.admission_revision, 1,
        "membership policy changes must invalidate admission work"
    );
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
async fn test_join_owner_affiliation_allowed_when_room_full() {
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

    actor
        .ask(Join {
            nick: "owner".to_string(),
            real_jid: test_full_jid("owner"),
            role: Role::Moderator,
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("owner affiliation should bypass full-room rejection");

    let count = actor.ask(OccupantCount).await.expect("ask");
    assert_eq!(count, 2);
}

#[tokio::test]
async fn test_join_with_admin_affiliation_allowed_when_room_full() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        max_occupants: 1,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");
    let admin = test_full_jid("admin");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("first join should succeed");

    let outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: admin,
            nick: "admin".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Admin),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("admin affiliation should bypass full-room rejection");

    assert_eq!(outcome.new_occupant_affiliation, Affiliation::Admin);
    assert_eq!(outcome.occupant_count, 2);
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("first join should succeed");

    let outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: alice,
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("existing session rejoin should bypass full-room rejection");

    assert_eq!(outcome.occupant_count, 1);
}

#[tokio::test]
async fn join_and_leave_emit_presence_and_occupant_metrics() {
    let guard = crate::telemetry::test_support::acquire().await;
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("join should succeed");

    assert_eq!(
        guard.counter_sum("xmpp.muc.presence", &[("event", "join")]),
        Some(1),
        "an accepted join must emit exactly one presence join event",
    );
    assert!(
        guard
            .metric_names()
            .contains(&"xmpp.muc.occupants".to_string()),
        "a brand-new occupant must publish the occupants gauge",
    );

    actor
        .ask(LeaveByRealJid {
            sender_jid: alice,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave is infallible");

    assert_eq!(
        guard.counter_sum("xmpp.muc.presence", &[("event", "leave")]),
        Some(1),
        "a processed leave must emit exactly one presence leave event",
    );
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
        .ask(LeaveByRealJid {
            sender_jid: test_full_jid("alice"),
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
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
        .ask(LeaveByRealJid {
            sender_jid: test_full_jid("ghost"),
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave reply");
    assert!(matches!(result, LeaveDisposition::NotOccupant));
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
        .ask(UpdateConfig {
            config: new_config,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
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
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::UnmanagedMembersOnlyPostEnforcement,
        })
        .await
        .expect("config update");

    let applied = actor.ask(EnforceMembersOnly).await.expect("enforce");
    // A status-322 ejection ends room membership, so it must also end
    // the ejected occupant's SFU call participation.
    assert_eq!(
        applied
            .removed_by_moderation
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec![alice.to_string()],
        "the ejected occupant must be marked for SFU eviction"
    );
    let updates = applied.presence_updates;
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
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
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("config update");

    let result = actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Unaffiliated,
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
async fn channel_reconciliation_invalidates_pre_policy_change_repairs() {
    let actor = spawn_room_actor().await;
    let stale_revision = current_admission_revision(&actor).await;

    actor
        .ask(ReconcileChannelBackedRoom {
            room_jid: "testroom@muc.example.com".parse().expect("room JID"),
            waddle_id: crate::muc::durable::WaddleId::new("waddle-2".to_string()),
            channel_id: crate::muc::durable::ChannelId::new("channel-2".to_string()),
            desired_config: RoomConfig {
                members_only: true,
                ..RoomConfig::default()
            },
        })
        .await
        .expect("reconcile channel-backed room");

    let snapshot = actor.ask(GetSnapshot).await.expect("room snapshot");
    assert!(snapshot.room.config.members_only);
    assert_eq!(snapshot.config_revision, 1);
    assert_eq!(snapshot.admission_revision, stale_revision + 1);
    assert_eq!(
        actor
            .ask(SyncResolverAffiliation {
                jid: "alice@example.com".parse().expect("bare JID"),
                affiliation: Affiliation::None,
                expected_admission_revision: stale_revision,
            })
            .await
            .expect("pre-reconcile resolver repair"),
        ResolverAffiliationSyncOutcome::StaleAdmissionRevision,
        "room-wide policy changes must reject repairs from older snapshots"
    );
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("first session join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_phone.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("first nick join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_phone.clone(),
            nick: "alice-phone".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
async fn unmanaged_members_only_post_enforcement_snapshot_excludes_removed_config_recipient() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");
    let bob = test_full_jid("bob");
    for (nick, real_jid, affiliation) in [
        ("alice", alice.clone(), Affiliation::Owner),
        ("bob", bob.clone(), Affiliation::None),
    ] {
        actor
            .ask(Join {
                nick: nick.to_owned(),
                real_jid,
                role: Role::Participant,
                affiliation,
            })
            .await
            .expect("join open room");
    }

    let mut config = actor.ask(GetConfig).await.expect("current config");
    config.members_only = true;
    actor
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::UnmanagedMembersOnlyPostEnforcement,
        })
        .await
        .expect("owner config update");

    let applied = actor.ask(EnforceMembersOnly).await.expect("enforce");
    assert!(
        applied
            .presence_updates
            .iter()
            .any(|(recipient, presence)| recipient == &bob && presence_has_status(presence, "322")),
        "the removed occupant must receive 322"
    );
    assert!(
        applied.presence_updates.iter().any(
            |(recipient, presence)| recipient == &alice && presence_has_status(presence, "322")
        ),
        "the remaining occupant must observe the 322 broadcast before config fan-out"
    );

    // The store-less owner-config handler deliberately builds its config
    // audience from this post-enforcement room snapshot.
    let snapshot = actor
        .ask(GetSnapshot)
        .await
        .expect("post-enforcement snapshot");
    let config_recipients: Vec<FullJid> = snapshot
        .room
        .occupants
        .values()
        .flat_map(|occupant| snapshot.room.get_occupant_sessions(&occupant.nick))
        .collect();
    assert_eq!(
        config_recipients,
        vec![alice],
        "the removed occupant must be excluded from the post-enforcement config audience"
    );
}

#[tokio::test]
async fn managed_members_only_enforcement_uses_explicit_affiliation_snapshot() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("open-room inherited member join");

    let config = RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    };
    let update = actor
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::ManagedMembersOnlyFallback,
        })
        .await
        .expect("members-only config");
    let notification = update.notification.expect("config notification");

    let updates = actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: vec![(alice.to_bare(), Affiliation::None)],
            fallback_reservation: update.reservation,
            config_status_codes: notification.status_codes,
        })
        .await
        .expect("managed enforcement succeeds")
        .presence_updates;

    assert!(updates.iter().any(|(recipient, presence)| {
        recipient == &alice && presence_has_status(presence, "322")
    }));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
}

#[tokio::test]
async fn managed_members_only_enforcement_persists_survivor_voice_changes() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_config_and_store(
        RoomConfig {
            members_only: false,
            moderated: true,
            ..RoomConfig::default()
        },
        store.clone(),
    )
    .await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Visitor,
            affiliation: Affiliation::None,
        })
        .await
        .expect("open moderated visitor joins");

    let mut config = actor.ask(GetConfig).await.expect("config");
    config.members_only = true;
    let update = actor
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::ManagedMembersOnlyFallback,
        })
        .await
        .expect("members-only config");
    let notification = update.notification.expect("config notification");

    let applied = actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: vec![(alice.to_bare(), Affiliation::Member)],
            fallback_reservation: update.reservation,
            config_status_codes: notification.status_codes.clone(),
        })
        .await
        .expect("managed enforcement succeeds");
    assert_eq!(
        applied.voice_changes,
        vec![(alice.clone(), Voice::Voiced)],
        "the promoted survivor regains voice immediately"
    );

    let saved_effects = store.saved_effects();
    let fused = &saved_effects[1];
    match &fused.effects()[1] {
        crate::muc::RoomEffect::AdminRemainingBroadcast {
            presence_updates,
            removed_sessions,
            voice_changes,
        } => {
            assert!(presence_updates.is_empty());
            assert!(removed_sessions.is_empty());
            assert_eq!(
                voice_changes,
                &vec![crate::muc::OccupantVoiceChange {
                    session: alice,
                    voice: Voice::Voiced,
                }]
            );
        }
        other => panic!("expected remaining-broadcast effect, got {other:?}"),
    }
}

#[tokio::test]
async fn managed_members_only_enforcement_treats_missing_snapshot_entry_as_none() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("stale-open join");

    let config = RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    };
    let update = actor
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::ManagedMembersOnlyFallback,
        })
        .await
        .expect("members-only config");
    let notification = update.notification.expect("config notification");

    let updates = actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: Vec::new(),
            fallback_reservation: update.reservation,
            config_status_codes: notification.status_codes,
        })
        .await
        .expect("managed enforcement succeeds")
        .presence_updates;

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
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: alice,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .map(departed)
        .expect("leave");
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
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: alice,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .map(departed)
        .expect("leave");
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
    assert!(
        actor
            .ask(crate::muc::room_actor::IsDormant)
            .await
            .unwrap()
            .dormant
    );
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
    assert!(
        !actor
            .ask(crate::muc::room_actor::IsDormant)
            .await
            .unwrap()
            .dormant
    );
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
        !actor
            .ask(crate::muc::room_actor::IsDormant)
            .await
            .unwrap()
            .dormant,
        "in-memory affiliation grants must keep the room non-dormant \
         so eviction does not drop them"
    );
}

/// #1110: a resolver-derived member affiliation (the one every managed
/// join writes via the authz resolver) is reconstructible on the next
/// join by construction, so it must NOT pin the room actor in memory
/// forever. After the last such member leaves, the room is dormant and
/// the dormancy janitor may reap it.
#[tokio::test]
async fn is_dormant_true_after_resolver_derived_member_leaves() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("resolver-derived member join");
    actor
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: alice,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .map(departed)
        .expect("leave");
    assert!(
        actor
            .ask(crate::muc::room_actor::IsDormant)
            .await
            .unwrap()
            .dormant,
        "a resolver-derived member affiliation is re-derived on the next \
         join, so it must not block dormancy after the last leave — \
         otherwise every managed room lives forever (#1110)"
    );
}

/// #1134 defense-in-depth: XEP-0045 §10.1.1 — only the room creator
/// gets Owner. Even if two racing first-joins both arrive claiming
/// `CreatorOwner` (both call sites believed they created the room),
/// the room actor grants Owner only while no owner exists yet; the
/// loser joins with no affiliation of their own.
#[tokio::test]
async fn creator_owner_grant_applies_only_while_room_has_no_owner() {
    // Instant rooms (the only rooms whose join carries CreatorOwner)
    // are open and non-persistent — see CreateInstantRoom.
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: false,
        persistent: false,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");
    let bob = test_full_jid("bob");

    let alice_outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: alice,
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::CreatorOwner,
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("creator join");
    assert_eq!(
        alice_outcome.new_occupant_affiliation,
        Affiliation::Owner,
        "the first creator-join gets Owner (XEP-0045 §10.1.1)"
    );

    let bob_outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: bob,
            nick: "bob".to_string(),
            affiliation_grant: JoinAffiliationGrant::CreatorOwner,
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("racing second creator-join");
    assert_ne!(
        bob_outcome.new_occupant_affiliation,
        Affiliation::Owner,
        "a second racing 'creator' join must NOT also get Owner \
         (XEP-0045 §10.1.1: only the creator; #1134)"
    );
}

/// #1107 part 1: a FULL JID that already occupies the room under nick
/// A must not be admitted under nick B — that created a second
/// occupancy whose leave-cleanup only removed one, leaving a permanent
/// ghost. Waddle locks nicknames to identity, so per XEP-0045 §7.6 the
/// request is denied with `<not-acceptable/>` (type='cancel') rather
/// than performing a §7.6 nick change.
#[tokio::test]
async fn same_full_jid_joining_under_second_nick_is_rejected() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("first join");

    let result = actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice-again".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await;
    assert!(
        matches!(
            &result,
            Err(SendError::HandlerError(
                RoomActorError::OccupantAlreadyJoinedUnderDifferentNick { current_nick, .. }
            )) if current_nick == "alice"
        ),
        "the same full JID under a second nick must be refused, not \
         admitted as a ghost occupant (#1107); got: {result:?}"
    );
    assert_eq!(
        actor.ask(OccupantCount).await.expect("count"),
        1,
        "no second occupancy was created"
    );
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(snapshot.find_nick_by_real_jid(&alice), Some("alice"));
}

/// #1107 part 2: disconnect cleanup must remove EVERY occupancy held
/// by the full JID, not just the first-found nick. The two-nick state
/// is constructed via the unguarded legacy `Join` message (mirroring
/// pre-fix ghosts); after `LeaveByRealJid` no occupancy of the JID may
/// survive, and the remaining-occupant fan-out set is exactly the
/// other occupants.
#[tokio::test]
async fn leave_by_real_jid_removes_every_occupancy_of_the_full_jid() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    let bob = test_full_jid("bob");

    for nick in ["alice", "alice-ghost"] {
        actor
            .ask(Join {
                nick: nick.to_string(),
                real_jid: alice.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("legacy join");
    }
    actor
        .ask(Join {
            nick: "bob".to_string(),
            real_jid: bob.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("bob join");
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 3);

    let outcome = actor
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: alice.clone(),
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .map(departed)
        .expect("leave");

    assert_eq!(
        outcome.occupant_count, 1,
        "every occupancy of the leaving full JID is removed — \
         only bob remains (#1107)"
    );
    assert!(outcome.removed_last_session);
    assert_eq!(
        outcome.remaining_occupants,
        vec![bob.clone()],
        "the leave fan-out set contains only the other occupants — \
         never a dead session of the leaver"
    );
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert!(
        snapshot.find_nick_by_real_jid(&alice).is_none(),
        "no ghost occupancy survives for the disconnected full JID"
    );
    assert_eq!(snapshot.find_nick_by_real_jid(&bob), Some("bob"));

    // Rejoin + disconnect converges: counts stay correct.
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("rejoin");
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 2);
    actor
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: alice,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .map(departed)
        .expect("second leave");
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 1);
}

/// #1110 counterpart: an explicit grant (here a XEP-0045 §9.1 ban)
/// is in-memory only, so it MUST keep blocking dormancy after every
/// occupant leaves — otherwise the ban would evaporate on eviction.
#[tokio::test]
async fn is_dormant_false_when_explicit_ban_outlives_occupancy() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("join");
    actor
        .ask(ChangeAffiliation {
            jid: "banned@example.com".parse().expect("bare jid"),
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("ban");
    actor
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: alice,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .map(departed)
        .expect("leave");
    assert!(
        !actor
            .ask(crate::muc::room_actor::IsDormant)
            .await
            .unwrap()
            .dormant,
        "an explicit ban is memory-only; evicting the room would let \
         the banned user back in, so the room must stay non-dormant"
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
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: alice,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .map(departed)
        .expect("leave");
    assert!(
        actor
            .ask(crate::muc::room_actor::IsDormant)
            .await
            .unwrap()
            .dormant,
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
            fallback_reservation: None,
            config_status_codes: Vec::new(),
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
            fallback_reservation: None,
            config_status_codes: Vec::new(),
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: alice,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave");
    let join_outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: carol,
            nick: "carol".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("desktop leave");
    let leave_outcome = departed(leave_outcome);
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: mobile,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("mobile leave");

    // A late joiner sees alice still on the call — desktop is the
    // originator and is still in the room.
    let carol = test_full_jid("carol");
    let join_outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: carol,
            nick: "carol".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
        .ask(crate::muc::room_actor::LeaveByRealJid {
            sender_jid: mobile,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .map(departed)
        .expect("mobile leave");

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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("join alice desktop");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid_resource("alice", "mobile"),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("join alice desktop");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: test_full_jid_resource("alice", "mobile"),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
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

// ---------------------------------------------------------------------------
// Ownership (ADR-0017 Phase 3 Slice 3: steal-intent owner-veto path)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_check_replies_when_the_room_actor_is_idle() {
    let actor = spawn_room_actor().await;
    actor.ask(HealthCheck).await.expect("health check replies");
}

// ---------------------------------------------------------------------------
// ADR-0017 Phase 3 Slice 7: the durable commit is the ownership authority for
// mutations that carry a durable delta. Zero-delta paths retain a direct probe.
// ---------------------------------------------------------------------------

/// A [`crate::muc::durable::MucDurableStore`] test double whose
/// `check_exact_claim_fence` and durable-commit result are controlled by the
/// test, so ownership handling can be exercised without a real Postgres
/// backend. `save_*` calls always succeed (or, when `fail_persist` is set,
/// always fail, or when `lose_config_persist_ownership` is set, return exact
/// ownership loss). `commit_outcome_unknown` simulates a lost COMMIT
/// acknowledgement after durable state may have advanced. The concrete
/// Postgres fencing SQL itself is covered by `waddle-server::muc_durable`'s
/// own Postgres-gated test suite.
#[derive(Default)]
struct FakeDurableStore {
    /// Ownership result for both direct zero-delta probes and durable commits:
    /// `Some(true)` = owned, `Some(false)` = deposed, `None` = transient
    /// backend error (fails closed).
    fenced: std::sync::Mutex<Option<bool>>,
    fail_persist: bool,
    commit_outcome_unknown: bool,
    commit_config_outcome_unknown: bool,
    lose_config_persist_ownership: bool,
    lose_projection_persist_ownership: bool,
    lose_restore_ownership: bool,
    load_calls: std::sync::atomic::AtomicUsize,
    save_calls: std::sync::atomic::AtomicUsize,
    saved_affiliations: std::sync::Mutex<Vec<(BareJid, BareJid, Affiliation)>>,
    saved_effects: std::sync::Mutex<Vec<crate::muc::RoomMutationEffects>>,
    established_fences:
        std::sync::Mutex<std::collections::HashMap<BareJid, crate::muc::RoomClaimFenceContext>>,
    lifecycle: std::sync::OnceLock<crate::muc::RoomLifecycleId>,
    next_revision: std::sync::atomic::AtomicUsize,
    replay_last_coordinates: std::sync::atomic::AtomicBool,
    last_coordinates: std::sync::Mutex<Option<crate::muc::RoomCommittedCoordinates>>,
    recorded_intents: std::sync::Mutex<Vec<crate::muc::RoomDurableMutation>>,
    recorded_projection_revisions: std::sync::Mutex<Vec<crate::muc::RoomRevision>>,
    commit_events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    projection_pause: std::sync::Mutex<Option<ProjectionCommitPause>>,
    restored_state: std::sync::Mutex<Option<crate::muc::durable::DurableRoomState>>,
}

#[derive(Clone)]
struct ProjectionCommitPause {
    entered: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::Arc<tokio::sync::Notify>,
}

impl ProjectionCommitPause {
    fn new() -> Self {
        Self {
            entered: std::sync::Arc::new(tokio::sync::Notify::new()),
            release: std::sync::Arc::new(tokio::sync::Notify::new()),
        }
    }
}

impl FakeDurableStore {
    fn with_established_test_fence(store: std::sync::Arc<Self>) -> std::sync::Arc<Self> {
        let room_jid = test_room().room_jid;
        <Self as crate::muc::durable::MucDurableStore>::establish_claim_fence(
            &*store,
            &room_jid,
            test_claim_fence(&room_jid),
        );
        store
    }

    fn owned() -> std::sync::Arc<Self> {
        Self::with_established_test_fence(std::sync::Arc::new(Self {
            fenced: std::sync::Mutex::new(Some(true)),
            ..Default::default()
        }))
    }

    fn deposed() -> std::sync::Arc<Self> {
        Self::with_established_test_fence(std::sync::Arc::new(Self {
            fenced: std::sync::Mutex::new(Some(false)),
            ..Default::default()
        }))
    }

    fn deposed_on_projection() -> std::sync::Arc<Self> {
        Self::with_established_test_fence(std::sync::Arc::new(Self {
            fenced: std::sync::Mutex::new(Some(true)),
            lose_projection_persist_ownership: true,
            ..Default::default()
        }))
    }

    fn transient_failure() -> std::sync::Arc<Self> {
        Self::with_established_test_fence(std::sync::Arc::new(Self {
            fenced: std::sync::Mutex::new(None),
            ..Default::default()
        }))
    }

    fn owned_but_persist_fails() -> std::sync::Arc<Self> {
        Self::with_established_test_fence(std::sync::Arc::new(Self {
            fenced: std::sync::Mutex::new(Some(true)),
            fail_persist: true,
            lose_config_persist_ownership: false,
            save_calls: std::sync::atomic::AtomicUsize::new(0),
            saved_affiliations: std::sync::Mutex::new(Vec::new()),
            saved_effects: std::sync::Mutex::new(Vec::new()),
            ..Default::default()
        }))
    }

    fn owned_but_commit_outcome_is_unknown() -> std::sync::Arc<Self> {
        Self::with_established_test_fence(std::sync::Arc::new(Self {
            fenced: std::sync::Mutex::new(Some(true)),
            commit_outcome_unknown: true,
            ..Default::default()
        }))
    }

    fn owned_but_config_commit_outcome_is_unknown() -> std::sync::Arc<Self> {
        Self::with_established_test_fence(std::sync::Arc::new(Self {
            fenced: std::sync::Mutex::new(Some(true)),
            commit_config_outcome_unknown: true,
            ..Default::default()
        }))
    }

    fn owned_but_config_persist_loses_ownership() -> std::sync::Arc<Self> {
        Self::with_established_test_fence(std::sync::Arc::new(Self {
            fenced: std::sync::Mutex::new(Some(true)),
            lose_config_persist_ownership: true,
            ..Default::default()
        }))
    }

    fn ownership_lost_during_restore() -> std::sync::Arc<Self> {
        Self::with_established_test_fence(std::sync::Arc::new(Self {
            fenced: std::sync::Mutex::new(Some(true)),
            lose_restore_ownership: true,
            ..Default::default()
        }))
    }

    fn save_call_count(&self) -> usize {
        self.save_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn saved_affiliations(&self) -> Vec<(BareJid, BareJid, Affiliation)> {
        self.saved_affiliations.lock().expect("lock").clone()
    }

    fn saved_effects(&self) -> Vec<crate::muc::RoomMutationEffects> {
        self.saved_effects.lock().expect("lock").clone()
    }

    fn set_fenced(&self, fenced: Option<bool>) {
        *self.fenced.lock().expect("lock") = fenced;
    }

    fn set_replay_last_coordinates(&self, replay: bool) {
        self.replay_last_coordinates
            .store(replay, std::sync::atomic::Ordering::SeqCst);
    }

    fn recorded_intents(&self) -> Vec<crate::muc::RoomDurableMutation> {
        self.recorded_intents.lock().expect("lock").clone()
    }

    fn recorded_projection_revisions(&self) -> Vec<crate::muc::RoomRevision> {
        self.recorded_projection_revisions
            .lock()
            .expect("lock")
            .clone()
    }

    fn commit_events(&self) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
        self.commit_events.clone()
    }

    fn pause_next_projection_commit(&self) -> ProjectionCommitPause {
        let pause = ProjectionCommitPause::new();
        *self.projection_pause.lock().expect("lock") = Some(pause.clone());
        pause
    }

    fn set_restored_coordinates(&self, coordinates: crate::muc::RoomCommittedCoordinates) {
        *self.restored_state.lock().expect("lock") = Some(crate::muc::durable::DurableRoomState {
            coordinates: Some(coordinates),
            config_coordinates: None,
            waddle_id: "waddle-1".to_owned(),
            channel_id: "channel-1".to_owned(),
            config: RoomConfig::default(),
            subject: None,
            affiliations: Vec::new(),
        });
    }

    fn next_commit_coordinates(&self) -> crate::muc::RoomCommittedCoordinates {
        let lifecycle = *self
            .lifecycle
            .get_or_init(crate::muc::RoomLifecycleId::generate);
        let revision = self
            .next_revision
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        crate::muc::RoomCommittedCoordinates {
            lifecycle,
            revision: crate::muc::RoomRevision::from_stored(revision as i64)
                .expect("positive revision"),
        }
    }
}

impl crate::muc::durable::MucDurableStore for FakeDurableStore {
    fn commit_room_mutation<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a crate::muc::RoomClaimFenceContext,
        intent: crate::muc::RoomDurableMutation,
        effects: crate::muc::RoomMutationEffects,
    ) -> crate::muc::RoomCommitFuture<'a> {
        if let Err(error) = validate_test_claim_fence(room_jid, fence) {
            return Box::pin(async move {
                match error {
                    crate::XmppError::OwnershipLost { .. } => {
                        Err(crate::muc::RoomCommitError::NotOwner)
                    }
                    _ => Err(crate::muc::RoomCommitError::OwnershipUnavailable),
                }
            });
        }
        self.save_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.recorded_intents
            .lock()
            .expect("lock")
            .push(intent.clone());
        let established =
            self.established_fences.lock().expect("lock").get(room_jid) == Some(fence);
        let fenced = *self.fenced.lock().expect("lock");
        let lose_ownership = matches!(intent, crate::muc::RoomDurableMutation::Config { .. })
            && self.lose_config_persist_ownership;
        // Projection commits (#1647) carry no effects by construction and are
        // observed through `recorded_intents`; `saved_effects` keeps describing
        // state commits only so positional fixtures stay meaningful.
        let is_projection = matches!(intent, crate::muc::RoomDurableMutation::Projection(_));
        let projection_kind = match &intent {
            crate::muc::RoomDurableMutation::Projection(projection) => Some(projection.kind()),
            _ => None,
        };
        let projection_pause = is_projection
            .then(|| self.projection_pause.lock().expect("lock").take())
            .flatten();
        let lose_projection_ownership = is_projection && self.lose_projection_persist_ownership;
        let config_commit = matches!(&intent, crate::muc::RoomDurableMutation::Config { .. });
        let committed_affiliations = match intent {
            crate::muc::RoomDurableMutation::Affiliation(entry)
            | crate::muc::RoomDurableMutation::MediatedInviteGrant(entry)
            | crate::muc::RoomDurableMutation::MediatedInviteRollback(entry) => vec![entry],
            crate::muc::RoomDurableMutation::AffiliationBatch(entries)
            | crate::muc::RoomDurableMutation::MembersOnlyEnforcement {
                affiliations: entries,
                ..
            }
            | crate::muc::RoomDurableMutation::Create {
                initial_affiliations: entries,
                ..
            } => entries,
            _ => Vec::new(),
        };
        let saved_affiliations = &self.saved_affiliations;
        let saved_effects = &self.saved_effects;
        let commit_events = self.commit_events.clone();
        let committed_room = room_jid.clone();
        let fail = self.fail_persist;
        let commit_outcome_unknown =
            self.commit_outcome_unknown || (self.commit_config_outcome_unknown && config_commit);
        let coordinates = if self
            .replay_last_coordinates
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.last_coordinates
                .lock()
                .expect("lock")
                .unwrap_or_else(|| self.next_commit_coordinates())
        } else {
            self.next_commit_coordinates()
        };
        *self.last_coordinates.lock().expect("lock") = Some(coordinates);
        if is_projection {
            self.recorded_projection_revisions
                .lock()
                .expect("lock")
                .push(coordinates.revision);
        }
        let reservation =
            (!effects.effects().is_empty()).then(|| crate::muc::RoomEffectReservation {
                lifecycle: coordinates.lifecycle,
                revision: coordinates.revision,
                ordinals: (0..effects.effects().len())
                    .map(|ordinal| {
                        crate::muc::RoomEffectOrdinal::from_stored(ordinal as i64)
                            .expect("non-negative ordinal")
                    })
                    .collect(),
            });
        Box::pin(async move {
            if let Some(pause) = projection_pause {
                commit_events.lock().expect("lock").push(format!(
                    "commit_started:{}",
                    projection_kind.expect("kind").as_str()
                ));
                pause.entered.notify_one();
                pause.release.notified().await;
                commit_events
                    .lock()
                    .expect("lock")
                    .push("commit_finished".to_owned());
            }
            if !established {
                Err(crate::muc::RoomCommitError::OwnershipUnavailable)
            } else if fenced == Some(false) {
                Err(crate::muc::RoomCommitError::NotOwner)
            } else if fenced.is_none() {
                Err(crate::muc::RoomCommitError::OwnershipUnavailable)
            } else if lose_ownership || lose_projection_ownership {
                Err(crate::muc::RoomCommitError::NotOwner)
            } else if commit_outcome_unknown {
                Err(crate::muc::RoomCommitError::CommitOutcomeUnknown)
            } else if fail {
                Err(crate::muc::RoomCommitError::Database(
                    crate::muc::durable::RoomCommitDatabaseError::sanitized(),
                ))
            } else {
                saved_affiliations.lock().expect("lock").extend(
                    committed_affiliations.into_iter().map(|entry| {
                        (
                            committed_room.clone(),
                            entry.jid,
                            entry.affiliation.unwrap_or(crate::Affiliation::None),
                        )
                    }),
                );
                if !is_projection {
                    saved_effects.lock().expect("lock").push(effects);
                }
                Ok(crate::muc::RoomCommitOutcome {
                    coordinates,
                    reservation,
                })
            }
        })
    }

    fn load_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a crate::muc::RoomClaimFenceContext,
    ) -> crate::muc::durable::MucDurableFuture<'a, Option<crate::muc::durable::DurableRoomState>>
    {
        let validation = validate_test_claim_fence(room_jid, fence);
        self.load_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let lose_ownership = self.lose_restore_ownership;
        let entity = fence.entity.clone();
        let restored_state = self.restored_state.lock().expect("lock").clone();
        Box::pin(async move {
            validation?;
            if lose_ownership {
                Err(crate::XmppError::OwnershipLost { entity })
            } else {
                Ok(restored_state)
            }
        })
    }

    fn establish_claim_fence(&self, room_jid: &BareJid, fence: crate::muc::RoomClaimFenceContext) {
        self.established_fences
            .lock()
            .expect("lock")
            .insert(room_jid.clone(), fence);
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a crate::muc::RoomClaimFenceContext,
    ) -> crate::muc::durable::MucDurableFuture<'a, bool> {
        let exact_fence = validate_test_claim_fence(room_jid, fence).is_ok();
        let fenced = *self.fenced.lock().expect("lock");
        Box::pin(async move {
            if !exact_fence {
                return Ok(false);
            }
            match fenced {
                Some(owned) => Ok(owned),
                None => Err(crate::XmppError::internal(
                    "simulated transient fencing failure",
                )),
            }
        })
    }
}

struct FailNthAffiliationSaveStore {
    fail_on_call: usize,
    save_calls: std::sync::atomic::AtomicUsize,
    lifecycle: std::sync::OnceLock<crate::muc::RoomLifecycleId>,
    next_revision: std::sync::atomic::AtomicUsize,
    established_fences:
        std::sync::Mutex<std::collections::HashMap<BareJid, crate::muc::RoomClaimFenceContext>>,
}

impl FailNthAffiliationSaveStore {
    fn new(fail_on_call: usize) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            fail_on_call,
            save_calls: std::sync::atomic::AtomicUsize::new(0),
            lifecycle: std::sync::OnceLock::new(),
            next_revision: std::sync::atomic::AtomicUsize::new(0),
            established_fences: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn save_call_count(&self) -> usize {
        self.save_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn next_commit_coordinates(&self) -> crate::muc::RoomCommittedCoordinates {
        let lifecycle = *self
            .lifecycle
            .get_or_init(crate::muc::RoomLifecycleId::generate);
        let revision = self
            .next_revision
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        crate::muc::RoomCommittedCoordinates {
            lifecycle,
            revision: crate::muc::RoomRevision::from_stored(revision as i64)
                .expect("positive revision"),
        }
    }
}

impl crate::muc::durable::MucDurableStore for FailNthAffiliationSaveStore {
    fn commit_room_mutation<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a crate::muc::RoomClaimFenceContext,
        intent: crate::muc::RoomDurableMutation,
        _effects: crate::muc::RoomMutationEffects,
    ) -> crate::muc::RoomCommitFuture<'a> {
        if let Err(error) = validate_test_claim_fence(room_jid, fence) {
            return Box::pin(async move {
                match error {
                    crate::XmppError::OwnershipLost { .. } => {
                        Err(crate::muc::RoomCommitError::NotOwner)
                    }
                    _ => Err(crate::muc::RoomCommitError::OwnershipUnavailable),
                }
            });
        }
        let counts_as_affiliation_commit = matches!(
            intent,
            crate::muc::RoomDurableMutation::Affiliation(_)
                | crate::muc::RoomDurableMutation::AffiliationBatch(_)
                | crate::muc::RoomDurableMutation::MembersOnlyEnforcement { .. }
                | crate::muc::RoomDurableMutation::MediatedInviteGrant(_)
                | crate::muc::RoomDurableMutation::MediatedInviteRollback(_)
                | crate::muc::RoomDurableMutation::Create { .. }
        );
        let established =
            self.established_fences.lock().expect("lock").get(room_jid) == Some(fence);
        let fail = if counts_as_affiliation_commit {
            let call = self
                .save_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            call == self.fail_on_call
        } else {
            false
        };
        let coordinates = self.next_commit_coordinates();
        Box::pin(async move {
            if !established {
                Err(crate::muc::RoomCommitError::OwnershipUnavailable)
            } else if fail {
                Err(crate::muc::RoomCommitError::Database(
                    crate::muc::durable::RoomCommitDatabaseError::sanitized(),
                ))
            } else {
                Ok(crate::muc::RoomCommitOutcome {
                    coordinates,
                    reservation: None,
                })
            }
        })
    }

    fn load_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a crate::muc::RoomClaimFenceContext,
    ) -> crate::muc::durable::MucDurableFuture<'a, Option<crate::muc::durable::DurableRoomState>>
    {
        let validation = validate_test_claim_fence(room_jid, fence);
        Box::pin(async move {
            validation?;
            Ok(None)
        })
    }

    fn establish_claim_fence(&self, room_jid: &BareJid, fence: crate::muc::RoomClaimFenceContext) {
        self.established_fences
            .lock()
            .expect("lock")
            .insert(room_jid.clone(), fence);
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a crate::muc::RoomClaimFenceContext,
    ) -> crate::muc::durable::MucDurableFuture<'a, bool> {
        let exact_fence = validate_test_claim_fence(room_jid, fence).is_ok();
        Box::pin(async move { Ok(exact_fence) })
    }
}

async fn spawn_room_actor_with_store(
    store: std::sync::Arc<dyn crate::muc::durable::MucDurableStore>,
) -> ActorRef<RoomActor> {
    spawn_room_actor_with_config_and_store(RoomConfig::default(), store).await
}

async fn spawn_room_actor_with_config_and_store(
    mut config: RoomConfig,
    store: std::sync::Arc<dyn crate::muc::durable::MucDurableStore>,
) -> ActorRef<RoomActor> {
    config.name = "Test Room".to_string();
    let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
    let actor = RoomActor::spawn(RoomActor::new(
        MucRoom::new(
            room_jid,
            "waddle-1".to_string(),
            "channel-1".to_string(),
            config,
        ),
        test_secret(),
    ));
    let room_jid = test_room().room_jid;
    let claim_fence = test_claim_fence(&room_jid);
    store.establish_claim_fence(&room_jid, claim_fence.clone());
    actor
        .ask(RestoreDurableRoomState { store, claim_fence })
        .await
        .expect("restore");
    actor
}

fn test_pinned_entry(id: &str) -> PinnedEntry {
    use crate::muc::pin::PinPreview;

    let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
    PinnedEntry {
        target_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
            id.to_owned(),
            jid::Jid::from(room_jid),
        ),
        pinner_jid: "admin@example.com".parse().expect("valid jid"),
        pinned_at: chrono::Utc::now(),
        preview: PinPreview::new(
            "alice@example.com".parse().expect("valid jid"),
            None,
            "important",
            chrono::Utc::now(),
        ),
    }
}

struct GetProjectionTestState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectionTestState {
    occupancy_revision: u64,
    projected_revision: Option<crate::muc::RoomRevision>,
}

impl kameo::message::Message<GetProjectionTestState> for RoomActor {
    type Reply = Result<ProjectionTestState, std::convert::Infallible>;

    async fn handle(
        &mut self,
        _msg: GetProjectionTestState,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(ProjectionTestState {
            occupancy_revision: self.occupancy_revision,
            projected_revision: self.projected_revision,
        })
    }
}

struct SetRoomSealForTest(RoomSealState);

impl kameo::message::Message<SetRoomSealForTest> for RoomActor {
    type Reply = Result<(), std::convert::Infallible>;

    async fn handle(
        &mut self,
        msg: SetRoomSealForTest,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.seal_state = msg.0;
        Ok(())
    }
}

struct SetProjectionApplyHookForTest(std::sync::Arc<dyn Fn(super::ProjectionProbe) + Send + Sync>);

impl kameo::message::Message<SetProjectionApplyHookForTest> for RoomActor {
    type Reply = Result<(), std::convert::Infallible>;

    async fn handle(
        &mut self,
        msg: SetProjectionApplyHookForTest,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.test_projection_apply_hook = Some(msg.0);
        Ok(())
    }
}

async fn install_projection_apply_hook(
    actor: &ActorRef<RoomActor>,
    events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    let hook_events = events.clone();
    actor
        .ask(SetProjectionApplyHookForTest(std::sync::Arc::new(
            move |probe: super::ProjectionProbe| {
                hook_events.lock().expect("lock").push(format!(
                    "{}:occupants={}:sessions={}:pins={}",
                    probe.phase, probe.occupants, probe.sessions, probe.pins
                ));
            },
        )))
        .await
        .expect("install apply hook");
}

fn projection_event_snapshot(
    events: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> Vec<String> {
    events.lock().expect("lock").clone()
}

async fn join_as_resolver(
    actor: &ActorRef<RoomActor>,
    jid: FullJid,
    nick: &str,
) -> Result<JoinOutcome, SendError<JoinWithAffiliation, RoomActorError>> {
    actor
        .ask(JoinWithAffiliation {
            sender_jid: jid,
            nick: nick.to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_owned(),
            admission_revision: current_admission_revision(actor).await,
        })
        .await
}

async fn leave_with_attempt(
    actor: &ActorRef<RoomActor>,
    jid: FullJid,
    attempt: LeaveAttemptId,
) -> LeaveDisposition {
    actor
        .ask(LeaveByRealJid {
            sender_jid: jid,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt,
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave ask")
}

#[tokio::test]
async fn replayed_receipt_is_superseded_when_the_session_rejoined() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let first_attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), first_attempt).await,
        LeaveDisposition::Left(_)
    ));
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice rejoins");

    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), first_attempt).await,
        LeaveDisposition::Superseded
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("occupant count"), 1);

    assert!(matches!(
        leave_with_attempt(&actor, alice, LeaveAttemptId::generate()).await,
        LeaveDisposition::Left(_)
    ));
}

#[tokio::test]
async fn old_receipt_is_not_replayed_after_the_jid_rejoined_and_left_again() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let first_attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), first_attempt).await,
        LeaveDisposition::Left(_)
    ));
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice rejoins");
    let second_attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), second_attempt).await,
        LeaveDisposition::Left(_)
    ));

    let snapshot = actor
        .ask(GetSnapshot)
        .await
        .expect("snapshot after second leave");
    assert_eq!(snapshot.departures.receipts.len(), 1);
    assert_eq!(snapshot.departures.receipts[0].attempt, second_attempt);

    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), first_attempt).await,
        LeaveDisposition::NotOccupant | LeaveDisposition::Superseded
    ));
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), second_attempt).await,
        LeaveDisposition::Left(_)
    ));
    assert!(matches!(
        leave_with_attempt(&actor, alice, second_attempt).await,
        LeaveDisposition::NotOccupant
    ));
    assert!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot after replay")
            .departures
            .receipts
            .is_empty(),
        "the departed-generation tombstone prunes with B's consumed receipt"
    );
}

async fn leave_with_attempt_and_cause(
    actor: &ActorRef<RoomActor>,
    jid: FullJid,
    attempt: LeaveAttemptId,
    cause: crate::muc::durable::OccupancyLeaveCause,
) -> LeaveDisposition {
    actor
        .ask(LeaveByRealJid {
            sender_jid: jid,
            cause,
            session: LeaveSessionSelector::Any,
            attempt,
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave ask")
}

/// A janitor-style retained retry: the only origin allowed to consume the
/// full JID's unacknowledged receipt when its own attempt is unknown.
async fn retry_with_attempt_and_cause(
    actor: &ActorRef<RoomActor>,
    jid: FullJid,
    attempt: LeaveAttemptId,
    cause: crate::muc::durable::OccupancyLeaveCause,
) -> LeaveDisposition {
    actor
        .ask(LeaveByRealJid {
            sender_jid: jid,
            cause,
            session: LeaveSessionSelector::Any,
            attempt,
            origin: LeaveOrigin::RetainedRetry,
        })
        .await
        .expect("retry ask")
}

fn receipts_of(actor_snapshot: &RoomSnapshot) -> &[DepartureReceipt] {
    &actor_snapshot.departures.receipts
}

#[tokio::test]
async fn retry_of_an_attempt_older_than_the_live_session_is_superseded() {
    // join → leave A1 (reply lost) → rejoin → leave A2 (acked) → rejoin →
    // retry A1: the live session joined after A1 was minted, so A1 must not
    // evict it even though A1's receipt tombstone was pruned with A2's ack.
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let first_attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), first_attempt).await,
        LeaveDisposition::Left(_)
    ));
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice rejoins");
    let second_attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), second_attempt).await,
        LeaveDisposition::Left(_)
    ));
    actor
        .ask(AckDepartureReceipt {
            attempt: second_attempt,
        })
        .await
        .expect("ack second departure");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(
        receipts_of(&snapshot).is_empty(),
        "the acknowledged receipt pruned every tombstone of the JID"
    );
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice rejoins again");

    let stale_retry = leave_with_attempt(&actor, alice.clone(), first_attempt).await;
    assert!(
        matches!(stale_retry, LeaveDisposition::Superseded),
        "stale retry must be superseded, got {stale_retry:?}"
    );
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(
        snapshot.room.get_occupant("alice").is_some(),
        "the live session joined after the stale attempt and must remain"
    );
}

#[tokio::test]
async fn old_receipt_is_not_replayed_after_the_rejoined_session_was_kicked() {
    let actor = spawn_room_actor().await;
    let owner = test_full_jid("owner");
    let alice = test_full_jid("alice");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: owner.clone(),
            nick: "owner".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Owner),
            local_domain: "example.com".to_owned(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("owner joins");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let lost_attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), lost_attempt).await,
        LeaveDisposition::Left(_)
    ));
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice rejoins");
    actor
        .ask(ApplyAdminItems {
            sender_jid: owner,
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
        .expect("kick alice");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(snapshot.room.get_occupant("alice").is_none(), "kicked");
    assert!(
        snapshot
            .departures
            .latest_generations
            .iter()
            .any(|(jid, generation)| {
                jid == &alice && *generation > snapshot.departures.receipts[0].generation
            }),
        "the rejoin advanced the JID's latest generation: {:?}",
        snapshot.departures
    );

    let stale_retry = leave_with_attempt(&actor, alice.clone(), lost_attempt).await;
    assert!(
        matches!(stale_retry, LeaveDisposition::Superseded),
        "a newer generation existed (and was removed by moderation): the stale \
         ordinary departure must not be announced after the 307, got {stale_retry:?}"
    );
    assert!(
        receipts_of(&actor.ask(GetSnapshot).await.expect("snapshot")).is_empty(),
        "the stale receipt is consumed, not left for a later fallback"
    );
}

#[tokio::test]
async fn jid_fallback_only_replays_a_receipt_of_the_same_cause() {
    use crate::muc::durable::OccupancyLeaveCause;
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let explicit_attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt_and_cause(
            &actor,
            alice.clone(),
            explicit_attempt,
            OccupancyLeaveCause::Explicit
        )
        .await,
        LeaveDisposition::Left(_)
    ));

    // An administrative leave for the now-absent JID must not consume the
    // explicit receipt: its caller would run the wrong effect policy and the
    // explicit retry would find nothing.
    assert!(matches!(
        retry_with_attempt_and_cause(
            &actor,
            alice.clone(),
            LeaveAttemptId::generate(),
            OccupancyLeaveCause::Administrative
        )
        .await,
        LeaveDisposition::NotOccupant
    ));
    assert_eq!(
        receipts_of(&actor.ask(GetSnapshot).await.expect("snapshot")).len(),
        1,
        "the explicit receipt stays retained for its own retry"
    );
    // A FRESH explicit leave of the gone JID never consumes it either: the
    // receipt may belong to a departure whose acknowledgement is in flight.
    assert!(matches!(
        leave_with_attempt_and_cause(
            &actor,
            alice.clone(),
            LeaveAttemptId::generate(),
            OccupancyLeaveCause::Explicit
        )
        .await,
        LeaveDisposition::NotOccupant
    ));
    assert_eq!(
        receipts_of(&actor.ask(GetSnapshot).await.expect("snapshot")).len(),
        1
    );
    // A coalesced explicit RETRY (unknown attempt, same cause) replays it.
    assert!(matches!(
        retry_with_attempt_and_cause(
            &actor,
            alice.clone(),
            LeaveAttemptId::generate(),
            OccupancyLeaveCause::Explicit
        )
        .await,
        LeaveDisposition::Left(_)
    ));
    assert!(receipts_of(&actor.ask(GetSnapshot).await.expect("snapshot")).is_empty());
}

#[tokio::test]
async fn superseded_attempt_tombstones_survive_live_roster_transfer() {
    // A (reply lost) is superseded by B (reply lost). After the transfer a
    // retry of A must not consume B's receipt through the JID fallback.
    let predecessor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    join_as_resolver(&predecessor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let attempt_a = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&predecessor, alice.clone(), attempt_a).await,
        LeaveDisposition::Left(_)
    ));
    join_as_resolver(&predecessor, alice.clone(), "alice")
        .await
        .expect("alice rejoins");
    let attempt_b = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&predecessor, alice.clone(), attempt_b).await,
        LeaveDisposition::Left(_)
    ));
    let snapshot = predecessor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(snapshot.departures.receipts.len(), 1);
    assert_eq!(snapshot.departures.superseded_attempts.len(), 1);

    let successor = spawn_room_actor().await;
    successor
        .ask(RestoreLiveRoster {
            room: snapshot.room,
            occupancy_revision: snapshot.occupancy_revision,
            departures: snapshot.departures,
        })
        .await
        .expect("transfer");

    assert!(matches!(
        leave_with_attempt(&successor, alice.clone(), attempt_a).await,
        LeaveDisposition::Superseded
    ));
    assert_eq!(
        receipts_of(&successor.ask(GetSnapshot).await.expect("snapshot")).len(),
        1,
        "B's receipt is still owed to B's retry"
    );
    assert!(matches!(
        leave_with_attempt(&successor, alice, attempt_b).await,
        LeaveDisposition::Left(_)
    ));
}

#[tokio::test]
async fn receipt_replays_while_a_sibling_session_still_holds_the_nick() {
    // alice/desktop and alice/mobile share nick "alice". desktop's departure
    // (removed_last_session == false) completes with a lost reply; mobile
    // still holding the nick is the normal multi-resource case and must not
    // make desktop's retained outcome look like a retaken nick.
    let actor = spawn_room_actor().await;
    let desktop = test_full_jid_resource("alice", "desktop");
    let mobile = test_full_jid_resource("alice", "mobile");
    join_as_resolver(&actor, desktop.clone(), "alice")
        .await
        .expect("desktop joins");
    join_as_resolver(&actor, mobile.clone(), "alice")
        .await
        .expect("mobile joins");
    let attempt = LeaveAttemptId::generate();
    let LeaveDisposition::Left(first) = leave_with_attempt(&actor, desktop.clone(), attempt).await
    else {
        panic!("desktop leaves");
    };
    assert!(!first.removed_last_session);

    let replay = leave_with_attempt(&actor, desktop.clone(), attempt).await;
    assert!(
        matches!(&replay, LeaveDisposition::Left(outcome) if !outcome.removed_last_session),
        "the exact-attempt retry replays the non-final departure, got {replay:?}"
    );
    assert!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .get_occupant("alice")
            .is_some(),
        "mobile keeps the nick"
    );
}

#[tokio::test]
async fn replayed_final_departure_is_superseded_when_the_same_account_retook_the_nick() {
    // alice/web was the only session on "alice"; its departure freed the nick
    // (reply lost). alice reconnects on a NEW resource and rejoins the nick:
    // the retained retry for /web must not announce alice's departure.
    let actor = spawn_room_actor().await;
    let web = test_full_jid_resource("alice", "web");
    let web2 = test_full_jid_resource("alice", "web2");
    join_as_resolver(&actor, web.clone(), "alice")
        .await
        .expect("web joins");
    let attempt = LeaveAttemptId::generate();
    let LeaveDisposition::Left(first) = leave_with_attempt(&actor, web.clone(), attempt).await
    else {
        panic!("web leaves");
    };
    assert!(first.removed_last_session);
    join_as_resolver(&actor, web2.clone(), "alice")
        .await
        .expect("web2 retakes the nick");

    let replay = retry_with_attempt_and_cause(
        &actor,
        web,
        attempt,
        crate::muc::durable::OccupancyLeaveCause::Disconnect,
    )
    .await;
    assert!(
        matches!(replay, LeaveDisposition::Superseded),
        "a freed nick now held by anyone supersedes the receipt, got {replay:?}"
    );
    assert!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .get_occupant("alice")
            .is_some(),
        "web2 keeps the nick"
    );
}

#[tokio::test]
async fn coalesced_retry_is_superseded_when_the_same_account_retook_the_freed_nick() {
    // Same as above through the janitor's real path after attempt coalescing:
    // the retry carries an attempt the actor never saw, so it reaches the
    // JID fallback — which must apply the same freed-nick rule.
    let actor = spawn_room_actor().await;
    let web = test_full_jid_resource("alice", "web");
    let web2 = test_full_jid_resource("alice", "web2");
    join_as_resolver(&actor, web.clone(), "alice")
        .await
        .expect("web joins");
    let LeaveDisposition::Left(first) =
        leave_with_attempt(&actor, web.clone(), LeaveAttemptId::generate()).await
    else {
        panic!("web leaves");
    };
    assert!(first.removed_last_session);
    join_as_resolver(&actor, web2.clone(), "alice")
        .await
        .expect("web2 retakes the nick");

    let replay = retry_with_attempt_and_cause(
        &actor,
        web,
        LeaveAttemptId::generate(),
        crate::muc::durable::OccupancyLeaveCause::Explicit,
    )
    .await;
    assert!(
        matches!(replay, LeaveDisposition::Superseded),
        "the JID fallback applies the freed-nick rule, got {replay:?}"
    );
    assert!(actor
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room
        .get_occupant("alice")
        .is_some());
}

#[tokio::test]
async fn non_final_receipt_is_superseded_once_the_sibling_left_and_the_nick_was_retaken() {
    // web and mobile share "alice"; web's NON-final departure loses its
    // reply. mobile then leaves, and alice rejoins on web2 (a new nick
    // generation). web's receipt captured the old generation's roster/Muji
    // state and must not be replayed over the new one.
    let actor = spawn_room_actor().await;
    let web = test_full_jid_resource("alice", "web");
    let mobile = test_full_jid_resource("alice", "mobile");
    let web2 = test_full_jid_resource("alice", "web2");
    join_as_resolver(&actor, web.clone(), "alice")
        .await
        .expect("web joins");
    join_as_resolver(&actor, mobile.clone(), "alice")
        .await
        .expect("mobile joins");
    let attempt = LeaveAttemptId::generate();
    let LeaveDisposition::Left(first) = leave_with_attempt(&actor, web.clone(), attempt).await
    else {
        panic!("web leaves");
    };
    assert!(!first.removed_last_session);
    assert!(matches!(
        leave_with_attempt(&actor, mobile, LeaveAttemptId::generate()).await,
        LeaveDisposition::Left(_)
    ));
    join_as_resolver(&actor, web2, "alice")
        .await
        .expect("web2 retakes the nick");

    let replay = leave_with_attempt(&actor, web, attempt).await;
    assert!(
        matches!(replay, LeaveDisposition::Superseded),
        "a new nick generation supersedes the non-final receipt, got {replay:?}"
    );
}

#[tokio::test]
async fn non_final_receipt_is_superseded_once_the_sibling_left_even_without_a_retake() {
    // web's non-final departure captured mobile's roster/Muji state; mobile
    // then leaves finally. With the nick absent, replaying web's receipt
    // would resurrect the nick's stale available state.
    let actor = spawn_room_actor().await;
    let web = test_full_jid_resource("alice", "web");
    let mobile = test_full_jid_resource("alice", "mobile");
    join_as_resolver(&actor, web.clone(), "alice")
        .await
        .expect("web joins");
    join_as_resolver(&actor, mobile.clone(), "alice")
        .await
        .expect("mobile joins");
    let attempt = LeaveAttemptId::generate();
    let LeaveDisposition::Left(first) = leave_with_attempt(&actor, web.clone(), attempt).await
    else {
        panic!("web leaves");
    };
    assert!(!first.removed_last_session);
    assert!(matches!(
        leave_with_attempt(&actor, mobile, LeaveAttemptId::generate()).await,
        LeaveDisposition::Left(_)
    ));

    let replay = leave_with_attempt(&actor, web, attempt).await;
    assert!(
        matches!(replay, LeaveDisposition::Superseded),
        "an absent nick supersedes a non-final receipt, got {replay:?}"
    );
}

#[tokio::test]
async fn acknowledgement_is_refused_by_an_actor_that_lost_ownership() {
    // After an ambiguous commit the actor is sealed OwnershipLost and its
    // ledger may already be on its way to a successor: it must not claim to
    // have dropped the receipt.
    let actor = spawn_room_actor_with_store(FakeDurableStore::owned()).await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), attempt).await,
        LeaveDisposition::Left(_)
    ));
    actor
        .ask(SetRoomSealForTest(RoomSealState::OwnershipLost))
        .await
        .expect("test seal");
    assert_eq!(
        actor
            .ask(AckDepartureReceipt { attempt })
            .await
            .expect("ack ask"),
        AckDepartureOutcome::NotAuthoritative
    );
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .departures
            .receipts
            .len(),
        1,
        "the receipt stays in the ledger for the successor"
    );
    actor
        .ask(SetRoomSealForTest(RoomSealState::Open))
        .await
        .expect("test seal");
    assert_eq!(
        actor
            .ask(AckDepartureReceipt { attempt })
            .await
            .expect("ack ask"),
        AckDepartureOutcome::Acknowledged
    );
}

#[tokio::test]
async fn replayed_receipt_is_superseded_when_the_nick_was_retaken() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    let bob = test_full_jid("bob");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), attempt).await,
        LeaveDisposition::Left(_)
    ));
    join_as_resolver(&actor, bob.clone(), "alice")
        .await
        .expect("bob takes alice nick");

    assert!(matches!(
        leave_with_attempt(&actor, alice, attempt).await,
        LeaveDisposition::Superseded
    ));
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(
        snapshot
            .room
            .get_occupant("alice")
            .map(|occupant| &occupant.real_jid),
        Some(&bob),
        "the retry must not evict the current nick holder"
    );
}

#[tokio::test]
async fn unknown_attempt_for_a_gone_session_replays_the_jids_unacknowledged_receipt_once() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let original_attempt = LeaveAttemptId::generate();
    let original = leave_with_attempt(&actor, alice.clone(), original_attempt).await;
    assert!(matches!(original, LeaveDisposition::Left(_)));

    assert!(matches!(
        retry_with_attempt_and_cause(
            &actor,
            alice.clone(),
            LeaveAttemptId::generate(),
            crate::muc::durable::OccupancyLeaveCause::Explicit
        )
        .await,
        LeaveDisposition::Left(_)
    ));
    assert!(matches!(
        retry_with_attempt_and_cause(
            &actor,
            alice,
            LeaveAttemptId::generate(),
            crate::muc::durable::OccupancyLeaveCause::Explicit
        )
        .await,
        LeaveDisposition::NotOccupant
    ));
}

#[tokio::test]
async fn acknowledged_departure_leaves_no_receipt() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), attempt).await,
        LeaveDisposition::Left(_)
    ));
    actor
        .ask(AckDepartureReceipt { attempt })
        .await
        .expect("ack receipt");

    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), attempt).await,
        LeaveDisposition::NotOccupant
    ));
    assert!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .departures
            .receipts
            .is_empty(),
        "acknowledged departures do not remain replayable"
    );
}

#[tokio::test]
async fn receipts_are_transferred_on_live_roster_restore() {
    let predecessor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    join_as_resolver(&predecessor, alice.clone(), "alice")
        .await
        .expect("alice joins predecessor");
    let attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&predecessor, alice.clone(), attempt).await,
        LeaveDisposition::Left(_)
    ));
    let snapshot = predecessor
        .ask(GetSnapshot)
        .await
        .expect("predecessor snapshot");
    assert_eq!(snapshot.departures.receipts.len(), 1);

    let successor = spawn_room_actor().await;
    successor
        .ask(RestoreLiveRoster {
            room: snapshot.room,
            occupancy_revision: snapshot.occupancy_revision,
            departures: snapshot.departures,
        })
        .await
        .expect("restore successor live roster");
    assert!(matches!(
        leave_with_attempt(&successor, alice, attempt).await,
        LeaveDisposition::Left(_)
    ));
}

#[tokio::test]
async fn transferred_older_generation_receipt_is_refused() {
    let predecessor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    join_as_resolver(&predecessor, alice.clone(), "alice")
        .await
        .expect("alice joins predecessor");
    let older_attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&predecessor, alice.clone(), older_attempt).await,
        LeaveDisposition::Left(_)
    ));
    let mut older_snapshot = predecessor.ask(GetSnapshot).await.expect("older snapshot");
    older_snapshot.departures.receipts[0].generation = OccupancyOrder::from_raw(1);
    older_snapshot.departures.latest_generations[0].1 = OccupancyOrder::from_raw(1);

    let newer_source = spawn_room_actor().await;
    join_as_resolver(&newer_source, alice.clone(), "alice")
        .await
        .expect("alice joins newer source");
    let newer_attempt = LeaveAttemptId::generate();
    assert!(matches!(
        leave_with_attempt(&newer_source, alice.clone(), newer_attempt).await,
        LeaveDisposition::Left(_)
    ));
    let mut newer_snapshot = newer_source.ask(GetSnapshot).await.expect("newer snapshot");
    newer_snapshot.departures.receipts[0].generation = OccupancyOrder::from_raw(2);
    newer_snapshot.departures.latest_generations[0].1 = OccupancyOrder::from_raw(2);

    let successor = spawn_room_actor().await;
    successor
        .ask(RestoreLiveRoster {
            room: newer_snapshot.room,
            occupancy_revision: newer_snapshot.occupancy_revision,
            departures: newer_snapshot.departures,
        })
        .await
        .expect("restore newer receipt");
    successor
        .ask(RestoreLiveRoster {
            room: older_snapshot.room,
            occupancy_revision: older_snapshot.occupancy_revision,
            departures: older_snapshot.departures,
        })
        .await
        .expect("restore older receipt");

    let snapshot = successor
        .ask(GetSnapshot)
        .await
        .expect("successor snapshot");
    assert_eq!(snapshot.departures.receipts.len(), 1);
    assert_eq!(snapshot.departures.receipts[0].attempt, newer_attempt);
    assert_eq!(
        snapshot.departures.receipts[0].generation,
        OccupancyOrder::from_raw(2)
    );
    assert!(matches!(
        leave_with_attempt(&successor, alice.clone(), older_attempt).await,
        LeaveDisposition::NotOccupant | LeaveDisposition::Superseded
    ));
    assert!(matches!(
        leave_with_attempt(&successor, alice, newer_attempt).await,
        LeaveDisposition::Left(_)
    ));
}

#[tokio::test]
async fn store_less_suppressed_departure_mints_a_receipt_replayed_as_suppressed() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("alice joins");
    let attempt = LeaveAttemptId::generate();
    seal_for_destroy(&actor).await;
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), attempt).await,
        LeaveDisposition::Suppressed { ref nick, .. } if nick.as_str() == "alice"
    ));
    assert!(matches!(
        leave_with_attempt(&actor, alice.clone(), attempt).await,
        LeaveDisposition::Suppressed { ref nick, .. } if nick.as_str() == "alice"
    ));
    assert!(matches!(
        leave_with_attempt(&actor, alice, attempt).await,
        LeaveDisposition::NotOccupant
    ));
}

fn projection_leave_intent_for(intents: &[crate::muc::RoomDurableMutation], jid: &FullJid) -> bool {
    intents.iter().any(|intent| {
        matches!(
            intent,
            crate::muc::RoomDurableMutation::Projection(
                crate::muc::durable::RoomProjection::OccupancyLeave { occupant, .. }
            ) if occupant == jid
        )
    })
}

#[tokio::test]
async fn join_with_lost_claim_projects_nothing() {
    let store = FakeDurableStore::deposed_on_projection();
    let actor = spawn_room_actor_with_store(store).await;
    let before = actor.ask(GetProjectionTestState).await.expect("state");
    let alice = test_full_jid("alice");

    let result = join_as_resolver(&actor, alice.clone(), "alice").await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(snapshot.room.occupant_count(), 0);
    assert_eq!(
        actor
            .ask(GetProjectionTestState)
            .await
            .expect("state")
            .occupancy_revision,
        before.occupancy_revision
    );
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("seal"),
        RoomSealState::OwnershipLost
    );
    // Resolver cache writes intentionally precede the projection commit so a
    // revocation is visible to admission checks even while the store is down.
    assert_eq!(
        snapshot.room.get_affiliation(&alice.to_bare()),
        Affiliation::Member
    );
}

#[tokio::test]
async fn join_with_transient_store_failure_bounces_without_memory_change() {
    let actor = spawn_room_actor_with_store(FakeDurableStore::owned_but_persist_fails()).await;
    let before = actor.ask(GetProjectionTestState).await.expect("state");

    let result = join_as_resolver(&actor, test_full_jid("alice"), "alice").await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            RoomActorError::OwnershipUnavailable
        ))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
    assert_eq!(
        actor
            .ask(GetProjectionTestState)
            .await
            .expect("state")
            .occupancy_revision,
        before.occupancy_revision
    );
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("seal"),
        RoomSealState::Open
    );
}

#[tokio::test]
async fn leave_commits_projection_before_removing_session() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("join");
    let before = store.recorded_intents().len();

    let result = actor
        .ask(LeaveByRealJid {
            sender_jid: alice.clone(),
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave");

    assert!(matches!(result, LeaveDisposition::Left(_)));
    assert!(projection_leave_intent_for(
        &store.recorded_intents()[before..],
        &alice
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
}

#[tokio::test]
async fn leave_with_lost_claim_keeps_session_and_seals() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("join");
    store.set_fenced(Some(false));

    let result = actor
        .ask(LeaveByRealJid {
            sender_jid: alice,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 1);
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("seal"),
        RoomSealState::OwnershipLost
    );
}

#[tokio::test]
async fn leave_under_inactive_seal_is_deferred_then_unseal_inactive_retry_leaves() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("join");
    actor
        .ask(SetRoomSealForTest(RoomSealState::Inactive))
        .await
        .expect("test seal");

    let watermark = match actor
        .ask(LeaveByRealJid {
            sender_jid: alice.clone(),
            cause: crate::muc::durable::OccupancyLeaveCause::Disconnect,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("deferred leave")
    {
        LeaveDisposition::Deferred { watermark } => watermark,
        other => panic!("expected deferred leave, got {other:?}"),
    };
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 1);
    assert!(actor.ask(UnsealInactive).await.expect("unseal"));
    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: crate::muc::durable::OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::JoinedAtOrBefore(watermark),
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("retried leave"),
        LeaveDisposition::Left(_)
    ));
}

#[tokio::test]
async fn leave_under_ownership_lost_seal_is_room_sealed_without_memory_change() {
    let actor = spawn_room_actor_with_store(FakeDurableStore::owned()).await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("join");
    actor
        .ask(SetRoomSealForTest(RoomSealState::OwnershipLost))
        .await
        .expect("test seal");

    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 1);
}

#[tokio::test]
async fn leave_unknown_jid_is_not_occupant_without_commit() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;

    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: test_full_jid("unknown"),
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave"),
        LeaveDisposition::NotOccupant
    ));
    assert!(!store
        .recorded_intents()
        .iter()
        .any(|intent| matches!(intent, crate::muc::RoomDurableMutation::Projection(_))));
}

#[tokio::test]
async fn non_occupant_leave_under_inactive_and_ownership_lost_seals_is_not_occupant_without_commit()
{
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    actor
        .ask(SetRoomSealForTest(RoomSealState::Inactive))
        .await
        .expect("seal inactive");

    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: test_full_jid("unknown-inactive"),
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("non-occupant leave"),
        LeaveDisposition::NotOccupant
    ));
    assert!(store.recorded_intents().is_empty());

    actor
        .ask(SetRoomSealForTest(RoomSealState::OwnershipLost))
        .await
        .expect("seal ownership lost");
    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: test_full_jid("unknown-lost"),
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("non-occupant leave"),
        LeaveDisposition::NotOccupant
    ));
    assert!(store.recorded_intents().is_empty());
}

#[tokio::test]
async fn join_commit_pending_leaves_memory_untouched_until_authorized() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let events = store.commit_events();
    install_projection_apply_hook(&actor, events.clone()).await;
    let pause = store.pause_next_projection_commit();
    let join_actor = actor.clone();
    let join = tokio::spawn(async move {
        join_as_resolver(&join_actor, test_full_jid("pending-join"), "pending").await
    });
    pause.entered.notified().await;
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
    // If the join mutation were hoisted above `commit_projection`, this would
    // flip to `pre_commit:occupants=1:...` before the durable commit finished.
    assert_eq!(
        projection_event_snapshot(&events),
        vec![
            "pre_commit:occupants=0:sessions=0:pins=0".to_owned(),
            "commit_started:occupancy_join".to_owned(),
        ],
        "the in-memory join must not apply while the projection commit is paused"
    );
    pause.release.notify_one();
    join.await
        .expect("join task")
        .expect("join after authorization");
    assert_eq!(
        projection_event_snapshot(&events),
        vec![
            "pre_commit:occupants=0:sessions=0:pins=0".to_owned(),
            "commit_started:occupancy_join".to_owned(),
            "commit_finished".to_owned(),
            "apply:occupants=0:sessions=0:pins=0".to_owned(),
        ]
    );
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 1);
}

#[tokio::test]
async fn leave_commit_pending_keeps_session_until_authorized() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let alice = test_full_jid("pending-leave");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("join");
    let events = store.commit_events();
    events.lock().expect("lock").clear();
    install_projection_apply_hook(&actor, events.clone()).await;
    let pause = store.pause_next_projection_commit();
    let leave_actor = actor.clone();
    let leave = tokio::spawn(async move {
        leave_actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
    });
    pause.entered.notified().await;
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        projection_event_snapshot(&events),
        vec![
            "pre_commit:occupants=1:sessions=1:pins=0".to_owned(),
            "commit_started:occupancy_leave".to_owned(),
        ],
        "the in-memory leave must not apply while the projection commit is paused"
    );
    pause.release.notify_one();
    assert!(matches!(
        leave.await.expect("leave task").expect("leave"),
        LeaveDisposition::Left(_)
    ));
    assert_eq!(
        projection_event_snapshot(&events),
        vec![
            "pre_commit:occupants=1:sessions=1:pins=0".to_owned(),
            "commit_started:occupancy_leave".to_owned(),
            "commit_finished".to_owned(),
            "apply:occupants=1:sessions=1:pins=0".to_owned(),
        ]
    );
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
}

#[tokio::test]
async fn pin_commit_pending_keeps_pin_list_until_authorized() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let events = store.commit_events();
    install_projection_apply_hook(&actor, events.clone()).await;
    let pause = store.pause_next_projection_commit();
    let pin_actor = actor.clone();
    let pin = test_pinned_entry("pending-pin");
    let apply = tokio::spawn(async move {
        pin_actor
            .ask(ApplyPin {
                change: PinStateChange::Pin(pin),
            })
            .await
    });
    pause.entered.notified().await;
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        projection_event_snapshot(&events),
        vec![
            "pre_commit:occupants=0:sessions=0:pins=0".to_owned(),
            "commit_started:pin".to_owned(),
        ],
        "the in-memory pin apply must not run while the projection commit is paused"
    );
    pause.release.notify_one();
    apply
        .await
        .expect("pin task")
        .expect("pin after authorization");
    assert_eq!(
        projection_event_snapshot(&events),
        vec![
            "pre_commit:occupants=0:sessions=0:pins=0".to_owned(),
            "commit_started:pin".to_owned(),
            "commit_finished".to_owned(),
            "apply:occupants=0:sessions=0:pins=0".to_owned(),
        ]
    );
    assert_eq!(actor.ask(GetPinList).await.expect("pins").len(), 1);
}

#[tokio::test]
async fn leave_retry_with_same_attempt_replays_retained_outcome_once() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice-replay");
    let bob = test_full_jid("bob-replay");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("join alice");
    join_as_resolver(&actor, bob.clone(), "bob")
        .await
        .expect("join bob");
    let attempt = LeaveAttemptId::generate();

    let first = departed(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("first leave"),
    );
    let replayed = departed(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("replayed leave"),
    );

    assert_eq!(replayed.nick, first.nick);
    assert_eq!(replayed.remaining_occupants, first.remaining_occupants);
    assert_eq!(replayed.affiliation, first.affiliation);

    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("third leave"),
        LeaveDisposition::NotOccupant
    ));
    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("different attempt"),
        LeaveDisposition::NotOccupant
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 1);
}

#[test]
fn departure_receipts_keep_only_the_newest_generation_per_jid() {
    // Receipts are uncapped but a newer departure of the same full JID
    // supersedes every older receipt of that JID, so the retained set is
    // bounded by full JIDs with a lost reply.
    let mut actor = RoomActor::new(test_room(), test_secret());
    let alice = test_full_jid_resource("alice", "web");
    let bob = test_full_jid_resource("bob", "web");
    let outcome = |nick: &str, revision: u64| {
        super::DepartureReceiptOutcome::Left(Box::new(LeaveOutcome {
            nick: nick.to_owned(),
            affiliation: Affiliation::Member,
            role: Role::Participant,
            leaving_room_jid: test_room()
                .room_jid
                .clone()
                .with_resource_str(nick)
                .expect("nick jid"),
            remaining_occupants: Vec::new(),
            removed_last_session: true,
            cleared_muji_state: false,
            remaining_muji: None,
            remaining_muji_sessions: Vec::new(),
            remaining_nick_real_jid: None,
            occupant_count: 0,
            is_persistent: false,
            occupancy_revision: revision,
        }))
    };
    let alice_first = LeaveAttemptId::generate();
    let alice_second = LeaveAttemptId::generate();
    let bob_attempt = LeaveAttemptId::generate();
    actor.retain_departure_receipt(super::DepartureReceipt {
        attempt: alice_first,
        jid: alice.clone(),
        cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
        nick_generation: None,
        generation: OccupancyOrder::from_raw(1),
        outcome: outcome("alice", 1),
    });
    actor.retain_departure_receipt(super::DepartureReceipt {
        attempt: bob_attempt,
        jid: bob.clone(),
        cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
        nick_generation: None,
        generation: OccupancyOrder::from_raw(2),
        outcome: outcome("bob", 2),
    });
    actor.retain_departure_receipt(super::DepartureReceipt {
        attempt: alice_second,
        jid: alice.clone(),
        cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
        nick_generation: None,
        generation: OccupancyOrder::from_raw(3),
        outcome: outcome("alice", 3),
    });
    assert_eq!(actor.departure_receipts.len(), 2, "one receipt per JID");
    assert!(
        actor.take_departure_receipt(alice_first).is_none(),
        "the older departure of the same JID is superseded"
    );
    // An older generation retained late (e.g. transferred) is refused too.
    actor.retain_departure_receipt(super::DepartureReceipt {
        attempt: LeaveAttemptId::generate(),
        jid: alice.clone(),
        cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
        nick_generation: None,
        generation: OccupancyOrder::from_raw(0),
        outcome: outcome("alice", 0),
    });
    assert_eq!(actor.departure_receipts.len(), 2);
    assert!(actor.take_departure_receipt(alice_second).is_some());
    assert!(
        actor.take_departure_receipt(alice_second).is_none(),
        "a replayed receipt is consumed"
    );
    assert!(actor.take_departure_receipt(bob_attempt).is_some());
    assert!(actor.departure_receipts.is_empty());
    assert!(
        actor.latest_generations.is_empty(),
        "tombstones prune with the last receipt"
    );
}

#[tokio::test]
async fn deferred_leave_watermark_supersedes_after_replacement_rejoin() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("join");
    store.set_fenced(None);
    let watermark = match actor
        .ask(LeaveByRealJid {
            sender_jid: alice.clone(),
            cause: crate::muc::durable::OccupancyLeaveCause::Disconnect,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("deferred leave")
    {
        LeaveDisposition::Deferred { watermark } => watermark,
        other => panic!("expected deferred leave, got {other:?}"),
    };
    store.set_fenced(Some(true));
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("replacement rejoin");

    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: crate::muc::durable::OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::JoinedAtOrBefore(watermark),
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("stale retry"),
        LeaveDisposition::Superseded
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 1);
    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: crate::muc::durable::OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("current departure"),
        LeaveDisposition::Left(_)
    ));
}

#[tokio::test]
async fn legacy_join_is_gated_by_projection_commit() {
    let actor = spawn_room_actor_with_store(FakeDurableStore::deposed()).await;

    assert!(matches!(
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: test_full_jid("alice"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
}

#[tokio::test]
async fn pin_commits_projection_before_storing() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let pin = test_pinned_entry("pin");

    actor
        .ask(ApplyPin {
            change: PinStateChange::Pin(pin.clone()),
        })
        .await
        .expect("pin");

    assert!(matches!(
        store.recorded_intents().last(),
        Some(crate::muc::RoomDurableMutation::Projection(
            crate::muc::durable::RoomProjection::Pin(
                crate::muc::durable::RoomPinProjection::Pin { target }
            )
        )) if target == &pin.target_stanza_id
    ));
    assert_eq!(actor.ask(GetPinList).await.expect("pins"), vec![pin]);
}

#[tokio::test]
async fn pin_with_lost_claim_is_not_stored() {
    let actor = spawn_room_actor_with_store(FakeDurableStore::deposed()).await;

    assert!(matches!(
        actor
            .ask(ApplyPin {
                change: PinStateChange::Pin(test_pinned_entry("pin")),
            })
            .await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert!(actor.ask(GetPinList).await.expect("pins").is_empty());
}

#[tokio::test]
async fn unpin_commits_projection_before_removing() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let pin = test_pinned_entry("pin");
    actor
        .ask(ApplyPin {
            change: PinStateChange::Pin(pin.clone()),
        })
        .await
        .expect("seed pin");
    let before = store.recorded_intents().len();

    actor
        .ask(ApplyPin {
            change: PinStateChange::Unpin {
                target_stanza_id: pin.target_stanza_id.clone(),
            },
        })
        .await
        .expect("unpin");

    assert!(matches!(
        store.recorded_intents()[before..].first(),
        Some(crate::muc::RoomDurableMutation::Projection(
            crate::muc::durable::RoomProjection::Pin(
                crate::muc::durable::RoomPinProjection::Unpin { target }
            )
        )) if target == &pin.target_stanza_id
    ));
    assert!(actor.ask(GetPinList).await.expect("pins").is_empty());
}

async fn simulate_crash_after_projection_commit(
    store: std::sync::Arc<FakeDurableStore>,
) -> crate::muc::RoomCommittedCoordinates {
    let mut actor = RoomActor::new(test_room(), test_secret());
    let fence = test_claim_fence(&actor.room.room_jid);
    <FakeDurableStore as crate::muc::durable::MucDurableStore>::establish_claim_fence(
        &*store,
        &actor.room.room_jid,
        fence.clone(),
    );
    actor.durable_store = Some(store.clone());
    actor.durable_claim_fence = Some(fence);
    let gate = actor
        .commit_projection(crate::muc::durable::RoomProjection::OccupancyJoin {
            occupant: test_full_jid("alice"),
            nick: crate::muc::durable::MucOccupantNick::new("alice".to_owned())
                .expect("valid nick"),
        })
        .await
        .expect("projection commit");
    drop(gate);
    store
        .last_coordinates
        .lock()
        .expect("lock")
        .expect("projection head")
}

#[tokio::test]
async fn crash_between_commit_and_projection_resyncs_from_durable_head() {
    let store = FakeDurableStore::owned();
    let head = simulate_crash_after_projection_commit(store.clone()).await;
    store.set_restored_coordinates(head);

    let successor = spawn_room_actor_with_store(store.clone()).await;
    let snapshot = successor.ask(GetSnapshot).await.expect("snapshot");
    let projection = successor
        .ask(GetProjectionTestState)
        .await
        .expect("projection state");
    assert_eq!(snapshot.room.occupant_count(), 0);
    assert_eq!(snapshot.durable_coordinates, Some(head));
    assert_eq!(projection.projected_revision, Some(head.revision));
    assert!(matches!(
        store.recorded_intents().last(),
        Some(crate::muc::RoomDurableMutation::Projection(_))
    ));
}

#[tokio::test]
async fn retired_occupant_self_ping_fails_and_rejoin_projects_again() {
    let store = FakeDurableStore::owned();
    let head = simulate_crash_after_projection_commit(store.clone()).await;
    store.set_restored_coordinates(head);
    let successor = spawn_room_actor_with_store(store.clone()).await;
    let alice = test_full_jid("alice");

    assert!(matches!(
        successor
            .ask(PingSelfCheck {
                nick: "alice".to_owned(),
                sender_jid: alice.clone(),
            })
            .await,
        Err(SendError::HandlerError(RoomActorError::OccupantNotFound(_)))
    ));
    join_as_resolver(&successor, alice, "alice")
        .await
        .expect("rejoin");
    let rejoin_head = successor
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .durable_coordinates
        .expect("coordinates");
    assert!(rejoin_head.revision > head.revision);
}

#[test]
fn restore_resets_projected_revision_to_head() {
    let mut actor = RoomActor::new(test_room(), test_secret());
    let coordinates = crate::muc::RoomCommittedCoordinates {
        lifecycle: crate::muc::RoomLifecycleId::generate(),
        revision: crate::muc::RoomRevision::from_stored(7).expect("revision"),
    };
    actor.projected_revision = Some(crate::muc::RoomRevision::from_stored(3).expect("revision"));
    actor.install_durable_room_state(crate::muc::durable::DurableRoomState {
        coordinates: Some(coordinates),
        config_coordinates: None,
        waddle_id: "waddle-1".to_owned(),
        channel_id: "channel-1".to_owned(),
        config: RoomConfig::default(),
        subject: None,
        affiliations: Vec::new(),
    });
    assert_eq!(actor.projected_revision, Some(coordinates.revision));
}

#[test]
fn restore_installs_config_coordinates() {
    let mut actor = RoomActor::new(test_room(), test_secret());
    let coordinates = crate::muc::RoomCommittedCoordinates {
        lifecycle: crate::muc::RoomLifecycleId::generate(),
        revision: crate::muc::RoomRevision::from_stored(7).expect("revision"),
    };
    let config_coordinates = crate::muc::RoomCommittedCoordinates {
        lifecycle: coordinates.lifecycle,
        revision: crate::muc::RoomRevision::from_stored(5).expect("revision"),
    };

    actor.install_durable_room_state(crate::muc::durable::DurableRoomState {
        coordinates: Some(coordinates),
        config_coordinates: Some(config_coordinates),
        waddle_id: "waddle-1".to_owned(),
        channel_id: "channel-1".to_owned(),
        config: RoomConfig::default(),
        subject: None,
        affiliations: Vec::new(),
    });

    assert_eq!(actor.config_durable_coordinates, Some(config_coordinates));
}

#[tokio::test]
async fn members_only_enforcement_advances_config_coordinates() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_config_and_store(
        RoomConfig {
            members_only: false,
            ..RoomConfig::default()
        },
        store,
    )
    .await;
    let alice = test_full_jid("alice");
    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("join");

    let mut config = actor.ask(GetConfig).await.expect("current config");
    config.members_only = true;
    let update = actor
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::ManagedMembersOnlyFallback,
        })
        .await
        .expect("stage members-only fallback");
    let before = actor
        .ask(GetSnapshot)
        .await
        .expect("snapshot before enforcement");
    let fallback = update
        .reservation
        .expect("managed members-only stages a fallback reservation");

    actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: vec![(alice.to_bare(), Affiliation::Member)],
            fallback_reservation: Some(fallback),
            config_status_codes: update
                .notification
                .expect("managed members-only notification")
                .status_codes,
        })
        .await
        .expect("members-only enforcement");

    let after = actor
        .ask(GetSnapshot)
        .await
        .expect("snapshot after enforcement");
    assert_ne!(
        after.durable_coordinates, before.durable_coordinates,
        "the enforcement commit must advance the durable head"
    );
    assert_eq!(
        after.config_durable_coordinates, after.durable_coordinates,
        "members-only enforcement must retarget config recovery to its own durable head"
    );
}

#[tokio::test]
async fn join_commits_projection_revision_before_admitting_occupant() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let alice = test_full_jid("alice");

    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_owned(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("projection-authorized join");

    assert!(matches!(
        store.recorded_intents().last(),
        Some(crate::muc::RoomDurableMutation::Projection(
            crate::muc::durable::RoomProjection::OccupancyJoin { occupant, nick }
        )) if occupant == &alice && nick.as_str() == "alice"
    ));
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(snapshot.room.occupant_count(), 1);
    assert_eq!(
        snapshot.durable_coordinates,
        store.last_coordinates.lock().expect("lock").to_owned()
    );
}

#[tokio::test]
async fn replayed_projection_authorization_is_refused() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    actor
        .ask(ApplyPin {
            change: PinStateChange::Pin(test_pinned_entry("first")),
        })
        .await
        .expect("first projection");
    store.set_replay_last_coordinates(true);

    let result = actor
        .ask(ApplyPin {
            change: PinStateChange::Pin(test_pinned_entry("replayed")),
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            RoomActorError::OwnershipUnavailable
        ))
    ));
    assert_eq!(actor.ask(GetPinList).await.expect("pins").len(), 1);
    assert!(matches!(
        actor.ask(GetRoomSealState).await.expect("seal"),
        RoomSealState::Open
    ));
}

#[tokio::test]
async fn store_less_rooms_project_without_authorization() {
    let actor = spawn_room_actor().await;
    actor
        .ask(ApplyPin {
            change: PinStateChange::Pin(test_pinned_entry("local")),
        })
        .await
        .expect("store-less pin projection");
    assert_eq!(actor.ask(GetPinList).await.expect("pins").len(), 1);
}

#[test]
fn foreign_projection_authorization_seals_actor() {
    let mut actor = RoomActor::new(test_room(), test_secret());
    let coordinates = crate::muc::RoomCommittedCoordinates {
        lifecycle: crate::muc::RoomLifecycleId::generate(),
        revision: crate::muc::RoomRevision::initial(),
    };
    let fence = test_claim_fence(&actor.room.room_jid);
    actor.durable_claim_fence = Some(fence.clone());
    actor.durable_coordinates = Some(coordinates);
    let mut foreign_fence = fence;
    foreign_fence.epoch = crate::ownership::ClaimEpoch(2);
    let commit = crate::muc::durable::mint_room_mutation_commit(
        foreign_fence,
        coordinates,
        crate::muc::durable::RoomCommitKind::Projection(
            crate::muc::durable::RoomProjectionKind::OccupancyJoin,
        ),
    );
    let authorization = crate::muc::durable::authorize_ephemeral_projection(commit)
        .expect("projection authorization");

    let mut applied = false;
    assert_eq!(
        actor.project(
            ProjectionGate::Authorized(authorization),
            crate::muc::durable::RoomProjectionKind::OccupancyJoin,
            |_| applied = true,
        ),
        Err(ProjectionRefusal::ForeignCapability),
    );
    assert!(
        !applied,
        "a foreign capability must never run its projection"
    );
    assert_eq!(actor.seal_state, RoomSealState::OwnershipLost);
    assert_eq!(actor.room.occupant_count(), 0);
}

#[test]
fn projection_kind_mismatch_is_refused_without_sealing() {
    let mut actor = RoomActor::new(test_room(), test_secret());
    let coordinates = crate::muc::RoomCommittedCoordinates {
        lifecycle: crate::muc::RoomLifecycleId::generate(),
        revision: crate::muc::RoomRevision::initial(),
    };
    let fence = test_claim_fence(&actor.room.room_jid);
    actor.durable_claim_fence = Some(fence.clone());
    actor.durable_coordinates = Some(coordinates);
    let commit = crate::muc::durable::mint_room_mutation_commit(
        fence,
        coordinates,
        crate::muc::durable::RoomCommitKind::Projection(
            crate::muc::durable::RoomProjectionKind::Pin,
        ),
    );
    let authorization = crate::muc::durable::authorize_ephemeral_projection(commit)
        .expect("projection authorization");

    let projected_revision_before = actor.projected_revision;
    let mut applied = false;
    assert_eq!(
        actor.project(
            ProjectionGate::Authorized(authorization),
            crate::muc::durable::RoomProjectionKind::OccupancyJoin,
            |_| applied = true,
        ),
        Err(ProjectionRefusal::WrongProjectionKind),
    );
    assert!(
        !applied,
        "a mismatched capability must never run its projection"
    );
    assert_eq!(actor.seal_state, RoomSealState::Open);
    assert_eq!(actor.projected_revision, projected_revision_before);
}

#[tokio::test]
async fn live_roster_transfer_adjusts_occupant_gauge_by_roster_delta() {
    let _guard = crate::telemetry::test_support::acquire().await;
    let actor = spawn_room_actor().await;
    let before = crate::metrics::muc_occupant_total_for_test();
    let mut roster = test_room();
    for (nick, jid) in [
        ("alice", test_full_jid("transfer-alice")),
        ("bob", test_full_jid("transfer-bob")),
    ] {
        roster.add_occupant(crate::muc::Occupant {
            real_jid: jid,
            nick: nick.to_owned(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: false,
            home_server: None,
        });
    }
    actor
        .ask(RestoreLiveRoster {
            room: roster,
            occupancy_revision: 2,
            departures: Default::default(),
        })
        .await
        .expect("transfer");
    assert_eq!(crate::metrics::muc_occupant_total_for_test(), before + 2);
}

#[tokio::test]
async fn live_roster_transfer_preserves_actor_room_jid() {
    let actor = spawn_room_actor().await;
    let mut foreign_room = test_room();
    foreign_room.room_jid = "other@muc.example.com".parse().expect("jid");
    actor
        .ask(RestoreLiveRoster {
            room: foreign_room,
            occupancy_revision: 0,
            departures: Default::default(),
        })
        .await
        .expect("transfer");
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .room_jid,
        test_room().room_jid
    );
}

#[tokio::test]
async fn projection_commit_failure_is_counted_with_outcome_label() {
    let guard = crate::telemetry::test_support::acquire().await;
    let failed = spawn_room_actor_with_store(FakeDurableStore::owned_but_persist_fails()).await;
    assert!(
        join_as_resolver(&failed, test_full_jid("metric-failed"), "failed")
            .await
            .is_err()
    );
    assert_eq!(
        guard.counter_sum(
            "waddle.muc.projection.commit",
            &[
                ("projection", "occupancy_join"),
                ("outcome", "persist_failed")
            ],
        ),
        Some(1)
    );

    let succeeded = spawn_room_actor_with_store(FakeDurableStore::owned()).await;
    join_as_resolver(&succeeded, test_full_jid("metric-ok"), "ok")
        .await
        .expect("successful join");
    assert_eq!(
        guard.counter_sum(
            "waddle.muc.projection.commit",
            &[("projection", "occupancy_join"), ("outcome", "ok")],
        ),
        Some(1)
    );

    let unfenced = spawn_room_actor().await;
    join_as_resolver(&unfenced, test_full_jid("metric-local"), "local")
        .await
        .expect("store-less join");
    assert_eq!(
        guard.counter_sum(
            "waddle.muc.projection.commit",
            &[("projection", "occupancy_join"), ("outcome", "unfenced")],
        ),
        Some(1)
    );
}

#[tokio::test]
async fn occupancy_handler_duration_is_recorded_for_join_and_leave() {
    let guard = crate::telemetry::test_support::acquire().await;
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("handler-duration");

    join_as_resolver(&actor, alice.clone(), "alice")
        .await
        .expect("join");
    actor
        .ask(LeaveByRealJid {
            sender_jid: alice,
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave");

    assert_eq!(
        guard.histogram_count("waddle.muc.occupancy.handler.duration", &[("op", "join")]),
        Some(1)
    );
    assert_eq!(
        guard.histogram_count("waddle.muc.occupancy.handler.duration", &[("op", "leave")]),
        Some(1)
    );
    assert_eq!(
        guard.histogram_count(
            "waddle.muc.occupancy.handler.duration",
            &[("op", "join_request")]
        ),
        Some(0)
    );
    assert_eq!(
        guard.histogram_count(
            "waddle.muc.occupancy.handler.duration",
            &[("op", "leave_request")]
        ),
        Some(0)
    );
}

#[tokio::test]
async fn concurrent_joins_to_one_room_serialize_into_distinct_revisions_through_the_actor() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let joins = (0..16).map(|index| {
        let actor = actor.clone();
        tokio::spawn(async move {
            join_as_resolver(
                &actor,
                test_full_jid(&format!("concurrent-{index}")),
                &format!("nick-{index}"),
            )
            .await
        })
    });
    for join in joins {
        join.await
            .expect("join task")
            .expect("serialized join should succeed");
    }
    let mut revisions = store.recorded_projection_revisions();
    revisions.sort_unstable();
    assert_eq!(revisions.len(), 16);
    assert!(revisions
        .iter()
        .zip(1_i64..=16)
        .all(|(actual, expected)| actual.as_i64() == expected));
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(snapshot.room.occupant_count(), 16);
    assert_eq!(
        actor
            .ask(GetProjectionTestState)
            .await
            .expect("projection state")
            .projected_revision,
        snapshot
            .durable_coordinates
            .map(|coordinates| coordinates.revision)
    );
}

async fn seal_for_destroy(actor: &ActorRef<RoomActor>) {
    actor
        .ask(SealForDestroy {
            attempt: crate::muc::DestroyAttemptId::generate(),
        })
        .await
        .expect("pre-seal for destroy");
}

fn correlated_groupchat_message() -> xmpp_parsers::message::Message {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.id = Some(xmpp_parsers::message::Id(
        "sealed-groupchat-message".to_string(),
    ));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "sealed room message".into(),
    );
    message
}

#[tokio::test]
async fn ownership_lost_during_restore_is_terminal_and_never_retries() {
    let store = FakeDurableStore::ownership_lost_during_restore();
    let actor = spawn_room_actor().await;
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
            claim_fence: test_claim_fence(&test_room().room_jid),
        })
        .await
        .expect("restore message");

    assert_eq!(
        actor
            .ask(GetDurableRestoreReadiness)
            .await
            .expect("restore readiness"),
        DurableRestoreReadiness::OwnershipLost
    );
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("seal state"),
        RoomSealState::OwnershipLost
    );
    assert_eq!(
        store.load_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the terminal restore state must not perform the Pending retry"
    );
}

// ---------------------------------------------------------------------------
// ADR-0017 Phase 3 Slice 7 FIX 4 (council-adjudicated): restore fail-closed.
// ---------------------------------------------------------------------------

/// A [`crate::muc::durable::MucDurableStore`] test double whose
/// `load_room_state` fails its first `fail_count` calls, then succeeds
/// with a fixed [`crate::muc::durable::DurableRoomState`] carrying one
/// `Outcast` (ban) affiliation entry — proving FIX 4's "no ban-list loss"
/// requirement: the ban must still be applied once the store recovers,
/// never silently dropped by the earlier failures.
struct FlakyThenRecoveringStore {
    fail_count: usize,
    calls: std::sync::atomic::AtomicUsize,
    banned: BareJid,
    lifecycle: std::sync::OnceLock<crate::muc::RoomLifecycleId>,
    next_revision: std::sync::atomic::AtomicUsize,
    established_fences:
        std::sync::Mutex<std::collections::HashMap<BareJid, crate::muc::RoomClaimFenceContext>>,
}

impl FlakyThenRecoveringStore {
    fn new(fail_count: usize, banned: BareJid) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            fail_count,
            calls: std::sync::atomic::AtomicUsize::new(0),
            banned,
            lifecycle: std::sync::OnceLock::new(),
            next_revision: std::sync::atomic::AtomicUsize::new(0),
            established_fences: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn next_commit_coordinates(&self) -> crate::muc::RoomCommittedCoordinates {
        let lifecycle = *self
            .lifecycle
            .get_or_init(crate::muc::RoomLifecycleId::generate);
        let revision = self
            .next_revision
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        crate::muc::RoomCommittedCoordinates {
            lifecycle,
            revision: crate::muc::RoomRevision::from_stored(revision as i64)
                .expect("positive revision"),
        }
    }
}

impl crate::muc::durable::MucDurableStore for FlakyThenRecoveringStore {
    fn commit_room_mutation<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a crate::muc::RoomClaimFenceContext,
        _intent: crate::muc::RoomDurableMutation,
        _effects: crate::muc::RoomMutationEffects,
    ) -> crate::muc::RoomCommitFuture<'a> {
        let validation = validate_test_claim_fence(room_jid, fence);
        let established =
            self.established_fences.lock().expect("lock").get(room_jid) == Some(fence);
        let coordinates = self.next_commit_coordinates();
        Box::pin(async move {
            validation.map_err(|error| match error {
                crate::XmppError::OwnershipLost { .. } => crate::muc::RoomCommitError::NotOwner,
                _ => crate::muc::RoomCommitError::OwnershipUnavailable,
            })?;
            if !established {
                return Err(crate::muc::RoomCommitError::OwnershipUnavailable);
            }
            Ok(crate::muc::RoomCommitOutcome {
                coordinates,
                reservation: None,
            })
        })
    }

    fn load_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a crate::muc::RoomClaimFenceContext,
    ) -> crate::muc::durable::MucDurableFuture<'a, Option<crate::muc::durable::DurableRoomState>>
    {
        let validation = validate_test_claim_fence(room_jid, fence);
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let fail_count = self.fail_count;
        let banned = self.banned.clone();
        Box::pin(async move {
            validation?;
            if call < fail_count {
                Err(crate::XmppError::internal(
                    "simulated transient restore failure",
                ))
            } else {
                Ok(Some(crate::muc::durable::DurableRoomState {
                    coordinates: None,
                    config_coordinates: None,
                    waddle_id: "waddle-1".to_string(),
                    channel_id: "channel-1".to_string(),
                    config: RoomConfig {
                        members_only: true,
                        ..RoomConfig::default()
                    },
                    subject: None,
                    affiliations: vec![crate::muc::affiliation::AffiliationEntry::new(
                        banned,
                        Affiliation::Outcast,
                    )],
                }))
            }
        })
    }

    fn establish_claim_fence(&self, room_jid: &BareJid, fence: crate::muc::RoomClaimFenceContext) {
        self.established_fences
            .lock()
            .expect("lock")
            .insert(room_jid.clone(), fence);
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a crate::muc::RoomClaimFenceContext,
    ) -> crate::muc::durable::MucDurableFuture<'a, bool> {
        let exact_fence = validate_test_claim_fence(room_jid, fence).is_ok();
        Box::pin(async move { Ok(exact_fence) })
    }
}

fn test_join_msg(nick: &str, sender: FullJid) -> JoinWithAffiliation {
    JoinWithAffiliation {
        sender_jid: sender,
        nick: nick.to_string(),
        affiliation_grant: JoinAffiliationGrant::Unaffiliated,
        local_domain: "example.com".to_string(),
        admission_revision: 0,
    }
}

#[tokio::test]
async fn join_is_refused_while_restore_is_pending_then_succeeds_once_recovered_with_no_ban_lost() {
    let banned: BareJid = "banned@example.com".parse().expect("valid jid");
    // Fails the initial `RestoreDurableRoomState` load AND the first
    // in-line retry (calls 0 and 1); the SECOND retry (call 2) succeeds.
    let store = FlakyThenRecoveringStore::new(2, banned.clone());
    let actor = spawn_room_actor().await;
    let room_jid = test_room().room_jid;
    let claim_fence = test_claim_fence(&room_jid);
    <FlakyThenRecoveringStore as crate::muc::durable::MucDurableStore>::establish_claim_fence(
        &*store,
        &room_jid,
        claim_fence.clone(),
    );
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
            claim_fence,
        })
        .await
        .expect("restore ask itself always replies, even on a load failure");
    assert_eq!(store.call_count(), 1, "the initial load attempted once");

    // First join attempt: the inline retry (call 1) also fails — refused.
    let first_attempt = actor
        .ask(test_join_msg("alice", test_full_jid("alice")))
        .await;
    assert!(
        matches!(
            first_attempt,
            Err(SendError::HandlerError(RoomActorError::RestorePending))
        ),
        "expected RestorePending, got: {first_attempt:?}"
    );
    assert_eq!(store.call_count(), 2);

    // Second join attempt: the inline retry (call 2) succeeds. Applying the
    // recovered state advances the room-wide fence, so the join must
    // re-snapshot instead of proceeding with resolver input computed against
    // the pre-restore config.
    let recovering_attempt = actor
        .ask(test_join_msg("banned-nick", test_full_jid("banned")))
        .await;
    assert!(
        matches!(
            recovering_attempt,
            Err(SendError::HandlerError(
                RoomActorError::StaleAdmissionRevision
            ))
        ),
        "the join that recovered durable state must re-snapshot, got: \
         {recovering_attempt:?}"
    );
    assert_eq!(store.call_count(), 3);

    // The retry uses the recovered snapshot and enforces the restored ban.
    let recovered_revision = current_admission_revision(&actor).await;
    let mut banned_join = test_join_msg("banned-nick", test_full_jid("banned"));
    banned_join.admission_revision = recovered_revision;
    let banned_attempt = actor.ask(banned_join).await;
    assert!(
        matches!(
            banned_attempt,
            Err(SendError::HandlerError(
                RoomActorError::JoinForbidden { .. }
            ))
        ),
        "the restored ban must be enforced after re-snapshot, got: {banned_attempt:?}"
    );

    // A different, never-banned sender can now join normally too.
    let mut carol_join = test_join_msg("carol", test_full_jid("carol"));
    carol_join.admission_revision = recovered_revision;
    carol_join.affiliation_grant = JoinAffiliationGrant::Resolver(Affiliation::Member);
    actor
        .ask(carol_join)
        .await
        .expect("a non-banned sender joins normally once restore has recovered");
}

#[tokio::test]
async fn update_config_durable_commit_blocks_the_mutation_when_deposed() {
    let store = FakeDurableStore::deposed();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let original = actor.ask(GetConfig).await.expect("ask").members_only;

    let mut new_config = actor.ask(GetConfig).await.expect("ask");
    new_config.members_only = !original;
    let result = actor
        .ask(UpdateConfig {
            config: new_config,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(RoomMutationError::NotOwner))
        ),
        "expected NotOwner, got: {result:?}"
    );

    let after = actor.ask(GetConfig).await.expect("ask").members_only;
    assert_eq!(
        after, original,
        "a rejected durable commit must never apply the mutation in memory"
    );

    store.set_fenced(None);
    let mut retry_config = actor.ask(GetConfig).await.expect("retry config");
    retry_config.members_only = !original;
    let retry = actor
        .ask(UpdateConfig {
            config: retry_config,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await;
    assert!(
        matches!(
            retry,
            Err(SendError::HandlerError(RoomMutationError::NotOwner))
        ),
        "a definitive mutation-gate loss must remain terminal across later uncertainty"
    );
}

#[tokio::test]
async fn update_config_durable_commit_allows_the_mutation_when_owned() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let original = actor.ask(GetConfig).await.expect("ask").members_only;

    let mut new_config = actor.ask(GetConfig).await.expect("ask");
    new_config.members_only = !original;
    actor
        .ask(UpdateConfig {
            config: new_config,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("owned mutation must apply");

    let after = actor.ask(GetConfig).await.expect("ask").members_only;
    assert_ne!(after, original, "the mutation must have applied");
}

#[tokio::test]
async fn update_config_persists_moderation_flip_voice_changes() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Visitor,
            affiliation: Affiliation::None,
        })
        .await
        .expect("visitor joins unmoderated room");

    let mut config = actor.ask(GetConfig).await.expect("config");
    config.moderated = true;
    actor
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("moderation flip persists");

    let saved_effects = store.saved_effects();
    match &saved_effects[0].effects()[0] {
        crate::muc::RoomEffect::ConfigChanged {
            status_codes,
            recipients,
            voice_changes,
        } => {
            assert_eq!(
                status_codes,
                &vec![crate::muc::MucConfigStatusCode::NonPrivacyConfigurationChange]
            );
            assert_eq!(recipients, &vec![alice.clone()]);
            assert_eq!(
                voice_changes,
                &vec![crate::muc::OccupantVoiceChange {
                    session: alice,
                    voice: Voice::Muted,
                }]
            );
        }
        other => panic!("expected config-changed effect, got {other:?}"),
    }
}

#[tokio::test]
async fn update_config_durable_commit_fails_closed_on_transient_ownership_error() {
    let actor = spawn_room_actor_with_store(FakeDurableStore::transient_failure()).await;
    let original = actor.ask(GetConfig).await.expect("ask").members_only;

    let mut new_config = actor.ask(GetConfig).await.expect("ask");
    new_config.members_only = !original;
    let result = actor
        .ask(UpdateConfig {
            config: new_config,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(
                RoomMutationError::OwnershipUnavailable
            ))
        ),
        "an unprovable exact fence must fail closed with a typed retryable error: {result:?}"
    );

    let after = actor.ask(GetConfig).await.expect("ask").members_only;
    assert_eq!(
        after, original,
        "ownership uncertainty must prevent the in-memory mutation"
    );
}

#[tokio::test]
async fn inactive_seal_strengthens_to_ownership_lost_on_join() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let probe = actor.ask(IsDormant).await.expect("dormancy probe");
    assert_eq!(
        actor
            .ask(SealIfInactive {
                expected_occupancy_revision: probe.occupancy_revision,
                guard: SealGuard::Dormant,
            })
            .await
            .expect("seal inactive room"),
        SealIfInactiveOutcome::Inactive,
    );

    store.set_fenced(Some(false));
    let join = actor
        .ask(test_join_msg("alice", test_full_jid("alice")))
        .await;
    assert!(matches!(
        join,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("typed seal state"),
        RoomSealState::OwnershipLost,
        "an inactivity seal must retain the stronger ownership-loss proof"
    );
}

#[tokio::test]
async fn destroy_seal_blocks_zero_delta_mutations_and_pins() {
    use crate::muc::pin::{PinPreview, PinStateChange, PinnedEntry};
    use chrono::Utc;
    use waddle_xmpp_core::xep0359::StanzaId;

    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Moderator,
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("join before seal");

    seal_for_destroy(&actor).await;

    // Zero-durable-delta admin work (a role-only change) reaches only the
    // pre-mutation gate; the destroy pre-seal must refuse it so no kicks,
    // presence, or SFU effects race the terminal commit.
    let role_only = actor
        .ask(ApplyAdminItems {
            sender_jid: alice.clone(),
            sender_affiliation: Affiliation::Owner,
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
    assert!(
        matches!(
            role_only,
            Err(SendError::HandlerError(
                AdminApplyError::OwnershipUnavailable
            ))
        ),
        "destroy pre-seal must refuse zero-delta mutations: {role_only:?}"
    );

    // Pins are ungated in-memory state; the seal must still make them inert.
    let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
    let target = StanzaId::new(
        "pin-during-destroy".to_string(),
        jid::Jid::from(room_jid.clone()),
    );
    let pin_result = actor
        .ask(ApplyPin {
            change: PinStateChange::Pin(PinnedEntry {
                target_stanza_id: target,
                pinner_jid: "admin@example.com".parse().expect("valid jid"),
                pinned_at: Utc::now(),
                preview: PinPreview::new(
                    "alice@example.com".parse().expect("valid jid"),
                    Some("alice".into()),
                    "sealed",
                    Utc::now(),
                ),
            }),
        })
        .await;
    assert!(
        matches!(
            pin_result,
            Err(SendError::HandlerError(
                RoomActorError::OwnershipUnavailable
            ))
        ),
        "destroy pre-seal must refuse pin state changes: {pin_result:?}"
    );
    assert!(
        actor
            .ask(GetPinList)
            .await
            .expect("pins readable")
            .is_empty(),
        "a sealed actor must not mutate pin state"
    );
}

#[tokio::test]
async fn destroy_seal_blocks_members_only_enforcement() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;
    let alice = test_full_jid("alice");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice,
            role: Role::Participant,
            affiliation: Affiliation::None,
        })
        .await
        .expect("join before seal");

    seal_for_destroy(&actor).await;

    assert!(matches!(
        actor.ask(EnforceMembersOnly).await,
        Err(SendError::HandlerError(
            RoomMutationError::OwnershipUnavailable
        ))
    ));
    assert_eq!(
        actor.ask(OccupantCount).await.expect("occupant count"),
        1,
        "sealed enforcement must leave the occupant set untouched"
    );
}

#[tokio::test]
async fn leave_under_destroying_seal_is_deferred_without_memory_change() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;
    let alice = test_full_jid("alice");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join before seal");

    let attempt = crate::muc::DestroyAttemptId::generate();
    actor
        .ask(SealForDestroy { attempt })
        .await
        .expect("pre-seal for destroy");

    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave ask"),
        LeaveDisposition::Deferred { .. }
    ));
    assert_eq!(
        actor.ask(OccupantCount).await.expect("occupant count"),
        1,
        "durable departures wait for a projection commit while the destroy is unresolved"
    );
    assert!(actor
        .ask(UnsealDestroy { attempt })
        .await
        .expect("matching unseal reply"));
    assert_eq!(
        actor.ask(OccupantCount).await.expect("occupant count"),
        1,
        "unsealing does not apply a deferred departure until it is retried"
    );
}

#[tokio::test]
async fn store_less_leave_under_destroying_seal_is_suppressed_and_recorded() {
    let actor = spawn_room_actor().await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_owned(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");
    seal_for_destroy(&actor).await;

    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave reply"),
        LeaveDisposition::Suppressed { .. }
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
}

#[tokio::test]
async fn unseal_destroy_then_retried_leave_projects_departure() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;
    let alice = test_full_jid("alice");
    actor
        .ask(Join {
            nick: "alice".to_owned(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");
    let attempt = crate::muc::DestroyAttemptId::generate();
    actor.ask(SealForDestroy { attempt }).await.expect("seal");
    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("deferred reply"),
        LeaveDisposition::Deferred { .. }
    ));
    assert!(actor.ask(UnsealDestroy { attempt }).await.expect("unseal"));
    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: crate::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("retry reply"),
        LeaveDisposition::Left(_)
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("count"), 0);
}

#[tokio::test]
async fn destroy_seal_blocks_presence_reflection() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;
    let alice = test_full_jid("alice");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join before seal");

    seal_for_destroy(&actor).await;

    assert!(
        actor
            .ask(PresenceUpdateData { sender_jid: alice })
            .await
            .expect("presence ask")
            .is_none(),
        "a sealed actor must not return broadcast recipients for plain presence reflection"
    );
}

#[tokio::test]
async fn destroy_seal_blocks_muji_upserts() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;
    let alice = test_full_jid("alice");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join before seal");

    seal_for_destroy(&actor).await;

    assert!(
        actor
            .ask(UpsertMujiPresence {
                sender_jid: alice.clone(),
                muji: audio_muji(),
            })
            .await
            .expect("muji ask")
            .is_none(),
        "a sealed actor must refuse queued Muji advertisements"
    );
    assert!(
        actor
            .ask(GetActiveMujiSessions)
            .await
            .expect("active Muji sessions")
            .is_empty(),
        "refused Muji work must not mutate active call state"
    );
}

#[tokio::test]
async fn destroy_seal_blocks_muji_clears() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;
    let alice = test_full_jid("alice");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join before seal");
    actor
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: audio_muji(),
        })
        .await
        .expect("seed muji")
        .expect("occupant can advertise before seal");

    seal_for_destroy(&actor).await;

    assert!(
        actor
            .ask(ClearMujiPresence {
                sender_jid: alice.clone(),
            })
            .await
            .expect("clear ask")
            .is_none(),
        "a sealed actor must not clear call presence or emit reflected clears"
    );
    assert_eq!(
        actor
            .ask(GetActiveMujiSessions)
            .await
            .expect("active Muji sessions"),
        vec![alice],
        "refused clears must leave prior Muji state untouched"
    );
}

#[tokio::test]
async fn destroy_seal_blocks_in_call_state_updates() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;
    let alice = test_full_jid("alice");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join before seal");

    seal_for_destroy(&actor).await;

    assert!(
        actor
            .ask(UpsertInCallState {
                sender_jid: alice,
                state: crate::xep::InCallPresenceState {
                    hand_raised: true,
                    muted: false,
                },
            })
            .await
            .expect("in-call ask")
            .is_none(),
        "a sealed actor must not mutate in-call state or emit reflected presence"
    );
    assert!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .in_call_sessions_for_nick("alice")
            .is_empty(),
        "refused in-call updates must leave the session state empty"
    );
}

#[tokio::test]
async fn destroy_seal_defers_leave_handler() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join before seal");

    seal_for_destroy(&actor).await;

    let leave = actor
        .ask(LeaveByRealJid {
            sender_jid: test_full_jid("alice"),
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave reply");
    assert!(matches!(leave, LeaveDisposition::Deferred { .. }));
    assert_eq!(
        actor.ask(OccupantCount).await.expect("occupant count"),
        1,
        "a deferred leave must not remove the occupant"
    );
}

#[tokio::test]
async fn destroy_seal_blocks_groupchat_broadcasts() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;
    let alice = test_full_jid("alice");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join before seal");

    seal_for_destroy(&actor).await;

    let broadcast = actor
        .ask(BuildGroupchatBroadcast {
            sender_jid: alice,
            message: correlated_groupchat_message(),
        })
        .await;
    assert!(
        matches!(
            broadcast,
            Err(SendError::HandlerError(
                RoomActorError::OwnershipUnavailable
            ))
        ),
        "a sealed actor must not fan out queued groupchat messages: {broadcast:?}"
    );
}

#[tokio::test]
async fn destroy_seal_blocks_room_dispatch_snapshots() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store).await;
    let alice = test_full_jid("alice");

    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join before seal");

    seal_for_destroy(&actor).await;

    let snapshot = actor.ask(GetRoomSnapshot { sender_jid: alice }).await;
    assert!(
        matches!(
            snapshot,
            Err(SendError::HandlerError(
                RoomActorError::OwnershipUnavailable
            ))
        ),
        "a sealed actor must not mint dispatch-authorizing room snapshots: {snapshot:?}"
    );
}

#[tokio::test]
async fn ownership_lost_seal_blocks_a_later_mutation() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let original = actor.ask(GetConfig).await.expect("config");

    store.set_fenced(Some(false));
    let join = actor
        .ask(test_join_msg("alice", test_full_jid("alice")))
        .await;
    assert!(matches!(
        join,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));

    // A prior definitive loss remains terminal for this actor incarnation,
    // even if the next database probe is merely uncertain.
    store.set_fenced(None);
    let mut changed = original.clone();
    changed.members_only = !changed.members_only;
    let update = actor
        .ask(UpdateConfig {
            config: changed,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await;
    assert!(matches!(
        update,
        Err(SendError::HandlerError(RoomMutationError::NotOwner))
    ));
    assert_eq!(
        actor
            .ask(GetConfig)
            .await
            .expect("unchanged config")
            .members_only,
        original.members_only,
        "a definitive ownership-loss seal must dominate later uncertainty"
    );
    assert_eq!(
        store.save_call_count(),
        0,
        "the rejected mutation must not attempt a durable write"
    );
}

#[tokio::test]
async fn update_config_surfaces_a_typed_persist_failure_without_mutating_memory() {
    let store = FakeDurableStore::owned_but_persist_fails();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let original = actor.ask(GetConfig).await.expect("ask").members_only;

    let mut new_config = actor.ask(GetConfig).await.expect("ask");
    new_config.members_only = !original;
    let result = actor
        .ask(UpdateConfig {
            config: new_config,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(RoomMutationError::PersistFailed))
        ),
        "expected PersistFailed, got: {result:?}"
    );
    assert_eq!(store.save_call_count(), 1);

    let after = actor.ask(GetConfig).await.expect("ask").members_only;
    assert_eq!(
        after, original,
        "a failed durable commit must leave the in-memory config unchanged"
    );
}

#[tokio::test]
async fn update_config_seals_the_actor_when_the_fenced_write_loses_ownership() {
    let store = FakeDurableStore::owned_but_config_persist_loses_ownership();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let original = actor.ask(GetConfig).await.expect("original config");

    let mut changed = original.clone();
    changed.members_only = !changed.members_only;
    let result = actor
        .ask(UpdateConfig {
            config: changed,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(RoomMutationError::NotOwner))
        ),
        "expected NotOwner, got: {result:?}"
    );
    assert_eq!(store.save_call_count(), 1, "the fenced write was attempted");
    assert_eq!(
        actor
            .ask(GetConfig)
            .await
            .expect("config after failed commit"),
        original,
        "a lost durable claim must leave config memory unchanged"
    );
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("typed seal state"),
        RoomSealState::OwnershipLost,
        "the exact ownership loss seals this actor incarnation"
    );

    let later = actor
        .ask(UpdateConfig {
            config: original,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await;
    assert!(
        matches!(
            later,
            Err(SendError::HandlerError(RoomMutationError::NotOwner))
        ),
        "a sealed actor must reject later mutation work: {later:?}"
    );
    assert_eq!(
        store.save_call_count(),
        1,
        "later rejected mutation work must not attempt another durable write"
    );
}

#[tokio::test]
async fn unknown_durable_commit_outcome_seals_actor_before_it_can_serve_stale_memory() {
    let store = FakeDurableStore::owned_but_commit_outcome_is_unknown();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let original = actor.ask(GetConfig).await.expect("original config");

    let mut changed = original.clone();
    changed.members_only = !changed.members_only;
    let result = actor
        .ask(UpdateConfig {
            config: changed.clone(),
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(
                RoomMutationError::CommitOutcomeUnknown
            ))
        ),
        "an ambiguous durable commit must retire the stale actor: {result:?}"
    );
    assert_eq!(
        store.save_call_count(),
        1,
        "the ambiguous commit was attempted"
    );
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("typed seal state"),
        RoomSealState::OwnershipLost,
        "the stale actor must become non-serving until the registry retires it"
    );

    let later = actor
        .ask(UpdateConfig {
            config: original,
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await;
    assert!(
        matches!(
            later,
            Err(SendError::HandlerError(RoomMutationError::NotOwner))
        ),
        "the sealed actor must never attempt a follow-up durable mutation: {later:?}"
    );
    assert_eq!(
        store.save_call_count(),
        1,
        "a sealed actor must not write a possibly stale follow-up mutation"
    );

    let join = actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: test_full_jid("alice"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await;
    assert!(
        matches!(
            join,
            Err(SendError::HandlerError(RoomActorError::RoomSealed))
        ),
        "the stale actor must refuse admissions until a fresh actor restores durable state: {join:?}"
    );
}

#[tokio::test]
async fn change_affiliation_durable_commit_blocks_the_mutation_when_deposed() {
    let actor = spawn_room_actor_with_store(FakeDurableStore::deposed()).await;
    let jid: BareJid = "carol@example.com".parse().expect("valid jid");

    let result = actor
        .ask(ChangeAffiliation {
            jid: jid.clone(),
            affiliation: Affiliation::Member,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(AffiliationMutationError::NotOwner))
        ),
        "expected NotOwner, got: {result:?}"
    );

    let affiliation = actor.ask(GetAffiliation { jid }).await.expect("ask");
    assert_eq!(
        affiliation,
        Affiliation::None,
        "a rejected durable affiliation commit must never apply in memory"
    );
}

#[tokio::test]
async fn change_affiliation_surfaces_ambiguous_commit_outcome_without_compensation_proof() {
    let actor =
        spawn_room_actor_with_store(FakeDurableStore::owned_but_commit_outcome_is_unknown()).await;
    let jid: BareJid = "carol@example.com".parse().expect("valid jid");

    let result = actor
        .ask(ChangeAffiliation {
            jid: jid.clone(),
            affiliation: Affiliation::Member,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(
                AffiliationMutationError::CommitOutcomeUnknown
            ))
        ),
        "an ambiguous durable affiliation commit must not masquerade as a failed grant: {result:?}"
    );
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("typed seal state"),
        RoomSealState::OwnershipLost,
        "the stale actor must still retire after the ambiguous commit"
    );
    assert_eq!(
        actor
            .ask(GetAffiliation { jid })
            .await
            .expect("affiliation query"),
        Affiliation::None,
        "the actor must not apply an in-memory affiliation after an ambiguous durable commit"
    );
}

#[tokio::test]
async fn apply_admin_items_surfaces_ambiguous_commit_outcome_without_compensation_proof() {
    let actor =
        spawn_room_actor_with_store(FakeDurableStore::owned_but_commit_outcome_is_unknown()).await;
    let jid: BareJid = "dana@example.com".parse().expect("valid jid");

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: Some(jid.clone()),
                nick: None,
                affiliation: Some(Affiliation::Member),
                role: None,
                reason: None,
            }],
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(
                AdminApplyError::CommitOutcomeUnknown
            ))
        ),
        "an ambiguous durable admin batch must stay ambiguous instead of masquerading as a clean rollback: {result:?}"
    );
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("typed seal state"),
        RoomSealState::OwnershipLost,
        "the stale actor must still retire after the ambiguous admin batch"
    );
    assert_eq!(
        actor
            .ask(GetAffiliation { jid })
            .await
            .expect("affiliation query"),
        Affiliation::None,
        "the actor must not apply an in-memory affiliation after an ambiguous durable admin batch"
    );
}

#[tokio::test]
async fn role_only_admin_items_stay_direct_without_an_outbox_reservation() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
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

    let applied = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
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
        .expect("role change applies");

    assert!(
        applied.outbox_reservation.is_none(),
        "pure role-only outcomes stay on the direct path"
    );
    assert_eq!(
        store.save_call_count(),
        1,
        "the join projection commits once; the role-only change adds no durable commit"
    );
    assert!(
        store.saved_effects().is_empty(),
        "role-only mutations must not enqueue durable admin effects"
    );
}

#[tokio::test]
async fn apply_affiliation_change_threads_admin_effects_and_reservation() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_config_and_store(
        RoomConfig {
            members_only: true,
            ..RoomConfig::default()
        },
        store.clone(),
    )
    .await;
    let alice = test_full_jid("alice");
    let bob = test_full_jid("bob");

    for (nick, real_jid) in [("alice", alice.clone()), ("bob", bob.clone())] {
        actor
            .ask(JoinWithAffiliation {
                sender_jid: real_jid,
                nick: nick.to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision: current_admission_revision(&actor).await,
            })
            .await
            .expect("join");
    }

    let applied = actor
        .ask(ApplyAffiliationChange {
            actor: Some("owner@example.com".parse().expect("owner jid")),
            jid: alice.to_bare(),
            affiliation: Affiliation::None,
        })
        .await
        .expect("affiliation change applies");

    let reservation = applied
        .outbox_reservation
        .clone()
        .expect("durable affiliation admin work should reserve its outbox rows");
    assert_eq!(reservation.ordinals.len(), 2);

    let saved_effects = store.saved_effects();
    assert_eq!(saved_effects.len(), 1);
    let effects = &saved_effects[0];
    assert_eq!(
        effects.staging(),
        crate::muc::RoomEffectStagingClass::HandlerWindow
    );
    assert_eq!(effects.effects().len(), 2);

    match &effects.effects()[0] {
        crate::muc::RoomEffect::AdminSelfNotify { updates } => {
            assert_eq!(updates.len(), 1);
            assert_eq!(updates[0].recipient, alice);
            assert_eq!(updates[0].occupant_bare_jid, alice.to_bare());
            assert_eq!(updates[0].disclosed_real_jid.as_ref(), Some(&alice));
            assert!(matches!(
                updates[0].kind,
                crate::muc::AdminPresenceKind::AffiliationRemoved
            ));
        }
        other => panic!("expected self-notify effect, got {other:?}"),
    }

    match &effects.effects()[1] {
        crate::muc::RoomEffect::AdminRemainingBroadcast {
            presence_updates,
            removed_sessions,
            voice_changes,
        } => {
            assert_eq!(presence_updates.len(), 1);
            assert_eq!(presence_updates[0].recipient, bob);
            assert!(!presence_updates[0].is_self);
            assert_eq!(presence_updates[0].occupant_bare_jid, alice.to_bare());
            assert_eq!(
                presence_updates[0].disclosed_real_jid.as_ref(),
                Some(&alice)
            );
            assert_eq!(removed_sessions, &vec![alice.clone()]);
            assert!(voice_changes.is_empty());
        }
        other => panic!("expected remaining-broadcast effect, got {other:?}"),
    }
}

#[tokio::test]
async fn zero_delta_managed_members_only_enforcement_still_commits_one_config_effect() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_config_and_store(
        RoomConfig {
            members_only: false,
            ..RoomConfig::default()
        },
        store.clone(),
    )
    .await;
    let alice = test_full_jid("alice");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_owned(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("already-qualified member joins");

    let mut config = actor.ask(GetConfig).await.expect("current config");
    config.members_only = true;
    let update = actor
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::ManagedMembersOnlyFallback,
        })
        .await
        .expect("stage members-only fallback");
    let notification = update.notification.expect("members-only notification");
    let fallback = update.reservation.expect("staged fallback reservation");
    let expected_fallback = fallback.clone();

    let applied = actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: vec![(alice.to_bare(), Affiliation::Member)],
            fallback_reservation: Some(fallback),
            config_status_codes: notification.status_codes.clone(),
        })
        .await
        .expect("zero-delta enforcement commits");
    assert!(
        applied.outbox_reservation.is_some(),
        "a zero-delta enforcement must still reserve its fused config notification"
    );
    assert!(
        store.saved_affiliations().is_empty(),
        "the qualified occupant causes no affiliation delta"
    );

    let saved_effects = store.saved_effects();
    assert_eq!(
        saved_effects.len(),
        2,
        "fallback plus exactly one fused effect"
    );
    let fused = &saved_effects[1];
    assert_eq!(
        fused.staging(),
        crate::muc::RoomEffectStagingClass::HandlerWindow
    );
    assert_eq!(
        fused.superseding_reservation(),
        Some(&expected_fallback),
        "the zero-delta commit must supersede the exact staged fallback"
    );
    assert_eq!(fused.effects().len(), 3);
    assert!(matches!(
        &fused.effects()[0],
        crate::muc::RoomEffect::AdminSelfNotify { updates } if updates.is_empty()
    ));
    assert!(matches!(
        &fused.effects()[1],
        crate::muc::RoomEffect::AdminRemainingBroadcast { presence_updates, removed_sessions, voice_changes }
            if presence_updates.is_empty() && removed_sessions.is_empty() && voice_changes.is_empty()
    ));
    assert!(matches!(
        &fused.effects()[2],
        crate::muc::RoomEffect::ConfigChanged { status_codes, recipients, voice_changes }
            if status_codes == &notification.status_codes
                && recipients.as_slice() == std::slice::from_ref(&alice)
                && voice_changes.is_empty()
    ));
}

#[tokio::test]
async fn managed_members_only_enforcement_dedupes_sibling_nick_322_effects() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_config_and_store(
        RoomConfig {
            members_only: false,
            ..RoomConfig::default()
        },
        store.clone(),
    )
    .await;
    let owner = test_full_jid("owner");
    let alice_laptop = test_full_jid_resource("alice", "laptop");
    let alice_phone = test_full_jid_resource("alice", "phone");

    for (nick, real_jid, affiliation_grant) in [
        (
            "owner",
            owner.clone(),
            JoinAffiliationGrant::Resolver(Affiliation::Owner),
        ),
        (
            "alice",
            alice_laptop.clone(),
            JoinAffiliationGrant::Resolver(Affiliation::None),
        ),
        (
            "alice-phone",
            alice_phone.clone(),
            JoinAffiliationGrant::Resolver(Affiliation::None),
        ),
    ] {
        actor
            .ask(JoinWithAffiliation {
                sender_jid: real_jid,
                nick: nick.to_owned(),
                affiliation_grant,
                local_domain: "example.com".to_owned(),
                admission_revision: current_admission_revision(&actor).await,
            })
            .await
            .expect("join");
    }

    let mut config = actor.ask(GetConfig).await.expect("current config");
    config.members_only = true;
    let update = actor
        .ask(UpdateConfig {
            config,
            effect_plan: ConfigEffectPlan::ManagedMembersOnlyFallback,
        })
        .await
        .expect("stage members-only fallback");
    let notification = update.notification.expect("members-only notification");

    actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: vec![
                (owner.to_bare(), Affiliation::Owner),
                (alice_laptop.to_bare(), Affiliation::None),
            ],
            fallback_reservation: update.reservation,
            config_status_codes: notification.status_codes.clone(),
        })
        .await
        .expect("managed enforcement commits");

    let saved_effects = store.saved_effects();
    assert_eq!(
        saved_effects.len(),
        2,
        "fallback plus exactly one fused effect"
    );
    let fused = &saved_effects[1];
    assert_eq!(fused.effects().len(), 3);

    match &fused.effects()[0] {
        crate::muc::RoomEffect::AdminSelfNotify { updates } => {
            let pairs: std::collections::BTreeSet<(String, String)> = updates
                .iter()
                .map(|update| (update.occupant.to_string(), update.recipient.to_string()))
                .collect();
            assert_eq!(updates.len(), 2, "one self 322 per removed nick");
            assert_eq!(
                pairs.len(),
                2,
                "duplicate sibling self-updates must not be persisted"
            );
        }
        other => panic!("expected self-notify effect, got {other:?}"),
    }

    match &fused.effects()[1] {
        crate::muc::RoomEffect::AdminRemainingBroadcast {
            presence_updates,
            removed_sessions,
            voice_changes,
        } => {
            let pairs: std::collections::BTreeSet<(String, String)> = presence_updates
                .iter()
                .map(|update| (update.occupant.to_string(), update.recipient.to_string()))
                .collect();
            assert_eq!(
                presence_updates.len(),
                4,
                "two removed nicks should fan out to the owner and sibling session once each"
            );
            assert_eq!(
                pairs.len(),
                4,
                "duplicate sibling broadcasts must not be persisted"
            );
            let removed_sessions: std::collections::BTreeSet<String> =
                removed_sessions.iter().map(ToString::to_string).collect();
            assert_eq!(
                removed_sessions,
                std::collections::BTreeSet::from([
                    alice_laptop.to_string(),
                    alice_phone.to_string(),
                ])
            );
            assert!(voice_changes.is_empty());
        }
        other => panic!("expected remaining-broadcast effect, got {other:?}"),
    }
}

#[tokio::test]
async fn update_group_dm_config_surfaces_ambiguous_commit_outcome_without_compensation_proof() {
    let actor = spawn_room_actor_with_config_and_store(
        RoomConfig {
            group_dm: true,
            members_only: true,
            ..RoomConfig::default()
        },
        FakeDurableStore::owned_but_config_commit_outcome_is_unknown(),
    )
    .await;
    let alice_bare: BareJid = "alice@example.com".parse().expect("valid jid");
    let alice = test_full_jid("alice");
    let config = actor.ask(GetConfig).await.expect("config");
    actor
        .ask(ChangeAffiliation {
            jid: alice_bare,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("member grant");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: alice.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");

    let result = actor
        .ask(UpdateGroupDmConfigByMember {
            config,
            sender_jid: alice,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(
                UpdateGroupDmConfigByMemberError::CommitOutcomeUnknown
            ))
        ),
        "an ambiguous durable group-DM rename must stay ambiguous instead of masquerading as a failed ownership check: {result:?}"
    );
}

#[tokio::test]
async fn no_op_affiliation_change_still_checks_ownership_when_deposed() {
    let actor = spawn_room_actor_with_store(FakeDurableStore::deposed()).await;
    let jid: BareJid = "carol@example.com".parse().expect("valid jid");

    let result = actor
        .ask(ChangeAffiliation {
            jid,
            affiliation: Affiliation::None,
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(AffiliationMutationError::NotOwner))
    ));
}

#[tokio::test]
async fn set_subject_owned_commit_persists_zero_effect_rows() {
    let store = FakeDurableStore::owned();
    let actor = spawn_room_actor_with_store(store.clone()).await;
    let setter: BareJid = "alice@example.com".parse().expect("valid jid");

    actor
        .ask(SetSubject {
            texts: RoomSubjectTexts::from_iter([(String::new(), "new subject".to_string())]),
            setter,
            setter_nick: "alice".to_string(),
            set_at: chrono::Utc::now(),
        })
        .await
        .expect("subject update succeeds");

    let saved_effects = store.saved_effects();
    assert_eq!(saved_effects.len(), 1, "subject commit should persist once");
    assert!(
        saved_effects
            .iter()
            .all(|effects| effects.effects().is_empty()),
        "subject mutations must not enqueue durable effect rows"
    );
    assert!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("subject query")
            .room
            .subject
            .is_some(),
        "the successful subject commit must still update room state"
    );
}

#[tokio::test]
async fn set_subject_surfaces_ambiguous_commit_outcome_without_mutating() {
    let actor =
        spawn_room_actor_with_store(FakeDurableStore::owned_but_commit_outcome_is_unknown()).await;
    let setter: BareJid = "alice@example.com".parse().expect("valid jid");

    let result = actor
        .ask(SetSubject {
            texts: RoomSubjectTexts::from_iter([(String::new(), "new subject".to_string())]),
            setter,
            setter_nick: "alice".to_string(),
            set_at: chrono::Utc::now(),
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(
                SetSubjectError::CommitOutcomeUnknown
            ))
        ),
        "an ambiguous durable subject commit must stay ambiguous instead of masquerading as ownership loss: {result:?}"
    );
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("typed seal state"),
        RoomSealState::OwnershipLost,
        "the stale actor must still retire after the ambiguous subject commit"
    );
    assert!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("subject query")
            .room
            .subject
            .is_none(),
        "the actor must not apply an in-memory subject after an ambiguous durable commit"
    );
}

#[tokio::test]
async fn set_subject_gate_blocks_the_mutation_when_deposed() {
    let actor = spawn_room_actor_with_store(FakeDurableStore::deposed()).await;
    let setter: BareJid = "alice@example.com".parse().expect("valid jid");

    let result = actor
        .ask(SetSubject {
            texts: RoomSubjectTexts::from_iter([(String::new(), "new subject".to_string())]),
            setter,
            setter_nick: "alice".to_string(),
            set_at: chrono::Utc::now(),
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(SetSubjectError::NotOwner))
        ),
        "expected NotOwner, got: {result:?}"
    );
}

#[tokio::test]
async fn set_subject_gate_surfaces_ownership_uncertainty_without_mutating() {
    let actor = spawn_room_actor_with_store(FakeDurableStore::transient_failure()).await;
    let setter: BareJid = "alice@example.com".parse().expect("valid jid");

    let result = actor
        .ask(SetSubject {
            texts: RoomSubjectTexts::from_iter([(String::new(), "new subject".to_string())]),
            setter,
            setter_nick: "alice".to_string(),
            set_at: chrono::Utc::now(),
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            SetSubjectError::OwnershipUnavailable
        ))
    ));
    assert!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("subject query")
            .room
            .subject
            .is_none(),
        "an uncertain ownership gate must not apply the subject"
    );
}

/// A resolver-derived affiliation must never replace an explicit grant
/// (#1110 follow-up): an explicit Outcast (ban) survives a resolver
/// write saying the user is a Member, and the banned user's join is
/// still refused. XEP-0045 §7.2.8: outcasts are denied entry.
#[tokio::test]
async fn resolver_write_does_not_replace_explicit_ban_and_join_stays_forbidden() {
    let actor = spawn_room_actor().await;
    let banned: BareJid = "mallory@example.com".parse().expect("bare jid");
    actor
        .ask(ChangeAffiliation {
            jid: banned.clone(),
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("ban mallory");

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    let outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: "mallory@example.com/res".parse().expect("full jid"),
            nick: "mallory".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: snapshot.admission_revision,
        })
        .await;

    assert!(
        matches!(
            outcome,
            Err(SendError::HandlerError(
                RoomActorError::JoinForbidden { .. }
            ))
        ),
        "banned user joining with a resolver-derived Member affiliation \
         must still be refused, got {outcome:?}"
    );
}

/// A `Resolver(Affiliation::None)` grant must be APPLIED, not skipped:
/// the authz resolver revoking a previously derived Member/Admin tier
/// has to clear the stale resolver-derived entry from room state, so a
/// revoked user no longer passes members-only admission on rejoin.
#[tokio::test]
async fn resolver_none_clears_stale_resolver_derived_affiliation_and_join_is_forbidden() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    })
    .await;
    let alice: FullJid = test_full_jid("alice");
    let alice_bare = alice.to_bare();

    // Seed a resolver-derived Member entry, then leave the room.
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("resolver-derived member joins the members-only room");
    actor
        .ask(LeaveByRealJid {
            sender_jid: alice.clone(),
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave");

    // The resolver has since revoked the derived tier: the rejoin
    // carries Resolver(None) and must be refused, not admitted via the
    // stale Member entry.
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    let outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: alice,
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::None),
            local_domain: "example.com".to_string(),
            admission_revision: snapshot.admission_revision,
        })
        .await;
    assert!(
        matches!(
            outcome,
            Err(SendError::HandlerError(
                RoomActorError::JoinForbidden { .. }
            ))
        ),
        "revoked user's rejoin must be refused by members-only admission, got {outcome:?}"
    );

    let affiliation = actor
        .ask(GetAffiliation {
            jid: alice_bare.clone(),
        })
        .await
        .expect("affiliation query");
    assert_eq!(
        affiliation,
        Affiliation::None,
        "the stale resolver-derived Member entry must be cleared"
    );
}

/// Companion to the Resolver(None) fix: applying the None write must
/// NOT lift an explicit ban — `set_with_provenance` refuses
/// resolver-derived writes over explicit grants, so the Outcast entry
/// survives and the join stays forbidden.
#[tokio::test]
async fn resolver_none_does_not_clear_explicit_ban() {
    let actor = spawn_room_actor().await;
    let banned: BareJid = "mallory@example.com".parse().expect("bare jid");
    actor
        .ask(ChangeAffiliation {
            jid: banned.clone(),
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("ban mallory");

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    let outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: "mallory@example.com/res".parse().expect("full jid"),
            nick: "mallory".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::None),
            local_domain: "example.com".to_string(),
            admission_revision: snapshot.admission_revision,
        })
        .await;
    assert!(
        matches!(
            outcome,
            Err(SendError::HandlerError(
                RoomActorError::JoinForbidden { .. }
            ))
        ),
        "banned user must stay refused, got {outcome:?}"
    );

    let affiliation = actor
        .ask(GetAffiliation { jid: banned })
        .await
        .expect("affiliation query");
    assert_eq!(
        affiliation,
        Affiliation::Outcast,
        "the explicit ban must survive the resolver None write"
    );
}

/// Review F3: on the members-only path the handler rejects a revoked
/// user's join BEFORE any actor message, so `JoinWithAffiliation`'s
/// `Resolver(None)` write never reaches a live actor and the stale
/// resolver-derived Member from before the revocation lingers on the
/// room's affiliation list (admin queries, XEP-0045 §7.x member lists)
/// until eviction. `SyncResolverAffiliation` lets the handler clear it
/// best-effort as part of the rejection.
#[tokio::test]
async fn sync_resolver_affiliation_clears_stale_resolver_derived_member() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    })
    .await;
    let alice: FullJid = test_full_jid("alice");
    let alice_bare = alice.to_bare();

    // Seed a resolver-derived Member entry, then leave the room.
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("resolver-derived member joins the members-only room");
    actor
        .ask(LeaveByRealJid {
            sender_jid: alice.clone(),
            cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: crate::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave");

    let outcome = actor
        .ask(SyncResolverAffiliation {
            jid: alice_bare.clone(),
            affiliation: Affiliation::None,
            expected_admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("sync resolver affiliation");
    assert_eq!(
        outcome,
        ResolverAffiliationSyncOutcome::Applied {
            admission_revision: current_admission_revision(&actor).await,
        },
        "a sync at the current admission revision must apply"
    );

    let affiliation = actor
        .ask(GetAffiliation { jid: alice_bare })
        .await
        .expect("affiliation query");
    assert_eq!(
        affiliation,
        Affiliation::None,
        "the stale resolver-derived Member entry must be cleared by the sync"
    );
}

/// Companion: the sync is provenance-aware — a resolver-derived None
/// write must never lift an explicit ban.
#[tokio::test]
async fn sync_resolver_affiliation_does_not_touch_explicit_grants() {
    let actor = spawn_room_actor().await;
    let banned: BareJid = "mallory@example.com".parse().expect("bare jid");
    actor
        .ask(ChangeAffiliation {
            jid: banned.clone(),
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("ban mallory");

    actor
        .ask(SyncResolverAffiliation {
            jid: banned.clone(),
            affiliation: Affiliation::None,
            expected_admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("sync resolver affiliation");

    let affiliation = actor
        .ask(GetAffiliation { jid: banned })
        .await
        .expect("affiliation query");
    assert_eq!(
        affiliation,
        Affiliation::Outcast,
        "the explicit ban must survive the resolver-derived sync"
    );
}

/// Admission-revision freshness guard: join A is rejected at revision
/// R (resolver None), the user is re-granted, join B succeeds at R and
/// re-derives Member (bumping the admission revision), THEN A's
/// delayed `SyncResolverAffiliation { None, expected: R }` lands. The
/// stale sync must be refused — otherwise it clears the Member of a
/// live occupant admitted by join B.
#[tokio::test]
async fn stale_sync_resolver_affiliation_does_not_clear_readmitted_member() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");
    let alice_bare = alice.to_bare();

    // The revision both join A's rejection decision and join B's
    // admission were computed against.
    let stale_revision = current_admission_revision(&actor).await;

    // Join B succeeds and re-derives Member — an admission-relevant
    // affiliation change, so the admission revision moves on.
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice,
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: stale_revision,
        })
        .await
        .expect("re-granted member joins the members-only room");
    assert_ne!(
        current_admission_revision(&actor).await,
        stale_revision,
        "a join that re-derives the resolver affiliation must bump the admission revision"
    );

    // Join A's delayed rejection sync lands with the stale revision.
    let outcome = actor
        .ask(SyncResolverAffiliation {
            jid: alice_bare.clone(),
            affiliation: Affiliation::None,
            expected_admission_revision: stale_revision,
        })
        .await
        .expect("ask");
    assert_eq!(
        outcome,
        ResolverAffiliationSyncOutcome::StaleAdmissionRevision,
        "the stale sync must be refused"
    );

    let affiliation = actor
        .ask(GetAffiliation { jid: alice_bare })
        .await
        .expect("affiliation query");
    assert_eq!(
        affiliation,
        Affiliation::Member,
        "a stale rejection sync must not clear the live occupant's re-derived Member"
    );
}

/// The re-granted affiliation can already be present in actor memory when
/// join B arrives: rejection A has not delivered its delayed resolver sync
/// yet. The successful resolver-backed admission must still advance the
/// revision even though writing Member is a no-op, or A's stale None repair
/// can demote the live occupant after admission.
#[tokio::test]
async fn stale_sync_does_not_clear_readmitted_member_when_resolver_grant_is_identical() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");
    let alice_bare = alice.to_bare();

    let seeded = actor
        .ask(SyncResolverAffiliation {
            jid: alice_bare.clone(),
            affiliation: Affiliation::Member,
            expected_admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("seed resolver-derived member");
    assert!(matches!(
        seeded,
        ResolverAffiliationSyncOutcome::Applied { .. }
    ));
    let stale_revision = current_admission_revision(&actor).await;

    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: stale_revision,
        })
        .await
        .expect("already-derived member rejoins successfully");
    assert_eq!(
        current_admission_revision(&actor).await,
        stale_revision.saturating_add(1),
        "successful resolver-backed admission must invalidate older rejection repairs even when the affiliation write is identical"
    );

    let outcome = actor
        .ask(SyncResolverAffiliation {
            jid: alice_bare.clone(),
            affiliation: Affiliation::None,
            expected_admission_revision: stale_revision,
        })
        .await
        .expect("deliver delayed rejection repair");
    assert_eq!(
        outcome,
        ResolverAffiliationSyncOutcome::StaleAdmissionRevision,
    );
    assert_eq!(
        actor
            .ask(GetAffiliation {
                jid: alice_bare.clone(),
            })
            .await
            .expect("affiliation after stale repair"),
        Affiliation::Member,
    );
    assert_eq!(
        actor
            .ask(GetOccupantByJid { jid: alice })
            .await
            .expect("live occupant lookup")
            .expect("member remains admitted")
            .affiliation,
        Affiliation::Member,
        "the delayed repair must not demote the live occupant",
    );
}

/// A successful resolver-backed join must fence delayed repairs only for the
/// joining bare JID. Bob's identical Member grant must not make Alice's
/// already-queued revocation stale and leave her old Member entry behind.
#[tokio::test]
async fn resolver_join_does_not_invalidate_another_members_repair() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    })
    .await;
    let alice = test_full_jid("alice");
    let alice_bare = alice.to_bare();
    let bob = test_full_jid("bob");
    let bob_bare = bob.to_bare();

    for jid in [&alice_bare, &bob_bare] {
        let outcome = actor
            .ask(SyncResolverAffiliation {
                jid: jid.clone(),
                affiliation: Affiliation::Member,
                expected_admission_revision: current_admission_revision(&actor).await,
            })
            .await
            .expect("seed resolver-derived member");
        assert!(matches!(
            outcome,
            ResolverAffiliationSyncOutcome::Applied { .. }
        ));
    }
    let alice_repair_revision = current_admission_revision(&actor).await;

    actor
        .ask(JoinWithAffiliation {
            sender_jid: bob.clone(),
            nick: "bob".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: alice_repair_revision,
        })
        .await
        .expect("bob joins with an identical resolver grant");

    let outcome = actor
        .ask(SyncResolverAffiliation {
            jid: alice_bare.clone(),
            affiliation: Affiliation::None,
            expected_admission_revision: alice_repair_revision,
        })
        .await
        .expect("apply Alice's queued revocation");
    assert!(matches!(
        outcome,
        ResolverAffiliationSyncOutcome::Applied { .. }
    ));
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: alice_bare })
            .await
            .expect("Alice affiliation after repair"),
        Affiliation::None,
    );
    assert_eq!(
        actor
            .ask(GetAffiliation { jid: bob_bare })
            .await
            .expect("Bob affiliation after Alice repair"),
        Affiliation::Member,
    );
    assert!(
        actor
            .ask(GetOccupantByJid { jid: bob })
            .await
            .expect("Bob occupant lookup")
            .is_some(),
        "Alice's repair must not disturb Bob's successful admission"
    );
}

struct GetMemberAdmissionRevisionCount;

impl kameo::message::Message<GetMemberAdmissionRevisionCount> for RoomActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: GetMemberAdmissionRevisionCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.member_admission_revisions.len()
    }
}

#[tokio::test]
async fn member_admission_watermarks_compact_without_accepting_stale_work() {
    let actor = spawn_room_actor().await;
    let stale_revision = current_admission_revision(&actor).await;

    for index in 0..=MAX_MEMBER_ADMISSION_REVISIONS {
        let jid: BareJid = format!("historical-{index}@example.com")
            .parse()
            .expect("bare JID");
        actor
            .ask(ChangeAffiliation {
                jid: jid.clone(),
                affiliation: Affiliation::Member,
            })
            .await
            .expect("add historical member");
        actor
            .ask(ChangeAffiliation {
                jid,
                affiliation: Affiliation::None,
            })
            .await
            .expect("remove historical member");
    }

    assert!(
        actor
            .ask(GetMemberAdmissionRevisionCount)
            .await
            .expect("watermark count")
            <= MAX_MEMBER_ADMISSION_REVISIONS,
        "historical JIDs must not grow the per-room tracker without bound"
    );
    assert_eq!(
        actor
            .ask(SyncResolverAffiliation {
                jid: "pending@example.com".parse().expect("bare JID"),
                affiliation: Affiliation::None,
                expected_admission_revision: stale_revision,
            })
            .await
            .expect("stale post-compaction repair"),
        ResolverAffiliationSyncOutcome::StaleAdmissionRevision,
        "compaction must retain a conservative fence for discarded entries"
    );
}

/// A sealed actor (#1108) is pending destruction; a delayed rejection
/// sync must be refused instead of mutating state the registry already
/// decided to drop.
#[tokio::test]
async fn sync_resolver_affiliation_refuses_sealed_actor() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        persistent: false,
        ..RoomConfig::default()
    })
    .await;
    let alice: BareJid = "alice@example.com".parse().expect("bare jid");
    actor
        .ask(SyncResolverAffiliation {
            jid: alice.clone(),
            affiliation: Affiliation::Member,
            expected_admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("seed resolver-derived member");

    let probe = actor
        .ask(crate::muc::room_actor::IsDormant)
        .await
        .expect("dormancy probe");
    let sealed = actor
        .ask(crate::muc::room_actor::SealIfInactive {
            expected_occupancy_revision: probe.occupancy_revision,
            guard: crate::muc::room_actor::SealGuard::EmptyNonPersistent,
        })
        .await
        .expect("seal");
    assert_eq!(
        sealed,
        crate::muc::room_actor::SealIfInactiveOutcome::Inactive,
        "empty non-persistent room must seal"
    );

    let outcome = actor
        .ask(SyncResolverAffiliation {
            jid: alice.clone(),
            affiliation: Affiliation::None,
            expected_admission_revision: current_admission_revision(&actor).await,
        })
        .await
        .expect("ask");
    assert_eq!(
        outcome,
        ResolverAffiliationSyncOutcome::RoomSealed,
        "a sealed actor must refuse the sync"
    );
    let affiliation = actor
        .ask(GetAffiliation { jid: alice })
        .await
        .expect("affiliation query");
    assert_eq!(
        affiliation,
        Affiliation::Member,
        "the refused sync must leave the sealed actor's state untouched"
    );
}

/// A resolver write with a different value must not downgrade an
/// explicit grant (#1110): an admin-granted Admin stays Admin when the
/// resolver reports Member, and it keeps blocking dormancy.
#[tokio::test]
async fn resolver_write_does_not_downgrade_explicit_grant() {
    let mut room = test_room();
    let alice: BareJid = "alice@example.com".parse().expect("bare jid");
    room.set_affiliation(alice.clone(), Affiliation::Admin);

    room.update_affiliation_from_resolver(alice.clone(), Affiliation::Member);

    assert_eq!(
        room.get_affiliation(&alice),
        Affiliation::Admin,
        "resolver-derived Member must not downgrade the explicit Admin grant"
    );
    assert!(
        !room.is_dormant(),
        "the surviving explicit grant must keep blocking dormancy"
    );
}

/// gpt-5.5 review follow-up to #1108: a sealed actor must report
/// dormant even when explicit affiliations would normally block
/// dormancy (the EmptyNonPersistent guard seals instant rooms holding
/// the creator's Owner grant). Otherwise a seal whose registry reply
/// timed out leaves a sealed-but-registered room the janitor never
/// re-confirms — permanently unjoinable.
#[tokio::test]
async fn sealed_room_reports_dormant_so_the_sweep_converges() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        persistent: false,
        ..RoomConfig::default()
    })
    .await;
    let owner: BareJid = "creator@example.com".parse().expect("bare jid");
    actor
        .ask(ChangeAffiliation {
            jid: owner,
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("grant owner");
    let probe = actor
        .ask(crate::muc::room_actor::IsDormant)
        .await
        .expect("dormancy probe before seal");
    let sealed = actor
        .ask(crate::muc::room_actor::SealIfInactive {
            expected_occupancy_revision: probe.occupancy_revision,
            guard: crate::muc::room_actor::SealGuard::EmptyNonPersistent,
        })
        .await
        .expect("seal");
    assert_eq!(
        sealed,
        crate::muc::room_actor::SealIfInactiveOutcome::Inactive,
        "empty non-persistent room must seal"
    );

    let status = actor
        .ask(crate::muc::room_actor::IsDormant)
        .await
        .expect("dormancy probe");
    assert!(
        status.dormant,
        "a sealed room must report dormant regardless of explicit \
         affiliations, so the next sweep re-confirms the seal"
    );
}

// ---------------------------------------------------------------------------
// XEP-0045 §8.2/§8.4/§9.7 role-change target protections (#1262).
//
// The target-protection matrix binds EVERY actor, the room owner
// included: §8.4 "a service MUST NOT allow the voice privileges of an
// admin or owner to be removed by anyone"; §9.7 moderator status
// "cannot be revoked from a room owner or room admin"; §8.2 "a user
// cannot be kicked by a moderator with a lower affiliation".
// ---------------------------------------------------------------------------

async fn join_with(actor: &ActorRef<RoomActor>, user: &str, affiliation: Affiliation, role: Role) {
    actor
        .ask(Join {
            nick: user.to_string(),
            real_jid: test_full_jid(user),
            role,
            affiliation,
        })
        .await
        .expect("join");
}

fn role_item(nick: &str, role: Role) -> AdminItem {
    AdminItem {
        jid: None,
        nick: Some(nick.to_string()),
        affiliation: None,
        role: Some(role),
        reason: None,
    }
}

async fn role_of(actor: &ActorRef<RoomActor>, nick: &str) -> Role {
    actor
        .ask(GetOccupantByNick {
            nick: nick.to_string(),
        })
        .await
        .expect("occupant ask")
        .expect("occupant exists")
        .role
}

/// §8.4: even the room owner must not remove an admin's voice
/// (role → visitor is a spec-impossible state for an admin).
#[tokio::test]
async fn xep0045_owner_cannot_revoke_voice_from_admin() {
    let actor = spawn_room_actor().await;
    join_with(&actor, "admin", Affiliation::Admin, Role::Moderator).await;

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![role_item("admin", Role::Visitor)],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            AdminApplyError::CannotModifyPrivilegedRole
        ))
    ));
    assert_eq!(role_of(&actor, "admin").await, Role::Moderator);
}

/// §8.4: an owner's voice is equally protected against another owner.
#[tokio::test]
async fn xep0045_owner_cannot_revoke_voice_from_owner() {
    let actor = spawn_room_actor().await;
    join_with(&actor, "cohost", Affiliation::Owner, Role::Moderator).await;

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![role_item("cohost", Role::Visitor)],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            AdminApplyError::CannotModifyPrivilegedRole
        ))
    ));
    assert_eq!(role_of(&actor, "cohost").await, Role::Moderator);
}

/// §9.7: even the room owner must not revoke moderator status from an
/// admin (role → participant).
#[tokio::test]
async fn xep0045_owner_cannot_revoke_moderator_from_admin() {
    let actor = spawn_room_actor().await;
    join_with(&actor, "admin", Affiliation::Admin, Role::Moderator).await;

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![role_item("admin", Role::Participant)],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            AdminApplyError::CannotModifyPrivilegedRole
        ))
    ));
    assert_eq!(role_of(&actor, "admin").await, Role::Moderator);
}

/// §8.2 only bars kicks by actors with a LOWER affiliation: the owner
/// may still kick an admin out of the room (ejection is not a voice or
/// moderator-status revocation).
#[tokio::test]
async fn xep0045_owner_can_kick_admin() {
    let actor = spawn_room_actor().await;
    join_with(&actor, "admin", Affiliation::Admin, Role::Moderator).await;

    let applied = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![role_item("admin", Role::None)],
        })
        .await
        .expect("owner kick of an admin is allowed");

    assert!(
        applied
            .removed_by_moderation
            .contains(&test_full_jid("admin")),
        "kicked admin session must be marked removed-by-moderation"
    );
    let occupant = actor
        .ask(GetOccupantByNick {
            nick: "admin".to_string(),
        })
        .await
        .expect("occupant ask");
    assert!(occupant.is_none(), "kicked admin must leave the room");
}

/// §8.2: an admin (higher target affiliation) must not kick an owner.
#[tokio::test]
async fn xep0045_admin_cannot_kick_owner() {
    let actor = spawn_room_actor().await;
    join_with(&actor, "owner", Affiliation::Owner, Role::Moderator).await;

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("admin"),
            sender_affiliation: Affiliation::Admin,
            sender_role: Role::Moderator,
            items: vec![role_item("owner", Role::None)],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            AdminApplyError::CannotModifyPrivilegedRole
        ))
    ));
    assert_eq!(role_of(&actor, "owner").await, Role::Moderator);
}

/// §8.4: a member-affiliated moderator must not revoke voice from a
/// target whose affiliation is at or above their own level.
#[tokio::test]
async fn xep0045_member_moderator_cannot_revoke_voice_from_equal_affiliation() {
    let actor = spawn_room_actor().await;
    join_with(&actor, "target", Affiliation::Member, Role::Participant).await;

    let result = actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("mod"),
            sender_affiliation: Affiliation::Member,
            sender_role: Role::Moderator,
            items: vec![role_item("target", Role::Visitor)],
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            AdminApplyError::CannotModifyPrivilegedRole
        ))
    ));
    assert_eq!(role_of(&actor, "target").await, Role::Participant);
}

/// Sanity: the owner may still revoke voice from a plain member —
/// target protection only shields admins/owners (and same-or-higher
/// affiliations from plain moderators).
#[tokio::test]
async fn xep0045_owner_can_revoke_voice_from_member() {
    let actor = spawn_room_actor().await;
    join_with(&actor, "member", Affiliation::Member, Role::Participant).await;

    actor
        .ask(ApplyAdminItems {
            sender_jid: test_full_jid("owner"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![role_item("member", Role::Visitor)],
        })
        .await
        .expect("owner devoices a plain member");

    assert_eq!(role_of(&actor, "member").await, Role::Visitor);
}

/// #1265 item 1 / XEP-0045 §7.2.8: a banned user joining a
/// members-only room is refused as `Banned` (→ <forbidden/>), never as
/// `MembersOnly` (→ <registration-required/>, which would invite the
/// banned user to apply for membership). A plain non-member keeps the
/// `MembersOnly` reason.
#[tokio::test]
async fn banned_join_denial_reason_is_banned_even_in_members_only_room() {
    let actor = spawn_room_actor_with_config(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    })
    .await;
    let banned: BareJid = "mallory@example.com".parse().expect("bare jid");
    actor
        .ask(ChangeAffiliation {
            jid: banned,
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("ban mallory");

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    let outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: "mallory@example.com/res".parse().expect("full jid"),
            nick: "mallory".to_string(),
            affiliation_grant: JoinAffiliationGrant::Unaffiliated,
            local_domain: "example.com".to_string(),
            admission_revision: snapshot.admission_revision,
        })
        .await;
    assert!(
        matches!(
            outcome,
            Err(SendError::HandlerError(RoomActorError::JoinForbidden {
                reason: crate::muc::room_actor::JoinDenialReason::Banned
            }))
        ),
        "ban outranks members-only in the denial reason, got {outcome:?}"
    );

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    let outcome = actor
        .ask(JoinWithAffiliation {
            sender_jid: "stranger@example.com/res".parse().expect("full jid"),
            nick: "stranger".to_string(),
            affiliation_grant: JoinAffiliationGrant::Unaffiliated,
            local_domain: "example.com".to_string(),
            admission_revision: snapshot.admission_revision,
        })
        .await;
    assert!(
        matches!(
            outcome,
            Err(SendError::HandlerError(RoomActorError::JoinForbidden {
                reason: crate::muc::room_actor::JoinDenialReason::MembersOnly
            }))
        ),
        "plain non-member keeps the MembersOnly reason, got {outcome:?}"
    );
}

#[tokio::test]
async fn destroy_unseal_only_reopens_the_matching_attempt() {
    let actor = spawn_room_actor().await;
    let first = crate::muc::DestroyAttemptId::generate();
    let second = crate::muc::DestroyAttemptId::generate();

    assert_eq!(
        actor
            .ask(SealForDestroy { attempt: first })
            .await
            .expect("pre-seal first destroy"),
        RoomSealState::Destroying { attempt: first }
    );
    assert_eq!(
        actor
            .ask(SealForDestroy { attempt: second })
            .await
            .expect("do not replace active destroy attempt"),
        RoomSealState::Destroying { attempt: first }
    );
    assert!(actor
        .ask(UnsealDestroy { attempt: first })
        .await
        .expect("matching unseal reply"));
    assert_eq!(
        actor
            .ask(SealForDestroy { attempt: second })
            .await
            .expect("pre-seal newer destroy"),
        RoomSealState::Destroying { attempt: second }
    );
    assert!(!actor
        .ask(UnsealDestroy { attempt: first })
        .await
        .expect("stale unseal reply"));
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("seal state"),
        RoomSealState::Destroying { attempt: second }
    );
}
