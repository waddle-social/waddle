//! Incremental XML parsing for XMPP streams using rxml and minidom.
//!
//! XMPP uses a single long-lived XML document per session, so we need
//! incremental parsing that can handle partial data and maintain state
//! across multiple read operations.

use minidom::Element;
use std::collections::VecDeque;

use crate::XmppError;

/// Namespace URIs used in XMPP
pub mod ns {
    /// XMPP client namespace
    pub const JABBER_CLIENT: &str = "jabber:client";
    /// XMPP server namespace
    pub const JABBER_SERVER: &str = "jabber:server";
    /// XMPP streams namespace
    pub const STREAM: &str = "http://etherx.jabber.org/streams";
    /// STARTTLS namespace
    pub const TLS: &str = "urn:ietf:params:xml:ns:xmpp-tls";
    /// SASL namespace
    pub const SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
    /// Resource binding namespace
    pub const BIND: &str = "urn:ietf:params:xml:ns:xmpp-bind";
    /// Session namespace
    pub const SESSION: &str = "urn:ietf:params:xml:ns:xmpp-session";
    /// Stanza error namespace
    pub const STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
}

/// Parsed stream header information.
#[derive(Debug, Clone, Default)]
pub struct StreamHeader {
    /// The 'to' attribute (target domain)
    pub to: Option<String>,
    /// The 'from' attribute (source domain)
    pub from: Option<String>,
    /// The 'id' attribute (stream ID, set by server)
    pub id: Option<String>,
    /// The 'version' attribute (should be "1.0")
    pub version: Option<String>,
    /// The 'xml:lang' attribute
    pub lang: Option<String>,
}

impl StreamHeader {
    /// Parse a stream header from raw XML data.
    ///
    /// This handles the special case of XMPP stream headers which are
    /// incomplete XML (the closing tag comes at session end).
    pub fn parse(data: &str) -> Result<Self, XmppError> {
        let mut header = StreamHeader::default();

        // Find the stream:stream opening tag
        let stream_start = data
            .find("<stream:stream")
            .or_else(|| data.find("<stream "))
            .ok_or_else(|| XmppError::xml_parse("No stream:stream element found"))?;

        let stream_end = data[stream_start..]
            .find('>')
            .map(|i| stream_start + i)
            .ok_or_else(|| XmppError::xml_parse("Incomplete stream header"))?;

        let tag = &data[stream_start..=stream_end];

        // Parse attributes manually since the tag is intentionally unclosed
        header.to = extract_attribute(tag, "to");
        header.from = extract_attribute(tag, "from");
        header.id = extract_attribute(tag, "id");
        header.version = extract_attribute(tag, "version");
        header.lang = extract_attribute(tag, "xml:lang");

        Ok(header)
    }

    /// Validate the stream header per RFC 6120.
    pub fn validate(&self) -> Result<(), XmppError> {
        // Version should be 1.0 for RFC 6120
        if let Some(ref version) = self.version {
            if version != "1.0" {
                return Err(XmppError::stream(format!(
                    "Unsupported XMPP version: {}",
                    version
                )));
            }
        }
        Ok(())
    }
}

/// Extract an attribute value from an XML tag string.
fn extract_attribute(tag: &str, name: &str) -> Option<String> {
    // Try both single and double quotes
    for quote in ['"', '\''] {
        let pattern = format!("{}={}", name, quote);
        if let Some(start) = tag.find(&pattern) {
            let value_start = start + pattern.len();
            if let Some(value_end) = tag[value_start..].find(quote) {
                return Some(tag[value_start..value_start + value_end].to_string());
            }
        }
    }
    None
}

/// Incremental XML parser for XMPP stanzas.
///
/// This parser accumulates data and emits complete XML elements (stanzas)
/// as they become available. It handles the XMPP stream framing where
/// the stream:stream element wraps all stanzas.
pub struct XmlParser {
    /// Accumulated data buffer
    buffer: Vec<u8>,
    /// Queue of parsed elements ready to be consumed
    elements: VecDeque<Element>,
    /// Whether we've seen the stream header
    stream_started: bool,
    /// Current parsing depth (for tracking element boundaries)
    depth: usize,
    /// Start position of current element
    element_start: Option<usize>,
}

