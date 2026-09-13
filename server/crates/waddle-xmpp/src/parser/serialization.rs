use minidom::Element;

use crate::XmppError;

/// Convert a minidom Element back to an XML string.
pub fn element_to_string(element: &Element) -> Result<String, XmppError> {
    let mut output = Vec::new();
    element
        .write_to(&mut output)
        .map_err(|e| XmppError::xml_parse(format!("Failed to serialize element: {}", e)))?;
    String::from_utf8(output).map_err(|e| XmppError::xml_parse(format!("Invalid UTF-8: {}", e)))
}

/// Convert an xmpp_parsers type to XML string via minidom.
pub fn stanza_to_string<T: Into<Element>>(stanza: T) -> Result<String, XmppError> {
    let element: Element = stanza.into();
    element_to_string(&element)
}

/// Convert a `Message` to an XML string, preserving the RFC 6121 `<thread/>`
/// element which `xmpp_parsers 0.21`'s `From<Message> for Element`
/// incorrectly drops.
pub fn message_to_string(msg: &xmpp_parsers::message::Message) -> Result<String, XmppError> {
    let thread_id = msg.thread.as_ref().map(|t| t.id.as_str());
    let mut element: Element = msg.clone().into();

    waddle_xmpp_core::parser_utils::ensure_thread_element(&mut element, thread_id);

    element_to_string(&element)
}

/// Decode a stored message without losing its RFC 6121 thread parent.
/// Shared by canonical envelopes and frozen intent payloads.
pub fn message_from_string(text: &str) -> Result<xmpp_parsers::message::Message, XmppError> {
    let element: Element = text
        .parse()
        .map_err(|_| XmppError::xml_parse("invalid stored message XML"))?;
    let stanza_ns = element.ns().to_string();
    let thread_parent = waddle_xmpp_core::parser_utils::extract_thread_parent(&element);
    let mut message = xmpp_parsers::message::Message::try_from(element)
        .map_err(|_| XmppError::xml_parse("invalid stored message stanza"))?;
    if let Some(parent) = thread_parent {
        waddle_xmpp_core::parser_utils::reattach_thread_parent(&mut message, parent, &stanza_ns);
    }
    Ok(message)
}
