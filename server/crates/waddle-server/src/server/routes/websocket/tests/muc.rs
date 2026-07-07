use super::*;
use crate::permissions::CheckPermission;

#[tokio::test]
async fn muc_stale_leave_does_not_remove_current_resource() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let current_jid: FullJid = "alice@example.com/current".parse().expect("current jid");
    let stale_jid: FullJid = "alice@example.com/stale".parse().expect("stale jid");

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &current_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    let responses = handle_muc_leave(state.as_ref(), &room_jid, &stale_jid, "alice").await;

    assert_eq!(responses.len(), 1);
    let response = Element::from_str(&responses[0]).expect("leave response XML");
    assert_eq!(response.name(), "presence");
    assert_eq!(response.attr("type"), Some("unavailable"));

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.find_nick_by_real_jid(&current_jid), Some("alice"));
    assert!(room.find_nick_by_real_jid(&stale_jid).is_none());
    assert_eq!(room.occupant_count(), 1);
}

/// #1108: after the dormancy janitor's guarded destroy evicts a room,
/// a join must transparently respawn it through the registry — the
/// "room not registered; dropping" failure mode must not exist for a
/// joining occupant.
#[tokio::test]
async fn muc_join_after_guarded_dormancy_eviction_respawns_room() {
    use waddle_xmpp::muc::room_actor::{IsDormant, SealGuard};
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DestroyRoomIfInactive, RoomCount};

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "evicted-then-joined@muc.example.com"
        .parse()
        .expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    // Seed a persistent, dormant room (like topology seeding does),
    // then evict it exactly as the janitor would.
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w".to_string(),
            channel_id: "c".to_string(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("create room");
    let probe = actor.ask(IsDormant).await.expect("probe");
    assert!(probe.dormant);
    let destroyed: bool = state
        .deps
        .protocol
        .room_registry
        .ask(DestroyRoomIfInactive {
            room_jid: room_jid.clone(),
            expected_occupancy_revision: probe.occupancy_revision,
            guard: SealGuard::Dormant,
        })
        .await
        .expect("guarded destroy");
    assert!(destroyed, "dormant room evicted");

    // The join after eviction must succeed against a respawned room.
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let self_presence = Element::from_str(&responses[0]).expect("self presence xml");
    assert_eq!(self_presence.name(), "presence");
    assert_ne!(
        self_presence.attr("type"),
        Some("error"),
        "join after eviction must not error: {responses:?}"
    );
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.find_nick_by_real_jid(&sender_jid), Some("alice"));
    assert_eq!(
        state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("count"),
        1,
        "the registry holds the respawned room"
    );
}

/// #1107 / XEP-0045 §7.6: a full JID already in the room under nick A
/// joining as nick B gets `<error type='cancel'><not-acceptable/>`
/// on the wire (nicknames are locked to identity) and no second
/// occupancy is created.
#[tokio::test]
async fn muc_join_under_second_nick_returns_not_acceptable() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "no-second-nick@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice-two",
        None,
        &Some(session),
    )
    .await;
    assert_eq!(responses.len(), 1, "one error presence: {responses:?}");
    let error_presence = Element::from_str(&responses[0]).expect("error presence xml");
    assert_eq!(error_presence.name(), "presence");
    assert_eq!(error_presence.attr("type"), Some("error"));
    let error = error_presence
        .get_child("error", "jabber:client")
        .expect("typed error element");
    assert_eq!(error.attr("type"), Some("cancel"));
    assert!(
        error
            .get_child("not-acceptable", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some(),
        "XEP-0045 §7.6 locked-nickname refusal uses <not-acceptable/>: {responses:?}"
    );

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.occupant_count(), 1, "no ghost occupancy");
    assert_eq!(room.find_nick_by_real_jid(&sender_jid), Some("alice"));
}

/// #1111 / XEP-0045 §7.2.9: joining a room that has reached its maximum
/// number of occupants returns `<presence type='error'>` with
/// `<error type='wait'><service-unavailable/></error>` from the
/// requested room-nick to the joiner — never an empty reply that
/// leaves the client stalled waiting for self-presence.
#[tokio::test]
async fn muc_join_full_room_returns_service_unavailable() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "full-room@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/web".parse().expect("bob jid");

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let room_actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    let mut config = room_actor
        .ask(GetSnapshot)
        .await
        .expect("room snapshot")
        .room
        .config;
    config.max_occupants = 1;
    room_actor
        .ask(UpdateConfig { config })
        .await
        .expect("room config update");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    assert_eq!(
        responses.len(),
        1,
        "exactly one error presence (never an empty reply): {responses:?}"
    );
    let error_presence = Element::from_str(&responses[0]).expect("error presence xml");
    assert_eq!(error_presence.name(), "presence");
    assert_eq!(error_presence.attr("type"), Some("error"));
    assert_eq!(
        error_presence.attr("from"),
        Some(format!("{room_jid}/bob").as_str()),
        "XEP-0045 §7.2.9: the error comes from the requested room-nick: {responses:?}"
    );
    assert_eq!(
        error_presence.attr("to"),
        Some(bob.to_string().as_str()),
        "the error is addressed to the joining full JID: {responses:?}"
    );
    assert!(
        error_presence
            .get_child("x", xmpp_parsers::ns::MUC)
            .is_some(),
        "join-failure presence echoes <x xmlns='http://jabber.org/protocol/muc'/>: {responses:?}"
    );
    let error = error_presence
        .get_child("error", waddle_xmpp::parser::ns::JABBER_CLIENT)
        .expect("typed error element");
    assert_eq!(
        error.attr("type"),
        Some("wait"),
        "XEP-0045 §7.2.9 max-users refusal uses error type='wait': {responses:?}"
    );
    assert_eq!(
        error.attr("by"),
        Some(room_jid.to_string().as_str()),
        "the erroring entity is the room bare JID: {responses:?}"
    );
    assert!(
        error
            .get_child("service-unavailable", waddle_xmpp::parser::ns::STANZAS)
            .is_some(),
        "XEP-0045 §7.2.9 max-users refusal uses <service-unavailable/>: {responses:?}"
    );

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.occupant_count(), 1, "the full room admits no one");
    assert_eq!(room.find_nick_by_real_jid(&bob), None);
}

/// #1134 / XEP-0045 §10.1.1: only the room creator receives Owner. The
/// second user to join an unmanaged room must not be granted Owner —
/// the created-bit comes from the registry, not from call-site
/// inference.
#[tokio::test]
async fn second_joiner_of_unmanaged_room_is_not_owner() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_server_owner_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "creator-owner@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");

    let alice_responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;
    let alice_presence = Element::from_str(&alice_responses[0]).expect("alice presence");
    let alice_item = alice_presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .and_then(|x| x.get_child("item", "http://jabber.org/protocol/muc#user"))
        .expect("alice muc item");
    assert_eq!(
        alice_item.attr("affiliation"),
        Some("owner"),
        "the creator gets Owner (XEP-0045 §10.1.1)"
    );

    let bob_responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    let bob_self = bob_responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|el| {
            el.name() == "presence"
                && el
                    .get_child("x", "http://jabber.org/protocol/muc#user")
                    .is_some_and(|x| {
                        x.children()
                            .any(|c| c.name() == "status" && c.attr("code") == Some("110"))
                    })
        })
        .expect("bob self presence");
    let bob_item = bob_self
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .and_then(|x| x.get_child("item", "http://jabber.org/protocol/muc#user"))
        .expect("bob muc item");
    assert_ne!(
        bob_item.attr("affiliation"),
        Some("owner"),
        "a later joiner is not the creator and must not be Owner (#1134)"
    );
}

#[tokio::test]
async fn muc_join_responses_use_client_namespace() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    assert_eq!(responses.len(), 2);

    let self_presence = Element::from_str(&responses[0]).expect("self presence xml");
    assert_eq!(self_presence.name(), "presence");
    assert_eq!(self_presence.ns(), waddle_xmpp::ns::JABBER_CLIENT);
    let user_x = self_presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");
    assert_eq!(item.attr("jid"), Some("alice@example.com/web"));
    assert_eq!(item.attr("affiliation"), Some("owner"));
    assert_eq!(item.attr("role"), Some("moderator"));
    assert!(user_x
        .children()
        .any(|child| child.name() == "status" && child.attr("code") == Some("100")));
    assert!(user_x
        .children()
        .any(|child| child.name() == "status" && child.attr("code") == Some("110")));
    assert!(
        self_presence
            .get_child("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
            .is_some(),
        "self-presence must carry XEP-0421 occupant-id"
    );

    let subject_message = Element::from_str(&responses[1]).expect("subject xml");
    assert_eq!(subject_message.name(), "message");
    assert_eq!(subject_message.ns(), waddle_xmpp::ns::JABBER_CLIENT);
    assert_eq!(subject_message.attr("type"), Some("groupchat"));
}

#[tokio::test]
async fn xep_0045_join_replay_exposes_existing_occupant_real_jids() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "icepuma").await;
    let room_jid: BareJid = "mentions@muc.example.com".parse().expect("room jid");

    let occupants = [
        ("icepuma", "icepuma@example.com/web"),
        ("randax", "randax@example.com/desktop"),
        ("rawkode", "rawkode@example.com/mobile"),
    ];

    for (index, (nick, full_jid)) in occupants.iter().enumerate() {
        let sender_jid: FullJid = full_jid.parse().expect("occupant jid");
        let session = if index == 0 {
            Some(owner_session.clone())
        } else {
            None
        };
        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &sender_jid,
            nick,
            None,
            &session,
        )
        .await;
    }

    let joiner: FullJid = "witness@example.com/browser".parse().expect("joiner jid");
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &joiner,
        "witness",
        None,
        &None,
    )
    .await;

    for (nick, full_jid) in occupants {
        let from = format!("{room_jid}/{nick}");
        let replay = responses
            .iter()
            .filter_map(|xml| Element::from_str(xml).ok())
            .find(|element| {
                element.name() == "presence"
                    && element.attr("from") == Some(from.as_str())
                    && element.attr("to") == Some(joiner.as_str())
            })
            .unwrap_or_else(|| panic!("missing replay presence for {nick}: {responses:?}"));
        let user_x = replay
            .get_child("x", "http://jabber.org/protocol/muc#user")
            .expect("muc user payload");
        let item = user_x
            .get_child("item", "http://jabber.org/protocol/muc#user")
            .expect("muc user item");

        assert_eq!(item.attr("jid"), Some(full_jid));
        assert!(
            user_x
                .children()
                .any(|child| child.name() == "status" && child.attr("code") == Some("100")),
            "non-anonymous replay must disclose status 100 for {nick}"
        );
        assert!(
            !user_x
                .children()
                .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
            "replay presence for another occupant must not be self-presence"
        );
        assert!(
            replay
                .get_child("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
                .is_some(),
            "replay presence for {nick} must carry XEP-0421 occupant-id"
        );
    }
}

