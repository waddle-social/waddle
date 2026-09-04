use super::*;
use std::str::FromStr;

#[test]
fn muc_proxy_kind_validates_the_carried_stanza_kind() {
    let mut receiver = OrderedRelayReceiverState::default();
    let envelope = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: room_channel(),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: room_claim(),
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::JoinPresence,
            origin: connection_origin(1),
            stanza: presence_stanza(),
        },
        origin_proof: None,
    };

    assert!(matches!(
        receive(&mut receiver, envelope),
        OrderedRelayReply::Ack(OrderedRelayAck {
            duplicate: false,
            next_expected: OrderedRelaySequence(2),
            ..
        })
    ));

    let wrong_kind = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: room_channel(),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: room_claim(),
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::GroupchatMessage,
            origin: MucProxyOrigin::Server,
            stanza: presence_stanza(),
        },
        origin_proof: None,
    };

    let mut fresh_receiver = OrderedRelayReceiverState::default();
    assert!(matches!(
        receive(&mut fresh_receiver, wrong_kind),
        OrderedRelayReply::Nack(OrderedRelayNack {
            reason: OrderedRelayNackReason::ParseFailure,
            ..
        })
    ));
}
/// #1597: the channel lane is part of envelope validation. A Muji
/// IQ smuggled onto the MUC stanza lane (or a groupchat message
/// onto the Muji lane) must be rejected as inconsistent, so lane
/// isolation cannot be bypassed by mislabeling the channel.
#[test]
fn room_lane_must_match_the_muc_proxy_kind() {
    let mut receiver = OrderedRelayReceiverState::default();
    let mut muji_on_muc_lane = muji_envelope(
        OrderedRelayMucProxyKind::MujiJingleIq,
        muji_initiate_stanza(
            xmpp_parsers::jingle::Action::SessionInitiate,
            "room@example.test",
        ),
    );
    muji_on_muc_lane.channel = room_channel_for_lane(OrderedRelayRoomLane::MucStanza);
    assert!(
        matches!(
            receive(&mut receiver, muji_on_muc_lane),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ),
        "a Muji IQ on the MUC stanza lane must be rejected"
    );

    let mut receiver = OrderedRelayReceiverState::default();
    let groupchat_on_muji_lane = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: room_channel_for_lane(OrderedRelayRoomLane::MujiSignaling),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: room_claim(),
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::GroupchatMessage,
            origin: MucProxyOrigin::Server,
            stanza: groupchat_stanza_to("room@example.test"),
        },
        origin_proof: None,
    };
    assert!(
        matches!(
            receive(&mut receiver, groupchat_on_muji_lane),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ),
        "a groupchat message on the Muji lane must be rejected"
    );
}
#[test]
fn muji_jingle_iq_validation_binds_payload_room_to_channel_room() {
    let mut receiver = OrderedRelayReceiverState::default();
    assert!(
        matches!(
            receive(
                &mut receiver,
                muji_envelope(
                    OrderedRelayMucProxyKind::MujiJingleIq,
                    muji_initiate_stanza(
                        xmpp_parsers::jingle::Action::SessionInitiate,
                        "room@example.test"
                    ),
                ),
            ),
            OrderedRelayReply::Ack(OrderedRelayAck { .. })
        ),
        "a session-initiate whose <muji room> matches the channel room must validate"
    );

    let mut receiver = OrderedRelayReceiverState::default();
    assert!(
        matches!(
            receive(
                &mut receiver,
                muji_envelope(
                    OrderedRelayMucProxyKind::MujiJingleIq,
                    muji_initiate_stanza(
                        xmpp_parsers::jingle::Action::SessionInitiate,
                        "other-room@example.test"
                    ),
                ),
            ),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ),
        "a <muji room> naming a different room than the channel must be rejected"
    );
}
/// `session-terminate` rides the same kind as `session-initiate`
/// (#1445): the initiate registers the participant on the room
/// owner, so the terminate must reach that same node to unregister
/// them — a locally-executed terminate would leave a phantom
/// in-call participant there.
#[test]
fn muji_jingle_iq_validation_accepts_terminate_for_the_channel_room() {
    let mut receiver = OrderedRelayReceiverState::default();
    assert!(
        matches!(
            receive(
                &mut receiver,
                muji_envelope(
                    OrderedRelayMucProxyKind::MujiJingleIq,
                    muji_initiate_stanza(
                        xmpp_parsers::jingle::Action::SessionTerminate,
                        "room@example.test"
                    ),
                ),
            ),
            OrderedRelayReply::Ack(OrderedRelayAck { .. })
        ),
        "a terminate for the channel's room must relay to the owner"
    );

    let mut receiver = OrderedRelayReceiverState::default();
    assert!(
        matches!(
            receive(
                &mut receiver,
                muji_envelope(
                    OrderedRelayMucProxyKind::MujiJingleIq,
                    muji_initiate_stanza(
                        xmpp_parsers::jingle::Action::SessionTerminate,
                        "other-room@example.test"
                    ),
                ),
            ),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ),
        "the payload room must stay bound to the channel room on terminate"
    );
}
#[test]
fn muji_jingle_iq_validation_rejects_other_actions_and_non_muji_iqs() {
    let mut receiver = OrderedRelayReceiverState::default();
    assert!(
        matches!(
            receive(
                &mut receiver,
                muji_envelope(
                    OrderedRelayMucProxyKind::MujiJingleIq,
                    muji_initiate_stanza(
                        xmpp_parsers::jingle::Action::SessionInfo,
                        "room@example.test"
                    ),
                ),
            ),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ),
        "only initiate and terminate have owner-node side effects"
    );

    let plain_jingle = {
        let payload: xmpp_parsers::minidom::Element = xmpp_parsers::jingle::Jingle::new(
            xmpp_parsers::jingle::Action::SessionInitiate,
            xmpp_parsers::jingle::SessionId("p2p-sid".into()),
        )
        .into();
        RemoteStanza(waddle_xmpp::Stanza::Iq(Box::new(
            xmpp_parsers::iq::Iq::Set {
                from: Some(jid::Jid::from_str("romeo@example.test/phone").expect("full jid")),
                to: Some(jid::Jid::from_str("calls.example.test").expect("mixer jid")),
                id: "p2p-iq".into(),
                payload,
            },
        )))
    };
    let mut receiver = OrderedRelayReceiverState::default();
    assert!(
        matches!(
            receive(
                &mut receiver,
                muji_envelope(OrderedRelayMucProxyKind::MujiJingleIq, plain_jingle),
            ),
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ParseFailure,
                ..
            })
        ),
        "a Jingle IQ without a <muji/> child (1:1 call) must never ride this kind"
    );
}
#[test]
fn muc_proxy_validation_rejects_malformed_xep0045_shapes() {
    let unavailable_join = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: room_channel(),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: room_claim(),
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::JoinPresence,
            origin: connection_origin(1),
            stanza: presence_stanza_to(
                "room@example.test/romeo",
                xmpp_parsers::presence::Type::Unavailable,
            ),
        },
        origin_proof: None,
    };
    let bare_groupchat = RemoteStanzaEnvelope {
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::GroupchatMessage,
            origin: MucProxyOrigin::Server,
            stanza: groupchat_stanza_to("room@example.test"),
        },
        ..unavailable_join.clone()
    };
    let full_groupchat = RemoteStanzaEnvelope {
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::GroupchatMessage,
            origin: MucProxyOrigin::Server,
            stanza: groupchat_stanza_to("room@example.test/romeo"),
        },
        ..unavailable_join.clone()
    };

    let mut receiver = OrderedRelayReceiverState::default();
    assert!(matches!(
        receive(&mut receiver, unavailable_join),
        OrderedRelayReply::Nack(OrderedRelayNack {
            reason: OrderedRelayNackReason::ParseFailure,
            ..
        })
    ));
    assert!(matches!(
        receive(&mut receiver, bare_groupchat),
        OrderedRelayReply::Ack(OrderedRelayAck { .. })
    ));
    let mut receiver = OrderedRelayReceiverState::default();
    assert!(matches!(
        receive(&mut receiver, full_groupchat),
        OrderedRelayReply::Nack(OrderedRelayNack {
            reason: OrderedRelayNackReason::ParseFailure,
            ..
        })
    ));
}

