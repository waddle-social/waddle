use super::*;
use crate::server::routes::websocket::tests as socket_tests;
use waddle_xmpp::registry::DeliveryKind;

fn reflection(target: &jid::FullJid) -> waddle_xmpp::Stanza {
    let mut message = xmpp_parsers::message::Message::new(Some(target.clone().into()));
    message.from = Some(
        "room@conference.example.com/alice"
            .parse()
            .expect("room nick"),
    );
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    waddle_xmpp::Stanza::Message(message)
}

#[tokio::test]
async fn declined_full_jid_relay_delivers_local_groupchat_peer() {
    let state = socket_tests::create_test_websocket_state().await;
    let target: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &target, tx).await;
    let deps = Deps {
        user_registry: Some(&state.deps.protocol.user_registry),
        ..Deps::registry_only(&state.deps.protocol.connection_registry)
    };
    let stanza = reflection(&target);
    let outcome = execute(
        ExternalDeliveryEffect::RelayFullJid {
            route_identity: None,
            origin: None,
            target,
            stanza: Box::new(stanza.clone()),
            call_setup: None,
        },
        &deps,
    )
    .await;
    assert!(matches!(
        outcome,
        EffectOutcome::Delivery(FullJidDeliveryOutcome::Delivered)
    ));
    let received = rx
        .try_recv()
        .expect("local fallback sends the occupant copy");
    assert_eq!(received.kind, DeliveryKind::PeerStanza);
    let (waddle_xmpp::Stanza::Message(received), waddle_xmpp::Stanza::Message(expected)) =
        (received.stanza, stanza)
    else {
        panic!("fallback preserves the groupchat message");
    };
    assert_eq!(received.type_, expected.type_);
    assert_eq!(received.from, expected.from);
    assert_eq!(received.to, expected.to);
    assert!(rx.try_recv().is_err(), "one copy only");
}

#[tokio::test]
async fn handled_full_jid_relay_failure_never_falls_back() {
    let state = socket_tests::create_test_websocket_state().await;
    let target: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &target, tx).await;
    let deps = Deps {
        user_registry: Some(&state.deps.protocol.user_registry),
        ..Deps::registry_only(&state.deps.protocol.connection_registry)
    };
    for handled in [
        #[cfg(feature = "clustering")]
        FullJidDeliveryOutcome::MaybeCommitted,
        FullJidDeliveryOutcome::Dropped,
        FullJidDeliveryOutcome::Unavailable,
    ] {
        assert_eq!(
            finish_full_jid_relay(Some(handled), &deps, &target, &reflection(&target), None).await,
            handled,
        );
        assert!(
            rx.try_recv().is_err(),
            "handled relay must not send a local copy"
        );
    }
}

#[path = "delivery_immediate_offline_tests.rs"]
mod offline;