#[tokio::test]
async fn managed_members_only_join_requires_explicit_channel_member_affiliation() {
    let state = create_test_websocket_state().await;
    let session = crate::auth::Session::new("alice@example.com", "alice", "alice");
    let room_jid: BareJid = "private-space@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "private-space".to_string(),
            name: "Private Space".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: true,
            public_room: false,
        },
    )
    .await
    .expect("channel upsert");

    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "private-space"),
                Relation::new("parent"),
                Subject::userset(crate::permissions::SubjectType::Space, "team", ""),
            ),
        })
        .await
        .expect("channel parent tuple");
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "team"),
                Relation::new("member"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("space member tuple");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "open-space".to_string(),
            name: "Open Space".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: false,
            public_room: true,
        },
    )
    .await
    .expect("open channel upsert");
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "open-space"),
                Relation::new("parent"),
                Subject::userset(crate::permissions::SubjectType::Space, "team", ""),
            ),
        })
        .await
        .expect("open channel parent tuple");
    let open_room_jid: BareJid = "open-space@muc.example.com".parse().expect("open room jid");
    let open_join = handle_muc_join(
        state.as_ref(),
        "example.com",
        &open_room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;
    assert!(
        open_join
            .first()
            .is_some_and(|frame| frame.contains("affiliation='none'")
                || frame.contains(r#"affiliation="none""#)),
        "inherited read access must not masquerade as persistent MUC membership in open rooms: {open_join:?}"
    );

    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;
    assert_eq!(denied.len(), 1);
    assert!(
        denied[0].contains("registration-required"),
        "members-only MUC admission must not treat inherited read access as membership: {denied:?}"
    );
    assert!(
        get_room_actor(state.as_ref(), &room_jid).await.is_none(),
        "denied members-only join must not create the room actor"
    );

    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "private-space"),
                Relation::new("member"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("channel member tuple");

    let admitted = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session),
    )
    .await;
    assert!(
        admitted
            .first()
            .is_some_and(|frame| frame.contains("affiliation='member'")
                || frame.contains(r#"affiliation="member""#)),
        "explicit channel member affiliation must admit the members-only join: {admitted:?}"
    );
}

#[tokio::test]
async fn managed_public_channel_allows_deployment_member_without_channel_tuple() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "project@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "project".to_string(),
            name: "Project".to_string(),
            description: None,
            channel_type: "text".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: false,
            public_room: true,
        },
    )
    .await
    .expect("channel upsert");
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                Relation::new("member"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("server member tuple");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session),
    )
    .await;

    let presence =
        Element::from_str(responses.first().expect("self-presence")).expect("presence XML");
    assert_eq!(presence.name(), "presence");
    assert_ne!(
        presence.attr("type"),
        Some("error"),
        "public/open managed channel must admit deployment members: {responses:?}"
    );
    let user_x = presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");
    assert_eq!(item.attr("affiliation"), Some("none"));
    assert!(
        user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
        "successful join must complete with XEP-0045 self-presence 110: {responses:?}"
    );
}

#[tokio::test]
async fn managed_join_uses_live_actor_members_only_config_over_stale_channel_row() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "chat@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "bob@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "chat".to_string(),
            name: "Chat".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: true,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: false,
            public_room: true,
        },
    )
    .await
    .expect("channel upsert");
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                Relation::new("member"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("server member tuple");

    let actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: true,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "chat".to_string(),
    )
    .await
    .expect("room actor")
    .actor_ref;

    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "bob",
        None,
        &Some(session),
    )
    .await;

    assert_eq!(denied.len(), 1);
    assert!(
        denied[0].contains("registration-required"),
        "live actor members-only config must override stale open channel row: {denied:?}"
    );
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert!(snapshot.find_nick_by_real_jid(&sender_jid).is_none());
}

/// Review F3: on the members-only path a `Resolver(None)` verdict makes
/// the handler return registration-required BEFORE any actor message,
/// so a stale resolver-derived Member inside a LIVE room actor (written
/// before the revocation) was never cleared — the revoked user stayed
/// on the room's affiliation list (admin queries, XEP-0045 §7.x member
/// lists) until eviction. The rejection must best-effort sync
/// `Affiliation::None` into the existing actor.
#[tokio::test]
async fn rejected_members_only_join_clears_stale_resolver_affiliation_in_live_actor() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "revoked@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "bob@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "revoked".to_string(),
            name: "Revoked".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: true,
            public_room: false,
        },
    )
    .await
    .expect("channel upsert");

    let actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: true,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "revoked".to_string(),
    )
    .await
    .expect("room actor")
    .actor_ref;

    // Stale resolver-derived Member from before the revocation.
    let seed_revision = snapshot_room(state.as_ref(), &room_jid)
        .await
        .admission_revision;
    let seeded = actor
        .ask(waddle_xmpp::muc::room_actor::SyncResolverAffiliation {
            jid: sender_jid.to_bare(),
            affiliation: waddle_xmpp::Affiliation::Member,
            expected_admission_revision: seed_revision,
        })
        .await
        .expect("seed stale resolver-derived member");
    assert_eq!(
        seeded,
        waddle_xmpp::muc::room_actor::ResolverAffiliationSyncOutcome::Applied,
        "seeding the stale member must apply"
    );

    // No permission tuples exist for bob: the resolver reports None.
    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "bob",
        None,
        &Some(session),
    )
    .await;
    assert_eq!(denied.len(), 1, "denied join: {denied:?}");
    assert!(
        denied[0].contains("registration-required"),
        "revoked user's members-only join must be refused: {denied:?}"
    );

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(
        room.get_affiliation(&sender_jid.to_bare()),
        waddle_xmpp::Affiliation::None,
        "the rejection must clear the stale resolver-derived Member from the live actor"
    );
}

#[tokio::test]
async fn standard_members_only_join_rejects_unaffiliated_user() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "standard-private@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    let mut config = actor.ask(GetConfig).await.expect("config");
    config.members_only = true;
    actor
        .ask(UpdateConfig { config })
        .await
        .expect("members-only config");

    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &None,
    )
    .await;

    assert_eq!(denied.len(), 1);
    assert!(
        denied[0].contains("registration-required"),
        "standard members-only MUC admission must reject unaffiliated users: {denied:?}"
    );
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(snapshot.occupant_count(), 1);
    assert!(snapshot.find_nick_by_real_jid(&bob_jid).is_none());
}

#[tokio::test]
async fn xep_0045_section_7_2_15_join_replay_serializes_full_subject_envelope() {
    // Boundary test for the WebSocket join wiring (Copilot review,
    // PR #319). Pre-populates `MucRoom.subject` with a multi-language
    // SubjectState via the production `SetSubject` actor message,
    // then drives a fresh join through `handle_muc_join` and asserts
    // the serialized subject message carries every conformance
    // element: `from='room/setter_nick'`, every persisted
    // `<subject xml:lang='...'>`, the XEP-0203 `<delay/>` from the
    // room JID, and the XEP-0421 `<occupant-id/>`.
    use chrono::TimeZone;
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let setter_session = create_test_server_owner_session(state.as_ref(), "setter").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let setter_jid: FullJid = "setter@example.com/web".parse().expect("setter jid");
    let joiner_jid: FullJid = "alice@example.com/web".parse().expect("joiner jid");

    // Bootstrap the room actor by joining the setter (first joiner
    // becomes Owner → Moderator), then seed the subject state with
    // a multi-language `texts` map matching what a real §8.1
    // dispatch would produce.
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &setter_jid,
        "setter-nick",
        None,
        &Some(setter_session),
    )
    .await;
    let room_actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    let texts = waddle_xmpp::muc::RoomSubjectTexts::from_iter([
        (String::new(), "Default subject".to_string()),
        ("en".to_string(), "English subject".to_string()),
    ]);
    let set_at = chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();
    room_actor
        .ask(SetSubject {
            texts,
            setter: setter_jid.to_bare(),
            setter_nick: "setter-nick".to_string(),
            set_at,
        })
        .await
        .expect("SetSubject succeeds");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &joiner_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    // 1 existing-occupant presence (setter) + self-presence + subject = 3.
    assert_eq!(
        responses.len(),
        3,
        "join responses: existing-occupants + self-presence + subject"
    );

    let subject_msg =
        Element::from_str(responses.last().expect("subject is last")).expect("subject xml");
    assert_eq!(subject_msg.name(), "message");
    assert_eq!(subject_msg.attr("type"), Some("groupchat"));
    assert_eq!(
        subject_msg.attr("from"),
        Some("channel@muc.example.com/setter-nick"),
        "§7.2.15 nick-form `from` for set room"
    );

    let subject_children: Vec<&Element> = subject_msg
        .children()
        .filter(|c| c.name() == "subject")
        .collect();
    assert_eq!(
        subject_children.len(),
        2,
        "every persisted xml:lang variant round-trips into the join replay"
    );
    // minidom 0.18 keys attributes by `(Namespace, NcName)` — `xml:lang`
    // lives in the XML namespace, not the default. Use `attr_ns` to read it.
    let lang_of = |c: &Element| {
        c.attr_ns(&minidom::rxml::Namespace::XML, "lang")
            .map(str::to_string)
    };
    let default_subject = subject_children
        .iter()
        .find(|c| lang_of(c).as_deref().map(|v| v.is_empty()).unwrap_or(true))
        .expect("default-language subject present");
    assert_eq!(default_subject.text(), "Default subject");
    let en_subject = subject_children
        .iter()
        .find(|c| lang_of(c).as_deref() == Some("en"))
        .expect("xml:lang=en subject present");
    assert_eq!(en_subject.text(), "English subject");

    let delay = subject_msg
        .get_child("delay", "urn:xmpp:delay")
        .expect("XEP-0203 <delay/> stamped per §7.2.15 SHOULD");
    assert_eq!(
        delay.attr("from"),
        Some("channel@muc.example.com"),
        "§7.2.15 conditional MUST: delay's `from` is the room JID"
    );
    assert!(
        delay.attr("stamp").is_some_and(|s| !s.is_empty()),
        "delay stamp present and non-empty"
    );

    let occupant_id = subject_msg
        .get_child("occupant-id", "urn:xmpp:occupant-id:0")
        .expect("XEP-0421 <occupant-id/> stamped on set-subject replay");
    assert!(
        occupant_id.attr("id").is_some_and(|s| !s.is_empty()),
        "occupant-id `id` attribute present"
    );

    assert!(
        subject_msg.children().all(|c| c.name() != "body"),
        "subject message MUST have no <body/>"
    );
}

