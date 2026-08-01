//! XEP-0191: Blocking Command — message-pipeline filter.
//!
//! Locality-aware filter that runs first in the message pipeline so
//! later stages (canonicalize, archive, carbons, route) never see a
//! blocked stanza. Two distinct rules apply:
//!
//! - **Server Behavior, incoming side** — when a stanza is being
//!   delivered TO the local user and the sender's bare JID is on the
//!   local blocklist, the server MUST NOT deliver it. Per the current
//!   XEP-0191 text ("If a blocked JID attempts to send a stanza to the
//!   user"), for message stanzas the server SHOULD return an error,
//!   which SHOULD be `<service-unavailable/>` — indistinguishable from
//!   the recipient not existing, so the user appears offline to the
//!   blocked JID. Presence stanzas and stanzas of type `error` are
//!   still dropped silently (no error MAY be returned for presence;
//!   RFC 6121 §8.3 forbids error-of-error).
//! - **§3.2 Server Behavior, outgoing side** — when the local user
//!   attempts to send a stanza TO a JID on their own blocklist, the
//!   server MUST return `<not-acceptable/>` with a
//!   `<blocked xmlns='urn:xmpp:blocking:errors'/>` application condition.
//!
//! In Waddle's locality-aware single-pipeline model (issue #229 Q4), the
//! same handler instance runs for both directions; it inspects
//! `ctx.locality` to decide which rule applies.
//!
//! # XEP custom test suite
//!
//! See `#[cfg(test)] mod tests` below. Test names are prefixed
//! `xep_0191_*` per the convention from #229 Q9(b) so
//! `cargo test xep_0191` returns every XEP-0191 conformance test in the
//! workspace.

use super::errors::{message_error_reply, outgoing_block_error_reply, send_message_error};
use crate::protocol::event::OutboundEvent;
use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::Locality;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Pipeline filter for XEP-0191 blocking-list rules.
#[derive(Debug, Default, Clone, Copy)]
pub struct BlockingFilterHandler;

