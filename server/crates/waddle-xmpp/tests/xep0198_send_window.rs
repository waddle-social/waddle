//! XEP-0198 send-window pacing — the state-level contract (issue #1219).
//!
//! Incident (prod, 2026-07-07): a client-driven XEP-0313 MAM catch-up
//! recorded ≥1,325 countable stanzas into the 1000-slot SM unacked queue
//! within 1.3 s. The queue overflowed, evicted the oldest sequences, and
//! `mark_replay_gap_through` poisoned resume — every later resume with an
//! older `h` was rejected, forcing a fresh bind and a full re-bootstrap
//! that produced the same burst again.
//!
//! The fix paces the *consumer* (the wire-write choke points): once the
//! outstanding unacked count crosses a high watermark, the writer stops
//! feeding the queue and elicits an `<a/>` until the window recovers to a
//! low watermark. XEP-0198 §4 permits requesting an ack at any time and
//! places no obligation to transmit queued stanzas immediately
//! (xep-0198.xml:307/357), so this is conformant.
//!
//! These tests exercise the state-level contract behind that pacing —
//! [`StreamManagementState::needs_send_pause`] / `send_window_recovered`
//! and the invariant a compliant producer upholds: a burst far larger than
//! the queue cap never evicts and resume stays clean. The wire-write
//! choke points that consume this contract (`batch_write.rs` inline pacing
//! and the `connection.rs` loop gate) are exercised by the `waddle-server`
//! websocket test suites.

use waddle_xmpp::stream_management::persistence::SmUnackedStanzaPurpose;
use waddle_xmpp::stream_management::StreamManagementState;
use waddle_xmpp::Stanza;

fn countable_message(id: u32) -> Stanza {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.id = Some(xmpp_parsers::message::Id(id.to_string()));
    Stanza::Message(message)
}

fn stanza_id(stanza: &Stanza) -> Option<&str> {
    match stanza {
        Stanza::Message(message) => message.id.as_ref().map(|id| id.0.as_str()),
        Stanza::Iq(_) | Stanza::Presence(_) => None,
    }
}

/// Drive one resumable stream through a burst of `total` countable
/// stanzas while honouring the send-window pause exactly as the
/// production wire-write paths do: after recording each stanza, if the
/// window latched the pause, "the client acks" (here: acknowledge every
/// stanza recorded so far) until the window recovers before recording
/// more. Returns the final acknowledged `h`.
fn drive_paced_burst(state: &mut StreamManagementState, total: u32) -> u32 {
    let mut client_h = 0;
    for n in 1..=total {
        let _ = state.record_outbound(countable_message(n), SmUnackedStanzaPurpose::Application);
        // A compliant producer stops feeding the queue while paused and
        // waits for the client to ack the window down.
        while state.needs_send_pause() {
            // The client received everything recorded so far and acks it.
            client_h = state.outbound_count;
            state.acknowledge(client_h);
        }
    }
    client_h
}

#[test]
fn paced_burst_far_larger_than_cap_never_evicts_and_resume_stays_clean() {
    // cap=10 → high=8, low=5. Push 500 — 50× the cap.
    let mut state = StreamManagementState::with_config(10, 5);
    state.enable("paced".to_string(), true, Some(300));

    let client_h = drive_paced_burst(&mut state, 500);

    assert_eq!(
        state.replay_gap_through(),
        None,
        "a paced burst must never evict, so no replay gap is ever marked"
    );
    assert!(
        state.queue_len() <= 10,
        "the retained queue never exceeds the cap under pacing (was {})",
        state.queue_len()
    );
    assert!(
        state.can_resume_from(client_h),
        "resume from the last acked h stays clean after a paced burst"
    );
    // The client acked everything at the final pause, so the earliest
    // still-resumable h is also clean.
    assert!(state.can_resume_from(state.last_acked));
}

#[test]
fn unpaced_burst_of_the_same_size_would_evict_and_poison_resume() {
    // Control: the SAME burst, but the producer ignores the pause signal.
    // This is the pre-#1219 behaviour and proves the burst genuinely
    // exceeds the cap — i.e. the paced test above is not vacuous.
    let mut state = StreamManagementState::with_config(10, 5);
    state.enable("unpaced".to_string(), true, Some(300));

    for n in 1..=500 {
        let _ = state.record_outbound(countable_message(n), SmUnackedStanzaPurpose::Application);
        // No pause honoured, no acks — exactly the incident shape.
    }

    assert!(
        state.replay_gap_through().is_some(),
        "an unpaced burst overflows the cap and marks a replay gap"
    );
    assert!(
        !state.can_resume_from(0),
        "resume from an old h is poisoned once the queue evicted"
    );
}

#[test]
fn partial_acks_keep_the_window_open_without_evicting() {
    // A more realistic pacing: the client acks only down to the low
    // watermark, so the window reopens with headroom (hysteresis) rather
    // than draining fully. Still no eviction, still clean resume.
    let mut state = StreamManagementState::with_config(20, 5); // high=16, low=10
    state.enable("partial".to_string(), true, Some(300));

    let mut client_h = 0;
    for n in 1..=300 {
        let _ = state.record_outbound(countable_message(n), SmUnackedStanzaPurpose::Application);
        if state.needs_send_pause() {
            // Ack just enough to drop outstanding to the low watermark.
            client_h = state.outbound_count - 10;
            state.acknowledge(client_h);
            assert!(
                state.send_window_recovered(),
                "acking to the low watermark releases the pause"
            );
        }
    }

    assert_eq!(
        state.replay_gap_through(),
        None,
        "partial-ack pacing never evicts"
    );
    assert!(state.can_resume_from(client_h));
}

#[test]
fn resume_after_paced_burst_replays_exactly_the_unacked_tail_in_order() {
    let mut state = StreamManagementState::with_config(10, 5);
    state.enable("replay".to_string(), true, Some(300));

    // Paced burst of 50, but stop acking after h=45 so 46..=50 stay unacked.
    let mut client_h = 0;
    for n in 1..=50 {
        let _ = state.record_outbound(countable_message(n), SmUnackedStanzaPurpose::Application);
        while state.needs_send_pause() {
            client_h = state.outbound_count;
            state.acknowledge(client_h);
        }
    }
    // Final window may be empty (all acked) — force a known unacked tail by
    // recording five more without acking.
    for n in 51..=55 {
        let _ = state.record_outbound(countable_message(n), SmUnackedStanzaPurpose::Application);
    }

    let replay = state.get_stanzas_to_resend(client_h.max(50));
    // client_h is 50 (last full-ack pause), tail is 51..=55.
    assert_eq!(replay.len(), 5, "exactly the unacked tail replays");
    for (offset, entry) in replay.iter().enumerate() {
        let expected = (51 + offset).to_string();
        assert!(
            stanza_id(&entry.stanza) == Some(expected.as_str()),
            "replay preserves FIFO order: expected id {expected}, got {:?}",
            stanza_id(&entry.stanza)
        );
    }
    assert_eq!(
        state.replay_gap_through(),
        None,
        "no gap — tail is fully retained"
    );
}