#[test]
fn test_parse_room_jid_valid() {
    let jid: jid::BareJid = "channel456@muc.example.com".parse().unwrap();
    let (waddle, channel) = parse_room_jid_context(&jid);
    assert_eq!(waddle, "space");
    assert_eq!(channel, "channel456");
}

#[test]
fn test_parse_room_jid_fallback() {
    let jid: jid::BareJid = "singlename@muc.example.com".parse().unwrap();
    let (waddle, channel) = parse_room_jid_context(&jid);
    assert_eq!(waddle, "space");
    assert_eq!(channel, "singlename");
}

#[tokio::test]
async fn native_muc_admin_rejects_mixed_role_and_affiliation_set() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "admin-mixed@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "admin-mixed")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "alice")
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "none")
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@example.com",
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "outcast",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("bad-request"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_eq!(snapshot.occupant_count(), 1);
    assert_ne!(
        snapshot.get_affiliation(&alice_jid.to_bare()),
        Affiliation::Outcast
    );
}

#[tokio::test]
async fn native_muc_admin_last_owner_demotion_returns_conflict() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "admin-last-owner@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-last-owner",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@example.com",
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "member",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("conflict"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_eq!(
        snapshot.get_affiliation(&alice_jid.to_bare()),
        Affiliation::Owner
    );
}

#[tokio::test]
async fn native_muc_admin_admin_banning_owner_returns_not_allowed() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "admin-owner-ban@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol_bare: BareJid = "carol@example.com".parse().expect("carol jid");
    let ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ChangeAffiliation {
            jid: bob_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("bob admin affiliation");
    actor
        .ask(ChangeAffiliation {
            jid: carol_bare,
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("second owner affiliation");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-owner-ban",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@example.com",
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "outcast",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("not-allowed"));

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(
        snapshot.get_affiliation(&alice_jid.to_bare()),
        Affiliation::Owner
    );
    assert_eq!(snapshot.find_nick_by_real_jid(&alice_jid), Some("alice"));
}

#[tokio::test]
async fn native_muc_admin_admin_cannot_grant_admin_affiliation() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "admin-list-owner-only@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol_bare: BareJid = "carol@example.com".parse().expect("carol jid");
    let ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ChangeAffiliation {
            jid: bob_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("bob admin affiliation");
    actor
        .ask(ChangeAffiliation {
            jid: carol_bare.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("carol member affiliation");

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-grant-admin",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                carol_bare.to_string(),
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "admin",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("forbidden"));

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(snapshot.get_affiliation(&carol_bare), Affiliation::Member);
}

#[tokio::test]
async fn native_muc_admin_admin_cannot_kick_another_admin_role() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let carol_session = create_test_session(state.as_ref(), "carol").await;
    let room_jid: BareJid = "admin-role-admin-target@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol_jid: FullJid = "carol@example.com/web".parse().expect("carol jid");
    let ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ChangeAffiliation {
            jid: bob_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("bob admin affiliation");
    actor
        .ask(ChangeAffiliation {
            jid: carol_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("carol admin affiliation");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &carol_jid,
        "carol",
        None,
        &Some(carol_session),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-role-admin-target",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "carol")
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "none")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("not-allowed"));

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    let carol = snapshot.get_occupant("carol").expect("carol occupant");
    assert_eq!(carol.role, waddle_xmpp::Role::Moderator);
    assert_eq!(snapshot.find_nick_by_real_jid(&carol_jid), Some("carol"));
}

#[tokio::test]
async fn native_muc_admin_membership_grant_allows_optional_nick() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "admin-member-nick@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let carol_bare: BareJid = "carol@example.com".parse().expect("carol jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-member-nick",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                carol_bare.to_string(),
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "member",
                            )
                            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "thirdwitch")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='result'"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_eq!(snapshot.get_affiliation(&carol_bare), Affiliation::Member);
}

#[tokio::test]
async fn native_muc_admin_get_role_list_accepts_role_without_nick() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "admin-role-list@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ApplyAdminItems {
            sender_jid: alice_jid.clone(),
            sender_affiliation: Affiliation::Owner,
            sender_role: waddle_xmpp::Role::Moderator,
            items: vec![waddle_xmpp::muc::AdminItem {
                jid: None,
                nick: Some("bob".to_string()),
                affiliation: None,
                role: Some(waddle_xmpp::Role::Moderator),
                reason: None,
            }],
        })
        .await
        .expect("owner grants temporary moderator role");

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-role-list",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "moderator")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='result'"));
    assert!(responses[0].contains("nick='bob'"));
    assert!(responses[0].contains("role='moderator'"));
    assert!(responses[0].contains("affiliation='none'"));
    assert!(responses[0].contains("jid='bob@example.com/web'"));
}

#[tokio::test]
async fn native_muc_admin_role_set_allows_jid_echo_without_affiliation() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "admin-role-jid-echo@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-role-jid-echo",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                bob_jid.to_bare().to_string(),
                            )
                            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "bob")
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "visitor")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='result'"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    let bob = snapshot.get_occupant("bob").expect("bob occupant");
    assert_eq!(bob.role, waddle_xmpp::Role::Visitor);
}

#[tokio::test]
async fn native_muc_admin_role_moderator_cannot_grant_moderator_role() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let carol_session = create_test_session(state.as_ref(), "carol").await;
    let room_jid: BareJid = "admin-role-moderator-grant@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol_jid: FullJid = "carol@example.com/web".parse().expect("carol jid");
    let ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &carol_jid,
        "carol",
        None,
        &Some(carol_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ApplyAdminItems {
            sender_jid: alice_jid.clone(),
            sender_affiliation: Affiliation::Owner,
            sender_role: waddle_xmpp::Role::Moderator,
            items: vec![waddle_xmpp::muc::AdminItem {
                jid: None,
                nick: Some("bob".to_string()),
                affiliation: None,
                role: Some(waddle_xmpp::Role::Moderator),
                reason: None,
            }],
        })
        .await
        .expect("owner grants temporary moderator role");

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "moderator-grant-role",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "carol")
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "moderator")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("forbidden"));

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    let carol = snapshot.get_occupant("carol").expect("carol occupant");
    assert_eq!(carol.role, waddle_xmpp::Role::Participant);
}

#[tokio::test]
async fn native_muc_admin_role_moderator_cannot_write_affiliations_durably() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "moderator-durable@muc.example.com"
        .parse()
        .expect("room jid");
    let channel_id = waddle_xmpp::parse_managed_room_jid(&room_jid).expect("managed channel id");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol_bare: BareJid = "carol@example.com".parse().expect("carol jid");
    let ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ApplyAdminItems {
            sender_jid: alice_jid.clone(),
            sender_affiliation: Affiliation::Owner,
            sender_role: waddle_xmpp::Role::Moderator,
            items: vec![waddle_xmpp::muc::AdminItem {
                jid: None,
                nick: Some("bob".to_string()),
                affiliation: None,
                role: Some(waddle_xmpp::Role::Moderator),
                reason: None,
            }],
        })
        .await
        .expect("owner grants temporary moderator role");

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "moderator-member-write",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                carol_bare.to_string(),
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "member",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("forbidden"));

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(snapshot.get_affiliation(&carol_bare), Affiliation::None);

    let durable_membership = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(carol_bare.to_string()),
            permission: Permission::Member,
            object: Object::new(ObjectType::Channel, channel_id),
        })
        .await
        .expect("permission check");
    assert!(
        !durable_membership.allowed,
        "role-only moderators must not persist channel membership"
    );
}

#[tokio::test]
async fn native_muc_admin_self_ban_returns_conflict() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "admin-self-ban@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-self-ban",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@example.com",
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "outcast",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("conflict"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_ne!(
        snapshot.get_affiliation(&alice_jid.to_bare()),
        Affiliation::Outcast
    );
    assert_eq!(snapshot.occupant_count(), 1);
}

#[tokio::test]
async fn native_muc_admin_incomplete_affiliation_item_returns_bad_request() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "admin-incomplete@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-incomplete",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "bob@example.com",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("bad-request"));
}

#[tokio::test]
async fn standard_muc_owner_config_persists_room_and_enforces_nonanonymous_defaults() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;

    let room_jid: BareJid = "project@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_roomname",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("Project Room")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_persistentroom",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("0")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_whois",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("moderators")
                        .build(),
                )
                .build(),
        )
        .build();
    let owner_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "owner-submit")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                    .append(submit_form)
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
    assert!(responses[0].contains("type='result'"));

    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let channel = crate::server::xmpp_state::get_xmpp_channel(actor, "project")
        .await
        .expect("channel lookup")
        .expect("persisted channel");
    assert_eq!(channel.name, "Project Room");

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert!(snapshot.config.persistent);

    let disco = disco_items_iq_frame("muc-items", "muc.example.com", None);
    let responses = handle_iq(
        &disco,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    assert!(responses[0].contains("project@muc.example.com"));
}

#[tokio::test]
async fn standard_muc_owner_get_returns_config_without_persisting_room() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;

    let room_jid: BareJid = "config-get@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let owner_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "owner-get")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER).build())
            .build(),
    );

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "owner get response: {responses:?}");
    assert!(responses[0].contains("type='result'"));
    assert!(responses[0].contains("muc#roomconfig_roomname"));

    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let channel = crate::server::xmpp_state::get_xmpp_channel(actor, "config-get")
        .await
        .expect("channel lookup");
    assert!(channel.is_none());
}

#[tokio::test]
async fn standard_muc_owner_config_rejects_non_owner() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;

    let room_jid: BareJid = "locked@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/web", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");
    let bob_ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_roomname",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("Hacked")
                        .build(),
                )
                .build(),
        )
        .build();
    let owner_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "owner-submit")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                    .append(submit_form)
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session.clone()),
        &bob_ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("forbidden"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_ne!(snapshot.config.name, "Hacked");

    let room_actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    room_actor
        .ask(ChangeAffiliation {
            jid: bob_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("set admin affiliation");

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &bob_ready,
    )
    .await;
    assert_eq!(
        responses.len(),
        1,
        "admin owner config response: {responses:?}"
    );
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("forbidden"));
}

