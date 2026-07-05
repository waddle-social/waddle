//! XEP-0184: Message Delivery Receipts — recipient-pass acknowledgements.
//!
//! XEP-0184 §5 says the recipient generates an ack message only for a
//! content message that requested a receipt, and XEP-0184 §4 says entities
//! MUST NOT include `<request/>` in an ack message. Waddle therefore emits
//! receipts only on the live recipient pass for direct `chat`/`normal`/
//! `headline` messages that carry `<request/>` and a non-empty `id`.
//!
//! The handler is deliberately registered **after** [`super::route::RouteHandler`]
//! so the original content stanza's `SendStanza` is queued before the receipt's
//! `RouteToConnection`, avoiding an ack being processed ahead of the content.
//! Headless offline-recipient passes are excluded because they persist/archive
//! on behalf of an offline user but do not represent delivery to a live client.

use crate::protocol::event::OutboundEvent;
use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::Locality;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use crate::xep::xep0184::{build_receipt_message, has_receipt_received, has_receipt_request};
use crate::Stanza;
use jid::Jid;
use xmpp_parsers::message::{Message, MessageType};

/// Recipient-pass XEP-0184 receipt emitter.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReceiptsHandler;

impl MessageHandler for ReceiptsHandler {
    fn name(&self) -> &'static str {
        "xep-0184-receipts"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        if ctx.locality != Locality::Recipient
            || !ctx.has_live_transport
            || !is_receipt_eligible(message)
        {
            return HandlerOutcome::Continue(Vec::new());
        }

        let Some(original_id) = message
            .id
            .as_ref()
            .map(|id| id.0.as_str())
            .filter(|id| !id.is_empty())
        else {
            return HandlerOutcome::Continue(Vec::new());
        };
        let Some(target) = message.from.clone() else {
            return HandlerOutcome::Continue(Vec::new());
        };

        let mut receipt = build_receipt_message(
            Some(target.clone()),
            Some(Jid::from(ctx.full_jid.clone())),
            original_id,
        );
        receipt.type_ = message.type_.clone();

        HandlerOutcome::Continue(vec![OutboundEvent::RouteToConnection {
            jid: target,
            stanza: Box::new(Stanza::Message(receipt)),
        }])
    }
}

fn is_receipt_eligible(message: &Message) -> bool {
    matches!(
        message.type_,
        MessageType::Chat | MessageType::Normal | MessageType::Headline
    ) && has_receipt_request(message)
        && !has_receipt_received(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy};
    use crate::xep::xep0184::extract_received_id;
    use jid::FullJid;

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn receipt_request_message() -> Message {
        let mut message = Message::new(Some("bob@example.com".parse().expect("jid")));
        message.from = Some("alice@example.com/web".parse().expect("jid"));
        message.type_ = MessageType::Chat;
        message.id = Some(xmpp_parsers::message::Id("msg-1".to_string()));
        message
            .payloads
            .push(crate::xep::xep0184::build_receipt_request_element());
        message
    }

    fn run(local: &FullJid, has_live_transport: bool, message: &mut Message) -> HandlerOutcome {
        let blocklist = Blocklist::empty();
        let occupancy = MucOccupancy::empty();
        let id_gen = FixedIdGenerator("id".to_string());
        let env = MessageContextEnv {
            domain: "example.com",
            full_jid: local,
            blocklist: &blocklist,
            carbons: CarbonsState::Disabled,
            muc_occupancy: &occupancy,
            has_live_transport,
            delivery_fanout: &[],
            id_gen: &id_gen,
        };
        let mut ctx = MessageContext::derive(env, message);
        ctx.locality = Locality::Recipient;
        ReceiptsHandler.handle(message, &ctx)
    }

    #[test]
    fn xep_0184_live_recipient_pass_routes_receipt() {
        let local = full("bob@example.com/desk");
        let mut message = receipt_request_message();

        let outcome = run(&local, true, &mut message);
        let events = match outcome {
            HandlerOutcome::Continue(events) => events,
            other => panic!("expected Continue, got {other:?}"),
        };
        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::RouteToConnection { jid, stanza } => {
                assert_eq!(jid.to_string(), "alice@example.com/web");
                let Stanza::Message(receipt) = stanza.as_ref() else {
                    panic!("expected receipt message")
                };
                assert_eq!(
                    receipt.from.as_ref().map(ToString::to_string),
                    Some(local.to_string())
                );
                assert_eq!(extract_received_id(receipt), Some("msg-1".to_string()));
            }
            other => panic!("expected RouteToConnection, got {other:?}"),
        }
    }

    #[test]
    fn xep_0184_headless_recipient_pass_does_not_route_receipt() {
        let local = full("bob@example.com/custom");
        let mut message = receipt_request_message();

        let outcome = run(&local, false, &mut message);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref events) if events.is_empty()));
    }
}
