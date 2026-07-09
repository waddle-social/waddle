//! XEP-0092 — Software Version.
//!
//! Responds with the immutable build commit while omitting the optional
//! operating-system field, which keeps the server compliant with XEP-0092's OS
//! privacy guidance. Runtime deployment metadata cannot relabel this surface.

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::xep::xep0092::{build_version_response, is_version_query, SoftwareVersion, NS_VERSION};
use crate::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Handler for `jabber:iq:version` (XEP-0092).
#[derive(Debug, Default, Clone, Copy)]
pub struct VersionHandler;

impl IqHandler for VersionHandler {
    fn namespace(&self) -> &'static str {
        NS_VERSION
    }

    fn handle(&self, iq: &Iq, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        if !is_version_query(iq) {
            return vec![send_iq(build_version_error(
                iq,
                ErrorType::Modify,
                DefinedCondition::BadRequest,
            ))];
        }

        if iq
            .to()
            .is_some_and(|target| target.to_bare().as_str() != ctx.domain)
        {
            return vec![send_iq(build_version_error(
                iq,
                ErrorType::Cancel,
                DefinedCondition::ServiceUnavailable,
            ))];
        }

        let version = server_version();
        let info = SoftwareVersion {
            name: "Waddle".to_string(),
            version,
            os: None,
        };
        vec![send_iq(build_version_response(iq, &info))]
    }
}

fn send_iq(iq: Iq) -> OutboundEvent {
    OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(iq))))
}

fn build_version_error(iq: &Iq, error_type: ErrorType, condition: DefinedCondition) -> Iq {
    Iq::Error {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_string(),
        error: StanzaError::new(error_type, condition, "en", ""),
        payload: None,
    }
}

fn server_version() -> String {
    waddle_xmpp_core::build_identity::printable_git_sha().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::iq::Iq;

    fn test_ctx_jid() -> jid::FullJid {
        "alice@waddle.social/web".parse().expect("valid test JID")
    }

    fn reply_iq(events: &[OutboundEvent]) -> &Iq {
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => reply,
                other => panic!("expected Iq stanza, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn handler_namespace_is_version() {
        assert_eq!(VersionHandler.namespace(), NS_VERSION);
    }

    #[test]
    fn version_query_produces_result_without_os() {
        let query = Element::builder("query", NS_VERSION).build();
        let iq = Iq::Get {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("waddle.social".parse().expect("valid jid")),
            id: "v1".to_string(),
            payload: query,
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
                    assert_eq!(reply.id(), "v1");
                    assert_eq!(
                        reply.from().map(|jid| jid.to_string()),
                        Some("waddle.social".to_string())
                    );
                    assert_eq!(
                        reply.to().map(|jid| jid.to_string()),
                        Some("alice@waddle.social/web".to_string())
                    );
                    let Iq::Result {
                        payload: Some(payload),
                        ..
                    } = reply.as_ref()
                    else {
                        panic!("expected version result payload, got {reply:?}");
                    };
                    assert_eq!(payload.name(), "query");
                    assert_eq!(payload.ns(), NS_VERSION);
                    assert!(payload.get_child("name", NS_VERSION).is_some());
                    let version = payload.get_child("version", NS_VERSION).expect("version");
                    assert_eq!(
                        version.text(),
                        waddle_xmpp_core::build_identity::printable_git_sha()
                    );
                    assert!(payload.get_child("os", NS_VERSION).is_none());
                }
                other => panic!("expected Iq stanza, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn version_response_uses_the_shared_immutable_build_identity() {
        let query = Element::builder("query", NS_VERSION).build();
        let iq = Iq::Get {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("waddle.social".parse().expect("valid jid")),
            id: "v2".to_string(),
            payload: query,
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
                    let Iq::Result {
                        payload: Some(payload),
                        ..
                    } = reply.as_ref()
                    else {
                        panic!("expected version result payload");
                    };
                    let version = payload.get_child("version", NS_VERSION).expect("version");
                    assert_eq!(
                        version.text(),
                        waddle_xmpp_core::build_identity::printable_git_sha()
                    );
                }
                other => panic!("expected Iq stanza, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn version_query_rejects_non_get_iq() {
        let query = Element::builder("query", NS_VERSION).build();
        let iq = Iq::Set {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("waddle.social".parse().expect("valid jid")),
            id: "v4".to_string(),
            payload: query,
        };
        let jid = test_ctx_jid();
        let ctx = StanzaContext {
            domain: "waddle.social",
            full_jid: &jid,
        };

        let events = VersionHandler.handle(&iq, &ctx);
        let reply = reply_iq(&events);

        assert_eq!(reply.id(), "v4");
        let Iq::Error { error, .. } = reply else {
            panic!("expected version error payload");
        };
        assert_eq!(error.type_, ErrorType::Modify);
        assert_eq!(error.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn version_query_rejects_non_server_target() {
        let query = Element::builder("query", NS_VERSION).build();
        let iq = Iq::Get {
            from: Some("alice@waddle.social/web".parse().expect("valid jid")),
            to: Some("alice@waddle.social".parse().expect("valid jid")),
            id: "v5".to_string(),
            payload: query,
        };
        let jid = test_ctx_jid();
        let ctx = StanzaContext {
            domain: "waddle.social",
            full_jid: &jid,
        };

        let events = VersionHandler.handle(&iq, &ctx);
        let reply = reply_iq(&events);

        assert_eq!(reply.id(), "v5");
        let Iq::Error { error, .. } = reply else {
            panic!("expected version error payload");
        };
        assert_eq!(error.type_, ErrorType::Cancel);
        assert_eq!(
            error.defined_condition,
            DefinedCondition::ServiceUnavailable
        );
    }
}
