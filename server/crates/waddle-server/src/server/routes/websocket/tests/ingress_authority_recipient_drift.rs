use super::*;
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey};
use waddle_xmpp::protocol::InboundEvent;

async fn recorded_obligations(state: &WebSocketState) -> Vec<IngressEffectIntent> {
    let key = {
        let db = state
            .deps
            .app_state
            .db_pool
            .global()
            .guard()
            .await
            .expect("database");
        let mut rows = db
            .query("SELECT CAST(message_key AS TEXT) FROM ingress_messages", ())
            .await
            .expect("canonical key");
        let row = rows.next().await.expect("row").expect("canonical message");
        let key = MessageKey::from_storage(
            row.get::<String>(0)
                .expect("stored key")
                .parse()
                .expect("message UUID"),
        );
        assert!(
            rows.next().await.expect("next row").is_none(),
            "one canonical message"
        );
        key
    };
    let uow = crate::ingress_uow::IngressUnitOfWork::open(
        state.deps.app_state.db_pool.global().clone(),
        crate::ingress::test_lineage_config(),
    )
    .expect("read UoW");
    let mut tx = uow.begin().await.expect("read authority");
    let intents = crate::ingress_uow::EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("recorded obligations");
    tx.commit().await.expect("close read");
    intents
}

async fn assert_terminal_with_receipts(state: &WebSocketState, intents: &[IngressEffectIntent]) {
    let (receipts, terminal) = frame_receipt_state(state).await;
    assert_eq!(
        receipts,
        i64::try_from(intents.len()).expect("intent count")
    );
    assert_eq!(terminal, 1, "all recorded obligations terminalized");
}

async fn live_full_jid_disconnect_retry(state: Arc<WebSocketState>) {
    seed_local_account(&state, "bob").await;
    let full: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    register_test_connection(&state, &full, tx).await;
    let mut conn = connection(&state, true).await;
    let mut message = xmpp_parsers::message::Message::new(Some(full.clone().into()));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Default::default(), "live then offline".to_owned());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, "live-disconnect-retry");
    let wire = super::super::super::transport_xml::stanza_to_xml(&Stanza::Message(message));
    handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    let outbound = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("live delivery")
        .expect("recipient channel");
    assert!(matches!(
        outbound.kind,
        waddle_xmpp::registry::DeliveryKind::PeerStanza
    ));
    let mut recipient_conn = WsConnState::new();
    recipient_conn.phase = ConnectionPhase::ready(full.clone(), false);
    recipient_conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        full.clone(),
        false,
        Default::default(),
    );
    let machine = recipient_conn
        .state_machine
        .as_mut()
        .expect("recipient machine");
    let events = machine.handle(InboundEvent::StanzaFromPeer(Box::new(outbound.stanza)));
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
    crate::server::routes::websocket::replay::drive_interpret_loop(events, machine, &deps).await;
    let archived = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &full.to_bare(),
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("recipient archive")
        .messages;
    assert_eq!(archived.len(), 1);
    let original = &archived[0];
    let obligations = recorded_obligations(&state).await;
    assert_terminal_with_receipts(&state, &obligations).await;
    state.deps.protocol.connection_registry.unregister(&full);
    let user = state
        .deps
        .protocol
        .user_registry
        .ask(waddle_xmpp::registry::user_registry::GetUser {
            bare_jid: full.to_bare(),
        })
        .await
        .expect("lookup user")
        .expect("recipient actor");
    user.ask(waddle_xmpp::registry::user_actor::UnregisterConnection {
        jid: full.clone(),
        owner: None,
    })
    .await
    .expect("disconnect resource");
    while rx.try_recv().is_ok() {}
    let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
    assert!(frames
        .iter()
        .all(|frame| !frame.contains("internal-server-error")));
    assert_eq!(conn.sm_state.get_inbound_count(), 2);
    assert!(!conn.sm_inbound_completion.has_unhandled_hole());
    assert!(rx.try_recv().is_err(), "no duplicate recipient delivery");
    let archived = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &full.to_bare(),
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("recipient archive after retry")
        .messages;
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].id, original.id);
    assert_eq!(archived[0].stanza_id, original.stanza_id);
    assert_eq!(archived[0].timestamp, original.timestamp);
    assert_eq!(archived[0].body, original.body);
    assert_eq!(archived[0].stanza_xml, original.stanza_xml);
    assert_eq!(recorded_obligations(&state).await, obligations);
    assert_terminal_with_receipts(&state, &obligations).await;
    let ack = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmRequest::to_xml(),
        "example.com",
        &state,
        &mut conn,
    )
    .await;
    let ack: minidom::Element = ack.first().expect("ACK").parse().expect("ACK XML");
    assert_eq!(ack.attr("h"), Some("2"));
}

