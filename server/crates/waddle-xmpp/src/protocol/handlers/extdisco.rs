//! XEP-0215 External Service Discovery (urn:xmpp:extdisco:2).
//!
//! Synchronous handler that issues a fresh TURN credential pair and
//! a STUN entry for the requester, both pointing at the configured
//! LiveKit TURN host. The credentials are time-limited (the TURN
//! username is `<unix_expiry>:<identity>`, the password is an HMAC
//! over the username); the response carries an `expires` attribute
//! per XEP-0215 §3.6.5 so the client can refresh proactively.

use std::sync::Arc;

use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use waddle_sfu::{Identity, SfuService};

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::telemetry::attributes::RequestOutcome;
use crate::xep::xep0215::{
    build_services_result_element, build_stun_service_element, build_turn_service, ServiceType,
    NS_EXT_DISCO,
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
        let Iq::Get { payload: query, .. } = iq else {
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
        let mut services: Vec<minidom::Element> = Vec::with_capacity(2);
        if want_turn {
            // Mint credentials scoped to the authenticated session
            // JID, never the client-supplied `iq.from` (which could
            // spoof any identity into the TURN username).
            let identity = Identity::from_jid(ctx.full_jid.clone());
            match self.sfu.issue_turn_credentials(&identity) {
                Ok(cred) => {
                    record_turn_credential_outcome(RequestOutcome::Success);
                    services.push(build_turn_service(
                        self.sfu.turn_host(),
                        self.turn_tls_port,
                        &cred,
                    ));
                }
                Err(e) => {
                    record_turn_credential_outcome(RequestOutcome::Failure);
                    tracing::error!(
                        user = %ctx.full_jid.to_bare(),
                        error = %e,
                        "TURN credential mint failed"
                    );
                    return error_reply(
                        iq,
                        DefinedCondition::InternalServerError,
                        "internal error",
                    );
                }
            }
        }
        if want_stun {
            services.push(build_stun_service_element(
                self.sfu.turn_host(),
                self.turn_udp_port,
            ));
        }
        // The wrapper element is built manually rather than via
        // `ServicesResult` so it can carry the manually-built
        // `<service type='turns'/>` child — `ServicesResult.services:
        // Vec<Service>` can only hold the typed `Stun`/`Turn`
        // variants and would emit `type='turn'` (wrong per XEP-0215
        // §3.6.5).
        let type_filter_str = type_filter.map(service_type_wire_str);
        let result = build_services_result_element(type_filter_str, services);
        let reply = Iq::Result {
            from: iq.to().cloned(),
            to: iq.from().cloned(),
            id: iq.id().to_string(),
            payload: Some(result),
        };
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
            reply,
        ))))]
    }
}

fn record_turn_credential_outcome(outcome: RequestOutcome) {
    crate::counter_add!(
        "waddle.call.turn_credentials",
        "1",
        "TURN credential issuance attempts by outcome.",
        1,
        outcome,
    );
}

/// Wire-form `type` value for the (typed) filter on the inbound
/// request. The TLS-protected `turns` variant is only ever emitted
/// in the response (XEP-0215 §3.6.5); the filter on the request
/// itself uses the typed surface, which never includes `turns`.
fn service_type_wire_str(t: ServiceType) -> &'static str {
    match t {
        ServiceType::Stun => "stun",
        ServiceType::Turn => "turn",
    }
}

