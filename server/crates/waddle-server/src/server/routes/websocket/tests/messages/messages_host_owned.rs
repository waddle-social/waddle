//! Extension bots are host-owned: a joined bot observes room messages through hooks without
//! a socket copy, and is never a peer for direct messages, private messages or subscriptions.
use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{
    CanonicalMessageRepository, EffectIntentRepository, EffectReceiptRepository,
};
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey};

async fn user_groupchat_with_bot(f: IngressFixture, another_user: bool) {
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(f.db.driver(), f.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared database pool");
    let state = create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool),
        Arc::new(f.authority().await),
    )
    .await;
    let sender: FullJid = "alice@example.com/web".parse().expect("sender");
    let bot: FullJid = "observer@extensions.example.com/bot".parse().expect("bot");
    let session = create_test_session(&state, "alice").await;
    let (sender_tx, mut sender_rx) = mpsc::channel(32);
    register_test_connection(&state, &sender, sender_tx).await;
    let room: BareJid = "host-copy@muc.example.com".parse().expect("room");
    let actor = get_or_create_room_actor(
        &state,
        &room,
        RoomConfig {
            persistent: true,
            members_only: false,
            ..Default::default()
        },
        "space".into(),
        "host-copy".into(),
    )
    .await
    .expect("room")
    .actor_ref;
    let mut occupants = vec![(sender.clone(), "alice"), (bot.clone(), "observer")];
    let mut recipient_rx = None;
    if another_user {
        let other: FullJid = "bob@example.com/web".parse().expect("other recipient");
        let (tx, rx) = mpsc::channel(32);
        register_test_connection(&state, &other, tx).await;
        recipient_rx = Some(rx);
        occupants.push((other, "bob"));
    }
    for (real_jid, nick) in occupants {
        actor
            .ask(waddle_xmpp::muc::room_actor::Join {
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                nick: nick.into(),
                real_jid,
                role: waddle_xmpp::Role::Participant,
                affiliation: Affiliation::None,
            })
            .await
            .expect("occupant joins");
    }
    let mut message = xmpp_parsers::message::Message::new(Some(room.clone().into()));
    message.id = Some(xmpp_parsers::message::Id("user-host-copy".into()));
    message.type_ = XmppMessageType::Groupchat;
    message
        .bodies
        .insert(Default::default(), "message observed by bot".into());
    message
        .payloads
        .push(waddle_xmpp_core::xep0359::build_origin_id_element(
            "user-host-copy",
        ));
    let responses =
        handle_message_through_ingress(&state, &sender, Some(&session), message.clone()).await;
    assert!(
        responses
            .iter()
            .all(|response| !response.contains("type=\"error\"")),
        "{responses:?}"
    );
    assert!(
        responses.iter().all(|response| {
            let stanza: Element = response.parse().expect("well-formed response");
            stanza.attr("to") != Some(bot.as_str())
        }),
        "bot copies never leak into the user's websocket frames"
    );
    let reflection = std::iter::from_fn(|| sender_rx.try_recv().ok())
        .find_map(|outbound| match outbound.stanza {
            Stanza::Message(message) if message.type_ == XmppMessageType::Groupchat => {
                Some(message)
            }
            _ => None,
        })
        .expect("registered sender receives its groupchat reflection");
    assert_eq!(reflection.to, Some(sender.clone().into()));
    assert_eq!(
        reflection.from.as_ref().map(jid::Jid::to_bare),
        Some(room.clone())
    );
    let key = MessageKey::from_storage(
        f.optional_text("SELECT CAST(message_key AS TEXT) FROM ingress_messages")
            .await
            .expect("message key")
            .parse()
            .expect("uuid"),
    );
    let mut tx = f.uow.begin().await.expect("inspect receipts");
    let intents = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("intents");
    assert!(intents.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { occupants, reflection, .. } if occupants.contains(&bot) && reflection == &sender)), "bot copy must be in the recorded route");
    assert!(
        EffectReceiptRepository::receipts_complete(&mut tx, key)
            .await
            .expect("receipts"),
        "the no-transport bot copy must complete the groupchat route receipt"
    );
    assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal"));
    tx.commit().await.expect("inspection commit");
    if let Some(rx) = recipient_rx.as_mut() {
        assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|outbound| matches!(outbound.stanza, Stanza::Message(ref m) if m.type_ == XmppMessageType::Groupchat)), "connected occupant gets its ordinary copy");
    }
    let receipts = f.count("ingress_effect_receipts").await;
    handle_message_through_ingress(&state, &sender, Some(&session), message).await;
    assert_eq!(
        f.count("ingress_messages").await,
        1,
        "same origin reuses the canonical row"
    );
    assert_eq!(
        f.count("ingress_effect_receipts").await,
        receipts,
        "settled bot copy adds no duplicate receipt work"
    );
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    if let Some(rx) = recipient_rx.as_mut() {
        assert!(!std::iter::from_fn(|| rx.try_recv().ok()).any(|outbound| matches!(outbound.stanza, Stanza::Message(ref m) if m.type_ == XmppMessageType::Groupchat)), "duplicate does not fan out again");
    }
    assert!(
        state
            .deps
            .protocol
            .ingress
            .drain_and_join(std::time::Duration::from_secs(5))
            .await
    );
    drop(state);
    f.close().await;
}

