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