#[tokio::test]
async fn room_disco_info_advertises_parent_space_metadata_for_linked_channel() {
    let state = create_test_websocket_state().await;
    let space_db = state.deps.app_state.db_pool.global();
    let conn = space_db.guard().await.expect("persistent connection");
    conn.execute(
            "INSERT INTO channels (id, name, description, channel_type, position, is_default) VALUES (?, ?, ?, 'text', 0, 0)",
            crate::db_params!["linked", "Linked", "Linked channel description"],
        )
        .await
        .expect("insert channel");
    drop(conn);
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");
    let channel = waddle_xmpp::ChannelInfo {
        id: "linked".to_string(),
        name: "Linked".to_string(),
        channel_type: "text".to_string(),
    };
    let item =
        waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com").expect("bookmark item");
    state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, "team", &item, None, false)
        .await
        .expect("publish bookmark");

    let query = disco_info_iq_frame("room-info", "linked@muc.example.com", None);
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let response = responses.first().expect("room disco response");
    assert!(response.contains("muc_nonanonymous"));
    assert!(response.contains("urn:xmpp:spaces:0"));
    assert!(response.contains("var='parent'"));
    assert!(response.contains("xmpp:spaces.example.com?;node=team"));
    assert!(response.contains("http://jabber.org/protocol/muc#roominfo"));
    assert!(response.contains("muc#roomconfig_pubsub"));
    assert!(response.contains("muc#roominfo_description"));
    assert!(response.contains("Linked channel description"));
}

#[tokio::test]
async fn active_room_disco_preserves_managed_announcement_channel_type() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("persistent connection");
    conn.execute(
            "INSERT INTO channels (id, name, description, channel_type, position, is_default, members_only, public_room) VALUES (?, ?, ?, 'announcement', 0, 0, 0, 1)",
            crate::db_params!["announcements", "Announcements", "Owner-posted announcements"],
        )
        .await
        .expect("insert announcement channel");
    drop(conn);

    let room_jid: BareJid = "announcements@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let query = disco_info_iq_frame("announcement-info", "announcements@muc.example.com", None);
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&alice_jid),
    )
    .await;
    let response = responses.first().expect("room disco response");
    assert!(response.contains("muc_moderated"), "response: {response}");
    assert!(response.contains("muc_open"), "response: {response}");
    assert!(
        !response.contains("muc_membersonly"),
        "response: {response}"
    );
    assert!(response.contains("muc_public"), "response: {response}");
    assert!(
        response.contains("waddle#channel_type"),
        "response: {response}"
    );
    assert!(response.contains("announcement"), "response: {response}");
    assert!(
        !response.contains("<value>text</value>"),
        "announcement room must not be reported as text: {response}"
    );
}

#[tokio::test]
async fn muc_owner_config_cannot_unmoderate_managed_announcement_channel() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("persistent connection");
    conn.execute(
        "INSERT INTO channels (id, name, description, channel_type, position, is_default, members_only, public_room) VALUES (?, ?, ?, 'announcement', 0, 0, 0, 1)",
        crate::db_params![
            "ops-announcements",
            "Ops Announcements",
            "Owner-posted operational updates"
        ],
    )
    .await
    .expect("insert announcement channel");
    drop(conn);

    let room_jid: BareJid = "ops-announcements@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/web", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;

    let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_moderatedroom",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("0")
                        .build(),
                )
                .build(),
        )
        .build();
    let owner_set = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "announcement-owner-submit",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                    .append(submit_form)
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &owner_set,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session.clone()),
        &ready_phase(&alice_jid),
    )
    .await;
    assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
    assert!(responses[0].contains("type='result'"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert!(
        snapshot.config.moderated,
        "announcement owner config must keep the live room moderated"
    );
    assert!(
        !snapshot.config.forum,
        "announcement owner config must not drift into forum mode"
    );

    let owner_get = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "announcement-owner-get",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER).build())
            .build(),
    );
    let responses = handle_iq(
        &owner_get,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready_phase(&alice_jid),
    )
    .await;
    assert_eq!(responses.len(), 1, "owner get response: {responses:?}");
    let response =
        Element::from_str(responses.first().expect("owner get response")).expect("owner get XML");
    let form = response
        .get_child("query", waddle_xmpp::muc::NS_MUC_OWNER)
        .and_then(|query| query.get_child("x", waddle_xmpp::muc::DATA_FORMS_NS))
        .expect("owner config form");
    let moderated_field = form
        .children()
        .find(|field| {
            field.name() == "field"
                && field.ns() == waddle_xmpp::muc::DATA_FORMS_NS
                && field.attr("var") == Some("muc#roomconfig_moderatedroom")
        })
        .expect("moderated field");
    let moderated_value = moderated_field
        .get_child("value", waddle_xmpp::muc::DATA_FORMS_NS)
        .map(|value| value.texts().collect::<String>())
        .expect("moderated value");
    assert_eq!(
        moderated_value, "1",
        "owner config GET must project managed announcements as moderated"
    );

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;
    let bob = snapshot_room(state.as_ref(), &room_jid)
        .await
        .room
        .get_occupant("bob")
        .expect("bob occupant")
        .clone();
    assert_eq!(bob.role, waddle_xmpp::Role::Visitor);

    let message = format!(
        "<message xmlns='jabber:client' to='{room_jid}' type='groupchat' id='bob-announcement-post'>\
            <body>not an owner</body>\
        </message>"
    );
    let responses = handle_message_for_test(
        state.as_ref(),
        &bob_jid,
        Some(&bob_session),
        parse_message_for_test(&message),
    )
    .await;
    assert_eq!(responses.len(), 1, "message response: {responses:?}");
    assert!(
        responses[0].contains("type='error'") && responses[0].contains("forbidden"),
        "announcement visitor post must be rejected: {}",
        responses[0]
    );
}

#[tokio::test]
async fn managed_space_bookmark_join_repairs_missing_parent_tuple() {
    let state = create_test_websocket_state().await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("db connection");
    conn.execute(
        "INSERT INTO channels (id, name, description, channel_type, position, is_default, members_only, public_room) VALUES (?, ?, ?, 'text', 0, 0, 0, 1)",
        crate::db_params!["legacy", "Legacy", "Legacy channel description"],
    )
    .await
    .expect("insert channel");
    drop(conn);

    let spaces_jid = state.deps.app_state.spaces_jid.clone();
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "alpha")
        .await
        .expect("space node");
    let channel = waddle_xmpp::ChannelInfo {
        id: "legacy".to_string(),
        name: "Legacy".to_string(),
        channel_type: "text".to_string(),
    };
    let item =
        waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com").expect("bookmark item");
    state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, "alpha", &item, None, false)
        .await
        .expect("publish bookmark");

    let viewer = create_test_session(state.as_ref(), "viewer").await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "alpha"),
                Relation::new("member"),
                Subject::user(&viewer.user_jid),
            ),
        })
        .await
        .expect("space member tuple");
    let allowed_before = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&viewer.user_jid),
            permission: Permission::Read,
            object: Object::new(ObjectType::Channel, "legacy"),
        })
        .await
        .expect("permission actor")
        .allowed;
    assert!(
        !allowed_before,
        "test setup should start without the channel parent tuple"
    );

    let room_jid: BareJid = "legacy@muc.example.com".parse().expect("room jid");
    let viewer_jid: FullJid = format!("{}@example.com/web", viewer.xmpp_localpart)
        .parse()
        .expect("viewer jid");
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &viewer_jid,
        "viewer",
        None,
        &Some(viewer.clone()),
    )
    .await;

    let presence = Element::from_str(responses.first().expect("self-presence response"))
        .expect("presence XML");
    assert_eq!(presence.name(), "presence");
    assert_ne!(
        presence.attr("type"),
        Some("error"),
        "managed Space bookmark join should not be rejected: {responses:?}"
    );
    let user_x = presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");
    assert_eq!(
        item.attr("affiliation"),
        Some("none"),
        "repaired Space read access must not become persistent MUC membership: {responses:?}"
    );
    assert!(
        user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
        "successful join must complete with XEP-0045 self-presence 110: {responses:?}"
    );

    let allowed_after = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&viewer.user_jid),
            permission: Permission::Read,
            object: Object::new(ObjectType::Channel, "legacy"),
        })
        .await
        .expect("permission actor")
        .allowed;
    assert!(
        allowed_after,
        "join should repair the missing channel parent tuple"
    );
}

#[tokio::test]
async fn managed_space_bookmark_join_repairs_only_projected_parent_tuple() {
    let state = create_test_websocket_state().await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("db connection");
    conn.execute(
        "INSERT INTO channels (id, name, description, channel_type, position, is_default, members_only, public_room) VALUES (?, ?, ?, 'text', 0, 0, 1, 0)",
        crate::db_params!["projected", "Projected", "Projected channel description"],
    )
    .await
    .expect("insert channel");
    drop(conn);

    let spaces_jid = state.deps.app_state.spaces_jid.clone();
    for node in ["alpha", "beta"] {
        state
            .deps
            .protocol
            .pubsub_storage
            .get_or_create_node(&spaces_jid, node)
            .await
            .expect("space node");
    }
    let channel = waddle_xmpp::ChannelInfo {
        id: "projected".to_string(),
        name: "Projected".to_string(),
        channel_type: "text".to_string(),
    };
    let item =
        waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com").expect("bookmark item");
    for node in ["alpha", "beta"] {
        state
            .deps
            .protocol
            .pubsub_storage
            .publish_item(&spaces_jid, node, &item, None, false)
            .await
            .expect("publish bookmark");
    }

    let room_jid: BareJid = "projected@muc.example.com".parse().expect("room jid");
    state
        .deps
        .app_state
        .channel_space_link_store
        .set(&crate::channel_space_links::ChannelSpaceLink {
            channel_jid: room_jid.clone(),
            space_jid: format!("beta@{}", spaces_jid.domain())
                .parse()
                .expect("space jid"),
            space_node: crate::space_identity::SpaceNode::from("beta"),
            created_at: 0,
        })
        .await
        .expect("channel-space link");

    let stale_viewer = create_test_session(state.as_ref(), "stale-viewer").await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "alpha"),
                Relation::new("member"),
                Subject::user(&stale_viewer.user_jid),
            ),
        })
        .await
        .expect("stale space member tuple");
    let current_viewer = create_test_session(state.as_ref(), "current-viewer").await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "beta"),
                Relation::new("member"),
                Subject::user(&current_viewer.user_jid),
            ),
        })
        .await
        .expect("current space member tuple");

    let stale_jid: FullJid = format!("{}@example.com/web", stale_viewer.xmpp_localpart)
        .parse()
        .expect("stale viewer jid");
    let stale_responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &stale_jid,
        "stale-viewer",
        None,
        &Some(stale_viewer.clone()),
    )
    .await;
    let stale_presence = Element::from_str(stale_responses.first().expect("stale join response"))
        .expect("presence XML");
    assert_eq!(
        stale_presence.attr("type"),
        Some("error"),
        "stale Space member must remain denied: {stale_responses:?}"
    );
    assert!(
        !state
            .deps
            .app_state
            .permission_actor
            .ask(CheckPermission {
                subject: Subject::user(&stale_viewer.user_jid),
                permission: Permission::Read,
                object: Object::new(ObjectType::Channel, "projected"),
            })
            .await
            .expect("permission actor")
            .allowed,
        "join must not restore the stale alpha parent tuple"
    );
    assert!(
        state
            .deps
            .app_state
            .permission_actor
            .ask(CheckPermission {
                subject: Subject::user(&current_viewer.user_jid),
                permission: Permission::Read,
                object: Object::new(ObjectType::Channel, "projected"),
            })
            .await
            .expect("permission actor")
            .allowed,
        "join should repair only the projected beta parent tuple"
    );
}

