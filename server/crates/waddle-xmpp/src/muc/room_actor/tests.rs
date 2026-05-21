use super::*;

use crate::muc::admin::AdminItem;
use crate::xep::xep0421::OccupantIdSecret;
use kameo::actor::{ActorRef, Spawn};
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

fn empty_muji() -> crate::xep::xep0272::Muji {
    // No `<preparing/>`, no `<content/>` → XEP-0272 §Leaving "absence
    // marker": the participant has exited the call.
    crate::xep::xep0272::Muji::default()
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
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
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
async fn clear_muji_presence_returns_none_when_no_state_exists() {
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

    assert!(
        outcome.is_none(),
        "plain available presence without prior Muji state stays on the existing join/update path"
    );
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
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
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
        })
        .await
        .expect("desktop join");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: mobile.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
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
