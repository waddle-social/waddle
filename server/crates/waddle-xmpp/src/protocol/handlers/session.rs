//! RFC 3921 / legacy XEP-0078 — session establishment.
//!
//! Session establishment was removed from RFC 6121 (the binding step alone
//! is sufficient) but many older XMPP clients still send the empty
//! `<session/>` IQ and will refuse to proceed until they get a `result`.
//! This handler exists solely for compatibility: it acknowledges the
//! request with an empty result IQ and otherwise does nothing.

use super::empty_iq_result;
use crate::parser::ns;
use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::Stanza;
use xmpp_parsers::iq::Iq;

/// Handler for `urn:ietf:params:xml:ns:xmpp-session`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SessionHandler;

impl IqHandler for SessionHandler {
    fn namespace(&self) -> &'static str {
        ns::SESSION
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
    fn handler_namespace_is_session() {
        assert_eq!(SessionHandler.namespace(), ns::SESSION);
    }

    #[test]
    fn session_iq_set_produces_empty_result() {
        let session_elem = Element::builder("session", ns::SESSION).build();
        let iq = Iq::Set {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("waddle.social".parse().expect("valid jid")),
            id: "s1".to_string(),
            payload: session_elem,
        };
        let jid = test_jid();
        let ctx = StanzaContext {
            domain: "waddle.social",
            full_jid: &jid,
            occupant_session: None,
            media_capabilities: None,
        };

        let events = SessionHandler.handle(&iq, &ctx);

        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    assert_eq!(reply.id(), "s1");
                    assert!(matches!(reply.as_ref(), Iq::Result { payload: None, .. }));
                    assert_eq!(
                        reply.from().map(|j| j.to_string()),
                        Some("waddle.social".to_string())
                    );
                    assert_eq!(
                        reply.to().map(|j| j.to_string()),
                        Some("alice@waddle.social/web".to_string())
                    );
                }
                other => panic!("expected Iq, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }
}
