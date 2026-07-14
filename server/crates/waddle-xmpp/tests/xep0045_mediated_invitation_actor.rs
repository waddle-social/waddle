//! XEP-0045 §7.8 mediated-invitation actor conformance tests.
//!
//! The room actor is the policy boundary that verifies the exact inviting
//! occupant and, when required, creates the temporary member-list grant.

use jid::{BareJid, FullJid};
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use waddle_xmpp::muc::presence::NS_MUC_USER;
use waddle_xmpp::muc::room_actor::{
    AuthorizeMediatedInvite, ChangeAffiliation, CommitMediatedInviteGrantRollback, GetAffiliation,
    InviteMembershipGrant, Join, MediatedInviteGrantError, MediatedInviteOperationId,
    MediatedInviteRollbackCommit, PrepareMediatedInviteGrantRollback, RoomActor,
};
use waddle_xmpp::muc::{MucRoom, MucStatusCode, RoomConfig};
use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OCCUPANT_ID_SECRET_MIN_BYTES};
use waddle_xmpp::{Affiliation, Role};

fn full_jid(resource: &str) -> FullJid {
    format!("inviter@example.com/{resource}")
        .parse()
        .expect("valid inviter JID")
}

fn invitee() -> BareJid {
    "invitee@example.com".parse().expect("valid invitee JID")
}

fn spawn_room(config: RoomConfig) -> ActorRef<RoomActor> {
    let room_jid = "invitations@muc.example.com"
        .parse()
        .expect("valid room JID");
    let room = MucRoom::new(
        room_jid,
        "waddle-1".to_string(),
        "channel-1".to_string(),
        config,
    );
    let secret = OccupantIdSecret::new(vec![7; OCCUPANT_ID_SECRET_MIN_BYTES])
        .expect("valid occupant-id secret");
    RoomActor::spawn(RoomActor::new(room, secret))
}

async fn join(actor: &ActorRef<RoomActor>, jid: FullJid, affiliation: Affiliation) {
    actor
        .ask(waddle_xmpp::muc::room_actor::Join {
            nick: "inviter".to_string(),
            real_jid: jid,
            role: Role::Moderator,
            affiliation,
        })
        .await
        .expect("join inviter");
}

#[tokio::test]
async fn open_room_occupied_inviter_needs_no_membership_grant() {
    let actor = spawn_room(RoomConfig {
        members_only: false,
        ..RoomConfig::default()
    });
    let inviter = full_jid("web");
    join(&actor, inviter.clone(), Affiliation::None).await;

    let authorized = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: MediatedInviteOperationId::generate(),
            inviter,
            invitee: invitee(),
        })
        .await
        .expect("occupied inviter may invite into an open room");

    assert!(authorized.grant.is_none());
    assert!(!authorized.members_only);
    assert_eq!(authorized.invitee_affiliation, Affiliation::None);
}

#[tokio::test]
async fn occupancy_is_scoped_to_the_exact_full_jid() {
    let actor = spawn_room(RoomConfig {
        members_only: false,
        ..RoomConfig::default()
    });
    join(&actor, full_jid("web"), Affiliation::None).await;

    let result = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: MediatedInviteOperationId::generate(),
            inviter: full_jid("mobile"),
            invitee: invitee(),
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            MediatedInviteGrantError::NotOccupant
        ))
    ));
}

#[tokio::test]
async fn members_only_admin_gets_typed_temporary_member_grant_with_stable_replay() {
    let actor = spawn_room(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    });
    let inviter = full_jid("web");
    join(&actor, inviter.clone(), Affiliation::Admin).await;
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("persist inviter admin affiliation");
    let operation_id = MediatedInviteOperationId::generate();
    let invitee = invitee();
    let request = || AuthorizeMediatedInvite {
        operation_id,
        inviter: inviter.clone(),
        invitee: invitee.clone(),
    };

    let first = actor
        .ask(request())
        .await
        .expect("admin may invite into a members-only room");
    let replay = actor
        .ask(request())
        .await
        .expect("same operation replays its authorization");

    assert_eq!(replay, first);
    let grant: &InviteMembershipGrant = first
        .grant
        .as_ref()
        .expect("actor-created temporary membership authority");
    assert_eq!(grant.operation_id(), operation_id);
    assert_eq!(grant.invitee(), &invitee);
    assert_eq!(grant.previous_affiliation(), Affiliation::None);
    assert_eq!(first.invitee_affiliation, Affiliation::Member);
    assert!(first.members_only);
}

