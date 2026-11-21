//! XEP-0198 §5 `<resumed/>` replay frame construction.
//!
//! Both resume paths — the pre-registration one in [`super::stream_management`]
//! and the post-registration one in [`super::connection`] — rebuild the same
//! frames from the same restored unacked queue. They were written twice, and
//! diverged: the post-registration copy dropped each entry's ingress receipt
//! obligations, so a resume that replayed a frame carrying an outstanding
//! obligation settled nothing and left its canonical row non-terminal. One
//! builder removes the class of defect rather than the instance.

use waddle_xmpp::stream_management::{
    persistence::SmReplayPayload, stamp_replay_delay, ReplayStanza, StreamManagementState,
};

use super::frame::ResponseFrame;
use crate::permissions::host_grant::HostGrantGate;

pub(super) async fn protected_replay_is_authorized(
    sm_state: &StreamManagementState,
    client_h: u32,
    recipient: &jid::FullJid,
    spaces_service: &str,
    grants: &HostGrantGate,
) -> bool {
    for entry in sm_state.stanzas_to_resend(client_h) {
        let admitted = if SmReplayPayload::is_protected_storage(&entry.stanza_xml) {
            authorize_protected(entry.stanza_xml, recipient, grants)
                .await
                .is_some()
        } else {
            authorize_legacy_reply(entry.stanza_xml, Some(recipient), spaces_service, grants)
                .is_some()
        };
        if !admitted {
            return false;
        }
    }
    true
}

pub(super) enum LegacyReplay {
    Ordinary(String),
    Fenced(ResponseFrame),
}

/// A reply retained before enrollment cannot carry canonical authority.
pub(super) fn authorize_legacy_reply(
    wire: String,
    recipient: Option<&jid::FullJid>,
    spaces_service: &str,
    grants: &HostGrantGate,
) -> Option<LegacyReplay> {
    let iq = wire.parse::<minidom::Element>().ok()?;
    if !iq.is("iq", "jabber:client")
        || iq.attr("type") != Some("result")
        || iq.attr("from") != Some(spaces_service)
    {
        return Some(LegacyReplay::Ordinary(wire));
    }
    let nodes = legacy_reply_nodes(&iq);
    if nodes.is_empty() {
        return Some(LegacyReplay::Ordinary(wire));
    }
    let guard = grants.unmanaged_space_result_guard(
        spaces_service.parse().ok()?,
        recipient?,
        &iq,
        nodes,
    )?;
    ResponseFrame::unmanaged_space_frame(wire, guard)
        .ok()
        .map(LegacyReplay::Fenced)
}

fn legacy_reply_nodes(iq: &minidom::Element) -> Vec<String> {
    iq.children()
        .filter(|payload| {
            matches!(
                payload.ns().as_str(),
                "http://jabber.org/protocol/disco#info"
                    | "http://jabber.org/protocol/disco#items"
                    | waddle_xmpp::pubsub::NS_PUBSUB
                    | waddle_xmpp::pubsub::NS_PUBSUB_OWNER
            )
        })
        .flat_map(|payload| {
            payload
                .attr("node")
                .into_iter()
                .chain(payload.children().filter_map(|child| child.attr("node")))
                .map(str::to_owned)
        })
        .collect()
}

async fn authorize_protected(
    stored: String,
    recipient: &jid::FullJid,
    grants: &HostGrantGate,
) -> Option<ResponseFrame> {
    let payload = SmReplayPayload::from_storage(stored).ok()?;
    authorize_protected_payload(payload, recipient, grants, None).await
}

pub(super) async fn authorize_protected_payload(
    payload: SmReplayPayload,
    recipient: &jid::FullJid,
    grants: &HostGrantGate,
    attempt: Option<&crate::permissions::host_grant::SpaceDeletionAttempt>,
) -> Option<ResponseFrame> {
    let mut grant = grants
        .refresh_local_space_reply(&payload, recipient)
        .await?;
    if let Some(attempt) = attempt {
        if !grant.bind_deletion_attempt(attempt.clone()) {
            return None;
        }
    }
    ResponseFrame::protected_space_frame(payload.wire().to_owned(), grant).ok()
}

pub(super) async fn authorized_replay_frames(
    sm_state: &StreamManagementState,
    client_h: u32,
    server_domain: &str,
    recipient: Option<&jid::FullJid>,
    spaces_service: &str,
    grants: &HostGrantGate,
) -> Option<Vec<ResponseFrame>> {
    let mut frames = Vec::new();
    for mut entry in sm_state.stanzas_to_resend(client_h) {
        let wire = std::mem::take(&mut entry.stanza_xml);
        let frame = if SmReplayPayload::is_protected_storage(&wire) {
            authorize_protected(wire, recipient?, grants).await?
        } else {
            match authorize_legacy_reply(wire, recipient, spaces_service, grants)? {
                LegacyReplay::Ordinary(wire) => {
                    entry.stanza_xml = wire;
                    frames.push(ordinary_replay_frame(entry, server_domain));
                    continue;
                }
                LegacyReplay::Fenced(frame) => frame,
            }
        };
        frames.push(
            frame
                .with_ingress_receipts(entry.ingress_receipts)
                .with_provenance(entry.provenance),
        );
    }
    Some(frames)
}

