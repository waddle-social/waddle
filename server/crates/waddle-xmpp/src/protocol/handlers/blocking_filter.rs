//! XEP-0191: Blocking Command — message-pipeline filter.
//!
//! Locality-aware filter that runs first in the message pipeline so
//! later stages (canonicalize, archive, carbons, route) never see a
//! blocked stanza. Two distinct rules apply:
//!
//! - **§3.1 Server Behavior, incoming side** — when a stanza is being
//!   delivered TO the local user and the sender's bare JID is on the
//!   local blocklist, the server MUST drop the stanza silently (or, for
//!   IQs, return `<service-unavailable/>`). For messages the conformant
//!   choice is silent drop.
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

use super::errors::{outgoing_block_error_reply, send_message_error};
use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::Locality;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use xmpp_parsers::message::{Message, MessageType};

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
                    let to_bare = to.to_bare();
                    if ctx.blocklist.contains(&to_bare) {
                        if is_error_stanza {
                            return HandlerOutcome::Halt(Vec::new());
                        }
                        let reply = outgoing_block_error_reply(message);
                        return HandlerOutcome::Halt(vec![send_message_error(reply)]);
                    }
                }
                if matches!(ctx.locality, Locality::Both) {
                    if let Some(from) = message.from.as_ref() {
                        let from_bare = from.to_bare();
                        if ctx.blocklist.contains(&from_bare) {
                            // Self-blocked; treat as silent drop on the
                            // recipient side.
                            return HandlerOutcome::Halt(Vec::new());
                        }
                    }
                }
                HandlerOutcome::Continue(Vec::new())
            }
            // Recipient pass: §3.1 — silently drop incoming stanzas
            // from blocked senders.
            Locality::Recipient => {
                if let Some(from) = message.from.as_ref() {
                    let from_bare = from.to_bare();
                    if ctx.blocklist.contains(&from_bare) {
                        return HandlerOutcome::Halt(Vec::new());
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

    // -----------------------------------------------------------------
    // XEP-0191 §3.1 — incoming silent drop
    // -----------------------------------------------------------------

    #[test]
    fn xep_0191_recipient_pass_drops_silently_when_sender_is_blocked() {
        let local = full("bob@example.com/desk");
        let bl = Blocklist::new([bare("alice@example.com")]);
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        let (outcome, _) = run(&local, &bl, &mut msg);
        assert_halt_no_events(&outcome);
    }

    #[test]
    fn xep_0191_recipient_pass_continues_when_sender_not_blocked() {
        let local = full("bob@example.com/desk");
        let bl = Blocklist::new([bare("eve@example.com")]);
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        let (outcome, _) = run(&local, &bl, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
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
        // alice/web exactly — full match for Recipient). The §3.1
        // recipient-side rule applies: silent drop, no events.
        let local = full("alice@example.com/web");
        let bl = Blocklist::new([bare("alice@example.com")]);
        let mut msg = chat_msg("alice@example.com/phone", "alice@example.com/web");
        let (outcome, _) = run(&local, &bl, &mut msg);
        assert_halt_no_events(&outcome);
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
