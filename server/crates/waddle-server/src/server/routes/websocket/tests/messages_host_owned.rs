//! A joined extension bot observes messages through hooks, without a socket copy.
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
