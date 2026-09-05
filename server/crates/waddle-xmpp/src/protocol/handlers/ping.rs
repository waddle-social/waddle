//! XEP-0199 — XMPP Ping.
//!
//! Validates incoming ping IQs and emits either a conformant pong reply
//! or a typed `<bad-request/>` error for malformed requests.

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::xep::xep0199;
use crate::Stanza;
use xmpp_parsers::iq::Iq;

/// Handler for `urn:xmpp:ping` (XEP-0199).
#[derive(Debug, Default, Clone, Copy)]
pub struct PingHandler;

impl IqHandler for PingHandler {
    fn namespace(&self) -> &'static str {
        xep0199::NS_PING
    }

    fn handle(&self, iq: &Iq, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        let session_bare = ctx.full_jid.to_bare();
        let reply = if xep0199::is_ping(iq) {
            xep0199::build_ping_result(iq, ctx.domain, &session_bare)
        } else {
            xep0199::build_ping_bad_request(iq, ctx.domain, &session_bare)
        };
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
            reply,
        ))))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::iq::Iq;
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

    fn test_ctx_jid() -> jid::FullJid {
        "alice@waddle.social/web".parse().expect("valid test JID")
    }

    #[test]
    fn handler_namespace_is_urn_xmpp_ping() {
        assert_eq!(PingHandler.namespace(), "urn:xmpp:ping");
    }

    #[test]
    fn ping_get_produces_result_stanza() {
        let ping_elem = Element::builder("ping", xep0199::NS_PING).build();
        let iq = Iq::Get {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("waddle.social".parse().expect("valid jid")),
            id: "p1".to_string(),
            payload: ping_elem,
        };
        let jid = test_ctx_jid();
        let ctx = StanzaContext {
            domain: "waddle.social",
            full_jid: &jid,
            occupant_session: None,
            media_capabilities: None,
        };

        let events = PingHandler.handle(&iq, &ctx);

        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    assert_eq!(reply.id(), "p1");
                    assert!(matches!(reply.as_ref(), Iq::Result { .. }));
                    assert_eq!(
                        reply.from().map(|j| j.to_string()),
                        Some("waddle.social".to_string())
                    );
                    assert_eq!(
                        reply.to().map(|j| j.to_string()),
                        Some("alice@waddle.social/web".to_string())
                    );
                }
                other => panic!("expected Iq stanza, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn ping_without_to_uses_server_domain_in_result() {
        let ping_elem = Element::builder("ping", xep0199::NS_PING).build();
        let iq = Iq::Get {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: None,
            id: "p2".to_string(),
            payload: ping_elem,
        };
        let jid = test_ctx_jid();
        let ctx = StanzaContext {
            domain: "waddle.social",
            full_jid: &jid,
            occupant_session: None,
            media_capabilities: None,
        };

        let events = PingHandler.handle(&iq, &ctx);

        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    assert_eq!(
                        reply.from().map(|j| j.to_string()),
                        Some("waddle.social".into())
                    );
                    assert_eq!(
                        reply.to().map(|j| j.to_string()),
                        Some("alice@waddle.social/web".into())
                    );
                    assert!(matches!(reply.as_ref(), Iq::Result { .. }));
                }
                other => panic!("expected Iq stanza, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn malformed_ping_produces_bad_request_error() {
        let ping_elem = Element::builder("ping", xep0199::NS_PING)
            .append(Element::builder("extra", xep0199::NS_PING).build())
            .build();
        let iq = Iq::Get {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: None,
            id: "p3".to_string(),
            payload: ping_elem,
        };
        let jid = test_ctx_jid();
        let ctx = StanzaContext {
            domain: "waddle.social",
            full_jid: &jid,
            occupant_session: None,
            media_capabilities: None,
        };

        let events = PingHandler.handle(&iq, &ctx);

        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    assert_eq!(
                        reply.from().map(|j| j.to_string()),
                        Some("waddle.social".into())
                    );
                    assert_eq!(
                        reply.to().map(|j| j.to_string()),
                        Some("alice@waddle.social/web".into())
                    );
                    let Iq::Error { error, .. } = reply.as_ref() else {
                        panic!("expected error reply")
                    };
                    assert_eq!(error.type_, ErrorType::Modify);
                    assert_eq!(error.defined_condition, DefinedCondition::BadRequest);
                }
                other => panic!("expected Iq stanza, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }
}
