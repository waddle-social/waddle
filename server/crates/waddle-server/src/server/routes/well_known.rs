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
    let xml = host_meta_xml(&state.public_xmpp_websocket_url);

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

fn host_meta_xml(websocket_url: &url::Url) -> String {
    const XRD_NS: &str = "http://docs.oasis-open.org/ns/xri/xrd-1.0";
    let xrd = xmpp_parsers::minidom::Element::builder("XRD", XRD_NS)
        .append(
            xmpp_parsers::minidom::Element::builder("Link", XRD_NS)
                .attr(
                    xmpp_parsers::minidom::rxml::xml_ncname!("rel").to_owned(),
                    XMPP_ALT_CONNECTIONS_WEBSOCKET_REL,
                )
                .attr(
                    xmpp_parsers::minidom::rxml::xml_ncname!("href").to_owned(),
                    websocket_url.as_str(),
                )
                .build(),
        )
        .build();
    crate::server::routes::websocket::element_to_xml(xrd)
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
    use super::{host_meta_xml, XMPP_ALT_CONNECTIONS_WEBSOCKET_REL};
    use std::str::FromStr;

    #[test]
    fn xep0156_consistency_uses_single_rel_identifier_constant() {
        assert_eq!(
            XMPP_ALT_CONNECTIONS_WEBSOCKET_REL,
            "urn:xmpp:alt-connections:websocket"
        );
    }

    #[test]
    fn host_meta_serialization_escapes_url_attributes() {
        let url =
            url::Url::parse("wss://xmpp.example.com/ws?one=1&two=2").expect("test WebSocket URL");
        let xml = host_meta_xml(&url);
        let xrd = xmpp_parsers::minidom::Element::from_str(&xml).expect("valid XRD XML");
        let link = xrd
            .get_child("Link", "http://docs.oasis-open.org/ns/xri/xrd-1.0")
            .expect("Link child");
        assert_eq!(link.attr("href"), Some(url.as_str()));
        assert!(xml.contains("&amp;"));
    }
}
