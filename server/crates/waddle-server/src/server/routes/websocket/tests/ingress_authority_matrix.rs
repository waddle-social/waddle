use super::*;
use crate::ingress::IngressDecisionClass;
use waddle_xmpp::ingress::ProtocolEpoch;

/// XEP-0198 §4 and §6: every uncommitted decision retains sender responsibility.
#[tokio::test]
async fn non_advancing_failure_matrix_keeps_wire_ack_and_rows_unchanged() {
    non_advancing_matrix_case(create_test_websocket_state().await).await;
}

/// XEP-0198 §4–§6: PostgreSQL failures retain sender responsibility and unchanged rows.
#[tokio::test]
async fn non_advancing_failure_matrix_keeps_wire_ack_and_rows_unchanged_postgres() {
    postgres_case(non_advancing_matrix_case).await;
}

async fn non_advancing_matrix_case(state: Arc<WebSocketState>) {
    let failures = [
        (
            IngressUowError::PrincipalAssertionFailed,
            IngressDecisionClass::PrincipalMissing,
        ),
        #[cfg(feature = "clustering")]
        (
            IngressUowError::ClaimFenceMissing,
            IngressDecisionClass::ClaimFenceMissing,
        ),
        (
            IngressUowError::RoomGenerationStale,
            IngressDecisionClass::RoomGenerationStale,
        ),
        (
            IngressUowError::IngressFrontierStale,
            IngressDecisionClass::FrontierStale,
        ),
        (
            IngressUowError::EffectIntentConflict,
            IngressDecisionClass::IntentContradiction,
        ),
        (
            IngressUowError::EffectIntentMessageMissing,
            IngressDecisionClass::Storage,
        ),
        (IngressUowError::Timeout, IngressDecisionClass::Timeout),
        (
            IngressUowError::AmbiguousCommit,
            IngressDecisionClass::AmbiguousCommit,
        ),
        (
            IngressUowError::EpochUnsupported {
                live: ProtocolEpoch::from_storage(1),
                supported: ProtocolEpoch::ZERO,
            },
            IngressDecisionClass::EpochUnsupported,
        ),
        (
            IngressUowError::Substrate(
                crate::ingress_substrate::IngressSubstrateError::SmOrdinalConflict,
            ),
            IngressDecisionClass::SmOrdinalConflict,
        ),
        (
            IngressUowError::Lineage(crate::db::DatabaseError::ConnectionFailed(
                "injected lineage failure".into(),
            )),
            IngressDecisionClass::Lineage,
        ),
    ];
    for (failure, expected) in failures {
        let mut conn = connection(&state, true).await;
        assert_ack(&state, &mut conn, 0).await;
        let observed = commit_hooks::FAILURE
            .scope(
                std::cell::RefCell::new(Some(failure)),
                observed_dispatch(&state, &mut conn, &offered_message()),
            )
            .await;
        assert_eq!(observed, expected);
        assert!(!observed.advances());
        assert!(conn.sm_inbound_completion.has_unhandled_hole());
        assert!(conn.sm_state.is_resumable());
        assert_ack(&state, &mut conn, 0).await;
        for table in [
            "ingress_messages",
            "ingress_sm_refs",
            "ingress_effect_intents",
            "ingress_effect_receipts",
        ] {
            assert_eq!(row_count(&state, table).await, 0, "{table}");
        }
    }
}

/// XEP-0461 Use Cases and RFC 0018 §3.5: malformed references are stanza denials.
#[tokio::test]
async fn semantic_malformed_replies_commit_on_ephemeral_and_resumable_connections() {
    malformed_reply_case(create_test_websocket_state().await).await;
}

#[tokio::test]
async fn semantic_malformed_replies_commit_on_ephemeral_and_resumable_connections_postgres() {
    postgres_case(malformed_reply_case).await;
}

async fn malformed_reply_case(state: Arc<WebSocketState>) {
    for resumable in [false, true] {
        let mut conn = connection(&state, resumable).await;
        let mut message = xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse().expect("room"),
        ));
        message.type_ = xmpp_parsers::message::MessageType::Groupchat;
        message.id = Some(xmpp_parsers::message::Id("malformed-reply".into()));
        message.payloads.push(
            minidom::Element::builder("reply", waddle_xmpp::xep::xep0461::NS_REPLY)
                .attr(minidom::rxml::xml_ncname!("to").to_owned(), " ")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "parent-1")
                .build(),
        );
        let wire = crate::server::routes::websocket::transport_xml::stanza_to_xml(
            &Stanza::Message(message.clone()),
        );
        commit_hooks::OBSERVED_CLASS
            .scope(std::cell::Cell::new(None), async {
                let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
                assert_eq!(
                    commit_hooks::OBSERVED_CLASS.with(std::cell::Cell::get),
                    Some(IngressDecisionClass::SemanticMalformed)
                );
                assert_eq!(frames.len(), 1);
                let reply = xmpp_parsers::message::Message::try_from(
                    frames[0].parse::<minidom::Element>().expect("reply XML"),
                )
                .expect("standard message reply");
                assert_eq!(reply.type_, xmpp_parsers::message::MessageType::Error);
                assert_eq!(reply.id, message.id);
                assert_eq!(
                    reply.to,
                    Some("alice@example.com/web".parse().expect("sender"))
                );
                assert!(reply.payloads.iter().any(|payload| payload
                    .get_child("bad-request", xmpp_parsers::ns::XMPP_STANZAS)
                    .is_some()));
            })
            .await;
        assert!(!conn.sm_inbound_completion.has_unhandled_hole());
        if resumable {
            assert_ack(&state, &mut conn, 1).await;
        }
        // RFC 6120 §8.3.1 forbids replying to an error with another error.
        message.type_ = xmpp_parsers::message::MessageType::Error;
        let wire = crate::server::routes::websocket::transport_xml::stanza_to_xml(
            &Stanza::Message(message),
        );
        let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
        assert!(frames.is_empty());
        assert!(!conn.sm_inbound_completion.has_unhandled_hole());
        if resumable {
            assert_ack(&state, &mut conn, 2).await;
        }
    }
    assert_eq!(row_count(&state, "ingress_messages").await, 4);
    assert_eq!(row_count(&state, "ingress_effect_intents").await, 2);
    assert_eq!(row_count(&state, "ingress_sm_refs").await, 2);
    assert_eq!(row_count(&state, "mam_messages").await, 0);
}
