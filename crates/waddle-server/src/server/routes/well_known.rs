//! Well-known endpoints for service discovery
//!
//! Implements:
//! - /.well-known/host-meta (XEP-0156) - XMPP connection discovery
//! - /.well-known/host-meta.json - JSON variant

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::sync::Arc;

use super::auth::AuthState;

/// Create the well-known router
pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route("/.well-known/host-meta", get(host_meta_xml_handler))
        .route("/.well-known/host-meta.json", get(host_meta_json_handler))
        .with_state(auth_state)
}

/// GET /.well-known/host-meta
///
/// Returns XRD document for XMPP service discovery (XEP-0156).
/// Used by XMPP clients to discover WebSocket/BOSH endpoints.
async fn host_meta_xml_handler(State(state): State<Arc<AuthState>>) -> Response {
    let domain = extract_domain(&state.base_url);
    let websocket_url = format!("wss://{}/xmpp-websocket", domain);

    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<XRD xmlns="http://docs.oasis-open.org/ns/xri/xrd-1.0">
  <Link rel="urn:xmpp:alt-connections:websocket" href="{}" />
</XRD>"#,
        websocket_url
    );

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xrd+xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

/// GET /.well-known/host-meta.json
///
/// Returns JSON variant of host-meta for XMPP service discovery.
async fn host_meta_json_handler(State(state): State<Arc<AuthState>>) -> Response {
    let domain = extract_domain(&state.base_url);
    let websocket_url = format!("wss://{}/xmpp-websocket", domain);

    let json = serde_json::json!({
        "links": [
            {
                "rel": "urn:xmpp:alt-connections:websocket",
                "href": websocket_url
            }
        ]
    });

    (StatusCode::OK, axum::Json(json)).into_response()
}

/// Extract domain from base URL
fn extract_domain(base_url: &str) -> String {
    url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "localhost".to_string())
}
