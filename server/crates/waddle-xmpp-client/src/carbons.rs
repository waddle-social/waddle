//! XEP-0280 Message Carbons, the client's side of it: asking the server
//! to copy this resource in on what the account's other resources send
//! and receive. Unwrapping the copies is in `runtime::app_stanza`.

use minidom::Element;
use xmpp_parsers::carbons::Enable;

use crate::NS_CLIENT;

/// XEP-0280 namespace: `<enable/>`, and the `<sent/>` and `<received/>` envelopes.
pub const NS_CARBONS: &str = "urn:xmpp:carbons:2";

/// XEP-0280 §5 Example 4: `<iq type='set'><enable xmlns='urn:xmpp:carbons:2'/></iq>`.
///
/// The server keeps the setting for the life of the session, across a
/// stream resumption but not across a fresh bind, so it is asked for once
/// after every bind.
#[must_use]
pub fn build_carbons_enable_iq(iq_id: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id)
        .append(Element::from(Enable))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_is_an_iq_set_with_the_carbons_enable_child() {
        let iq = build_carbons_enable_iq("carbons-1");
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.ns(), NS_CLIENT);
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("id"), Some("carbons-1"));
        assert_eq!(
            iq.attr("to"),
            None,
            "addressed to our own account, as the XEP has it"
        );
        let children: Vec<_> = iq.children().collect();
        assert_eq!(children.len(), 1);
        assert!(children[0].is("enable", NS_CARBONS));
        assert_eq!(children[0].children().count(), 0);
    }
}