impl XmlParser {
    /// Create a new XML parser.
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(8192),
            elements: VecDeque::new(),
            stream_started: false,
            depth: 0,
            element_start: None,
        }
    }

    /// Feed data into the parser.
    pub fn feed(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Check if we have a complete stream header in the buffer.
    pub fn has_stream_header(&self) -> bool {
        let s = String::from_utf8_lossy(&self.buffer);
        (s.contains("<stream:stream") || s.contains("<stream ")) && s.contains('>')
    }

    /// Extract and consume the stream header from the buffer.
    pub fn take_stream_header(&mut self) -> Result<StreamHeader, XmppError> {
        let data = String::from_utf8_lossy(&self.buffer).to_string();
        let header = StreamHeader::parse(&data)?;
        self.stream_started = true;
        // Don't clear buffer - there might be more data after the header
        Ok(header)
    }

    /// Check if there's a complete stanza in the buffer.
    ///
    /// Returns true if we have a complete top-level element after the stream header.
    pub fn has_complete_stanza(&self) -> bool {
        let data = String::from_utf8_lossy(&self.buffer);

        // Check for stream close
        if data.contains("</stream:stream>") {
            return true;
        }

        // Simple heuristic: look for matching opening and closing tags
        // for top-level stanzas (message, presence, iq, starttls, auth, etc.)
        let stanza_tags = [
            "message",
            "presence",
            "iq",
            "starttls",
            "proceed",
            "failure",
            "auth",
            "success",
            "stream:features",
        ];

        for tag in stanza_tags {
            if let Some(start) = data.find(&format!("<{}", tag)) {
                // Check for self-closing tag
                let after_tag = &data[start..];
                if let Some(end) = after_tag.find('>') {
                    if end > 0 && after_tag.as_bytes()[end - 1] == b'/' {
                        return true;
                    }
                }
                // Check for closing tag
                if data.contains(&format!("</{}", tag)) {
                    return true;
                }
            }
        }

        false
    }

    /// Parse and return the next complete stanza from the buffer.
    ///
    /// Returns None if no complete stanza is available.
    pub fn next_stanza(&mut self) -> Result<Option<ParsedStanza>, XmppError> {
        let data = String::from_utf8_lossy(&self.buffer).to_string();

        // Check for stream close
        if data.contains("</stream:stream>") {
            if let Some(pos) = data.find("</stream:stream>") {
                self.buffer = self.buffer[pos + 16..].to_vec();
            }
            return Ok(Some(ParsedStanza::StreamEnd));
        }

        // Try to parse each known stanza type
        let stanza_patterns: &[(&str, fn(&str) -> Result<ParsedStanza, XmppError>)] = &[
            ("<starttls", |d| parse_starttls(d)),
            ("<auth", |d| parse_auth(d)),
            ("<iq", |d| parse_iq_stanza(d)),
            ("<message", |d| parse_message_stanza(d)),
            ("<presence", |d| parse_presence_stanza(d)),
        ];

        for (pattern, parser) in stanza_patterns {
            if let Some(start) = data.find(pattern) {
                // Find the end of this stanza
                let tag_name = &pattern[1..]; // Strip leading <
                if let Some(end) = find_stanza_end(&data, start, tag_name) {
                    let stanza_xml = &data[start..end];
                    let result = parser(stanza_xml)?;

                    // Remove parsed data from buffer
                    self.buffer = data[end..].as_bytes().to_vec();
                    return Ok(Some(result));
                }
            }
        }

        Ok(None)
    }

    /// Clear the parser state and buffer.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.elements.clear();
        self.stream_started = false;
        self.depth = 0;
        self.element_start = None;
    }

    /// Get the current buffer contents as a string (for debugging).
    pub fn buffer_str(&self) -> String {
        String::from_utf8_lossy(&self.buffer).to_string()
    }
}

impl Default for XmlParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Find the end position of a stanza (after the closing tag or self-closing tag).
fn find_stanza_end(data: &str, start: usize, tag_name: &str) -> Option<usize> {
    let after_start = &data[start..];

    // Check for self-closing tag first
    if let Some(gt_pos) = after_start.find('>') {
        if gt_pos > 0 && after_start.as_bytes()[gt_pos - 1] == b'/' {
            return Some(start + gt_pos + 1);
        }
    }

    // Look for closing tag
    let close_tag = format!("</{}", tag_name);
    if let Some(close_start) = after_start.find(&close_tag) {
        // Find the > after the closing tag
        if let Some(close_end) = after_start[close_start..].find('>') {
            return Some(start + close_start + close_end + 1);
        }
    }

    None
}

/// Parsed stanza variants.
#[derive(Debug, Clone)]
pub enum ParsedStanza {
    /// STARTTLS request
    StartTls,
    /// SASL auth request with mechanism and base64 data
    SaslAuth { mechanism: String, data: String },
    /// Stream end
    StreamEnd,
    /// Message stanza
    Message(Element),
    /// Presence stanza
    Presence(Element),
    /// IQ stanza
    Iq(Element),
    /// Unknown/raw element
    Unknown(Element),
}

fn parse_starttls(data: &str) -> Result<ParsedStanza, XmppError> {
    if data.contains("starttls") {
        Ok(ParsedStanza::StartTls)
    } else {
        Err(XmppError::xml_parse("Invalid starttls element"))
    }
}

