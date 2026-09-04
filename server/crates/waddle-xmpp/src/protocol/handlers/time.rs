//! XEP-0202 — Entity Time.

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::xep::xep0202::{build_current_time_response, NS_TIME};
use crate::Stanza;
use xmpp_parsers::iq::Iq;

/// Handler for `urn:xmpp:time` (XEP-0202).
#[derive(Debug, Default, Clone, Copy)]
pub struct TimeHandler;

impl IqHandler for TimeHandler {
    fn namespace(&self) -> &'static str {
        NS_TIME
    }

    fn handle(&self, iq: &Iq, _ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
            build_current_time_response(iq),
        ))))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::iq::Iq;

    fn test_ctx_jid() -> jid::FullJid {
        "alice@waddle.social/web".parse().expect("valid test JID")
    }

    #[test]
    fn handler_namespace_is_time() {
        assert_eq!(TimeHandler.namespace(), NS_TIME);
    }

    #[test]
    fn time_query_produces_result_with_utc_and_tzo() {
        let time = Element::builder("time", NS_TIME).build();
        let iq = Iq::Get {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("waddle.social".parse().expect("valid jid")),
            id: "time1".to_string(),
            payload: time,
        };
        let jid = test_ctx_jid();
        let ctx = StanzaContext {
            domain: "waddle.social",
            full_jid: &jid,
            occupant_session: None,
            media_capabilities: None,
        };

        let events = TimeHandler.handle(&iq, &ctx);

        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    assert_eq!(reply.id(), "time1");
                    let Iq::Result {
                        payload: Some(payload),
                        ..
                    } = reply.as_ref()
                    else {
                        panic!("expected time result payload, got {reply:?}");
                    };
                    assert_eq!(payload.name(), "time");
                    assert_eq!(payload.ns(), NS_TIME);
                    assert!(payload.get_child("utc", NS_TIME).is_some());
                    assert!(payload.get_child("tzo", NS_TIME).is_some());
                }
                other => panic!("expected Iq stanza, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }
}
