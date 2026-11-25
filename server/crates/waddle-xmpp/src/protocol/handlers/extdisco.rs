//! XEP-0215 External Service Discovery (urn:xmpp:extdisco:2).
//!
//! Synchronous handler that issues a fresh TURN credential pair and
//! a STUN entry for the requester, both pointing at the configured
//! LiveKit TURN host. The credentials are time-limited (the TURN
//! username is `<unix_expiry>:<identity>`, the password is an HMAC
//! over the username); entries carry an `expires` attribute so the
//! client can refresh proactively. Also answers `<credentials/>`
//! requests (XEP-0215 "Requesting Credentials").

use std::sync::Arc;

use minidom::Element;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use waddle_sfu::{Identity, SfuService, TurnCredential};

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::handlers::session_initiate_rate_limit::TurnCredentialRateLimit;
use crate::protocol::traits::IqHandler;
use crate::telemetry::attributes::{CallControlRateLimitedSurface, RequestOutcome};
use crate::xep::xep0215::{
    build_credentials_result_element, build_credentials_service, build_services_result_element,
    build_stun_service_element, build_turn_service, build_turns_service, ServiceKind, NS_EXT_DISCO,
};
use crate::Stanza;

#[derive(Clone)]
pub struct ExtDiscoHandler {
    sfu: Arc<dyn SfuService>,
    turn_tls_port: u16,
    turn_udp_port: u16,
    turn_credential_rate_limit: Arc<TurnCredentialRateLimit>,
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
            turn_credential_rate_limit: Arc::new(TurnCredentialRateLimit::with_defaults()),
        }
    }

    /// Constructor that accepts a caller-supplied TURN credential
    /// limiter. Used by the dedicated XEP-0215 suite to pin
    /// rate-limit wire behavior with a short window.
    pub fn with_turn_credential_rate_limit(
        sfu: Arc<dyn SfuService>,
        turn_tls_port: u16,
        turn_udp_port: u16,
        turn_credential_rate_limit: Arc<TurnCredentialRateLimit>,
    ) -> Self {
        Self {
            sfu,
            turn_tls_port,
            turn_udp_port,
            turn_credential_rate_limit,
        }
    }
}

impl ExtDiscoHandler {
    /// Apply the per-user rate limit and mint a TURN credential bound
    /// to the authenticated session JID (never the client-supplied
    /// `iq.from`, which could spoof any identity into the username).
    /// The error arm is the ready-to-send reply.
    fn mint_credential(
        &self,
        iq: &Iq,
        ctx: &StanzaContext<'_>,
    ) -> Result<TurnCredential, Vec<OutboundEvent>> {
        let requester = ctx.full_jid.to_bare();
        if let Err(exceeded) = self.turn_credential_rate_limit.check_and_record(&requester) {
            tracing::warn!(
                jid = %requester,
                %exceeded,
                "rate-limit dropped TURN credential request"
            );
            crate::telemetry::call::increment_call_control_rate_limited(
                CallControlRateLimitedSurface::Turn,
            );
            return Err(error_reply(
                iq,
                DefinedCondition::PolicyViolation,
                "TURN credential rate limit exceeded",
            ));
        }
        let identity = Identity::from_jid(ctx.full_jid.clone());
        match self.sfu.issue_turn_credentials(&identity) {
            Ok(cred) => {
                record_turn_credential_outcome(RequestOutcome::Success);
                Ok(cred)
            }
            Err(e) => {
                record_turn_credential_outcome(RequestOutcome::Failure);
                tracing::error!(
                    user = %ctx.full_jid.to_bare(),
                    error = %e,
                    "TURN credential mint failed"
                );
                Err(error_reply(
                    iq,
                    DefinedCondition::InternalServerError,
                    "internal error",
                ))
            }
        }
    }

