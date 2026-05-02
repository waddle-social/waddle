//! XEP-0092 — Software Version.
//!
//! Responds with the running Waddle package version and host OS so WebSocket
//! clients can display deployment state.

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::xep::xep0092::{build_version_response, SoftwareVersion, NS_VERSION};
use crate::Stanza;
use xmpp_parsers::iq::Iq;

/// Handler for `jabber:iq:version` (XEP-0092).
#[derive(Debug, Default, Clone, Copy)]
pub struct VersionHandler;

impl IqHandler for VersionHandler {
    fn namespace(&self) -> &'static str {
        NS_VERSION
    }

    fn handle(&self, iq: &Iq, _ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        let info = SoftwareVersion {
            name: "Waddle".to_string(),
            version: env!("WADDLE_GIT_SHA").to_string(),
            os: Some(std::env::consts::OS.to_string()),
        };
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(
            build_version_response(iq, &info),
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
    fn handler_namespace_is_version() {
        assert_eq!(VersionHandler.namespace(), NS_VERSION);
    }

    #[test]
    fn version_query_produces_result_with_name_version_and_os() {
        let query = Element::builder("query", NS_VERSION).build();
        let iq = Iq {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("waddle.social".parse().expect("valid jid")),
            id: "v1".to_string(),
            payload: IqType::Get(query),
        };
        let jid = test_ctx_jid();
        let ctx = StanzaContext {
            domain: "waddle.social",
            full_jid: &jid,
        };

        let events = VersionHandler.handle(&iq, &ctx);

        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    assert_eq!(reply.id, "v1");
                    assert_eq!(
                        reply.from.as_ref().map(|jid| jid.to_string()),
                        Some("waddle.social".to_string())
                    );
                    assert_eq!(
                        reply.to.as_ref().map(|jid| jid.to_string()),
                        Some("alice@waddle.social/web".to_string())
                    );
                    let IqType::Result(Some(payload)) = &reply.payload else {
                        panic!("expected version result payload, got {:?}", reply.payload);
                    };
                    assert_eq!(payload.name(), "query");
                    assert_eq!(payload.ns(), NS_VERSION);
                    assert!(payload.get_child("name", NS_VERSION).is_some());
                    let version = payload.get_child("version", NS_VERSION).expect("version");
                    assert!(!version.text().is_empty(), "version should be non-empty commit SHA");
                    assert!(payload.get_child("os", NS_VERSION).is_some());
                }
                other => panic!("expected Iq stanza, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }
}