impl MessageHandler for BlockingFilterHandler {
    fn name(&self) -> &'static str {
        "xep-0191-blocking-filter"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        // RFC 6121 §8.3 forbids replying to a stanza of type='error'
        // with another error stanza. If the local user attempts to send
        // an error message to a blocked recipient, drop silently rather
        // than emit a §3.2 reply that would itself be a forbidden
        // error-of-error. Same on the recipient side.
        let is_error_stanza = matches!(message.type_, MessageType::Error);
        match ctx.locality {
            // Sender pass: §3.2 — the local user sends to a blocked
            // recipient; reply with not-acceptable + <blocked/>.
            Locality::Sender | Locality::Both => {
                if let Some(to) = message.to.as_ref() {
                    if ctx.blocklist.contains_jid(to) {
                        if is_error_stanza {
                            return HandlerOutcome::Halt(Vec::new());
                        }
                        let reply = outgoing_block_error_reply(message);
                        return HandlerOutcome::Halt(vec![send_message_error(reply)]);
                    }
                }
                if matches!(ctx.locality, Locality::Both) {
                    if let Some(from) = message.from.as_ref() {
                        if ctx.blocklist.contains_jid(from) {
                            // Self-blocked; treat as silent drop on the
                            // recipient side.
                            return HandlerOutcome::Halt(Vec::new());
                        }
                    }
                }
                HandlerOutcome::Continue(Vec::new())
            }
            // Recipient pass — the sender is blocked: do not deliver.
            // XEP-0191 ("If a blocked JID attempts to send a stanza to
            // the user"): for message stanzas the server SHOULD return
            // an error, which SHOULD be <service-unavailable/> — the
            // same condition a nonexistent account returns, so the
            // block is not a presence oracle. Error-typed stanzas are
            // dropped silently (RFC 6121 §8.3 forbids error-of-error).
            // The bounce is emitted as a `RouteToConnection` addressed
            // to the sender because on the recipient pass `SendStanza`
            // would write to the *recipient's* wire.
            Locality::Recipient => {
                if let Some(from) = message.from.as_ref() {
                    if ctx.blocklist.contains_jid(from) {
                        if is_error_stanza {
                            return HandlerOutcome::Halt(Vec::new());
                        }
                        let reply = message_error_reply(
                            message,
                            StanzaError::new(
                                ErrorType::Cancel,
                                DefinedCondition::ServiceUnavailable,
                                "en",
                                "Service unavailable.",
                            ),
                        );
                        return HandlerOutcome::Halt(vec![OutboundEvent::RouteToConnection {
                            jid: from.clone(),
                            stanza: Box::new(crate::Stanza::Message(reply)),
                            call_setup: None,
                        }]);
                    }
                }
                HandlerOutcome::Continue(Vec::new())
            }
            // Neither sender nor recipient — should not happen on a C2S
            // pipeline; leave the stanza alone for diagnostic logging
            // by later handlers.
            Locality::Neither => HandlerOutcome::Continue(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::event::OutboundEvent;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy};
    use crate::Stanza;
    use jid::{BareJid, FullJid};
    use xmpp_parsers::message::{Message, MessageType};
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn jid(s: &str) -> jid::Jid {
        s.parse().expect("valid jid")
    }

    fn chat_msg(from: &str, to: &str) -> Message {
        let mut m = Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = MessageType::Chat;
        m
    }

    fn run(
        local: &FullJid,
        bl: &Blocklist,
        msg: &mut Message,
    ) -> (HandlerOutcome, FixedIdGenerator) {
        let occ = MucOccupancy::empty();
        let gen = FixedIdGenerator("test-id".to_string());
        let env = MessageContextEnv {
            domain: "example.com",
            full_jid: local,
            blocklist: bl,
            carbons: CarbonsState::Disabled,
            muc_occupancy: &occ,
            has_live_transport: true,
            delivery_fanout: &[],
            id_gen: &gen,
        };
        let ctx = MessageContext::derive(env, msg);
        (BlockingFilterHandler.handle(msg, &ctx), gen)
    }

    fn assert_halt_no_events(outcome: &HandlerOutcome) {
        match outcome {
            HandlerOutcome::Halt(events) => assert!(
                events.is_empty(),
                "expected silent drop, got events: {events:?}"
            ),
            other => panic!("expected Halt([]), got {other:?}"),
        }
    }

    fn extract_error_payload(outcome: &HandlerOutcome) -> StanzaError {
        let events = match outcome {
            HandlerOutcome::Halt(events) => events,
            other => panic!("expected Halt with reply, got {other:?}"),
        };
        assert_eq!(events.len(), 1);
        let stanza = match &events[0] {
            OutboundEvent::SendStanza(s) => s,
            other => panic!("expected SendStanza, got {other:?}"),
        };
        let msg = match stanza.as_ref() {
            Stanza::Message(m) => m,
            other => panic!("expected Message stanza, got {other:?}"),
        };
        assert_eq!(msg.type_, MessageType::Error);
        let elem = msg
            .payloads
            .iter()
            .find(|p| p.name() == "error")
            .expect("error payload present");
        StanzaError::try_from(elem.clone()).expect("typed parse")
    }

    /// Assert a recipient-pass halt whose single event is a
    /// `<service-unavailable/>` message-error bounce routed back to
    /// the blocked sender (XEP-0191 incoming message rule).
    fn assert_halt_service_unavailable_bounce(outcome: &HandlerOutcome, expected_sender: &str) {
        let events = match outcome {
            HandlerOutcome::Halt(events) => events,
            other => panic!("expected Halt with bounce, got {other:?}"),
        };
        assert_eq!(events.len(), 1);
        let (jid, stanza) = match &events[0] {
            OutboundEvent::RouteToConnection { jid, stanza, .. } => (jid, stanza),
            other => panic!("expected RouteToConnection bounce, got {other:?}"),
        };
        assert_eq!(jid.to_string(), expected_sender);
        let msg = match stanza.as_ref() {
            crate::Stanza::Message(m) => m,
            other => panic!("expected Message stanza, got {other:?}"),
        };
        assert_eq!(msg.type_, MessageType::Error);
        assert_eq!(
            msg.to.as_ref().map(ToString::to_string),
            Some(expected_sender.to_string())
        );
        let elem = msg
            .payloads
            .iter()
            .find(|p| p.name() == "error")
            .expect("error payload present");
        let parsed = StanzaError::try_from(elem.clone()).expect("typed parse");
        assert_eq!(parsed.type_, ErrorType::Cancel);
        assert_eq!(
            parsed.defined_condition,
            DefinedCondition::ServiceUnavailable
        );
    }

    // -----------------------------------------------------------------
    // XEP-0191 — incoming message bounce (service-unavailable)
    // -----------------------------------------------------------------

    #[test]
    fn xep_0191_recipient_pass_bounces_service_unavailable_when_sender_is_blocked() {
        let local = full("bob@example.com/desk");
        let bl = Blocklist::new([bare("alice@example.com")]);
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        let (outcome, _) = run(&local, &bl, &mut msg);
        assert_halt_service_unavailable_bounce(&outcome, "alice@example.com/web");
    }

    #[test]
    fn xep_0191_recipient_pass_continues_when_sender_not_blocked() {
        let local = full("bob@example.com/desk");
        let bl = Blocklist::new([bare("eve@example.com")]);
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        let (outcome, _) = run(&local, &bl, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn xep_0191_recipient_pass_honors_full_jid_block() {
        let local = full("bob@example.com/desk");
        let bl = Blocklist::new([jid("alice@example.com/web")]);
        let mut blocked = chat_msg("alice@example.com/web", "bob@example.com");
        let (blocked_outcome, _) = run(&local, &bl, &mut blocked);
        assert_halt_service_unavailable_bounce(&blocked_outcome, "alice@example.com/web");

        let mut other_resource = chat_msg("alice@example.com/mobile", "bob@example.com");
        let (other_outcome, _) = run(&local, &bl, &mut other_resource);
        assert!(matches!(other_outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn xep_0191_recipient_pass_honors_domain_block() {
        let local = full("bob@example.com/desk");
        let bl = Blocklist::new([jid("blocked.example.com")]);
        let mut msg = chat_msg("alice@blocked.example.com/web", "bob@example.com");
        let (outcome, _) = run(&local, &bl, &mut msg);
        assert_halt_service_unavailable_bounce(&outcome, "alice@blocked.example.com/web");
    }

    // -----------------------------------------------------------------
    // XEP-0191 §3.2 — outgoing not-acceptable + <blocked/>
    // -----------------------------------------------------------------

    #[test]
    fn xep_0191_sender_pass_replies_not_acceptable_when_recipient_is_blocked() {
        let local = full("alice@example.com/web");
        let bl = Blocklist::new([bare("blocked@example.com")]);
        let mut msg = chat_msg("alice@example.com/web", "blocked@example.com");
        let (outcome, _) = run(&local, &bl, &mut msg);
        let parsed = extract_error_payload(&outcome);
        assert_eq!(parsed.type_, ErrorType::Cancel);
        assert_eq!(parsed.defined_condition, DefinedCondition::NotAcceptable);
        let blocked = parsed.other.expect("application condition present");
        assert_eq!(blocked.name(), "blocked");
        assert_eq!(blocked.ns(), super::super::errors::NS_BLOCKING_ERRORS);
    }

    #[test]
    fn xep_0191_sender_pass_continues_when_recipient_not_blocked() {
        let local = full("alice@example.com/web");
        let bl = Blocklist::new([bare("eve@example.com")]);
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        let (outcome, _) = run(&local, &bl, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    // -----------------------------------------------------------------
    // Locality::Neither — defensive behaviour
    // -----------------------------------------------------------------

    #[test]
    fn xep_0191_neither_locality_is_a_noop() {
        // Local user is not the sender or recipient — third-party stanza
        // arriving via a routing path. Don't claim to enforce blocking
        // for someone else's session.
        let local = full("eve@example.com/web");
        let bl = Blocklist::new([bare("alice@example.com")]);
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        let (outcome, _) = run(&local, &bl, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn xep_0191_cross_resource_self_message_with_self_blocked_drops_silently_on_recipient() {
        // alice/phone -> alice/web with alice in her own blocklist. On
        // alice/web's connection the locality is Recipient (from is
        // alice/phone — different resource — so it's not Sender; to is
        // alice/web exactly — full match for Recipient). The incoming
        // recipient-side rule applies: no delivery, service-unavailable
        // back to the sending resource.
        let local = full("alice@example.com/web");
        let bl = Blocklist::new([bare("alice@example.com")]);
        let mut msg = chat_msg("alice@example.com/phone", "alice@example.com/web");
        let (outcome, _) = run(&local, &bl, &mut msg);
        assert_halt_service_unavailable_bounce(&outcome, "alice@example.com/phone");
    }

    // -----------------------------------------------------------------
    // RFC 6121 §8.3 — error-of-error guard
    // -----------------------------------------------------------------

    #[test]
    fn xep_0191_sender_pass_does_not_reply_to_an_error_stanza() {
        // Outgoing message of type='error' addressed to a blocked JID
        // must NOT generate a §3.2 not-acceptable reply, since RFC 6121
        // §8.3 forbids replying to an error stanza with another error
        // stanza. Drop silently instead.
        let local = full("alice@example.com/web");
        let bl = Blocklist::new([bare("blocked@example.com")]);
        let mut msg = chat_msg("alice@example.com/web", "blocked@example.com");
        msg.type_ = MessageType::Error;
        let (outcome, _) = run(&local, &bl, &mut msg);
        assert_halt_no_events(&outcome);
    }

    #[test]
    fn xep_0191_recipient_pass_silently_drops_error_stanza_from_blocked_sender() {
        let local = full("bob@example.com/desk");
        let bl = Blocklist::new([bare("alice@example.com")]);
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        msg.type_ = MessageType::Error;
        let (outcome, _) = run(&local, &bl, &mut msg);
        // §3.1 incoming silent drop applies regardless of stanza type.
        assert_halt_no_events(&outcome);
    }
}
