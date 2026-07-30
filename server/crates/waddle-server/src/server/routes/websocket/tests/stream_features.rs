use super::*;

#[tokio::test]
async fn xep0237_features_advertise_roster_versioning() {
    let features = build_stream_features_xml(true);
    let el = Element::from_str(&features).expect("features xml");
    assert!(
        el.children()
            .any(|child| { child.name() == "ver" && child.ns() == waddle_xmpp::ns::ROSTERVER }),
        "post-auth features must advertise urn:xmpp:features:rosterver"
    );
}

#[tokio::test]
async fn rfc6121_features_advertise_subscription_preapproval() {
    let features = build_stream_features_xml(true);
    let el = Element::from_str(&features).expect("features xml");
    assert!(
        el.children().any(|child| {
            child.name() == "sub" && child.ns() == "urn:xmpp:features:pre-approval"
        }),
        "post-auth features must advertise RFC 6121 subscription pre-approval"
    );
}

#[tokio::test]
async fn features_do_not_advertise_isr() {
    let features = build_stream_features_xml(true);
    let el = Element::from_str(&features).expect("features xml");
    assert!(
        !el.children().any(|child| child.name() == "isr"),
        "stream features must not advertise a retired ISR extension"
    );
}

#[test]
fn session_init_failure_uses_standalone_stream_error_plus_websocket_close() {
    let stream_error = build_internal_server_error_stream_error(
        "Session initialization failed; please reconnect.",
    );
    let error = Element::from_str(&stream_error).expect("stream error xml");
    assert_eq!(error.name(), "error");
    assert_eq!(error.ns(), waddle_xmpp::ns::STREAM);
    assert!(
        !stream_error.contains("</stream:stream>"),
        "RFC 7395 close must be a separate websocket frame"
    );

    let close = Element::from_str(&websocket_stream_close_xml()).expect("close frame xml");
    assert_eq!(close.name(), "close");
    assert_eq!(close.ns(), "urn:ietf:params:xml:ns:xmpp-framing");
}
