use jid::BareJid;
use minidom::Element;
use std::str::FromStr;

use crate::bootstrap::NS_CLIENT;
use crate::error::{ClientError, ClientResult};
use crate::state::StreamId;

use super::{StreamClose, StreamOpen, TransportMessage};

const NS_XMPP_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
const MAX_FRAME_SIZE: usize = 1024 * 1024;

/// Encode a typed transport message as one RFC 7395 WebSocket text frame.
///
/// Browser integrations can use this with a JavaScript-owned `WebSocket`
/// while keeping XMPP stream state in [`crate::runtime::XmppRuntime`].
pub fn encode_message(message: &TransportMessage) -> ClientResult<String> {
    let encoded = match message {
        TransportMessage::Open(open) => serialize_element(&stream_open_element(open))?,
        TransportMessage::Element(element) => serialize_element(element)?,
        TransportMessage::Close(_) => {
            serialize_element(&Element::builder("close", NS_XMPP_FRAMING).build())?
        }
    };

    if encoded.len() > MAX_FRAME_SIZE {
        return Err(ClientError::TransportFrameTooLarge {
            max: MAX_FRAME_SIZE,
        });
    }

    Ok(encoded)
}

fn serialize_element(element: &Element) -> ClientResult<String> {
    let mut buffer = Vec::new();
    element
        .write_to(&mut buffer)
        .map_err(|_| ClientError::InvalidTransportFrame)?;
    String::from_utf8(buffer).map_err(|_| ClientError::InvalidTransportFrame)
}

fn stream_open_element(open: &StreamOpen) -> Element {
    let mut builder = Element::builder("open", NS_XMPP_FRAMING).attr(
        minidom::rxml::xml_ncname!("version").to_owned(),
        open.version,
    );
    if let Some(to) = &open.to {
        builder = builder.attr(minidom::rxml::xml_ncname!("to").to_owned(), to.to_string());
    }
    if let Some(from) = &open.from {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            from.to_string(),
        );
    }
    if let Some(id) = &open.id {
        builder = builder.attr(minidom::rxml::xml_ncname!("id").to_owned(), id.as_str());
    }
    if let Some(language) = &open.language {
        builder = builder.attr_ns(
            minidom::rxml::Namespace::XML,
            minidom::rxml::xml_ncname!("lang").to_owned(),
            language,
        );
    }
    builder.build()
}

/// Decode one RFC 7395 WebSocket text frame into a typed transport message.
///
/// Incoming bare client stanzas without an explicit default namespace are
/// normalised to `jabber:client`, matching the native transport.
pub fn decode_message(frame: &str) -> ClientResult<TransportMessage> {
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return Err(ClientError::EmptyTransportFrame);
    }
    if trimmed.len() > MAX_FRAME_SIZE {
        return Err(ClientError::TransportFrameTooLarge {
            max: MAX_FRAME_SIZE,
        });
    }
    if !has_single_top_level_element(trimmed) {
        return Err(ClientError::InvalidTransportFrame);
    }

    match peek_root_name(trimmed) {
        Some("open") => parse_stream_open(trimmed),
        Some("close") => parse_stream_close(trimmed),
        Some("iq" | "message" | "presence") => {
            parse_element_frame(&inject_client_ns_if_missing(trimmed))
        }
        Some(_) => parse_element_frame(trimmed),
        None => Err(ClientError::InvalidTransportFrame),
    }
}

fn parse_stream_open(frame: &str) -> ClientResult<TransportMessage> {
    let element = Element::from_str(frame).map_err(|_| ClientError::InvalidTransportFrame)?;
    if element.name() != "open" || element.ns() != NS_XMPP_FRAMING {
        return Err(ClientError::InvalidTransportFrame);
    }

    let version = element.attr("version").unwrap_or(StreamOpen::VERSION);
    if version != StreamOpen::VERSION {
        return Err(ClientError::UnsupportedStreamVersion {
            version: version.to_string(),
        });
    }

    let to = element
        .attr("to")
        .map(BareJid::from_str)
        .transpose()
        .map_err(|_| ClientError::InvalidStreamOpenTo)?;
    let from = element
        .attr("from")
        .map(BareJid::from_str)
        .transpose()
        .map_err(|_| ClientError::InvalidStreamOpenFrom)?;

    Ok(TransportMessage::Open(StreamOpen {
        to,
        from,
        id: element.attr("id").map(StreamId::new),
        // minidom 0.18 keys attrs by (Namespace, NcName); xml:lang lives
        // in the XML namespace, not the default one.
        language: element
            .attr_ns(&minidom::rxml::Namespace::XML, "lang")
            .map(str::to_string),
        version: StreamOpen::VERSION,
    }))
}

fn parse_stream_close(frame: &str) -> ClientResult<TransportMessage> {
    let element = Element::from_str(frame).map_err(|_| ClientError::InvalidTransportFrame)?;
    if element.name() != "close" || element.ns() != NS_XMPP_FRAMING {
        return Err(ClientError::InvalidTransportFrame);
    }

    Ok(TransportMessage::Close(StreamClose))
}