#[test]
fn muc_proxy_origin_must_match_kind_policy() {
    let mut receiver = OrderedRelayReceiverState::default();
    let join_with_server_origin = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: room_channel(),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: room_claim(),
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::JoinPresence,
            origin: MucProxyOrigin::Server,
            stanza: presence_stanza(),
        },
        origin_proof: None,
    };
    assert!(matches!(
        receive(&mut receiver, join_with_server_origin),
        OrderedRelayReply::Nack(OrderedRelayNack {
            reason: OrderedRelayNackReason::ParseFailure,
            ..
        })
    ));

    let mut receiver = OrderedRelayReceiverState::default();
    let server_kind_with_connection_origin = RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: room_channel(),
        sequence: OrderedRelaySequence(1),
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: room_claim(),
        payload: OrderedRelayPayload::MucProxy {
            room_jid: room_jid(),
            kind: OrderedRelayMucProxyKind::GroupchatMessage,
            origin: connection_origin(2),
            stanza: groupchat_stanza_to("room@example.test"),
        },
        origin_proof: None,
    };
    assert!(matches!(
        receive(&mut receiver, server_kind_with_connection_origin),
        OrderedRelayReply::Nack(OrderedRelayNack {
            reason: OrderedRelayNackReason::ParseFailure,
            ..
        })
    ));
}