#[tokio::test]
async fn extension_host_owned_groupchat_copy_sqlite() {
    user_groupchat_with_bot(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn extension_host_owned_groupchat_copy_postgres() {
    if let Some(f) = IngressFixture::postgres("host_owned_copy").await {
        user_groupchat_with_bot(f, true).await;
    }
}
#[tokio::test]
async fn extension_host_owned_groupchat_only_bot_and_sender_sqlite() {
    user_groupchat_with_bot(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn extension_host_owned_groupchat_only_bot_and_sender_postgres() {
    if let Some(f) = IngressFixture::postgres("host_owned_only_bot").await {
        user_groupchat_with_bot(f, false).await;
    }
}

async fn table_rows(state: &WebSocketState, table: &str) -> i64 {
    let db = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("db");
    let mut rows = db
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .expect("count query");
    rows.next()
        .await
        .expect("count row")
        .expect("count result")
        .get(0)
        .expect("integer count")
}

/// What an accepted message leaves behind: archive rows and inbox projections.
/// (A refusal still gets a canonical ingress record, so a retry replays it.)
async fn message_footprint(state: &WebSocketState) -> Vec<i64> {
    let mut rows = Vec::new();
    for table in ["mam_messages", "inbox_entries"] {
        rows.push(table_rows(state, table).await);
    }
    rows
}

fn assert_service_unavailable(frame: &str, from: &str, to: &FullJid, id: &str) {
    let reply = Element::from_str(frame).expect("error reply XML");
    assert_eq!(reply.name(), "message", "{frame}");
    assert_eq!(reply.attr("type"), Some("error"), "{frame}");
    assert_eq!(reply.attr("from"), Some(from), "{frame}");
    assert_eq!(reply.attr("to"), Some(to.as_str()), "{frame}");
    assert_eq!(reply.attr("id"), Some(id), "{frame}");
    let error = reply
        .get_child("error", waddle_xmpp::ns::JABBER_CLIENT)
        .expect("error payload");
    assert_eq!(error.attr("type"), Some("cancel"), "{frame}");
    assert!(
        error
            .children()
            .any(|child| child.name() == "service-unavailable"),
        "{frame}"
    );
}

/// RFC 6121 §8.5.1: a bot is not a direct-message peer. Chat, an XEP-0249
/// direct invitation and a headline to a bot JID are refused with
/// `service-unavailable` from the bot, before the sender archive or inbox
/// see them. A message to an ordinary local account still
/// leaves rows, so the unchanged footprint is the refusal's doing.
#[tokio::test]
async fn extension_bot_direct_messages_are_refused_before_any_write() {
    let state = create_test_websocket_state_with_fixture_bot().await;
    let session = create_test_session(&state, "alice").await;
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    seed_local_account(&state, "bob").await;

    let mut control = xmpp_parsers::message::Message::new(Some("bob@example.com".parse().unwrap()));
    control.type_ = XmppMessageType::Chat;
    control.id = Some(xmpp_parsers::message::Id("control".into()));
    control.bodies.insert(Default::default(), "hi bob".into());
    handle_message_through_ingress(&state, &alice, Some(&session), control).await;
    let before = message_footprint(&state).await;
    assert!(
        before.iter().all(|rows| *rows > 0),
        "a message to a person is archived and projected to the inbox: {before:?}"
    );

    let bot = format!("{FIXTURE_BOT_PLUGIN}@extensions.example.com");
    let invitation = waddle_xmpp::xep::xep0249::build_direct_invite(
        &waddle_xmpp::xep::xep0249::DirectInvite::new("room@muc.example.com".parse().unwrap()),
    );
    let mut case = 0;
    for target in [bot.clone(), format!("{bot}/bot")] {
        for (type_, payload) in [
            (XmppMessageType::Chat, None),
            (XmppMessageType::Normal, Some(invitation.clone())),
            (XmppMessageType::Headline, None),
        ] {
            case += 1;
            let id = format!("to-bot-{case}");
            let mut message =
                xmpp_parsers::message::Message::new(Some(target.parse().expect("target")));
            message.type_ = type_.clone();
            message.id = Some(xmpp_parsers::message::Id(id.clone()));
            message
                .bodies
                .insert(Default::default(), "hello bot".into());
            message.payloads.extend(payload);
            let responses =
                handle_message_through_ingress(&state, &alice, Some(&session), message).await;
            assert_eq!(responses.len(), 1, "{target} {type_:?}: {responses:?}");
            assert_service_unavailable(&responses[0], &target, &alice, &id);
        }
    }
    assert_eq!(
        message_footprint(&state).await,
        before,
        "refused messages leave no archive or inbox row"
    );
}

/// XEP-0045 §7.5: a private message to a bot's occupant JID is refused by
/// the room as `service-unavailable` from that occupant JID, and nothing
/// is archived or routed. A private message to a human occupant still is.
#[tokio::test]
async fn extension_bot_muc_private_message_is_refused_from_the_occupant() {
    let state = create_test_websocket_state_with_fixture_bot().await;
    let alice_session = create_test_server_owner_session(&state, "alice").await;
    let bob_session = create_test_session(&state, "bob").await;
    let room: BareJid = "bot-pm@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let bob: FullJid = "bob@example.com/web".parse().expect("bob");
    let bot: FullJid = format!("{FIXTURE_BOT_PLUGIN}@extensions.example.com/bot")
        .parse()
        .expect("bot");
    let (bob_tx, mut bob_rx) = mpsc::channel(8);
    register_test_connection(&state, &bob, bob_tx).await;
    for (jid, nick, session) in [
        (&alice, "alice", alice_session.clone()),
        (&bob, "bob", bob_session),
    ] {
        let responses = super::super::handle_muc_join_with_occupancy_session(
            &state,
            "example.com",
            &room,
            jid,
            nick,
            None,
            (
                super::super::record_test_occupancy_session(&state, jid),
                &Some(session),
            ),
        )
        .await;
        assert!(
            !responses.iter().any(|frame| frame.contains("type='error'")),
            "{nick} joins: {responses:?}"
        );
    }
    get_room_actor(&state, &room)
        .await
        .expect("room actor")
        .ask(waddle_xmpp::muc::room_actor::Join {
            session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            nick: "helper".into(),
            real_jid: bot,
            role: waddle_xmpp::Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("bot joins");
    while bob_rx.try_recv().is_ok() {}

    let private_message = |nick: &str, id: &str| {
        let mut message = xmpp_parsers::message::Message::new(Some(
            room.clone()
                .with_resource_str(nick)
                .expect("occupant")
                .into(),
        ));
        message.type_ = XmppMessageType::Chat;
        message.id = Some(xmpp_parsers::message::Id(id.into()));
        message.bodies.insert(Default::default(), "psst".into());
        message
    };
    let to_human = handle_message_through_ingress(
        &state,
        &alice,
        Some(&alice_session),
        private_message("bob", "pm-bob"),
    )
    .await;
    assert!(
        to_human.is_empty(),
        "a human occupant's PM routes: {to_human:?}"
    );
    assert!(bob_rx.try_recv().is_ok(), "bob received the PM");
    let before = message_footprint(&state).await;
    assert!(before[0] > 0, "the PM to a person is archived: {before:?}");

    let helper = format!("{room}/helper");
    let responses = handle_message_through_ingress(
        &state,
        &alice,
        Some(&alice_session),
        private_message("helper", "pm-bot"),
    )
    .await;
    assert_eq!(responses.len(), 1, "{responses:?}");
    assert_service_unavailable(&responses[0], &helper, &alice, "pm-bot");
    assert_eq!(
        message_footprint(&state).await,
        before,
        "the refused PM leaves nothing behind"
    );
}

/// RFC 6121 §8.5.1: a bot declines a subscription request (`unsubscribed`
/// from its bare JID) and ignores the other subscription stanzas. Neither
/// creates a roster item or a roster push.
#[tokio::test]
async fn extension_bot_presence_subscription_is_declined_without_roster_write() {
    let state = create_test_websocket_state_with_fixture_bot().await;
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let (tx, mut rx) = mpsc::channel::<waddle_xmpp::registry::OutboundStanza>(16);
    let owner = register_test_connection(&state, &alice, tx).await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(alice.clone(), false);
    conn.registry_owner = Some(owner);
    let bot = format!("{FIXTURE_BOT_PLUGIN}@extensions.example.com");

    async fn roster_items(state: &WebSocketState, conn: &mut WsConnState) -> Vec<String> {
        let frames = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="get" id="roster"><query xmlns="jabber:iq:roster"/></iq>"#,
            "example.com",
            state,
            conn,
        )
        .await;
        let iq = Element::from_str(&frames[0]).expect("roster result");
        iq.get_child("query", "jabber:iq:roster")
            .expect("roster query")
            .children()
            .filter_map(|item| item.attr("jid").map(str::to_owned))
            .collect()
    }
    assert!(roster_items(&state, &mut conn).await.is_empty());
    while rx.try_recv().is_ok() {}

    let frames = handle_xmpp_frame(
        &format!(r#"<presence xmlns="jabber:client" type="subscribe" to="{bot}"/>"#),
        "example.com",
        &state,
        &mut conn,
    )
    .await;
    assert_eq!(frames.len(), 1, "{frames:?}");
    let reply = Element::from_str(&frames[0]).expect("presence reply");
    assert_eq!(reply.name(), "presence");
    assert_eq!(reply.attr("type"), Some("unsubscribed"), "{reply:?}");
    assert_eq!(reply.attr("from"), Some(bot.as_str()), "{reply:?}");
    assert_eq!(reply.attr("to"), Some("alice@example.com"), "{reply:?}");

    for kind in ["subscribed", "unsubscribe", "unsubscribed"] {
        let frames = handle_xmpp_frame(
            &format!(r#"<presence xmlns="jabber:client" type="{kind}" to="{bot}"/>"#),
            "example.com",
            &state,
            &mut conn,
        )
        .await;
        assert!(frames.is_empty(), "{kind} is ignored: {frames:?}");
    }
    assert!(
        roster_items(&state, &mut conn).await.is_empty(),
        "no roster item for the bot"
    );
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok()).next().is_none(),
        "no roster push or presence delivered"
    );
}
