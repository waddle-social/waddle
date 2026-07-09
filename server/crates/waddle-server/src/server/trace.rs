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
    if path.starts_with("/api/upload/") {
        return "/api/upload/:slot".to_string();
    }
    if path.starts_with("/api/files/") {
        return "/api/files/:slot/:file".to_string();
    }
    let Some(token_and_suffix) = path.strip_prefix("/api/calendar/community/") else {
        return path.to_string();
    };
    let Some(token) = token_and_suffix.strip_suffix("/events.ics") else {
        return path.to_string();
    };
    if token.is_empty() || token.contains('/') {
        return path.to_string();
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
    fn request_query_is_never_exported_to_the_span() {
        let uri: Uri = "/api/auth/session?session_id=secret&code=oauth-code&state=csrf-state"
            .parse()
            .unwrap();

        assert_eq!(redacted_request_uri(&uri), "/api/auth/session");
    }

    #[test]
    fn calendar_feed_query_and_token_are_both_redacted() {
        let uri: Uri = "/api/calendar/community/v1.payload.signature/events.ics?download=1"
            .parse()
            .unwrap();

        assert_eq!(
            redacted_request_uri(&uri),
            "/api/calendar/community/:token/events.ics",
        );
    }

    #[test]
    fn upload_capability_and_filename_are_redacted() {
        let upload: Uri = "/api/upload/secret-slot?signature=secret".parse().unwrap();
        let download: Uri = "/api/files/secret-slot/private-name.txt?download=1"
            .parse()
            .unwrap();

        assert_eq!(redacted_request_uri(&upload), "/api/upload/:slot");
        assert_eq!(redacted_request_uri(&download), "/api/files/:slot/:file");
    }
}