#[tokio::test]
async fn muc_self_rejoin_does_not_emit_ghost_presence() {
    // Same user joins the same nick twice from different resources —
    // the second join must NOT include a presence for the old resource
    // in the response (which used to be seen as a "ghost" occupant and
    // broke self-presence detection on the client).
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "rejoin-channel@muc.example.com".parse().expect("room");
    let first: FullJid = "alice@example.com/tab-1".parse().expect("first");
    let second: FullJid = "alice@example.com/tab-2".parse().expect("second");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &first,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &second,
        "alice",
        None,
        &None,
    )
    .await;

    // Count presences emitted to the joiner that came from room/alice.
    // Only the self-presence (status 110) should be there — no "ghost".
    let alice_presences_for_joiner = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .filter(|el| el.name() == "presence")
        .filter(|el| el.attr("from") == Some(&format!("{room_jid}/alice")))
        .filter(|el| el.attr("to") == Some(&second.to_string()))
        .count();
    assert_eq!(
        alice_presences_for_joiner, 1,
        "self-rejoin must produce exactly one self-presence, not a ghost + self pair"
    );

    // And the one presence we got must carry status 110.
    let self_presence = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|el| {
            el.name() == "presence"
                && el.attr("from") == Some(&format!("{room_jid}/alice"))
                && el.attr("to") == Some(&second.to_string())
        })
        .expect("self-presence must be present");
    let user_x = self_presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    assert!(
        user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
        "status 110 must be present on self-rejoin"
    );
}

#[tokio::test]
async fn muc_join_broadcast_includes_real_occupant_jid() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "public-channel@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let bob: FullJid = "bob@example.com/phone".parse().expect("bob");

    let (alice_tx, mut alice_rx) = mpsc::channel::<OutboundStanza>(4);
    state
        .deps
        .protocol
        .connection_registry
        .register(alice.clone(), alice_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;

    let broadcast = alice_rx.try_recv().expect("bob join broadcast to alice");
    let broadcast_xml = stanza_to_xml(&broadcast.stanza);
    let presence = Element::from_str(&broadcast_xml).expect("broadcast presence XML");
    let user_x = presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");

    let expected_from = format!("{room_jid}/bob");
    let expected_to = alice.to_string();
    assert_eq!(presence.attr("from"), Some(expected_from.as_str()));
    assert_eq!(presence.attr("to"), Some(expected_to.as_str()));
    assert_eq!(item.attr("jid"), Some("bob@example.com/phone"));
    assert_eq!(item.attr("affiliation"), Some("none"));
    assert_eq!(item.attr("role"), Some("participant"));
    assert!(
        user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("100")),
        "non-anonymous room presence must advertise status 100: {broadcast_xml}"
    );
}

#[tokio::test]
async fn muc_nick_collision_returns_conflict_presence() {
    // Two different users try to hold the same nick — second gets a
    // <presence type='error'/> with <conflict/>, and room state for
    // the incumbent is untouched.
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "conflict-channel@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/desktop".parse().expect("alice");
    let bob: FullJid = "bob@example.com/phone".parse().expect("bob");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "dino",
        None,
        &Some(owner_session),
    )
    .await;
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "dino",
        None,
        &None,
    )
    .await;

    assert_eq!(responses.len(), 1, "exactly one error presence");
    let el = Element::from_str(&responses[0]).expect("valid XML");
    assert_eq!(el.name(), "presence");
    assert_eq!(el.attr("type"), Some("error"));
    let bob_str = bob.to_string();
    assert_eq!(el.attr("to"), Some(bob_str.as_str()));
    let err = el
        .get_child("error", waddle_xmpp::ns::JABBER_CLIENT)
        .expect("error element");
    assert_eq!(err.attr("type"), Some("cancel"));
    assert!(err
        .get_child("conflict", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());

    // Alice still owns the nick.
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.find_nick_by_real_jid(&alice), Some("dino"));
    assert!(room.find_nick_by_real_jid(&bob).is_none());
    assert_eq!(room.occupant_count(), 1);
}

#[tokio::test]
async fn cleanup_muc_presence_broadcasts_unavailable_to_remaining_occupants() {
    // Regression for the "1 in call" ghost: when a user's connection
    // drops uncleanly (tab close, SM-expiry, panic-shed), the cleanup
    // path used to remove them from the room actor but never tell
    // the remaining occupants. Other clients then kept the leaver's
    // nick in their `$mucCallParticipants[room]` indefinitely, so
    // the "N in call" chip stayed lit with nobody actually present.
    //
    // The fix routes both the explicit-leave path AND the unclean
    // disconnect path through `broadcast_muc_leave_to_remaining`, so
    // remaining occupants receive a `<presence type='unavailable'/>`
    // either way. This test pins that contract.
    use super::cleanup::cleanup_muc_presence_for_jid;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "ghost-call-channel@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    // Register Bob's outbound channel so we can capture the broadcast
    // his client would receive.
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(4);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    // Both Alice and Bob join the MUC.
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;

    // Drain the join broadcast (Alice's room → Bob's mailbox is the
    // self-presence on join; Bob's room → Alice's mailbox would be
    // the cross-broadcast Alice would receive, but we registered only
    // Bob so we just need to drain his existing traffic).
    while bob_rx.try_recv().is_ok() {}

    // Simulate Alice's unclean disconnect — same entry point the SM
    // janitor and `cleanup_connection_shutdown` reach.
    cleanup_muc_presence_for_jid(state.as_ref(), &alice).await;

    // The room actor must have evicted Alice's occupant slot.
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.find_nick_by_real_jid(&alice).is_none(),
        "Alice's occupant slot must be cleared on unclean disconnect",
    );
    assert_eq!(room.find_nick_by_real_jid(&bob), Some("bob"));

    // The fix: Bob receives an `unavailable` presence from
    // `room/alice` so his client can drop Alice from any
    // call-participants set keyed by nick.
    let broadcast = bob_rx
        .try_recv()
        .expect("Bob must receive Alice's unavailable broadcast on her unclean disconnect");
    let xml = stanza_to_xml(&broadcast.stanza);
    let presence = Element::from_str(&xml).expect("broadcast presence XML");
    assert_eq!(presence.name(), "presence");
    assert_eq!(presence.attr("type"), Some("unavailable"));
    let expected_from = format!("{room_jid}/alice");
    let expected_to = bob.to_string();
    assert_eq!(presence.attr("from"), Some(expected_from.as_str()));
    assert_eq!(presence.attr("to"), Some(expected_to.as_str()));
}

fn active_muji() -> waddle_xmpp::xep::xep0272::Muji {
    use waddle_xmpp::xep::xep0167::MediaKind;
    use waddle_xmpp::xep::xep0272::{Creator, Muji, MujiContent};
    Muji::with_contents(vec![MujiContent::new(
        "audio",
        Creator::Initiator,
        MediaKind::Audio,
    )])
}

fn preparing_muji() -> waddle_xmpp::xep::xep0272::Muji {
    waddle_xmpp::xep::xep0272::Muji::preparing()
}

fn empty_muji() -> waddle_xmpp::xep::xep0272::Muji {
    waddle_xmpp::xep::xep0272::Muji::default()
}

#[derive(Default)]
struct RecordingSfu {
    calls: std::sync::Mutex<Vec<(waddle_sfu::CallId, waddle_sfu::Identity)>>,
    note_calls: std::sync::Mutex<Vec<(waddle_sfu::CallId, waddle_sfu::Identity)>>,
}

impl RecordingSfu {
    fn snapshot(&self) -> Vec<(waddle_sfu::CallId, waddle_sfu::Identity)> {
        self.calls.lock().expect("recording lock").clone()
    }
}

impl waddle_sfu::SfuService for RecordingSfu {
    fn issue_join_token(
        &self,
        _: &waddle_sfu::CallId,
        _: &waddle_sfu::Identity,
        _: waddle_sfu::MediaCapabilities,
    ) -> Result<waddle_sfu::JoinToken, waddle_sfu::SfuError> {
        unimplemented!("not exercised by this test")
    }

    fn issue_turn_credentials(
        &self,
        _: &waddle_sfu::Identity,
    ) -> Result<waddle_sfu::TurnCredential, waddle_sfu::SfuError> {
        unimplemented!("not exercised by this test")
    }

    fn register_call_participant(&self, _: &waddle_sfu::CallId, _: &waddle_sfu::Identity) {}

    fn has_call_participant(&self, _: &waddle_sfu::CallId, _: &waddle_sfu::Identity) -> bool {
        false
    }

    fn unregister_call_participant(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
    ) -> waddle_sfu::CallState {
        self.calls
            .lock()
            .expect("recording lock")
            .push((call_id.clone(), identity.clone()));
        waddle_sfu::CallState::Ended
    }