    fn handle_services(
        &self,
        iq: &Iq,
        query: &Element,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        // Optional `type` attribute restricts the response to services
        // of exactly that type. Unknown values are rejected; an absent
        // attribute returns every service.
        let type_filter = match query.attr("type") {
            None => None,
            Some(value) => match ServiceKind::parse(value) {
                Some(kind) => Some(kind),
                None => {
                    return error_reply(
                        iq,
                        DefinedCondition::BadRequest,
                        &format!("unsupported services type filter: {value}"),
                    );
                }
            },
        };
        let wants = |kind: ServiceKind| type_filter.is_none_or(|f| f == kind);

        // Mint TURN credentials only when the response will include a
        // credential-bearing entry.
        let cred = if wants(ServiceKind::Turn) || wants(ServiceKind::Turns) {
            match self.mint_credential(iq, ctx) {
                Ok(cred) => Some(cred),
                Err(reply) => return reply,
            }
        } else {
            None
        };

        let host = self.sfu.turn_host();
        let mut services: Vec<Element> = Vec::with_capacity(3);
        if let Some(cred) = &cred {
            if wants(ServiceKind::Turn) {
                services.push(build_turn_service(host, self.turn_udp_port, cred));
            }
            if wants(ServiceKind::Turns) {
                services.push(build_turns_service(host, self.turn_tls_port, cred));
            }
        }
        if wants(ServiceKind::Stun) {
            services.push(build_stun_service_element(host, self.turn_udp_port));
        }
        let result = build_services_result_element(type_filter.map(ServiceKind::as_str), services);
        result_reply(iq, result)
    }

    /// XEP-0215 "Requesting Credentials": `<credentials/>` carries
    /// `<service/>` children whose `host` and `type` MUST be present
    /// (`port` MAY disambiguate). Services Waddle does not issue
    /// credentials for yield `item-not-found`.
    fn handle_credentials(
        &self,
        iq: &Iq,
        query: &Element,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        let mut requested: Vec<(&str, ServiceKind, Option<u16>)> = Vec::new();
        for child in query.children() {
            if !child.is("service", NS_EXT_DISCO) {
                return error_reply(
                    iq,
                    DefinedCondition::BadRequest,
                    "<credentials/> may only contain <service/> children",
                );
            }
            let (Some(host), Some(type_attr)) = (child.attr("host"), child.attr("type")) else {
                return error_reply(
                    iq,
                    DefinedCondition::BadRequest,
                    "<service/> requires 'host' and 'type' attributes",
                );
            };
            let Some(kind) = ServiceKind::parse(type_attr) else {
                return error_reply(iq, DefinedCondition::ItemNotFound, "unknown service type");
            };
            let port = match child.attr("port").map(str::parse::<u16>) {
                None => None,
                Some(Ok(port)) => Some(port),
                Some(Err(_)) => {
                    return error_reply(
                        iq,
                        DefinedCondition::BadRequest,
                        "'port' must be an unsigned short",
                    );
                }
            };
            requested.push((host, kind, port));
        }
        if requested.is_empty() {
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "<credentials/> requires a <service/> child",
            );
        }

        let host = self.sfu.turn_host();
        let offered = [
            (ServiceKind::Turn, self.turn_udp_port),
            (ServiceKind::Turns, self.turn_tls_port),
        ];
        let matched: Vec<(ServiceKind, u16)> = offered
            .into_iter()
            .filter(|(kind, port)| {
                requested.iter().any(|(h, k, p)| {
                    h.eq_ignore_ascii_case(host.as_str())
                        && k == kind
                        && p.is_none_or(|p| p == *port)
                })
            })
            .collect();
        if matched.is_empty() {
            return error_reply(
                iq,
                DefinedCondition::ItemNotFound,
                "no credentials available for the requested service",
            );
        }

        let cred = match self.mint_credential(iq, ctx) {
            Ok(cred) => cred,
            Err(reply) => return reply,
        };
        let services = matched
            .into_iter()
            .map(|(kind, port)| build_credentials_service(kind, host, port, &cred))
            .collect();
        result_reply(iq, build_credentials_result_element(services))
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
                "extdisco requests must be IQ get",
            );
        };
        if query.ns() != NS_EXT_DISCO {
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "expected <services/> or <credentials/> in urn:xmpp:extdisco:2",
            );
        }
        match query.name() {
            "services" => self.handle_services(iq, query, ctx),
            "credentials" => self.handle_credentials(iq, query, ctx),
            _ => error_reply(
                iq,
                DefinedCondition::BadRequest,
                "expected <services/> or <credentials/> in urn:xmpp:extdisco:2",
            ),
        }
    }
}

