//! XEP-0280 — Message Carbons enable/disable.
//!
//! Carbons let a client see copies of messages sent from / received on
//! its *other* resources. The request is a simple IQ-set containing
//! `<enable xmlns='urn:xmpp:carbons:2'/>` or the mirror `<disable/>`.
//!
//! At this stage we ack the request with an empty result IQ — which
//! matches the legacy behaviour in `routes::websocket::handle_iq`.
//! Tracking the per-connection enabled flag and actually fanning out
//! carbon copies is follow-up work that needs the two-phase async
//! callback machinery (to update `ConnectionRegistry` state from a
//! handler). This handler intentionally does not error on unknown
//! `<enable|disable>` payloads so existing clients that send a bare
//! enable still get their expected ack.

use super::empty_iq_result;
use crate::carbons::CARBONS_NS;
use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::Stanza;
use xmpp_parsers::iq::Iq;

/// Handler for `urn:xmpp:carbons:2` IQ set.
#[derive(Debug, Default, Clone, Copy)]
pub struct CarbonsHandler;

impl IqHandler for CarbonsHandler {
    fn namespace(&self) -> &'static str {
        CARBONS_NS
    }

    fn handle(&self, iq: &Iq, _ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
            empty_iq_result(iq),
        ))))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::iq::Iq;

    fn test_jid() -> jid::FullJid {
        "alice@waddle.social/web".parse().expect("valid test JID")
    }

    #[test]
    fn handler_namespace_is_carbons() {
        assert_eq!(CarbonsHandler.namespace(), CARBONS_NS);
    }

    #[test]
    fn enable_produces_empty_result() {
        let enable = Element::builder("enable", CARBONS_NS).build();
        let iq = Iq::Set {
            from: Some("alice@waddle.social/web".parse().expect("jid")),
            to: None,
            id: "c1".to_string(),
            payload: enable,
        };
        let jid = test_jid();
        let ctx = StanzaContext {
            domain: "waddle.social",
            full_jid: &jid,
            media_capabilities: None,
        };
        let events = CarbonsHandler.handle(&iq, &ctx);
        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    assert_eq!(reply.id(), "c1");
                    assert!(matches!(reply.as_ref(), Iq::Result { payload: None, .. }));
                }
                other => panic!("expected Iq, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }
}