    fn note_participant_left(&self, call_id: &waddle_sfu::CallId, identity: &waddle_sfu::Identity) {
        // Recorded into `note_calls`, NOT `calls`: the trait splits
        // admin-evict from bookkeeping-only dispatch, and tests need
        // to distinguish which path was taken.
        self.note_calls
            .lock()
            .expect("recording lock")
            .push((call_id.clone(), identity.clone()));
    }

    fn is_revoked(&self, _: &waddle_sfu::Jti) -> bool {
        false
    }

    fn ws_url(&self) -> &waddle_sfu::WebsocketUrl {
        unimplemented!("not exercised by this test")
    }

    fn turn_host(&self) -> &waddle_sfu::TurnHost {
        unimplemented!("not exercised by this test")
    }

    fn webhook_secret(&self) -> &waddle_sfu::ApiSecret {
        unimplemented!("not exercised by this test")
    }

    fn participants_for_call(&self, _: &waddle_sfu::CallId) -> Vec<waddle_sfu::Identity> {
        Vec::new()
    }
}

async fn state_with_recording_sfu(
    recorder: std::sync::Arc<RecordingSfu>,
) -> std::sync::Arc<WebSocketState> {
    let base = create_test_websocket_state().await;
    let mut state = match std::sync::Arc::try_unwrap(base) {
        Ok(state) => state,
        Err(_) => panic!("test websocket state should be uniquely owned"),
    };
    let sfu: std::sync::Arc<dyn waddle_sfu::SfuService> = recorder;
    state.deps.protocol.sfu = Some(sfu);
    std::sync::Arc::new(state)
}

fn muc_user_item_jid(element: &Element) -> Option<String> {
    element
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .and_then(|x| x.get_child("item", "http://jabber.org/protocol/muc#user"))
        .and_then(|item| item.attr("jid"))
        .map(ToOwned::to_owned)
}

fn muc_presence_to(room_jid: &BareJid, nick: &str) -> xmpp_parsers::presence::Presence {
    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.to = Some(
        room_jid
            .clone()
            .with_resource_str(nick)
            .expect("valid room nick")
            .into(),
    );
    presence
}

fn muc_join_presence_to(room_jid: &BareJid, nick: &str) -> xmpp_parsers::presence::Presence {
    let mut presence = muc_presence_to(room_jid, nick);
    presence
        .payloads
        .push(Element::builder("x", waddle_xmpp::muc::presence::NS_MUC).build());
    presence
}

#[tokio::test]
async fn available_presence_without_muji_clears_existing_muji_state() {
    let recorder = std::sync::Arc::new(RecordingSfu::default());
    let state = state_with_recording_sfu(std::sync::Arc::clone(&recorder)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-clear-channel@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let plain_available = muc_presence_to(&room_jid, "alice");
    let responses = handlers::presence::handle_presence(
        plain_available,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &None,
        None,
    )
    .await;
    let recorded = recorder.snapshot();
    assert_eq!(
        recorded.len(),
        1,
        "Muji clear must unregister the sender from the SFU"
    );
    assert_eq!(recorded[0].0.as_str(), "muji-clear-channel@muc.example.com");
    assert_eq!(recorded[0].1.as_livekit_identity(), "alice@example.com/web");

    assert_eq!(responses.len(), 1, "sender receives reflected clear");
    let self_clear = Element::from_str(&responses[0]).expect("self clear XML");
    assert_eq!(
        self_clear.attr("from"),
        Some(format!("{room_jid}/alice").as_str())
    );
    assert!(
        self_clear
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_none(),
        "self reflection must omit <muji/> after clear"
    );

    let broadcast = bob_rx
        .try_recv()
        .expect("Bob must receive canonical non-Muji presence");
    let xml = stanza_to_xml(&broadcast.stanza);
    let presence = Element::from_str(&xml).expect("broadcast presence XML");
    assert_eq!(presence.name(), "presence");
    assert_eq!(presence.attr("type"), None);
    assert_eq!(
        presence.attr("from"),
        Some(format!("{room_jid}/alice").as_str())
    );
    assert_eq!(presence.attr("to"), Some(bob.to_string().as_str()));
    assert!(
        presence
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_none(),
        "clear broadcast must not carry <muji/>"
    );

    let carol: FullJid = "carol@example.com/web".parse().expect("carol jid");
    let replay = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &carol,
        "carol",
        None,
        &None,
    )
    .await;
    let alice_replay = replay
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| element.attr("from") == Some(format!("{room_jid}/alice").as_str()))
        .expect("carol receives alice replay");
    assert!(
        alice_replay
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_none(),
        "late join replay must not include stale Muji"
    );
}

#[tokio::test]
async fn empty_muji_presence_unregisters_the_sfu_participant() {
    let recorder = std::sync::Arc::new(RecordingSfu::default());
    let state = state_with_recording_sfu(std::sync::Arc::clone(&recorder)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "empty-muji-clear@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);

    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session.clone()),
        None,
    )
    .await;

    let mut empty = muc_presence_to(&room_jid, "alice");
    empty.payloads.push(empty_muji().to_element());
    let responses = handlers::presence::handle_presence(
        empty,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    let recorded = recorder.snapshot();
    assert_eq!(
        recorded.len(),
        1,
        "empty <muji/> leave marker must unregister the sender from the SFU"
    );
    assert_eq!(recorded[0].0.as_str(), "empty-muji-clear@muc.example.com");
    assert_eq!(recorded[0].1.as_livekit_identity(), "alice@example.com/web");
    let self_clear = Element::from_str(&responses[0]).expect("self clear XML");
    assert!(
        self_clear
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_none(),
        "empty <muji/> must reflect as a no-Muji leave marker"
    );
}

#[tokio::test]
async fn empty_muji_presence_ends_the_active_call_thread() {
    let recorder = std::sync::Arc::new(RecordingSfu::default());
    let state = state_with_recording_sfu(std::sync::Arc::clone(&recorder)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "presence-clear-ended@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);

    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session.clone()),
        None,
    )
    .await;
    // Anchor the call thread in the inbox under the same thread id the
    // active-call registration carries, so the ended summary can be
    // correlated back to this exact thread's row.
    let thread_id = "call-thread-uuid";
    state
        .deps
        .protocol
        .inbox_storage
        .upsert(
            &alice.to_bare(),
            waddle_xmpp::inbox::InboxEntry::new(
                room_jid.clone(),
                waddle_xmpp::inbox::ConversationKind::MucRoom,
                "anchor-stanza",
                1_700_000_000,
            )
            .with_thread(thread_id)
            .with_call_thread(
                waddle_xmpp::xep::CallThreadKind::Muc,
                waddle_xmpp::xep::CallThreadMedia {
                    audio: true,
                    video: false,
                },
            ),
            true,
        )
        .await
        .expect("seed call-thread anchor inbox row");

    state.deps.protocol.call_threads.insert(
        room_jid.clone(),
        crate::server::routes::websocket::ActiveCallThread {
            anchor_origin_id: "anchor-origin-id".to_owned(),
            initiator: alice.to_bare(),
            media: waddle_xmpp::xep::CallThreadMedia::audio_only(),
            started: chrono::Utc::now() - chrono::Duration::minutes(5),
            thread_id: thread_id.to_owned(),
        },
    );

    let mut empty = muc_presence_to(&room_jid, "alice");
    empty.payloads.push(empty_muji().to_element());
    let _ = handlers::presence::handle_presence(
        empty,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    assert!(
        !state.deps.protocol.call_threads.contains_key(&room_jid),
        "XMPP-native Muji clear must consume the active call thread when the SFU participant set is empty"
    );

    // The ended summary must be persisted onto the thread's inbox row so
    // the durable threads projection reflects the call ending without a
    // MAM replay.
    let anchor_row = state
        .deps
        .protocol
        .inbox_storage
        .list_threads(&alice.to_bare(), &room_jid)
        .await
        .expect("list room threads")
        .into_iter()
        .find(|entry| entry.thread_id.as_deref() == Some(thread_id))
        .expect("anchor thread row persists");
    assert!(
        anchor_row.call_ended_at.is_some(),
        "ending the active call must stamp call_ended_at onto the thread inbox row"
    );
    assert!(
        anchor_row
            .call_duration
            .as_ref()
            .is_some_and(|duration| duration.as_str().starts_with("PT")),
        "ending the active call must stamp an ISO-8601 call_duration onto the thread inbox row: {:?}",
        anchor_row.call_duration
    );
    assert_eq!(
        anchor_row.call_thread_kind,
        Some(waddle_xmpp::xep::CallThreadKind::Muc),
        "the anchor kind must survive the ended UPDATE"
    );
}

// Per #918: the whole call-thread lifecycle is gated on a configured SFU
// (`muc_update.rs`: anchor built only when `active_call_started &&
// sfu.is_some()`). Without an SFU the `active_call_started` flag from
// client-driven Muji presence must NOT register a call-thread anchor,
// because the end path that would consume it is itself SFU-gated. These
// two tests pin both sides of that gate.
#[tokio::test]
async fn active_muji_presence_without_sfu_does_not_register_call_thread_anchor() {
    // `create_test_websocket_state` builds state with `sfu: None`, so the
    // call-thread gate is closed.
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "no-sfu-anchor@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);

    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    assert!(
        !state.deps.protocol.call_threads.contains_key(&room_jid),
        "an active <muji/> must not register a call-thread anchor when no SFU is configured"
    );
}

#[tokio::test]
async fn active_muji_presence_with_sfu_registers_call_thread_anchor() {
    let recorder = std::sync::Arc::new(RecordingSfu::default());
    let state = state_with_recording_sfu(std::sync::Arc::clone(&recorder)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "with-sfu-anchor@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);

    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    assert!(
        state.deps.protocol.call_threads.contains_key(&room_jid),
        "an active <muji/> must register a call-thread anchor when an SFU is configured"
    );
}

#[tokio::test]
async fn resumed_muc_join_presence_replays_existing_muji_state() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-resume-replay@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;

    let bob_phase = waddle_xmpp::protocol::ConnectionPhase::ready(bob.clone(), false);
    let mut active = muc_presence_to(&room_jid, "bob");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &bob_phase,
        &None,
        None,
    )
    .await;

    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);
    let resumed_autojoin = muc_join_presence_to(&room_jid, "alice");
    let responses = handlers::presence::handle_presence(
        resumed_autojoin,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    let bob_replay = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| {
            element.attr("from") == Some(format!("{room_jid}/bob").as_str())
                && element.attr("to") == Some(alice.to_string().as_str())
        })
        .expect("resumed autojoin receives bob replay");
    assert!(
        bob_replay
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_some(),
        "resumed MUC autojoin must replay existing active-call Muji state"
    );
}

