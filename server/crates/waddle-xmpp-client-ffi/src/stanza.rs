use jid::Jid;
use minidom::Element;
use waddle_xmpp_client::messaging::NS_CLIENT;

/// Wrap a JMI payload in a `<message to='...'/>` envelope.
/// The destination is taken as a typed [`Jid`] so the caller has
/// already validated the JID at the FFI entry point; the `to`
/// attribute is rendered from the typed value.
pub(crate) fn message_with_jmi(to: &Jid, jmi: Element) -> Element {
    Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.to_string())
        .append(jmi)
        .build()
}

/// Wrap a Jingle payload in an `<iq type='set' id='...' to='...'/>`
/// envelope. A v4 UUID id is minted so the correlator in
/// [`waddle_xmpp_client::ClientHandle::send_iq`] can route the result.
pub(crate) fn iq_set(to: &Jid, payload: Element) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            uuid::Uuid::new_v4().to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.to_string())
        .append(payload)
        .build()
}
