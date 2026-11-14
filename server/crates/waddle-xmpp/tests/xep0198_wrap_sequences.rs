//! XEP-0198 wrapping counter behavior for detached-session persistence.

use std::time::Instant;

use chrono::Utc;
use proptest::prelude::{any, prop_assert, prop_assert_eq, proptest};
use waddle_xmpp::stream_management::{DetachedSession, StreamManagementState};
use waddle_xmpp::telemetry::attributes::SmEvictionPath;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Id, Message};

fn detached_session(last_acked: u32, outbound_count: u32) -> DetachedSession {
    DetachedSession {
        promotion_custody: waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
        stream_id: "wrap-sequences".to_string(),
        user_id: "user@example.com".to_string(),
        jid: "user@example.com/resource".parse().expect("valid full JID"),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count,
        last_acked,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

fn message_stanza(id: &str) -> Stanza {
    let mut message = Message::new(None::<jid::Jid>);
    message.id = Some(Id(id.to_string()));
    Stanza::Message(message)
}

fn message_xml(id: &str) -> String {
    let element = message_stanza(id).to_element();
    let mut bytes = Vec::new();
    element
        .write_to(&mut bytes)
        .expect("serialize message stanza");
    String::from_utf8(bytes).expect("message XML is UTF-8")
}

fn sequence_numbers(session: &DetachedSession) -> Vec<u32> {
    session
        .unacked_stanzas
        .iter()
        .map(|entry| entry.sequence)
        .collect()
}

#[test]
fn exact_window_predicates_wrap_and_reject_antipodes() {
    let mut state = StreamManagementState::new();
    state.last_acked = u32::MAX - 1;
    state.outbound_count = 2;

    for h in [u32::MAX - 1, u32::MAX, 0, 2] {
        assert!(!state.ack_exceeds_outbound(h));
    }
    assert!(state.ack_exceeds_outbound(3));

    state.last_acked = 2;
    state.outbound_count = 2;
    assert!(state.ack_exceeds_outbound(2u32.wrapping_add(0x8000_0000)));

    let mut detached = detached_session(u32::MAX - 1, 2);
    for h in [u32::MAX - 1, u32::MAX, 0, 2] {
        assert!(!detached.handled_count_exceeds_outbound(h));
    }
    assert!(detached.handled_count_exceeds_outbound(3));

    detached.last_acked = 2;
    detached.outbound_count = 2;
    assert!(detached.handled_count_exceeds_outbound(2u32.wrapping_add(0x8000_0000)));

    detached.last_acked = 0;
    assert!(!detached.can_resume_from(u32::MAX));
    assert!(detached.can_resume_from(0));
    // At the exact antipode neither direction is strictly greater. The
    // lower-bound comparator intentionally treats this degenerate input as
    // non-regressed; exact-window predicates above reject it separately.
    assert!(detached.can_resume_from(0x8000_0000));

    let mut state = StreamManagementState::with_config(1, 1);
    state.outbound_count = u32::MAX - 1;
    let _ = state.record_outbound("max".to_string(), SmEvictionPath::DirectOutbound);
    let _ = state.record_outbound("zero".to_string(), SmEvictionPath::DirectOutbound);
    assert!(!state.can_resume_from(u32::MAX - 1));
    assert!(state.can_resume_from(u32::MAX));
    assert!(state.can_resume_from(0x7fff_ffff));
}

proptest! {
    #[test]
    fn exact_window_predicates_follow_the_modulo_interval(
        last_acked in any::<u32>(),
        outbound_offset in 0u32..0x8000_0000,
        h in any::<u32>(),
    ) {
        let outbound_count = last_acked.wrapping_add(outbound_offset);
        let expected = h.wrapping_sub(last_acked) > outbound_offset;

        let mut state = StreamManagementState::new();
        state.last_acked = last_acked;
        state.outbound_count = outbound_count;
        prop_assert_eq!(state.ack_exceeds_outbound(h), expected);

        let detached = detached_session(last_acked, outbound_count);
        prop_assert_eq!(detached.handled_count_exceeds_outbound(h), expected);
    }

    #[test]
    fn resume_rejects_a_non_antipode_regression(last_acked in any::<u32>(), behind in 1u32..0x8000_0000) {
        let detached = detached_session(last_acked, last_acked);
        prop_assert!(!detached.can_resume_from(last_acked.wrapping_sub(behind)));
    }
}

#[test]
fn explicit_detached_sequences_advance_sort_deduplicate_and_reject_stale_inputs() {
    let last_acked = u32::MAX - 2;
    let mut session = detached_session(last_acked, u32::MAX - 1);

    session.record_detached_outbound_at(0, &message_stanza("zero"), Utc::now());
    session.record_detached_outbound_at(u32::MAX, &message_stanza("max"), Utc::now());
    assert_eq!(session.outbound_count, 0);
    assert_eq!(sequence_numbers(&session), vec![u32::MAX, 0]);

    let before_duplicate = session.clone();
    session.record_detached_outbound_at(0, &message_stanza("different duplicate"), Utc::now());
    assert_eq!(session.outbound_count, before_duplicate.outbound_count);
    assert_eq!(session.unacked_stanzas, before_duplicate.unacked_stanzas);
    assert_eq!(
        session.replay_gap_through,
        before_duplicate.replay_gap_through
    );

    let before_stale = session.clone();
    session.record_detached_outbound_at(last_acked, &message_stanza("acked"), Utc::now());
    session.record_detached_outbound_at(
        last_acked.wrapping_sub(1),
        &message_stanza("behind ack floor"),
        Utc::now(),
    );
    assert_eq!(session.outbound_count, before_stale.outbound_count);
    assert_eq!(session.unacked_stanzas, before_stale.unacked_stanzas);
    assert_eq!(session.replay_gap_through, before_stale.replay_gap_through);
}

#[test]
fn wrapping_ack_and_repeated_replay_keep_exact_frame_provenance() {
    use waddle_xmpp::ingress::MessageKey;
    use waddle_xmpp::stream_management::{
        SmFrameProvenance, SmIngressAppendKey, SmIngressReceiptKind,
    };
    let key = |hash| SmIngressAppendKey {
        message_key: MessageKey::new(),
        kind: SmIngressReceiptKind::from_storage(1),
        semantic_identity_hash: [hash; 32],
        resource: "user@example.com/resource".parse().expect("resource"),
    };
    let first = SmFrameProvenance::ingress([key(1)]);
    let second = SmFrameProvenance::ingress([key(2)]);
    let mut state = StreamManagementState::new();
    state.enable("wrapped-provenance".to_owned(), true, Some(300));
    state.last_acked = u32::MAX - 1;
    state.outbound_count = u32::MAX - 1;
    let _ = state.record_outbound_with_provenance(
        message_xml("max"),
        Utc::now(),
        SmEvictionPath::Batch,
        first.clone(),
    );
    let _ = state.record_outbound_with_provenance(
        message_xml("zero"),
        Utc::now(),
        SmEvictionPath::Batch,
        second.clone(),
    );
    for _ in 0..2 {
        let replay = state.stanzas_to_resend(u32::MAX - 1);
        assert_eq!(
            replay
                .iter()
                .map(|frame| &frame.provenance)
                .collect::<Vec<_>>(),
            vec![&first, &second]
        );
    }
    state.acknowledge(u32::MAX);
    assert_eq!(state.stanzas_to_resend(u32::MAX)[0].provenance, second);
    state.acknowledge(0);
    assert!(state.stanzas_to_resend(0).is_empty());
}
