//! XEP-0280: the client asks for Message Carbons.
//!
//! §5 "Enabling Carbons": a client sends an IQ-set to its own account
//! (no `to`) with a bare `<enable xmlns='urn:xmpp:carbons:2'/>`. When the
//! runtime sends it, once after each fresh bind and not after a stream
//! resumption, is pinned by the runtime's own tests, which can drive a
//! session to its bind.

use waddle_xmpp_client::carbons::{build_carbons_enable_iq, NS_CARBONS};

#[test]
fn xep0280_enable_is_the_iq_of_section_5() {
    let iq = build_carbons_enable_iq("carbons-7");

    assert!(iq.is("iq", "jabber:client"));
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("id"), Some("carbons-7"));
    assert_eq!(
        iq.attr("to"),
        None,
        "XEP-0280 §5: sent to the client's own account, so with no 'to'"
    );
    assert_eq!(iq.attr("from"), None, "the server stamps the sender");

    let payload: Vec<_> = iq.children().collect();
    assert_eq!(payload.len(), 1, "an IQ-set carries exactly one child");
    assert!(payload[0].is("enable", NS_CARBONS));
    assert_eq!(payload[0].attrs().into_iter().count(), 0);
    assert_eq!(payload[0].children().count(), 0);
    assert_eq!(payload[0].text(), "");
}

#[test]
fn xep0280_namespace_is_version_2() {
    assert_eq!(NS_CARBONS, "urn:xmpp:carbons:2");
}
