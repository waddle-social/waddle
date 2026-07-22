use jid::Jid;
use minidom::Element;
use waddle_xmpp_client::messaging::{wrap_jmi_message, NS_CLIENT};

/// Wrap a JMI payload in the XEP-0353 §3-conformant envelope: a
/// `<message type='chat'>` carrying a XEP-0334 `<store/>` hint before
/// the JMI body. Routes through the shared
/// [`waddle_xmpp_client::messaging::wrap_jmi_message`] so the native
/// and wasm clients ship byte-identical envelopes; the destination is
/// taken as a typed [`Jid`] the caller already validated at the FFI
/// entry point.
pub(crate) fn message_with_jmi(to: &Jid, jmi: Element) -> Element {
    wrap_jmi_message(to, jmi)
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
