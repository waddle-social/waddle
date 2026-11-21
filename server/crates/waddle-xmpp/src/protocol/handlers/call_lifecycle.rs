//! Stateless client call lifecycle reports, counted only after validation and
//! per-account rate limiting. `accepted` means the call's media session
//! connected on the reporting device, rather than a signalling acknowledgement.
//!
//! The server does not deduplicate. A client sends each report at most once and
//! never retries; there is no timestamp on the wire. Reports only produce
//! counters, a structured log, and an acknowledgement; they do not control calls.
//! Unbound requests are rejected by the WebSocket sans-I/O adapter before
//! dispatch, so the handler always receives an authenticated, bound context.

use std::sync::Arc;

use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::handlers::empty_iq_result;
use crate::protocol::handlers::session_initiate_rate_limit::CallLifecycleRateLimit;
use crate::protocol::traits::IqHandler;
use crate::telemetry::attributes::CallControlRateLimitedSurface;
use crate::telemetry::call::{increment_call_control_rate_limited, record_call_client_report};
use crate::xep::{CallLifecycleReport, NS_WADDLE_CALL_LIFECYCLE};
use crate::Stanza;

#[derive(Debug, Clone)]
pub struct CallLifecycleHandler {
    rate_limit: Arc<CallLifecycleRateLimit>,
}

impl Default for CallLifecycleHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl CallLifecycleHandler {
    pub fn new() -> Self {
        Self::with_rate_limit(Arc::new(CallLifecycleRateLimit::with_defaults()))
    }

    /// Inject a limiter to exercise the same policy with a controlled budget.
    pub fn with_rate_limit(rate_limit: Arc<CallLifecycleRateLimit>) -> Self {
        Self { rate_limit }
    }
}

impl IqHandler for CallLifecycleHandler {
    fn namespace(&self) -> &'static str {
        NS_WADDLE_CALL_LIFECYCLE
    }

    fn handle(&self, iq: &Iq, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        if iq.to().is_some_and(|to| to.as_str() != ctx.domain) {
            return error_reply(
                iq,
                DefinedCondition::ServiceUnavailable,
                "Call lifecycle reports must be addressed to this server.",
            );
        }
        let Iq::Set { payload, .. } = iq else {
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "Call lifecycle reports must be IQ set.",
            );
        };
        let report = match CallLifecycleReport::try_from(payload) {
            Ok(report) => report,
            Err(_) => {
                return error_reply(
                    iq,
                    DefinedCondition::BadRequest,
                    "Malformed call lifecycle report.",
                );
            }
        };
        let reporter = ctx.full_jid.to_bare();
        if let Err(exceeded) = self.rate_limit.check_and_record(&reporter) {
            tracing::warn!(
                user = %reporter,
                %exceeded,
                "rate-limit dropped call lifecycle report"
            );
            increment_call_control_rate_limited(CallControlRateLimitedSurface::LifecycleReport);
            return error_reply(
                iq,
                DefinedCondition::PolicyViolation,
                "Call lifecycle report rate limit exceeded.",
            );
        }

        record_call_client_report(&report);
        tracing::info!(
            user = %reporter,
            kind = report.kind.as_str(),
            setup_outcome = report.setup_outcome.as_str(),
            end_reason = report.end_reason.as_str(),
            duration = report.duration.as_str(),
            reconnects = report.reconnects.as_str(),
            platform = report.platform.as_str(),
            "client call lifecycle report"
        );
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
            empty_iq_result(iq),
        ))))]
    }
}

fn error_reply(iq: &Iq, condition: DefinedCondition, text: &str) -> Vec<OutboundEvent> {
    // Never speak as the addressee: a report sent to another entity is
    // refused by this server, not by that entity.
    let reply = Iq::Error {
        from: None,
        to: iq.from().cloned(),
        id: iq.id().to_owned(),
        error: StanzaError::new(ErrorType::Cancel, condition, "en", text),
        payload: None,
    };
    vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
        reply,
    ))))]
}
