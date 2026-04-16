//! XEP-0199 — XMPP Ping.
//!
//! The simplest possible handler: takes a `<ping>` IQ-get, emits the
//! corresponding IQ-result with swapped `to`/`from` addresses.

use crate::parser::stanza_to_string;
use crate::protocol::event::{IqContext, OutboundEvent};
use crate::protocol::traits::IqHandler;
use crate::xep::xep0199;
use tracing::Level;
use xmpp_parsers::iq::Iq;

/// Handler for `urn:xmpp:ping` (XEP-0199).
#[derive(Debug, Default, Clone, Copy)]
pub struct PingHandler;

impl IqHandler for PingHandler {
    fn namespace(&self) -> &'static str {
        xep0199::NS_PING
    }

    fn handle(&self, iq: &Iq, _ctx: &IqContext<'_>) -> Vec<OutboundEvent> {
        let result = xep0199::build_ping_result(iq);
        match stanza_to_string(result) {
            Ok(xml) => vec![OutboundEvent::SendFrame(xml)],
            Err(err) => vec![OutboundEvent::Log {
                level: Level::ERROR,
                message: format!("Failed to serialize ping result: {err}"),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::iq::{Iq, IqType};

    fn test_ctx_jid() -> jid::FullJid {
        "alice@waddle.social/web".parse().expect("valid test JID")
    }

    #[test]
    fn handler_namespace_is_urn_xmpp_ping() {
        assert_eq!(PingHandler.namespace(), "urn:xmpp:ping");
    }

    #[test]
    fn ping_get_produces_result_frame() {
        let ping_elem = Element::builder("ping", xep0199::NS_PING).build();
        let iq = Iq {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("waddle.social".parse().expect("valid jid")),
            id: "p1".to_string(),
            payload: IqType::Get(ping_elem),
        };
        let jid = test_ctx_jid();
        let ctx = IqContext {
            domain: "waddle.social",
            full_jid: &jid,
        };

        let events = PingHandler.handle(&iq, &ctx);

        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendFrame(xml) => {
                assert!(xml.contains("type=\"result\""));
                assert!(xml.contains("id=\"p1\""));
            }
            other => panic!("expected SendFrame, got {other:?}"),
        }
    }
}
