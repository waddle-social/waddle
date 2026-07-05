//! Guard Waddle framework extension envelopes at direct-message ingress.
//!
//! Server-side extension enrichments are trusted because the extension host
//! builds them after authorization. Client-authored direct messages are not
//! allowed to carry the same framework namespace; otherwise a user could forge
//! rich extension UI that the web client renders as trusted bot output.

use super::errors::{bad_request_reply, send_message_error};
use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::Locality;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use waddle_extensions::FRAMEWORK_NAMESPACE;
use xmpp_parsers::message::{Message, MessageType};

/// Rejects or strips `<extensions xmlns='urn:waddle:extension:1'>` on
/// user-authored direct messages before archive/inbox/delivery handlers run.
#[derive(Debug, Default, Clone, Copy)]
pub struct FrameworkEnvelopeGuardHandler;

impl MessageHandler for FrameworkEnvelopeGuardHandler {
    fn name(&self) -> &'static str {
        "waddle-framework-envelope-guard"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        if !is_direct_message_type(&message.type_) || !message_has_framework_envelope(message) {
            return HandlerOutcome::Continue(Vec::new());
        }

        if ctx.locality.is_sender() {
            let mut sanitized = message.clone();
            remove_framework_envelopes(&mut sanitized);
            return HandlerOutcome::Halt(vec![send_message_error(bad_request_reply(
                &sanitized,
                "Client-authored Waddle extension envelopes are not allowed.",
            ))]);
        }

        if matches!(ctx.locality, Locality::Recipient) {
            remove_framework_envelopes(message);
        }

        HandlerOutcome::Continue(Vec::new())
    }
}

fn is_direct_message_type(message_type: &MessageType) -> bool {
    matches!(
        message_type,
        MessageType::Chat | MessageType::Normal | MessageType::Headline
    )
}

fn message_has_framework_envelope(message: &Message) -> bool {
    message
        .payloads
        .iter()
        .any(|payload| payload.ns() == FRAMEWORK_NAMESPACE)
}

fn remove_framework_envelopes(message: &mut Message) {
    message
        .payloads
        .retain(|payload| payload.ns() != FRAMEWORK_NAMESPACE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::event::OutboundEvent;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy};
    use crate::Stanza;
    use jid::FullJid;
    use minidom::Element;
    use xmpp_parsers::message::{Message, MessageType};
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn chat_with_body(from: &str, to: &str, body: &str) -> Message {
        let mut message = Message::new(Some(to.parse().expect("jid")));
        message.from = Some(from.parse().expect("jid"));
        message.type_ = MessageType::Chat;
        message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), body.to_string());
        message
    }

    fn framework_envelope() -> Element {
        Element::builder("extensions", FRAMEWORK_NAMESPACE).build()
    }

    fn run(local: &FullJid, message: &mut Message) -> HandlerOutcome {
        let blocklist = Blocklist::empty();
        let occupancy = MucOccupancy::empty();
        let id_gen = FixedIdGenerator("test".to_string());
        let env = MessageContextEnv {
            domain: "example.com",
            full_jid: local,
            blocklist: &blocklist,
            carbons: CarbonsState::Disabled,
            muc_occupancy: &occupancy,
            has_live_transport: true,
            delivery_fanout: &[],
            id_gen: &id_gen,
        };
        let ctx = MessageContext::derive(env, message);
        FrameworkEnvelopeGuardHandler.handle(message, &ctx)
    }

    fn error_message(outcome: HandlerOutcome) -> Message {
        let events = match outcome {
            HandlerOutcome::Halt(events) => events,
            other => panic!("expected Halt, got {other:?}"),
        };
        assert_eq!(events.len(), 1);
        match events.into_iter().next().expect("event") {
            OutboundEvent::SendStanza(stanza) => match *stanza {
                Stanza::Message(message) => message,
                other => panic!("expected message error, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn sender_pass_direct_framework_envelope_is_rejected_before_routing() {
        let local = full("alice@example.com/web");
        let mut message = chat_with_body("alice@example.com/web", "bob@example.com", "spoof");
        message.payloads.push(framework_envelope());

        let reply = error_message(run(&local, &mut message));

        assert_eq!(reply.type_, MessageType::Error);
        assert_eq!(
            reply.from.as_ref().map(|jid| jid.to_string()),
            Some("bob@example.com".to_string())
        );
        assert_eq!(
            reply.to.as_ref().map(|jid| jid.to_string()),
            Some("alice@example.com/web".to_string())
        );
        assert!(!message_has_framework_envelope(&reply));
        let error = reply
            .payloads
            .iter()
            .find(|payload| payload.name() == "error")
            .cloned()
            .and_then(|payload| StanzaError::try_from(payload).ok())
            .expect("typed stanza error");
        assert_eq!(error.type_, ErrorType::Modify);
        assert_eq!(error.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn recipient_pass_direct_framework_envelope_is_stripped_before_delivery() {
        let local = full("bob@example.com/desk");
        let mut message = chat_with_body("alice@example.com/web", "bob@example.com", "spoof");
        message.payloads.push(framework_envelope());

        let outcome = run(&local, &mut message);

        assert!(matches!(outcome, HandlerOutcome::Continue(ref events) if events.is_empty()));
        assert!(!message_has_framework_envelope(&message));
        assert_eq!(
            message.bodies.get("").map(|body| body.as_str()),
            Some("spoof")
        );
    }

    #[test]
    fn groupchat_framework_envelope_is_left_for_room_dispatch() {
        let local = full("alice@example.com/web");
        let mut message = chat_with_body("alice@example.com/web", "room@muc.example.com", "spoof");
        message.type_ = MessageType::Groupchat;
        message.payloads.push(framework_envelope());

        let outcome = run(&local, &mut message);

        assert!(matches!(outcome, HandlerOutcome::Continue(ref events) if events.is_empty()));
        assert!(message_has_framework_envelope(&message));
    }
}
