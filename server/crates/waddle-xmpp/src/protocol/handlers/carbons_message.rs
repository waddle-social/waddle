//! XEP-0280: Message Carbons fan-out at the message-pipeline level.
//!
//! Distinct from [`super::carbons::CarbonsHandler`] which is the IQ-set
//! handler that toggles the per-connection enable/disable flag. This
//! handler runs in the *message* pipeline and emits
//! [`OutboundEvent::SendCarbons`] to fan a copy of the message out to
//! the owner's other resources.
//!
//! XEP-0280 §6 suppression rules — all enforced before emitting:
//!
//! - `<message type='groupchat'>` MUST NOT be carboned (§6.2).
//! - `<private xmlns='urn:xmpp:carbons:2'/>` MUST suppress carbons (§6.1).
//! - XEP-0334 `<no-copy xmlns='urn:xmpp:hints'/>` MUST suppress carbons.
//! - Body-less messages SHOULD NOT be carboned (`should_copy_message`).
//! - Error stanzas are not carboned.
//!
//! Locality / `kind` mapping:
//!
//! - **Sender pass** + carbons enabled → emit
//!   [`CarbonKind::Sent`] for the sender's other resources.
//! - **Recipient pass** + carbons enabled → emit
//!   [`CarbonKind::Received`] for the recipient's other resources.
//! - **Both** (true self-loop) → emit `Sent` once; received-carbons of a
//!   self-message is redundant under the §6 rules.
//! - **Neither** → no-op.
//!
//! `ctx.carbons` must be `Enabled` for the local connection — when
//! carbons are off, the handler is a no-op even for §6-eligible
//! messages. The interpreter is responsible for picking the
//! per-connection enabled state of *other* resources during fan-out.

use crate::carbons::should_copy_message;
use crate::protocol::event::{CarbonKind, OutboundEvent};
use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::{CarbonsState, Locality};
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use xmpp_parsers::message::Message;

/// XEP-0280 carbons fan-out for the message pipeline.
#[derive(Debug, Default, Clone, Copy)]
pub struct CarbonsMessageHandler;

impl MessageHandler for CarbonsMessageHandler {
    fn name(&self) -> &'static str {
        "xep-0280-carbons-fanout"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        // Fast-skip if carbons are not enabled for the local connection
        // — we only carbon for the local user's own other resources.
        if !matches!(ctx.carbons, CarbonsState::Enabled) {
            return HandlerOutcome::Continue(Vec::new());
        }
        // §6 suppression — single chokepoint via the shared helper.
        if !should_copy_message(message) {
            return HandlerOutcome::Continue(Vec::new());
        }

        let owner = ctx.full_jid.to_bare();
        let exclude = ctx.full_jid.clone();
        let kind = match ctx.locality {
            Locality::Sender => CarbonKind::Sent,
            Locality::Recipient => CarbonKind::Received,
            // Self-loop: §6.1's "private" semantic is implicit — a
            // message sent to one's own resource doesn't need the
            // received-mirror.
            Locality::Both => CarbonKind::Sent,
            Locality::Neither => return HandlerOutcome::Continue(Vec::new()),
        };

        HandlerOutcome::Continue(vec![OutboundEvent::SendCarbons {
            owner,
            message: Box::new(message.clone()),
            kind,
            exclude,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, MucOccupancy};
    use crate::xep::xep0334::Hint;
    use jid::{BareJid, FullJid};
    use minidom::Element;
    use xmpp_parsers::message::{Body, Message, MessageType};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn chat_with_body(from: &str, to: &str, body: &str) -> Message {
        let mut m = Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = MessageType::Chat;
        m.bodies.insert(String::new(), Body(body.to_string()));
        m
    }

    fn run(local: &FullJid, carbons: CarbonsState, msg: &mut Message) -> HandlerOutcome {
        let bl = Blocklist::empty();
        let occ = MucOccupancy::empty();
        let gen = FixedIdGenerator("test".to_string());
        let env = MessageContextEnv {
            domain: "example.com",
            full_jid: local,
            blocklist: &bl,
            carbons,
            muc_occupancy: &occ,
            id_gen: &gen,
        };
        let ctx = MessageContext::derive(env, msg);
        CarbonsMessageHandler.handle(msg, &ctx)
    }

    fn extract_carbons(outcome: &HandlerOutcome) -> Vec<(BareJid, CarbonKind)> {
        match outcome {
            HandlerOutcome::Continue(events) => events
                .iter()
                .filter_map(|e| match e {
                    OutboundEvent::SendCarbons { owner, kind, .. } => Some((owner.clone(), *kind)),
                    _ => None,
                })
                .collect(),
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Carbons disabled — no fan-out
    // -----------------------------------------------------------------

    #[test]
    fn xep_0280_handler_is_noop_when_carbons_disabled() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, CarbonsState::Disabled, &mut msg);
        assert!(extract_carbons(&outcome).is_empty());
    }

    // -----------------------------------------------------------------
    // Locality → CarbonKind
    // -----------------------------------------------------------------

    #[test]
    fn xep_0280_sender_pass_emits_sent_carbon_for_owner() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, CarbonsState::Enabled, &mut msg);
        let carbons = extract_carbons(&outcome);
        assert_eq!(carbons.len(), 1);
        assert_eq!(carbons[0].0, bare("alice@example.com"));
        assert_eq!(carbons[0].1, CarbonKind::Sent);
    }

