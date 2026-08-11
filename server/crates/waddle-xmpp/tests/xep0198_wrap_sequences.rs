//! XEP-0198 wrapping counter behavior for detached-session persistence.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use proptest::prelude::*;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{InMemorySmPersistence, SmPersistenceStorage};
use waddle_xmpp::stream_management::{
    DetachedSession, DetachedUnackedStanza, InMemorySmSessionRegistry, SmSessionRegistry,
    StreamManagementState, DEFAULT_MAX_UNACKED_QUEUE_SIZE,
};
use waddle_xmpp::telemetry::attributes::SmEvictionPath;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Id, Message};

fn detached_session(last_acked: u32, outbound_count: u32) -> DetachedSession {
    DetachedSession {
        stream_id: "wrap-sequences".to_string(),
        user_id: "user@example.com".to_string(),
        jid: "user@example.com/resource".parse().expect("valid full JID"),
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

fn message_xml(id: &str) -> String {
    let mut message = Message::new(None::<jid::Jid>);
    message.id = Some(Id(id.to_string()));
    let element = Stanza::Message(message).to_element();
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

    session.record_detached_outbound_at(0, "zero".to_string(), Utc::now());
    session.record_detached_outbound_at(u32::MAX, "max".to_string(), Utc::now());
    assert_eq!(session.outbound_count, 0);
    assert_eq!(sequence_numbers(&session), vec![u32::MAX, 0]);

    let before_duplicate = session.clone();
    session.record_detached_outbound_at(0, "different duplicate".to_string(), Utc::now());
    assert_eq!(session.outbound_count, before_duplicate.outbound_count);
    assert_eq!(session.unacked_stanzas, before_duplicate.unacked_stanzas);
    assert_eq!(
        session.replay_gap_through,
        before_duplicate.replay_gap_through
    );

    let before_stale = session.clone();
    session.record_detached_outbound_at(last_acked, "acked".to_string(), Utc::now());
    session.record_detached_outbound_at(
        last_acked.wrapping_sub(1),
        "behind ack floor".to_string(),
        Utc::now(),
    );
    assert_eq!(session.outbound_count, before_stale.outbound_count);
    assert_eq!(session.unacked_stanzas, before_stale.unacked_stanzas);
    assert_eq!(session.replay_gap_through, before_stale.replay_gap_through);
}

#[tokio::test]
async fn persistence_hydration_and_followup_eviction_stay_in_wrap_order() {
    let last_acked = u32::MAX - 500;
    let mut session = detached_session(
        last_acked,
        last_acked.wrapping_add(DEFAULT_MAX_UNACKED_QUEUE_SIZE as u32),
    );
    let stanza_xml = message_xml("wrapped");
    let expected: Vec<u32> = (1..=DEFAULT_MAX_UNACKED_QUEUE_SIZE as u32)
        .map(|offset| last_acked.wrapping_add(offset))
        .collect();
    session.unacked_stanzas = expected
        .iter()
        .rev()
        .map(|sequence| DetachedUnackedStanza {
            sequence: *sequence,
            stanza_xml: stanza_xml.clone(),
            original_receipt_at: Utc::now(),
        })
        .collect();

    let persistence: Arc<dyn SmPersistenceStorage> = Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    registry
        .store_session(session)
        .await
        .expect("store session");

    let stream_id = SmSessionId::new("wrap-sequences");
    let stored = persistence
        .list_unacked(&stream_id)
        .await
        .expect("list stored stanzas");
    assert_eq!(
        stored
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        expected
    );

    drop(registry);
    let restored_registry =
        InMemorySmSessionRegistry::new().with_persistence(Arc::clone(&persistence));
    assert_eq!(
        restored_registry
            .restore_from_persistence()
            .await
            .expect("restore session"),
        1
    );
    let mut restored = restored_registry
        .claim_session("wrap-sequences")
        .await
        .expect("claim restored session")
        .expect("restored session exists");
    assert_eq!(sequence_numbers(&restored), expected);
    assert_eq!(
        restored.stanzas_to_resend(last_acked).len(),
        DEFAULT_MAX_UNACKED_QUEUE_SIZE
    );

    let newest = last_acked.wrapping_add(DEFAULT_MAX_UNACKED_QUEUE_SIZE as u32 + 1);
    restored.record_detached_outbound_at(newest, stanza_xml, Utc::now());
    assert_eq!(
        restored.unacked_stanzas.len(),
        DEFAULT_MAX_UNACKED_QUEUE_SIZE
    );
    assert_eq!(restored.replay_gap_through, Some(expected[0]));
    assert_eq!(sequence_numbers(&restored)[0], expected[1]);
    assert_eq!(sequence_numbers(&restored).last(), Some(&newest));
}