fn parse_auth(data: &str) -> Result<ParsedStanza, XmppError> {
    let mechanism = extract_attribute(data, "mechanism").unwrap_or_default();

    // Extract content between > and </auth>
    let content_start = data.find('>').map(|i| i + 1).unwrap_or(0);
    let content_end = data.find("</auth>").unwrap_or(data.len());
    let content = if content_start < content_end {
        data[content_start..content_end].trim().to_string()
    } else {
        String::new()
    };

    Ok(ParsedStanza::SaslAuth {
        mechanism,
        data: content,
    })
}

fn parse_iq_stanza(data: &str) -> Result<ParsedStanza, XmppError> {
    let element = parse_element(data)?;
    Ok(ParsedStanza::Iq(element))
}

fn parse_message_stanza(data: &str) -> Result<ParsedStanza, XmppError> {
    let element = parse_element(data)?;
    Ok(ParsedStanza::Message(element))
}

fn parse_presence_stanza(data: &str) -> Result<ParsedStanza, XmppError> {
    let element = parse_element(data)?;
    Ok(ParsedStanza::Presence(element))
}

/// Parse a string into a minidom Element.
fn parse_element(data: &str) -> Result<Element, XmppError> {
    data.parse::<Element>()
        .map_err(|e| XmppError::xml_parse(format!("Failed to parse element: {}", e)))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_header_parsing() {
        let header_xml = r#"<?xml version='1.0'?><stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' to='example.com' from='user@example.com' version='1.0'>"#;

        let header = StreamHeader::parse(header_xml).unwrap();

        assert_eq!(header.to, Some("example.com".to_string()));
        assert_eq!(header.from, Some("user@example.com".to_string()));
        assert_eq!(header.version, Some("1.0".to_string()));
    }

    #[test]
    fn test_stream_header_with_double_quotes() {
        let header_xml = r#"<stream:stream xmlns="jabber:client" to="localhost" version="1.0">"#;

        let header = StreamHeader::parse(header_xml).unwrap();

        assert_eq!(header.to, Some("localhost".to_string()));
        assert_eq!(header.version, Some("1.0".to_string()));
    }

    #[test]
    fn test_parser_starttls() {
        let mut parser = XmlParser::new();
        parser.feed(b"<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>");

        assert!(parser.has_complete_stanza());

        let stanza = parser.next_stanza().unwrap();
        assert!(matches!(stanza, Some(ParsedStanza::StartTls)));
    }

    #[test]
    fn test_parser_auth() {
        let mut parser = XmlParser::new();
        parser.feed(b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>AGFsaWNlAHNlY3JldA==</auth>");

        assert!(parser.has_complete_stanza());

        let stanza = parser.next_stanza().unwrap();
        if let Some(ParsedStanza::SaslAuth { mechanism, data }) = stanza {
            assert_eq!(mechanism, "PLAIN");
            assert_eq!(data, "AGFsaWNlAHNlY3JldA==");
        } else {
            panic!("Expected SaslAuth");
        }
    }

    #[test]
    fn test_parser_message() {
        let mut parser = XmlParser::new();
        // Include xmlns='jabber:client' as minidom requires namespace declarations
        parser.feed(b"<message xmlns='jabber:client' to='bob@example.com' type='chat'><body>Hello!</body></message>");

        assert!(parser.has_complete_stanza());

        let stanza = parser.next_stanza().unwrap();
        assert!(matches!(stanza, Some(ParsedStanza::Message(_))));
    }

    #[test]
    fn test_parser_iq() {
        let mut parser = XmlParser::new();
        // Include xmlns='jabber:client' as minidom requires namespace declarations
        parser.feed(b"<iq xmlns='jabber:client' type='get' id='bind_1'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/></iq>");

        assert!(parser.has_complete_stanza());

        let stanza = parser.next_stanza().unwrap();
        assert!(matches!(stanza, Some(ParsedStanza::Iq(_))));
    }

    #[test]
    fn test_parser_stream_end() {
        let mut parser = XmlParser::new();
        parser.feed(b"</stream:stream>");

        assert!(parser.has_complete_stanza());

        let stanza = parser.next_stanza().unwrap();
        assert!(matches!(stanza, Some(ParsedStanza::StreamEnd)));
    }

    #[test]
    fn test_element_to_string_roundtrip() {
        let xml = "<message to='bob@example.com' type='chat' xmlns='jabber:client'><body>Hello!</body></message>";
        let element = parse_element(xml).unwrap();
        let output = element_to_string(&element).unwrap();

        // Parse again to verify
        let element2 = parse_element(&output).unwrap();
        assert_eq!(element.name(), element2.name());
        assert_eq!(element.attr("to"), element2.attr("to"));
    }
}
