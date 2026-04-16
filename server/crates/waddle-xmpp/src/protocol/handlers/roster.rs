//! RFC 6121 — roster query (`jabber:iq:roster`).
//!
//! Waddle does not persist per-user rosters (channels/waddles are the
//! discovery mechanism). For compatibility with roster-querying XMPP
//! clients we return an empty `<query xmlns='jabber:iq:roster'/>` in the
//! IQ result, which is what RFC 6121 §2.1.1 calls the "initial roster
//! request" response shape for a user with no contacts.
//!
//! The behaviour matches what the legacy string-matching path in
//! `routes::websocket::handle_iq` did for `jabber:iq:roster`.

use crate::parser::stanza_to_string;
use crate::protocol::event::{IqContext, OutboundEvent};
use crate::protocol::traits::IqHandler;
use crate::roster::ROSTER_NS;
use minidom::Element;
use tracing::Level;
use xmpp_parsers::iq::{Iq, IqType};

/// Handler for `jabber:iq:roster` IQ get/set.
#[derive(Debug, Default, Clone, Copy)]
pub struct RosterHandler;

impl IqHandler for RosterHandler {
    fn namespace(&self) -> &'static str {
        ROSTER_NS
    }

    fn handle(&self, iq: &Iq, _ctx: &IqContext<'_>) -> Vec<OutboundEvent> {
        // The result must echo an empty <query> element back, not be
        // a bare empty result; some clients reject the latter.
        let empty_query = Element::builder("query", ROSTER_NS).build();
        let result = Iq {
            from: iq.to.clone(),
            to: iq.from.clone(),
            id: iq.id.clone(),
            payload: IqType::Result(Some(empty_query)),
        };
        match stanza_to_string(result) {
            Ok(xml) => vec![OutboundEvent::SendFrame(xml)],
            Err(err) => vec![OutboundEvent::Log {
                level: Level::ERROR,
                message: format!("Failed to serialize empty roster result: {err}"),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::iq::{Iq, IqType};

    fn test_jid() -> jid::FullJid {
        "alice@waddle.social/web".parse().expect("valid test JID")
    }

    #[test]
    fn handler_namespace_is_roster() {
        assert_eq!(RosterHandler.namespace(), ROSTER_NS);
    }

    #[test]
    fn roster_get_returns_empty_query_result() {
        let query = Element::builder("query", ROSTER_NS).build();
        let iq = Iq {
            from: Some("alice@waddle.social/web".parse().expect("jid")),
            to: Some("waddle.social".parse().expect("jid")),
            id: "r1".to_string(),
            payload: IqType::Get(query),
        };
        let jid = test_jid();
        let ctx = IqContext {
            domain: "waddle.social",
            full_jid: &jid,
        };

        let events = RosterHandler.handle(&iq, &ctx);

        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendFrame(xml) => {
                assert!(xml.contains("type=\"result\""), "xml={xml}");
                assert!(xml.contains("id=\"r1\""), "xml={xml}");
                assert!(xml.contains("jabber:iq:roster"), "xml={xml}");
                assert!(xml.contains("from=\"waddle.social\""), "xml={xml}");
                assert!(xml.contains("to=\"alice@waddle.social/web\""), "xml={xml}");
            }
            other => panic!("expected SendFrame, got {other:?}"),
        }
    }
}