#[tokio::test]
async fn resumed_muc_join_presence_replays_own_muji_without_plain_broadcast() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-resume-own-replay@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (alice_tx, mut alice_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(alice.clone(), alice_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while alice_rx.try_recv().is_ok() {}

    let bob_phase = waddle_xmpp::protocol::ConnectionPhase::ready(bob.clone(), false);
    let mut active = muc_presence_to(&room_jid, "bob");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &bob_phase,
        &None,
        None,
    )
    .await;
    while alice_rx.try_recv().is_ok() {}

    let resumed_autojoin = muc_join_presence_to(&room_jid, "bob");
    let responses = handlers::presence::handle_presence(
        resumed_autojoin,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &bob_phase,
        &None,
        None,
    )
    .await;

    let self_replay = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| {
            element.attr("from") == Some(format!("{room_jid}/bob").as_str())
                && element.attr("to") == Some(bob.to_string().as_str())
                && element
                    .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                    .is_some()
        })
        .expect("resumed active participant receives own Muji replay");
    assert_eq!(
        muc_user_item_jid(&self_replay).as_deref(),
        Some(bob.to_string().as_str())
    );

    while let Ok(outbound) = alice_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        let presence = Element::from_str(&xml).expect("broadcast presence XML");
        if presence.attr("from") == Some(format!("{room_jid}/bob").as_str()) {
            assert!(
                presence
                    .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                    .is_some(),
                "same-session resumed autojoin must not broadcast a plain no-Muji update for the active participant"
            );
        }
    }
}

#[tokio::test]
async fn resumed_muc_join_presence_replays_muji_when_room_is_full() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-resume-full-room@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let room_actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    let mut config = room_actor
        .ask(GetSnapshot)
        .await
        .expect("room snapshot")
        .room
        .config;
    config.max_occupants = 1;
    room_actor
        .ask(UpdateConfig { config })
        .await
        .expect("room config update");

    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session.clone()),
        None,
    )
    .await;

    let resumed_autojoin = muc_join_presence_to(&room_jid, "alice");
    let responses = handlers::presence::handle_presence(
        resumed_autojoin,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    let self_replay = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| {
            element.attr("from") == Some(format!("{room_jid}/alice").as_str())
                && element.attr("to") == Some(alice.to_string().as_str())
                && element
                    .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                    .is_some()
        })
        .expect("full-room resumed autojoin receives own Muji replay");
    assert_eq!(
        muc_user_item_jid(&self_replay).as_deref(),
        Some(alice.to_string().as_str())
    );
}

#[tokio::test]
async fn same_nick_late_join_replays_existing_muji_with_exact_owner() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "same-nick-muji-replay@muc.example.com"
        .parse()
        .expect("room jid");
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop jid");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &desktop,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;

    let desktop_phase = waddle_xmpp::protocol::ConnectionPhase::ready(desktop.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &desktop_phase,
        &Some(owner_session),
        None,
    )
    .await;

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &mobile,
        "alice",
        None,
        &None,
    )
    .await;

    let sibling_muji_presence = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| {
            element.attr("from") == Some(format!("{room_jid}/alice").as_str())
                && element.attr("to") == Some(mobile.to_string().as_str())
                && muc_user_item_jid(element).as_deref() == Some(desktop.to_string().as_str())
                && element
                    .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                    .is_some()
        })
        .expect("mobile receives sibling Muji presence");

    assert!(
        sibling_muji_presence
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_some(),
        "same-nick sibling join must replay the existing Muji advertisement under the sibling JID"
    );
}

#[tokio::test]
async fn same_nick_late_join_replays_preparing_only_muji_with_exact_owner() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "same-nick-preparing-replay@muc.example.com"
        .parse()
        .expect("room jid");
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop jid");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &desktop,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &mobile,
        "alice",
        None,
        &None,
    )
    .await;

    let mobile_phase = waddle_xmpp::protocol::ConnectionPhase::ready(mobile.clone(), false);
    let mut preparing = muc_presence_to(&room_jid, "alice");
    preparing.payloads.push(preparing_muji().to_element());
    let _ = handlers::presence::handle_presence(
        preparing,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &mobile_phase,
        &Some(owner_session),
        None,
    )
    .await;

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;

    let replay = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| {
            element.attr("from") == Some(format!("{room_jid}/alice").as_str())
                && element.attr("to") == Some(bob.to_string().as_str())
                && muc_user_item_jid(element).as_deref() == Some(mobile.to_string().as_str())
                && element
                    .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                    .is_some()
        })
        .expect("bob receives mobile's preparing replay");

    let muji = replay
        .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
        .and_then(|element| waddle_xmpp::xep::xep0272::Muji::try_from(element).ok())
        .expect("Muji parses");
    assert!(muji.preparing);
    assert!(!muji.is_active());
}

#[tokio::test]
async fn same_nick_plain_presence_preserves_sibling_preparing_owner() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "same-nick-preparing-clear@muc.example.com"
        .parse()
        .expect("room jid");
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop jid");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &desktop,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &mobile,
        "alice",
        None,
        &None,
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let desktop_phase = waddle_xmpp::protocol::ConnectionPhase::ready(desktop.clone(), false);
    let mut preparing = muc_presence_to(&room_jid, "alice");
    preparing.payloads.push(preparing_muji().to_element());
    let _ = handlers::presence::handle_presence(
        preparing,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &desktop_phase,
        &Some(owner_session),
        None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let mobile_phase = waddle_xmpp::protocol::ConnectionPhase::ready(mobile.clone(), false);
    let plain_available = muc_presence_to(&room_jid, "alice");
    let _ = handlers::presence::handle_presence(
        plain_available,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &mobile_phase,
        &None,
        None,
    )
    .await;

    let mut saw_desktop_preparing = false;
    while let Ok(outbound) = bob_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        let presence = Element::from_str(&xml).expect("presence XML");
        if muc_user_item_jid(&presence).as_deref() == Some(desktop.to_string().as_str())
            && presence
                .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                .is_some()
        {
            saw_desktop_preparing = true;
        }
    }

    assert!(
        saw_desktop_preparing,
        "plain presence from one same-nick resource must preserve a sibling's preparing state under the sibling JID"
    );
}

#[tokio::test]
async fn same_nick_originator_leave_broadcasts_muji_clear() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "same-nick-muji-clear@muc.example.com"
        .parse()
        .expect("room jid");
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop jid");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (mobile_tx, mut mobile_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(mobile.clone(), mobile_tx);
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &desktop,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &mobile,
        "alice",
        None,
        &None,
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while mobile_rx.try_recv().is_ok() {}
    while bob_rx.try_recv().is_ok() {}

    let desktop_phase = waddle_xmpp::protocol::ConnectionPhase::ready(desktop.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &desktop_phase,
        &Some(owner_session),
        None,
    )
    .await;
    while mobile_rx.try_recv().is_ok() {}
    while bob_rx.try_recv().is_ok() {}

    let _ = handle_muc_leave(state.as_ref(), &room_jid, &desktop, "alice").await;

    for (recipient, rx) in [(&mobile, &mut mobile_rx), (&bob, &mut bob_rx)] {
        let broadcast = rx
            .try_recv()
            .unwrap_or_else(|_| panic!("{recipient} must receive Muji clear"));
        let xml = stanza_to_xml(&broadcast.stanza);
        let presence = Element::from_str(&xml).expect("clear presence XML");
        assert_eq!(presence.name(), "presence");
        assert_eq!(presence.attr("type"), None);
        assert_eq!(
            presence.attr("from"),
            Some(format!("{room_jid}/alice").as_str())
        );
        assert_eq!(presence.attr("to"), Some(recipient.to_string().as_str()));
        assert!(
            presence
                .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                .is_none(),
            "partial-resource clear broadcast must not carry <muji/>"
        );
    }

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.find_nick_by_real_jid(&mobile), Some("alice"));
    assert!(room.find_nick_by_real_jid(&desktop).is_none());
    assert!(room.muji_for_nick("alice").is_none());
}

#[tokio::test]
async fn same_nick_active_leave_broadcasts_departed_clear_before_preparing_sibling() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "same-nick-active-preparing-leave@muc.example.com"
        .parse()
        .expect("room jid");
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop jid");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &desktop,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &mobile,
        "alice",
        None,
        &None,
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let desktop_phase = waddle_xmpp::protocol::ConnectionPhase::ready(desktop.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &desktop_phase,
        &Some(owner_session.clone()),
        None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let mobile_phase = waddle_xmpp::protocol::ConnectionPhase::ready(mobile.clone(), false);
    let mut preparing = muc_presence_to(&room_jid, "alice");
    preparing.payloads.push(preparing_muji().to_element());
    let _ = handlers::presence::handle_presence(
        preparing,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &mobile_phase,
        &Some(owner_session),
        None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let _ = handle_muc_leave(state.as_ref(), &room_jid, &desktop, "alice").await;

    let clear_xml = stanza_to_xml(
        &bob_rx
            .try_recv()
            .expect("Bob receives departed desktop clear")
            .stanza,
    );
    let clear = Element::from_str(&clear_xml).expect("desktop clear presence XML");
    assert_eq!(
        muc_user_item_jid(&clear).as_deref(),
        Some(desktop.to_string().as_str())
    );
    assert!(
        clear
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_none(),
        "departed active resource must be explicitly cleared before sibling replay"
    );

    let preparing_xml = stanza_to_xml(
        &bob_rx
            .try_recv()
            .expect("Bob receives remaining mobile preparing replay")
            .stanza,
    );
    let preparing = Element::from_str(&preparing_xml).expect("mobile preparing presence XML");
    assert_eq!(
        muc_user_item_jid(&preparing).as_deref(),
        Some(mobile.to_string().as_str())
    );
    let muji = preparing
        .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
        .and_then(|element| waddle_xmpp::xep::xep0272::Muji::try_from(element).ok())
        .expect("remaining sibling Muji parses");
    assert!(muji.preparing);
    assert!(!muji.is_active());
    assert!(
        bob_rx.try_recv().is_err(),
        "only departed clear plus remaining sibling replay should be broadcast"
    );
}

/// XEP-0045 slice-2b ingest: a successful `handle_muc_join` MUST
/// record `(sender_bare, room)` activity in the
/// `notification_activity` projection so the T1 XEP-0513 `<active/>`
/// evaluator can admit ActiveChannelMention pushes for the joiner.
/// Pass-2 review caught this as a regression — the writer existed but
/// nothing in production called it. The test guards the wire-in.
#[tokio::test]
async fn handle_muc_join_records_notification_activity_for_sender() {
    use crate::notification_activity::NotificationActivityReader;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");

    // Pre-condition: no activity row yet for (alice, room).
    let before = state
        .deps
        .protocol
        .notification_activity
        .read_activity(&alice.to_bare(), &room_jid)
        .await
        .expect("read pre-join");
    assert!(
        before.is_none(),
        "projection MUST start empty for fresh user/room pair; got {before:?}",
    );

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        Some(crate::notification_activity::NotificationPresenceShow::Away),
        &Some(owner_session),
    )
    .await;

    let after = state
        .deps
        .protocol
        .notification_activity
        .read_activity(&alice.to_bare(), &room_jid)
        .await
        .expect("read post-join")
        .expect("activity row MUST exist after MUC join");
    assert!(
        after.last_active_at_ms > 0,
        "MUC join MUST bump last_active_at_ms; got {}",
        after.last_active_at_ms,
    );
    assert_eq!(
        after.presence_show,
        Some(crate::notification_activity::NotificationPresenceShow::Away),
        "join MUST persist the typed `<show/>` token; got {:?}",
        after.presence_show,
    );
}