fn result_reply(iq: &Iq, payload: Element) -> Vec<OutboundEvent> {
    let reply = Iq::Result {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_string(),
        payload: Some(payload),
    };
    vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
        reply,
    ))))]
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
            ws_url: WebsocketUrl::try_new("wss://livekit.test/".parse().unwrap()).unwrap(),
            turn_host: TurnHost::new("turn.test"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
            token_ttl: Duration::seconds(3600),
            turn_ttl: Duration::seconds(3600),
        };
        Arc::new(LiveKitSfu::try_new(cfg).expect("LiveKitSfu init in test"))
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
            participant_registration: None,
            domain: "waddle.test",
            full_jid: jid,
            occupant_session: None,
            media_capabilities: None,
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
        assert_eq!(entries.len(), 3);

        let turn = entries[0];
        assert_eq!(turn.attr("type"), Some("turn"));
        assert_eq!(turn.attr("transport"), Some("udp"));
        assert_eq!(turn.attr("port"), Some("3478"));
        assert!(turn.attr("username").is_some());

        // TLS-protected TURN goes out as `type='turns'`.
        let turns = entries[1];
        assert_eq!(turns.attr("type"), Some(TYPE_TURNS));
        assert_eq!(turns.attr("transport"), Some("tcp"));
        assert_eq!(turns.attr("port"), Some("443"));
        assert!(turns.attr("username").is_some());
        assert!(turns.attr("password").is_some());
        assert!(turns.attr("expires").is_some());

        let stun = entries[2];
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

    #[tokio::test(flavor = "current_thread")]
    async fn rate_limited_turn_request_returns_policy_violation_and_counts_rejection() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let jid = test_jid();
        let handler = ExtDiscoHandler::with_turn_credential_rate_limit(
            fixture_sfu(),
            443,
            3478,
            Arc::new(TurnCredentialRateLimit::new(
                1,
                std::time::Duration::from_secs(30),
            )),
        );

        let first = handler.handle(&services_get_iq(), &ctx(&jid));
        assert_eq!(first.len(), 1);

        let second = handler.handle(&services_get_iq(), &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = second.into_iter().next().unwrap() else {
            panic!("expected SendStanza")
        };
        let Stanza::Iq(reply) = *stanza else {
            panic!("expected Iq")
        };
        let Iq::Error { error, .. } = *reply else {
            panic!("expected error reply")
        };
        assert_eq!(error.defined_condition, DefinedCondition::PolicyViolation);
        assert_eq!(
            metrics.counter_sum("waddle.call.control.rate_limited", &[("surface", "turn")]),
            Some(1)
        );
    }

    #[test]
    fn stun_only_services_queries_bypass_the_turn_rate_limiter() {
        let jid = test_jid();
        let handler = ExtDiscoHandler::with_turn_credential_rate_limit(
            fixture_sfu(),
            443,
            3478,
            Arc::new(TurnCredentialRateLimit::new(
                0,
                std::time::Duration::from_secs(30),
            )),
        );
        let iq = Iq::Get {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "stun-only".into(),
            payload: Element::builder("services", NS_EXT_DISCO)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "stun")
                .build(),
        };

        let events = handler.handle(&iq, &ctx(&jid));

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
            panic!("expected success reply")
        };
        let entries: Vec<&Element> = elem.children().filter(|c| c.name() == "service").collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].attr("type"), Some("stun"));
    }

    #[test]
    fn wrong_child_element_returns_bad_request() {
        let iq = Iq::Get {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("waddle.test".parse().unwrap()),
            id: "e3".into(),
            payload: Element::builder("frobnicate", NS_EXT_DISCO).build(),
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
    fn services_query_with_type_turn_returns_only_turn_entry() {
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
        assert_eq!(entries[0].attr("type"), Some("turn"));
        assert_eq!(entries[0].attr("transport"), Some("udp"));
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
