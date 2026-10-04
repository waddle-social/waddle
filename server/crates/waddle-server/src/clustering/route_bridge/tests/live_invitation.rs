//! Generated room messages keep their frozen authority at the registered socket.
use super::*;
use crate::server::routes::websocket::handlers::message::{
    group_dm_invite, muc_direct, muc_invite,
};
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, GroupDmHistoryVisibility, GroupDmMembershipGrant,
    MucInviteLedgerAction, MucInviteLedgerMutation, NormalizedTarget, PendingDeliveryMutation,
};
use waddle_xmpp::pending_delivery::PendingRowId;

#[derive(Clone, Copy, Debug)]
enum GeneratedKind {
    Invite,
    Decline,
    GroupFull,
    GroupFromJoin,
}

async fn generated_frame(
    fixture: &IngressFixture,
    socket: &mut SocketFixture,
    kind: GeneratedKind,
) -> Message {
    let room: jid::BareJid = "invitation@muc.example.com".parse().expect("room");
    let target = socket.frame.frame.jid.clone();
    let source = fixture.principal.bare_jid().clone();
    let recipient = target.to_bare();
    let decline = matches!(kind, GeneratedKind::Decline);
    let inbound = minidom::Element::builder(
        if decline { "decline" } else { "invite" },
        waddle_xmpp::muc::presence::NS_MUC_USER,
    )
    .attr(
        minidom::rxml::xml_ncname!("to").to_owned(),
        recipient.to_string(),
    )
    .append(
        minidom::Element::builder("reason", waddle_xmpp::muc::presence::NS_MUC_USER)
            .append("recorded reason")
            .build(),
    )
    .build();
    let mut submission = fixture.submission(None, "");
    submission.target = NormalizedTarget::Bare(room.clone());
    let incoming = &mut submission.plan.sanitized_message;
    incoming.to = Some(room.clone().into());
    incoming.type_ = xmpp_parsers::message::MessageType::Normal;
    incoming.id = Some(xmpp_parsers::message::Id(uuid::Uuid::new_v4().to_string()));
    incoming.bodies.clear();
    incoming.payloads =
        vec![
            minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(inbound.clone())
                .build(),
        ];
    submission.digest_input = DigestInput::from_parsed(
        incoming,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![source.clone()],
            stanza_lang: None,
        },
    )
    .expect("canonical invitation digest");
    let route = IngressEffectIntent::RouteDirect {
        recipient: recipient.clone(),
        fanout: vec![target],
        route_identity: EffectMessageIdentity::capture_ordinal(7),
    };
    let receipt = crate::ingress::receipt_key(&route).expect("route receipt");
    submission.plan.intents = vec![
        route,
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient: recipient.clone(),
                row_id: PendingRowId::fresh(),
            },
        },
        IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room: room.clone(),
                inviter: if decline {
                    recipient.clone()
                } else {
                    source.clone()
                },
                invitee: if decline {
                    source.clone()
                } else {
                    recipient.clone()
                },
                action: if decline {
                    MucInviteLedgerAction::Claimed
                } else {
                    MucInviteLedgerAction::Recorded
                },
                recorded_at: Some(chrono::Utc::now()),
            },
        },
    ];
    let message = match kind {
        GeneratedKind::Invite => {
            muc_invite::mediated_invite_message(incoming, &room, &source, &recipient, &inbound)
        }
        GeneratedKind::Decline => {
            muc_direct::mediated_decline_message(incoming, &room, &source, &recipient, &inbound)
        }
        GeneratedKind::GroupFull | GeneratedKind::GroupFromJoin => {
            let grant = GroupDmMembershipGrant {
                room: room.clone(),
                inviter: source,
                invitee: recipient,
                history_visibility: match kind {
                    GeneratedKind::GroupFull => GroupDmHistoryVisibility::Full,
                    _ => GroupDmHistoryVisibility::FromJoin {
                        visible_after: chrono::Utc::now(),
                    },
                },
            };
            let message = group_dm_invite::recorded_invite_message(incoming, &grant, &inbound);
            submission
                .plan
                .intents
                .push(IngressEffectIntent::GroupDmMembershipGrant { grant });
            message
        }
    };
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("record generated delivery");
    socket.frame.frame.kind = DeliveryKind::DirectFrame;
    socket.frame.frame.stanza = RemoteStanza(Stanza::Message(message.clone()));
    socket.frame.frame.ingress_append = Some(IngressAppendObligationRef {
        message_key: decision.message_key.expect("canonical key"),
        sender_bare: room,
        receipt,
        received_at: None,
        archive_positions: vec![],
        dispatch_stream: None,
    });
    message
}

async fn generated_delivery_is_exact_and_retry_safe(fixture: IngressFixture) {
    for kind in [
        GeneratedKind::Invite,
        GeneratedKind::Decline,
        GeneratedKind::GroupFull,
        GeneratedKind::GroupFromJoin,
    ] {
        let mut socket = SocketFixture::new(&fixture).await;
        let expected = generated_frame(&fixture, &mut socket, kind).await;
        let authorized = socket.frame.clone();
        let before = fixture.count("ingress_send_attempts").await;
        for omit_payload in [false, true] {
            let mut forged = expected.clone();
            if omit_payload {
                forged.payloads.clear();
            } else {
                forged
                    .bodies
                    .insert(Default::default(), "unrecorded body".into());
            }
            socket.frame.frame.stanza = RemoteStanza(Stanza::Message(forged));
            assert_eq!(
                socket.deliver().await,
                RelayRemoteResourceFrameStatus::Backpressure,
                "{kind:?}: generated payload tampering must be rejected before enqueue"
            );
            assert!(socket.receiver.try_recv().is_err());
            assert_eq!(fixture.count("ingress_send_attempts").await, before);
        }
        socket.frame = authorized;
        assert_eq!(
            socket.deliver().await,
            RelayRemoteResourceFrameStatus::Delivered,
            "{kind:?}: exact room-authored message must be accepted"
        );
        let outbound = socket
            .receiver
            .try_recv()
            .expect("one generated invitation copy");
        assert_eq!(
            outbound.stanza.to_element(),
            Stanza::Message(expected).to_element()
        );
        assert!(
            outbound.ingress_append.is_some(),
            "accepted frame retains custody authority"
        );
        assert_eq!(
            fixture.count("ingress_effect_receipts").await,
            0,
            "simulate missing outer execution receipt"
        );
        assert_eq!(
            socket.deliver().await,
            RelayRemoteResourceFrameStatus::Delivered
        );
        assert!(
            socket.receiver.try_recv().is_err(),
            "{kind:?}: retry repairs proof without another copy"
        );
        socket.close().await;
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_remote_generated_invitations_reject_tampering_and_suppress_receipt_retry() {
    generated_delivery_is_exact_and_retry_safe(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_remote_generated_invitations_reject_tampering_and_suppress_receipt_retry() {
    if let Some(fixture) = IngressFixture::postgres("remote_generated_invites").await {
        generated_delivery_is_exact_and_retry_safe(fixture).await;
    }
}