fn error_reply(original: &Iq, cond: DefinedCondition, text: &str) -> Vec<OutboundEvent> {
    let err = Iq::Error {
        from: original.to().cloned(),
        to: original.from().cloned(),
        id: original.id().to_string(),
        error: StanzaError::new(ErrorType::Cancel, cond, "en", text),
        payload: None,
    };
    vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
        err,
    ))))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0215::TYPE_TURNS;
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
            webhook_secret: ApiSecret::from_text("api-secret-meets-min-length-32!!")
                .expect("test secret meets min length"),
            ws_url: WebsocketUrl::new("wss://livekit.test/".parse().unwrap()).unwrap(),
            turn_host: TurnHost::new("turn.test"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
            token_ttl: Duration::seconds(3600),
            turn_ttl: Duration::seconds(3600),
        };
        Arc::new(LiveKitSfu::new(cfg).expect("LiveKitSfu init in test"))
    }

    fn test_jid() -> FullJid {
        "alice@waddle.test/desktop".parse().unwrap()
    }

    fn services_get_iq() -> Iq {
        Iq::Get {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "e1".into(),
            payload: Element::builder("services", NS_EXT_DISCO).build(),
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
    fn services_query_returns_turns_and_stun_entries() {
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
        let Iq::Result {
            payload: Some(elem),
            ..
        } = *reply
        else {
            panic!("expected Iq result with payload")
        };
        assert_eq!(elem.name(), "services");
        assert_eq!(elem.ns(), NS_EXT_DISCO);

        let entries: Vec<&Element> = elem.children().filter(|c| c.name() == "service").collect();
        assert_eq!(entries.len(), 2);

        // XEP-0215 §3.6.5: TLS-protected TURN must go out as
        // `type='turns'`. The manual element-building path is
        // exactly what makes this possible — `ServicesResult`'s
        // typed `Vec<Service>` cannot represent this value.
        let turns = entries[0];
        assert_eq!(turns.attr("type"), Some(TYPE_TURNS));
        assert_eq!(turns.attr("transport"), Some("tcp"));
        assert_eq!(turns.attr("port"), Some("443"));
        assert!(turns.attr("username").is_some());
        assert!(turns.attr("password").is_some());
        assert!(turns.attr("expires").is_some());

        let stun = entries[1];
        assert_eq!(stun.attr("type"), Some("stun"));
        assert_eq!(stun.attr("transport"), Some("udp"));
        assert_eq!(stun.attr("port"), Some("3478"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn turn_credential_issuance_emits_success_counter() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);

        let events = handler.handle(&services_get_iq(), &ctx(&jid));

        assert_eq!(events.len(), 1);
        assert_eq!(
            metrics.counter_sum("waddle.call.turn_credentials", &[("outcome", "success")]),
            Some(1)
        );
        assert_eq!(
            metrics.metric_unit("waddle.call.turn_credentials"),
            Some("1".to_string())
        );
    }

    #[test]
    fn wrong_child_element_returns_bad_request() {
        let iq = Iq::Get {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "e3".into(),
            payload: Element::builder("credentials", NS_EXT_DISCO).build(),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let Iq::Error { error: err, .. } = *reply else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn wrong_namespace_returns_bad_request() {
        let iq = Iq::Get {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "e4".into(),
            payload: Element::builder("services", "urn:xmpp:other:1").build(),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let Iq::Error { error: err, .. } = *reply else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn services_query_with_type_turn_returns_only_turns_entry() {
        let iq = Iq::Get {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "f1".into(),
            payload: Element::builder("services", NS_EXT_DISCO)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "turn")
                .build(),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let Iq::Result {
            payload: Some(elem),
            ..
        } = *reply
        else {
            panic!("expected result")
        };
        assert_eq!(elem.attr("type"), Some("turn"));
        let entries: Vec<&Element> = elem.children().filter(|c| c.name() == "service").collect();
        assert_eq!(entries.len(), 1);
        // The filter on the request is the typed surface ("turn"),
        // but the response is the XEP-conformant "turns" wire value.
        assert_eq!(entries[0].attr("type"), Some(TYPE_TURNS));
    }

    #[test]
    fn services_query_with_type_stun_skips_turn_mint() {
        let iq = Iq::Get {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "f2".into(),
            payload: Element::builder("services", NS_EXT_DISCO)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "stun")
                .build(),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let Iq::Result {
            payload: Some(elem),
            ..
        } = *reply
        else {
            panic!("expected result")
        };
        assert_eq!(elem.attr("type"), Some("stun"));
        let entries: Vec<&Element> = elem.children().filter(|c| c.name() == "service").collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].attr("type"), Some("stun"));
        assert!(entries[0].attr("username").is_none());
    }

    #[test]
    fn services_query_with_unsupported_type_returns_bad_request() {
        let iq = Iq::Get {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "f3".into(),
            payload: Element::builder("services", NS_EXT_DISCO)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "ftp")
                .build(),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let Iq::Error { error: err, .. } = *reply else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn iq_set_rejected_as_bad_request() {
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "e2".into(),
            payload: Element::builder("services", NS_EXT_DISCO).build(),
        };
        let jid = test_jid();
        let handler = ExtDiscoHandler::new(fixture_sfu(), 443, 3478);
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let Iq::Error { error: err, .. } = *reply else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }
}
