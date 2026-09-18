use super::*;
use crate::ingress::{identity::IngressAppendObligationRef, EffectReceiptKey};
use crate::ingress_substrate::EffectReceiptKind;
use libp2p::identity::Keypair;
use waddle_xmpp::ingress::{IngressEffectKind, MessageKey};

fn obligation() -> IngressAppendObligationRef {
    IngressAppendObligationRef {
        message_key: MessageKey::from_storage(uuid::Uuid::from_u128(1778)),
        sender_bare: "romeo@example.test".parse().expect("sender"),
        receipt: EffectReceiptKey {
            kind: EffectReceiptKind::from_storage(IngressEffectKind::RouteDirect.storage_tag()),
            semantic_identity_hash: [17; 32],
        },
        received_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
    }
}

fn obligation_envelope() -> RemoteStanzaEnvelope {
    let recipient = "juliet@example.test/phone".parse().expect("full jid");
    let mut payload = message_payload_to("ingress-append", "juliet@example.test/phone");
    if let OrderedRelayPayload::Message { ingress_append, .. } = &mut payload {
        *ingress_append = Some(obligation());
    }
    RemoteStanzaEnvelope {
        asserted_origin_node: origin_node(),
        channel: OrderedRelayChannel {
            recipient: OrderedRelayRecipient::FullJid(recipient),
            ..channel()
        },
        sequence: OrderedRelaySequence::FIRST,
        origin_inbound_sequence: inbound(1),
        origin_claim: origin_claim(),
        sender_claim: sender_claim(),
        target_claim: target_claim(),
        payload,
        origin_proof: None,
    }
}

fn changed_obligations() -> Vec<IngressAppendObligationRef> {
    let original = obligation();
    vec![
        IngressAppendObligationRef {
            message_key: MessageKey::from_storage(uuid::Uuid::from_u128(1779)),
            ..original.clone()
        },
        IngressAppendObligationRef {
            sender_bare: "other@example.test".parse().expect("other sender"),
            ..original.clone()
        },
        IngressAppendObligationRef {
            receipt: EffectReceiptKey {
                kind: EffectReceiptKind::from_storage(
                    IngressEffectKind::RouteMucGroupchat.storage_tag(),
                ),
                ..original.receipt.clone()
            },
            ..original.clone()
        },
        IngressAppendObligationRef {
            receipt: EffectReceiptKey {
                semantic_identity_hash: [18; 32],
                ..original.receipt.clone()
            },
            ..original.clone()
        },
        IngressAppendObligationRef {
            received_at: chrono::DateTime::from_timestamp(1_700_000_001, 0),
            ..original
        },
    ]
}

fn replace_obligation(envelope: &mut RemoteStanzaEnvelope, obligation: IngressAppendObligationRef) {
    let OrderedRelayPayload::Message { ingress_append, .. } = &mut envelope.payload else {
        panic!("message fixture");
    };
    *ingress_append = Some(obligation);
}

#[test]
fn ingress_append_obligation_survives_envelope_serde_round_trip() {
    let envelope = obligation_envelope();
    assert!(envelope_is_consistent(&envelope));
    let encoded = serde_json::to_vec(&envelope).expect("serialize envelope");
    let decoded: RemoteStanzaEnvelope = serde_json::from_slice(&encoded).expect("decode envelope");
    assert!(envelope_is_consistent(&decoded));
    let OrderedRelayPayload::Message { ingress_append, .. } = &decoded.payload else {
        panic!("decoded message");
    };
    assert_eq!(ingress_append.as_ref(), Some(&obligation()));
    assert_eq!(
        envelope.signing_bytes().unwrap(),
        decoded.signing_bytes().unwrap()
    );
}

#[test]
fn changing_ingress_append_obligation_invalidates_existing_signature() {
    let keypair = Keypair::generate_ed25519();
    let envelope = obligation_envelope();
    let signing_bytes = envelope.signing_bytes().expect("signing bytes");
    let signature = keypair.sign(&signing_bytes).expect("sign envelope");
    assert!(keypair.public().verify(&signing_bytes, &signature));

    for obligation in changed_obligations() {
        let mut changed = envelope.clone();
        replace_obligation(&mut changed, obligation);
        let changed_bytes = changed.signing_bytes().expect("changed signing bytes");
        assert_ne!(signing_bytes, changed_bytes);
        assert!(!keypair.public().verify(&changed_bytes, &signature));
    }
}

fn assert_parse_failure_without_reservation(
    receiver: &mut OrderedRelayReceiverState,
    envelope: RemoteStanzaEnvelope,
) {
    // Delivery is allowed only by Reserved; a completed NACK cannot run an effect.
    assert!(matches!(
        receiver.reserve(envelope),
        OrderedRelayReservation::Completed(OrderedRelayReply::Nack(OrderedRelayNack {
            reason: OrderedRelayNackReason::ParseFailure,
            ..
        }))
    ));
}

#[test]
fn receiver_nacks_changed_ingress_append_obligation_while_pending_and_after_ack() {
    for obligation in changed_obligations() {
        let mut receiver = OrderedRelayReceiverState::default();
        let envelope = obligation_envelope();
        let mut changed = envelope.clone();
        replace_obligation(&mut changed, obligation);
        assert!(
            envelope_is_consistent(&changed),
            "test fingerprint, not structure"
        );
        let OrderedRelayReservation::Reserved(reserved) = receiver.reserve(envelope.clone()) else {
            panic!("original delivery must be reserved");
        };
        assert_parse_failure_without_reservation(&mut receiver, changed.clone());
        assert!(matches!(
            receiver.commit_reserved(*reserved),
            OrderedRelayReply::Ack(OrderedRelayAck {
                duplicate: false,
                ..
            })
        ));
        assert_parse_failure_without_reservation(&mut receiver, changed);
        assert!(matches!(
            receiver.reserve(envelope),
            OrderedRelayReservation::Completed(OrderedRelayReply::Ack(OrderedRelayAck {
                duplicate: true,
                next_expected: OrderedRelaySequence(2),
                ..
            }))
        ));
    }
}

#[test]
fn receiver_rejects_ingress_append_obligation_on_bare_recipient_before_effects() {
    let mut envelope = obligation_envelope();
    envelope.channel = channel();
    envelope.payload = message_payload("ingress-append");
    assert!(envelope_is_consistent(&envelope));
    replace_obligation(&mut envelope, obligation());
    assert!(!envelope_is_consistent(&envelope));
    let mut receiver = OrderedRelayReceiverState::default();
    assert_parse_failure_without_reservation(&mut receiver, envelope);
    assert!(receiver.pending_by_channel.is_empty());
}

#[test]
fn receiver_rejects_ineligible_ingress_append_obligation_before_effects() {
    let mut envelope = obligation_envelope();
    assert!(envelope_is_consistent(&envelope));
    let mut ineligible = obligation();
    ineligible.receipt.kind =
        EffectReceiptKind::from_storage(IngressEffectKind::ArchiveAuthoritative.storage_tag());
    replace_obligation(&mut envelope, ineligible);
    assert!(!envelope_is_consistent(&envelope));
    let mut receiver = OrderedRelayReceiverState::default();
    assert_parse_failure_without_reservation(&mut receiver, envelope);
    assert!(receiver.pending_by_channel.is_empty());
}
