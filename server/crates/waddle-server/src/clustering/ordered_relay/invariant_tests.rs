use super::*;
use std::str::FromStr;
use waddle_xmpp::ownership::EntityType;
use xmpp_parsers::message::{Lang, Message};

fn channel() -> OrderedRelayChannel {
    channel_for_bare("juliet@example.test")
}

fn channel_for_bare(bare: &str) -> OrderedRelayChannel {
    OrderedRelayChannel {
        origin: OrderedRelayOrigin::SmSession(SmSessionId::new("stream-1")),
        recipient: OrderedRelayRecipient::BareJid(jid::BareJid::from_str(bare).expect("bare jid")),
        target_epoch: ClaimEpoch(3),
    }
}

fn origin_node() -> NodeId {
    NodeId::new("origin-node".to_string())
}

fn inbound(sequence: u32) -> OriginInboundSequence {
    OriginInboundSequence(sequence)
}

fn claim(entity_type: EntityType, id: &str, epoch: i64) -> OrderedRelayClaim {
    OrderedRelayClaim {
        entity: Entity::new(entity_type, id),
        epoch: ClaimEpoch(epoch),
    }
}

fn origin_claim() -> OrderedRelayClaim {
    claim(EntityType::SmSession, "stream-1", 7)
}

fn sender_claim() -> OrderedRelayClaim {
    claim(EntityType::UserActor, "romeo@example.test", 5)
}

fn target_claim() -> OrderedRelayClaim {
    claim(EntityType::UserActor, "juliet@example.test", 3)
}

fn target_claim_for_bare(bare: &str) -> OrderedRelayClaim {
    claim(EntityType::UserActor, bare, 3)
}

fn claims() -> OrderedRelayEnvelopeClaims {
    OrderedRelayEnvelopeClaims::new(origin_claim(), sender_claim(), target_claim())
}

fn claims_for_target(target: OrderedRelayClaim) -> OrderedRelayEnvelopeClaims {
    OrderedRelayEnvelopeClaims::new(origin_claim(), sender_claim(), target)
}

fn message_payload(id: &str) -> OrderedRelayPayload {
    message_payload_to(id, "juliet@example.test")
}

fn message_payload_to(id: &str, recipient: &str) -> OrderedRelayPayload {
    let mut stanza = Message::new(Some(jid::Jid::from_str(recipient).expect("jid")));
    stanza.from = Some(jid::Jid::from_str("romeo@example.test/home").expect("jid"));
    stanza.id = Some(xmpp_parsers::message::Id(id.to_string()));
    stanza.bodies.insert(Lang::new(), format!("payload {id}"));
    OrderedRelayPayload::Message {
        recipient: jid::Jid::from_str(recipient).expect("jid"),
        stanza: RemoteStanza(waddle_xmpp::Stanza::Message(stanza)),
    }
}

#[test]
fn sender_diverts_after_sequence_space_is_exhausted() {
    let mut state = OrderedRelaySenderState::default();
    let channel = channel();
    state
        .next_by_channel
        .insert(channel.clone(), OrderedRelaySequence(u64::MAX));

    let blocked = state.next_envelope(
        origin_node(),
        channel,
        inbound(u32::MAX),
        claims(),
        message_payload("after-max"),
    );
    assert!(matches!(
        blocked,
        Err(OrderedRelayDiversion {
            reason: OrderedRelayDiversionReason::Backpressure,
            ..
        })
    ));
}

#[test]
fn sender_overflow_channels_do_not_grow_diversion_state_unbounded() {
    let mut state = OrderedRelaySenderState::default();
    for index in 0..MAX_TRACKED_ORDERED_RELAY_CHANNELS {
        state.next_by_channel.insert(
            channel_for_bare(&format!("user-{index}@example.test")),
            OrderedRelaySequence::FIRST,
        );
    }

    for index in 0..16 {
        let overflow = format!("overflow-{index}@example.test");
        let result = state.next_envelope(
            origin_node(),
            channel_for_bare(&overflow),
            inbound(index),
            claims_for_target(target_claim_for_bare(&overflow)),
            message_payload_to("overflow", &overflow),
        );
        assert!(matches!(
            result,
            Err(OrderedRelayDiversion {
                reason: OrderedRelayDiversionReason::Backpressure,
                ..
            })
        ));
    }

    assert!(state.diversions.is_empty());
    assert!(state.new_channels_diverted);
}
