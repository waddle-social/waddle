use super::*;

use crate::muc::admin::AdminItem;
use crate::xep::xep0421::OccupantIdSecret;
use kameo::actor::ActorRef;
use kameo::error::SendError;

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

async fn spawn_room_actor() -> ActorRef<RoomActor> {
    kameo::spawn(RoomActor::new(test_room(), test_secret()))
}

async fn spawn_room_actor_with_config(mut config: RoomConfig) -> ActorRef<RoomActor> {
    let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
    config.name = "Test Room".to_string();
    kameo::spawn(RoomActor::new(
        MucRoom::new(
            room_jid,
            "waddle-1".to_string(),
            "channel-1".to_string(),
            config,
        ),
        test_secret(),
    ))
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
        Err(SendError::HandlerError(AdminApplyError::PermissionDenied(
            _
        )))
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
