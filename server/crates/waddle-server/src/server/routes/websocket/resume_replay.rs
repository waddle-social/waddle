//! XEP-0198 §5 `<resumed/>` replay frame construction.
//!
//! Both resume paths — the pre-registration one in [`super::stream_management`]
//! and the post-registration one in [`super::connection`] — rebuild the same
//! frames from the same restored unacked queue. They were written twice, and
//! diverged: the post-registration copy dropped each entry's ingress receipt
//! obligations, so a resume that replayed a frame carrying an outstanding
//! obligation settled nothing and left its canonical row non-terminal. One
//! builder removes the class of defect rather than the instance.

use waddle_xmpp::stream_management::{stamp_replay_delay, StreamManagementState};

use super::frame::ResponseFrame;

/// Build the replay frames a resumed stream owes its client, in queue order.
///
/// Each frame is stamped with a XEP-0203 `<delay/>` carrying the stanza's
/// original receipt time (#1178) so the client sorts it at its true timeline
/// position rather than at drain time, and carries forward the ingress receipt
/// obligations the entry was holding, so reaching the wire settles them.
pub(super) fn replay_frames(
    sm_state: &StreamManagementState,
    client_h: u32,
    server_domain: &str,
) -> Vec<ResponseFrame> {
    sm_state
        .get_stanzas_to_resend(client_h)
        .into_iter()
        .map(|entry| {
            ResponseFrame::from_serialized_xml(stamp_replay_delay(
                &entry.stanza_xml,
                server_domain,
                entry.original_receipt_at,
            ))
            .with_ingress_receipts(entry.ingress_receipts)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::transport_xml::stanza_to_xml;
    use super::*;
    use chrono::{TimeZone, Utc};
    use minidom::Element;
    use waddle_xmpp::ingress::MessageKey;
    use waddle_xmpp::stream_management::{SmIngressFrameReceipt, SmIngressReceiptKind};
    use waddle_xmpp::telemetry::attributes::SmEvictionPath;
    use waddle_xmpp::xep::xep0203::NS_DELAY;
    use waddle_xmpp::Stanza;
    use xmpp_parsers::message::{Id, Lang, Message};

    fn message(id: Id) -> Stanza {
        let mut message = Message::new(Some("a@example.com/x".parse().expect("recipient jid")));
        message.id = Some(id);
        message.bodies.insert(Lang::new(), "hi".to_string());
        Stanza::Message(message)
    }

    fn receipt() -> SmIngressFrameReceipt {
        SmIngressFrameReceipt {
            message_key: MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(7),
            semantic_identity_hash: [3u8; 32],
        }
    }

    /// Regression: the post-registration resume path rebuilt these frames with
    /// its own copy of this logic and dropped `ingress_receipts`, so a resume
    /// that replayed a frame holding an outstanding obligation settled nothing
    /// and left its canonical row non-terminal. Both paths now share one
    /// builder; this pins what it must carry.
    #[test]
    fn replayed_frames_carry_their_ingress_obligations() {
        let mut sm_state = StreamManagementState::new();
        // The `<r/>` cadence this returns is irrelevant here: these tests pin
        // what a replayed frame carries, not when an ack is requested.
        let _ack = sm_state.record_outbound(
            stanza_to_xml(&message(Id("m1".to_string()))),
            SmEvictionPath::Batch,
        );
        let obligation = receipt();
        sm_state.attach_ingress_receipts(vec![obligation.clone()]);

        let frames = replay_frames(&sm_state, 0, "example.com");

        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].ingress_receipts(),
            std::slice::from_ref(&obligation),
            "a replayed frame must carry its obligation, or reaching the wire settles nothing"
        );
    }

    /// #1178: the replay is stamped with the stanza's original receipt time so
    /// the client sorts it at its true timeline position, not at drain time.
    #[test]
    fn replayed_frames_are_delay_stamped_for_the_serving_domain() {
        let mut sm_state = StreamManagementState::new();
        let original_receipt_at = Utc
            .with_ymd_and_hms(2026, 7, 1, 9, 15, 30)
            .single()
            .expect("valid receipt timestamp");
        // The `<r/>` cadence this returns is irrelevant here: these tests pin
        // what a replayed frame carries, not when an ack is requested.
        let _ack = sm_state.record_outbound_with_receipt_at(
            stanza_to_xml(&message(Id("m1".to_string()))),
            original_receipt_at,
            SmEvictionPath::Batch,
        );

        let frames = replay_frames(&sm_state, 0, "example.com");
        let xml = frames
            .into_iter()
            .next()
            .expect("one replayed frame")
            .into_serialized_xml();

        let replayed: Element = xml.parse().expect("replayed message xml");
        let delay = replayed
            .get_child("delay", NS_DELAY)
            .expect("replayed message must carry a XEP-0203 delay");
        assert_eq!(delay.attr("from"), Some("example.com"));
        assert_eq!(delay.attr("stamp"), Some("2026-07-01T09:15:30Z"));
    }

    /// The acknowledged prefix is not replayed, and nothing above it is lost.
    #[test]
    fn only_unacknowledged_frames_are_replayed() {
        let mut sm_state = StreamManagementState::new();
        for id in ["m1", "m2"] {
            let _ack = sm_state.record_outbound(
                stanza_to_xml(&message(Id(id.to_string()))),
                SmEvictionPath::Batch,
            );
        }

        assert_eq!(replay_frames(&sm_state, 0, "example.com").len(), 2);
        assert_eq!(replay_frames(&sm_state, 1, "example.com").len(), 1);
        assert_eq!(replay_frames(&sm_state, 2, "example.com").len(), 0);
    }
}