/// Build the replay frames a resumed stream owes its client, in queue order.
///
/// Each frame is stamped with a XEP-0203 `<delay/>` carrying the stanza's
/// original receipt time (#1178) so the client sorts it at its true timeline
/// position rather than at drain time, and carries forward the ingress receipt
/// obligations the entry was holding, so reaching the wire settles them.
#[cfg(test)]
pub(super) fn replay_frames(
    sm_state: &StreamManagementState,
    client_h: u32,
    server_domain: &str,
) -> Vec<ResponseFrame> {
    sm_state
        .stanzas_to_resend(client_h)
        .into_iter()
        .map(|entry| ordinary_replay_frame(entry, server_domain))
        .collect()
}

fn ordinary_replay_frame(entry: ReplayStanza, server_domain: &str) -> ResponseFrame {
    ResponseFrame::from_serialized_xml(stamp_replay_delay(
        &entry.stanza_xml,
        server_domain,
        entry.original_receipt_at,
    ))
    .with_ingress_receipts(entry.ingress_receipts)
    .with_provenance(entry.provenance)
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

    #[tokio::test]
    async fn legacy_space_replay_binds_to_the_resumed_resource() {
        let db = crate::db::Database::in_memory("legacy-space-resume")
            .await
            .unwrap();
        let gate = HostGrantGate::new(db);
        let recipient: jid::FullJid = "a@example.com/x".parse().unwrap();
        let wire = |to| {
            format!(
                "<iq xmlns='jabber:client' type='result' id='info' from='spaces.example.com'{to}>\
             <query xmlns='http://jabber.org/protocol/disco#info' node='legacy'/></iq>"
            )
        };
        for to in ["", " to='a@example.com/x'"] {
            let xml = wire(to);
            let Some(LegacyReplay::Fenced(frame)) =
                authorize_legacy_reply(xml.clone(), Some(&recipient), "spaces.example.com", &gate)
            else {
                panic!("expected a fenced legacy result");
            };
            assert_eq!(frame.protected_space_reply().unwrap().payload.wire(), xml);
        }
        assert!(authorize_legacy_reply(
            wire(" to='a@example.com/other'"),
            Some(&recipient),
            "spaces.example.com",
            &gate,
        )
        .is_none());
    }

    fn receipt() -> SmIngressFrameReceipt {
        SmIngressFrameReceipt {
            message_key: MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(7),
            semantic_identity_hash: [3u8; 32],
        }
    }

    #[tokio::test]
    async fn protected_reply_refusal_preserves_the_replay_prefix() {
        let db = crate::db::Database::in_memory("protected-space-resume")
            .await
            .unwrap();
        let gate = HostGrantGate::new(db);
        let recipient: jid::FullJid = "a@example.com/x".parse().unwrap();
        let mut sm = StreamManagementState::new();
        // An invalid retained protected record must never be interpreted
        // as ordinary XML or skipped to send the following ordinal.
        let _ = sm.record_outbound("sm-protected-space:1:invalid".into(), SmEvictionPath::Batch);
        let _ = sm.record_outbound(
            stanza_to_xml(&message(Id("ordinary".to_owned()))),
            SmEvictionPath::Batch,
        );
        let replay = |h| {
            authorized_replay_frames(
                &sm,
                h,
                "example.com",
                Some(&recipient),
                "spaces.example.com",
                &gate,
            )
        };
        assert!(
            !protected_replay_is_authorized(&sm, 0, &recipient, "spaces.example.com", &gate).await
        );
        assert!(replay(0).await.is_none());
        assert!(
            protected_replay_is_authorized(&sm, 1, &recipient, "spaces.example.com", &gate).await
        );
        let after_ack = replay(1).await.expect("only the ordinary frame remains");
        assert_eq!(after_ack.len(), 1);
        assert!(after_ack[0]
            .clone()
            .into_serialized_xml()
            .contains("ordinary"));
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

    #[test]
    fn provenance_follows_each_replay_and_legacy_conversion_is_unknown() {
        use waddle_xmpp::stream_management::{SmFrameProvenance, SmIngressAppendKey};
        let key = SmIngressAppendKey {
            message_key: MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(7),
            semantic_identity_hash: [3; 32],
            resource: "a@example.com/x".parse().expect("resource"),
        };
        let known = SmFrameProvenance::ingress([key]);
        let xml = stanza_to_xml(&message(Id("m1".to_string())));
        let mut sm = StreamManagementState::new();
        let _ = sm.record_outbound_with_provenance(
            xml.clone(),
            Utc::now(),
            SmEvictionPath::Batch,
            known.clone(),
        );
        let _ = sm.record_outbound(xml.clone(), SmEvictionPath::Batch);
        let frames = replay_frames(&sm, 0, "example.com");
        assert_eq!(frames[0].provenance(), known);
        assert_eq!(frames[1].provenance(), SmFrameProvenance::Unknown);
        let legacy = ResponseFrame::from_serialized_xml(frames[0].clone().into_serialized_xml());
        assert_eq!(legacy.provenance(), SmFrameProvenance::Unknown);
        assert_eq!(
            frames[0]
                .clone()
                .with_provenance(SmFrameProvenance::Unknown)
                .provenance(),
            SmFrameProvenance::Unknown
        );
        assert_eq!(
            legacy.into_serialized_xml(),
            frames[0].clone().into_serialized_xml()
        );
        assert_eq!(replay_frames(&sm, 0, "example.com")[0].provenance(), known);
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
