use axum::http::Uri;
use opentelemetry::trace::TraceContextExt;
use opentelemetry_http::HeaderExtractor;
use tracing::{info_span, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

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
pub(crate) fn make_request_span(request: &axum::http::Request<axum::body::Body>) -> Span {
    let span = info_span!(
        "http_request",
        method = %request.method(),
        uri = %redacted_request_uri(request.uri()),
        version = ?request.version(),
    );
    let parent_cx = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });
    if parent_cx.span().span_context().is_valid() {
        let _ = span.set_parent(parent_cx);
    }
    span
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
