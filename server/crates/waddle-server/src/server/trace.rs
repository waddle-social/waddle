use std::time::Duration;

use axum::{
    body::Body,
    extract::MatchedPath,
    http::{Request, Response, Uri},
    middleware::Next,
    response::Response as AxumResponse,
};
use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
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

/// Routes whose request spans are pure kube-probe noise (#1438): the
/// liveness/readiness endpoints and the `/metrics` liveness stub
/// (#1426), together ~98% of production `http_request` spans. Requests
/// to these routes get a disabled span; the
/// `http.server.request.duration` histogram is unaffected because
/// `observe_http_response` reads the route from the response extension,
/// not the span.
const PROBE_ROUTE_TEMPLATES: &[&str] = &[
    "/health",
    "/healthz",
    "/ready",
    "/readyz",
    "/metrics",
    "/api/v1/health",
];

/// Decide whether a request deserves a `tracing` span at all (#1438).
///
/// - No axum `MatchedPath` means no route matched — hostile-scanner
///   404s (`/enhancecp`, `/azure/.env`, …) that would each mint a root
///   trace. Not traced.
/// - A matched probe route is kube-probe noise. Not traced.
/// - A `MatchedPath` that is missing from `HTTP_ROUTE_TEMPLATES` is a
///   real route someone forgot to allowlist: keep tracing it (with an
///   empty `http.route`) rather than silently untracing an endpoint.
fn should_trace_request(request: &Request<Body>) -> bool {
    if request.extensions().get::<MatchedPath>().is_none() {
        return false;
    }
    !matched_route_template(request)
        .is_some_and(|template| PROBE_ROUTE_TEMPLATES.contains(&template))
}

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
    if !should_trace_request(request) {
        return Span::none();
    }
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
            buckets: waddle_xmpp::telemetry::SECOND_SCALE_BUCKETS,
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
/// Strictly parsed W3C trace parent supplied by the browser. Version `00`
/// must have exactly four fields; future versions retain the version-00 field
/// lengths and may append opaque extension fields. Keeping this typed until
/// the OpenTelemetry boundary prevents raw query strings from flowing through
/// connection state or tracing call sites.
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

/// Returns `None` for absent, malformed, version `ff`, uppercase, short, or
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
    let has_extension = parts.next().is_some();
    if !is_lower_hex(version, 2)
        || version == "ff"
        || (version == "00" && has_extension)
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

