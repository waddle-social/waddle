use super::*;

// ---------------------------------------------------------------------
// #1246 — RFC 6121 §8.5.1: message to a nonexistent local account is
// bounced with <service-unavailable/>, never persisted.
// ---------------------------------------------------------------------

#[tokio::test]
async fn route_bare_jid_message_to_nonexistent_local_user_bounces() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        web_socket_state: Some(&state),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("typo@example.com"),
        "anyone?",
    );
    let outcome = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "typo@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    assert_eq!(
        outcome.frames.len(),
        1,
        "sender must receive a bounce for a nonexistent local account"
    );
    assert!(
        outcome.frames[0].contains("service-unavailable"),
        "RFC 6121 §8.5.1: the bounce is <service-unavailable/>; got {}",
        outcome.frames[0]
    );
    assert!(
        outcome.frames[0].contains("type=\"error\"") || outcome.frames[0].contains("type='error'"),
        "bounce is a message of type error; got {}",
        outcome.frames[0]
    );

    let typo_bare: jid::BareJid = "typo@example.com".parse().expect("bare");
    let typo_archive = mam
        .query_messages(
            &typo_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query typo");
    assert!(
        typo_archive.messages.is_empty(),
        "no MAM rows may be created for a nonexistent account"
    );
}

#[tokio::test]
async fn route_bare_jid_message_to_existing_oidc_user_persists_offline() {
    // Two-table identity: an OIDC-provisioned account exists only in
    // `users` (no native_users row). The existence gate must accept it
    // and run the normal offline/headless persistence.
    use crate::db::actor::DbExecute;
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: "INSERT INTO users \
                  (jid, username, xmpp_localpart, display_name, avatar_url, primary_email, created_at, updated_at) \
                  VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            params: vec![
                "bob@example.com".into(),
                "bob".into(),
                "bob".into(),
                "Bob".into(),
                crate::db::Value::NullText,
                crate::db::Value::NullText,
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ],
        })
        .await
        .expect("seed oidc user");

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        web_socket_state: Some(&state),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "hello bob",
    );
    let outcome = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    assert!(
        outcome.frames.is_empty(),
        "existing OIDC account must not be bounced"
    );

    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "offline persistence runs for the OIDC-only account"
    );
}
