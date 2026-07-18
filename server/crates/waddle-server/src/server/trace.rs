use std::time::Duration;

use axum::{
    body::Body,
    extract::MatchedPath,
    http::{Request, Response, Uri},
    middleware::Next,
    response::Response as AxumResponse,
};
use opentelemetry::trace::TraceContextExt;
use opentelemetry_http::HeaderExtractor;
use tower_http::trace::{DefaultOnResponse, OnResponse};
use tracing::{field, info_span, Level, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use waddle_xmpp::telemetry::attributes::{HttpRouteTemplate, HttpStatusClass};

const HTTP_ROUTE_TEMPLATES: &[&str] = &[
    "/health",
    "/healthz",
    "/ready",
    "/readyz",
    "/metrics",
    "/api/v1/health",
    "/.well-known/acme-challenge/{challenge_token}",
    "/api/auth/providers",
    "/api/auth/start",
    "/api/auth/callback",
    "/api/auth/session",
    "/api/auth/logout",
    "/api/auth/device/start",
    "/api/auth/device/poll",
    "/api/auth/device/verify",
    "/.well-known/oauth-authorization-server",
    "/api/auth/xmpp/authorize",
    "/api/auth/xmpp/token",
    "/auth",
    "/auth/start",
    "/auth/callback",
    "/webhooks/providers/{provider_id}/{plugin_id}",
    "/api/v1/livekit/webhook",
    "/api/calendar/community-feed-url",
    "/api/calendar/community/{token}/events.ics",
    "/api/test/profile-publish",
    "/debug/state-inventory",
    "/ws",
    "/.well-known/host-meta",
    "/.well-known/host-meta.json",
    "/api/upload/{slot_id}",
    "/api/files/{slot_id}/{filename}",
];

/// Build the per-request `tracing` span and attach the inbound W3C
/// trace context (if any) as its OpenTelemetry parent.
///
/// `opentelemetry_http::HeaderExtractor` implements the `Extractor`
/// trait the propagator expects. After extraction, we only call
/// `set_parent` when the extracted span context is valid
/// (`parent_cx.span().span_context().is_valid()`), which
/// distinguishes a propagated request from an internal / non-browser
/// caller carrying no headers — the latter keeps starting a fresh
/// root span instead of being silently re-parented to whatever the
/// extractor returns for the empty case.
pub(crate) fn make_request_span(request: &Request<Body>) -> Span {
    let span = info_span!(
        "http_request",
        method = %request.method(),
        uri = %redacted_request_uri(request.uri()),
        version = ?request.version(),
        http.route = field::Empty,
        http.status_code = field::Empty,
    );
    if let Some(route) = matched_route_template(request) {
        span.record("http.route", route);
    }
    let parent_cx = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });
    if parent_cx.span().span_context().is_valid() {
        let _ = span.set_parent(parent_cx);
    }
    span
}

/// Carry the bounded route label from the matched request into the response,
/// where tower-http supplies the final status and elapsed request time.
pub(crate) async fn attach_http_route_template(request: Request<Body>, next: Next) -> AxumResponse {
    let route = matched_route_template(&request).map(HttpRouteTemplate::new);
    let mut response = next.run(request).await;
    if let Some(route) = route {
        response.extensions_mut().insert(route);
    }
    response
}

/// Add response-only span fields and record the request-duration sample.
pub(crate) fn observe_http_response<B>(response: &Response<B>, latency: Duration, span: &Span) {
    let status = response.status().as_u16();
    span.record("http.status_code", i64::from(status));

    if let Some(route) = response.extensions().get::<HttpRouteTemplate>() {
        waddle_xmpp::histogram_record!(
            "http.server.request.duration",
            "s",
            "HTTP server request duration.",
            latency.as_secs_f64(),
            *route,
            HttpStatusClass::from_status(status),
        );
    }

    DefaultOnResponse::new()
        .level(Level::INFO)
        .on_response(response, latency, span);
}

fn matched_route_template(request: &Request<Body>) -> Option<&'static str> {
    let matched = request.extensions().get::<MatchedPath>()?.as_str();
    HTTP_ROUTE_TEMPLATES
        .iter()
        .copied()
        .find(|template| *template == matched)
}

