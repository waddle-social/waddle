use axum::http::Uri;
use opentelemetry::trace::{
    SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
};
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

/// Parse a W3C `traceparent` value carried as a WebSocket-upgrade
/// **query parameter** (#1326 phase A). The browser `WebSocket` API
/// cannot set headers, so the chat client appends
/// `?traceparent=00-<trace-id>-<span-id>-<flags>` to the upgrade URL;
/// the resulting remote `SpanContext` becomes an OTel span **link**
/// on the connection-scoped span (links, not parenting — the
/// connection outlives the browser's connect span).
///
/// Strictly parsed W3C version-00 trace parent supplied by the browser.
/// Keeping this typed until the OpenTelemetry boundary prevents raw query
/// strings from flowing through connection state or tracing call sites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClientTraceParent {
    trace_id: TraceId,
    parent_id: SpanId,
    flags: TraceFlags,
}

impl ClientTraceParent {
    pub(crate) fn trace_id(self) -> TraceId {
        self.trace_id
    }

    pub(crate) fn remote_span_context(self) -> SpanContext {
        SpanContext::new(
            self.trace_id,
            self.parent_id,
            self.flags,
            true,
            TraceState::default(),
        )
    }
}

/// Returns `None` for absent, malformed, non-version-00, uppercase, short, or
/// all-zero ids. Invalid client input is deliberately ignored rather than
/// affecting the WebSocket upgrade response.
pub(crate) fn client_trace_parent_from_query(query: Option<&str>) -> Option<ClientTraceParent> {
    let traceparent = query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("traceparent="))?;
    let mut parts = traceparent.split('-');
    let version = parts.next()?;
    let trace_id_field = parts.next()?;
    let parent_id_field = parts.next()?;
    let flags_field = parts.next()?;
    if parts.next().is_some()
        || version != "00"
        || !is_lower_hex(trace_id_field, 32)
        || !is_lower_hex(parent_id_field, 16)
        || !is_lower_hex(flags_field, 2)
    {
        return None;
    }
    let trace_id = TraceId::from_hex(trace_id_field).ok()?;
    let parent_id = SpanId::from_hex(parent_id_field).ok()?;
    let flags = u8::from_str_radix(flags_field, 16).ok()?;
    if trace_id == TraceId::INVALID || parent_id == SpanId::INVALID {
        return None;
    }
    Some(ClientTraceParent {
        trace_id,
        parent_id,
        flags: TraceFlags::new(flags),
    })
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
    fn traceparent_query_parses_to_typed_remote_parent() {
        let parent = client_trace_parent_from_query(Some(
            "protocol=xmpp&traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        ))
        .expect("valid traceparent must parse");
        let context = parent.remote_span_context();
        assert_eq!(
            context.trace_id().to_string(),
            "0af7651916cd43dd8448eb211c80319c"
        );
        assert_eq!(context.span_id().to_string(), "b7ad6b7169203331");
        assert!(context.is_remote());
        assert!(context.is_sampled());
    }

    #[test]
    fn malformed_or_zero_traceparent_is_rejected() {
        for query in [
            None,
            Some(""),
            Some("traceparent="),
            Some("traceparent=nonsense"),
            Some("traceparent=00-00000000000000000000000000000000-b7ad6b7169203331-01"),
            Some("traceparent=00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01"),
            // Non-hex / uppercase bytes are rejected outright.
            Some("traceparent=zz-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
            Some("traceparent=0F-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
            Some("traceparent=00-0AF7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
            Some("traceparent=00-0af7651916cd43dd8448eb211c80319c-B7ad6b7169203331-01"),
            Some("traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-0A"),
            // Version 00 must have exactly four fields.
            Some("traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-extra"),
            // Flags must be exactly two hex digits.
            Some("traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-0"),
        ] {
            assert!(
                client_trace_parent_from_query(query).is_none(),
                "must reject {query:?}"
            );
        }
    }

    #[test]
    fn wrong_traceparent_version_is_rejected() {
        for version in ["01", "fe", "ff"] {
            let query = format!(
                "traceparent={version}-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
            );
            assert!(client_trace_parent_from_query(Some(&query)).is_none());
        }
    }

    #[test]
    fn short_traceparent_fields_are_rejected() {
        for query in [
            "traceparent=00-af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            "traceparent=00-0af7651916cd43dd8448eb211c80319c-7ad6b7169203331-01",
            "traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-1",
        ] {
            assert!(client_trace_parent_from_query(Some(query)).is_none());
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
