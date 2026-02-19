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
    let authority = extract_authority(&state.base_url);
    let websocket_scheme = extract_websocket_scheme(&state.base_url);
    let websocket_url = format!("{}://{}/xmpp-websocket", websocket_scheme, authority);

    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<XRD xmlns="http://docs.oasis-open.org/ns/xri/xrd-1.0">
  <Link rel="{}" href="{}" />
</XRD>"#,
        XMPP_ALT_CONNECTIONS_WEBSOCKET_REL,
        websocket_url
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
    let authority = extract_authority(&state.base_url);
    let websocket_scheme = extract_websocket_scheme(&state.base_url);
    let websocket_url = format!("{}://{}/xmpp-websocket", websocket_scheme, authority);

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

/// Extract host[:port] authority from base URL.
fn extract_authority(base_url: &str) -> String {
    let parsed = match url::Url::parse(base_url) {
        Ok(parsed) => parsed,
        Err(_) => return "localhost".to_string(),
    };

    let Some(host) = parsed.host_str() else {
        return "localhost".to_string();
    };

    match parsed.port() {
        Some(port) => format!("{}:{}", host, port),
        None => host.to_string(),
    }
}

/// Map HTTP URL scheme to the matching WebSocket scheme.
fn extract_websocket_scheme(base_url: &str) -> &'static str {
    match url::Url::parse(base_url)
        .ok()
        .map(|u| u.scheme().to_string())
        .as_deref()
    {
        Some("https") => "wss",
        _ => "ws",
    }
}

#[cfg(test)]
mod xep0156_host_meta_tests {
    use super::{
        extract_authority, extract_websocket_scheme, XMPP_ALT_CONNECTIONS_WEBSOCKET_REL,
    };

    #[test]
    fn xep0156_positive_https_maps_to_wss_with_authority() {
        let authority = extract_authority("https://chat.example:8443");
        let scheme = extract_websocket_scheme("https://chat.example:8443");

        assert_eq!(authority, "chat.example:8443");
        assert_eq!(scheme, "wss");
    }

    #[test]
    fn xep0156_negative_invalid_base_url_falls_back_to_localhost_ws() {
        let authority = extract_authority("not a url");
        let scheme = extract_websocket_scheme("not a url");

        assert_eq!(authority, "localhost");
        assert_eq!(scheme, "ws");
    }

    #[test]
    fn xep0156_consistency_uses_single_rel_identifier_constant() {
        assert_eq!(
            XMPP_ALT_CONNECTIONS_WEBSOCKET_REL,
            "urn:xmpp:alt-connections:websocket"
        );
    }
}
