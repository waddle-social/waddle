//! XEP-0215 External Service Discovery (urn:xmpp:extdisco:2).
//!
//! Synchronous handler that issues a fresh TURN credential pair and
//! a STUN entry for the requester, both pointing at the configured
//! LiveKit TURN host. The credentials are time-limited (the TURN
//! username is `<unix_expiry>:<identity>`, the password is an HMAC
//! over the username); the response carries an `expires` attribute
//! per XEP-0215 §3.6.5 so the client can refresh proactively.

use std::sync::Arc;

use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use waddle_sfu::{Identity, SfuService};

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::xep::xep0215::{
    build_stun_service, build_turn_service, ServiceType, ServicesResult, NS_EXT_DISCO,
};
use crate::Stanza;

#[derive(Clone)]
pub struct ExtDiscoHandler {
    sfu: Arc<dyn SfuService>,
    turn_tls_port: u16,
    turn_udp_port: u16,
}

impl std::fmt::Debug for ExtDiscoHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtDiscoHandler")
            .field("turn_tls_port", &self.turn_tls_port)
            .field("turn_udp_port", &self.turn_udp_port)
            .finish_non_exhaustive()
    }
}

impl ExtDiscoHandler {
    pub fn new(sfu: Arc<dyn SfuService>, turn_tls_port: u16, turn_udp_port: u16) -> Self {
        Self {
            sfu,
            turn_tls_port,
            turn_udp_port,
        }
    }
}

impl IqHandler for ExtDiscoHandler {
    fn namespace(&self) -> &'static str {
        NS_EXT_DISCO
    }

    fn handle(&self, iq: &Iq, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        let IqType::Get(query) = &iq.payload else {
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "extdisco services must be IQ get",
            );
        };
        if query.name() != "services" || query.ns() != NS_EXT_DISCO {
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "expected <services xmlns='urn:xmpp:extdisco:2'/>",
            );
        }

        // XEP-0215 §3.6.1: optional `type` attribute restricts the
        // response to a single service type. Unknown values are
        // rejected; an absent attribute returns every supported
        // service.
        let type_filter = match query.attr("type") {
            None => None,
            Some("turn") => Some(ServiceType::Turn),
            Some("stun") => Some(ServiceType::Stun),
            Some(other) => {
                return error_reply(
                    iq,
                    DefinedCondition::BadRequest,
                    &format!("unsupported services type filter: {other}"),
                );
            }
        };

        // Mint TURN credentials only when the response will actually
        // include the TURN entry. Avoids a wasted HMAC for clients
        // that explicitly filter to `type='stun'`.
        let want_turn = type_filter != Some(ServiceType::Stun);
        let want_stun = type_filter != Some(ServiceType::Turn);
        let mut services = Vec::with_capacity(2);
        if want_turn {
            // Mint credentials scoped to the authenticated session
            // JID, never the client-supplied `iq.from` (which could
            // spoof any identity into the TURN username).
            let identity = Identity::from_jid(ctx.full_jid.clone());
            match self.sfu.issue_turn_credentials(&identity) {
                Ok(cred) => services.push(build_turn_service(
                    self.sfu.turn_host(),
                    self.turn_tls_port,
                    &cred,
                )),
                Err(e) => {
                    tracing::error!(error = %e, "TURN credential mint failed");
                    return error_reply(
                        iq,
                        DefinedCondition::InternalServerError,
                        "internal error",
                    );
                }
            }
        }
        if want_stun {
            services.push(build_stun_service(self.sfu.turn_host(), self.turn_udp_port));
        }
        let result = ServicesResult {
            type_: type_filter,
            services,
        };
        let reply = Iq {
            from: iq.to.clone(),
            to: iq.from.clone(),
            id: iq.id.clone(),
            payload: IqType::Result(Some(result.into())),
        };
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(reply)))]
    }
}

