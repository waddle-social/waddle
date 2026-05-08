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

const XMPP_ALT_CONNECTIONS_WEBSOCKET_REL: &str = "urn:xmpp:alt-connections:websocket";

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
    let websocket_url = state.websocket_url();

    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<XRD xmlns="http://docs.oasis-open.org/ns/xri/xrd-1.0">
  <Link rel="{}" href="{}" />
</XRD>"#,
        XMPP_ALT_CONNECTIONS_WEBSOCKET_REL, websocket_url
    );

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/xrd+xml; charset=utf-8"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        xml,
    )
        .into_response()
}

/// GET /.well-known/host-meta.json
///
/// Returns JSON variant of host-meta for XMPP service discovery.
async fn host_meta_json_handler(State(state): State<Arc<AuthState>>) -> Response {
    let websocket_url = state.websocket_url();

    let json = serde_json::json!({
        "links": [
            {
                "rel": XMPP_ALT_CONNECTIONS_WEBSOCKET_REL,
                "href": websocket_url
            }
        ]
    });

    (
        StatusCode::OK,
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        axum::Json(json),
    )
        .into_response()
}

#[cfg(test)]
mod xep0156_host_meta_tests {
    use super::XMPP_ALT_CONNECTIONS_WEBSOCKET_REL;

    #[test]
    fn xep0156_consistency_uses_single_rel_identifier_constant() {
        assert_eq!(
            XMPP_ALT_CONNECTIONS_WEBSOCKET_REL,
            "urn:xmpp:alt-connections:websocket"
        );
    }
}