/// The `uri` span field, with every credential-bearing part removed
/// (#1439).
///
/// Query strings are dropped for **all** routes, not allowlisted ones:
/// observed production spans carried `/api/auth/session?session_id=<live
/// session credential>` and `/ws?traceparent=…`, and any new endpoint
/// would leak by default under a per-route redaction list. `http.route`
/// already carries the bounded template, so the query adds no queryable
/// signal.
///
/// Path segments that are themselves credentials still need naming: the
/// calendar feed token sits in the path, so it collapses to the route
/// template shape.
fn redacted_request_uri(uri: &Uri) -> String {
    let path = uri.path();
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
    use std::{
        collections::BTreeMap,
        fmt,
        sync::{Arc, Mutex},
    };

    use axum::{routing::get, Router};
    use tower::ServiceExt;
    use tower_http::trace::TraceLayer;
    use tracing::{
        field::{Field, Visit},
        span::{Attributes, Id, Record},
        Subscriber,
    };
    use tracing_subscriber::{layer::Context, prelude::*, registry::LookupSpan, Layer};

    use super::*;

    #[derive(Clone, Default)]
    struct HttpSpanCapture {
        fields: Arc<Mutex<BTreeMap<String, String>>>,
    }

    impl HttpSpanCapture {
        fn record(&self, values: impl FnOnce(&mut HttpSpanVisitor<'_>)) {
            let mut fields = self.fields.lock().expect("HTTP span capture lock");
            values(&mut HttpSpanVisitor {
                fields: &mut fields,
            });
        }
    }

    struct HttpSpanVisitor<'a> {
        fields: &'a mut BTreeMap<String, String>,
    }

    impl Visit for HttpSpanVisitor<'_> {
        fn record_i64(&mut self, field: &Field, value: i64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.fields
                .insert(field.name().to_string(), format!("{value:?}"));
        }
    }

    impl<S> Layer<S> for HttpSpanCapture
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_new_span(&self, attributes: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
            if attributes.metadata().name() == "http_request" {
                self.record(|visitor| attributes.record(visitor));
            }
        }

        fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
            let is_http_request = ctx
                .span(id)
                .is_some_and(|span| span.metadata().name() == "http_request");
            if is_http_request {
                self.record(|visitor| values.record(visitor));
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_span_records_route_status_and_redacted_uri() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let capture = HttpSpanCapture::default();
        let _subscriber =
            tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
        let app = Router::new()
            .route(
                "/api/calendar/community/{token}/events.ics",
                get(|| async { axum::http::StatusCode::UNAUTHORIZED }),
            )
            .layer(axum::middleware::from_fn(attach_http_route_template))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(make_request_span)
                    .on_response(observe_http_response),
            );
        let sensitive_token = "sensitive-calendar-token";
        let raw_uri = format!("/api/calendar/community/{sensitive_token}/events.ics");

        let response = app
            .oneshot(
                Request::builder()
                    .uri(&raw_uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
        let fields = capture.fields.lock().expect("HTTP span capture lock");
        assert_eq!(
            fields.get("http.route").map(String::as_str),
            Some("/api/calendar/community/{token}/events.ics"),
        );
        assert_eq!(
            fields.get("http.status_code").map(String::as_str),
            Some("401"),
        );
        assert_eq!(
            fields.get("uri").map(String::as_str),
            Some("/api/calendar/community/:token/events.ics"),
        );
        assert!(
            fields
                .values()
                .all(|value| !value.contains(sensitive_token)),
            "request span must not expose the raw calendar token: {fields:?}",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn probe_routes_get_no_request_span_but_keep_the_duration_histogram() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let capture = HttpSpanCapture::default();
        let _subscriber =
            tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
        for probe in PROBE_ROUTE_TEMPLATES {
            let app = Router::new()
                .route(probe, get(|| async { "ok" }))
                .layer(axum::middleware::from_fn(attach_http_route_template))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(make_request_span)
                        .on_response(observe_http_response),
                );

            let response = app
                .oneshot(
                    Request::builder()
                        .uri(*probe)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let fields = capture.fields.lock().expect("HTTP span capture lock");
            assert!(
                fields.is_empty(),
                "{probe} must not create an http_request span: {fields:?}",
            );
            drop(fields);
            assert_eq!(
                metrics.histogram_count(
                    "http.server.request.duration",
                    &[("route", probe), ("status_class", "2xx")],
                ),
                Some(1),
                "suppressing the {probe} span must not suppress the duration histogram",
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_duration_histogram_uses_second_scale_buckets() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let app = Router::new()
            .route("/api/auth/session", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(attach_http_route_template))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(make_request_span)
                    .on_response(observe_http_response),
            );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/session")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        // The instrument records seconds, so its buckets must be
        // second-scale: the SDK's millisecond-scale defaults put every
        // sub-second request in the first bucket and pin p99 at ~4.95s
        // (#1453).
        assert_eq!(
            metrics.histogram_bounds("http.server.request.duration"),
            Some(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unrouted_requests_get_no_request_span() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let capture = HttpSpanCapture::default();
        let _subscriber =
            tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
        let app = Router::new()
            .route("/api/auth/session", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(attach_http_route_template))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(make_request_span)
                    .on_response(observe_http_response),
            );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/enhancecp")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
        let fields = capture.fields.lock().expect("HTTP span capture lock");
        assert!(
            fields.is_empty(),
            "scanner 404s must not create an http_request span: {fields:?}",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn matched_route_missing_from_the_allowlist_is_still_traced() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let capture = HttpSpanCapture::default();
        let _subscriber =
            tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
        let app = Router::new()
            .route("/not/in/the/allowlist", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(attach_http_route_template))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(make_request_span)
                    .on_response(observe_http_response),
            );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/not/in/the/allowlist")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let fields = capture.fields.lock().expect("HTTP span capture lock");
        assert_eq!(
            fields.get("uri").map(String::as_str),
            Some("/not/in/the/allowlist"),
            "a real route missing from HTTP_ROUTE_TEMPLATES must stay traced",
        );
    }

    #[test]
    fn probe_routes_are_a_subset_of_the_route_allowlist() {
        // should_trace_request suppresses via matched_route_template,
        // which only recognizes HTTP_ROUTE_TEMPLATES entries — a probe
        // route absent from the allowlist would silently stay traced.
        for probe in PROBE_ROUTE_TEMPLATES {
            assert!(
                HTTP_ROUTE_TEMPLATES.contains(probe),
                "{probe} must be listed in HTTP_ROUTE_TEMPLATES",
            );
        }
    }

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
    fn higher_traceparent_version_is_accepted() {
        assert!(client_trace_parent_from_query(Some(
            "traceparent=01-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
        ))
        .is_some());
    }

    #[test]
    fn traceparent_version_ff_is_rejected() {
        assert!(client_trace_parent_from_query(Some(
            "traceparent=ff-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
        ))
        .is_none());
    }

    #[test]
    fn version_zero_traceparent_with_extras_is_rejected() {
        assert!(client_trace_parent_from_query(Some(
            "traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-extra"
        ))
        .is_none());
    }

    #[test]
    fn higher_version_traceparent_with_extras_is_accepted() {
        for query in [
            "traceparent=02-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-vendor-field",
            "traceparent=02-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-",
            "traceparent=02-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01--opaque",
        ] {
            assert!(client_trace_parent_from_query(Some(query)).is_some());
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
    fn query_strings_are_stripped_from_every_route() {
        for (raw, expected) in [
            (
                "/api/auth/session?session_id=live-session-credential",
                "/api/auth/session",
            ),
            (
                "/ws?traceparent=00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
                "/ws",
            ),
            (
                "/api/auth/callback?code=secret&state=secret",
                "/api/auth/callback",
            ),
            ("/api/auth/session", "/api/auth/session"),
            (
                "/api/calendar/community/v1.payload.signature/events.ics?x=y",
                "/api/calendar/community/:token/events.ics",
            ),
        ] {
            let uri: Uri = raw.parse().expect("uri");
            assert_eq!(redacted_request_uri(&uri), expected, "for {raw}");
        }
    }

    /// The full pipeline: a request whose query carries a live session
    /// credential must export a `uri` span field with no query string.
    #[tokio::test(flavor = "current_thread")]
    async fn session_credentials_never_reach_the_request_span() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let capture = HttpSpanCapture::default();
        let _subscriber =
            tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
        let app = Router::new()
            .route("/api/auth/session", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(attach_http_route_template))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(make_request_span)
                    .on_response(observe_http_response),
            );
        let credential = "live-session-credential";

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/auth/session?session_id={credential}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let fields = capture.fields.lock().expect("HTTP span capture lock");
        assert_eq!(
            fields.get("uri").map(String::as_str),
            Some("/api/auth/session"),
        );
        assert!(
            fields.values().all(|value| !value.contains(credential)),
            "request span must not expose the session credential: {fields:?}",
        );
    }
}