/// XEP-0045 slice-2b ingest: a `handle_muc_leave` MUST bump
/// `(sender_bare, room)` activity AND clear the persisted
/// `<show/>` (an explicit unavailable has no available presence).
#[tokio::test]
async fn handle_muc_leave_records_notification_activity_and_clears_show() {
    use crate::notification_activity::NotificationActivityReader;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        Some(crate::notification_activity::NotificationPresenceShow::Chat),
        &Some(owner_session),
    )
    .await;
    let after_join = state
        .deps
        .protocol
        .notification_activity
        .read_activity(&alice.to_bare(), &room_jid)
        .await
        .expect("read post-join")
        .expect("activity post-join");
    assert_eq!(
        after_join.presence_show,
        Some(crate::notification_activity::NotificationPresenceShow::Chat)
    );

    // No sleep needed: the monotonic UPSERTs in
    // `NotificationActivityStore` now use `>=` so tie-millis writes
    // (rapid join/leave landing in the same millisecond) still apply
    // the latest writer's columns — the leave's `last_active_at_ms`
    // and cleared `<show/>` are persisted even when `now_ms` matches
    // the join's. The previous 2ms sleep was a workaround for the
    // strict-`>` race (Codex/Copilot review on PR #731). The assertion
    // below uses `>=` to allow same-millisecond ties without flaking.
    let _ = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice").await;
    let after_leave = state
        .deps
        .protocol
        .notification_activity
        .read_activity(&alice.to_bare(), &room_jid)
        .await
        .expect("read post-leave")
        .expect("activity post-leave");
    assert!(
        after_leave.last_active_at_ms >= after_join.last_active_at_ms,
        "leave MUST NOT regress last_active_at_ms below join (got join={} leave={})",
        after_join.last_active_at_ms,
        after_leave.last_active_at_ms,
    );
    assert!(
        after_leave.presence_show.is_none(),
        "leave MUST clear persisted `<show/>`; got {:?}",
        after_leave.presence_show,
    );
}

// --- Issue #935: involuntary MUC removal must evict SFU call participants ---

fn build_admin_set_iq_xml(room_jid: &BareJid, id: &str, item: Element) -> String {
    element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(item)
                    .build(),
            )
            .build(),
    )
}

async fn join_alice_owner_and_bob(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> (Session, FullJid, FullJid) {
    let alice_session = create_test_server_owner_session(state, "alice").await;
    let bob_session = create_test_session(state, "bob").await;
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    handle_muc_join(
        state,
        "example.com",
        room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state,
        "example.com",
        room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    (alice_session, alice_jid, bob_jid)
}

#[tokio::test]
async fn muc_admin_kick_evicts_target_sessions_from_room_call() {
    let recorder = Arc::new(crate::server::routes::websocket::tests::RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let room_jid: BareJid = "kick-evicts@muc.example.com".parse().expect("room jid");
    let (alice_session, alice_jid, _bob_jid) =
        join_alice_owner_and_bob(state.as_ref(), &room_jid).await;
    let ready = ready_phase(&alice_jid);

    let kick_iq = build_admin_set_iq_xml(
        &room_jid,
        "kick-bob",
        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "bob")
            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "none")
            .build(),
    );
    let responses = handle_iq(
        &kick_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "kick response: {responses:?}");
    assert!(responses[0].contains("type='result'"), "{responses:?}");

    let evicted = recorder.snapshot();
    assert_eq!(
        evicted.len(),
        1,
        "exactly the kicked occupant's session is evicted: {evicted:?}"
    );
    assert_eq!(evicted[0].0.as_str(), "kick-evicts@muc.example.com");
    assert_eq!(evicted[0].1.as_livekit_identity(), "bob@example.com/web");
    assert!(
        recorder.note_snapshot().is_empty(),
        "moderation must use the full admin-evict path, not webhook bookkeeping"
    );
}

#[tokio::test]
async fn muc_admin_ban_evicts_target_sessions_from_room_call() {
    let recorder = Arc::new(crate::server::routes::websocket::tests::RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let room_jid: BareJid = "ban-evicts@muc.example.com".parse().expect("room jid");
    let (alice_session, alice_jid, _bob_jid) =
        join_alice_owner_and_bob(state.as_ref(), &room_jid).await;
    let ready = ready_phase(&alice_jid);

    let ban_iq = build_admin_set_iq_xml(
        &room_jid,
        "ban-bob",
        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                "bob@example.com",
            )
            .attr(
                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                "outcast",
            )
            .build(),
    );
    let responses = handle_iq(
        &ban_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "ban response: {responses:?}");
    assert!(responses[0].contains("type='result'"), "{responses:?}");

    let evicted = recorder.snapshot();
    assert_eq!(
        evicted.len(),
        1,
        "exactly the banned occupant's session is evicted: {evicted:?}"
    );
    assert_eq!(evicted[0].0.as_str(), "ban-evicts@muc.example.com");
    assert_eq!(evicted[0].1.as_livekit_identity(), "bob@example.com/web");
}

#[tokio::test]
async fn muc_admin_role_demotion_does_not_evict_from_room_call() {
    let recorder = Arc::new(crate::server::routes::websocket::tests::RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let room_jid: BareJid = "demote-keeps@muc.example.com".parse().expect("room jid");
    let (alice_session, alice_jid, _bob_jid) =
        join_alice_owner_and_bob(state.as_ref(), &room_jid).await;
    let ready = ready_phase(&alice_jid);

    let demote_iq = build_admin_set_iq_xml(
        &room_jid,
        "demote-bob",
        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "bob")
            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "visitor")
            .build(),
    );
    let responses = handle_iq(
        &demote_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "demote response: {responses:?}");
    assert!(responses[0].contains("type='result'"), "{responses:?}");

    assert!(
        recorder.snapshot().is_empty(),
        "a role change that keeps the occupant must not end their call session"
    );
}

/// Round-3 concurrency review: MUC occupancy is keyed by FULL JID, so
/// a detached session's invalidation cleanup must NOT evict the JID
/// from its rooms while a live same-JID replacement session exists —
/// the replacement shares the occupancy and would be kicked out of
/// every room it just (re)joined.
#[tokio::test]
async fn detached_invalidation_skips_muc_cleanup_when_live_replacement_exists() {
    use super::cleanup::cleanup_invalidated_detached_session;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "replacement-race-channel@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    // A live replacement session holds the registry slot for the SAME
    // full JID (fresh bind after the old session detached).
    let (repl_tx, _repl_rx) = mpsc::channel::<OutboundStanza>(4);
    state
        .deps
        .protocol
        .connection_registry
        .register(alice.clone(), repl_tx);

    // The stale detached session (a stream id the registry no longer
    // holds) gets invalidated.
    let detached = waddle_xmpp::stream_management::DetachedSession {
        stream_id: "stale-stream".to_string(),
        user_id: "alice@example.com".to_string(),
        jid: alice.clone(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: vec![],
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    cleanup_invalidated_detached_session(state.as_ref(), detached.clone(), None).await;

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(
        room.find_nick_by_real_jid(&alice),
        Some("alice"),
        "the live replacement's room occupancy must survive the stale \
         detached session's invalidation cleanup"
    );

    // Companion: with no live entry for the JID, the invalidation DOES
    // evict the occupancy.
    state.deps.protocol.connection_registry.unregister(&alice);
    cleanup_invalidated_detached_session(state.as_ref(), detached, None).await;
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.find_nick_by_real_jid(&alice).is_none(),
        "with no live replacement the invalidated session's occupancy \
         must be cleaned up"
    );
}

/// codex P1 on PR #1207: when the invalidation is triggered BY the
/// same client's fresh bind (the replacement owner IS the invalidating
/// caller), the new stream cannot have joined any rooms yet — the dead
/// session's occupancies are certainly stale and MUST be cleaned, or
/// the fresh connection inherits room fan-out without ever joining.
/// Only a FOREIGN live entry (someone else's slot, unknown join state)
/// warrants skipping room cleanup.
#[tokio::test]
async fn fresh_bind_invalidation_cleans_the_dead_sessions_room_occupancy() {
    use super::cleanup::cleanup_invalidated_detached_session;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "fresh-bind-cleanup-channel@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    // The same client fresh-binds: its NEW connection owns the slot and
    // triggers invalidation of the old detached session.
    let (fresh_tx, _fresh_rx) = mpsc::channel::<OutboundStanza>(4);
    let fresh_owner = state
        .deps
        .protocol
        .connection_registry
        .register(alice.clone(), fresh_tx);

    let detached = waddle_xmpp::stream_management::DetachedSession {
        stream_id: "stale-stream-fresh-bind".to_string(),
        user_id: "alice@example.com".to_string(),
        jid: alice.clone(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: vec![],
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    cleanup_invalidated_detached_session(state.as_ref(), detached, Some(&fresh_owner)).await;

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.find_nick_by_real_jid(&alice).is_none(),
        "a fresh bind's invalidation must clean the dead session's room \
         occupancy — the new stream has not joined anything yet"
    );
}