#[tokio::test]
async fn group_dm_rejects_an_occupied_inviter_below_member() {
    let actor = spawn_room(RoomConfig {
        group_dm: true,
        members_only: true,
        ..RoomConfig::default()
    });
    let inviter = full_jid("web");
    join(&actor, inviter.clone(), Affiliation::None).await;

    let result = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: MediatedInviteOperationId::generate(),
            inviter,
            invitee: invitee(),
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(MediatedInviteGrantError::Forbidden))
    ));
}

#[tokio::test]
async fn group_dm_member_invite_reports_members_only_and_grants_membership() {
    let actor = spawn_room(RoomConfig {
        group_dm: true,
        members_only: false,
        ..RoomConfig::default()
    });
    let inviter = full_jid("web");
    join(&actor, inviter.clone(), Affiliation::Member).await;
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("persist inviter membership");

    let authorized = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: MediatedInviteOperationId::generate(),
            inviter,
            invitee: invitee(),
        })
        .await
        .expect("group-DM member may invite");

    assert!(authorized.grant.is_some());
    assert_eq!(authorized.invitee_affiliation, Affiliation::Member);
    assert!(authorized.members_only);
}

#[tokio::test]
async fn mediated_invitation_cannot_replace_an_outcast_ban_with_membership() {
    for (config, inviter_affiliation) in [
        (
            RoomConfig {
                members_only: true,
                ..RoomConfig::default()
            },
            Affiliation::Admin,
        ),
        (
            RoomConfig {
                group_dm: true,
                members_only: false,
                ..RoomConfig::default()
            },
            Affiliation::Member,
        ),
    ] {
        let actor = spawn_room(config);
        let inviter = full_jid("web");
        let invitee = invitee();
        join(&actor, inviter.clone(), inviter_affiliation).await;
        actor
            .ask(ChangeAffiliation {
                jid: inviter.to_bare(),
                affiliation: inviter_affiliation,
            })
            .await
            .expect("persist inviter affiliation");
        actor
            .ask(ChangeAffiliation {
                jid: invitee.clone(),
                affiliation: Affiliation::Outcast,
            })
            .await
            .expect("ban invitee");

        assert!(matches!(
            actor
                .ask(AuthorizeMediatedInvite {
                    operation_id: MediatedInviteOperationId::generate(),
                    inviter,
                    invitee: invitee.clone(),
                })
                .await,
            Err(SendError::HandlerError(
                MediatedInviteGrantError::InviteeBanned
            ))
        ));
        assert_eq!(
            actor
                .ask(GetAffiliation { jid: invitee })
                .await
                .expect("ban remains authoritative"),
            Affiliation::Outcast,
        );
    }
}

#[tokio::test]
async fn rolling_back_a_joined_invitee_emits_xep0045_status_321() {
    let actor = spawn_room(RoomConfig {
        members_only: true,
        ..RoomConfig::default()
    });
    let inviter = full_jid("web");
    join(&actor, inviter.clone(), Affiliation::Admin).await;
    actor
        .ask(ChangeAffiliation {
            jid: inviter.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("persist inviter admin affiliation");
    let invitee = invitee();
    let grant = actor
        .ask(AuthorizeMediatedInvite {
            operation_id: MediatedInviteOperationId::generate(),
            inviter,
            invitee: invitee.clone(),
        })
        .await
        .expect("authorize members-only invite")
        .grant
        .expect("temporary membership grant");
    let invitee_session: FullJid = "invitee@example.com/web"
        .parse()
        .expect("valid invitee full JID");
    actor
        .ask(Join {
            nick: "invitee".to_string(),
            real_jid: invitee_session,
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("invitee joins before delivery fails");
    actor
        .ask(PrepareMediatedInviteGrantRollback {
            grant: grant.clone(),
        })
        .await
        .expect("prepare compensation");

    let MediatedInviteRollbackCommit::Applied { updates, .. } = actor
        .ask(CommitMediatedInviteGrantRollback { grant })
        .await
        .expect("commit compensation")
    else {
        panic!("the exact temporary membership grant must roll back");
    };
    assert!(updates.presence_updates.iter().any(|(_, presence)| {
        presence.payloads.iter().any(|payload| {
            payload.name() == "x"
                && payload.ns() == NS_MUC_USER
                && payload.children().any(|child| {
                    child.name() == "status"
                        && child.ns() == NS_MUC_USER
                        && child.attr("code").and_then(|code| code.parse::<u16>().ok())
                            == Some(MucStatusCode::AffiliationChange as u16)
                })
        })
    }));
}
