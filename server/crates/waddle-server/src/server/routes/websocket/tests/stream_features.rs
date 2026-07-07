use super::*;

#[tokio::test]
async fn xep0237_features_advertise_roster_versioning() {
    let features = build_stream_features_xml(true, false);
    let el = Element::from_str(&features).expect("features xml");
    assert!(
        el.children()
            .any(|child| { child.name() == "ver" && child.ns() == waddle_xmpp::ns::ROSTERVER }),
        "post-auth features must advertise urn:xmpp:features:rosterver"
    );
}

#[tokio::test]
async fn rfc6121_features_advertise_subscription_preapproval() {
    let features = build_stream_features_xml(true, false);
    let el = Element::from_str(&features).expect("features xml");
    assert!(
        el.children().any(|child| {
            child.name() == "sub" && child.ns() == "urn:xmpp:features:pre-approval"
        }),
        "post-auth features must advertise RFC 6121 subscription pre-approval"
    );
}

/// ADR-0017 Phase 3 Slice 8 (Q8): ISR is advertised ONLY when the caller
/// says it's available (`clustering.enabled && Postgres`, in production —
/// see `ClusteringHandles::isr_token_store`'s doc comment); a test asserts
/// both directions so the gate can't silently regress to "always on" or
/// "always off".
#[tokio::test]
async fn isr_feature_present_when_available() {
    let features = build_stream_features_xml(true, true);
    let el = Element::from_str(&features).expect("features xml");
    let isr = el
        .children()
        .find(|child| child.name() == "isr" && child.ns() == waddle_xmpp::isr::ISR_NS)
        .expect("isr feature must be present when available");
    let mechanisms = isr
        .get_child("mechanisms", waddle_xmpp::ns::SASL)
        .expect("isr feature must list eligible mechanisms");
    assert!(mechanisms
        .children()
        .any(|m| m.name() == "mechanism" && m.text() == waddle_xmpp::isr::ISR_PINNED_MECHANISM));
}

#[tokio::test]
async fn isr_feature_absent_when_unavailable() {
    let features = build_stream_features_xml(true, false);
    let el = Element::from_str(&features).expect("features xml");
    assert!(
        !el.children()
            .any(|child| child.name() == "isr" && child.ns() == waddle_xmpp::isr::ISR_NS),
        "isr feature must be absent when clustering.enabled && Postgres is not satisfied"
    );
}

#[tokio::test]
async fn isr_feature_absent_when_unauthenticated_even_if_available() {
    // The pre-auth mechanisms list is a separate concern (SASL1 login
    // mechanisms); ISR's own feature only ever appears alongside <bind/>/
    // <sm/> in the post-auth feature set, matching XEP-0397's own example.
    let features = build_stream_features_xml(false, true);
    let el = Element::from_str(&features).expect("features xml");
    assert!(!el
        .children()
        .any(|child| child.name() == "isr" && child.ns() == waddle_xmpp::isr::ISR_NS));
}

#[tokio::test]
async fn features_do_not_advertise_legacy_isr_namespace() {
    // Issue #1169 / ADR-0017 Phase 3 Slice 8: the old, non-conformant
    // `urn:xmpp:isr:0` IQ-token scheme was retired. Pin that it can never
    // reappear, in either the unavailable or available (new-ISR-enabled)
    // gate state.
    for isr_available in [false, true] {
        let features = build_stream_features_xml(true, isr_available);
        let el = Element::from_str(&features).expect("features xml");
        assert!(
            !el.children().any(|child| child.ns() == "urn:xmpp:isr:0"),
            "stream features must not advertise legacy urn:xmpp:isr:0"
        );
    }
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