fn parse_element_frame(frame: &str) -> ClientResult<TransportMessage> {
    Element::from_str(frame)
        .map(TransportMessage::Element)
        .map_err(|_| ClientError::InvalidTransportFrame)
}

fn has_single_top_level_element(xml: &str) -> bool {
    let Some(end) = top_level_element_end(xml) else {
        return false;
    };
    xml[end..].trim().is_empty()
}

fn top_level_element_end(xml: &str) -> Option<usize> {
    let bytes = xml.as_bytes();
    if bytes.first().copied()? != b'<' || matches!(bytes.get(1), Some(b'/' | b'!' | b'?')) {
        return None;
    }

    let mut idx = 0;
    let mut depth = 0usize;
    while idx < bytes.len() {
        if bytes[idx] != b'<' {
            idx += 1;
            continue;
        }

        if xml[idx..].starts_with("<!--") {
            idx += xml[idx..].find("-->")? + "-->".len();
            continue;
        }
        if xml[idx..].starts_with("<![CDATA[") {
            idx += xml[idx..].find("]]>")? + "]]>".len();
            continue;
        }
        if xml[idx..].starts_with("<?") {
            idx += xml[idx..].find("?>")? + "?>".len();
            continue;
        }

        let end = find_tag_end(xml, idx)?;
        if xml[idx..].starts_with("</") {
            depth = depth.checked_sub(1)?;
            idx = end + 1;
            if depth == 0 {
                return Some(idx);
            }
            continue;
        }

        depth += 1;
        let mut before_end = end;
        while before_end > idx && bytes[before_end - 1].is_ascii_whitespace() {
            before_end -= 1;
        }
        if before_end > idx && bytes[before_end - 1] == b'/' {
            depth = depth.checked_sub(1)?;
            idx = end + 1;
            if depth == 0 {
                return Some(idx);
            }
            continue;
        }

        idx = end + 1;
    }

    None
}

fn find_tag_end(xml: &str, start: usize) -> Option<usize> {
    let bytes = xml.as_bytes();
    let mut quote = None;
    let mut idx = start + 1;
    while idx < bytes.len() {
        let byte = bytes[idx];
        match quote {
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => return Some(idx),
            None => {}
        }
        idx += 1;
    }
    None
}

fn peek_root_name(xml: &str) -> Option<&str> {
    let rest = xml.strip_prefix('<')?;
    if rest.starts_with('?') || rest.starts_with('!') {
        return None;
    }

    let name_end = rest
        .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    if name_end == 0 {
        return None;
    }

    Some(&rest[..name_end])
}

fn inject_client_ns_if_missing(xml: &str) -> String {
    let trimmed = xml.trim();
    let Some(scan) = scan_start_tag(trimmed) else {
        return trimmed.to_string();
    };
    if scan.has_default_xmlns {
        return trimmed.to_string();
    }

    let mut patched = String::with_capacity(trimmed.len() + NS_CLIENT.len() + 9);
    patched.push_str(&trimmed[..scan.name_end]);
    patched.push_str(r#" xmlns=""#);
    patched.push_str(NS_CLIENT);
    patched.push('"');
    patched.push_str(&trimmed[scan.name_end..]);
    patched
}

#[derive(Debug, Clone, Copy)]
struct StartTagScan {
    name_end: usize,
    has_default_xmlns: bool,
}

fn scan_start_tag(xml: &str) -> Option<StartTagScan> {
    let bytes = xml.as_bytes();
    if bytes.first().copied()? != b'<' {
        return None;
    }

    let mut idx = 1;
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    if idx >= bytes.len() || matches!(bytes[idx], b'/' | b'!' | b'?') {
        return None;
    }

    let name_start = idx;
    while idx < bytes.len()
        && !bytes[idx].is_ascii_whitespace()
        && !matches!(bytes[idx], b'/' | b'>')
    {
        idx += 1;
    }
    if idx == name_start {
        return None;
    }
    let name_end = idx;

    let mut quote = None;
    let mut has_default_xmlns = false;
    while idx < bytes.len() {
        let byte = bytes[idx];
        match quote {
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => break,
            None if byte == b'x' && xml[idx..].starts_with("xmlns") => {
                idx += "xmlns".len();
                if idx < bytes.len() && bytes[idx] == b'=' {
                    idx += 1;
                    if idx < bytes.len() && (bytes[idx] == b'"' || bytes[idx] == b'\'') {
                        let q = bytes[idx];
                        // Only count as a non-empty default namespace when the value
                        // between the quotes is non-empty (i.e. not xmlns="").
                        if idx + 1 < bytes.len() && bytes[idx + 1] != q {
                            has_default_xmlns = true;
                        }
                        quote = Some(q);
                    } else if idx < bytes.len() && bytes[idx] != b'>' {
                        // Unquoted, non-empty value.
                        has_default_xmlns = true;
                    }
                    continue;
                }
            }
            None => {}
        }
        idx += 1;
    }

    Some(StartTagScan {
        name_end,
        has_default_xmlns,
    })
}
