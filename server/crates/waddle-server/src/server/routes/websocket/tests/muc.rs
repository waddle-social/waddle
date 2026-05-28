use super::*;

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
            "INSERT INTO channels (id, name, description, channel_type, position, is_default) VALUES (?, ?, ?, 'announcement', 0, 0)",
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
    assert_eq!(item.attr("affiliation"), Some("member"));
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
