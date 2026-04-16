//! RFC 3921 / legacy XEP-0078 — session establishment.
//!
//! Session establishment was removed from RFC 6121 (the binding step alone
//! is sufficient) but many older XMPP clients still send the empty
//! `<session/>` IQ and will refuse to proceed until they get a `result`.
//! This handler exists solely for compatibility: it acknowledges the
//! request with an empty result IQ and otherwise does nothing.

use super::empty_iq_result;
use crate::parser::{ns, stanza_to_string};
use crate::protocol::event::{IqContext, OutboundEvent};
use crate::protocol::traits::IqHandler;
use tracing::Level;
use xmpp_parsers::iq::Iq;

/// Handler for `urn:ietf:params:xml:ns:xmpp-session`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SessionHandler;

impl IqHandler for SessionHandler {
    fn namespace(&self) -> &'static str {
        ns::SESSION
    }

    fn handle(&self, iq: &Iq, _ctx: &IqContext<'_>) -> Vec<OutboundEvent> {
        match stanza_to_string(empty_iq_result(iq)) {
            Ok(xml) => vec![OutboundEvent::SendFrame(xml)],
            Err(err) => vec![OutboundEvent::Log {
                level: Level::ERROR,
                message: format!("Failed to serialize session result: {err}"),
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
    fn handler_namespace_is_session() {
        assert_eq!(SessionHandler.namespace(), ns::SESSION);
    }

    #[test]
    fn session_iq_set_produces_empty_result() {
        let session_elem = Element::builder("session", ns::SESSION).build();
        let iq = Iq {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("waddle.social".parse().expect("valid jid")),
            id: "s1".to_string(),
            payload: IqType::Set(session_elem),
        };
        let jid = test_jid();
        let ctx = IqContext {
            domain: "waddle.social",
            full_jid: &jid,
        };

        let events = SessionHandler.handle(&iq, &ctx);

        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendFrame(xml) => {
                assert!(xml.contains("type=\"result\""), "xml={xml}");
                assert!(xml.contains("id=\"s1\""), "xml={xml}");
                // Addresses must be swapped.
                assert!(xml.contains("from=\"waddle.social\""), "xml={xml}");
                assert!(xml.contains("to=\"alice@waddle.social/web\""), "xml={xml}");
            }
            other => panic!("expected SendFrame, got {other:?}"),
        }
    }
}