/// Parse a W3C `traceparent` value carried as a WebSocket-upgrade
/// **query parameter** (#1326 phase A). The browser `WebSocket` API
/// cannot set headers, so the chat client appends
/// `?traceparent=00-<trace-id>-<span-id>-<flags>` to the upgrade URL;
/// the resulting remote `SpanContext` becomes an OTel span **link**
/// on the connection-scoped span (links, not parenting — the
/// connection outlives the browser's connect span).
///
/// Returns `None` for absent, malformed, or all-zero ids — a garbage
/// value from a client must never poison the server span.
pub(crate) fn client_trace_context_from_query(
    query: Option<&str>,
) -> Option<opentelemetry::trace::SpanContext> {
    use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};

    let traceparent = query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("traceparent="))?;
    let mut parts = traceparent.split('-');
    let version = parts.next()?;
    let trace_id = TraceId::from_hex(parts.next()?).ok()?;
    let span_id = SpanId::from_hex(parts.next()?).ok()?;
    let flags_field = parts.next()?;
    if flags_field.len() != 2 {
        return None;
    }
    let flags = u8::from_str_radix(flags_field, 16).ok()?;
    // W3C trace-context: the version is exactly two lowercase hex
    // digits and 0xff is forbidden; version 00 has exactly four
    // fields (higher versions may append more, which we accept after
    // a parseable 00-shaped prefix).
    if version.len() != 2
        || !version
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || version == "ff"
    {
        return None;
    }
    if version == "00" && parts.next().is_some() {
        return None;
    }
    if trace_id == TraceId::INVALID || span_id == SpanId::INVALID {
        return None;
    }
    Some(SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::new(flags),
        true,
        TraceState::default(),
    ))
}

fn redacted_request_uri(uri: &Uri) -> String {
    let path = uri.path();
    let Some(token_and_suffix) = path.strip_prefix("/api/calendar/community/") else {
        return uri.to_string();
    };
    let Some(token) = token_and_suffix.strip_suffix("/events.ics") else {
        return uri.to_string();
    };
    if token.is_empty() || token.contains('/') {
        return uri.to_string();
    }
    "/api/calendar/community/:token/events.ics".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_feed_token_is_redacted_from_request_uri() {
        let uri: Uri = "/api/calendar/community/v1.payload.signature/events.ics"
            .parse()
            .unwrap();

        assert_eq!(
            redacted_request_uri(&uri),
            "/api/calendar/community/:token/events.ics",
        );
    }

    #[test]
    fn traceparent_query_parses_to_remote_span_context() {
        let context = client_trace_context_from_query(Some(
            "protocol=xmpp&traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        ))
        .expect("valid traceparent must parse");
        assert_eq!(
            context.trace_id().to_string(),
            "0af7651916cd43dd8448eb211c80319c"
        );
        assert_eq!(context.span_id().to_string(), "b7ad6b7169203331");
        assert!(context.is_remote());
        assert!(context.is_sampled());
    }

    #[test]
    fn garbage_or_zero_traceparent_is_rejected() {
        for query in [
            None,
            Some(""),
            Some("traceparent="),
            Some("traceparent=nonsense"),
            Some("traceparent=00-00000000000000000000000000000000-b7ad6b7169203331-01"),
            Some("traceparent=00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01"),
            Some("traceparent=ff-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
            // Non-hex / uppercase version bytes are rejected outright.
            Some("traceparent=zz-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
            Some("traceparent=0F-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
            // Version 00 must have exactly four fields.
            Some("traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-extra"),
            // Flags must be exactly two hex digits.
            Some("traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-0"),
        ] {
            assert!(
                client_trace_context_from_query(query).is_none(),
                "must reject {query:?}"
            );
        }
    }

    #[test]
    fn non_calendar_feed_uri_is_not_redacted() {
        let uri: Uri = "/api/auth/session?session_id=still-owned-by-telemetry-redactor"
            .parse()
            .unwrap();

        assert_eq!(
            redacted_request_uri(&uri),
            "/api/auth/session?session_id=still-owned-by-telemetry-redactor",
        );
    }
}