#[tokio::test]
async fn ingress_live_full_jid_disconnect_retry_advances_h_sqlite() {
    live_full_jid_disconnect_retry(create_test_websocket_state().await).await;
}
#[tokio::test]
async fn ingress_live_full_jid_disconnect_retry_advances_h_postgres() {
    recovery::postgres_case(live_full_jid_disconnect_retry).await;
}

async fn live_full_jid_recipient_stamp_sender_repair(state: Arc<WebSocketState>) {
    use waddle_xmpp_core::xep0359::{add_origin_id, add_stanza_id, StanzaId, NS_SID};

    seed_local_account(&state, "bob").await;
    let full: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
    let sender: jid::BareJid = "alice@example.com".parse().expect("sender");
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    register_test_connection(&state, &full, tx).await;
    let mut conn = connection(&state, true).await;
    let mut message = xmpp_parsers::message::Message::new(Some(full.clone().into()));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Default::default(), "recipient stamp sibling".to_owned());
    add_origin_id(&mut message, "recipient-stamp-sender-repair");
    add_stanza_id(
        &mut message,
        &StanzaId::new("recipient-stamp", full.to_bare().into()),
    );
    let wire = super::super::super::transport_xml::stanza_to_xml(&Stanza::Message(message));
    handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    let outbound = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("live delivery")
        .expect("recipient channel");
    assert!(matches!(
        outbound.kind,
        waddle_xmpp::registry::DeliveryKind::PeerStanza
    ));
    let archived = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &sender,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("sender archive")
        .messages;
    assert_eq!(archived.len(), 1);
    let original = &archived[0];
    let sanitized: minidom::Element = original
        .stanza_xml
        .as_ref()
        .expect("stored stanza")
        .parse()
        .expect("stored XML");
    assert!(
        sanitized
            .children()
            .any(|element| element.is("stanza-id", NS_SID)
                && element.attr("by") == Some("bob@example.com")
                && element.attr("id") == Some("recipient-stamp")),
        "sanitization preserves the recipient-assigned sibling"
    );
    let obligations = recorded_obligations(&state).await;
    let archive_receipt_kind = obligations.iter().find(|intent| matches!(intent, IngressEffectIntent::ArchiveAuthoritative { archive, .. } if archive == &sender))
        .expect("sender archive obligation")
        .with_encoded_v1(|kind, _| kind)
        .expect("archive receipt kind");
    assert_terminal_with_receipts(&state, &obligations).await;
    {
        let db = state
            .deps
            .app_state
            .db_pool
            .global()
            .guard()
            .await
            .expect("database");
        assert_eq!(
            db.execute(
                "DELETE FROM mam_messages WHERE id = ?",
                crate::db_params![original.id.clone()]
            )
            .await
            .expect("remove sender archive"),
            1
        );
        // Require the retry to receipt the repaired sender archive itself.
        assert_eq!(
            db.execute(
                "DELETE FROM ingress_effect_receipts WHERE kind = ?",
                crate::db_params![archive_receipt_kind]
            )
            .await
            .expect("remove sender archive receipt"),
            1
        );
    }
    while rx.try_recv().is_ok() {}
    let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
    assert!(frames
        .iter()
        .all(|frame| !frame.contains("internal-server-error")));
    assert_eq!(conn.sm_state.get_inbound_count(), 2);
    assert!(!conn.sm_inbound_completion.has_unhandled_hole());
    assert!(
        rx.try_recv().is_err(),
        "no duplicate live recipient delivery"
    );
    let repaired = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &sender,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("repaired sender archive")
        .messages;
    assert_eq!(repaired.len(), 1, "retry preserves sender archive repair");
    assert_eq!(repaired[0].id, original.id);
    assert_eq!(repaired[0].stanza_id, original.stanza_id);
    assert_eq!(repaired[0].timestamp, original.timestamp);
    assert_eq!(repaired[0].body, original.body);
    assert_eq!(repaired[0].stanza_xml, original.stanza_xml);
    assert_eq!(recorded_obligations(&state).await, obligations);
    assert_terminal_with_receipts(&state, &obligations).await;
}

#[tokio::test]
async fn ingress_live_full_jid_recipient_stamp_sender_repair_sqlite() {
    live_full_jid_recipient_stamp_sender_repair(create_test_websocket_state().await).await;
}

#[tokio::test]
async fn ingress_live_full_jid_recipient_stamp_sender_repair_postgres() {
    recovery::postgres_case(live_full_jid_recipient_stamp_sender_repair).await;
}