fn error_reply(original: &Iq, cond: DefinedCondition, text: &str) -> Vec<OutboundEvent> {
    let err = Iq {
        from: original.to.clone(),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Error(StanzaError::new(ErrorType::Cancel, cond, "en", text)),
    };
    vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(err)))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0215::{ServiceType, Transport};
    use chrono::Duration;
    use jid::FullJid;
    use minidom::Element;
    use waddle_sfu::{
        ApiKey, ApiSecret, LiveKitSfu, SfuConfig, TurnHost, TurnSharedSecret, WebsocketUrl,
    };

    fn fixture_sfu() -> Arc<dyn SfuService> {
        let cfg = SfuConfig {
            api_key: ApiKey::new("APIxxxxxxxx"),
            api_secret: ApiSecret::from_text("api-secret-meets-min-length-32!!")
                .expect("test secret meets min length"),
            ws_url: WebsocketUrl::new("wss://livekit.test/".parse().unwrap()).unwrap(),
            turn_host: TurnHost::new("turn.test"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
            token_ttl: Duration::seconds(3600),
            turn_ttl: Duration::seconds(3600),
        };
        Arc::new(LiveKitSfu::new(cfg))
    }

    fn test_jid() -> FullJid {
        "alice@waddle.test/desktop".parse().unwrap()
    }

    fn services_get_iq() -> Iq {
        Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "e1".into(),
            payload: IqType::Get(Element::builder("services", NS_EXT_DISCO).build()),
        }
    }

    fn ctx<'a>(jid: &'a FullJid) -> StanzaContext<'a> {
        StanzaContext {
            domain: "waddle.test",
            full_jid: jid,
        }
    }

    #[test]
    fn handler_namespace_is_extdisco() {
        assert_eq!(
            ExtDiscoHandler::new(fixture_sfu(), 443, 3478).namespace(),
            NS_EXT_DISCO
        );
    }

    #[test]
    fn services_query_returns_turn_and_stun_entries() {
        let iq = services_get_iq();
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));

        assert_eq!(events.len(), 1);
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!("expected SendStanza")
        };
        let Stanza::Iq(reply) = *stanza else {
            panic!("expected Iq")
        };
        let IqType::Result(Some(elem)) = reply.payload else {
            panic!("expected Iq result with payload")
        };
        let parsed = ServicesResult::try_from(elem).expect("services result parses");
        assert_eq!(parsed.services.len(), 2);
        assert_eq!(parsed.services[0].type_, ServiceType::Turn);
        assert_eq!(parsed.services[0].transport, Some(Transport::Tcp));
        assert_eq!(parsed.services[0].port, Some(443));
        assert!(parsed.services[0].username.is_some());
        assert!(parsed.services[0].password.is_some());
        assert!(parsed.services[0].expires.is_some());
        assert_eq!(parsed.services[1].type_, ServiceType::Stun);
        assert_eq!(parsed.services[1].transport, Some(Transport::Udp));
        assert_eq!(parsed.services[1].port, Some(3478));
    }

    #[test]
    fn wrong_child_element_returns_bad_request() {
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "e3".into(),
            // Right namespace, wrong element name.
            payload: IqType::Get(Element::builder("credentials", NS_EXT_DISCO).build()),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let IqType::Error(err) = reply.payload else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn wrong_namespace_returns_bad_request() {
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "e4".into(),
            payload: IqType::Get(Element::builder("services", "urn:xmpp:other:1").build()),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let IqType::Error(err) = reply.payload else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn services_query_with_type_turn_returns_only_turn_entry() {
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "f1".into(),
            payload: IqType::Get(
                Element::builder("services", NS_EXT_DISCO)
                    .attr("type", "turn")
                    .build(),
            ),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let IqType::Result(Some(elem)) = reply.payload else {
            panic!("expected result")
        };
        let parsed = ServicesResult::try_from(elem).expect("services result parses");
        assert_eq!(parsed.type_, Some(ServiceType::Turn));
        assert_eq!(parsed.services.len(), 1);
        assert_eq!(parsed.services[0].type_, ServiceType::Turn);
    }

    #[test]
    fn services_query_with_type_stun_skips_turn_mint() {
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "f2".into(),
            payload: IqType::Get(
                Element::builder("services", NS_EXT_DISCO)
                    .attr("type", "stun")
                    .build(),
            ),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let IqType::Result(Some(elem)) = reply.payload else {
            panic!("expected result")
        };
        let parsed = ServicesResult::try_from(elem).expect("services result parses");
        assert_eq!(parsed.type_, Some(ServiceType::Stun));
        assert_eq!(parsed.services.len(), 1);
        assert_eq!(parsed.services[0].type_, ServiceType::Stun);
        assert!(parsed.services[0].username.is_none());
    }

    #[test]
    fn services_query_with_unsupported_type_returns_bad_request() {
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "f3".into(),
            payload: IqType::Get(
                Element::builder("services", NS_EXT_DISCO)
                    .attr("type", "ftp")
                    .build(),
            ),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let IqType::Error(err) = reply.payload else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn iq_set_rejected_as_bad_request() {
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "e2".into(),
            payload: IqType::Set(Element::builder("services", NS_EXT_DISCO).build()),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let IqType::Error(err) = reply.payload else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }
}
