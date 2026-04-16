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
use crate::parser::stanza_to_string;
use crate::protocol::event::{IqContext, OutboundEvent};
use crate::protocol::traits::IqHandler;
use tracing::Level;
use xmpp_parsers::iq::Iq;

/// Handler for `urn:xmpp:carbons:2` IQ set.
#[derive(Debug, Default, Clone, Copy)]
pub struct CarbonsHandler;

impl IqHandler for CarbonsHandler {
    fn namespace(&self) -> &'static str {
        CARBONS_NS
    }

    fn handle(&self, iq: &Iq, _ctx: &IqContext<'_>) -> Vec<OutboundEvent> {
        match stanza_to_string(empty_iq_result(iq)) {
            Ok(xml) => vec![OutboundEvent::SendFrame(xml)],
            Err(err) => vec![OutboundEvent::Log {
                level: Level::ERROR,
                message: format!("Failed to serialize carbons result: {err}"),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::iq::{Iq, IqType};

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
        let iq = Iq {
            from: Some("alice@waddle.social/web".parse().expect("jid")),
            to: None,
            id: "c1".to_string(),
            payload: IqType::Set(enable),
        };
        let jid = test_jid();
        let ctx = IqContext {
            domain: "waddle.social",
            full_jid: &jid,
        };
        let events = CarbonsHandler.handle(&iq, &ctx);
        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendFrame(xml) => {
                assert!(xml.contains("type=\"result\""), "xml={xml}");
                assert!(xml.contains("id=\"c1\""), "xml={xml}");
            }
            other => panic!("expected SendFrame, got {other:?}"),
        }
    }
}
