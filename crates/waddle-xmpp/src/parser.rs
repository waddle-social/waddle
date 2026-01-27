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
    /// Stream Management namespace (XEP-0198, version 3)
    pub const SM: &str = "urn:xmpp:sm:3";
    /// Instant Stream Resumption namespace (XEP-0397)
    pub const ISR: &str = "urn:xmpp:isr:0";
    /// Client State Indication namespace (XEP-0352)
    pub const CSI: &str = "urn:xmpp:csi:0";
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
            // XEP-0198 Stream Management elements
            "enable",
            "enabled",
            "resume",
            "resumed",
            "failed",
            "r",
            "a",
            // XEP-0220 Server Dialback elements
            "db:result",
            "db:verify",
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
        // NOTE: Pattern order matters! More specific patterns must come before less specific ones.
        // e.g., "<resume" must come before "<r" since "<r" would otherwise match "<resume"
        type StanzaParser = fn(&str) -> Result<ParsedStanza, XmppError>;
        let stanza_patterns: &[(&str, StanzaParser)] = &[
            ("<starttls", parse_starttls),
            ("<proceed", parse_tls_proceed),
            ("<failure", parse_tls_failure),
            ("<stream:features", parse_stream_features),
            ("<stream:error", parse_stream_error),
            ("<auth", parse_auth),
            ("<response", parse_sasl_response),  // SASL response for SCRAM
            ("<iq", parse_iq_stanza),
            ("<message", parse_message_stanza),
            ("<presence", parse_presence_stanza),
            // XEP-0198 Stream Management stanzas (order matters!)
            ("<enable", parse_sm_enable),
            ("<resume", parse_sm_resume),  // Must come before <r
            ("<r", parse_sm_request),
            ("<a ", parse_sm_ack),  // Note: space to avoid matching <auth
            // XEP-0220 Server Dialback stanzas
            ("<db:result", parse_dialback_result),
            ("<db:verify", parse_dialback_verify),
            // XEP-0352 Client State Indication stanzas
            ("<active", parse_csi_active),
            ("<inactive", parse_csi_inactive),
        ];

        for (pattern, parser) in stanza_patterns {
            if let Some(start) = data.find(pattern) {
                // Find the end of this stanza
                let tag_name = &pattern[1..].trim(); // Strip leading < and any trailing space
                if let Some(end) = find_stanza_end(&data, start, tag_name) {
                    let stanza_xml = &data[start..end];
                    let result = parser(stanza_xml)?;

                    // Remove parsed data from buffer
                    self.buffer = data.as_bytes()[end..].to_vec();
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
    /// TLS proceed response (server accepts STARTTLS)
    TlsProceed,
    /// TLS failure response (server rejects STARTTLS)
    TlsFailure,
    /// Stream features
    Features {
        /// Whether STARTTLS is advertised
        starttls: bool,
        /// Whether STARTTLS is required
        starttls_required: bool,
        /// Whether dialback is advertised
        dialback: bool,
        /// SASL mechanisms available
        sasl_mechanisms: Vec<String>,
    },
    /// Stream error
    StreamError {
        /// Error condition
        condition: String,
        /// Optional error text
        text: Option<String>,
    },
    /// SASL auth request with mechanism and base64 data
    SaslAuth { mechanism: String, data: String },
    /// SASL response (for multi-step auth like SCRAM) with base64 data
    SaslResponse { data: String },
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
    /// XEP-0198: Stream Management enable request
    SmEnable { resume: bool, max: Option<u32> },
    /// XEP-0198: Stream Management request ack
    SmRequest,
    /// XEP-0198: Stream Management ack response
    SmAck { h: u32 },
    /// XEP-0198: Stream Management resume request
    SmResume { previd: String, h: u32 },
    /// XEP-0220: Server Dialback result (initial request or response)
    DialbackResult {
        /// Originating domain (from attribute)
        from: String,
        /// Receiving domain (to attribute)
        to: String,
        /// Dialback key (content of db:result for initial request)
        key: Option<String>,
        /// Result type (only present in response: "valid" or "invalid")
        result_type: Option<String>,
    },
    /// XEP-0220: Server Dialback verification request/response
    DialbackVerify {
        /// Originating domain (from attribute)
        from: String,
        /// Receiving domain (to attribute)
        to: String,
        /// Stream ID being verified
        id: String,
        /// Dialback key (content for request, empty for response)
        key: Option<String>,
        /// Result type (only present in response: "valid" or "invalid")
        result_type: Option<String>,
    },
    /// XEP-0352: Client State Indication - active
    CsiActive,
    /// XEP-0352: Client State Indication - inactive
    CsiInactive,
}

fn parse_starttls(data: &str) -> Result<ParsedStanza, XmppError> {
    if data.contains("starttls") {
        Ok(ParsedStanza::StartTls)
    } else {
        Err(XmppError::xml_parse("Invalid starttls element"))
    }
}

/// Parse TLS proceed response from server.
fn parse_tls_proceed(data: &str) -> Result<ParsedStanza, XmppError> {
    if data.contains("proceed") {
        Ok(ParsedStanza::TlsProceed)
    } else {
        Err(XmppError::xml_parse("Invalid TLS proceed element"))
    }
}

/// Parse TLS failure response from server.
fn parse_tls_failure(data: &str) -> Result<ParsedStanza, XmppError> {
    if data.contains("failure") && data.contains(ns::TLS) {
        Ok(ParsedStanza::TlsFailure)
    } else {
        Err(XmppError::xml_parse("Invalid TLS failure element"))
    }
}

/// Parse stream features element.
fn parse_stream_features(data: &str) -> Result<ParsedStanza, XmppError> {
    let starttls = data.contains("<starttls");
    let starttls_required = data.contains("<required");
    let dialback = data.contains("dialback") || data.contains("db:");

    // Extract SASL mechanisms
    let mut sasl_mechanisms = Vec::new();
    let mut search_pos = 0;
    while let Some(start) = data[search_pos..].find("<mechanism>") {
        let actual_start = search_pos + start + 11; // "<mechanism>".len()
        if let Some(end) = data[actual_start..].find("</mechanism>") {
            let mechanism = data[actual_start..actual_start + end].trim();
            sasl_mechanisms.push(mechanism.to_string());
            search_pos = actual_start + end;
        } else {
            break;
        }
    }

    Ok(ParsedStanza::Features {
        starttls,
        starttls_required,
        dialback,
        sasl_mechanisms,
    })
}

/// Parse stream error element.
fn parse_stream_error(data: &str) -> Result<ParsedStanza, XmppError> {
    // Common stream error conditions
    let conditions = [
        "bad-format",
        "bad-namespace-prefix",
        "conflict",
        "connection-timeout",
        "host-gone",
        "host-unknown",
        "improper-addressing",
        "internal-server-error",
        "invalid-from",
        "invalid-namespace",
        "invalid-xml",
        "not-authorized",
        "not-well-formed",
        "policy-violation",
        "remote-connection-failed",
        "reset",
        "resource-constraint",
        "restricted-xml",
        "see-other-host",
        "system-shutdown",
        "undefined-condition",
        "unsupported-encoding",
        "unsupported-feature",
        "unsupported-stanza-type",
        "unsupported-version",
    ];

    let condition = conditions
        .iter()
        .find(|c| data.contains(*c))
        .map(|c| c.to_string())
        .unwrap_or_else(|| "undefined-condition".to_string());

    // Try to extract text element
    let text = if let Some(start) = data.find("<text") {
        if let Some(content_start) = data[start..].find('>') {
            let actual_start = start + content_start + 1;
            if let Some(end) = data[actual_start..].find("</text>") {
                Some(data[actual_start..actual_start + end].trim().to_string())
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(ParsedStanza::StreamError { condition, text })
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

/// Parse SASL response stanza (for multi-step auth like SCRAM).
fn parse_sasl_response(data: &str) -> Result<ParsedStanza, XmppError> {
    // Extract content between > and </response>
    let content_start = data.find('>').map(|i| i + 1).unwrap_or(0);
    let content_end = data.find("</response>").unwrap_or(data.len());
    let content = if content_start < content_end {
        data[content_start..content_end].trim().to_string()
    } else {
        String::new()
    };

    Ok(ParsedStanza::SaslResponse { data: content })
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

/// Parse XEP-0198 Stream Management enable request.
fn parse_sm_enable(data: &str) -> Result<ParsedStanza, XmppError> {
    // Check for SM namespace
    if !data.contains(ns::SM) {
        return Err(XmppError::xml_parse("Invalid SM enable: wrong namespace"));
    }

    let resume = data.contains("resume='true'") || data.contains("resume=\"true\"");
    let max = extract_attribute(data, "max").and_then(|s| s.parse().ok());

    Ok(ParsedStanza::SmEnable { resume, max })
}

/// Parse XEP-0198 Stream Management request (<r/>).
fn parse_sm_request(data: &str) -> Result<ParsedStanza, XmppError> {
    // Accept both with namespace and bare <r/>
    if data.contains("<r") && (data.contains(ns::SM) || data.trim() == "<r/>" || data.contains("<r/>")) {
        Ok(ParsedStanza::SmRequest)
    } else {
        Err(XmppError::xml_parse("Invalid SM request"))
    }
}

/// Parse XEP-0198 Stream Management ack (<a h='N'/>).
fn parse_sm_ack(data: &str) -> Result<ParsedStanza, XmppError> {
    // Must have h attribute
    let h = extract_attribute(data, "h")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| XmppError::xml_parse("SM ack missing 'h' attribute"))?;

    Ok(ParsedStanza::SmAck { h })
}

/// Parse XEP-0198 Stream Management resume request.
fn parse_sm_resume(data: &str) -> Result<ParsedStanza, XmppError> {
    if !data.contains(ns::SM) {
        return Err(XmppError::xml_parse("Invalid SM resume: wrong namespace"));
    }

    let previd = extract_attribute(data, "previd")
        .ok_or_else(|| XmppError::xml_parse("SM resume missing 'previd' attribute"))?;
    let h = extract_attribute(data, "h")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| XmppError::xml_parse("SM resume missing 'h' attribute"))?;

    Ok(ParsedStanza::SmResume { previd, h })
}

/// Parse XEP-0220 Server Dialback result element.
///
/// Handles both initial requests (with key content) and responses (with type attribute).
fn parse_dialback_result(data: &str) -> Result<ParsedStanza, XmppError> {
    let from = extract_attribute(data, "from")
        .ok_or_else(|| XmppError::xml_parse("db:result missing 'from' attribute"))?;
    let to = extract_attribute(data, "to")
        .ok_or_else(|| XmppError::xml_parse("db:result missing 'to' attribute"))?;

    // Check for type attribute (present in responses)
    let result_type = extract_attribute(data, "type");

    // Extract key content (present in initial requests)
    let key = if result_type.is_none() {
        // Extract content between > and </db:result>
        let content_start = data.find('>').map(|i| i + 1).unwrap_or(0);
        let content_end = data.find("</db:result>").unwrap_or(data.len());
        if content_start < content_end {
            let content = data[content_start..content_end].trim();
            if !content.is_empty() {
                Some(content.to_string())
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(ParsedStanza::DialbackResult {
        from,
        to,
        key,
        result_type,
    })
}

/// Parse XEP-0220 Server Dialback verify element.
///
/// Handles both verification requests (with key content) and responses (with type attribute).
fn parse_dialback_verify(data: &str) -> Result<ParsedStanza, XmppError> {
    let from = extract_attribute(data, "from")
        .ok_or_else(|| XmppError::xml_parse("db:verify missing 'from' attribute"))?;
    let to = extract_attribute(data, "to")
        .ok_or_else(|| XmppError::xml_parse("db:verify missing 'to' attribute"))?;
    let id = extract_attribute(data, "id")
        .ok_or_else(|| XmppError::xml_parse("db:verify missing 'id' attribute"))?;

    // Check for type attribute (present in responses)
    let result_type = extract_attribute(data, "type");

    // Extract key content (present in verification requests)
    let key = if result_type.is_none() {
        // Extract content between > and </db:verify>
        let content_start = data.find('>').map(|i| i + 1).unwrap_or(0);
        let content_end = data.find("</db:verify>").unwrap_or(data.len());
        if content_start < content_end {
            let content = data[content_start..content_end].trim();
            if !content.is_empty() {
                Some(content.to_string())
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(ParsedStanza::DialbackVerify {
        from,
        to,
        id,
        key,
        result_type,
    })
}

/// Parse XEP-0352 Client State Indication active element.
fn parse_csi_active(data: &str) -> Result<ParsedStanza, XmppError> {
    if data.contains("<active") && data.contains(ns::CSI) {
        Ok(ParsedStanza::CsiActive)
    } else {
        Err(XmppError::xml_parse("Invalid CSI active element"))
    }
}

/// Parse XEP-0352 Client State Indication inactive element.
fn parse_csi_inactive(data: &str) -> Result<ParsedStanza, XmppError> {
    if data.contains("<inactive") && data.contains(ns::CSI) {
        Ok(ParsedStanza::CsiInactive)
    } else {
        Err(XmppError::xml_parse("Invalid CSI inactive element"))
    }
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

    // XEP-0198 Stream Management parsing tests

    #[test]
    fn test_parser_sm_enable() {
        let mut parser = XmlParser::new();
        parser.feed(b"<enable xmlns='urn:xmpp:sm:3'/>");

        assert!(parser.has_complete_stanza());

        let stanza = parser.next_stanza().unwrap();
        if let Some(ParsedStanza::SmEnable { resume, max }) = stanza {
            assert!(!resume);
            assert!(max.is_none());
        } else {
            panic!("Expected SmEnable");
        }
    }

    #[test]
    fn test_parser_sm_enable_with_resume() {
        let mut parser = XmlParser::new();
        parser.feed(b"<enable xmlns='urn:xmpp:sm:3' resume='true' max='300'/>");

        assert!(parser.has_complete_stanza());

        let stanza = parser.next_stanza().unwrap();
        if let Some(ParsedStanza::SmEnable { resume, max }) = stanza {
            assert!(resume);
            assert_eq!(max, Some(300));
        } else {
            panic!("Expected SmEnable with resume");
        }
    }

    #[test]
    fn test_parser_sm_request() {
        let mut parser = XmlParser::new();
        parser.feed(b"<r xmlns='urn:xmpp:sm:3'/>");

        assert!(parser.has_complete_stanza());

        let stanza = parser.next_stanza().unwrap();
        assert!(matches!(stanza, Some(ParsedStanza::SmRequest)));
    }

    #[test]
    fn test_parser_sm_ack() {
        let mut parser = XmlParser::new();
        parser.feed(b"<a xmlns='urn:xmpp:sm:3' h='5'/>");

        assert!(parser.has_complete_stanza());

        let stanza = parser.next_stanza().unwrap();
        if let Some(ParsedStanza::SmAck { h }) = stanza {
            assert_eq!(h, 5);
        } else {
            panic!("Expected SmAck");
        }
    }

    #[test]
    fn test_parser_sm_resume() {
        let mut parser = XmlParser::new();
        parser.feed(b"<resume xmlns='urn:xmpp:sm:3' previd='stream-123' h='10'/>");

        assert!(parser.has_complete_stanza());

        let stanza = parser.next_stanza().unwrap();
        if let Some(ParsedStanza::SmResume { previd, h }) = stanza {
            assert_eq!(previd, "stream-123");
            assert_eq!(h, 10);
        } else {
            panic!("Expected SmResume");
        }
    }
}
