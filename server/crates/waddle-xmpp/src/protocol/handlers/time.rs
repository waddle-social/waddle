//! XEP-0202 — Entity Time.

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::xep::xep0202::{build_time_response_utc, NS_TIME};
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
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(
            build_time_response_utc(iq),
        )))]
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
    fn handler_namespace_is_time() {
        assert_eq!(TimeHandler.namespace(), NS_TIME);
    }

    #[test]
    fn time_query_produces_result_with_utc_and_tzo() {
        let time = Element::builder("time", NS_TIME).build();
        let iq = Iq {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("waddle.social".parse().expect("valid jid")),
            id: "time1".to_string(),
            payload: IqType::Get(time),
        };
        let jid = test_ctx_jid();
        let ctx = StanzaContext {
            domain: "waddle.social",
            full_jid: &jid,
        };

        let events = TimeHandler.handle(&iq, &ctx);

        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    assert_eq!(reply.id, "time1");
                    let IqType::Result(Some(payload)) = &reply.payload else {
                        panic!("expected time result payload, got {:?}", reply.payload);
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
