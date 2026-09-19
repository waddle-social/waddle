//! XEP-0198 persistence leg of #1778. The crate-private receiver authorization
//! and origin receipt-failure path are covered in execute_relay_detached_tests.
use crate::{detached_progress_support, ingress_support::IngressFixture};
use waddle_server::{
    clustering::{
        codec::RemoteStanza,
        ordered_relay::{
            OrderedRelayChannel, OrderedRelayClaim, OrderedRelayEnvelopeClaims, OrderedRelayOrigin,
            OrderedRelayPayload, OrderedRelayReceiverState, OrderedRelayRecipient,
            OrderedRelayReply, OrderedRelayReservation, OrderedRelaySenderState,
            OrderedRelaySequence, OriginInboundSequence, RemoteStanzaEnvelope,
        },
        NodeId,
    },
    ingress::{commit::commit_submission, identity::IngressAppendObligationRef},
};
use waddle_xmpp::{
    ownership::{ClaimEpoch, Entity, EntityType},
    pending_delivery::SmSessionId,
    stream_management::{
        SmIngressAppendKey, SmIngressReceiptKind, SmKeyedAppendOutcome, StreamManagementState,
    },
    Stanza,
};

pub async fn distinct_sequences_share_durable_append(fixture: IngressFixture) {
    let [target, _, _] = detached_progress_support::resources();
    let mut sm = detached_progress_support::registry(&fixture).await;
    detached_progress_support::attach(&sm, &target).await;
    let mut submission = fixture.submission(Some("1778-xep0198"), "one durable allocation");
    submission.plan.sanitized_message.to = Some(target.clone().into());
    detached_progress_support::route(&mut submission, std::slice::from_ref(&target), 1);
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit recorded route");
    let obligation = IngressAppendObligationRef {
        message_key: decision.message_key.expect("canonical key"),
        sender_bare: submission.sender.to_bare(),
        receipt: decision.external_receipts[0][0].clone(),
        received_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
    };
    let epoch = ClaimEpoch(1);
    let origin = OrderedRelayClaim {
        entity: Entity::new(EntityType::UserActor, obligation.sender_bare.to_string()),
        epoch,
    };
    let channel = OrderedRelayChannel {
        origin: OrderedRelayOrigin::Entity(origin.entity.clone()),
        recipient: OrderedRelayRecipient::FullJid(target.clone()),
        origin_epoch: epoch,
        target_epoch: epoch,
    };
    let claims = OrderedRelayEnvelopeClaims::new(
        origin.clone(),
        origin,
        OrderedRelayClaim {
            entity: Entity::new(EntityType::UserActor, target.to_bare().to_string()),
            epoch,
        },
    );
    let mut sender = OrderedRelaySenderState::default();
    let mut receiver = OrderedRelayReceiverState::default();
    for sequence in 1..=2 {
        let offered = sender
            .next_envelope(
                NodeId::new("origin-node".to_owned()),
                channel.clone(),
                OriginInboundSequence(1),
                claims.clone(),
                OrderedRelayPayload::Message {
                    recipient: target.clone().into(),
                    stanza: RemoteStanza(Stanza::Message(
                        submission.plan.sanitized_message.clone(),
                    )),
                    ingress_append: Some(obligation.clone()),
                },
            )
            .expect("next valid ordered sequence");
        assert_eq!(offered.sequence, OrderedRelaySequence(sequence));
        let wire = serde_json::to_vec(&offered).expect("serialize relay boundary");
        let received: RemoteStanzaEnvelope =
            serde_json::from_slice(&wire).expect("deserialize relay");
        // Sequence 2 MUST reserve a new delivery, even with sequence 1's ACK
        // cached. Only the persistent ledger can suppress the second allocation.
        let OrderedRelayReservation::Reserved(reserved) = receiver.reserve(received) else {
            panic!("fresh sequence must reach the append effect");
        };
        let OrderedRelayPayload::Message {
            stanza,
            ingress_append: Some(carried),
            ..
        } = &reserved.envelope().payload
        else {
            panic!("wire identity retained");
        };
        assert_eq!(carried, &obligation);
        let outcome = sm
            .record_keyed_stanza_for_detached_bound_resource(
                &target,
                &stanza.0,
                carried.received_at.expect("original receive timestamp"),
                SmIngressAppendKey {
                    message_key: carried.message_key,
                    kind: SmIngressReceiptKind::from_storage(carried.receipt.kind.to_storage()),
                    semantic_identity_hash: carried.receipt.semantic_identity_hash,
                    resource: target.clone(),
                },
            )
            .await
            .expect("persistent receiver append");
        let stream = SmSessionId::new(target.to_string());
        assert_eq!(
            outcome,
            if sequence == 1 {
                SmKeyedAppendOutcome::Appended {
                    accepting_stream: stream,
                }
            } else {
                SmKeyedAppendOutcome::AlreadyAppended {
                    accepting_stream: stream,
                }
            }
        );
        assert!(
            matches!(receiver.commit_reserved(*reserved), OrderedRelayReply::Ack(ack) if !ack.duplicate)
        );
        assert_eq!(fixture.count("sm_ingress_appends").await, 1);
        let snapshot = detached_progress_support::queued(&sm, &target).await;
        assert_eq!(snapshot.outbound_count, 1);
        assert_eq!(snapshot.unacked_stanzas.len(), 1);
        if sequence == 1 {
            drop(sm);
            sm = detached_progress_support::registry(&fixture).await;
            assert_eq!(
                sm.restore_from_persistence()
                    .await
                    .expect("restore receiver"),
                1
            );
        }
    }
    let snapshot = detached_progress_support::queued(&sm, &target).await;
    let mut stream = StreamManagementState::new();
    stream.restore_from_session(&snapshot);
    let replay = stream.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 1, "XEP-0198 h=0 replays one stored stanza");
    let replayed: minidom::Element = replay[0].stanza_xml.parse().expect("stored stanza XML");
    let expected: minidom::Element = submission.plan.sanitized_message.into();
    // Detached delivery may attach the standard delay marker; the message body
    // and addressing remain the recorded canonical values.
    assert_eq!(replayed.attr("from"), expected.attr("from"));
    assert_eq!(replayed.attr("to"), expected.attr("to"));
    assert_eq!(
        replayed.get_child("body", waddle_xmpp_core::xep0201::CLIENT_STANZA_NS),
        expected.get_child("body", waddle_xmpp_core::xep0201::CLIENT_STANZA_NS)
    );
    stream.acknowledge(1);
    assert!(stream.get_stanzas_to_resend(1).is_empty());
    drop(sm);
    fixture.close().await;
}