    #[test]
    fn xep_0280_recipient_pass_emits_received_carbon_for_owner() {
        let local = full("bob@example.com/desk");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, CarbonsState::Enabled, &mut msg);
        let carbons = extract_carbons(&outcome);
        assert_eq!(carbons.len(), 1);
        assert_eq!(carbons[0].0, bare("bob@example.com"));
        assert_eq!(carbons[0].1, CarbonKind::Received);
    }

    // -----------------------------------------------------------------
    // §6.2 — groupchat MUST NOT be carboned
    // -----------------------------------------------------------------

    #[test]
    fn xep_0280_groupchat_is_not_carboned() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "room@conf.example.com", "shouted");
        msg.type_ = MessageType::Groupchat;
        let outcome = run(&local, CarbonsState::Enabled, &mut msg);
        assert!(extract_carbons(&outcome).is_empty());
    }

    // -----------------------------------------------------------------
    // §6.1 — <private/> hint suppresses carbons
    // -----------------------------------------------------------------

    #[test]
    fn xep_0280_private_hint_suppresses_carbons() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "secret");
        msg.payloads
            .push(Element::builder("private", "urn:xmpp:carbons:2").build());
        let outcome = run(&local, CarbonsState::Enabled, &mut msg);
        assert!(extract_carbons(&outcome).is_empty());
    }

    // -----------------------------------------------------------------
    // XEP-0334 — <no-copy/> suppresses carbons
    // -----------------------------------------------------------------

    #[test]
    fn xep_0334_no_copy_hint_suppresses_carbons() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "shh");
        msg.payloads.push(
            Element::builder(Hint::NoCopy.element_name(), crate::xep::xep0334::NS_HINTS).build(),
        );
        let outcome = run(&local, CarbonsState::Enabled, &mut msg);
        assert!(extract_carbons(&outcome).is_empty());
    }

    // -----------------------------------------------------------------
    // Body-less and error stanzas
    // -----------------------------------------------------------------

    #[test]
    fn xep_0280_body_less_message_is_not_carboned() {
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some("bob@example.com".parse().expect("jid")));
        msg.from = Some("alice@example.com/web".parse().expect("jid"));
        msg.type_ = MessageType::Chat;
        let outcome = run(&local, CarbonsState::Enabled, &mut msg);
        assert!(extract_carbons(&outcome).is_empty());
    }

    #[test]
    fn xep_0280_error_message_is_not_carboned() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "boom");
        msg.type_ = MessageType::Error;
        let outcome = run(&local, CarbonsState::Enabled, &mut msg);
        assert!(extract_carbons(&outcome).is_empty());
    }

    #[test]
    fn xep_0280_neither_locality_emits_nothing() {
        let local = full("eve@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, CarbonsState::Enabled, &mut msg);
        assert!(extract_carbons(&outcome).is_empty());
    }

    #[test]
    fn xep_0280_excludes_originating_resource_from_fanout() {
        // The `exclude` field carries the local connection's full JID
        // so the interpreter doesn't echo the carbon back to the
        // resource that originated the message.
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, CarbonsState::Enabled, &mut msg);
        match outcome {
            HandlerOutcome::Continue(events) => match &events[0] {
                OutboundEvent::SendCarbons { exclude, .. } => {
                    assert_eq!(exclude.to_string(), "alice@example.com/web");
                }
                _ => panic!("expected SendCarbons"),
            },
            other => panic!("expected Continue, got {other:?}"),
        }
    }
}
