//! Two-phase enrichment dispatch.
//!
//! Embed/link enrichment is not a XEP — it's a Waddle product feature
//! handled by `extension_manager.enrich_message(...)`. The legacy
//! `message.rs` calls that async API inline. In the sans-I/O pipeline
//! the same async work happens via the two-phase callback shape:
//!
//! 1. Pure handler emits
//!    [`super::super::event::OutboundEvent::RequestEnrichment`] carrying
//!    a fresh [`super::super::event::CallbackId`] and the message,
//!    returning [`super::super::traits::HandlerOutcome::AwaitCallback`].
//! 2. Interpreter performs the async enrichment work and produces an
//!    [`super::super::event::InboundEvent::EnrichmentComplete`] carrying
//!    the rewritten message.
//! 3. State machine resumes the pipeline at the handler immediately
//!    after this one, with the rewritten message replacing the original.
//!
//! Eligibility (must hold for the handler to park; otherwise pipeline
//! continues unchanged):
//!
//! - The local user is the sender (sender pass). On the recipient pass
//!   the message is already enriched — re-enriching would double-stamp.
//! - `message.type_` is `Chat` or `Normal`. Groupchat enrichment runs
//!   inside the room handler chain (PR5).
//! - The message has a non-empty body. (No body → nothing to enrich.)
//! - The message is not already an error reply.
//!
//! The handler doesn't allocate the [`super::super::event::CallbackId`]
//! itself — that's the state machine's responsibility (handlers are
//! pure). Instead it emits a sentinel `CallbackId(0)` placeholder; the
//! state machine swaps in a fresh id when it lifts the event into the
//! interpreter's queue. (Wired in alongside the first cutover PR; PR2
//! ships the handler shape and tests.)

use crate::protocol::event::{CallbackId, OutboundEvent};
use crate::protocol::message_context::MessageContext;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use xmpp_parsers::message::{Body, Message, MessageType};

/// Sentinel callback id meaning "state machine fills this in." The
/// dispatcher cannot allocate ids (it's pure); the state machine swaps
/// the sentinel for a real allocation when it processes the
/// `AwaitCallback` outcome.
pub const ENRICHMENT_CALLBACK_SENTINEL: CallbackId = CallbackId(0);

/// Pipeline handler that requests link/embed enrichment for eligible
/// messages and parks the pipeline pending the async response.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnrichmentDispatchHandler;

impl MessageHandler for EnrichmentDispatchHandler {
    fn name(&self) -> &'static str {
        "waddle-enrichment-dispatch"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        if !is_eligible(message, ctx) {
            return HandlerOutcome::Continue(Vec::new());
        }

        HandlerOutcome::AwaitCallback(vec![OutboundEvent::RequestEnrichment {
            id: ENRICHMENT_CALLBACK_SENTINEL,
            message: Box::new(message.clone()),
        }])
    }
}

/// Determine whether `message` is eligible for enrichment under
/// `ctx`. Pulled out as a free function so the L1 test suite can
/// exercise the eligibility table without going through the
/// dispatcher.
pub fn is_eligible(message: &Message, ctx: &MessageContext<'_>) -> bool {
    // Sender-side only.
    if !ctx.locality.is_sender() {
        return false;
    }
    // Type must be Chat or Normal — Groupchat is the room chain's job;
    // Error replies are not enriched.
    match message.type_ {
        MessageType::Chat | MessageType::Normal => {}
        MessageType::Groupchat | MessageType::Error | MessageType::Headline => return false,
    }
    // Must have a non-empty body.
    has_body(message)
}

fn has_body(message: &Message) -> bool {
    let any_non_empty = message
        .bodies
        .values()
        .any(|Body(text)| !text.trim().is_empty());
    any_non_empty
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy};
    use jid::FullJid;
    use xmpp_parsers::message::{Body, Message, MessageType};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn chat_with_body(from: &str, to: &str, body: &str) -> Message {
        let mut m = Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = MessageType::Chat;
        m.bodies.insert(String::new(), Body(body.to_string()));
        m
    }

    fn run(local: &FullJid, msg: &mut Message) -> HandlerOutcome {
        let bl = Blocklist::empty();
        let occ = MucOccupancy::empty();
        let gen = FixedIdGenerator("test".to_string());
        let env = MessageContextEnv {
            domain: "example.com",
            full_jid: local,
            blocklist: &bl,
            carbons: CarbonsState::Disabled,
            muc_occupancy: &occ,
            id_gen: &gen,
        };
        let ctx = MessageContext::derive(env, msg);
        EnrichmentDispatchHandler.handle(msg, &ctx)
    }

    #[test]
    fn eligible_chat_message_with_body_parks_pipeline() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body(
            "alice@example.com/web",
            "bob@example.com",
            "https://example.com/page",
        );
        let outcome = run(&local, &mut msg);
        match outcome {
            HandlerOutcome::AwaitCallback(events) => {
                assert_eq!(events.len(), 1);
                match &events[0] {
                    OutboundEvent::RequestEnrichment { id, message } => {
                        assert_eq!(*id, ENRICHMENT_CALLBACK_SENTINEL);
                        assert_eq!(
                            message.bodies.get("").map(|Body(s)| s.as_str()),
                            Some("https://example.com/page"),
                        );
                    }
                    other => panic!("expected RequestEnrichment, got {other:?}"),
                }
            }
            other => panic!("expected AwaitCallback, got {other:?}"),
        }
    }

    #[test]
    fn body_less_message_continues_without_enrichment() {
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some("bob@example.com".parse().expect("jid")));
        msg.from = Some("alice@example.com/web".parse().expect("jid"));
        msg.type_ = MessageType::Chat;
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn whitespace_only_body_continues_without_enrichment() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "   \n\t ");
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn recipient_pass_does_not_re_enrich() {
        // Bob receiving Alice's already-enriched message — should not
        // emit RequestEnrichment again.
        let local = full("bob@example.com/desk");
        let mut msg = chat_with_body(
            "alice@example.com/web",
            "bob@example.com",
            "https://example.com/page",
        );
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn groupchat_is_skipped_user_side() {
        // Groupchat enrichment is handled by the room chain (PR5).
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body(
            "alice@example.com/web",
            "room@conf.example.com",
            "https://example.com/page",
        );
        msg.type_ = MessageType::Groupchat;
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn error_message_is_skipped() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body(
            "alice@example.com/web",
            "bob@example.com",
            "https://example.com/page",
        );
        msg.type_ = MessageType::Error;
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn neither_locality_is_skipped() {
        let local = full("eve@example.com/web");
        let mut msg = chat_with_body(
            "alice@example.com/web",
            "bob@example.com",
            "https://example.com/page",
        );
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }
}
